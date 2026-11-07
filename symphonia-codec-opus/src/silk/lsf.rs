//! SILK Normalized Line Spectral Frequency (NLSF) Decoding and LPC Conversion
//!
//! Implements two-stage NLSF codebook decoding, backward prediction, inverse weighting,
//! minimum-spacing stabilization, 4-subframe interpolation, and NLSF-to-LPC conversion
//! (RFC 6716 Sections 4.2.7.5 and 4.2.7.9).

use symphonia_core::errors::Result;

use crate::entropy;
use crate::silk::constant;
use crate::silk::decoder::Decoder;
use crate::silk::error::Error;
use crate::silk::{
    Frame, MAX_LPC_ORDER, SignalType, inverse32_var_q, mul_high_i32, mul_q16, rshift_round,
    rshift_round64, sat16, sub_sat32,
};
use crate::toc::{Bandwidth, FrameDuration};

impl Decoder {
    /// Decodes the Normalized Line Spectral Frequencies (NLSF) for a SILK frame,
    /// interpolates across subframes when applicable, and converts NLSF to Q12 LPC
    /// synthesis coefficients (RFC 6716 Section 4.2.7.5).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5>
    pub(crate) fn decode_lsf(
        &mut self,
        decoder: &mut entropy::Decoder<'_>,
        frame: &mut Frame,
        channel: usize,
        silk_frame_duration: FrameDuration,
    ) -> Result<()> {
        let i1 = self.decode_lsf_stage1(decoder, frame.frame_type.signal_type)?;
        let (d_lpc, res_q10) = self.decode_lsf_stage2(decoder, i1)?;
        let mut nlsf_q15 = self.reconstruct_nlsf(d_lpc, &res_q10[..d_lpc], i1)?;
        self.stabilize_nlsf(&mut nlsf_q15[..d_lpc])?;

        let a1_q12 = Self::lsf_to_lpc(&nlsf_q15[..d_lpc])?;

        let ch_state = &mut self.state.channel_states[channel];
        let effective_w_q2 = if silk_frame_duration == FrameDuration::Ms20 {
            let raw_interp_idx =
                decoder.decode_symbol_with_icdf(&constant::ICDF_NORMALIZED_LSF_INTERPOLATION_INDEX)?;
            let w_q2 = if ch_state.first_frame_after_reset {
                4
            } else {
                raw_interp_idx
            };
            frame.lsf_interpolation_index = Some(w_q2);
            w_q2
        } else {
            frame.lsf_interpolation_index = None;
            4
        };

        if effective_w_q2 < 4 && frame.num_subframes == 4 {
            let mut nlsf0_q15 = [0i16; MAX_LPC_ORDER];
            for k in 0..d_lpc {
                let prev = ch_state.prev_nlsf_q15[k] as i32;
                let curr = nlsf_q15[k] as i32;
                nlsf0_q15[k] = (prev + (((curr - prev) * (effective_w_q2 as i32)) >> 2)) as i16;
            }
            let a0_q12 = Self::lsf_to_lpc(&nlsf0_q15[..d_lpc])?;

            frame.subframes[0].nlsf_q15 = nlsf0_q15;
            frame.subframes[0].lpc_coeffs_q12 = a0_q12;
            frame.subframes[1].nlsf_q15 = nlsf0_q15;
            frame.subframes[1].lpc_coeffs_q12 = a0_q12;
            frame.subframes[2].nlsf_q15 = nlsf_q15;
            frame.subframes[2].lpc_coeffs_q12 = a1_q12;
            frame.subframes[3].nlsf_q15 = nlsf_q15;
            frame.subframes[3].lpc_coeffs_q12 = a1_q12;
        } else {
            for subframe in frame.active_subframes_mut() {
                subframe.nlsf_q15 = nlsf_q15;
                subframe.lpc_coeffs_q12 = a1_q12;
            }
        }

        ch_state.prev_nlsf_q15 = nlsf_q15;
        return Ok(());
    }

