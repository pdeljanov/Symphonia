//! SILK linear prediction kernels
//! ([RFC 6716 Section 4.2.7.5.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.6>)
//! to [Section 4.2.7.5.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.8>),
//! [Section 4.2.7.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>)).
//!
//! The fixed-point kernels shared by frame reconstruction and by packet loss concealment
//! ([Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)): NLSF to LPC
//! conversion, coefficient limiting by bandwidth expansion, the prediction gain stability test,
//! and the LTP and LPC prediction and synthesis loops.

use symphonia_core::errors::Result;

use crate::fixed::{
    mul_add_q16_i16, mul_high_i32, mul_q16, reciprocal_q, round_shift, round_shift_i64,
    saturate_i16, shl_saturating,
};
use crate::silk::MAX_LPC_ORDER;
use crate::silk::constant;
use crate::silk::error::Error;

/// Q10 LPC prediction to Q14 output left shift (RFC 6716 Section 4.2.7.9.2).
const LPC_PRED_Q10_TO_Q14_SHIFT: u32 = 4;
/// Q16 gain times Q14 sample to Q8 PCM right shift (RFC 6716 Section 4.2.7.9.2).
const LPC_OUT_Q14_GAIN_TO_PCM_SHIFT: u32 = 8;
/// Rounding bias of the Q13 LTP prediction accumulator (RFC 6716 Section 4.2.7.9.1).
const LTP_PRED_ROUNDING_BIAS_Q13: i32 = 2;
/// Fractional bits of the NLSF to LPC conversion polynomials (RFC 6716 Section 4.2.7.5.6).
const QA: u32 = 16;
/// Q15 NLSF to cosine table index right shift (RFC 6716 Section 4.2.7.5.6).
const NLSF_TABLE_INDEX_SHIFT: i32 = 8;
/// Bits dropped when the interpolated Q20 cosine becomes a QA value (RFC 6716 Section 4.2.7.5.6).
const COSINE_Q20_TO_QA_SHIFT: u32 = 19 - QA;
/// Bandwidth expansion rounds of the stability loop after conversion (RFC 6716 Section 4.2.7.5.8).
const MAX_LPC_STABILIZE_ITERATIONS: usize = 16;
/// Unit chirp factor (`1.0`) in Q16 (RFC 6716 Section 4.2.7.5.7).
const UNIT_CHIRP_Q16: i32 = 1 << 16;
/// Chirp base `0.999` in Q16 of the coefficient fitting loop (RFC 6716 Section 4.2.7.5.7).
const FIT_CHIRP_BASE_Q16: i32 = 65_470;
/// Rounds of chirp expansion tried before saturating the coefficients (RFC 6716 Section 4.2.7.5.7).
const MAX_FIT_ITERATIONS: usize = 10;
/// Largest coefficient magnitude the chirp step is computed from (RFC 6716 Section 4.2.7.5.7).
const MAX_FIT_MAGNITUDE: i32 = 163_838;
/// Fractional bits of the reflection coefficient test (RFC 6716 Section 4.2.7.5.8).
const INV_GAIN_QA: u32 = 24;
/// Left shift turning a Q12 LPC coefficient into the Q24 of the stability test (RFC 6716 Section 4.2.7.5.8).
const Q12_TO_INV_GAIN_QA_SHIFT: u32 = INV_GAIN_QA - 12;
/// Left shift turning a Q24 tap into the Q31 reflection coefficient (RFC 6716 Section 4.2.7.5.8).
const INV_GAIN_QA_TO_Q31_SHIFT: u32 = 31 - INV_GAIN_QA;
/// Left shift restoring Q30 after the high 32 bits of a Q30 by Q30 product, which are Q28
/// (RFC 6716 Section 4.2.7.5.8).
const Q28_TO_Q30_SHIFT: u32 = 2;
/// Reflection coefficient magnitude limit `0.99975` in Q24 (RFC 6716 Section 4.2.7.5.8).
const REFLECTION_LIMIT_Q24: i32 = 16_773_022;
/// Minimum inverse prediction gain `1 / 10000` in Q30 (RFC 6716 Section 4.2.7.5.8).
const MIN_INV_GAIN_Q30: i32 = 107_374;
/// Unit inverse prediction gain (`1.0`) in Q30 (RFC 6716 Section 4.2.7.5.8).
const UNIT_INV_GAIN_Q30: i32 = 1 << 30;
/// Largest DC response of a stable filter in Q12 (RFC 6716 Section 4.2.7.5.8).
const MAX_DC_RESPONSE_Q12: i32 = 4096;

