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
use symphonia_core::codecs::audio::well_known::{CODEC_ID_MLP, CODEC_ID_TRUEHD};
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::packet::PacketRef;
use symphonia_core::support_audio_codec;

use crate::mlp::MlpCore;

const TRUEHD_CODEC_INFO: CodecInfo =
    CodecInfo { short_name: "truehd", long_name: "Dolby TrueHD / MLP", profiles: &[] };

/// The most frames that an access unit holds.
const MAX_PACKET_FRAMES: usize = 160;

/// The samples have 24 bits, which are placed at the top of the 32-bit samples.
const SAMPLE_SHIFT: u32 = 8;

/// Dolby TrueHD and Meridian Lossless Packing decoder.
pub struct TrueHdDecoder {
    params: AudioCodecParameters,
    buf: AudioBuffer<i32>,
    core: MlpCore,
    frames: Vec<[i32; 16]>,
    channels: usize,
}

impl TrueHdDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_TRUEHD && params.codec != CODEC_ID_MLP {
            return unsupported_error("truehd: invalid codec");
        }

        let (Some(rate), Some(channels)) = (params.sample_rate, params.channels.clone())
        else {
            return unsupported_error("truehd: missing sample rate or channels");
        };

        let count = channels.count();
        if count == 0 || count > 14 {
            return unsupported_error("truehd: unsupported channel count");
        }

        Ok(TrueHdDecoder {
            params: params.clone(),
            buf: AudioBuffer::new(AudioSpec::new(rate, channels), MAX_PACKET_FRAMES),
            core: MlpCore::default(),
            frames: Vec::with_capacity(MAX_PACKET_FRAMES),
            channels: count,
        })
    }

    fn decode_inner(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        self.buf.clear();
        self.frames.clear();

        if let Err(msg) = self.core.decode(packet.data, &mut self.frames) {
            return decode_error(msg);
        }
        if self.core.channels() != self.channels {
            return decode_error("truehd: the channel count changed");
        }

        self.buf.render_uninit(Some(self.frames.len()));
        for ch in 0..self.channels {
            if let Some(dst) = self.buf.plane_mut(ch) {
                for (sample, frame) in dst.iter_mut().zip(&self.frames) {
                    *sample = frame[ch].wrapping_shl(SAMPLE_SHIFT);
                }
            }
        }

        Ok(())
    }
}

impl AudioDecoder for TrueHdDecoder {
    fn reset(&mut self) {
        self.core.reset();
    }

    fn codec_info(&self) -> &CodecInfo {
        &TRUEHD_CODEC_INFO
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

impl RegisterableAudioDecoder for TrueHdDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(TrueHdDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[
            support_audio_codec!(CODEC_ID_TRUEHD, "truehd", "Dolby TrueHD"),
            support_audio_codec!(CODEC_ID_MLP, "mlp", "Meridian Lossless Packing"),
        ]
    }
}
