// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::cmp;
use std::convert::TryInto;
use std::num::Wrapping;

use symphonia_common::xiph::audio::flac::StreamInfo;
use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef,
};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::well_known::CODEC_ID_FLAC;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoderOptions};
use symphonia_core::codecs::audio::{AudioDecoder, FinalizeResult, VerificationCheck};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{Error, Result, decode_error, unsupported_error};
use symphonia_core::io::{BitReaderLtr, BufReader, ReadBitsLtr};
use symphonia_core::packet::PacketRef;
use symphonia_core::support_audio_codec;
use symphonia_core::util::bits::{sign_extend_leq32_to_i32, sign_extend_leq64_to_i64};

use log::{debug, log_enabled, warn};

use super::frame::*;
use super::validate::Validator;

/// A sample type that a subframe may be decoded into.
///
/// Subframes are normally decoded into `i32` samples. However, the Side channel of a 32-bit stereo
/// stream requires 33 bits per sample (RFC 9639, section 4.2), and must be decoded into `i64`
/// samples. Residuals always fit in an `i32` (section 9.2.7.3), and predictions are always
/// calculated with `i64`.
trait SubframeSample: Copy + From<i32> + Into<i64> {
    /// The maximum number of bits per sample this type can represent.
    const MAX_BITS: u32;

    /// Read a signed sample of `bps` bits.
    fn read<B: ReadBitsLtr>(bs: &mut B, bps: u32) -> Result<Self>;

    /// Add a prediction to the sample (residual). For `i32`, the prediction is truncated and the
    /// addition wraps. This is correct since the final sample of a valid stream always fits in an
    /// `i32`, even if the prediction alone does not.
    fn add_prediction(self, prediction: i64) -> Self;

    /// Shift the sample left by `shift` bits, wrapping on overflow.
    fn wrapping_shl(self, shift: u32) -> Self;
}

impl SubframeSample for i32 {
    const MAX_BITS: u32 = i32::BITS;

    #[inline(always)]
    fn read<B: ReadBitsLtr>(bs: &mut B, bps: u32) -> Result<Self> {
        Ok(sign_extend_leq32_to_i32(bs.read_bits_leq32(bps)?, bps))
    }

    #[inline(always)]
    fn add_prediction(self, prediction: i64) -> Self {
        self.wrapping_add(prediction as i32)
    }

    #[inline(always)]
    fn wrapping_shl(self, shift: u32) -> Self {
        i32::wrapping_shl(self, shift)
    }
}

impl SubframeSample for i64 {
    const MAX_BITS: u32 = i64::BITS;

    #[inline(always)]
    fn read<B: ReadBitsLtr>(bs: &mut B, bps: u32) -> Result<Self> {
        Ok(sign_extend_leq64_to_i64(bs.read_bits_leq64(bps)?, bps))
    }

    #[inline(always)]
    fn add_prediction(self, prediction: i64) -> Self {
        self.wrapping_add(prediction)
    }

    #[inline(always)]
    fn wrapping_shl(self, shift: u32) -> Self {
        i64::wrapping_shl(self, shift)
    }
}

fn decorrelate_left_side(left: &[i32], side: &mut [i32]) {
    for (s, l) in side.iter_mut().zip(left) {
        *s = *l - *s;
    }
}

fn decorrelate_mid_side(mid: &mut [i32], side: &mut [i32]) {
    for (m, s) in mid.iter_mut().zip(side) {
        // Mid (M) is given as M = L/2 + R/2, while Side (S) is given as S = L - R.
        //
        // To calculate the individual channels, the following equations can be used:
        //      - L = S/2 + M
        //      - R = M - S/2
        //
        // Ideally, this would work, but since samples are represented as integers, division yields
        // the floor of the divided value. Therefore, the channel restoration equations actually
        // yield:
        //      - L = floor(S/2) + M
        //      - R = M - floor(S/2)
        //
        // This will produce incorrect samples whenever the sample S is odd. For example:
        //      - 2/2 = 1
        //      - 3/2 = 1 (should be 2 if rounded!)
        //
        // To get the proper rounding behaviour, the solution is to add one to the result if S is
        // odd:
        //      - L = floor(S/2) + M + (S%2) = M + (S%2) + floor(S/2)
        //      - R = M - floor(S/2) + (S%2) = M + (S%2) - floor(S/2)
        //
        // Further, to prevent loss of accuracy, instead of dividing S/2 and adding or subtracting
        // it from M, multiply M*2, then add or subtract S, and then divide the whole result by 2.
        // This gives one extra bit of precision for the intermediate computations.
        //
        // Conveniently, since M should be doubled, the LSB will always be 0. This allows S%2 to
        // be added simply by bitwise ORing S&1 to M<<1.
        //
        // Therefore the final equations yield:
        //      - L = (2*M + (S%2) + S) / 2
        //      - R = (2*M + (S%2) - S) / 2
        let mid = (*m << 1) | (*s & 1);
        let side = *s;
        *m = (mid + side) >> 1;
        *s = (mid - side) >> 1;
    }
}