/// Computes the Q10 short-term LPC prediction for the sample following `history_q14`
/// from Q12 coefficients `a_q12` (RFC 6716 Section 4.2.7.9.2).
///
/// `history_q14` must hold at least `a_q12.len()` past Q14 samples, newest last.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9.2>
#[inline]
pub(crate) fn lpc_predict_q10(history_q14: &[i32], a_q12: &[i16]) -> i32 {
    let newest = history_q14.len() - 1;
    return a_q12.iter().enumerate().fold((a_q12.len() >> 1) as i32, |acc, (j, &a_tap)| {
        mul_add_q16_i16(acc, history_q14[newest - j], a_tap)
    });
}

/// Computes the Q13 long-term (pitch) prediction for the sample following `history_q15`
/// from the 5 Q14 LTP taps `b_q14` (RFC 6716 Section 4.2.7.9.1).
///
/// `history_q15` must hold at least `b_q14.len()` past samples, newest last, already
/// positioned so that its last element is `lag - 2` samples behind the predicted sample.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9.1>
#[inline]
pub(crate) fn ltp_predict_q13(history_q15: &[i32], b_q14: &[i16]) -> i32 {
    let newest = history_q15.len() - 1;
    return b_q14.iter().enumerate().fold(LTP_PRED_ROUNDING_BIAS_Q13, |acc, (j, &b_tap)| {
        mul_add_q16_i16(acc, history_q15[newest - j], b_tap)
    });
}

/// Synthesizes sample `i` of the all-pole LPC filter in place (RFC 6716 Section 4.2.7.9.2).
///
/// `state_q14[..MAX_LPC_ORDER]` holds the filter history and `state_q14[MAX_LPC_ORDER + i]`
/// the Q14 excitation of the sample, which is replaced by the Q14 synthesized value. The
/// returned PCM sample is the synthesized value scaled by the Q10 subframe gain and saturated
/// to 16 bits.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9.2>
#[inline]
pub(crate) fn lpc_synthesize_sample_q14(
    state_q14: &mut [i32],
    i: usize,
    a_q12: &[i16],
    gain_q10: i32,
) -> i16 {
    let lpc_pred_q10 = lpc_predict_q10(&state_q14[..MAX_LPC_ORDER + i], a_q12);
    let lpc_out_q14 = state_q14[MAX_LPC_ORDER + i]
        .saturating_add(shl_saturating(lpc_pred_q10, LPC_PRED_Q10_TO_Q14_SHIFT));
    state_q14[MAX_LPC_ORDER + i] = lpc_out_q14;
    return saturate_i16(round_shift(
        mul_q16(lpc_out_q14, gain_q10),
        LPC_OUT_Q14_GAIN_TO_PCM_SHIFT,
    ));
}

/// Runs the all-pole LPC synthesis filter in place over `state_q14`
/// (RFC 6716 Section 4.2.7.9.2).
///
/// `state_q14[..MAX_LPC_ORDER]` holds the filter history and
/// `state_q14[MAX_LPC_ORDER..MAX_LPC_ORDER + out_pcm.len()]` holds the Q14 excitation,
/// which is replaced by the Q14 synthesized signal; `out_pcm` receives the signal scaled
/// by the Q10 subframe gain and saturated to 16 bits.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9.2>
pub(crate) fn lpc_synthesize_q14(
    state_q14: &mut [i32],
    a_q12: &[i16],
    gain_q10: i32,
    out_pcm: &mut [i16],
) {
    out_pcm.iter_mut().enumerate().for_each(|(i, out_sample)| {
        *out_sample = lpc_synthesize_sample_q14(state_q14, i, a_q12, gain_q10);
    });
    return;
}

