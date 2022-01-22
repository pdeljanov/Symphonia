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
// Disable to better express the specification.
#![allow(clippy::collapsible_else_if)]

use std::cmp::min;

use symphonia_core::audio::{
    AsAudioBufferRef, AudioBuffer, AudioBufferRef, Channels, Signal, SignalSpec,
};
use symphonia_core::codecs::{
    CodecDescriptor, CodecParameters, Decoder, DecoderOptions, FinalizeResult, CODEC_TYPE_ALAC,
};
use symphonia_core::errors::{decode_error, unsupported_error, Result};
use symphonia_core::formats::Packet;
use symphonia_core::io::{BitReaderLtr, BufReader, FiniteStream, ReadBitsLtr, ReadBytes};
use symphonia_core::support_codec;

/// Supported ALAC version.
const ALAC_VERSION: u8 = 0;

/// Single Channel Element (SCE) tag.
const ALAC_ELEM_TAG_SCE: u32 = 0;
/// Channel Pair Element (CPE) tag.
const ALAC_ELEM_TAG_CPE: u32 = 1;
/// Coupling Channel Element CCE tag.
const ALAC_ELEM_TAG_CCE: u32 = 2;
/// LFE Channel Element (LFE) tag.
const ALAC_ELEM_TAG_LFE: u32 = 3;
/// Data Stream Element (DSE) tag.
const ALAC_ELEM_TAG_DSE: u32 = 4;
/// Program Control Element (PCE) tag.
const ALAC_ELEM_TAG_PCE: u32 = 5;
/// Fill Element (FIL) tag.
const ALAC_ELEM_TAG_FIL: u32 = 6;
/// Frame End Element (END) tag.
const ALAC_ELEM_TAG_END: u32 = 7;

/// An ALAC channel layout.
#[derive(Debug)]
enum ChannelLayout {
    /// Centre
    Mono,
    /// Front Left, Front Right
    Stereo,
    /// Centre, Front Left, Front Right
    Mpeg3p0B,
    /// Centre, Front Left, Front Right, Rear Centre
    Mpeg4p0B,
    /// Centre, Front Left, Front Right, Side Left, Side Right
    Mpeg5p0D,
    /// Centre, Front Left, Front Right, Side Left, Side Right, LFE
    Mpeg5p1D,
    /// Centre, Front Left, Front Right, Side Left, Side Right, Rear Centre, LFE
    Aac6p1,
    /// Centre, Front Left of Centre, Front Right of Centre, Front Left, Front Right, Side Left,
    /// Side Right, LFE
    Mpeg7p1B,
}

impl ChannelLayout {
    /// Given the current ALAC channel layout, this function will return a mappings of an ALAC
    /// channel number (the index into the array) to a Symphonia `AudioBuffer` channel index.
    fn channel_map(&self) -> [u8; 8] {
        match self {
            ChannelLayout::Mono => [0, 0, 0, 0, 0, 0, 0, 0],
            ChannelLayout::Stereo => [0, 1, 0, 0, 0, 0, 0, 0],
            ChannelLayout::Mpeg3p0B => [2, 0, 1, 0, 0, 0, 0, 0],
            ChannelLayout::Mpeg4p0B => [2, 0, 1, 3, 0, 0, 0, 0],
            ChannelLayout::Mpeg5p0D => [2, 0, 1, 3, 4, 0, 0, 0],
            ChannelLayout::Mpeg5p1D => [2, 0, 1, 4, 5, 3, 0, 0],
            ChannelLayout::Aac6p1 => [2, 0, 1, 5, 6, 4, 3, 0],
            ChannelLayout::Mpeg7p1B => [2, 4, 5, 0, 1, 6, 7, 3],
        }
    }

