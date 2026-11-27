// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
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

use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::support_audio_codec;

use symphonia_core::audio::sample::{Sample, i24, u24};
use symphonia_core::audio::{
    AsGenericAudioBufferRef, Audio, AudioBuffer, AudioMut, AudioSpec, GenericAudioBuffer,
    GenericAudioBufferRef,
};
use symphonia_core::codecs::audio::{
    AudioCodecId, AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
// Signed Int PCM codecs
use symphonia_core::codecs::audio::well_known::{CODEC_ID_PCM_S8, CODEC_ID_PCM_S16LE};
use symphonia_core::codecs::audio::well_known::{
    CODEC_ID_PCM_S16BE, CODEC_ID_PCM_S24BE, CODEC_ID_PCM_S32BE,
};
use symphonia_core::codecs::audio::well_known::{CODEC_ID_PCM_S24LE, CODEC_ID_PCM_S32LE};
// Unsigned Int PCM codecs
use symphonia_core::codecs::audio::well_known::{CODEC_ID_PCM_U8, CODEC_ID_PCM_U16LE};
use symphonia_core::codecs::audio::well_known::{
    CODEC_ID_PCM_U16BE, CODEC_ID_PCM_U24BE, CODEC_ID_PCM_U32BE,
};
use symphonia_core::codecs::audio::well_known::{CODEC_ID_PCM_U24LE, CODEC_ID_PCM_U32LE};
// Floating point PCM codecs
use symphonia_core::codecs::audio::well_known::{CODEC_ID_PCM_F32BE, CODEC_ID_PCM_F32LE};
use symphonia_core::codecs::audio::well_known::{CODEC_ID_PCM_F64BE, CODEC_ID_PCM_F64LE};
// G711 ALaw and MuLaw PCM codecs
use symphonia_core::audio::conv::IntoSample;
use symphonia_core::audio::sample::SampleFormat;
use symphonia_core::codecs::audio::well_known::{CODEC_ID_PCM_ALAW, CODEC_ID_PCM_MULAW};
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::io::ReadBytes;
use symphonia_core::packet::PacketRef;

#[inline(never)]
fn decode_interleaved<S, const SAMPLE_BYTES: usize, F>(
    buf: &mut AudioBuffer<S>,
    data: &[u8],
    num_frames: usize,
    decode: F,
) where
    S: Sample,
    F: Fn([u8; SAMPLE_BYTES]) -> S,
{
    buf.resize_uninit(num_frames);
    let num_channels = buf.num_planes();
    let frame_bytes = num_channels * SAMPLE_BYTES;
    let payload = &data[..num_frames * frame_bytes];

    match num_channels {
        1 => {
            let dst = buf.plane_mut(0).expect("mono buffer has plane 0");
            for (out, chunk) in dst.iter_mut().zip(payload.chunks_exact(SAMPLE_BYTES)) {
                *out = decode(chunk.try_into().expect("chunk matches SAMPLE_BYTES"));
            }
        }
        2 => {
            let (left, right) = buf.plane_pair_mut(0, 1).expect("stereo buffer has planes 0 and 1");
            for ((l, r), chunk) in
                left.iter_mut().zip(right.iter_mut()).zip(payload.chunks_exact(2 * SAMPLE_BYTES))
            {
                *l = decode(chunk[..SAMPLE_BYTES].try_into().expect("chunk matches SAMPLE_BYTES"));
                *r = decode(chunk[SAMPLE_BYTES..].try_into().expect("chunk matches SAMPLE_BYTES"));
            }
        }
        _ => {
            for (ch, plane) in buf.iter_planes_mut().enumerate() {
                let offset = ch * SAMPLE_BYTES;
                for (out, frame) in plane.iter_mut().zip(payload.chunks_exact(frame_bytes)) {
                    *out = decode(
                        frame[offset..offset + SAMPLE_BYTES]
                            .try_into()
                            .expect("slice matches SAMPLE_BYTES"),
                    );
                }
            }
        }
    }
}

#[inline(always)]
fn decode_endian<S, const N: usize, Le, Be>(
    buf: &mut AudioBuffer<S>,
    data: &[u8],
    num_frames: usize,
    is_be: bool,
    le: Le,
    be: Be,
) where
    S: Sample,
    Le: Fn([u8; N]) -> S,
    Be: Fn([u8; N]) -> S,
{
    if is_be {
        decode_interleaved::<S, N, _>(buf, data, num_frames, be);
    }
    else {
        decode_interleaved::<S, N, _>(buf, data, num_frames, le);
    }
}

macro_rules! read_pcm_signed {
    ($buf:expr, $fmt:tt, $read:expr, $width:expr, $shift:expr) => {
        // Get buffer of the correct sample format.
        match $buf {
            GenericAudioBuffer::$fmt(ref mut buf) => {
                // Read samples.
                buf.render_with(None, |idx, audio_planes| -> Result<()> {
                    for plane in audio_planes.iter_mut() {
                        plane[idx] = ($read << $shift).into_sample();
                    }
                    Ok(())
                })
            }
            _ => unreachable!(),
        }
    };
}

macro_rules! read_pcm_unsigned {
    ($buf:expr, $fmt:tt, $read:expr, $width:expr, $shift:expr) => {
        // Get buffer of the correct sample format.
        match $buf {
            GenericAudioBuffer::$fmt(ref mut buf) => {
                // Read samples.
                buf.render_with(None, |idx, audio_planes| -> Result<()> {
                    for plane in audio_planes.iter_mut() {
                        plane[idx] = ($read << $shift).into_sample();
                    }
                    Ok(())
                })
            }
            _ => unreachable!(),
        }
    };
}

macro_rules! read_pcm_floating {
    ($buf:expr, $fmt:tt, $read:expr) => {
        // Get buffer of the correct sample format.
        match $buf {
            GenericAudioBuffer::$fmt(ref mut buf) => {
                // Read samples.
                buf.render_with(None, |idx, audio_planes| -> Result<()> {
                    for plane in audio_planes.iter_mut() {
                        plane[idx] = $read;
                    }
                    Ok(())
                })
            }
            _ => unreachable!(),
        }
    };
}

macro_rules! read_pcm_transfer_func {
    ($buf:expr, $fmt:tt, $func:expr) => {
        // Get buffer of the correct sample format.
        match $buf {
            GenericAudioBuffer::$fmt(ref mut buf) => {
                // Read samples.
                buf.render_with(None, |idx, audio_planes| -> Result<()> {
                    for plane in audio_planes.iter_mut() {
                        plane[idx] = $func;
                    }
                    Ok(())
                })
            }
            _ => unreachable!(),
        }
    };
}

// alaw_to_linear and mulaw_to_linear are adaptations of alaw2linear and ulaw2linear from g711.c by
// SUN Microsystems (unrestricted use license), license text for these parts follow below.
//
// ---
//
// This source code is a product of Sun Microsystems, Inc. and is provided for
// unrestricted use.  Users may copy or modify this source code without
// charge.
//
// SUN SOURCE CODE IS PROVIDED AS IS WITH NO WARRANTIES OF ANY KIND INCLUDING THE
// WARRANTIES OF DESIGN, MERCHANTIBILITY AND FITNESS FOR A PARTICULAR
// PURPOSE, OR ARISING FROM A COURSE OF DEALING, USAGE OR TRADE PRACTICE.
//
// Sun source code is provided with no support and without any obligation on the
// part of Sun Microsystems, Inc. to assist in its use, correction,
// modification or enhancement.
//
// SUN MICROSYSTEMS, INC. SHALL HAVE NO LIABILITY WITH RESPECT TO THE
// INFRINGEMENT OF COPYRIGHTS, TRADE SECRETS OR ANY PATENTS BY THIS SOFTWARE
// OR ANY PART THEREOF.
//
// In no event will Sun Microsystems, Inc. be liable for any lost revenue
// or profits or other special, indirect and consequential damages, even if
// Sun has been advised of the possibility of such damages.
//
// Sun Microsystems, Inc.
// 2550 Garcia Avenue
// Mountain View, California  94043
const XLAW_QUANT_MASK: u8 = 0x0f;
const XLAW_SEG_MASK: u8 = 0x70;
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

    if mu_val & 0x80 == 0x80 { BIAS - t } else { t - BIAS }
}