fn decorrelate_right_side(right: &[i32], side: &mut [i32]) {
    for (s, r) in side.iter_mut().zip(right) {
        *s += *r;
    }
}

// The following functions restore the Left and Right channels when the Side channel was decoded
// into `i64` samples (i.e., 32-bit streams). The channel that would otherwise replace the Side
// channel in-place is written to `out`.

fn decorrelate_left_side_wide(left: &[i32], side: &[i64], out: &mut [i32]) {
    for ((o, &l), &s) in out.iter_mut().zip(left).zip(side) {
        *o = i64::from(l).wrapping_sub(s) as i32;
    }
}

fn decorrelate_mid_side_wide(mid: &mut [i32], side: &[i64], out: &mut [i32]) {
    for ((m, o), &s) in mid.iter_mut().zip(out.iter_mut()).zip(side) {
        // See `decorrelate_mid_side`. The intermediate values require up-to 34 bits.
        let mid = (i64::from(*m) << 1) | (s & 1);
        *m = (mid.wrapping_add(s) >> 1) as i32;
        *o = (mid.wrapping_sub(s) >> 1) as i32;
    }
}

fn decorrelate_right_side_wide(right: &[i32], side: &[i64], out: &mut [i32]) {
    for ((o, &r), &s) in out.iter_mut().zip(right).zip(side) {
        *o = s.wrapping_add(i64::from(r)) as i32;
    }
}

/// Free Lossless Audio Codec (FLAC) decoder.
pub struct FlacDecoder {
    params: AudioCodecParameters,
    is_validating: bool,
    validator: Validator,
    buf: AudioBuffer<i32>,
    /// Buffer for 33-bit Side channel samples. Only used for 32-bit stereo streams.
    side_buf: Vec<i64>,
}

impl FlacDecoder {
    pub fn try_new(params: &AudioCodecParameters, options: &AudioDecoderOptions) -> Result<Self> {
        // This decoder only supports FLAC.
        if params.codec != CODEC_ID_FLAC {
            return unsupported_error("flac: invalid codec");
        }

        // Obtain the extra data.
        let extra_data = match params.extra_data.as_ref() {
            Some(buf) => buf,
            _ => return unsupported_error("flac: missing extra data"),
        };

        // Read the stream information block.
        let info = StreamInfo::read(&mut BufReader::new(extra_data))?;

        // Clone the codec parameters so that the parameters can be supplemented and/or amended.
        let mut params = params.clone();

        // Amend the provided codec parameters with information from the stream information block.
        params
            .with_sample_rate(info.sample_rate)
            .with_bits_per_sample(info.bits_per_sample)
            .with_max_frames_per_packet(u64::from(info.block_len_max))
            .with_channels(info.channels.clone());

        if let Some(md5) = info.md5 {
            params.with_verification_code(VerificationCheck::Md5(md5));
        }

        let spec = AudioSpec::new(info.sample_rate, info.channels.clone());
        let buf = AudioBuffer::new(spec, usize::from(info.block_len_max));

        // TODO: Verify packet integrity if the demuxer is not.
        // if !params.packet_data_integrity {
        //     return unsupported_error("flac: packet integrity is required");
        // }

        Ok(FlacDecoder {
            params,
            is_validating: options.verify,
            validator: Default::default(),
            buf,
            side_buf: Vec::new(),
        })
    }

