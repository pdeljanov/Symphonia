// Symphonia APE Bundle
// Copyright (c) 2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

mod demuxer;

pub use demuxer::ApeReader;

use symphonia_core::audio::{AsAudioBufferRef, AudioBuffer, AudioBufferRef, Signal, SignalSpec};
use symphonia_core::codecs::{
    CodecDescriptor, CodecParameters, Decoder, DecoderOptions, FinalizeResult,
    CODEC_TYPE_MONKEYS_AUDIO,
};
use symphonia_core::errors::{decode_error, unsupported_error, Result};
use symphonia_core::formats::Packet;
use symphonia_core::support_codec;

use ape_decoder::FrameDecoder;

/// Monkey's Audio (APE) decoder.
pub struct ApeDecoder {
    params: CodecParameters,
    /// Internal audio buffer (planar i32).
    buf: AudioBuffer<i32>,
    /// The ape-decoder frame decoder for raw frame byte decoding.
    frame_decoder: FrameDecoder,
    /// Number of channels.
    channels: u16,
    /// Bits per sample.
    bits_per_sample: u16,
}

impl Decoder for ApeDecoder {
    fn try_new(params: &CodecParameters, _options: &DecoderOptions) -> Result<Self>
    where
        Self: Sized,
    {
        // Validate codec type.
        let codec = params.codec;
        if codec != CODEC_TYPE_MONKEYS_AUDIO {
            return unsupported_error("ape: unsupported codec type");
        }

        // Extract required parameters.
        let sample_rate = params
            .sample_rate
            .ok_or(symphonia_core::errors::Error::DecodeError("ape: missing sample rate"))?;

        let channels_mask = params
            .channels
            .ok_or(symphonia_core::errors::Error::DecodeError("ape: missing channels"))?;

        let bits_per_sample = params
            .bits_per_sample
            .ok_or(symphonia_core::errors::Error::DecodeError("ape: missing bits per sample"))?
            as u16;

        let max_frames = params.max_frames_per_packet.unwrap_or(73728 * 4) as usize;

        // Extract codec-specific parameters from extra_data.
        // Layout: version(u16), compression_level(u16), bits_per_sample(u16),
        //         channels(u16), format_flags(u16), padding(u16)
        let extra = params
            .extra_data
            .as_ref()
            .ok_or(symphonia_core::errors::Error::DecodeError("ape: missing extra_data"))?;

        if extra.len() < 10 {
            return decode_error("ape: extra_data too short");
        }

        let version = u16::from_le_bytes([extra[0], extra[1]]);
        let compression_level = u16::from_le_bytes([extra[2], extra[3]]);
        let channels_count = u16::from_le_bytes([extra[6], extra[7]]);

        // Create the ape-decoder FrameDecoder.
        let frame_decoder =
            FrameDecoder::new(version, channels_count, bits_per_sample, compression_level)
                .map_err(|e| match e {
                    ape_decoder::ApeError::UnsupportedVersion(_) => {
                        symphonia_core::errors::Error::Unsupported("ape: unsupported version")
                    }
                    ape_decoder::ApeError::InvalidFormat(msg) => {
                        symphonia_core::errors::Error::DecodeError(msg)
                    }
                    _ => symphonia_core::errors::Error::DecodeError("ape: invalid decoder params"),
                })?;

        // Create the audio buffer.
        let spec = SignalSpec::new(sample_rate, channels_mask);
        let buf = AudioBuffer::new(max_frames as u64, spec);

        // Use the channel count from the Channels bitmask (which matches the AudioBuffer)
        // rather than extra_data, to guarantee consistency. These should be equal since
        // the demuxer sets both from the same source, but this is defensive.
        let buf_channels = channels_mask.count() as u16;

        Ok(ApeDecoder {
            params: params.clone(),
            buf,
            frame_decoder,
            channels: buf_channels,
            bits_per_sample,
        })
    }

    fn supported_codecs() -> &'static [CodecDescriptor]
    where
        Self: Sized,
    {
        &[support_codec!(CODEC_TYPE_MONKEYS_AUDIO, "ape", "Monkey's Audio (APE)")]
    }

