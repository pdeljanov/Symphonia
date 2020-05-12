// Sonata
// Copyright (c) 2020 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use sonata_core::audio::{AudioBuffer, AudioBufferRef, AsAudioBufferRef};
use sonata_core::codecs::CODEC_TYPE_OPUS;
use sonata_core::codecs::{CodecParameters, CodecDescriptor, Decoder, DecoderOptions};
use sonata_core::errors::{Result};
use sonata_core::formats::Packet;
use sonata_core::support_codec;

/// Opus decoder.
pub struct OpusDecoder {
    params: CodecParameters,
    buf: AudioBuffer<f32>,
}

impl Decoder for OpusDecoder {

    fn try_new(params: &CodecParameters, _: &DecoderOptions) -> Result<Self> {
        Ok(OpusDecoder {
            params: params.clone(),
            buf: AudioBuffer::unused(),
        })
    }

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[
            support_codec!(CODEC_TYPE_OPUS, "opus", "Opus"),
        ]
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn decode(&mut self, _: &Packet) -> Result<AudioBufferRef<'_>> {
        Ok(self.buf.as_audio_buffer_ref())
    }

    fn close(&mut self) {

    }
}