    fn decode_inner(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        let mut reader = packet.as_buf_reader();

        // Synchronize to a frame and get the synchronization code.
        let sync = sync_frame(&mut reader)?;

        let header = read_frame_header(&mut reader, sync)?;

        // Use the bits per sample and sample rate as stated in the frame header, falling back to
        // the stream information if provided. If neither are available, return an error.
        let bits_per_sample = if let Some(bps) = header.bits_per_sample {
            bps
        }
        else if let Some(bps) = self.params.bits_per_sample {
            bps
        }
        else {
            return decode_error("flac: bits per sample not provided");
        };
        if bits_per_sample > u32::BITS {
            return decode_error("flac: invalid bit width");
        }

        // trace!("frame: [{:?}] strategy={:?}, n_samples={}, bps={}, channels={:?}",
        //     header.block_sequence,
        //     header.blocking_strategy,
        //     header.block_num_samples,
        //     bits_per_sample,
        //     &header.channel_assignment);

        // Reserve a writeable chunk in the buffer equal to the number of samples in the block.
        if header.block_num_samples as usize > self.buf.capacity() {
            return decode_error("flac: allocation would overflow buffer");
        }
        self.buf.clear();
        self.buf.render_uninit(Some(header.block_num_samples as usize));

        // Only Bitstream reading for subframes.
        {
            // Sub-frames don't have any byte-aligned content, so use a BitReader.
            let mut bs = BitReaderLtr::new(reader.read_buf_bytes_available_ref());

            // Read each subframe based on the channel assignment into a planar buffer.
            match header.channel_assignment {
                ChannelAssignment::Independant(channels) => {
                    for i in 0..channels as usize {
                        read_subframe(
                            &mut bs,
                            bits_per_sample,
                            self.buf
                                .plane_mut(i)
                                .ok_or(Error::DecodeError("flac: unexpected channel assignment"))?,
                        )?;
                    }
                }
                // For Left/Side, Mid/Side, and Right/Side channel configurations, the Side
                // (Difference) channel requires an extra bit per sample. For 32-bit streams, the
                // Side channel therefore can't be decoded in-place and is decoded into `side_buf`.
                ChannelAssignment::LeftSide => {
                    let (left, side) = self
                        .buf
                        .plane_pair_mut(0, 1)
                        .ok_or(Error::DecodeError("flac: unexpected channel assignment"))?;

                    read_subframe(&mut bs, bits_per_sample, left)?;

                    if bits_per_sample < u32::BITS {
                        read_subframe(&mut bs, bits_per_sample + 1, side)?;
                        decorrelate_left_side(left, side);
                    }
                    else {
                        let wide = side_buf(&mut self.side_buf, side.len());
                        read_subframe(&mut bs, bits_per_sample + 1, wide)?;
                        decorrelate_left_side_wide(left, wide, side);
                    }
                }
                ChannelAssignment::MidSide => {
                    let (mid, side) = self
                        .buf
                        .plane_pair_mut(0, 1)
                        .ok_or(Error::DecodeError("flac: unexpected channel assignment"))?;

                    read_subframe(&mut bs, bits_per_sample, mid)?;

                    if bits_per_sample < u32::BITS {
                        read_subframe(&mut bs, bits_per_sample + 1, side)?;
                        decorrelate_mid_side(mid, side);
                    }
                    else {
                        let wide = side_buf(&mut self.side_buf, side.len());
                        read_subframe(&mut bs, bits_per_sample + 1, wide)?;
                        decorrelate_mid_side_wide(mid, wide, side);
                    }
                }
                ChannelAssignment::RightSide => {
                    let (side, right) = self
                        .buf
                        .plane_pair_mut(0, 1)
                        .ok_or(Error::DecodeError("flac: unexpected channel assignment"))?;

                    if bits_per_sample < u32::BITS {
                        read_subframe(&mut bs, bits_per_sample + 1, side)?;
                        read_subframe(&mut bs, bits_per_sample, right)?;
                        decorrelate_right_side(right, side);
                    }
                    else {
                        let wide = side_buf(&mut self.side_buf, side.len());
                        read_subframe(&mut bs, bits_per_sample + 1, wide)?;
                        read_subframe(&mut bs, bits_per_sample, right)?;
                        decorrelate_right_side_wide(right, wide, side);
                    }
                }
            }
        }

        // Feed the validator if validation is enabled.
        if self.is_validating {
            self.validator.update(&self.buf, bits_per_sample);
        }

        // The decoder uses a 32bit sample format as a common denominator, but that doesn't mean
        // the encoded audio samples are actually 32bit. Shift all samples in the output buffer
        // so that regardless the encoded bits/sample, the output is always 32bits/sample.
        if bits_per_sample < 32 {
            let shift = 32 - bits_per_sample;
            self.buf.apply(|sample| sample << shift);
        }

        Ok(())
    }
}

/// Get a slice of `len` samples from the Side channel buffer, growing it if required.
fn side_buf(buf: &mut Vec<i64>, len: usize) -> &mut [i64] {
    if buf.len() < len {
        buf.resize(len, 0);
    }
    &mut buf[..len]
}

impl AudioDecoder for FlacDecoder {
    fn codec_info(&self) -> &CodecInfo {
        // Only one codec is supported.
        &Self::supported_codecs().first().expect("at least one codec registered").info
    }

    fn reset(&mut self) {
        // No state is stored between packets, therefore do nothing.
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
        let mut result: FinalizeResult = Default::default();

        // If verifying...
        if self.is_validating {
            // Try to get the expected MD5 checksum and compare it against the decoded checksum.
            if let Some(VerificationCheck::Md5(expected)) = self.params.verification_check {
                let decoded = self.validator.md5();

                // Only generate the expected and decoded MD5 checksum strings if logging is
                // enabled at the debug level.
                if log_enabled!(log::Level::Debug) {
                    use std::fmt::Write;

                    let mut expected_s = String::with_capacity(32);
                    let mut decoded_s = String::with_capacity(32);

                    expected.iter().for_each(|b| {
                        write!(expected_s, "{b:02x}").expect("write to String never fails")
                    });
                    decoded.iter().for_each(|b| {
                        write!(decoded_s, "{b:02x}").expect("write to String never fails")
                    });

                    debug!("verification: expected md5 = {expected_s}");
                    debug!("verification: decoded md5  = {decoded_s}");
                }

                result.verify_ok = Some(decoded == expected)
            }
            else {
                warn!("verification requested but the expected md5 checksum was not provided");
            }
        }

        result
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for FlacDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(FlacDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(CODEC_ID_FLAC, "flac", "Free Lossless Audio Codec")]
    }
}

