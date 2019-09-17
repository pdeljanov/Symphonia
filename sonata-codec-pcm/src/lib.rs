// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]

use sonata_core::support_codec;

use sonata_core::audio::{AudioBuffer, AudioBufferRef, AsAudioBufferRef};
use sonata_core::audio::{Duration, Signal, SignalSpec};
use sonata_core::codecs::{CodecParameters, CodecDescriptor, Decoder, DecoderOptions};
// Signed Int PCM codecs
use sonata_core::codecs::{CODEC_TYPE_PCM_S8, CODEC_TYPE_PCM_S16LE};
use sonata_core::codecs::{CODEC_TYPE_PCM_S24LE, CODEC_TYPE_PCM_S32LE};
use sonata_core::codecs::{CODEC_TYPE_PCM_S16BE, CODEC_TYPE_PCM_S24BE, CODEC_TYPE_PCM_S32BE};
// Unsigned Int PCM codecs
use sonata_core::codecs::{CODEC_TYPE_PCM_U8, CODEC_TYPE_PCM_U16LE};
use sonata_core::codecs::{CODEC_TYPE_PCM_U24LE, CODEC_TYPE_PCM_U32LE};
use sonata_core::codecs::{CODEC_TYPE_PCM_U16BE, CODEC_TYPE_PCM_U24BE, CODEC_TYPE_PCM_U32BE};
// Floating point PCM codecs
use sonata_core::codecs::{CODEC_TYPE_PCM_F32LE, CODEC_TYPE_PCM_F32BE};
use sonata_core::codecs::{CODEC_TYPE_PCM_F64LE, CODEC_TYPE_PCM_F64BE};
// G711 ALaw and MuLaw PCM cdoecs.
use sonata_core::codecs::{CODEC_TYPE_PCM_ALAW, CODEC_TYPE_PCM_MULAW};
use sonata_core::conv::FromSample;
use sonata_core::errors::{Result, decode_error, unsupported_error};
use sonata_core::formats::Packet;
use sonata_core::io::ByteStream;

macro_rules! read_pcm_signed {
    ($buf:expr, $read:expr, $shift:expr) => {
        $buf.fill(| audio_planes, idx | -> Result<()> {
            for plane in audio_planes.planes() {
                plane[idx] = (($read as u32) << $shift) as i32;
            }
            Ok(())
        })
    };
}

macro_rules! read_pcm_unsigned {
    ($buf:expr, $read:expr, $shift:expr) => {
        $buf.fill(| audio_planes, idx | -> Result<()> {
            for plane in audio_planes.planes() {
                plane[idx] = (($read as u32) << $shift).wrapping_add(0x80000000) as i32
            }
            Ok(())
        })
    };
}

macro_rules! read_pcm_floating {
    ($buf:expr, $read:expr) => {
        $buf.fill(| audio_planes, idx | -> Result<()> {
            for plane in audio_planes.planes() {
                plane[idx] = i32::from_sample($read);
            }
            Ok(())
        })
    };
}

macro_rules! read_pcm_transfer_func {
    ($buf:expr, $func:expr) => {
        $buf.fill(| audio_planes, idx | -> Result<()> {
            for plane in audio_planes.planes() {
                plane[idx] = i32::from_sample($func);
            }
            Ok(())
        })
    };
}

// alaw_to_linear and mulaw_to_linear are adaptations of alaw2linear and ulaw2linear from g711.c by
// SUN Microsystems (unrestricted use license).
const XLAW_QUANT_MASK: u8 = 0x0f;
const XLAW_SEG_MASK: u8   = 0x70;
const XLAW_SEG_SHIFT: u32 = 4;

fn alaw_to_linear(mut a_val: u8) -> i16 {
    a_val ^= 0x55;

    let mut t = i16::from((a_val & XLAW_QUANT_MASK) << 4);
    let seg = (a_val & XLAW_SEG_MASK) >> XLAW_SEG_SHIFT;

    match seg {
        0 => t += 0x8,
        1 => t += 0x108,
        _ => t = (t + 0x108) << (seg - 1),
    }

    if a_val & 0x80 == 0x80 { t } else { -t }
}

