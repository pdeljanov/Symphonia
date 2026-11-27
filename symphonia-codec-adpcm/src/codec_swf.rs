// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Shockwave Flash (SWF) ADPCM, which FLV files use for audio.
//!
//! A packet holds blocks of up to 4096 samples per channel. A block starts with the size of the
//! codes in bits (2 to 5), and the initial sample and step index of every channel, and is followed
//! by the codes of the remaining samples, interleaved by channel.

use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef,
};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::well_known::CODEC_ID_ADPCM_SWF;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::packet::PacketRef;
use symphonia_core::support_audio_codec;

use crate::common_ima::IMA_STEP_TABLE;

const SWF_CODEC_INFO: CodecInfo =
    CodecInfo { short_name: "adpcm_swf", long_name: "ADPCM Shockwave Flash", profiles: &[] };

/// The number of samples of a channel in a block.
const BLOCK_SAMPLES: usize = 4096;

/// The step index changes by the code size, and the magnitude of the code.
const INDEX_TABLES: [&[i32]; 4] = [
    &[-1, 2],
    &[-1, -1, 2, 4],
    &[-1, -1, -1, -1, 2, 4, 6, 8],
    &[-1, -1, -1, -1, -1, -1, -1, -1, 1, 2, 4, 6, 8, 10, 13, 16],
];

/// A reader of bits, most significant bit first.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }

    fn get(&mut self, n: usize) -> i32 {
        let mut v = 0;
        for _ in 0..n {
            let bit = (self.data[self.pos >> 3] >> (7 - (self.pos & 7))) & 1;
            v = (v << 1) | i32::from(bit);
            self.pos += 1;
        }
        v
    }
}

/// Returns the number of samples of every channel that a packet decodes to.
pub fn swf_adpcm_samples(data: &[u8], channels: usize) -> usize {
    let mut bits = Bits { data, pos: 0 };
    let mut samples = 0;

    while bits.left() >= 2 + channels * 22 {
        let code_size = bits.get(2) as usize + 2;
        bits.pos += channels * 22;
        let coded = (bits.left() / (channels * code_size)).min(BLOCK_SAMPLES - 1);
        bits.pos += coded * channels * code_size;
        samples += 1 + coded;
    }

    samples
}

pub struct SwfAdpcmDecoder {
    params: AudioCodecParameters,
    buf: AudioBuffer<i32>,
    spec: AudioSpec,
    channels: usize,
}

impl SwfAdpcmDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_ADPCM_SWF {
            return unsupported_error("adpcm_swf: invalid codec");
        }

        let (Some(rate), Some(channels)) = (params.sample_rate, params.channels.clone())
        else {
            return unsupported_error("adpcm_swf: missing sample rate or channels");
        };

        let count = channels.count();

        let spec = AudioSpec::new(rate, channels);

        Ok(SwfAdpcmDecoder {
            params: params.clone(),
            buf: AudioBuffer::new(spec.clone(), 0),
            spec,
            channels: count,
        })
    }

    fn decode_inner(&mut self, data: &[u8]) -> Result<()> {
        let frames = swf_adpcm_samples(data, self.channels);
        if frames == 0 {
            return decode_error("adpcm_swf: packet is too short");
        }

        self.buf = AudioBuffer::new(self.spec.clone(), frames);
        self.buf.render_uninit(Some(frames));

        let mut bits = Bits { data, pos: 0 };
        let mut at = 0;

        while bits.left() >= 2 + self.channels * 22 {
            let code_size = bits.get(2) as usize + 2;
            let (sign, first) = (1 << (code_size - 1), 1 << (code_size - 2));
            let index_table = INDEX_TABLES[code_size - 2];

            let mut predictor = vec![0i32; self.channels];
            let mut index = vec![0usize; self.channels];
            for ch in 0..self.channels {
                predictor[ch] = bits.get(16) as i16 as i32;
                index[ch] = (bits.get(6) as usize).min(88);
                self.set_sample(ch, at, predictor[ch]);
            }

            let coded = (bits.left() / (self.channels * code_size)).min(BLOCK_SAMPLES - 1);
            for n in 1..=coded {
                for ch in 0..self.channels {
                    let code = bits.get(code_size);

                    let mut step = IMA_STEP_TABLE[index[ch]];
                    let mut diff = 0;
                    let mut k = first;
                    while k != 0 {
                        if code & k != 0 {
                            diff += step;
                        }
                        step >>= 1;
                        k >>= 1;
                    }
                    diff += step;

                    predictor[ch] = (predictor[ch] + if code & sign != 0 { -diff } else { diff })
                        .clamp(-32768, 32767);
                    index[ch] = (index[ch] as i32 + index_table[(code & (sign - 1)) as usize])
                        .clamp(0, 88) as usize;

                    self.set_sample(ch, at + n, predictor[ch]);
                }
            }

            at += 1 + coded;
        }

        Ok(())
    }

    fn set_sample(&mut self, ch: usize, at: usize, value: i32) {
        if let Some(plane) = self.buf.plane_mut(ch) {
            plane[at] = value << 16;
        }
    }
}