    /// Get a Symphonia channels bitmask from the ALAC channel layout.
    fn channels(&self) -> Channels {
        match self {
            ChannelLayout::Mono => Channels::FRONT_LEFT,
            ChannelLayout::Stereo => Channels::FRONT_LEFT | Channels::FRONT_RIGHT,
            ChannelLayout::Mpeg3p0B => {
                Channels::FRONT_CENTRE | Channels::FRONT_LEFT | Channels::FRONT_RIGHT
            }
            ChannelLayout::Mpeg4p0B => {
                Channels::FRONT_CENTRE
                    | Channels::FRONT_LEFT
                    | Channels::FRONT_RIGHT
                    | Channels::REAR_CENTRE
            }
            ChannelLayout::Mpeg5p0D => {
                Channels::FRONT_CENTRE
                    | Channels::FRONT_LEFT
                    | Channels::FRONT_RIGHT
                    | Channels::SIDE_LEFT
                    | Channels::SIDE_RIGHT
            }
            ChannelLayout::Mpeg5p1D => {
                Channels::FRONT_CENTRE
                    | Channels::FRONT_LEFT
                    | Channels::FRONT_RIGHT
                    | Channels::SIDE_LEFT
                    | Channels::SIDE_RIGHT
                    | Channels::LFE1
            }
            ChannelLayout::Aac6p1 => {
                Channels::FRONT_CENTRE
                    | Channels::FRONT_LEFT
                    | Channels::FRONT_RIGHT
                    | Channels::SIDE_LEFT
                    | Channels::SIDE_RIGHT
                    | Channels::REAR_CENTRE
                    | Channels::LFE1
            }
            ChannelLayout::Mpeg7p1B => {
                Channels::FRONT_CENTRE
                    | Channels::FRONT_LEFT_CENTRE
                    | Channels::FRONT_RIGHT_CENTRE
                    | Channels::FRONT_LEFT
                    | Channels::FRONT_RIGHT
                    | Channels::SIDE_LEFT
                    | Channels::SIDE_RIGHT
                    | Channels::LFE1
            }
        }
    }
}

/// The ALAC "magic cookie" or codec specific configuration.
#[derive(Debug)]
#[allow(dead_code)]
struct MagicCookie {
    frame_length: u32,
    compatible_version: u8,
    bit_depth: u8,
    pb: u8,
    mb: u8,
    kb: u8,
    num_channels: u8,
    max_run: u16,
    max_frame_bytes: u32,
    avg_bit_rate: u32,
    sample_rate: u32,
    channel_layout: ChannelLayout,
}