/// Converts stabilized Q15 Normalized Line Spectral Frequencies (`nlsf_q15`)
/// into Q12 monic LPC synthesis filter coefficients (`a_q12`)
/// (RFC 6716 Section 4.2.7.5.6).
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.6>
pub(crate) fn nlsf_to_lpc_q12(nlsf_q15: &[i16]) -> Result<[i16; MAX_LPC_ORDER]> {
    let d = nlsf_q15.len();
    let ordering: &[u8] = match d {
        10 => &constant::LSF_ORDERING_POLYNOMIAL_EVALUATION_NB_MB,
        16 => &constant::LSF_ORDERING_POLYNOMIAL_EVALUATION_WB,
        _ => return Err(Error::InvalidLSFCoefficients.into()),
    };

    let mut cos_lsf_qa = [0i32; MAX_LPC_ORDER];
    for (&nlsf_q15, &position) in nlsf_q15.iter().zip(ordering.iter()) {
        cos_lsf_qa[position as usize] = cosine_of_nlsf_qa(nlsf_q15);
    }

    let dd = d >> 1;
    let p = find_lpc_polynomial(&cos_lsf_qa, 0, dd);
    let q = find_lpc_polynomial(&cos_lsf_qa, 1, dd);

    let mut a32_qa1 = [0i32; MAX_LPC_ORDER];
    for i in 0..dd {
        let p_tmp = p[i + 1].wrapping_add(p[i]);
        let q_tmp = q[i + 1].wrapping_sub(q[i]);

        a32_qa1[i] = (-q_tmp).wrapping_sub(p_tmp);
        a32_qa1[d - i - 1] = q_tmp.wrapping_sub(p_tmp);
    }

    let mut a_q12 = [0i16; MAX_LPC_ORDER];
    fit_lpc_to_i16(&mut a_q12[..d], &mut a32_qa1[..d], 12, (QA + 1) as i32);

    for i in 0..MAX_LPC_STABILIZE_ITERATIONS {
        if inverse_prediction_gain_q30(&a_q12[..d]) != 0 {
            break;
        }
        bandwidth_expand_q16(&mut a32_qa1[..d], UNIT_CHIRP_Q16 - (2 << i));
        for (a_tap, &a32) in a_q12[..d].iter_mut().zip(a32_qa1[..d].iter()) {
            *a_tap = round_shift(a32, QA + 1 - 12) as i16;
        }
    }

    return Ok(a_q12);
}

/// Interpolates the Q12 cosine table at a Q15 NLSF and scales the result to QA
/// (RFC 6716 Section 4.2.7.5.6).
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.6>
#[inline]
fn cosine_of_nlsf_qa(nlsf_q15: i16) -> i32 {
    let nlsf = (nlsf_q15 as i32).clamp(0, i16::MAX as i32);
    let f_int = (nlsf >> NLSF_TABLE_INDEX_SHIFT) as usize;
    let f_frac = nlsf - ((f_int as i32) << NLSF_TABLE_INDEX_SHIFT);

    let cos_val = constant::Q12_COSINE_TABLE_FOR_LSF_CONVERSION[f_int];
    let delta = constant::Q12_COSINE_TABLE_FOR_LSF_CONVERSION[f_int + 1] - cos_val;

    return round_shift(
        (cos_val << NLSF_TABLE_INDEX_SHIFT) + delta * f_frac,
        COSINE_Q20_TO_QA_SHIFT,
    );
}