    pub(crate) fn decode_lsf_stage1(
        &self,
        decoder: &mut entropy::Decoder<'_>,
        signal_type: SignalType,
    ) -> Result<u32> {
        let icdf: &[u32] = match (self.state.bandwidth, signal_type) {
            (Bandwidth::NarrowBand | Bandwidth::MediumBand, SignalType::Voiced) => {
                &constant::ICDF_NORMALIZED_LSF_STAGE_ONE_INDEX_NARROWBAND_OR_MEDIUMBAND_VOICED
            }
            (
                Bandwidth::NarrowBand | Bandwidth::MediumBand,
                SignalType::Inactive | SignalType::Unvoiced,
            ) => &constant::ICDF_NORMALIZED_LSF_STAGE_ONE_INDEX_NARROWBAND_OR_MEDIUMBAND_UNVOICED,
            (
                Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand,
                SignalType::Voiced,
            ) => &constant::ICDF_NORMALIZED_LSF_STAGE_ONE_INDEX_WIDEBAND_VOICED,
            (
                Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand,
                SignalType::Inactive | SignalType::Unvoiced,
            ) => &constant::ICDF_NORMALIZED_LSF_STAGE_ONE_INDEX_WIDEBAND_UNVOICED,
        };

        return decoder.decode_symbol_with_icdf(icdf);
    }

    pub(crate) fn decode_lsf_stage2(
        &self,
        decoder: &mut entropy::Decoder<'_>,
        i1: u32,
    ) -> Result<(usize, [i16; MAX_LPC_ORDER])> {
        let d_lpc = self.state.lpc_order;
        let i1_idx = i1 as usize;
        if i1_idx >= 32 {
            return Err(Error::InvalidLSFCoefficients.into());
        }

        let codebook_selector: &[u8] = match self.state.bandwidth {
            Bandwidth::NarrowBand | Bandwidth::MediumBand => {
                &constant::CODEBOOK_NORMALIZED_LSF_STAGE_TWO_INDEX_NARROWBAND_OR_MEDIUMBAND[i1_idx]
            }
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => {
                &constant::CODEBOOK_NORMALIZED_LSF_STAGE_TWO_INDEX_WIDEBAND[i1_idx]
            }
        };

        let mut stage2_indices = [0i16; MAX_LPC_ORDER];
        for (k, idx_out) in stage2_indices[..d_lpc].iter_mut().enumerate() {
            let codebook_idx = codebook_selector[k] as usize;
            let icdf = &constant::ICDF_NORMALIZED_LSF_STAGE_TWO_INDEX[codebook_idx];

            let symbol = decoder.decode_symbol_with_icdf(icdf)?;
            let mut idx = (symbol as i16) - 4;

            if idx == -4 || idx == 4 {
                let extension = decoder.decode_symbol_with_icdf(
                    &constant::ICDF_NORMALIZED_LSF_STAGE_TWO_INDEX_EXTENSION,
                )? as i16;
                idx += if idx < 0 { -extension } else { extension };
            }

            *idx_out = idx;
        }

        let qstep_q16 = match self.state.bandwidth {
            Bandwidth::NarrowBand | Bandwidth::MediumBand => {
                constant::LSF_STAGE_TWO_QUANTIZATION_STEP_Q16_NB_MB
            }
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => {
                constant::LSF_STAGE_TWO_QUANTIZATION_STEP_Q16_WB
            }
        };

        let mut res_q10 = [0i16; MAX_LPC_ORDER];
        for k in (0..d_lpc).rev() {
            let pred_q10 = if k + 1 < d_lpc {
                let pred_q8 = match self.state.bandwidth {
                    Bandwidth::NarrowBand | Bandwidth::MediumBand => {
                        let sel = constant::PREDICTION_WEIGHT_SELECTION_NB_MB[i1_idx][k] as usize;
                        constant::PREDICTION_WEIGHTS_NB_MB[sel][k] as i32
                    }
                    Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => {
                        let sel = constant::PREDICTION_WEIGHT_SELECTION_WB[i1_idx][k] as usize;
                        constant::PREDICTION_WEIGHTS_WB[sel][k] as i32
                    }
                };
                ((res_q10[k + 1] as i32) * pred_q8) >> 8
            } else {
                0
            };

            let i2_k = stage2_indices[k] as i32;
            let out_q10 = if i2_k > 0 {
                (i2_k << 10) - constant::LSF_STAGE_TWO_QUANTIZATION_LEVEL_OFFSET_Q10
            } else if i2_k < 0 {
                (i2_k << 10) + constant::LSF_STAGE_TWO_QUANTIZATION_LEVEL_OFFSET_Q10
            } else {
                0
            };

            res_q10[k] =
                (pred_q10 + ((((out_q10 as i64) * (qstep_q16 as i64)) >> 16) as i32)) as i16;
        }

        return Ok((d_lpc, res_q10));
    }