impl MagicCookie {
    fn try_read<B: ReadBytes + FiniteStream>(reader: &mut B) -> Result<MagicCookie> {
        // The magic cookie is either 24 or 48 bytes long.
        if reader.byte_len() != 24 && reader.byte_len() != 48 {
            return unsupported_error("alac: invalid magic cookie size");
        }

        let mut config = MagicCookie {
            frame_length: reader.read_be_u32()?,
            compatible_version: reader.read_u8()?,
            bit_depth: reader.read_u8()?,
            pb: reader.read_u8()?,
            mb: reader.read_u8()?,
            kb: reader.read_u8()?,
            num_channels: reader.read_u8()?,
            max_run: reader.read_be_u16()?,
            max_frame_bytes: reader.read_be_u32()?,
            avg_bit_rate: reader.read_be_u32()?,
            sample_rate: reader.read_be_u32()?,
            channel_layout: ChannelLayout::Mono,
        };

        // Only support up-to the currently implemented ALAC version.
        if config.compatible_version > ALAC_VERSION {
            return unsupported_error("alac: not compatible with alac version 0");
        }

        // A bit-depth greater than 32 is not allowed.
        if config.bit_depth > 32 {
            return decode_error("alac: invalid bit depth");
        }

        // Only 8 channel layouts exist.
        // TODO: Support discrete/auxiliary channels.
        if config.num_channels < 1 || config.num_channels > 8 {
            return unsupported_error("alac: more than 8 channels");
        }

        // If the magic cookie is 48 bytes, the channel layout is explictly set, otherwise select a
        // channel layout from the number of channels.
        config.channel_layout = if reader.byte_len() == 48 {
            // The first field is the size of the channel layout info. This should always be 24.
            if reader.read_be_u32()? != 24 {
                return decode_error("alac: invalid channel layout info size");
            }

            // The channel layout info identifier should be the ascii string "chan".
            if reader.read_quad_bytes()? != *b"chan" {
                return decode_error("alac: invalid channel layout info id");
            }

            // The channel layout info version must be 0.
            if reader.read_be_u32()? != 0 {
                return decode_error("alac: invalid channel layout info version");
            }

            // Read the channel layout tag. The numerical value of this tag is defined by the Apple
            // CoreAudio API.
            let layout = match reader.read_be_u32()? {
                // 100 << 16
                0x64_0001 => ChannelLayout::Mono,
                // 101 << 16
                0x65_0002 => ChannelLayout::Stereo,
                // 113 << 16
                0x71_0003 => ChannelLayout::Mpeg3p0B,
                // 116 << 16
                0x74_0004 => ChannelLayout::Mpeg4p0B,
                // 120 << 16
                0x78_0005 => ChannelLayout::Mpeg5p0D,
                // 124 << 16
                0x7c_0006 => ChannelLayout::Mpeg5p1D,
                // 142 << 16
                0x8e_0007 => ChannelLayout::Aac6p1,
                // 127 << 16
                0x7f_0008 => ChannelLayout::Mpeg7p1B,
                _ => return decode_error("alac: invalid channel layout tag"),
            };

            // The number of channels stated in the mandatory part of the magic cookie should match
            // the number of channels implicit to the channel layout.
            if config.num_channels != layout.channels().count() as u8 {
                return decode_error(
                    "alac: the number of channels differs from the channel layout",
                );
            }

            // The next two fields are reserved and should be 0.
            if reader.read_be_u32()? != 0 || reader.read_be_u32()? != 0 {
                return decode_error("alac: reserved values in channel layout info are not 0");
            }

            layout
        }
        else {
            // If extra channel information is not provided, use the number of channels to assign
            // a channel layout.
            //
            // TODO: If the number of channels is > 2, then the additional channels are considered
            // discrete and not part of a channel layout. However, Symphonia does not support
            // discrete/auxiliary channels so the standard ALAC channel layouts are used for now.
            match config.num_channels {
                1 => ChannelLayout::Mono,
                2 => ChannelLayout::Stereo,
                3 => ChannelLayout::Mpeg3p0B,
                4 => ChannelLayout::Mpeg4p0B,
                5 => ChannelLayout::Mpeg5p0D,
                6 => ChannelLayout::Mpeg5p1D,
                7 => ChannelLayout::Aac6p1,
                8 => ChannelLayout::Mpeg7p1B,
                _ => return decode_error("alac: unknown channel layout for number of channels"),
            }
        };

        Ok(config)
    }
}

#[derive(Debug)]
struct ElementChannel {
    pred_bits: u32,
    kb: u32,
    mb: u32,
    mode: u32,
    shift: u32,
    pb_factor: u32,
    lpc_order: u32,
    lpc_coeffs: [i32; 32],
}

impl ElementChannel {
    fn try_read<B: ReadBitsLtr>(
        bs: &mut B,
        config: &MagicCookie,
        pred_bits: u8,
    ) -> Result<ElementChannel> {
        let mode = bs.read_bits_leq32(4)?;
        let shift = bs.read_bits_leq32(4)?;
        let pb_factor = (bs.read_bits_leq32(3)? * u32::from(config.pb)) >> 2;
        let lpc_order = bs.read_bits_leq32(5)?;

        // Read the predictor coefficients.
        let mut lpc_coeffs = [0; 32];

        for coeff in &mut lpc_coeffs[..lpc_order as usize] {
            *coeff = bs.read_bits_leq32_signed(16)?;
        }

        Ok(ElementChannel {
            pred_bits: u32::from(pred_bits),
            kb: u32::from(config.kb),
            mb: u32::from(config.mb),
            mode,
            shift,
            pb_factor,
            lpc_order,
            lpc_coeffs,
        })
    }

