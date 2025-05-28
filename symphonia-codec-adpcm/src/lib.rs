// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]
// The following lints are allowed in all Symphonia crates. Please see clippy.toml for their
// justification.
#![allow(clippy::comparison_chain)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_range_contains)]

use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::support_audio_codec;

use symphonia_core::audio::{
    AsGenericAudioBufferRef, Audio, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef,
};
use symphonia_core::codecs::audio::well_known::{
    CODEC_ID_ADPCM_IMA_QT, CODEC_ID_ADPCM_IMA_WAV, CODEC_ID_ADPCM_MS,
};
use symphonia_core::codecs::audio::{AudioCodecId, AudioCodecParameters, AudioDecoderOptions};
use symphonia_core::codecs::audio::{AudioDecoder, FinalizeResult};
use symphonia_core::errors::{unsupported_error, Result};
use symphonia_core::formats::Packet;
use symphonia_core::io::ReadBytes;

mod codec_ima_wav;
mod codec_ima_qt;
mod codec_ms;
mod common_ima;
mod common;

fn is_supported_adpcm_codec(codec_id: AudioCodecId) -> bool {
    matches!(codec_id, CODEC_ID_ADPCM_MS | CODEC_ID_ADPCM_IMA_WAV | CODEC_ID_ADPCM_IMA_QT)
}

enum InnerDecoder {
    AdpcmMs,
    AdpcmIma,
    AdpcmImaQT,
}

impl InnerDecoder {
    fn decode_mono_fn<B: ReadBytes>(&self) -> impl Fn(&mut B, &mut [i32], usize) -> Result<()> {
        match *self {
            InnerDecoder::AdpcmMs => codec_ms::decode_mono,
            InnerDecoder::AdpcmIma => codec_ima_wav::decode_mono,
            InnerDecoder::AdpcmImaQT => codec_ima_qt::decode_mono,
        }
    }

    fn decode_stereo_fn<B: ReadBytes>(
        &self,
    ) -> impl Fn(&mut B, [&mut [i32]; 2], usize) -> Result<()> {
        match *self {
            InnerDecoder::AdpcmMs => codec_ms::decode_stereo,
            InnerDecoder::AdpcmIma => codec_ima_wav::decode_stereo,
            InnerDecoder::AdpcmImaQT => codec_ima_qt::decode_stereo,
        }
    }
}

/// Adaptive Differential Pulse Code Modulation (ADPCM) decoder.
pub struct AdpcmDecoder {
    params: AudioCodecParameters,
    inner_decoder: InnerDecoder,
    buf: AudioBuffer<i32>,
}

impl AdpcmDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        // This decoder only supports certain ADPCM codecs.
        if !is_supported_adpcm_codec(params.codec) {
            return unsupported_error("adpcm: invalid codec");
        }

        let frames = match params.max_frames_per_packet {
            Some(frames) => frames as usize,
            _ => return unsupported_error("adpcm: maximum frames per packet is required"),
        };

        if params.frames_per_block.is_none() || params.frames_per_block.unwrap() == 0 {
            return unsupported_error("adpcm: valid frames per block is required");
        }

        let rate = match params.sample_rate {
            Some(rate) => rate,
            _ => return unsupported_error("adpcm: sample rate is required"),
        };

        let spec = if let Some(channels) = &params.channels {
            if channels.count() > 2 {
                return unsupported_error("adpcm: up to two channels are supported");
            }

            AudioSpec::new(rate, channels.clone())
        }
        else {
            return unsupported_error("adpcm: channels or channel_layout is required");
        };

        let inner_decoder = match params.codec {
            CODEC_ID_ADPCM_MS => InnerDecoder::AdpcmMs,
            CODEC_ID_ADPCM_IMA_WAV => InnerDecoder::AdpcmIma,
            CODEC_ID_ADPCM_IMA_QT => InnerDecoder::AdpcmImaQT,
            _ => return unsupported_error("adpcm: codec is unsupported"),
        };

        Ok(AdpcmDecoder {
            params: params.clone(),
            inner_decoder,
            buf: AudioBuffer::new(spec, frames),
        })
    }

    fn decode_inner(&mut self, packet: &Packet) -> Result<()> {
        let mut stream = packet.as_buf_reader();

        let frames_per_block = self.params.frames_per_block.unwrap() as usize;

        let block_count = packet.block_dur() as usize / frames_per_block;

        self.buf.clear();
        self.buf.render_uninit(Some(block_count * frames_per_block));

        let channel_count = self.buf.spec().channels().count();
        match channel_count {
            1 => {
                let buffer = self.buf.plane_mut(0).unwrap();
                let decode_mono = self.inner_decoder.decode_mono_fn();
                for block_id in 0..block_count {
                    let offset = frames_per_block * block_id;
                    let buffer_range = offset..(offset + frames_per_block);
                    let buffer = &mut buffer[buffer_range];
                    decode_mono(&mut stream, buffer, frames_per_block)?;
                }
            }
            2 => {
                let buffers = self.buf.plane_pair_mut(0, 1).unwrap();
                let decode_stereo = self.inner_decoder.decode_stereo_fn();
                for block_id in 0..block_count {
                    let offset = frames_per_block * block_id;
                    let buffer_range = offset..(offset + frames_per_block);
                    let buffers =
                        [&mut buffers.0[buffer_range.clone()], &mut buffers.1[buffer_range]];
                    decode_stereo(&mut stream, buffers, frames_per_block)?;
                }
            }
            _ => unreachable!(),
        }

        Ok(())
    }
}

impl AudioDecoder for AdpcmDecoder {
    fn reset(&mut self) {
        // No state is stored between packets, therefore do nothing.
    }

    fn codec_info(&self) -> &CodecInfo {
        // Return the codec that's in-use.
        &Self::supported_codecs().iter().find(|desc| desc.id == self.params.codec).unwrap().info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: &Packet) -> Result<GenericAudioBufferRef<'_>> {
        if let Err(e) = self.decode_inner(packet) {
            self.buf.clear();
            Err(e)
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

impl RegisterableAudioDecoder for AdpcmDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(AdpcmDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[
            support_audio_codec!(CODEC_ID_ADPCM_MS, "adpcm_ms", "Microsoft ADPCM"),
            support_audio_codec!(CODEC_ID_ADPCM_IMA_WAV, "adpcm_ima_wav", "ADPCM IMA WAV"),
            support_audio_codec!(CODEC_ID_ADPCM_IMA_QT, "adpcm_ima_qt", "ADPCM IMA QT"),
        ]
    }
}
