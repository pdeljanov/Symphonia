// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::f64::consts::{PI, SQRT_2};

use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef,
};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::well_known::CODEC_ID_ADPCM_ADX;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::packet::PacketRef;
use symphonia_core::support_audio_codec;

const ADX_CODEC_INFO: CodecInfo =
    CodecInfo { short_name: "adx", long_name: "CRI ADX ADPCM", profiles: &[] };

/// The number of bytes of the block of a channel: a scale and 32 samples of 4 bits.
const BLOCK_LEN: usize = 18;
const BLOCK_SAMPLES: usize = 32;

/// The offset of the cutoff frequency in the header, which is the extra data of the codec.
const CUTOFF_OFFSET: usize = 0x10;
/// The number of bytes of the header that the decoder needs.
pub(crate) const HEADER_PREFIX_LEN: usize = 0x14;

const COEFF_BITS: u32 = 12;

/// The most samples of a channel that a packet may hold.
const MAX_PACKET_SAMPLES: usize = 64 * BLOCK_SAMPLES;

pub struct AdxDecoder {
    params: AudioCodecParameters,
    buf: AudioBuffer<f32>,
    coeff: [i32; 2],
    channels: usize,
    history: Vec<[i32; 2]>,
}

impl AdxDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_ADPCM_ADX {
            return unsupported_error("adx: invalid codec");
        }

        let (Some(rate), Some(channels)) = (params.sample_rate, params.channels.clone())
        else {
            return unsupported_error("adx: missing sample rate or channels");
        };
        let Some(extra) = params.extra_data.as_ref().filter(|e| e.len() >= HEADER_PREFIX_LEN)
        else {
            return unsupported_error("adx: missing header");
        };

        let cutoff =
            f64::from(u16::from_be_bytes([extra[CUTOFF_OFFSET], extra[CUTOFF_OFFSET + 1]]));

        // The coefficients of the two-tap predictor, which follow from the cutoff frequency of the
        // high pass filter that the encoder applies.
        let a = SQRT_2 - (2.0 * PI * cutoff / f64::from(rate)).cos();
        let b = SQRT_2 - 1.0;
        let c = (a - ((a + b) * (a - b)).sqrt()) / b;
        let coeff = [
            (c * 2.0 * f64::from(1 << COEFF_BITS)).round() as i32,
            (-(c * c) * f64::from(1 << COEFF_BITS)).round() as i32,
        ];

        let count = channels.count();

        Ok(AdxDecoder {
            params: params.clone(),
            buf: AudioBuffer::new(AudioSpec::new(rate, channels), MAX_PACKET_SAMPLES),
            coeff,
            channels: count,
            history: vec![[0; 2]; count],
        })
    }

    fn decode_inner(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        self.buf.clear();

        let frame_len = BLOCK_LEN * self.channels;
        let frames = packet.data.len() / frame_len;
        if frames == 0 {
            return decode_error("adx: packet is too short");
        }

        let capacity = frames * BLOCK_SAMPLES;
        if capacity > MAX_PACKET_SAMPLES {
            return decode_error("adx: packet is too long");
        }
        let mut wanted = (packet.dur.get() as usize).min(capacity);

        let mut decoded = vec![vec![0f32; capacity]; self.channels];

        for (f, frame) in packet.data.chunks_exact(frame_len).enumerate() {
            for ch in 0..self.channels {
                let block = &frame[ch * BLOCK_LEN..(ch + 1) * BLOCK_LEN];
                let scale = i32::from(u16::from_be_bytes([block[0], block[1]]));

                // A scale with the top bit set marks the end of the stream.
                if scale & 0x8000 != 0 {
                    wanted = wanted.min(f * BLOCK_SAMPLES);
                    continue;
                }

                let [mut s1, mut s2] = self.history[ch];
                for i in 0..BLOCK_SAMPLES {
                    let byte = block[2 + i / 2];
                    let nibble = if i & 1 == 0 { byte >> 4 } else { byte & 0xf };
                    let delta = i32::from((nibble as i8) << 4 >> 4);

                    let predicted =
                        self.coeff[0].wrapping_mul(s1).wrapping_add(self.coeff[1].wrapping_mul(s2));
                    let s0 = (delta * scale).wrapping_add(predicted >> COEFF_BITS);
                    s2 = s1;
                    s1 = s0;
                    decoded[ch][f * BLOCK_SAMPLES + i] = s0.clamp(-32768, 32767) as f32 / 32768.0;
                }
                self.history[ch] = [s1, s2];
            }
        }

        self.buf.render_uninit(Some(wanted));
        for (ch, samples) in decoded.iter().enumerate() {
            if let Some(dst) = self.buf.plane_mut(ch) {
                dst.copy_from_slice(&samples[..wanted]);
            }
        }

        Ok(())
    }
}

impl AudioDecoder for AdxDecoder {
    fn reset(&mut self) {
        self.history.fill([0; 2]);
    }

    fn codec_info(&self) -> &CodecInfo {
        &ADX_CODEC_INFO
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

impl RegisterableAudioDecoder for AdxDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(AdxDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(CODEC_ID_ADPCM_ADX, "adx", "CRI ADX ADPCM")]
    }
}