// Subframe business

#[derive(Debug)]
enum SubFrameType {
    Constant,
    Verbatim,
    FixedLinear(u32),
    Linear(u32),
}

fn read_subframe<B: ReadBitsLtr, S: SubframeSample>(
    bs: &mut B,
    frame_bps: u32,
    buf: &mut [S],
) -> Result<()> {
    if frame_bps > S::MAX_BITS {
        return decode_error("flac: subframe bit width exceeds sample type");
    }

    // First sub-frame bit must always 0.
    if bs.read_bool()? {
        return decode_error("flac: subframe padding is not 0");
    }

    // Next 6 bits designate the sub-frame type.
    let subframe_type_enc = bs.read_bits_leq32(6)?;

    let subframe_type = match subframe_type_enc {
        0x00 => SubFrameType::Constant,
        0x01 => SubFrameType::Verbatim,
        0x08..=0x0f => {
            let order = subframe_type_enc & 0x07;
            // The Fixed Predictor only supports orders between 0 and 4.
            if order > 4 {
                return decode_error("flac: fixed predictor orders of greater than 4 are invalid");
            }
            SubFrameType::FixedLinear(order)
        }
        0x20..=0x3f => SubFrameType::Linear((subframe_type_enc & 0x1f) + 1),
        _ => {
            return decode_error("flac: subframe type set to reserved value");
        }
    };

    // Bit 7 of the sub-frame header designates if there are any dropped (wasted in FLAC terms)
    // bits per sample in the audio sub-block. If the bit is set, unary decode the number of
    // dropped bits per sample.
    let dropped_bps = if bs.read_bool()? { bs.read_unary_zeros()? + 1 } else { 0 };
    if dropped_bps > frame_bps {
        return decode_error(
            "flac: dropped bits per sample is greater than the frame bits per sample",
        );
    }

    // The bits per sample stated in the frame header is for the decoded audio sub-block samples.
    // However, it is likely that the lower order bits of all the samples are simply 0. Therefore,
    // the encoder will truncate `dropped_bps` of lower order bits for every sample in a sub-block.
    // The decoder simply needs to shift left all samples by `dropped_bps` after decoding the
    // sub-frame and obtaining the truncated audio sub-block samples.
    let bps = frame_bps - dropped_bps;

    // trace!("\tsubframe: type={:?}, bps={}, dropped_bps={}",
    //     &subframe_type,
    //     bps,
    //     dropped_bps);

    match subframe_type {
        SubFrameType::Constant => decode_constant(bs, bps, buf)?,
        SubFrameType::Verbatim => decode_verbatim(bs, bps, buf)?,
        SubFrameType::FixedLinear(order) => decode_fixed_linear(bs, bps, order, buf)?,
        SubFrameType::Linear(order) => decode_linear(bs, bps, order, buf)?,
    };

    // Shift the samples to account for the dropped bits.
    samples_shl(dropped_bps, buf);

    Ok(())
}

#[inline(always)]
fn samples_shl<S: SubframeSample>(shift: u32, buf: &mut [S]) {
    if shift > 0 {
        for sample in buf.iter_mut() {
            *sample = sample.wrapping_shl(shift);
        }
    }
}

fn decode_constant<B: ReadBitsLtr, S: SubframeSample>(
    bs: &mut B,
    bps: u32,
    buf: &mut [S],
) -> Result<()> {
    let const_sample = S::read(bs, bps)?;

    for sample in buf.iter_mut() {
        *sample = const_sample;
    }

    Ok(())
}

fn decode_verbatim<B: ReadBitsLtr, S: SubframeSample>(
    bs: &mut B,
    bps: u32,
    buf: &mut [S],
) -> Result<()> {
    for sample in buf.iter_mut() {
        *sample = S::read(bs, bps)?;
    }

    Ok(())
}

fn decode_fixed_linear<B: ReadBitsLtr, S: SubframeSample>(
    bs: &mut B,
    bps: u32,
    order: u32,
    buf: &mut [S],
) -> Result<()> {
    if order as usize > buf.len() {
        return decode_error("flac: fixed predictor order is greater than the block size");
    }

    // The first `order` samples are encoded verbatim to warm-up the LPC decoder.
    decode_verbatim(bs, bps, &mut buf[..order as usize])?;

    // Decode the residuals for the predicted samples.
    decode_residual(bs, order, buf)?;

    // Run the Fixed predictor (appends to residuals).
    //
    // TODO: The fixed predictor uses 64-bit accumulators by default to support bps > 26. On 64-bit
    // machines, this is preferable, but on 32-bit machines if bps <= 26, run a 32-bit predictor,
    // and fallback to the 64-bit predictor if necessary (which is basically never).
    fixed_predict(order, buf);

    Ok(())
}