    pub(crate) fn reconstruct_nlsf(
        &self,
        d_lpc: usize,
        res_q10: &[i16],
        i1: u32,
    ) -> Result<[i16; MAX_LPC_ORDER]> {
        let i1_idx = i1 as usize;
        if i1_idx >= 32 || d_lpc > MAX_LPC_ORDER || res_q10.len() < d_lpc {
            return Err(Error::InvalidLSFCoefficients.into());
        }

        let cb1_q8: &[u8] = match self.state.bandwidth {
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => {
                &constant::CODEBOOK_NORMALIZED_LSF_STAGE_ONE_WIDEBAND[i1_idx]
            }
            Bandwidth::NarrowBand | Bandwidth::MediumBand => {
                &constant::CODEBOOK_NORMALIZED_LSF_STAGE_ONE_NARROWBAND_OR_MEDIUMBAND[i1_idx]
            }
        };

        let mut nlsf_q15 = [0i16; MAX_LPC_ORDER];
        for k in 0..d_lpc {
            let cb1_cur = cb1_q8[k] as i32;
            let cb1_prev = if k == 0 { 0 } else { cb1_q8[k - 1] as i32 };
            let cb1_next = if k + 1 == d_lpc {
                256
            } else {
                cb1_q8[k + 1] as i32
            };

            let diff_prev = (cb1_cur - cb1_prev).max(1);
            let diff_next = (cb1_next - cb1_cur).max(1);
            let w2_q18 = ((1024 / diff_prev) + (1024 / diff_next)) << 16;

            let i = 32 - (w2_q18 as u32).leading_zeros() as i32;
            let f = (w2_q18 >> (i - 8)) & 0x7F;
            let y = (if (i & 1) != 0 { 32768 } else { 46214 }) >> ((32 - i) >> 1);
            let w_q9 = (y + ((((213 * f * y) as i64) >> 16) as i32)).max(1);

            let nlsf = ((cb1_cur << 7) + (((res_q10[k] as i32) << 14) / w_q9)).clamp(0, 32767);
            nlsf_q15[k] = nlsf as i16;
        }

        return Ok(nlsf_q15);
    }