/// Evaluates the even or odd Chebyshev polynomial from interleaved `2 * cos(LSF)` roots,
/// expanding one half of the P and Q root pairs (RFC 6716 Section 4.2.7.5.6).
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.6>
fn find_lpc_polynomial(
    cos_lsf_qa: &[i32; MAX_LPC_ORDER],
    start_offset: usize,
    dd: usize,
) -> [i32; MAX_LPC_ORDER / 2 + 1] {
    let mut out = [0i32; MAX_LPC_ORDER / 2 + 1];
    out[0] = 1 << QA;
    out[1] = -cos_lsf_qa[start_offset];

    for i in 1..dd {
        let ftmp = cos_lsf_qa[start_offset + 2 * i] as i64;
        out[i + 1] =
            (out[i - 1] << 1).wrapping_sub(round_shift_i64(ftmp * (out[i] as i64), QA) as i32);
        for j in (2..=i).rev() {
            out[j] = out[j]
                .wrapping_add(out[j - 2])
                .wrapping_sub(round_shift_i64(ftmp * (out[j - 1] as i64), QA) as i32);
        }
        out[1] = out[1].wrapping_sub(ftmp as i32);
    }

    return out;
}

/// Fits QIN 32-bit LPC coefficients into 16-bit QOUT coefficients without wrap-around
/// by applying progressive chirp bandwidth expansion until every coefficient fits
/// (RFC 6716 Section 4.2.7.5.7).
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.7>
pub(crate) fn fit_lpc_to_i16(a_qout: &mut [i16], a_qin: &mut [i32], q_out: i32, q_in: i32) {
    let shift = (q_in - q_out) as u32;
    if expand_until_fits_i16(a_qin, shift) {
        a_qout.iter_mut().zip(a_qin.iter()).for_each(|(out, &a_in)| {
            *out = round_shift(a_in, shift) as i16;
        });
        return;
    }

    a_qout.iter_mut().zip(a_qin.iter_mut()).for_each(|(out, a_in)| {
        *out = saturate_i16(round_shift(*a_in, shift));
        *a_in = (*out as i32) << shift;
    });
}

/// Expands the bandwidth of `a_qin` until every coefficient shifted right by `shift` fits in
/// 16 bits, giving up after `MAX_FIT_ITERATIONS` rounds (RFC 6716 Section 4.2.7.5.7).
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.7>
fn expand_until_fits_i16(a_qin: &mut [i32], shift: u32) -> bool {
    for _ in 0..MAX_FIT_ITERATIONS {
        let (maxabs, max_i) =
            a_qin.iter().enumerate().fold((0i32, 0usize), |(best_abs, best_i), (i, &val)| {
                let absval = val.saturating_abs();
                if absval > best_abs { (absval, i) } else { (best_abs, best_i) }
            });

        let maxabs = round_shift(maxabs, shift);
        if maxabs <= i16::MAX as i32 {
            return true;
        }

        let maxabs = maxabs.min(MAX_FIT_MAGNITUDE);
        let numer = (maxabs - (i16::MAX as i32)) << 14;
        let denom = (maxabs * ((max_i as i32) + 1)) >> 2;
        bandwidth_expand_q16(a_qin, FIT_CHIRP_BASE_Q16 - (numer / denom));
    }
    return false;
}

/// Applies a Q16 chirp bandwidth expansion factor across the 32-bit coefficients `ar`
/// (RFC 6716 Section 4.2.7.5.7).
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.7>
pub(crate) fn bandwidth_expand_q16(ar: &mut [i32], mut chirp_q16: i32) {
    let Some((last_coeff, head_coeffs)) = ar.split_last_mut()
    else {
        return;
    };

    let chirp_minus_one_q16 = chirp_q16 - UNIT_CHIRP_Q16;
    for coeff in head_coeffs.iter_mut() {
        *coeff = mul_q16(chirp_q16, *coeff);
        chirp_q16 += round_shift(chirp_q16 * chirp_minus_one_q16, 16);
    }
    *last_coeff = mul_q16(chirp_q16, *last_coeff);
}