fn decode_linear<B: ReadBitsLtr, S: SubframeSample>(
    bs: &mut B,
    bps: u32,
    order: u32,
    buf: &mut [S],
) -> Result<()> {
    if order as usize > buf.len() {
        return decode_error("flac: predictor order is greater than the block size");
    }

    // The order of the Linear Predictor should be between 1 and 32.
    debug_assert!(order > 0 && order <= 32);

    // The first `order` samples are encoded verbatim to warm-up the LPC decoder.
    decode_verbatim(bs, bps, &mut buf[0..order as usize])?;

    // Quantized linear predictor (QLP) coefficients precision in bits (1-16).
    let qlp_precision = bs.read_bits_leq32(4)? + 1;

    if qlp_precision > 15 {
        return decode_error("flac: qlp precision set to reserved value");
    }

    // QLP coefficients bit shift [-16, 15].
    let qlp_coeff_shift = sign_extend_leq32_to_i32(bs.read_bits_leq32(5)?, 5);

    if qlp_coeff_shift >= 0 {
        let mut qlp_coeffs = [0i32; 32];

        for c in qlp_coeffs.iter_mut().rev().take(order as usize) {
            *c = sign_extend_leq32_to_i32(bs.read_bits_leq32(qlp_precision)?, qlp_precision);
        }

        decode_residual(bs, order, buf)?;

        // Helper function to dispatch to a predictor with a maximum order of N.
        #[inline(always)]
        fn lpc<const N: usize, S: SubframeSample>(
            order: u32,
            coeffs: &[i32; 32],
            coeff_shift: i32,
            buf: &mut [S],
        ) {
            let coeffs_n = (&coeffs[32 - N..32]).try_into().expect("slice has exactly N elements");
            lpc_predict::<N, _>(order as usize, coeffs_n, coeff_shift as u32, buf);
        }

        // Pick the best length linear predictor to use based on the order. Most FLAC streams use
        // the subset format and have an order <= 12. Therefore, for orders <= 12, dispatch to
        // predictors that roughly match the order. If a predictor is too long for a given order,
        // then there will be wasted computations. On the other hand, it is not worth the code bloat
        // to specialize for every order <= 12.
        match order {
            0..=4 => lpc::<4, _>(order, &qlp_coeffs, qlp_coeff_shift, buf),
            5..=6 => lpc::<6, _>(order, &qlp_coeffs, qlp_coeff_shift, buf),
            7..=8 => lpc::<8, _>(order, &qlp_coeffs, qlp_coeff_shift, buf),
            9..=10 => lpc::<10, _>(order, &qlp_coeffs, qlp_coeff_shift, buf),
            11..=12 => lpc::<12, _>(order, &qlp_coeffs, qlp_coeff_shift, buf),
            _ => lpc::<32, _>(order, &qlp_coeffs, qlp_coeff_shift, buf),
        };
    }
    else {
        return unsupported_error("flac: lpc shifts less than 0 are not supported");
    }

    Ok(())
}

fn decode_residual<B: ReadBitsLtr, S: SubframeSample>(
    bs: &mut B,
    n_prelude_samples: u32,
    buf: &mut [S],
) -> Result<()> {
    let method_enc = bs.read_bits_leq32(2)?;

    // The FLAC specification defines two residual coding methods: Rice and Rice2. The
    // only difference between the two is the bit width of the Rice parameter. Note the
    // bit width based on the residual encoding method and use the same code path for
    // both cases.
    let param_bit_width = match method_enc {
        0x0 => 4,
        0x1 => 5,
        _ => {
            return decode_error("flac: residual method set to reserved value");
        }
    };

    // Read the partition order.
    let order = bs.read_bits_leq32(4)?;

    // The number of partitions is equal to 2^order.
    let n_partitions = 1usize << order;

    // In general, all partitions have the same number of samples such that the sum of all partition
    // lengths equal the block length. The number of samples in a partition can therefore be
    // calculated with block_size / 2^order *in general*. However, since there are warm-up samples
    // stored verbatim, the first partition has n_prelude_samples less samples. Likewise, if there
    // is only one partition, then it too has n_prelude_samples less samples.
    let n_partition_samples = buf.len() >> order;

    // The size of the first (and/or only) partition as per the specification is n_partition_samples
    // minus the number of warm-up samples (which is the predictor order). Ensure the number of
    // samples in these types of partitions cannot be negative.
    if n_prelude_samples as usize > n_partition_samples {
        return decode_error("flac: residual partition too small for given predictor order");
    }

    // Ensure that the sum of all partition lengths equal the block size.
    if n_partitions * n_partition_samples != buf.len() {
        return decode_error("flac: block size is not same as encoded residual");
    }

    // trace!("\t\tresidual: n_partitions={}, n_partition_samples={}, n_prelude_samples={}",
    //     n_partitions,
    //     n_partition_samples,
    //     n_prelude_samples);

    // Decode the first partition as it may have less than n_partition_samples samples.
    decode_rice_partition(
        bs,
        param_bit_width,
        &mut buf[n_prelude_samples as usize..n_partition_samples],
    )?;

    // Decode the remaining partitions.
    for buf_chunk in buf[n_partition_samples..].chunks_mut(n_partition_samples) {
        decode_rice_partition(bs, param_bit_width, buf_chunk)?;
    }

    Ok(())
}

