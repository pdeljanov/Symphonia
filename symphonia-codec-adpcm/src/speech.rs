// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Decoder for the stream-oriented speech ADPCM codecs (ITU-T G.722 and G.726).

use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef,
};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::well_known::{
    CODEC_ID_ADPCM_G722, CODEC_ID_ADPCM_G726, CODEC_ID_ADPCM_G726LE, CODEC_ID_ADPCM_YAMAHA,
};
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::packet::PacketRef;

use crate::codec_g722::G722Decoder;
use crate::codec_g726::G726Decoder;
use crate::codec_yamaha::YamahaChannel;

enum Inner {
    G722(G722Decoder),
    /// A G.726 decoder, and if the codes are packed starting from the least significant bit.
    G726(G726Decoder, bool),
    /// Yamaha ADPCM, with the state of each channel (mono or stereo).
    Yamaha(Vec<YamahaChannel>),
}

pub(crate) struct SpeechDecoder {
    params: AudioCodecParameters,
    info: CodecInfo,
    inner: Inner,
    buf: AudioBuffer<i32>,
}

/// Returns true if the codec is decoded by the `SpeechDecoder`.
pub(crate) fn is_speech_codec(codec: symphonia_core::codecs::audio::AudioCodecId) -> bool {
    matches!(
        codec,
        CODEC_ID_ADPCM_G722 | CODEC_ID_ADPCM_G726 | CODEC_ID_ADPCM_G726LE | CODEC_ID_ADPCM_YAMAHA
    )
}

impl SpeechDecoder {
    pub(crate) fn try_new(
        params: &AudioCodecParameters,
        _opts: &AudioDecoderOptions,
    ) -> Result<Self> {
        let Some(rate) = params.sample_rate
        else {
            return unsupported_error("adpcm: sample rate is required");
        };

        let Some(channels) = params.channels.clone()
        else {
            return unsupported_error("adpcm: channels or channel layout is required");
        };

        let max_channels = if params.codec == CODEC_ID_ADPCM_YAMAHA { 2 } else { 1 };

        if channels.count() > max_channels {
            return unsupported_error("adpcm: unsupported number of channels");
        }

        let (inner, info) = match params.codec {
            CODEC_ID_ADPCM_G722 => (
                Inner::G722(G722Decoder::new()),
                CodecInfo { short_name: "g722", long_name: "ITU-T G.722 ADPCM", profiles: &[] },
            ),
            CODEC_ID_ADPCM_G726 | CODEC_ID_ADPCM_G726LE => {
                let code_size = params.bits_per_coded_sample.unwrap_or(4);

                let Some(decoder) = G726Decoder::new(code_size)
                else {
                    return unsupported_error("adpcm (g726): unsupported bits per sample");
                };

                let little_endian = params.codec == CODEC_ID_ADPCM_G726LE;

                (
                    Inner::G726(decoder, little_endian),
                    CodecInfo { short_name: "g726", long_name: "ITU-T G.726 ADPCM", profiles: &[] },
                )
            }
            CODEC_ID_ADPCM_YAMAHA => (
                Inner::Yamaha(vec![YamahaChannel::new(); channels.count()]),
                CodecInfo { short_name: "adpcm_yamaha", long_name: "ADPCM Yamaha", profiles: &[] },
            ),
            _ => return unsupported_error("adpcm: codec is unsupported"),
        };

        let capacity = params.max_frames_per_packet.unwrap_or(1 << 12) as usize;

        if !symphonia_core::audio::is_reasonable_capacity(capacity, channels.count()) {
            return unsupported_error("adpcm: maximum frames per packet is too large");
        }

        Ok(SpeechDecoder {
            params: params.clone(),
            info,
            inner,
            buf: AudioBuffer::new(AudioSpec::new(rate, channels), capacity),
        })
    }

    fn decode_inner(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        self.buf.clear();

        match &mut self.inner {
            Inner::G722(decoder) => {
                if packet.data.len() * 2 > self.buf.capacity() {
                    return decode_error("adpcm: packet exceeds the maximum frames per packet");
                }
                self.buf.render_uninit(Some(packet.data.len() * 2));

                let Some(plane) = self.buf.plane_mut(0)
                else {
                    return decode_error("adpcm: missing audio plane");
                };

                for (byte, out) in packet.data.iter().zip(plane.chunks_exact_mut(2)) {
                    let [first, second] = decoder.decode_byte(*byte);
                    out[0] = first << 16;
                    out[1] = second << 16;
                }
            }
            Inner::Yamaha(channels) => {
                // A mono stream has two samples per byte, and a stereo stream has one sample of
                // each channel per byte. The lowest nibble is first.
                let num_channels = channels.len();
                let num_frames = packet.data.len() * 2 / num_channels;

                if num_frames > self.buf.capacity() {
                    return decode_error("adpcm: packet exceeds the maximum frames per packet");
                }
                self.buf.render_uninit(Some(num_frames));

                for (i, &byte) in packet.data.iter().enumerate() {
                    let nibbles = [byte & 0xf, byte >> 4];

                    for (j, nibble) in nibbles.into_iter().enumerate() {
                        let (channel, frame) =
                            if num_channels == 1 { (0, 2 * i + j) } else { (j, i) };

                        let sample = channels[channel].decode(nibble) << 16;

                        if let Some(plane) = self.buf.plane_mut(channel) {
                            plane[frame] = sample;
                        }
                    }
                }
            }
            Inner::G726(decoder, little_endian) => {
                let code_size = decoder.code_size() as usize;
                let num_codes = packet.data.len() * 8 / code_size;

                if num_codes > self.buf.capacity() {
                    return decode_error("adpcm: packet exceeds the maximum frames per packet");
                }
                self.buf.render_uninit(Some(num_codes));

                let Some(plane) = self.buf.plane_mut(0)
                else {
                    return decode_error("adpcm: missing audio plane");
                };

                let mut bit_pos = 0;

                for out in plane.iter_mut() {
                    let mut code = 0;

                    for j in 0..code_size {
                        let byte = packet.data[bit_pos / 8];

                        if *little_endian {
                            code |= u32::from((byte >> (bit_pos % 8)) & 1) << j;
                        }
                        else {
                            code = (code << 1) | u32::from((byte >> (7 - bit_pos % 8)) & 1);
                        }

                        bit_pos += 1;
                    }

                    *out = decoder.decode(code) << 16;
                }
            }
        }

        Ok(())
    }
}

impl AudioDecoder for SpeechDecoder {
    fn reset(&mut self) {
        match &mut self.inner {
            Inner::G722(decoder) => *decoder = G722Decoder::new(),
            Inner::Yamaha(channels) => channels.fill(YamahaChannel::new()),
            Inner::G726(decoder, _) => {
                if let Some(fresh) = G726Decoder::new(decoder.code_size()) {
                    *decoder = fresh;
                }
            }
        }
    }

    fn codec_info(&self) -> &CodecInfo {
        &self.info
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