fn is_supported_pcm_codec(codec_id: AudioCodecId) -> bool {
    matches!(
        codec_id,
        CODEC_ID_PCM_S32LE
            | CODEC_ID_PCM_S32BE
            | CODEC_ID_PCM_S24LE
            | CODEC_ID_PCM_S24BE
            | CODEC_ID_PCM_S16LE
            | CODEC_ID_PCM_S16BE
            | CODEC_ID_PCM_S8
            | CODEC_ID_PCM_U32LE
            | CODEC_ID_PCM_U32BE
            | CODEC_ID_PCM_U24LE
            | CODEC_ID_PCM_U24BE
            | CODEC_ID_PCM_U16LE
            | CODEC_ID_PCM_U16BE
            | CODEC_ID_PCM_U8
            | CODEC_ID_PCM_F32LE
            | CODEC_ID_PCM_F32BE
            | CODEC_ID_PCM_F64LE
            | CODEC_ID_PCM_F64BE
            | CODEC_ID_PCM_ALAW
            | CODEC_ID_PCM_MULAW
    )
}

/// Pulse Code Modulation (PCM) decoder for all raw PCM, and log-PCM codecs.
pub struct PcmDecoder {
    params: AudioCodecParameters,
    /// The number of bits a coded sample should be shifted up to reach the decoded sample width.
    shift: u32,
    /// The number of bytes per coded frame.
    bytes_per_coded_frame: usize,
    buf: GenericAudioBuffer,
}