impl AudioDecoder for SwfAdpcmDecoder {
    fn reset(&mut self) {
        // Every packet starts with the state of the channels.
    }

    fn codec_info(&self) -> &CodecInfo {
        &SWF_CODEC_INFO
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        if let Err(err) = self.decode_inner(packet.data) {
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

impl RegisterableAudioDecoder for SwfAdpcmDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(SwfAdpcmDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(CODEC_ID_ADPCM_SWF, "adpcm_swf", "ADPCM Shockwave Flash")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The start of a mono stream of 4-bit codes, and the output of an independent decoder.
    const DATA: [u8; 40] = [
        0x80, 0x10, 0x00, 0xff, 0xff, 0xff, 0x87, 0x32, 0xaf, 0xab, 0xfa, 0x99, 0x98, 0x81, 0x28,
        0xa8, 0x01, 0x31, 0xdb, 0x81, 0x22, 0x07, 0x52, 0x24, 0x18, 0x99, 0xda, 0x81, 0x34, 0x21,
        0x54, 0x22, 0x09, 0x8a, 0x99, 0x9a, 0x99, 0xa1, 0x09, 0x15,
    ];
    const SAMPLES: [i16; 75] = [
        64, 53, 23, -40, -176, -469, -1100, -1190, 43, 1276, 2077, 1349, -638, -2058, -3865, -7385,
        -9901, -11273, -12519, -13653, -13996, -14308, -13456, -12165, -12399, -13465, -13659,
        -13483, -13003, -11984, -11587, -12910, -14143, -14303, -13867, -13205, -12604, -12495,
        -11003, -8657, -7096, -5676, -3352, -2416, -2700, -3474, -4177, -6523, -8084, -8368, -7594,
        -5952, -4032, -2741, -2038, 308, 3119, 5009, 6726, 7038, 6186, 5928, 4755, 4116, 3534,
        3006, 2205, 1769, 1372, 771, 1099, 1198, 927, 1173, 1994,
    ];

    #[test]
    fn counts_the_samples() {
        assert_eq!(swf_adpcm_samples(&DATA, 1), 75);
        assert_eq!(swf_adpcm_samples(&DATA[..2], 1), 0);
    }

    #[test]
    fn decodes_a_block() {
        use symphonia_core::audio::layouts::CHANNEL_LAYOUT_MONO;
        use symphonia_core::packet::Packet;
        use symphonia_core::units::{Duration, Timestamp};

        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_ADPCM_SWF)
            .with_sample_rate(44100)
            .with_channels(CHANNEL_LAYOUT_MONO);
        let mut decoder =
            SwfAdpcmDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();

        let packet = Packet::new(0, Timestamp::new(0), Duration::new(75), DATA.to_vec());
        let buf = decoder.decode(&packet).unwrap();
        let mut got = vec![0i32; 75];
        buf.copy_to_slice_planar(&mut [&mut got[..]]);
        let got: Vec<i16> = got.iter().map(|&s| (s >> 16) as i16).collect();
        assert_eq!(got, SAMPLES);
    }
}