    fn reset(&mut self) {
        // APE frames are independently decodable (predictors/entropy/range coder
        // are flushed at the start of each frame), so no persistent state to clear.
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: &Packet) -> Result<AudioBufferRef<'_>> {
        if let Err(e) = self.decode_inner(packet) {
            self.buf.clear();
            Err(e)
        }
        else {
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

impl ApeDecoder {
    fn decode_inner(&mut self, packet: &Packet) -> Result<()> {
        let data = &packet.data;

        // The first 4 bytes are the seek_remainder, prepended by the FormatReader.
        if data.len() < 5 {
            return decode_error("ape: packet too short");
        }

        let seek_remainder = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let frame_data = &data[4..];
        let frame_blocks = packet.dur as usize;

        // Decode the compressed frame to PCM bytes using ape-decoder.
        let pcm_bytes = self.frame_decoder.decode_frame(frame_data, seek_remainder, frame_blocks)
            .map_err(|e| match e {
                ape_decoder::ApeError::InvalidChecksum => {
                    symphonia_core::errors::Error::DecodeError("ape: frame CRC mismatch")
                }
                ape_decoder::ApeError::DecodingError(msg) => {
                    symphonia_core::errors::Error::DecodeError(msg)
                }
                ape_decoder::ApeError::InvalidFormat(msg) => {
                    symphonia_core::errors::Error::DecodeError(msg)
                }
                _ => symphonia_core::errors::Error::DecodeError("ape: decode error"),
            })?;

        // Convert interleaved PCM bytes to planar AudioBuffer<i32>.
        self.buf.clear();
        self.buf.render_reserved(Some(frame_blocks));

        let ch_count = self.channels as usize;

        match self.bits_per_sample {
            8 => {
                // 8-bit unsigned PCM: one byte per sample. APE unprepare adds +128
                // bias, so silence=128, negative peak=0, positive peak=255. Convert
                // to signed by subtracting 128.
                if pcm_bytes.len() < frame_blocks * ch_count {
                    return decode_error("ape: PCM data too short for 8-bit");
                }
                for frame in 0..frame_blocks {
                    for ch in 0..ch_count {
                        let sample = pcm_bytes[frame * ch_count + ch] as i32 - 128;
                        self.buf.chan_mut(ch)[frame] = sample;
                    }
                }
            }
            16 => {
                // 16-bit signed LE PCM: two bytes per sample.
                let bytes_per_frame = ch_count * 2;
                if pcm_bytes.len() < frame_blocks * bytes_per_frame {
                    return decode_error("ape: PCM data too short for 16-bit");
                }
                for frame in 0..frame_blocks {
                    for ch in 0..ch_count {
                        let offset = (frame * ch_count + ch) * 2;
                        let sample = i16::from_le_bytes([
                            pcm_bytes[offset],
                            pcm_bytes[offset + 1],
                        ]) as i32;
                        self.buf.chan_mut(ch)[frame] = sample;
                    }
                }
            }
            24 => {
                // 24-bit signed LE PCM: three bytes per sample, sign-extend to i32.
                let bytes_per_frame = ch_count * 3;
                if pcm_bytes.len() < frame_blocks * bytes_per_frame {
                    return decode_error("ape: PCM data too short for 24-bit");
                }
                for frame in 0..frame_blocks {
                    for ch in 0..ch_count {
                        let offset = (frame * ch_count + ch) * 3;
                        let lo = pcm_bytes[offset] as u32;
                        let mid = pcm_bytes[offset + 1] as u32;
                        let hi = pcm_bytes[offset + 2] as u32;
                        let raw = lo | (mid << 8) | (hi << 16);
                        // Sign-extend from 24 bits.
                        let sample = if raw & 0x80_0000 != 0 {
                            (raw | 0xFF00_0000) as i32
                        }
                        else {
                            raw as i32
                        };
                        self.buf.chan_mut(ch)[frame] = sample;
                    }
                }
            }
            32 => {
                // 32-bit signed LE PCM: four bytes per sample.
                let bytes_per_frame = ch_count * 4;
                if pcm_bytes.len() < frame_blocks * bytes_per_frame {
                    return decode_error("ape: PCM data too short for 32-bit");
                }
                for frame in 0..frame_blocks {
                    for ch in 0..ch_count {
                        let offset = (frame * ch_count + ch) * 4;
                        let sample = i32::from_le_bytes([
                            pcm_bytes[offset],
                            pcm_bytes[offset + 1],
                            pcm_bytes[offset + 2],
                            pcm_bytes[offset + 3],
                        ]);
                        self.buf.chan_mut(ch)[frame] = sample;
                    }
                }
            }
            _ => {
                return unsupported_error("ape: unsupported bit depth");
            }
        }

        // Normalize samples to 32-bit width (left-justify), matching FLAC convention.
        if self.bits_per_sample < 32 {
            let shift = 32 - self.bits_per_sample as u32;
            self.buf.transform(|sample| sample << shift);
        }

        Ok(())
    }
}