impl PcmDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        // This decoder only supports certain PCM codecs.
        if !is_supported_pcm_codec(params.codec) {
            return unsupported_error("pcm: invalid codec");
        }

        let rate = match params.sample_rate {
            Some(rate) => rate,
            _ => return unsupported_error("pcm: sample rate is required"),
        };

        let spec = if let Some(channels) = &params.channels {
            // Atleast one channel is required.
            if channels.count() < 1 {
                return unsupported_error("pcm: number of channels cannot be 0");
            }

            AudioSpec::new(rate, channels.clone())
        }
        else {
            return unsupported_error("pcm: channels or channel_layout is required");
        };

        // If the maximum number of frames is known, use it to pre-allocate the audio buffer.
        // Otherwise, start with a 0 capacity and let the buffer grow dynamically while decoding.
        let capacity = params.max_frames_per_packet.unwrap_or(0) as usize;

        if !symphonia_core::audio::is_reasonable_capacity(
            capacity,
            params.channels.as_ref().map_or(1, |c| c.count()),
        ) {
            return unsupported_error("pcm: maximum frames per packet is too large");
        }

        // Determine the sample format for the audio buffer based on the codec ID.
        let (sample_format, sample_format_width, bytes_per_coded_sample) = match params.codec {
            CODEC_ID_PCM_S32LE | CODEC_ID_PCM_S32BE => (SampleFormat::S32, 32, 4),
            CODEC_ID_PCM_S24LE | CODEC_ID_PCM_S24BE => (SampleFormat::S24, 24, 3),
            CODEC_ID_PCM_S16LE | CODEC_ID_PCM_S16BE => (SampleFormat::S16, 16, 2),
            CODEC_ID_PCM_S8 => (SampleFormat::S8, 8, 1),
            CODEC_ID_PCM_U32LE | CODEC_ID_PCM_U32BE => (SampleFormat::U32, 32, 4),
            CODEC_ID_PCM_U24LE | CODEC_ID_PCM_U24BE => (SampleFormat::U24, 24, 3),
            CODEC_ID_PCM_U16LE | CODEC_ID_PCM_U16BE => (SampleFormat::U16, 16, 2),
            CODEC_ID_PCM_U8 => (SampleFormat::U8, 8, 1),
            CODEC_ID_PCM_F32LE | CODEC_ID_PCM_F32BE => (SampleFormat::F32, 32, 4),
            CODEC_ID_PCM_F64LE | CODEC_ID_PCM_F64BE => (SampleFormat::F64, 64, 8),
            CODEC_ID_PCM_ALAW => (SampleFormat::S16, 16, 1),
            CODEC_ID_PCM_MULAW => (SampleFormat::S16, 16, 1),
            _ => unreachable!(),
        };

        // Do not allow the stated bits per coded sample to exceed the intrinsic width of the sample
        // format.
        if let Some(bits_per_coded_sample) = params.bits_per_coded_sample {
            if bits_per_coded_sample > sample_format_width {
                return decode_error("pcm: bits per coded sample greater-than sample format");
            }
        }

        // Do not allow the stated bits per sample to exceed the intrinsic width of the sample
        // format.
        if let Some(bits_per_sample) = params.bits_per_sample {
            if bits_per_sample > sample_format_width {
                return decode_error("pcm: bits per sample is greater-than sample format");
            }

            // Do not allow the bits per coded sample to exceed the bits per sample.
            if let Some(bits_per_coded_sample) = params.bits_per_coded_sample {
                if bits_per_coded_sample > bits_per_sample {
                    return decode_error("pcm: bits per coded sample greater-than bits per sample");
                }
            }
        }

        let decoded_width = params.bits_per_sample.unwrap_or(sample_format_width);
        let coded_width = params.bits_per_coded_sample.unwrap_or(decoded_width);

        // A-Law, Mu-Law, and floating point codecs have an implicit coded sample bit-width. Check
        // the decoded and coded widths are not illogical.
        match params.codec {
            CODEC_ID_PCM_F32LE | CODEC_ID_PCM_F32BE | CODEC_ID_PCM_F64LE | CODEC_ID_PCM_F64BE
            | CODEC_ID_PCM_ALAW | CODEC_ID_PCM_MULAW => {
                if decoded_width != sample_format_width {
                    return decode_error("pcm: unexpected bits per sample");
                }
                if coded_width != sample_format_width {
                    return decode_error("pcm: unexpected bits per coded sample");
                }
            }
            _ => (),
        }

        // Signed and unsigned integer PCM codecs may code fewer bits per sample than the actual
        // decoded sample width.
        let shift = decoded_width - coded_width;

        // The number of bytes per coded audio frame.
        let bytes_per_coded_frame = bytes_per_coded_sample * spec.channels().count();

        // Create an audio buffer of the correct sample format.
        let buf = GenericAudioBuffer::new(sample_format, spec, capacity);

        Ok(PcmDecoder { params: params.clone(), shift, bytes_per_coded_frame, buf })
    }

    fn decode_inner(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        // Calculate the number of complete audio frames per-packet.
        let num_frames = packet.data.len() / self.bytes_per_coded_frame;

        // Clear and grow the audio buffer to the required capacity.
        self.buf.clear();
        self.buf.grow_capacity(num_frames);

        if self.shift == 0 {
            let data = &packet.data;
            let codec = self.params.codec;
            let is_be = matches!(
                codec,
                CODEC_ID_PCM_S32BE
                    | CODEC_ID_PCM_S24BE
                    | CODEC_ID_PCM_S16BE
                    | CODEC_ID_PCM_U32BE
                    | CODEC_ID_PCM_U24BE
                    | CODEC_ID_PCM_U16BE
                    | CODEC_ID_PCM_F32BE
                    | CODEC_ID_PCM_F64BE
            );

            match &mut self.buf {
                GenericAudioBuffer::S16(buf) if codec == CODEC_ID_PCM_ALAW => {
                    decode_interleaved::<_, 1, _>(buf, data, num_frames, |b| alaw_to_linear(b[0]));
                }
                GenericAudioBuffer::S16(buf) if codec == CODEC_ID_PCM_MULAW => {
                    decode_interleaved::<_, 1, _>(buf, data, num_frames, |b| mulaw_to_linear(b[0]));
                }
                GenericAudioBuffer::U8(buf) => {
                    decode_interleaved::<_, 1, _>(buf, data, num_frames, |b| b[0]);
                }
                GenericAudioBuffer::S8(buf) => {
                    decode_interleaved::<_, 1, _>(buf, data, num_frames, |b| b[0] as i8);
                }
                GenericAudioBuffer::U16(buf) => decode_endian::<_, 2, _, _>(
                    buf,
                    data,
                    num_frames,
                    is_be,
                    u16::from_le_bytes,
                    u16::from_be_bytes,
                ),
                GenericAudioBuffer::S16(buf) => decode_endian::<_, 2, _, _>(
                    buf,
                    data,
                    num_frames,
                    is_be,
                    i16::from_le_bytes,
                    i16::from_be_bytes,
                ),
                GenericAudioBuffer::U24(buf) => decode_endian::<_, 3, _, _>(
                    buf,
                    data,
                    num_frames,
                    is_be,
                    |b| u24(u32::from_le_bytes([b[0], b[1], b[2], 0])),
                    |b| u24(u32::from_be_bytes([0, b[0], b[1], b[2]])),
                ),
                GenericAudioBuffer::S24(buf) => decode_endian::<_, 3, _, _>(
                    buf,
                    data,
                    num_frames,
                    is_be,
                    |b| i24(i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8),
                    |b| i24(i32::from_be_bytes([b[0], b[1], b[2], 0]) >> 8),
                ),
                GenericAudioBuffer::U32(buf) => decode_endian::<_, 4, _, _>(
                    buf,
                    data,
                    num_frames,
                    is_be,
                    u32::from_le_bytes,
                    u32::from_be_bytes,
                ),
                GenericAudioBuffer::S32(buf) => decode_endian::<_, 4, _, _>(
                    buf,
                    data,
                    num_frames,
                    is_be,
                    i32::from_le_bytes,
                    i32::from_be_bytes,
                ),
                GenericAudioBuffer::F32(buf) => decode_endian::<_, 4, _, _>(
                    buf,
                    data,
                    num_frames,
                    is_be,
                    f32::from_le_bytes,
                    f32::from_be_bytes,
                ),
                GenericAudioBuffer::F64(buf) => decode_endian::<_, 8, _, _>(
                    buf,
                    data,
                    num_frames,
                    is_be,
                    f64::from_le_bytes,
                    f64::from_be_bytes,
                ),
            }

            return Ok(());
        }

        let mut reader = packet.as_buf_reader();

        let _ = match self.params.codec {
            CODEC_ID_PCM_S32LE => {
                read_pcm_signed!(self.buf, S32, reader.read_i32()?, 32, self.shift)
            }
            CODEC_ID_PCM_S32BE => {
                read_pcm_signed!(self.buf, S32, reader.read_be_i32()?, 32, self.shift)
            }
            CODEC_ID_PCM_S24LE => {
                read_pcm_signed!(self.buf, S24, reader.read_i24()? << 8, 24, self.shift)
            }
            CODEC_ID_PCM_S24BE => {
                read_pcm_signed!(self.buf, S24, reader.read_be_i24()? << 8, 24, self.shift)
            }
            CODEC_ID_PCM_S16LE => {
                read_pcm_signed!(self.buf, S16, reader.read_i16()?, 16, self.shift)
            }
            CODEC_ID_PCM_S16BE => {
                read_pcm_signed!(self.buf, S16, reader.read_be_i16()?, 16, self.shift)
            }
            CODEC_ID_PCM_S8 => {
                read_pcm_signed!(self.buf, S8, reader.read_i8()?, 8, self.shift)
            }
            CODEC_ID_PCM_U32LE => {
                read_pcm_unsigned!(self.buf, U32, reader.read_u32()?, 32, self.shift)
            }
            CODEC_ID_PCM_U32BE => {
                read_pcm_unsigned!(self.buf, U32, reader.read_be_u32()?, 32, self.shift)
            }
            CODEC_ID_PCM_U24LE => {
                read_pcm_unsigned!(self.buf, U24, reader.read_u24()? << 8, 24, self.shift)
            }
            CODEC_ID_PCM_U24BE => {
                read_pcm_unsigned!(self.buf, U24, reader.read_be_u24()? << 8, 24, self.shift)
            }
            CODEC_ID_PCM_U16LE => {
                read_pcm_unsigned!(self.buf, U16, reader.read_u16()?, 16, self.shift)
            }
            CODEC_ID_PCM_U16BE => {
                read_pcm_unsigned!(self.buf, U16, reader.read_be_u16()?, 16, self.shift)
            }
            CODEC_ID_PCM_U8 => {
                read_pcm_unsigned!(self.buf, U8, reader.read_u8()?, 8, self.shift)
            }
            CODEC_ID_PCM_F32LE => {
                read_pcm_floating!(self.buf, F32, reader.read_f32()?)
            }
            CODEC_ID_PCM_F32BE => {
                read_pcm_floating!(self.buf, F32, reader.read_be_f32()?)
            }
            CODEC_ID_PCM_F64LE => {
                read_pcm_floating!(self.buf, F64, reader.read_f64()?)
            }
            CODEC_ID_PCM_F64BE => {
                read_pcm_floating!(self.buf, F64, reader.read_be_f64()?)
            }
            CODEC_ID_PCM_ALAW => {
                read_pcm_transfer_func!(self.buf, S16, alaw_to_linear(reader.read_u8()?))
            }
            CODEC_ID_PCM_MULAW => {
                read_pcm_transfer_func!(self.buf, S16, mulaw_to_linear(reader.read_u8()?))
            }
            // CODEC_ID_PCM_S32LE_PLANAR =>
            // CODEC_ID_PCM_S32BE_PLANAR =>
            // CODEC_ID_PCM_S24LE_PLANAR =>
            // CODEC_ID_PCM_S24BE_PLANAR =>
            // CODEC_ID_PCM_S16LE_PLANAR =>
            // CODEC_ID_PCM_S16BE_PLANAR =>
            // CODEC_ID_PCM_S8_PLANAR    =>
            // CODEC_ID_PCM_U32LE_PLANAR =>
            // CODEC_ID_PCM_U32BE_PLANAR =>
            // CODEC_ID_PCM_U24LE_PLANAR =>
            // CODEC_ID_PCM_U24BE_PLANAR =>
            // CODEC_ID_PCM_U16LE_PLANAR =>
            // CODEC_ID_PCM_U16BE_PLANAR =>
            // CODEC_ID_PCM_U8_PLANAR    =>
            // CODEC_ID_PCM_F32LE_PLANAR =>
            // CODEC_ID_PCM_F32BE_PLANAR =>
            // CODEC_ID_PCM_F64LE_PLANAR =>
            // CODEC_ID_PCM_F64BE_PLANAR =>
            _ => unsupported_error("pcm: codec is unsupported"),
        };

        Ok(())
    }
}