fn decode_rice_partition<B: ReadBitsLtr, S: SubframeSample>(
    bs: &mut B,
    param_bit_width: u32,
    buf: &mut [S],
) -> Result<()> {
    // Read the encoding parameter, generally the Rice parameter.
    let rice_param = bs.read_bits_leq32(param_bit_width)?;

    // If the Rice parameter is all 1s (e.g., 0xf for a 4bit parameter, 0x1f for a 5bit parameter),
    // then it indicates that residuals in this partition are not Rice encoded, rather they are
    // binary encoded. Conversely, if the parameter is less than this value, the residuals are Rice
    // encoded.
    if rice_param < (1 << param_bit_width) - 1 {
        // println!("\t\t\tPartition (Rice): n_residuals={}, rice_param={}", buf.len(), rice_param);

        // Read each rice encoded residual and store in buffer.
        for sample in buf.iter_mut() {
            let q = bs.read_unary_zeros()?;
            let r = bs.read_bits_leq32(rice_param)?;
            *sample = S::from(rice_signed_to_i32((q << rice_param) | r));
        }
    }
    else {
        let residual_bits = bs.read_bits_leq32(5)?;

        // trace!(
        //     "\t\t\tpartition (Binary): n_residuals={}, residual_bits={}",
        //     buf.len(),
        //     residual_bits
        // );

        // Read each binary encoded residual and store in buffer.
        for sample in buf.iter_mut() {
            let residual =
                sign_extend_leq32_to_i32(bs.read_bits_leq32(residual_bits)?, residual_bits);
            *sample = S::from(residual);
        }
    }

    Ok(())
}

#[inline(always)]
fn rice_signed_to_i32(word: u32) -> i32 {
    // Input  => 0  1  2  3  4  5  6  7  8  9  10
    // Output => 0 -1  1 -2  2 -3  3 -4  4 -5   5
    //
    //  - If even: output = input / 2
    //  - If odd:  output = -(input + 1) / 2
    //                    =  (input / 2) - 1

    // Divide the input by 2 and convert to signed.
    let div2 = (word >> 1) as i32;

    // Using the LSB of the input, create a new signed integer that's either
    // -1 (0b1111_11110) or 0 (0b0000_0000). For odd inputs, this will be -1, for even
    // inputs it'll be 0.
    let sign = -((word & 0x1) as i32);

    // XOR the div2 result with the sign. If sign is 0, the XOR produces div2. If sign is -1, then
    // -div2 - 1 is returned.
    //
    // Example:  input = 9 => div2 = 0b0000_0100, sign = 0b1111_11110
    //
    //           div2 ^ sign =   0b0000_0100
    //                         ^ 0b1111_1110
    //                           -----------
    //                           0b1111_1011  (-5)
    div2 ^ sign
}

#[test]
fn verify_rice_signed_to_i32() {
    assert_eq!(rice_signed_to_i32(0), 0);
    assert_eq!(rice_signed_to_i32(1), -1);
    assert_eq!(rice_signed_to_i32(2), 1);
    assert_eq!(rice_signed_to_i32(3), -2);
    assert_eq!(rice_signed_to_i32(4), 2);
    assert_eq!(rice_signed_to_i32(5), -3);
    assert_eq!(rice_signed_to_i32(6), 3);
    assert_eq!(rice_signed_to_i32(7), -4);
    assert_eq!(rice_signed_to_i32(8), 4);
    assert_eq!(rice_signed_to_i32(9), -5);
    assert_eq!(rice_signed_to_i32(10), 5);

    assert_eq!(rice_signed_to_i32(u32::max_value()), -2_147_483_648);
}

