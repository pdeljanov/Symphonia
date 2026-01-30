// Symphonia DSD Codec
// Copyright (c) 2026 M0Rf30
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

use symphonia_core::audio::{AsAudioBufferRef, AudioBuffer, AudioBufferRef, Signal, SignalSpec};
use symphonia_core::codecs::{decl_codec_type, CodecDescriptor, CodecParameters, CodecType};
use symphonia_core::codecs::{Decoder, DecoderOptions, FinalizeResult};
use symphonia_core::errors::{decode_error, unsupported_error, Result};
use symphonia_core::formats::Packet;
use symphonia_core::support_codec;

use log::debug;

/// DSD codec type "DSD\0"
pub const CODEC_TYPE_DSD: CodecType = decl_codec_type(b"DSD\0");

/// DSD Decoder
///
/// This decoder implements native DSD pass-through. It reads DSD data from
/// packets and stores it as U8 samples (1 byte = 8 DSD bits).
///
/// The output can be sent directly to audio outputs that support native DSD
/// (such as cpal with DSD support), or it can be converted to PCM by a
/// separate converter component.
pub struct DsdDecoder {
    params: CodecParameters,
    buf: AudioBuffer<u8>,
}

impl Decoder for DsdDecoder {
    fn try_new(params: &CodecParameters, _options: &DecoderOptions) -> Result<Self> {
        // Verify this is a DSD codec
        if params.codec != CODEC_TYPE_DSD {
            return unsupported_error("dsd: codec type is not DSD");
        }

        // Get the signal specification
        let sample_rate = match params.sample_rate {
            Some(rate) => rate,
            None => return decode_error("dsd: missing sample rate"),
        };

        let channels = match params.channels {
            Some(ch) => ch,
            None => return decode_error("dsd: missing channel layout"),
        };

        let spec = SignalSpec::new(sample_rate, channels);

        // Calculate duration from codec parameters
        // For DSD, each packet typically contains one block
        let duration = params.max_frames_per_packet.unwrap_or(4096);

        debug!(
            "DSD decoder initialized: rate={}, channels={}, duration={}",
            spec.rate,
            spec.channels.count(),
            duration
        );

        // Create audio buffer for DSD data (U8 format)
        let buf = AudioBuffer::new(duration, spec);

        Ok(DsdDecoder { params: params.clone(), buf })
    }

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[support_codec!(CODEC_TYPE_DSD, "dsd", "Direct Stream Digital")]
    }

    fn reset(&mut self) {
        // Clear the buffer
        self.buf.clear();
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: &Packet) -> Result<AudioBufferRef<'_>> {
        // DSD is stored as packed bytes (8 bits per byte)
        // We'll read the packet data directly into the buffer

        let data = packet.buf();
        let channels = self.buf.spec().channels.count();

        // Calculate samples per channel in this packet
        let samples_per_channel = data.len() / channels;

        // Make sure we have enough space
        if samples_per_channel > self.buf.capacity() {
            return decode_error("dsd: packet too large for buffer");
        }

        // Clear and resize buffer for this packet
        self.buf.clear();
        self.buf.render_reserved(Some(samples_per_channel));

        // Fill buffer with DSD data
        // DSD data is interleaved by default in DSF files
        self.buf.fill(|audio_planes, idx| -> Result<()> {
            let data_offset = idx * channels;
            for (ch, plane) in audio_planes.planes().iter_mut().enumerate() {
                if data_offset + ch < data.len() {
                    plane[idx] = data[data_offset + ch];
                }
                else {
                    plane[idx] = 0x69; // DSD silence pattern
                }
            }
            Ok(())
        })?;

        Ok(self.buf.as_audio_buffer_ref())
    }

    fn finalize(&mut self) -> FinalizeResult {
        Default::default()
    }

    fn last_decoded(&self) -> AudioBufferRef<'_> {
        self.buf.as_audio_buffer_ref()
    }
}