impl AudioDecoder for PcmDecoder {
    fn reset(&mut self) {
        // No state is stored between packets, therefore do nothing.
    }

    fn codec_info(&self) -> &CodecInfo {
        // Return the codec that's in-use.
        &Self::supported_codecs()
            .iter()
            .find(|desc| desc.id == self.params.codec)
            .expect("codec registered in supported_codecs")
            .info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
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

impl RegisterableAudioDecoder for PcmDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(PcmDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[
            support_audio_codec!(
                CODEC_ID_PCM_S32LE,
                "pcm_s32le",
                "PCM Signed 32-bit Little-Endian Interleaved"
            ),
            support_audio_codec!(
                CODEC_ID_PCM_S32BE,
                "pcm_s32be",
                "PCM Signed 32-bit Big-Endian Interleaved"
            ),
            support_audio_codec!(
                CODEC_ID_PCM_S24LE,
                "pcm_s24le",
                "PCM Signed 24-bit Little-Endian Interleaved"
            ),
            support_audio_codec!(
                CODEC_ID_PCM_S24BE,
                "pcm_s24be",
                "PCM Signed 24-bit Big-Endian Interleaved"
            ),
            support_audio_codec!(
                CODEC_ID_PCM_S16LE,
                "pcm_s16le",
                "PCM Signed 16-bit Little-Endian Interleaved"
            ),
            support_audio_codec!(
                CODEC_ID_PCM_S16BE,
                "pcm_s16be",
                "PCM Signed 16-bit Big-Endian Interleaved"
            ),
            support_audio_codec!(CODEC_ID_PCM_S8, "pcm_s8", "PCM Signed 8-bit Interleaved"),
            support_audio_codec!(
                CODEC_ID_PCM_U32LE,
                "pcm_u32le",
                "PCM Unsigned 32-bit Little-Endian Interleaved"
            ),
            support_audio_codec!(
                CODEC_ID_PCM_U32BE,
                "pcm_u32be",
                "PCM Unsigned 32-bit Big-Endian Interleaved"
            ),
            support_audio_codec!(
                CODEC_ID_PCM_U24LE,
                "pcm_u24le",
                "PCM Unsigned 24-bit Little-Endian Interleaved"
            ),
            support_audio_codec!(
                CODEC_ID_PCM_U24BE,
                "pcm_u24be",
                "PCM Unsigned 24-bit Big-Endian Interleaved"
            ),
            support_audio_codec!(
                CODEC_ID_PCM_U16LE,
                "pcm_u16le",
                "PCM Unsigned 16-bit Little-Endian Interleaved"
            ),
            support_audio_codec!(
                CODEC_ID_PCM_U16BE,
                "pcm_u16be",
                "PCM Unsigned 16-bit Big-Endian Interleaved"
            ),
            support_audio_codec!(CODEC_ID_PCM_U8, "pcm_u8", "PCM Unsigned 8-bit Interleaved"),
            support_audio_codec!(
                CODEC_ID_PCM_F32LE,
                "pcm_f32le",
                "PCM 32-bit Little-Endian Floating Point Interleaved"
            ),
            support_audio_codec!(
                CODEC_ID_PCM_F32BE,
                "pcm_f32be",
                "PCM 32-bit Big-Endian Floating Point Interleaved"
            ),
            support_audio_codec!(
                CODEC_ID_PCM_F64LE,
                "pcm_f64le",
                "PCM 64-bit Little-Endian Floating Point Interleaved"
            ),
            support_audio_codec!(
                CODEC_ID_PCM_F64BE,
                "pcm_f64be",
                "PCM 64-bit Big-Endian Floating Point Interleaved"
            ),
            support_audio_codec!(CODEC_ID_PCM_ALAW, "pcm_alaw", "PCM A-law"),
            support_audio_codec!(CODEC_ID_PCM_MULAW, "pcm_mulaw", "PCM Mu-law"),
            // support_audio_codec!(
            //     CODEC_ID_PCM_S32LE_PLANAR,
            //     "pcm_s32le_planar",
            //     "PCM Signed 32-bit Little-Endian Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_S32BE_PLANAR,
            //     "pcm_s32be_planar",
            //     "PCM Signed 32-bit Big-Endian Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_S24LE_PLANAR,
            //     "pcm_s24le_planar",
            //     "PCM Signed 24-bit Little-Endian Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_S24BE_PLANAR,
            //     "pcm_s24be_planar",
            //     "PCM Signed 24-bit Big-Endian Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_S16LE_PLANAR,
            //     "pcm_s16le_planar",
            //     "PCM Signed 16-bit Little-Endian Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_S16BE_PLANAR,
            //     "pcm_s16be_planar",
            //     "PCM Signed 16-bit Big-Endian Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_S8_PLANAR   ,
            //     "pcm_s8_planar"   ,
            //     "PCM Signed 8-bit Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_U32LE_PLANAR,
            //     "pcm_u32le_planar",
            //     "PCM Unsigned 32-bit Little-Endian Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_U32BE_PLANAR,
            //     "pcm_u32be_planar",
            //     "PCM Unsigned 32-bit Big-Endian Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_U24LE_PLANAR,
            //     "pcm_u24le_planar",
            //     "PCM Unsigned 24-bit Little-Endian Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_U24BE_PLANAR,
            //     "pcm_u24be_planar",
            //     "PCM Unsigned 24-bit Big-Endian Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_U16LE_PLANAR,
            //     "pcm_u16le_planar",
            //     "PCM Unsigned 16-bit Little-Endian Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_U16BE_PLANAR,
            //     "pcm_u16be_planar",
            //     "PCM Unsigned 16-bit Big-Endian Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_U8_PLANAR   ,
            //     "pcm_u8_planar"   ,
            //     "PCM Unsigned 8-bit Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_F32LE_PLANAR,
            //     "pcm_f32le_planar",
            //     "PCM 32-bit Little-Endian Floating Point Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_F32BE_PLANAR,
            //     "pcm_f32be_planar",
            //     "PCM 32-bit Big-Endian Floating Point Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_F64LE_PLANAR,
            //     "pcm_f64le_planar",
            //     "PCM 64-bit Little-Endian Floating Point Planar"
            // ),
            // support_audio_codec!(
            //     CODEC_ID_PCM_F64BE_PLANAR,
            //     "pcm_f64be_planar",
            //     "PCM 64-bit Big-Endian Floating Point Planar"
            // ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symphonia_core::audio::{Channels, layouts};

    fn verify_codec<S, E, X>(codec: AudioCodecId, pattern: &[S], encode: E, extract_plane: X)
    where
        S: Copy + PartialEq + std::fmt::Debug,
        E: Fn(S) -> Vec<u8>,
        X: Fn(&GenericAudioBufferRef<'_>, usize) -> Vec<S>,
    {
        for num_channels in [1usize, 2, 3] {
            let mut params = AudioCodecParameters::new();
            params
                .for_codec(codec)
                .with_sample_rate(48000)
                .with_channels(Channels::Discrete(num_channels as u16));
            let mut decoder = PcmDecoder::try_new(&params, &AudioDecoderOptions::default())
                .expect("PcmDecoder::try_new should succeed");

            let num_frames = pattern.len();
            let mut expected: Vec<Vec<S>> = Vec::with_capacity(num_channels);
            for ch in 0..num_channels {
                let mut plane = Vec::with_capacity(num_frames);
                for frame in 0..num_frames {
                    plane.push(pattern[(frame + ch) % num_frames]);
                }
                expected.push(plane);
            }

            let mut bytes = Vec::new();
            for frame in 0..num_frames {
                for ch_plane in &expected {
                    bytes.extend_from_slice(&encode(ch_plane[frame]));
                }
            }

            let packet = PacketRef::new(0, 0i64.into(), (num_frames as u64).into(), &bytes);
            let decoded = decoder.decode_ref(&packet).expect("decode_ref should succeed");
            assert_eq!(decoded.num_planes(), num_channels);
            assert_eq!(decoded.frames(), num_frames);
            for (ch, exp_plane) in expected.iter().enumerate() {
                assert_eq!(
                    extract_plane(&decoded, ch),
                    *exp_plane,
                    "codec={codec:?}, num_channels={num_channels}, ch={ch}"
                );
            }
        }
    }

    #[test]
    fn decode_unshifted_all_formats() {
        let extract_s32 = |buf: &GenericAudioBufferRef<'_>, ch: usize| match buf {
            GenericAudioBufferRef::S32(b) => b.plane(ch).expect("plane exists").to_vec(),
            other => panic!("expected S32, got {:?}", other.spec()),
        };
        let extract_s24 = |buf: &GenericAudioBufferRef<'_>, ch: usize| match buf {
            GenericAudioBufferRef::S24(b) => b.plane(ch).expect("plane exists").to_vec(),
            other => panic!("expected S24, got {:?}", other.spec()),
        };
        let extract_s16 = |buf: &GenericAudioBufferRef<'_>, ch: usize| match buf {
            GenericAudioBufferRef::S16(b) => b.plane(ch).expect("plane exists").to_vec(),
            other => panic!("expected S16, got {:?}", other.spec()),
        };
        let extract_s8 = |buf: &GenericAudioBufferRef<'_>, ch: usize| match buf {
            GenericAudioBufferRef::S8(b) => b.plane(ch).expect("plane exists").to_vec(),
            other => panic!("expected S8, got {:?}", other.spec()),
        };
        let extract_u32 = |buf: &GenericAudioBufferRef<'_>, ch: usize| match buf {
            GenericAudioBufferRef::U32(b) => b.plane(ch).expect("plane exists").to_vec(),
            other => panic!("expected U32, got {:?}", other.spec()),
        };
        let extract_u24 = |buf: &GenericAudioBufferRef<'_>, ch: usize| match buf {
            GenericAudioBufferRef::U24(b) => b.plane(ch).expect("plane exists").to_vec(),
            other => panic!("expected U24, got {:?}", other.spec()),
        };
        let extract_u16 = |buf: &GenericAudioBufferRef<'_>, ch: usize| match buf {
            GenericAudioBufferRef::U16(b) => b.plane(ch).expect("plane exists").to_vec(),
            other => panic!("expected U16, got {:?}", other.spec()),
        };
        let extract_u8 = |buf: &GenericAudioBufferRef<'_>, ch: usize| match buf {
            GenericAudioBufferRef::U8(b) => b.plane(ch).expect("plane exists").to_vec(),
            other => panic!("expected U8, got {:?}", other.spec()),
        };
        let extract_f32 = |buf: &GenericAudioBufferRef<'_>, ch: usize| match buf {
            GenericAudioBufferRef::F32(b) => b.plane(ch).expect("plane exists").to_vec(),
            other => panic!("expected F32, got {:?}", other.spec()),
        };
        let extract_f64 = |buf: &GenericAudioBufferRef<'_>, ch: usize| match buf {
            GenericAudioBufferRef::F64(b) => b.plane(ch).expect("plane exists").to_vec(),
            other => panic!("expected F64, got {:?}", other.spec()),
        };

        let s32_samples = [0, 0x1234_5678, -0x1234_5678, i32::MIN, i32::MAX];
        verify_codec(CODEC_ID_PCM_S32LE, &s32_samples, |s| s.to_le_bytes().to_vec(), extract_s32);
        verify_codec(CODEC_ID_PCM_S32BE, &s32_samples, |s| s.to_be_bytes().to_vec(), extract_s32);

        let s24_samples =
            [i24(0), i24(0x12_3456), i24(-0x12_3456), i24(-0x80_0000), i24(0x7F_FFFF)];
        verify_codec(
            CODEC_ID_PCM_S24LE,
            &s24_samples,
            |s| s.inner().to_le_bytes()[..3].to_vec(),
            extract_s24,
        );
        verify_codec(
            CODEC_ID_PCM_S24BE,
            &s24_samples,
            |s| s.inner().to_be_bytes()[1..].to_vec(),
            extract_s24,
        );

        let s16_samples = [0, 0x1234, -0x1234, i16::MIN, i16::MAX];
        verify_codec(CODEC_ID_PCM_S16LE, &s16_samples, |s| s.to_le_bytes().to_vec(), extract_s16);
        verify_codec(CODEC_ID_PCM_S16BE, &s16_samples, |s| s.to_be_bytes().to_vec(), extract_s16);

        let s8_samples = [0, 42, -42, i8::MIN, i8::MAX];
        verify_codec(CODEC_ID_PCM_S8, &s8_samples, |s| vec![s as u8], extract_s8);

        let u32_samples = [0, 0x1234_5678, 0x8000_0001, u32::MAX];
        verify_codec(CODEC_ID_PCM_U32LE, &u32_samples, |s| s.to_le_bytes().to_vec(), extract_u32);
        verify_codec(CODEC_ID_PCM_U32BE, &u32_samples, |s| s.to_be_bytes().to_vec(), extract_u32);

        let u24_samples = [u24(0), u24(0x12_3456), u24(0x80_0001), u24(0xFF_FFFF)];
        verify_codec(
            CODEC_ID_PCM_U24LE,
            &u24_samples,
            |s| s.inner().to_le_bytes()[..3].to_vec(),
            extract_u24,
        );
        verify_codec(
            CODEC_ID_PCM_U24BE,
            &u24_samples,
            |s| s.inner().to_be_bytes()[1..].to_vec(),
            extract_u24,
        );

        let u16_samples = [0, 0x1234, 0x8001, u16::MAX];
        verify_codec(CODEC_ID_PCM_U16LE, &u16_samples, |s| s.to_le_bytes().to_vec(), extract_u16);
        verify_codec(CODEC_ID_PCM_U16BE, &u16_samples, |s| s.to_be_bytes().to_vec(), extract_u16);

        let u8_samples = [0, 0x12, 0x80, u8::MAX];
        verify_codec(CODEC_ID_PCM_U8, &u8_samples, |s| vec![s], extract_u8);

        let f32_samples = [0.0f32, 0.25, -0.75, 1.0, -1.0];
        verify_codec(CODEC_ID_PCM_F32LE, &f32_samples, |s| s.to_le_bytes().to_vec(), extract_f32);
        verify_codec(CODEC_ID_PCM_F32BE, &f32_samples, |s| s.to_be_bytes().to_vec(), extract_f32);

        let f64_samples = [0.0f64, 0.25, -0.75, 1.0, -1.0];
        verify_codec(CODEC_ID_PCM_F64LE, &f64_samples, |s| s.to_le_bytes().to_vec(), extract_f64);
        verify_codec(CODEC_ID_PCM_F64BE, &f64_samples, |s| s.to_be_bytes().to_vec(), extract_f64);

        let alaw_pairs: &[(u8, i16)] =
            &[(0xD5, 8), (0x55, -8), (0x2A, -32256), (0xAA, 32256), (0x00, -5504)];
        let mulaw_pairs: &[(u8, i16)] =
            &[(0xFF, 0), (0x7F, 0), (0x00, -32124), (0x80, 32124), (0xAA, 5372)];
        for (codec, pairs) in [(CODEC_ID_PCM_ALAW, alaw_pairs), (CODEC_ID_PCM_MULAW, mulaw_pairs)] {
            for num_channels in [1usize, 2, 3] {
                let mut params = AudioCodecParameters::new();
                params
                    .for_codec(codec)
                    .with_sample_rate(8000)
                    .with_channels(Channels::Discrete(num_channels as u16));
                let mut decoder = PcmDecoder::try_new(&params, &AudioDecoderOptions::default())
                    .expect("PcmDecoder::try_new should succeed");

                let num_frames = pairs.len();
                let mut bytes = Vec::with_capacity(num_frames * num_channels);
                let mut expected = vec![Vec::with_capacity(num_frames); num_channels];
                for frame in 0..num_frames {
                    for (ch, ch_plane) in expected.iter_mut().enumerate() {
                        let (coded, linear) = pairs[(frame + ch) % num_frames];
                        bytes.push(coded);
                        ch_plane.push(linear);
                    }
                }

                let packet = PacketRef::new(0, 0i64.into(), (num_frames as u64).into(), &bytes);
                let decoded = decoder.decode_ref(&packet).expect("decode_ref should succeed");
                assert_eq!(decoded.num_planes(), num_channels);
                assert_eq!(decoded.frames(), num_frames);
                for (ch, exp_plane) in expected.iter().enumerate() {
                    assert_eq!(extract_s16(&decoded, ch), *exp_plane);
                }
            }
        }
    }

    #[test]
    fn decode_edge_cases_and_shifted_fallback() {
        for num_channels in [1u16, 2, 3] {
            let mut params = AudioCodecParameters::new();
            params
                .for_codec(CODEC_ID_PCM_S16LE)
                .with_sample_rate(48000)
                .with_channels(Channels::Discrete(num_channels));
            let mut decoder = PcmDecoder::try_new(&params, &AudioDecoderOptions::default())
                .expect("try_new should succeed");
            let empty = PacketRef::new(0, 0i64.into(), 0u64.into(), &[]);
            let decoded = decoder.decode_ref(&empty).expect("empty decode should succeed");
            assert_eq!(decoded.num_planes(), usize::from(num_channels));
            assert_eq!(decoded.frames(), 0);
        }

        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_PCM_S16LE)
            .with_sample_rate(48000)
            .with_channels(layouts::CHANNEL_LAYOUT_STEREO);
        let mut decoder = PcmDecoder::try_new(&params, &AudioDecoderOptions::default())
            .expect("try_new should succeed");

        let mut partial = Vec::new();
        for s in [100i16, -200, 300, -400] {
            partial.extend_from_slice(&s.to_le_bytes());
        }
        partial.extend_from_slice(&[0xDE, 0xAD, 0xBE]);
        {
            let decoded = decoder
                .decode_ref(&PacketRef::new(0, 0i64.into(), 2u64.into(), &partial))
                .expect("partial decode should succeed");
            assert_eq!(decoded.frames(), 2);
            let GenericAudioBufferRef::S16(buf) = decoded
            else {
                panic!("expected S16");
            };
            assert_eq!(buf.plane(0).expect("plane 0"), &[100, 300]);
            assert_eq!(buf.plane(1).expect("plane 1"), &[-200, -400]);
        }

        let large = vec![0u8; 64 * 4];
        assert_eq!(
            decoder
                .decode_ref(&PacketRef::new(0, 0i64.into(), 64u64.into(), &large))
                .expect("large decode should succeed")
                .frames(),
            64
        );
        let one_frame: Vec<u8> =
            [0x1111i16, -0x2222i16].iter().flat_map(|s| s.to_le_bytes()).collect();
        {
            let decoded = decoder
                .decode_ref(&PacketRef::new(0, 0i64.into(), 1u64.into(), &one_frame))
                .expect("small decode should succeed");
            assert_eq!(decoded.frames(), 1);
            let GenericAudioBufferRef::S16(buf) = decoded
            else {
                panic!("expected S16");
            };
            assert_eq!(buf.plane(0).expect("plane 0"), &[0x1111]);
            assert_eq!(buf.plane(1).expect("plane 1"), &[-0x2222]);
        }

        let mut shifted_params = AudioCodecParameters::new();
        shifted_params
            .for_codec(CODEC_ID_PCM_S16LE)
            .with_sample_rate(48000)
            .with_channels(layouts::CHANNEL_LAYOUT_STEREO)
            .with_bits_per_sample(16)
            .with_bits_per_coded_sample(12);
        let mut shifted_decoder =
            PcmDecoder::try_new(&shifted_params, &AudioDecoderOptions::default())
                .expect("shifted try_new should succeed");
        let shifted_bytes: Vec<u8> = [0x0012i16, -0x0034i16, 0x0100i16, -0x0200i16]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let decoded = shifted_decoder
            .decode_ref(&PacketRef::new(0, 0i64.into(), 2u64.into(), &shifted_bytes))
            .expect("shifted decode should succeed");
        assert_eq!(decoded.frames(), 2);
        let GenericAudioBufferRef::S16(buf) = decoded
        else {
            panic!("expected S16");
        };
        assert_eq!(buf.plane(0).expect("plane 0"), &[0x0120, 0x1000]);
        assert_eq!(buf.plane(1).expect("plane 1"), &[-0x0340, -0x2000]);

        // Shifted S24LE (20-bit coded in 24-bit container => shift = 4), including
        // sign-bit wrap at -0x08_0000 -> -0x80_0000.
        let mut s24_params = AudioCodecParameters::new();
        s24_params
            .for_codec(CODEC_ID_PCM_S24LE)
            .with_sample_rate(48000)
            .with_channels(layouts::CHANNEL_LAYOUT_MONO)
            .with_bits_per_sample(24)
            .with_bits_per_coded_sample(20);
        let mut s24_decoder = PcmDecoder::try_new(&s24_params, &AudioDecoderOptions::default())
            .expect("s24 try_new should succeed");
        let s24_bytes: Vec<u8> = [0x01_2345i32, -0x01_2345i32, -0x08_0000i32]
            .iter()
            .flat_map(|s| s.to_le_bytes()[..3].to_vec())
            .collect();
        let decoded = s24_decoder
            .decode_ref(&PacketRef::new(0, 0i64.into(), 3u64.into(), &s24_bytes))
            .expect("s24 shifted decode should succeed");
        let GenericAudioBufferRef::S24(buf) = decoded
        else {
            panic!("expected S24");
        };
        assert_eq!(
            buf.plane(0).expect("plane 0"),
            &[i24(0x12_3450), i24(-0x12_3450), i24(-0x80_0000)]
        );

        // Shifted U24BE (20-bit coded in 24-bit container => shift = 4).
        let mut u24_params = AudioCodecParameters::new();
        u24_params
            .for_codec(CODEC_ID_PCM_U24BE)
            .with_sample_rate(48000)
            .with_channels(layouts::CHANNEL_LAYOUT_MONO)
            .with_bits_per_sample(24)
            .with_bits_per_coded_sample(20);
        let mut u24_decoder = PcmDecoder::try_new(&u24_params, &AudioDecoderOptions::default())
            .expect("u24 try_new should succeed");
        let u24_bytes: Vec<u8> = [0x01_2345u32, 0x0F_FFFFu32]
            .iter()
            .flat_map(|s| s.to_be_bytes()[1..].to_vec())
            .collect();
        let decoded = u24_decoder
            .decode_ref(&PacketRef::new(0, 0i64.into(), 2u64.into(), &u24_bytes))
            .expect("u24 shifted decode should succeed");
        let GenericAudioBufferRef::U24(buf) = decoded
        else {
            panic!("expected U24");
        };
        assert_eq!(buf.plane(0).expect("plane 0"), &[u24(0x12_3450), u24(0xFF_FFF0)]);

        // Shifted U8 (4-bit coded in 8-bit container => shift = 4).
        let mut u8_params = AudioCodecParameters::new();
        u8_params
            .for_codec(CODEC_ID_PCM_U8)
            .with_sample_rate(48000)
            .with_channels(layouts::CHANNEL_LAYOUT_MONO)
            .with_bits_per_sample(8)
            .with_bits_per_coded_sample(4);
        let mut u8_decoder = PcmDecoder::try_new(&u8_params, &AudioDecoderOptions::default())
            .expect("u8 try_new should succeed");
        let decoded = u8_decoder
            .decode_ref(&PacketRef::new(0, 0i64.into(), 3u64.into(), &[0x00, 0x05, 0x0F]))
            .expect("u8 shifted decode should succeed");
        let GenericAudioBufferRef::U8(buf) = decoded
        else {
            panic!("expected U8");
        };
        assert_eq!(buf.plane(0).expect("plane 0"), &[0x00, 0x50, 0xF0]);
    }
}