    pub(crate) fn stabilize_nlsf(&self, nlsf_q15: &mut [i16]) -> Result<()> {
        let d_lpc = nlsf_q15.len();
        let min_delta: &[i32] = match self.state.bandwidth {
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => {
                &constant::MINIMUM_SPACING_NORMALIZED_LSF_WB
            }
            Bandwidth::NarrowBand | Bandwidth::MediumBand => {
                &constant::MINIMUM_SPACING_NORMALIZED_LSF_NARROWBAND_MEDIUMBAND
            }
        };

        if d_lpc == 0 || min_delta.len() < d_lpc + 1 {
            return Err(Error::InvalidLSFCoefficients.into());
        }

        const MAX_STABILIZATION_ITERATIONS: usize = 20;
        let mut loops = 0;
        while loops < MAX_STABILIZATION_ITERATIONS {
            let mut min_diff = nlsf_q15[0] as i32 - min_delta[0];
            let mut min_diff_index = 0usize;

            for i in 1..d_lpc {
                let diff = nlsf_q15[i] as i32 - (nlsf_q15[i - 1] as i32 + min_delta[i]);
                if diff < min_diff {
                    min_diff = diff;
                    min_diff_index = i;
                }
            }

            let upper_diff = (1 << 15) - (nlsf_q15[d_lpc - 1] as i32 + min_delta[d_lpc]);
            if upper_diff < min_diff {
                min_diff = upper_diff;
                min_diff_index = d_lpc;
            }

            if min_diff >= 0 {
                return Ok(());
            }

            if min_diff_index == 0 {
                nlsf_q15[0] = min_delta[0] as i16;
            } else if min_diff_index == d_lpc {
                nlsf_q15[d_lpc - 1] = ((1 << 15) - min_delta[d_lpc]) as i16;
            } else {
                let min_center_q15: i32 = min_delta[0..min_diff_index].iter().sum::<i32>()
                    + (min_delta[min_diff_index] >> 1);
                let max_center_q15: i32 = (1 << 15)
                    - min_delta[(min_diff_index + 1)..=d_lpc].iter().sum::<i32>()
                    - (min_delta[min_diff_index] - (min_delta[min_diff_index] >> 1));

                let raw_center =
                    (nlsf_q15[min_diff_index - 1] as i32 + nlsf_q15[min_diff_index] as i32 + 1)
                        >> 1;
                let center_freq_q15 = raw_center.clamp(min_center_q15, max_center_q15);

                nlsf_q15[min_diff_index - 1] =
                    (center_freq_q15 - (min_delta[min_diff_index] >> 1)) as i16;
                nlsf_q15[min_diff_index] =
                    (nlsf_q15[min_diff_index - 1] as i32 + min_delta[min_diff_index]) as i16;
            }

            loops += 1;
        }

        nlsf_q15.sort_unstable();
        for i in 0..d_lpc {
            let prev = if i == 0 { 0 } else { nlsf_q15[i - 1] as i32 };
            nlsf_q15[i] = (nlsf_q15[i] as i32).max(prev + min_delta[i]) as i16;
        }
        for i in (0..d_lpc).rev() {
            let next = if i + 1 == d_lpc {
                1 << 15
            } else {
                nlsf_q15[i + 1] as i32
            };
            nlsf_q15[i] = (nlsf_q15[i] as i32).min(next - min_delta[i + 1]) as i16;
        }

        return Ok(());
    }

    /// Converts stabilized Q15 Normalized Line Spectral Frequencies (`nlsf_q15`)
    /// into Q12 monic LPC synthesis filter coefficients (`a_q12`)
    /// (RFC 6716 Section 4.2.7.5.6, matching `silk_NLSF2A` in `silk/NLSF2A.c`).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.6>
    pub(crate) fn lsf_to_lpc(nlsf_q15: &[i16]) -> Result<[i16; MAX_LPC_ORDER]> {
        const QA: u32 = 16;
        const MAX_LPC_STABILIZE_ITERATIONS: usize = 16;

        let d = nlsf_q15.len();
        let ordering: &[u8] = match d {
            10 => &constant::LSF_ORDERING_POLYNOMIAL_EVALUATION_NB_MB,
            16 => &constant::LSF_ORDERING_POLYNOMIAL_EVALUATION_WB,
            _ => return Err(Error::InvalidLSFCoefficients.into()),
        };

        let mut cos_lsf_qa = [0i32; MAX_LPC_ORDER];
        for k in 0..d {
            let nlsf = (nlsf_q15[k] as i32).clamp(0, 32767);
            let f_int = (nlsf >> 8) as usize;
            let f_frac = nlsf - ((f_int as i32) << 8);

            let cos_val = constant::Q12_COSINE_TABLE_FOR_LSF_CONVERSION[f_int];
            let delta = constant::Q12_COSINE_TABLE_FOR_LSF_CONVERSION[f_int + 1] - cos_val;

            cos_lsf_qa[ordering[k] as usize] =
                rshift_round((cos_val << 8) + delta * f_frac, 19 - QA);
        }

        let dd = d >> 1;
        let p = Self::find_lpc_polynomial(&cos_lsf_qa, 0, dd);
        let q = Self::find_lpc_polynomial(&cos_lsf_qa, 1, dd);

        let mut a32_qa1 = [0i32; MAX_LPC_ORDER];
        for k in 0..dd {
            let p_tmp = p[k + 1].wrapping_add(p[k]);
            let q_tmp = q[k + 1].wrapping_sub(q[k]);

            a32_qa1[k] = (-q_tmp).wrapping_sub(p_tmp);
            a32_qa1[d - k - 1] = q_tmp.wrapping_sub(p_tmp);
        }

        let mut a_q12 = [0i16; MAX_LPC_ORDER];
        Self::lpc_fit(&mut a_q12[..d], &mut a32_qa1[..d], 12, (QA + 1) as i32);

        for i in 0..MAX_LPC_STABILIZE_ITERATIONS {
            if Self::lpc_inverse_pred_gain(&a_q12[..d]) != 0 {
                break;
            }
            Self::bwexpander_32(&mut a32_qa1[..d], 65_536 - (2 << i));
            for k in 0..d {
                a_q12[k] = rshift_round(a32_qa1[k], QA + 1 - 12) as i16;
            }
        }

        return Ok(a_q12);
    }

