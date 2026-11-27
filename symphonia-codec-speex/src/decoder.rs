// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, Channels, GenericAudioBufferRef,
    Position,
};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::well_known::CODEC_ID_SPEEX;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::packet::PacketRef;
use symphonia_core::support_audio_codec;

use crate::bits::Bits;
use crate::header::SpeexHeader;
use crate::nb::NbDecoder;
use crate::sb::{Layer, SbDecoder};
use crate::stereo::Stereo;

const SPEEX_CODEC_INFO: CodecInfo =
    CodecInfo { short_name: "speex", long_name: "Speex", profiles: &[] };

/// The samples of the 16-bit range that decode to a full scale sample.
const SCALE: f32 = 1.0 / 32768.0;

/// The most frames a packet can hold, bounding the size of the output buffer.
const MAX_FRAMES_PER_PACKET: usize = 8;

pub struct SpeexDecoder {
    params: AudioCodecParameters,
    buf: AudioBuffer<f32>,
    layer: Layer,
    channels: usize,
    frames_per_packet: usize,
}

impl SpeexDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_SPEEX {
            return unsupported_error("speex: invalid codec");
        }

        let Some(extra) = params.extra_data.as_ref()
        else {
            return unsupported_error("speex: missing header");
        };
        let header = SpeexHeader::parse(extra)?;

        let mut layer = match header.mode {
            0 => Layer::Narrow(Box::new(NbDecoder::new())),
            1 => Layer::Split(Box::new(SbDecoder::new(false))),
            _ => Layer::Split(Box::new(SbDecoder::new(true))),
        };

        let channels = header.channels as usize;
        if channels == 2 {
            layer.narrow().stereo = Some(Stereo::new());
        }

        let frames_per_packet = (header.frames_per_packet as usize).min(MAX_FRAMES_PER_PACKET);
        let layout = if channels == 2 {
            Position::FRONT_LEFT | Position::FRONT_RIGHT
        }
        else {
            Position::FRONT_CENTER
        };

        let spec = AudioSpec::new(header.sample_rate, Channels::Positioned(layout));
        let buf = AudioBuffer::new(spec, layer.frame_size() * frames_per_packet);

        let mut params = params.clone();
        params.with_sample_rate(header.sample_rate).with_channels(Channels::Positioned(layout));

        Ok(SpeexDecoder { params, buf, layer, channels, frames_per_packet })
    }

    fn decode_inner(&mut self, data: &[u8]) -> Result<()> {
        self.buf.clear();

        let frame_size = self.layer.frame_size();
        let mut bits = Bits::new(data);
        let mut mono = vec![0f32; frame_size];
        let mut pcm: Vec<Vec<f32>> = vec![Vec::new(); self.channels];

        for n in 0..self.frames_per_packet {
            if let Err(err) = self.layer.decode(&mut bits, &mut mono) {
                if n == 0 {
                    return Err(err);
                }
                break;
            }

            if self.channels == 2 {
                let mut stereo = vec![0f32; 2 * frame_size];
                match self.layer.narrow().stereo.as_mut() {
                    Some(s) => s.expand(&mono, &mut stereo),
                    None => return decode_error("speex: missing stereo state"),
                }
                pcm[0].extend(stereo.iter().step_by(2).map(|v| v * SCALE));
                pcm[1].extend(stereo.iter().skip(1).step_by(2).map(|v| v * SCALE));
            }
            else {
                pcm[0].extend(mono.iter().map(|v| v * SCALE));
            }
        }

        let frames = pcm[0].len();
        self.buf.render_uninit(Some(frames));
        for (ch, samples) in pcm.iter().enumerate() {
            if let Some(dst) = self.buf.plane_mut(ch) {
                dst.copy_from_slice(samples);
            }
        }

        Ok(())
    }
}

impl AudioDecoder for SpeexDecoder {
    fn reset(&mut self) {
        let layer = match &self.layer {
            Layer::Narrow(_) => Layer::Narrow(Box::new(NbDecoder::new())),
            Layer::Split(_) => {
                let ultra = self.layer.frame_size() > 320;
                Layer::Split(Box::new(SbDecoder::new(ultra)))
            }
        };
        let stereo = self.channels == 2;
        self.layer = layer;
        if stereo {
            self.layer.narrow().stereo = Some(Stereo::new());
        }
    }

    fn codec_info(&self) -> &CodecInfo {
        &SPEEX_CODEC_INFO
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

impl RegisterableAudioDecoder for SpeexDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(SpeexDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(CODEC_ID_SPEEX, "speex", "Speex")]
    }
}