fn fixed_predict<S: SubframeSample>(order: u32, buf: &mut [S]) {
    debug_assert!(order <= 4);

    // The Fixed Predictor is just a hard-coded version of the Linear Predictor up to order 4 and
    // with fixed coefficients. Some cases may be simplified such as orders 0 and 1. For orders 2
    // through 4, use the same IIR-style algorithm as the Linear Predictor.
    match order {
        // A 0th order predictor always predicts 0, and therefore adds nothing to any of the samples
        // in buf. Do nothing.
        0 => (),
        // A 1st order predictor always returns the previous sample since the polynomial is:
        // s(i) = 1*s(i),
        1 => {
            for i in 1..buf.len() {
                buf[i] = buf[i].add_prediction(buf[i - 1].into());
            }
        }
        // A 2nd order predictor uses the polynomial: s(i) = 2*s(i-1) - 1*s(i-2).
        2 => {
            for i in 2..buf.len() {
                let a = Wrapping(-1) * Wrapping(buf[i - 2].into());
                let b = Wrapping(2) * Wrapping(buf[i - 1].into());
                buf[i] = buf[i].add_prediction((a + b).0);
            }
        }
        // A 3rd order predictor uses the polynomial: s(i) = 3*s(i-1) - 3*s(i-2) + 1*s(i-3).
        3 => {
            for i in 3..buf.len() {
                let a = Wrapping(1) * Wrapping(buf[i - 3].into());
                let b = Wrapping(-3) * Wrapping(buf[i - 2].into());
                let c = Wrapping(3) * Wrapping(buf[i - 1].into());
                buf[i] = buf[i].add_prediction((a + b + c).0);
            }
        }
        // A 4th order predictor uses the polynomial:
        // s(i) = 4*s(i-1) - 6*s(i-2) + 4*s(i-3) - 1*s(i-4).
        4 => {
            for i in 4..buf.len() {
                let a = Wrapping(-1) * Wrapping(buf[i - 4].into());
                let b = Wrapping(4) * Wrapping(buf[i - 3].into());
                let c = Wrapping(-6) * Wrapping(buf[i - 2].into());
                let d = Wrapping(4) * Wrapping(buf[i - 1].into());
                buf[i] = buf[i].add_prediction((a + b + c + d).0);
            }
        }
        _ => unreachable!(),
    };
}

