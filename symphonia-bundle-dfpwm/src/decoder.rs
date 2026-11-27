// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef,
};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::well_known::CODEC_ID_DFPWM;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::packet::PacketRef;
use symphonia_core::support_audio_codec;

const DFPWM_CODEC_INFO: CodecInfo =
    CodecInfo { short_name: "dfpwm", long_name: "DFPWM", profiles: &[] };

/// The number of fractional bits of the response of the integrator.
const RESPONSE_BITS: u32 = 10;
/// The lowest response, which keeps the integrator from stalling.
const RESPONSE_MIN: i32 = 2 << (RESPONSE_BITS - 8);
const RESPONSE_MAX: i32 = (1 << RESPONSE_BITS) - 1;
/// The strength of the output low pass filter, out of 256.
const LOW_PASS_STRENGTH: i32 = 140;

/// The most bytes that a packet may hold.
pub(crate) const MAX_PACKET_BYTES: usize = 8 * 1024;

/// The state of the DFPWM 1a decoder.
#[derive(Default)]
struct DfpwmState {
    charge: i32,
    response: i32,
    filtered: i32,
    last_bit: bool,
}

impl DfpwmState {
    fn decode_bit(&mut self, bit: bool) -> i32 {
        let target = if bit { 127 } else { -128 };
        let previous = self.charge;

        let mut charge = previous
            + ((self.response * (target - previous) + (1 << (RESPONSE_BITS - 1))) >> RESPONSE_BITS);
        if charge == previous && previous != target {
            charge += if bit { 1 } else { -1 };
        }

        let same = bit == self.last_bit;
        let response_target = if same { RESPONSE_MAX } else { 0 };
        if response_target > self.response {
            self.response += 1;
        }
        else if response_target < self.response {
            self.response -= 1;
        }
        self.response = self.response.max(RESPONSE_MIN);

        // A change of the bit is smoothed over the two last charges.
        let shaped = if same { charge } else { (charge + previous + 1) >> 1 };
        self.filtered += (LOW_PASS_STRENGTH * (shaped - self.filtered) + 0x80) >> 8;

        self.charge = charge;
        self.last_bit = bit;
        self.filtered
    }
}

pub struct DfpwmDecoder {
    params: AudioCodecParameters,
    buf: AudioBuffer<f32>,
    channels: usize,
    state: DfpwmState,
}

impl DfpwmDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_DFPWM {
            return unsupported_error("dfpwm: invalid codec");
        }

        let (Some(rate), Some(channels)) = (params.sample_rate, params.channels.clone())
        else {
            return unsupported_error("dfpwm: missing sample rate or channels");
        };

        let count = channels.count();

        Ok(DfpwmDecoder {
            params: params.clone(),
            buf: AudioBuffer::new(AudioSpec::new(rate, channels), MAX_PACKET_BYTES * 8 / count),
            channels: count,
            state: DfpwmState::default(),
        })
    }

    fn decode_inner(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        self.buf.clear();

        if packet.data.len() > MAX_PACKET_BYTES {
            return decode_error("dfpwm: packet is too long");
        }

        let frames = packet.data.len() * 8 / self.channels;
        if frames == 0 {
            return decode_error("dfpwm: packet is too short");
        }

        self.buf.render_uninit(Some(frames));

        // The channels are the consecutive samples of a single stream.
        let mut index = 0;
        for &byte in packet.data.iter() {
            for bit in 0..8 {
                let sample = self.state.decode_bit((byte >> bit) & 1 != 0);

                let (frame, ch) = (index / self.channels, index % self.channels);
                if frame < frames {
                    if let Some(plane) = self.buf.plane_mut(ch) {
                        plane[frame] = sample as f32 / 128.0;
                    }
                }
                index += 1;
            }
        }

        Ok(())
    }
}

impl AudioDecoder for DfpwmDecoder {
    fn reset(&mut self) {
        self.state = DfpwmState::default();
    }

    fn codec_info(&self) -> &CodecInfo {
        &DFPWM_CODEC_INFO
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        if let Err(err) = self.decode_inner(packet) {
            self.buf.clear();
            Err(err)
        }
        else {
            Ok(self.buf.as_generic_audio_buffer_ref())
        }
    }

    fn finalize(&mut self) -> FinalizeResult {
        Default::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for DfpwmDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(DfpwmDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(CODEC_ID_DFPWM, "dfpwm", "DFPWM")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_stays_near_zero() {
        let mut state = DfpwmState::default();
        let mut last = 0;
        for i in 0..64 {
            last = state.decode_bit(i % 2 == 0);
        }
        assert!(last.abs() < 8);
    }

    #[test]
    fn constant_bits_saturate() {
        let mut state = DfpwmState::default();
        let mut last = 0;
        for _ in 0..4096 {
            last = state.decode_bit(true);
        }
        assert!(last > 100);
    }
}