    fn read_residuals<B: ReadBitsLtr>(&mut self, bs: &mut B, out: &mut [i32]) -> Result<()> {
        let out_len = out.len();

        let mut mb = self.mb;
        let mut sign_toggle = 0;
        let mut zero_run_end = 0;

        for (i, sample) in out.iter_mut().enumerate() {
            // If the current sample is within a run of zeros, skip to the next sample since the
            // output is already zeroed.
            if i < zero_run_end {
                continue;
            }

            let k = lg3a(mb);
            let val = read_rice_code(bs, k.min(self.kb), self.pred_bits)? + sign_toggle;

            *sample = rice_code_to_signed(val);

            if val > 0xffff {
                mb = 0xffff;
            }
            else {
                // Order is important here.
                mb -= (self.pb_factor * mb) >> 9;
                mb += self.pb_factor * val;
            }

            sign_toggle = 0;

            // In this special case, a run of zeros is signalled.
            if mb < 128 && i + 1 < out_len {
                // This subtraction cannot overflow because mb is a u32 and < 128. Therefore, mb
                // will always have 25 leading zeros.
                let k = mb.leading_zeros() - 24 + ((mb + 16) >> 6);

                // The decoded rice code indicates the length of the run of zeros.
                let zeros = read_rice_code(bs, k.min(self.kb), 16)?;

                if zeros < 0xffff {
                    sign_toggle = 1;
                }

                mb = 0;
                zero_run_end = i + 1 + zeros as usize;
            }
        }
        Ok(())
    }