fn mulaw_to_linear(mut mu_val: u8) -> i16 {
    const BIAS: i16 = 0x84;

    // Complement to obtain normal u-law value.
    mu_val = !mu_val;

    // Extract and bias the quantization bits. Then shift up by the segment number and subtract out
    // the bias.
    let mut t = i16::from((mu_val & XLAW_QUANT_MASK) << 3) + BIAS;
    t <<= (mu_val & XLAW_SEG_MASK) >> XLAW_SEG_SHIFT;

    if mu_val & 0x80 == 0x80 { t - BIAS } else { BIAS - t }
}

/// `PcmDecoder` implements a decoder for all raw PCM, and log-PCM codecs.
pub struct PcmDecoder {
    params: CodecParameters,
    buf: AudioBuffer<i32>,
}

impl Decoder for PcmDecoder {

    fn try_new(params: &CodecParameters, _options: &DecoderOptions) -> Result<Self> {
        let frames = match params.max_frames_per_packet {
            Some(frames) => frames,
            _            => return unsupported_error("pcm: maximum frames per packet is required"),
        };

        let rate = match params.sample_rate {
            Some(rate) => rate,
            _          => return unsupported_error("pcm: sample rate is required")
        };

        let spec = if let Some(channels) = params.channels {
            SignalSpec::new(rate, channels)
        }
        else if let Some(layout) = params.channel_layout {
            SignalSpec::new_with_layout(rate, layout)
        }
        else {
            return unsupported_error("pcm: channels or channel_layout is required");
        };

        Ok(PcmDecoder {
            params: params.clone(),
            buf: AudioBuffer::new(Duration::Frames(frames), spec),
        })
    }

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[
            support_codec!(
                CODEC_TYPE_PCM_S32LE,
                "pcm_s32le",
                "PCM Signed 32-bit Little-Endian Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_S32BE,
                "pcm_s32be",
                "PCM Signed 32-bit Big-Endian Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_S24LE,
                "pcm_s24le",
                "PCM Signed 24-bit Little-Endian Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_S24BE,
                "pcm_s24be",
                "PCM Signed 24-bit Big-Endian Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_S16LE,
                "pcm_s16le",
                "PCM Signed 16-bit Little-Endian Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_S16BE,
                "pcm_s16be",
                "PCM Signed 16-bit Big-Endian Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_S8,
                "pcm_s8",
                "PCM Signed 8-bit Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_U32LE,
                "pcm_u32le",
                "PCM Unsigned 32-bit Little-Endian Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_U32BE,
                "pcm_u32be",
                "PCM Unsigned 32-bit Big-Endian Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_U24LE,
                "pcm_u24le",
                "PCM Unsigned 24-bit Little-Endian Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_U24BE,
                "pcm_u24be",
                "PCM Unsigned 24-bit Big-Endian Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_U16LE,
                "pcm_u16le",
                "PCM Unsigned 16-bit Little-Endian Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_U16BE,
                "pcm_u16be",
                "PCM Unsigned 16-bit Big-Endian Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_U8,
                "pcm_u8",
                "PCM Unsigned 8-bit Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_F32LE,
                "pcm_f32le",
                "PCM 32-bit Little-Endian Floating Point Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_F32BE,
                "pcm_f32be",
                "PCM 32-bit Big-Endian Floating Point Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_F64LE,
                "pcm_f64le",
                "PCM 64-bit Little-Endian Floating Point Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_F64BE,
                "pcm_f64be",
                "PCM 64-bit Big-Endian Floating Point Interleaved"
            ),
            support_codec!(
                CODEC_TYPE_PCM_ALAW ,
                "pcm_alaw" ,
                "PCM A-law"
            ),
            support_codec!(
                CODEC_TYPE_PCM_MULAW,
                "pcm_mulaw",
                "PCM Mu-law"
            ),
            // support_codec!(
            //     CODEC_TYPE_PCM_S32LE_PLANAR,
            //     "pcm_s32le_planar",
            //     "PCM Signed 32-bit Little-Endian Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_S32BE_PLANAR,
            //     "pcm_s32be_planar",
            //     "PCM Signed 32-bit Big-Endian Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_S24LE_PLANAR,
            //     "pcm_s24le_planar",
            //     "PCM Signed 24-bit Little-Endian Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_S24BE_PLANAR,
            //     "pcm_s24be_planar",
            //     "PCM Signed 24-bit Big-Endian Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_S16LE_PLANAR,
            //     "pcm_s16le_planar",
            //     "PCM Signed 16-bit Little-Endian Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_S16BE_PLANAR,
            //     "pcm_s16be_planar",
            //     "PCM Signed 16-bit Big-Endian Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_S8_PLANAR   ,
            //     "pcm_s8_planar"   ,
            //     "PCM Signed 8-bit Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_U32LE_PLANAR,
            //     "pcm_u32le_planar",
            //     "PCM Unsigned 32-bit Little-Endian Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_U32BE_PLANAR,
            //     "pcm_u32be_planar",
            //     "PCM Unsigned 32-bit Big-Endian Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_U24LE_PLANAR,
            //     "pcm_u24le_planar",
            //     "PCM Unsigned 24-bit Little-Endian Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_U24BE_PLANAR,
            //     "pcm_u24be_planar",
            //     "PCM Unsigned 24-bit Big-Endian Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_U16LE_PLANAR,
            //     "pcm_u16le_planar",
            //     "PCM Unsigned 16-bit Little-Endian Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_U16BE_PLANAR,
            //     "pcm_u16be_planar",
            //     "PCM Unsigned 16-bit Big-Endian Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_U8_PLANAR   ,
            //     "pcm_u8_planar"   ,
            //     "PCM Unsigned 8-bit Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_F32LE_PLANAR,
            //     "pcm_f32le_planar",
            //     "PCM 32-bit Little-Endian Floating Point Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_F32BE_PLANAR,
            //     "pcm_f32be_planar",
            //     "PCM 32-bit Big-Endian Floating Point Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_F64LE_PLANAR,
            //     "pcm_f64le_planar",
            //     "PCM 64-bit Little-Endian Floating Point Planar"
            // ),
            // support_codec!(
            //     CODEC_TYPE_PCM_F64BE_PLANAR,
            //     "pcm_f64be_planar",
            //     "PCM 64-bit Big-Endian Floating Point Planar"
            // ),
        ]
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: Packet<'_>) -> Result<AudioBufferRef<'_>> {
        let mut stream = packet.into_stream();

        let width = self.params.bits_per_coded_sample.unwrap_or_else(
            || self.params.bits_per_sample.unwrap_or(0));

        // Only A-Law and Mu-Law codecs have an implicit coded sample bit-width (8 bits), other PCM
        // codecs require atleast either bits_per_sample or bits_per_coded_sample to be declared.
        if width == 0 &&
           self.params.codec != CODEC_TYPE_PCM_ALAW &&
           self.params.codec != CODEC_TYPE_PCM_MULAW
        {
            return decode_error("pcm: unknown bits per (coded) sample.");
        }

        // Signed or unsigned integer PCM codecs must be shifted to expand the sample into the
        // entire i32 range. Only floating point samples may exceed 32 bits per coded sample, but
        // they cannot be shifted, so int_shift = 0.
        let int_shift = if width <= 32 { 32 - width } else { 0 };

        match self.params.codec {
            CODEC_TYPE_PCM_S32LE => read_pcm_signed!(self.buf,   stream.read_u32()?,    int_shift),
            CODEC_TYPE_PCM_S32BE => read_pcm_signed!(self.buf,   stream.read_be_u32()?, int_shift),
            CODEC_TYPE_PCM_S24LE => read_pcm_signed!(self.buf,   stream.read_u24()?,    int_shift),
            CODEC_TYPE_PCM_S24BE => read_pcm_signed!(self.buf,   stream.read_be_u24()?, int_shift),
            CODEC_TYPE_PCM_S16LE => read_pcm_signed!(self.buf,   stream.read_u16()?,    int_shift),
            CODEC_TYPE_PCM_S16BE => read_pcm_signed!(self.buf,   stream.read_be_u16()?, int_shift),
            CODEC_TYPE_PCM_S8    => read_pcm_signed!(self.buf,   stream.read_u8()?,     int_shift),
            CODEC_TYPE_PCM_U32LE => read_pcm_unsigned!(self.buf, stream.read_u32()?,    int_shift),
            CODEC_TYPE_PCM_U32BE => read_pcm_unsigned!(self.buf, stream.read_be_u32()?, int_shift),
            CODEC_TYPE_PCM_U24LE => read_pcm_unsigned!(self.buf, stream.read_u24()?,    int_shift),
            CODEC_TYPE_PCM_U24BE => read_pcm_unsigned!(self.buf, stream.read_be_u24()?, int_shift),
            CODEC_TYPE_PCM_U16LE => read_pcm_unsigned!(self.buf, stream.read_u16()?,    int_shift),
            CODEC_TYPE_PCM_U16BE => read_pcm_unsigned!(self.buf, stream.read_be_u16()?, int_shift),
            CODEC_TYPE_PCM_U8    => read_pcm_unsigned!(self.buf, stream.read_u8()?,     int_shift),
            CODEC_TYPE_PCM_F32LE => read_pcm_floating!(self.buf, stream.read_f32()?),
            CODEC_TYPE_PCM_F32BE => read_pcm_floating!(self.buf, stream.read_be_f32()?),
            CODEC_TYPE_PCM_F64LE => read_pcm_floating!(self.buf, stream.read_f64()?),
            CODEC_TYPE_PCM_F64BE => read_pcm_floating!(self.buf, stream.read_be_f64()?),
            CODEC_TYPE_PCM_ALAW  => {
                read_pcm_transfer_func!(self.buf, alaw_to_linear(stream.read_u8()?))
            },
            CODEC_TYPE_PCM_MULAW => {
                read_pcm_transfer_func!(self.buf, mulaw_to_linear(stream.read_u8()?))
            },
            // CODEC_TYPE_PCM_S32LE_PLANAR =>
            // CODEC_TYPE_PCM_S32BE_PLANAR =>
            // CODEC_TYPE_PCM_S24LE_PLANAR =>
            // CODEC_TYPE_PCM_S24BE_PLANAR =>
            // CODEC_TYPE_PCM_S16LE_PLANAR =>
            // CODEC_TYPE_PCM_S16BE_PLANAR =>
            // CODEC_TYPE_PCM_S8_PLANAR    =>
            // CODEC_TYPE_PCM_U32LE_PLANAR =>
            // CODEC_TYPE_PCM_U32BE_PLANAR =>
            // CODEC_TYPE_PCM_U24LE_PLANAR =>
            // CODEC_TYPE_PCM_U24BE_PLANAR =>
            // CODEC_TYPE_PCM_U16LE_PLANAR =>
            // CODEC_TYPE_PCM_U16BE_PLANAR =>
            // CODEC_TYPE_PCM_U8_PLANAR    =>
            // CODEC_TYPE_PCM_F32LE_PLANAR =>
            // CODEC_TYPE_PCM_F32BE_PLANAR =>
            // CODEC_TYPE_PCM_F64LE_PLANAR =>
            // CODEC_TYPE_PCM_F64BE_PLANAR =>
            _ => return unsupported_error("pcm: codec is unsupported.")
        }?;

        Ok(self.buf.as_audio_buffer_ref())
    }

    fn close(&mut self) {
        // Intentionally left empty.
    }
}