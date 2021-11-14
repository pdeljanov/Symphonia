// Symphonia
// Copyright (c) 2019-2021 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::audio::{AudioBuffer, AudioBufferRef, AsAudioBufferRef, Signal};
use symphonia_core::codecs::{CODEC_TYPE_MP3, CodecParameters, CodecDescriptor};
use symphonia_core::codecs::{Decoder, DecoderOptions, FinalizeResult};
use symphonia_core::errors::{Result, decode_error};
use symphonia_core::formats::Packet;
use symphonia_core::support_codec;

use super::{common::*, header, layer3};

/// MPEG1 and MPEG2 Layer 1, 2, and 3 decoder.
pub struct Mp3Decoder {
    params: CodecParameters,
    state: State,
    buf: AudioBuffer<f32>,
}

impl Mp3Decoder {
    fn decode_inner(&mut self, packet: &Packet) -> Result<()> {
        let mut reader = packet.as_buf_reader();

        let header = header::read_frame_header(&mut reader)?;

        // The buffer can only be created after the first frame is decoded. Technically, it can
        // change throughout the stream as well...
        if self.buf.is_unused() {
            self.buf = AudioBuffer::new(1152, header.spec());
        }

        // Clear the audio output buffer.
        self.buf.clear();

        // Choose the decode step based on the MPEG layer and the current codec type.
        match header.layer {
            MpegLayer::Layer3 if self.params.codec == CODEC_TYPE_MP3 => {
                // Layer 3
                layer3::decode_frame(&mut reader, &header, &mut self.state, &mut self.buf)?;
            },
            _ => return decode_error("mp3: invalid mpeg audio layer"),
        }

        Ok(())
    }
}

impl Decoder for Mp3Decoder {

    fn try_new(params: &CodecParameters, _: &DecoderOptions) -> Result<Self> {
        Ok(Mp3Decoder {
            params: params.clone(),
            state: State::new(),
            buf: AudioBuffer::unused(),
        })
    }

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[
            // support_codec!(CODEC_TYPE_MP1, "mp1", "MPEG Audio Layer 1"),
            // support_codec!(CODEC_TYPE_MP2, "mp2", "MPEG Audio Layer 2"),
            support_codec!(CODEC_TYPE_MP3, "mp3", "MPEG Audio Layer 3"),
        ]
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn reset(&mut self) {
        // Fully reset the decoder state.
        self.state = State::new();
    }

    fn decode(&mut self, packet: &Packet) -> Result<AudioBufferRef<'_>> {
        if let Err(e) = self.decode_inner(packet) {
            self.buf.clear();
            Err(e)
        } else {
            Ok(self.buf.as_audio_buffer_ref())
        }
    }

    fn finalize(&mut self) -> FinalizeResult {
        Default::default()
    }

    fn last_decoded(&self) -> AudioBufferRef<'_> {
        self.buf.as_audio_buffer_ref()
    }
}