/// Generalized Linear Predictive Coding (LPC) decoder. The exact number of coefficients given is
/// specified by `order`. Coefficients must be stored in reverse order in `coeffs` with the first
/// coefficient at index 31. Coefficients at indices less than 31 - `order` must be 0.
/// It is expected that the first `order` samples in `buf` are warm-up samples.
fn lpc_predict<const N: usize, S: SubframeSample>(
    order: usize,
    coeffs: &[i32; N],
    coeff_shift: u32,
    buf: &mut [S],
) {
    // Order must be less than or equal to the number of coefficients.
    debug_assert!(order <= coeffs.len());

    // Order must be less than to equal to the number of samples the buffer can hold.
    debug_assert!(order <= buf.len());

    // The main, efficient, predictor loop needs N previous samples to run. Since order <= N,
    // calculate enough samples to reach N.
    let n_prefill = cmp::min(N, buf.len()) - order;

    // For valid streams, the prediction cannot overflow an i64 since the coefficients are at most
    // 15 bits, and samples at most 33 bits. Wrapping arithmetic is used to prevent invalid streams
    // from causing a panic.
    for i in order..order + n_prefill {
        let predicted = coeffs[N - order..N]
            .iter()
            .zip(&buf[i - order..i])
            .map(|(&c, &s)| i64::from(c).wrapping_mul(s.into()))
            .fold(0, i64::wrapping_add);

        buf[i] = buf[i].add_prediction(predicted >> coeff_shift);
    }

    // If the pre-fill operation filled the entire sample buffer, return immediately.
    if buf.len() <= N {
        return;
    }

    // Main predictor loop. Calculate each sample by applying what is essentially an IIR filter.
    for i in N..buf.len() {
        let predicted = coeffs
            .iter()
            .zip(&buf[i - N..i])
            .map(|(&c, &s)| i64::from(c).wrapping_mul(s.into()))
            .fold(0, i64::wrapping_add);

        buf[i] = buf[i].add_prediction(predicted >> coeff_shift);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a bitstream, most-significant bit first.
    #[derive(Default)]
    struct BitWriter {
        buf: Vec<u8>,
        n_bits: usize,
    }

    impl BitWriter {
        /// Write the low `width` bits of `value`.
        fn write(&mut self, value: u64, width: u32) {
            for i in (0..width).rev() {
                if self.n_bits % 8 == 0 {
                    self.buf.push(0);
                }
                let bit = ((value >> i) & 1) as u8;
                *self.buf.last_mut().unwrap() |= bit << (7 - (self.n_bits % 8));
                self.n_bits += 1;
            }
        }

        fn write_signed(&mut self, value: i64, width: u32) {
            self.write(value as u64, width);
        }

        fn write_rice(&mut self, residual: i32, rice_param: u32) {
            let folded = ((residual << 1) ^ (residual >> 31)) as u32;
            for _ in 0..(folded >> rice_param) {
                self.write(0, 1);
            }
            self.write(1, 1);
            self.write(u64::from(folded), rice_param);
        }

        fn write_subframe_header(&mut self, subframe_type: u64, dropped_bps: u32) {
            self.write(0, 1);
            self.write(subframe_type, 6);
            if dropped_bps > 0 {
                self.write(1, 1);
                self.write(0, dropped_bps - 1);
                self.write(1, 1);
            }
            else {
                self.write(0, 1);
            }
        }

        /// Write a single partition of Rice coded residuals.
        fn write_residuals(&mut self, residuals: &[i32], rice_param: u32) {
            // Rice coding method, partition order 0.
            self.write(0, 2);
            self.write(0, 4);
            self.write(u64::from(rice_param), 4);
            for &r in residuals {
                self.write_rice(r, rice_param);
            }
        }

        fn finish(mut self) -> Vec<u8> {
            // Pad so the reader can always fetch a full cache.
            self.buf.extend_from_slice(&[0; 8]);
            self.buf
        }
    }

    // A 33-bit Side channel can represent values in the range [-2^32, 2^32 - 1].
    const SIDE_MIN: i64 = -(1 << 32);
    const SIDE_MAX: i64 = (1 << 32) - 1;

    #[test]
    fn verify_33bit_verbatim_subframe() {
        let samples = [SIDE_MIN, SIDE_MAX, 0, -1, i64::from(i32::MIN) - 1, i64::from(i32::MAX) + 1];

        let mut w = BitWriter::default();
        w.write_subframe_header(0x01, 0);
        for &s in &samples {
            w.write_signed(s, 33);
        }
        let data = w.finish();

        let mut buf = [0i64; 6];
        read_subframe(&mut BitReaderLtr::new(&data), 33, &mut buf).unwrap();
        assert_eq!(buf, samples);
    }

    #[test]
    fn verify_33bit_constant_subframe_with_dropped_bits() {
        // A 32-bit stream with 24-bit content has 8 dropped bits, but the Side channel can still
        // exceed 32 bits once shifted.
        let mut w = BitWriter::default();
        w.write_subframe_header(0x00, 8);
        w.write_signed(-(1 << 24), 25);
        let data = w.finish();

        let mut buf = [0i64; 4];
        read_subframe(&mut BitReaderLtr::new(&data), 33, &mut buf).unwrap();
        assert_eq!(buf, [SIDE_MIN; 4]);
    }

    #[test]
    fn verify_33bit_fixed_subframe() {
        let warmup = [4_000_000_000i64, 3_900_000_000];
        let residuals = [7, -3, 0, 12, -25, 1];

        let mut w = BitWriter::default();
        w.write_subframe_header(0x08 | 2, 0);
        for &s in &warmup {
            w.write_signed(s, 33);
        }
        w.write_residuals(&residuals, 5);
        let data = w.finish();

        let mut expected = warmup.to_vec();
        for &r in &residuals {
            let n = expected.len();
            expected.push(2 * expected[n - 1] - expected[n - 2] + i64::from(r));
        }

        let mut buf = [0i64; 8];
        read_subframe(&mut BitReaderLtr::new(&data), 33, &mut buf).unwrap();
        assert_eq!(buf[..], expected[..]);
    }

    #[test]
    fn verify_33bit_lpc_subframe() {
        let warmup = [3_000_000_000i64, 3_010_000_000];
        let coeffs = [15565i32, -7373];
        let shift = 13;
        let residuals = [100, -200, 3, 0, 4095, -4096];

        let mut w = BitWriter::default();
        w.write_subframe_header(0x20 | (coeffs.len() as u64 - 1), 0);
        for &s in &warmup {
            w.write_signed(s, 33);
        }
        // QLP precision (15 bits) minus 1, and the QLP shift.
        w.write(14, 4);
        w.write(shift, 5);
        for &c in &coeffs {
            w.write_signed(i64::from(c), 15);
        }
        w.write_residuals(&residuals, 8);
        let data = w.finish();

        let mut expected = warmup.to_vec();
        for &r in &residuals {
            let n = expected.len();
            let prediction =
                i64::from(coeffs[0]) * expected[n - 1] + i64::from(coeffs[1]) * expected[n - 2];
            expected.push((prediction >> shift) + i64::from(r));
        }

        let mut buf = [0i64; 8];
        read_subframe(&mut BitReaderLtr::new(&data), 33, &mut buf).unwrap();
        assert_eq!(buf[..], expected[..]);
    }

    #[test]
    fn verify_i32_subframe_rejects_33bits() {
        let mut w = BitWriter::default();
        w.write_subframe_header(0x01, 0);
        let data = w.finish();

        let mut buf = [0i32; 4];
        assert!(read_subframe(&mut BitReaderLtr::new(&data), 33, &mut buf).is_err());
    }

    #[test]
    fn verify_wide_decorrelation() {
        let values = [i32::MIN, i32::MIN + 1, -1, 0, 1, i32::MAX - 1, i32::MAX];

        for &l in &values {
            for &r in &values {
                let side = [i64::from(l) - i64::from(r)];
                let mid = (i64::from(l) + i64::from(r)) >> 1;

                let mut out = [0];
                decorrelate_left_side_wide(&[l], &side, &mut out);
                assert_eq!(out, [r], "left/side l={l} r={r}");

                let mut out = [0];
                decorrelate_right_side_wide(&[r], &side, &mut out);
                assert_eq!(out, [l], "right/side l={l} r={r}");

                let mut mid = [mid as i32];
                let mut out = [0];
                decorrelate_mid_side_wide(&mut mid, &side, &mut out);
                assert_eq!((mid, out), ([l], [r]), "mid/side l={l} r={r}");
            }
        }
    }
}