/// Computes the inverse LPC prediction gain in Q30 and tests filter stability
/// (RFC 6716 Section 4.2.7.5.8, with the saturating subtraction and 64-bit
/// intermediate of RFC 8251 Section 6).
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.8>
///
/// <https://datatracker.ietf.org/doc/html/rfc8251#section-6>
///
/// Returns `0` if the filter is unstable or exceeds `MAX_PREDICTION_POWER_GAIN`.
pub(crate) fn inverse_prediction_gain_q30(a_q12: &[i16]) -> i32 {
    let order = a_q12.len();
    if order == 0 || order > MAX_LPC_ORDER {
        return 0;
    }

    let dc_resp = a_q12.iter().fold(0i32, |acc, &coeff| acc + coeff as i32);
    if dc_resp >= MAX_DC_RESPONSE_Q12 {
        return 0;
    }

    let mut a_qa = [0i32; MAX_LPC_ORDER];
    for (a_tap, &coeff) in a_qa.iter_mut().zip(a_q12.iter()) {
        *a_tap = (coeff as i32) << Q12_TO_INV_GAIN_QA_SHIFT;
    }

    let mut inv_gain_q30 = UNIT_INV_GAIN_Q30;
    for i in (1..order).rev() {
        let Some(step) = ReflectionStep::new(a_qa[i], inv_gain_q30)
        else {
            return 0;
        };
        inv_gain_q30 = step.inv_gain_q30;
        if !step.step_down(&mut a_qa[..i]) {
            return 0;
        }
    }

    return ReflectionStep::new(a_qa[0], inv_gain_q30).map_or(0, |step| step.inv_gain_q30);
}

/// One step of the backward Levinson recursion of the stability test: the reflection
/// coefficient of the highest remaining filter tap and the inverse gain after removing it
/// (RFC 6716 Section 4.2.7.5.8).
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.8>
#[derive(Debug, Clone, Copy)]
struct ReflectionStep {
    /// Reflection coefficient in Q31.
    rc_q31: i32,
    /// `1 - rc^2` in Q30.
    rc_mult1_q30: i32,
    /// Inverse prediction gain after this step in Q30.
    inv_gain_q30: i32,
}

impl ReflectionStep {
    /// Derives the step from the highest tap `a_qa` of the remaining filter and the inverse gain
    /// so far; `None` when the reflection coefficient or the gain leaves the stable range.
    fn new(a_qa: i32, inv_gain_q30: i32) -> Option<Self> {
        if !(-REFLECTION_LIMIT_Q24..=REFLECTION_LIMIT_Q24).contains(&a_qa) {
            return None;
        }

        let rc_q31 = -(a_qa << INV_GAIN_QA_TO_Q31_SHIFT);
        let rc_mult1_q30 = UNIT_INV_GAIN_Q30 - mul_high_i32(rc_q31, rc_q31);
        let inv_gain_q30 = mul_high_i32(inv_gain_q30, rc_mult1_q30) << Q28_TO_Q30_SHIFT;
        if inv_gain_q30 < MIN_INV_GAIN_Q30 {
            return None;
        }
        return Some(Self { rc_q31, rc_mult1_q30, inv_gain_q30 });
    }

    /// Removes the reflection coefficient from the lower-order taps `a_qa` in place; `false`
    /// when a tap overflows 32 bits (RFC 8251 Section 6).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc8251#section-6>
    fn step_down(&self, a_qa: &mut [i32]) -> bool {
        let i = a_qa.len();
        let mult2_q = 32 - (self.rc_mult1_q30.unsigned_abs().leading_zeros() as i32);
        let rc_mult2 = reciprocal_q(self.rc_mult1_q30, mult2_q + 30);

        for j in 0..((i + 1) >> 1) {
            let tmp1 = a_qa[j];
            let tmp2 = a_qa[i - j - 1];

            let Some(low) = self.remove_reflection(tmp1, tmp2, rc_mult2, mult2_q)
            else {
                return false;
            };
            let Some(high) = self.remove_reflection(tmp2, tmp1, rc_mult2, mult2_q)
            else {
                return false;
            };
            a_qa[j] = low;
            a_qa[i - j - 1] = high;
        }
        return true;
    }