    fn predict(&mut self, out: &mut [i32]) -> Result<()> {
        // Modes other than 0 and 15 are invalid.
        if self.mode > 0 && self.mode < 15 {
            return decode_error("alac: invalid mode");
        }

        // An order of 0 indicates no prediction is done (the residuals are the samples).
        if self.lpc_order == 0 {
            return Ok(());
        }

        // Decoding is performed on signed 32-bit numbers, however, the actual predicted samples
        // have a bit-width of `pred_bits`. Therefore, the top `32 - pred_bits` bits should be
        // clipped.
        let num_clip_bits = 32 - self.pred_bits;

        // An order of 31, or a mode of 15, are special cases where the predictor runs twice. The
        // first-pass uses a first-order prediction. The second pass is then the regular prediction
        // using the coefficients from the bitstream.
        if self.lpc_order == 31 || self.mode == 15 {
            for i in 1..out.len() {
                out[i] = clip_msbs(out[i].wrapping_add(out[i - 1]), num_clip_bits);
            }
        }

        let order = self.lpc_order as usize;

        // Process warm-up samples.
        for i in 1..1 + order {
            out[i] = clip_msbs(out[i].wrapping_add(out[i - 1]), num_clip_bits);
        }

        // Do the prediction.
        //
        // TODO: Orders for 4 and 8 are special-cased in the reference decoder. Consider using
        // optimized versions for those cases like the FLAC decoder does.
        for i in 1 + order..out.len() {
            // Value of the output sample before prediction (the residual or difference).
            let mut res = out[i];

            // Value of the sample preceeding the first past sample.
            let past0 = out[i - order - 1];

            // Run the FIR filter.
            let sum = self.lpc_coeffs[..order]
                .iter()
                .rev()
                .zip(&out[i - order..i])
                .map(|(&coeff, &s)| coeff.wrapping_mul(s - past0))
                .fold(0i32, |sum, s| sum.wrapping_add(s));

            // Rewrite `1 << (self.shift - 1)` as `(1 << self.shift) >> 1` to prevent overflowing
            // when shift is 0.
            let val = (sum + ((1 << self.shift) >> 1)) >> self.shift;
            out[i] = clip_msbs(out[i].wrapping_add(past0).wrapping_add(val), num_clip_bits);

            // Adjust the coefficients if the initial value of the residual was not 0.
            if res != 0 {
                let iter =
                    self.lpc_coeffs[..order].iter_mut().rev().zip(&out[i - order..i]).enumerate();

                // Note the subtle change in operations and signs for the following two cases.
                if res > 0 {
                    // Positive residual case.
                    for (j, (coeff, &sample)) in iter {
                        let val = past0 - sample;
                        let sign = val.signum();

                        *coeff -= sign;

                        res -= (1 + j as i32) * ((sign * val) >> self.shift);

                        if res <= 0 {
                            break;
                        }
                    }
                }
                else {
                    // Negative residual case.
                    for (j, (coeff, &sample)) in iter {
                        let val = past0 - sample;
                        let sign = val.signum();

                        *coeff += sign;

                        res -= (1 + j as i32) * ((-sign * val) >> self.shift);

                        if res >= 0 {
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// Apple Lossless Audio Codec (ALAC) decoder.
pub struct AlacDecoder {
    /// Codec paramters.
    params: CodecParameters,
    /// A temporary buffer to store the tail bits while decoding an element with a bit-shift > 0. If
    /// `config.num_channels` > 1, then this buffer must be 2x the frame length.
    tail_bits: Vec<u16>,
    /// ALAC codec-specific configuration.
    config: MagicCookie,
    /// Output buffer.
    buf: AudioBuffer<i32>,
}

impl AlacDecoder {
    fn decode_inner(&mut self, packet: &Packet) -> Result<()> {
        let mut bs = BitReaderLtr::new(packet.buf());

        let channel_map = self.config.channel_layout.channel_map();
        let num_channels = self.config.num_channels as usize;
        let mut next_channel = 0;
        let mut num_frames = 0;

        // Fill the audio buffer with silence.
        self.buf.clear();
        self.buf.render_silence(None);

        loop {
            let tag = bs.read_bits_leq32(3)?;

            match tag {
                ALAC_ELEM_TAG_SCE | ALAC_ELEM_TAG_LFE => {
                    let out0 = self.buf.chan_mut(channel_map[next_channel] as usize);

                    num_frames =
                        decode_sce_or_cpe(&self.config, &mut bs, &mut self.tail_bits, out0, None)?;

                    next_channel += 1;
                }
                ALAC_ELEM_TAG_CPE => {
                    // There may only be one channel left in the output buffer, do not attempt to
                    // decode in this case.
                    if next_channel + 2 > num_channels {
                        break;
                    }

                    let (out0, out1) = self.buf.chan_pair_mut(
                        channel_map[next_channel + 0] as usize,
                        channel_map[next_channel + 1] as usize,
                    );

                    num_frames = decode_sce_or_cpe(
                        &self.config,
                        &mut bs,
                        &mut self.tail_bits,
                        out0,
                        Some(out1),
                    )?;

                    next_channel += 2;
                }
                ALAC_ELEM_TAG_DSE => {
                    let _tag = bs.read_bits_leq32(4)?;
                    let align_flag = bs.read_bool()?;

                    let count = match bs.read_bits_leq32(8)? {
                        val @ 0..=254 => val,
                        val @ 255 => val + bs.read_bits_leq32(8)?,
                        _ => unreachable!(),
                    };

                    if align_flag {
                        bs.realign();
                    }

                    bs.ignore_bits(8 * count)?;
                }
                ALAC_ELEM_TAG_FIL => {
                    let count = match bs.read_bits_leq32(4)? {
                        val @ 0..=14 => val,
                        val @ 15 => val + bs.read_bits_leq32(8)? - 1,
                        _ => unreachable!(),
                    };

                    bs.ignore_bits(8 * count)?;
                }
                ALAC_ELEM_TAG_CCE | ALAC_ELEM_TAG_PCE => {
                    // These elements are unsupported in ALAC version 0.
                    return decode_error("alac: unsupported element");
                }
                ALAC_ELEM_TAG_END => break,
                _ => unreachable!(),
            }

            // Exit if all channels are decoded.
            if next_channel >= num_channels {
                break;
            }
        }

        // Truncate the audio buffer to the number of samples of the last element.
        self.buf.truncate(num_frames);

        // The audio buffer is always signed 32-bit, but the actual bit-depth may be smaller. If
        // the bit-depth is less-than 32, shift the final samples up.
        let shift = 32 - self.config.bit_depth;

        if shift > 0 {
            self.buf.transform(|sample| sample << shift);
        }

        Ok(())
    }
}

impl Decoder for AlacDecoder {
    fn try_new(params: &CodecParameters, _: &DecoderOptions) -> Result<Self> {
        // Verify codec type.
        if params.codec != CODEC_TYPE_ALAC {
            return unsupported_error("alac: invalid codec type");
        }

        // Read the config (magic cookie).
        let config = if let Some(extra_data) = &params.extra_data {
            MagicCookie::try_read(&mut BufReader::new(extra_data))?
        }
        else {
            return unsupported_error("alac: missing extra data");
        };

        let spec = SignalSpec::new(config.sample_rate, config.channel_layout.channels());
        let buf = AudioBuffer::new(u64::from(config.frame_length), spec);

        let max_tail_values = min(2, config.num_channels) as usize * config.frame_length as usize;

        Ok(AlacDecoder { params: params.clone(), tail_bits: vec![0; max_tail_values], buf, config })
    }

    fn reset(&mut self) {
        // Nothing to do.
    }

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[support_codec!(CODEC_TYPE_ALAC, "alac", "Apple Lossless Audio Codec")]
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

/// Reads and decodes a SCE or CPE (if the second output channel not `None`).
fn decode_sce_or_cpe<B: ReadBitsLtr>(
    config: &MagicCookie,
    bs: &mut B,
    tail_bits: &mut [u16],
    out0: &mut [i32],
    mut out1: Option<&mut [i32]>,
) -> Result<usize> {
    // If the second output channel is provided, decode as a Channel Pair Element (CPE), otherwise
    // as a Single Channel Element (SCE).
    let is_cpe = out1.is_some();

    // Element instance tag.
    let _elem_instance_tag = bs.read_bits_leq32(4)?;

    // Unused header bits.
    if bs.read_bits_leq32(12)? != 0 {
        return decode_error("alac: unused header bits not 0");
    };

    let is_partial_frame = bs.read_bool()?;
    let shift = 8 * bs.read_bits_leq32(2)? as u8;
    let is_uncompressed = bs.read_bool()?;

    // The shift must not be >= 24-bits, or exceed the encoded bit-depth.
    if shift >= 8 * 3 || shift >= config.bit_depth {
        return decode_error("alac: invalid shift value");
    }

    // If this is a partial frame, then read the frame length from the element,
    // otherwise use the frame length in the configuration.
    let num_samples =
        if is_partial_frame { bs.read_bits_leq32(32)? } else { config.frame_length } as usize;

    if !is_uncompressed {
        // The number of upper sample bits that will be predicted per channel. This may be less-than
        // the bit-depth if the lower sample bits will be encoded separately. If decoding a CPE,
        // each channel gets an extra bit allocated to it for mid-side encoding.
        let pred_bits = config.bit_depth - shift + if is_cpe { 1 } else { 0 };

        let mid_side_shift = bs.read_bits_leq32(8)? as u8;
        let mid_side_weight = bs.read_bits_leq32_signed(8)?;

        // For SCE elements, the mid-side parameters must (should?) be 0.
        if !is_cpe && (mid_side_shift != 0 || mid_side_weight != 0) {
            return decode_error("alac: invalid mixing information for mono channel");
        }

        // Read the headers for each channel in the element.
        let mut elem0 = ElementChannel::try_read(bs, config, pred_bits)?;
        let mut elem1 =
            if is_cpe { Some(ElementChannel::try_read(bs, config, pred_bits)?) } else { None };

        // If there is a shift, read and save the "tail" bits that will be appended to the predicted
        // samples.
        if shift > 0 {
            let num_tail_values = if is_cpe { 2 } else { 1 } * num_samples;

            for val in &mut tail_bits[..num_tail_values] {
                *val = bs.read_bits_leq32(u32::from(shift))? as u16;
            }
        }

        elem0.read_residuals(bs, &mut out0[..num_samples])?;
        elem0.predict(&mut out0[..num_samples])?;

        if let Some(out1) = out1.as_mut() {
            let elem1 = elem1.as_mut().unwrap();

            elem1.read_residuals(bs, &mut out1[..num_samples])?;
            elem1.predict(&mut out1[..num_samples])?;

            if mid_side_weight != 0 {
                decorrelate_mid_side(out0, out1, mid_side_weight, mid_side_shift);
            }
        }

        // If there is a shift, append the saved "tail" bits to each predicted sample.
        if shift > 0 {
            let out0_iter = out0[..num_samples].iter_mut();

            if let Some(out1) = out1.as_mut() {
                let out1_iter = out1[..num_samples].iter_mut();
                let tail_iter = tail_bits[..2 * num_samples].chunks_exact(2);

                // For a CPE, the tail bits are interleaved.
                for ((s0, s1), vals) in out0_iter.zip(out1_iter).zip(tail_iter) {
                    *s0 = (*s0 << shift) | vals[0] as i32;
                    *s1 = (*s1 << shift) | vals[1] as i32;
                }
            }
            else {
                let tail_iter = tail_bits[..num_samples].iter();

                for (s0, &val) in out0_iter.zip(tail_iter) {
                    *s0 = (*s0 << shift) | val as i32;
                }
            }
        }
    }
    else {
        // Read uncompressed samples directly from the bitstream.
        if let Some(out1) = out1.as_mut() {
            // For a CPE, the samples are interleaved.
            for (s0, s1) in out0[..num_samples].iter_mut().zip(&mut out1[..num_samples]) {
                *s0 = bs.read_bits_leq32_signed(u32::from(config.bit_depth))?;
                *s1 = bs.read_bits_leq32_signed(u32::from(config.bit_depth))?;
            }
        }
        else {
            for s0 in out0[..num_samples].iter_mut() {
                *s0 = bs.read_bits_leq32_signed(u32::from(config.bit_depth))?;
            }
        }
    }

    Ok(num_samples)
}

#[inline(always)]
fn lg3a(val: u32) -> u32 {
    31 - ((val >> 9) + 3).leading_zeros()
}

/// Read a rice code from the bitstream.
#[inline(always)]
fn read_rice_code<B: ReadBitsLtr>(bs: &mut B, k: u32, kb: u32) -> Result<u32> {
    let prefix = bs.read_unary_ones_capped(9)?;

    // If the prefix is > 8, the value is read as an arbitrary width unsigned integer.
    let value = if prefix > 8 {
        bs.read_bits_leq32(kb)?
    }
    else if k > 1 {
        // The reference decoder specifies prefix to be multiplied by a parameter `m`. The parameter
        // `m` is always `(1<<k)-1` which is `2^k - 1`. This can be rewritten using a bit-shift.
        let value = (prefix << k) - prefix;

        // Ideally, we need to read, but not consume, `k` bits here. This is because if the value is
        // less-than 2 we must only consume `k-1` bits. The bit reader does not support peeking,
        // therefore, we read the `k-1` top-most bits. If the value is > 0, then the `k`-bit value
        // would be > 2. In that case, we'll then read the least-significant bit in a second read
        // operation.
        let suffix = bs.read_bits_leq32(k - 1)?;

        if suffix > 0 {
            // Shift suffix left by 1 because it is missing its LSb, and then read the missing bit.
            value + (suffix << 1) + bs.read_bit()? - 1
        }
        else {
            value
        }
    }
    else if k == 1 {
        prefix
    }
    else {
        0
    };

    Ok(value)
}

/// Converts the unsigned rice code into a signed i32.
#[inline(always)]
fn rice_code_to_signed(val: u32) -> i32 {
    // The last bit of the decoded rice value is the sign-bit. See FLAC decoder for a derivation
    // of this function.
    (val >> 1) as i32 ^ -((val & 0x1) as i32)
}

/// Clips `num` most significant bits from the provided value and returns the result.
#[inline(always)]
fn clip_msbs(val: i32, num: u32) -> i32 {
    (val << num) >> num
}

/// Decorrelates a mid-side channel pair.
fn decorrelate_mid_side(out0: &mut [i32], out1: &mut [i32], weight: i32, shift: u8) {
    assert!(out0.len() == out1.len());

    for (s0, s1) in out0.iter_mut().zip(out1.iter_mut()) {
        *s0 = *s0 + *s1 - ((*s1 * weight) >> shift);
        *s1 = *s0 - *s1;
    }
}
