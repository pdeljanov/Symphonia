// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::f64::consts::PI;

use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef,
};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::well_known::CODEC_ID_SBC;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::packet::PacketRef;
use symphonia_core::support_audio_codec;

use crate::frame::{FrameHeader, MAX_BLOCKS, MAX_SUBBANDS, SubbandSamples, unpack};
use crate::tables::{PROTO_4_40, PROTO_8_80};

const SBC_CODEC_INFO: CodecInfo =
    CodecInfo { short_name: "sbc", long_name: "Low-complexity Subband Coding", profiles: &[] };

/// The number of blocks of the synthesis window.
const WINDOW_BLOCKS: usize = 10;

/// The cosine matrix and the window of the synthesis filter bank of a number of subbands.
struct FilterBank {
    subbands: usize,
    matrix: Vec<f32>,
    window: Vec<f32>,
}

impl FilterBank {
    fn new(subbands: usize, proto: &[f64]) -> Self {
        let m = subbands as f64;

        let mut matrix = Vec::with_capacity(2 * subbands * subbands);
        for k in 0..2 * subbands {
            for i in 0..subbands {
                matrix.push((((i as f64 + 0.5) * (k as f64 + m / 2.0) * PI) / m).cos() as f32);
            }
        }

        // The window is scaled by the number of subbands, and to the range of the 16-bit samples.
        let window = proto.iter().map(|&c| (-c * m / 32768.0) as f32).collect();

        FilterBank { subbands, matrix, window }
    }

    /// Synthesizes the PCM samples of a block from its subband samples. `v` is the history of the
    /// matrixing, which is shifted by the block.
    fn synthesize(&self, v: &mut [f32], input: &[f32], out: &mut [f32]) {
        let m = self.subbands;
        let len = 2 * m * WINDOW_BLOCKS;

        v.copy_within(0..len - 2 * m, 2 * m);
        for (k, row) in self.matrix.chunks_exact(m).enumerate() {
            v[k] = row.iter().zip(input).map(|(c, s)| c * s).sum();
        }

        for (j, sample) in out.iter_mut().enumerate().take(m) {
            let mut acc = 0.0;
            for i in 0..WINDOW_BLOCKS / 2 {
                acc += v[i * 4 * m + j] * self.window[i * 2 * m + j];
                acc += v[i * 4 * m + 3 * m + j] * self.window[i * 2 * m + m + j];
            }
            *sample = acc;
        }
    }
}

pub struct SbcDecoder {
    params: AudioCodecParameters,
    buf: AudioBuffer<f32>,
    channels: usize,
    bank4: FilterBank,
    bank8: FilterBank,
    history: [[f32; 2 * MAX_SUBBANDS * WINDOW_BLOCKS]; 2],
    last_subbands: usize,
    samples: Box<SubbandSamples>,
}

impl SbcDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_SBC {
            return unsupported_error("sbc: invalid codec");
        }

        let (Some(rate), Some(channels)) = (params.sample_rate, params.channels.clone())
        else {
            return unsupported_error("sbc: missing sample rate or channels");
        };

        let count = channels.count();
        if count == 0 || count > 2 {
            return unsupported_error("sbc: unsupported channel count");
        }

        Ok(SbcDecoder {
            params: params.clone(),
            buf: AudioBuffer::new(AudioSpec::new(rate, channels), MAX_BLOCKS * MAX_SUBBANDS),
            channels: count,
            bank4: FilterBank::new(4, &PROTO_4_40),
            bank8: FilterBank::new(8, &PROTO_8_80),
            history: [[0.0; 2 * MAX_SUBBANDS * WINDOW_BLOCKS]; 2],
            last_subbands: 0,
            samples: Box::new([[[0.0; MAX_SUBBANDS]; 2]; MAX_BLOCKS]),
        })
    }

    fn decode_inner(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        self.buf.clear();

        let Some(header) = FrameHeader::parse(packet.data)
        else {
            return decode_error("sbc: invalid frame header");
        };
        if packet.data.len() < header.frame_len {
            return decode_error("sbc: truncated frame");
        }
        if header.channels() != self.channels {
            return decode_error("sbc: the channel count has changed");
        }

        unpack(&header, packet.data, &mut self.samples)?;

        if header.subbands != self.last_subbands {
            self.history = [[0.0; 2 * MAX_SUBBANDS * WINDOW_BLOCKS]; 2];
            self.last_subbands = header.subbands;
        }
        let bank = if header.subbands == 4 { &self.bank4 } else { &self.bank8 };
        let m = header.subbands;

        self.buf.render_uninit(Some(header.samples_per_frame()));

        for ch in 0..self.channels {
            let Some(plane) = self.buf.plane_mut(ch)
            else {
                continue;
            };
            let history = &mut self.history[ch][..2 * m * WINDOW_BLOCKS];

            for (block, out) in self.samples[..header.blocks].iter().zip(plane.chunks_exact_mut(m))
            {
                bank.synthesize(history, &block[ch][..m], out);
            }
        }

        Ok(())
    }
}

impl AudioDecoder for SbcDecoder {
    fn reset(&mut self) {
        self.history = [[0.0; 2 * MAX_SUBBANDS * WINDOW_BLOCKS]; 2];
    }

    fn codec_info(&self) -> &CodecInfo {
        &SBC_CODEC_INFO
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

impl RegisterableAudioDecoder for SbcDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(SbcDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(CODEC_ID_SBC, "sbc", "Low-complexity Subband Coding")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_stays_silent() {
        for (m, proto) in [(4, &PROTO_4_40[..]), (8, &PROTO_8_80[..])] {
            let bank = FilterBank::new(m, proto);
            let mut history = vec![0.0; 2 * m * WINDOW_BLOCKS];
            let mut out = vec![1.0; m];
            bank.synthesize(&mut history, &vec![0.0; m], &mut out);
            assert!(out.iter().all(|&s| s == 0.0));
        }
    }

    #[test]
    fn impulse_response_is_finite_and_bounded() {
        let bank = FilterBank::new(8, &PROTO_8_80);
        let mut history = vec![0.0; 2 * 8 * WINDOW_BLOCKS];
        let mut input = vec![0.0; 8];
        input[0] = 32768.0;
        let mut peak = 0.0f32;
        for _ in 0..12 {
            let mut out = vec![0.0; 8];
            bank.synthesize(&mut history, &input, &mut out);
            input[0] = 0.0;
            peak = out.iter().fold(peak, |p, s| p.max(s.abs()));
        }
        assert!(peak > 0.1 && peak < 2.0);
    }
}