    /// Evaluates the even or odd Chebyshev polynomial from interleaved `2 * cos(LSF)` roots
    /// (`silk_NLSF2A_find_poly` in `silk/NLSF2A.c`).
    fn find_lpc_polynomial(
        cos_lsf_qa: &[i32; MAX_LPC_ORDER],
        start_offset: usize,
        dd: usize,
    ) -> [i32; MAX_LPC_ORDER / 2 + 1] {
        const QA: u32 = 16;

        let mut out = [0i32; MAX_LPC_ORDER / 2 + 1];
        out[0] = 1 << QA;
        out[1] = -cos_lsf_qa[start_offset];

        for k in 1..dd {
            let ftmp = cos_lsf_qa[start_offset + 2 * k] as i64;
            out[k + 1] = (out[k - 1] << 1)
                .wrapping_sub(rshift_round64(ftmp * (out[k] as i64), QA) as i32);
            for n in (2..=k).rev() {
                out[n] = out[n]
                    .wrapping_add(out[n - 2])
                    .wrapping_sub(rshift_round64(ftmp * (out[n - 1] as i64), QA) as i32);
            }
            out[1] = out[1].wrapping_sub(ftmp as i32);
        }

        return out;
    }

    /// Fits QIN 32-bit LPC coefficients into 16-bit QOUT coefficients without wrap-around
    /// by applying progressive chirp bandwidth expansion (`silk_LPC_fit` in `silk/LPC_fit.c`).
    pub(crate) fn lpc_fit(a_qout: &mut [i16], a_qin: &mut [i32], q_out: i32, q_in: i32) {
        let d = a_qin.len();
        let shift = (q_in - q_out) as u32;
        let mut iter = 0usize;

        while iter < 10 {
            let mut maxabs = 0i32;
            let mut idx = 0usize;
            for (k, &val) in a_qin.iter().enumerate() {
                let absval = val.saturating_abs();
                if absval > maxabs {
                    maxabs = absval;
                    idx = k;
                }
            }

            maxabs = rshift_round(maxabs, shift);
            if maxabs > i16::MAX as i32 {
                maxabs = maxabs.min(163_838);
                // SILK_FIX_CONST(0.999, 16) = 65470
                let numer = (maxabs - (i16::MAX as i32)) << 14;
                let denom = (maxabs * ((idx as i32) + 1)) >> 2;
                let chirp_q16 = 65_470 - (numer / denom);
                Self::bwexpander_32(a_qin, chirp_q16);
                iter += 1;
            } else {
                break;
            }
        }

        if iter == 10 {
            for k in 0..d {
                a_qout[k] = sat16(rshift_round(a_qin[k], shift));
                a_qin[k] = (a_qout[k] as i32) << shift;
            }
        } else {
            for k in 0..d {
                a_qout[k] = rshift_round(a_qin[k], shift) as i16;
            }
        }
    }