    /// `(tap - mirror * rc) / (1 - rc^2)` with saturating subtraction and a 64-bit product;
    /// `None` when the result overflows 32 bits (RFC 8251 Section 6).
    #[inline]
    fn remove_reflection(&self, tap: i32, mirror: i32, rc_mult2: i32, mult2_q: i32) -> Option<i32> {
        let prod = round_shift_i64((mirror as i64) * (self.rc_q31 as i64), 31) as i32;
        let diff = i64::from(tap.saturating_sub(prod));
        let scaled = round_shift_i64(diff * (rc_mult2 as i64), mult2_q as u32);
        return i32::try_from(scaled).ok();
    }
}

#[cfg(test)]
mod tests {
    // Fixtures: packets and expected values recorded from libopus 1.6.1. The behaviour they pin is
    // normative (RFC 6716 Section 4.2.7.5.6) and is verified against the RFC 8251 test vectors by
    // `tests/rfc6716_conformance.rs`.
    use super::*;

    macro_rules! assert_nlsf_to_lpc {
        ($nlsf:expr, $expected_a_q12:expr, $expected_inv_gain:expr) => {{
            let order = $expected_a_q12.len();
            let a_q12 = nlsf_to_lpc_q12(&$nlsf).unwrap();
            assert_eq!(&a_q12[..order], &$expected_a_q12);
            assert_eq!(inverse_prediction_gain_q30(&a_q12[..order]), $expected_inv_gain);
        }};
    }

    #[test]
    fn nlsf_to_lpc_nb_wb_and_resonant_stabilization_match_recorded_values() {
        // NLSFs and the LPC coefficients and inverse prediction gains they convert to:
        const NLSF_Q15_NB: [i16; 10] =
            [1547, 4508, 7814, 10871, 14261, 17641, 21363, 25184, 26336, 26339];
        const EXPECTED_A_Q12_NB: [i16; 10] =
            [1545, 716, 1663, -2049, 2207, -1542, 1286, -805, 595, -332];

        const NLSF_Q15_WB: [i16; 16] = [
            674, 1292, 2214, 4949, 6390, 9088, 11264, 13184, 15232, 17536, 19712, 21888, 24192,
            26240, 28416, 30336,
        ];
        const EXPECTED_A_Q12_WB: [i16; 16] =
            [7772, -3675, 43, -45, 264, -676, 460, -283, 541, -219, 61, -369, 301, -381, 139, 146];

        const NLSF_Q15_RESONANT: [i16; 10] = [100, 120, 140, 160, 180, 200, 220, 240, 260, 280];
        const EXPECTED_A_Q12_RESONANT: [i16; 10] =
            [16591, -30244, 32672, -23164, 11261, -3802, 880, -134, 12, -1];

        assert_nlsf_to_lpc!(NLSF_Q15_NB, EXPECTED_A_Q12_NB, 3_238_224);
        assert_nlsf_to_lpc!(NLSF_Q15_WB, EXPECTED_A_Q12_WB, 864_548);
        assert_nlsf_to_lpc!(NLSF_Q15_RESONANT, EXPECTED_A_Q12_RESONANT, 386_396);
    }

    /// NLSF to LPC conversion supports the orders `10` and `16` only
    /// ([RFC 6716 Section 4.2.7.5.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.6>)).
    #[test]
    fn nlsf_to_lpc_rejects_an_unsupported_order() {
        assert!(Error::InvalidLSFCoefficients.matches(&nlsf_to_lpc_q12(&[0; 12])));
    }

    /// A Q12 filter whose backward Levinson step overflows 32 bits is rejected as unstable
    /// ([RFC 8251 Section 6](<https://datatracker.ietf.org/doc/html/rfc8251#section-6>)).
    #[test]
    fn inverse_prediction_gain_rejects_32_bit_step_overflow() {
        const OVERFLOWING_A_Q12: [i16; 10] = [-275, 2794, -4094, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(inverse_prediction_gain_q30(&OVERFLOWING_A_Q12), 0);
    }
}