    /// Applies a Q16 chirp bandwidth expansion factor across `ar` (`silk_bwexpander_32`).
    pub(crate) fn bwexpander_32(ar: &mut [i32], mut chirp_q16: i32) {
        let d = ar.len();
        if d == 0 {
            return;
        }

        let chirp_minus_one_q16 = chirp_q16 - 65_536;
        for coeff in ar[..d - 1].iter_mut() {
            *coeff = mul_q16(chirp_q16, *coeff);
            chirp_q16 += rshift_round(chirp_q16 * chirp_minus_one_q16, 16);
        }
        ar[d - 1] = mul_q16(chirp_q16, ar[d - 1]);
    }

    /// Computes the inverse LPC prediction gain in Q30 and tests filter stability
    /// (`silk_LPC_inverse_pred_gain_c` in `silk/LPC_inv_pred_gain.c`).
    ///
    /// Returns `0` if the filter is unstable or exceeds `MAX_PREDICTION_POWER_GAIN`.
    pub(crate) fn lpc_inverse_pred_gain(a_q12: &[i16]) -> i32 {
        const QA: u32 = 24;
        // SILK_FIX_CONST(0.99975, 24)
        const A_LIMIT: i32 = 16_773_022;
        // SILK_FIX_CONST(1.0f / 1e4f, 30)
        const MIN_INV_GAIN_Q30: i32 = 107_374;

        let order = a_q12.len();
        if order == 0 || order > MAX_LPC_ORDER {
            return 0;
        }

        let mut a_qa = [0i32; MAX_LPC_ORDER];
        let mut dc_resp = 0i32;
        for (k, &coeff) in a_q12.iter().enumerate() {
            dc_resp += coeff as i32;
            a_qa[k] = (coeff as i32) << (QA - 12);
        }

        if dc_resp >= 4096 {
            return 0;
        }

        let mut inv_gain_q30 = 1i32 << 30;
        for k in (1..order).rev() {
            if a_qa[k] > A_LIMIT || a_qa[k] < -A_LIMIT {
                return 0;
            }

            let rc_q31 = -(a_qa[k] << (31 - QA));
            let rc_mult1_q30 = (1i32 << 30) - mul_high_i32(rc_q31, rc_q31);

            inv_gain_q30 = mul_high_i32(inv_gain_q30, rc_mult1_q30) << 2;
            if inv_gain_q30 < MIN_INV_GAIN_Q30 {
                return 0;
            }

            let mult2_q = 32 - (rc_mult1_q30.unsigned_abs().leading_zeros() as i32);
            let rc_mult2 = inverse32_var_q(rc_mult1_q30, mult2_q + 30);

            for n in 0..((k + 1) >> 1) {
                let tmp1 = a_qa[n];
                let tmp2 = a_qa[k - n - 1];

                let prod2 = rshift_round64((tmp2 as i64) * (rc_q31 as i64), 31) as i32;
                let diff1 = sub_sat32(tmp1, prod2) as i64;
                let tmp64_1 = rshift_round64(diff1 * (rc_mult2 as i64), mult2_q as u32);
                if tmp64_1 > i32::MAX as i64 || tmp64_1 < i32::MIN as i64 {
                    return 0;
                }
                a_qa[n] = tmp64_1 as i32;

                let prod1 = rshift_round64((tmp1 as i64) * (rc_q31 as i64), 31) as i32;
                let diff2 = sub_sat32(tmp2, prod1) as i64;
                let tmp64_2 = rshift_round64(diff2 * (rc_mult2 as i64), mult2_q as u32);
                if tmp64_2 > i32::MAX as i64 || tmp64_2 < i32::MIN as i64 {
                    return 0;
                }
                a_qa[k - n - 1] = tmp64_2 as i32;
            }
        }

        if a_qa[0] > A_LIMIT || a_qa[0] < -A_LIMIT {
            return 0;
        }

        let rc_q31 = -(a_qa[0] << (31 - QA));
        let rc_mult1_q30 = (1i32 << 30) - mul_high_i32(rc_q31, rc_q31);
        inv_gain_q30 = mul_high_i32(inv_gain_q30, rc_mult1_q30) << 2;
        if inv_gain_q30 < MIN_INV_GAIN_Q30 {
            return 0;
        }

        return inv_gain_q30;
    }
}
