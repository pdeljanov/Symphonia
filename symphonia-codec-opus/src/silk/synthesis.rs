//! SILK frame reconstruction, stereo unmixing and output conversion
//! ([RFC 6716 Section 4.2.7.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>),
//! [Section 4.2.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.8>),
//! [Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
//!
//! Synthesizes one channel frame from its Q14 excitation through LTP rewhitening, LTP synthesis
//! and LPC synthesis, unmixes the adaptive mid-side representation into left and right, and
//! resamples the result into `f32` output.

use symphonia_core::errors::Result;

use crate::fixed::{
    div_q, mul_add_q16_i16, mul_q16, mul_q16_i16, reciprocal_q, round_shift, saturate_i16,
};
use crate::silk::error::Error;
use crate::silk::lpc::{lpc_synthesize_q14, ltp_predict_q13};
use crate::silk::resampler::ResamplerState;
use crate::silk::{
    ChannelState, Frame, LTP_ORDER, MAX_FRAME_SAMPLES, MAX_LPC_ORDER, MAX_LTP_MEM_SAMPLES,
    MAX_SUBFRAME_SAMPLES, SignalType, StereoState, StereoWeights,
};

/// Maximum number of output samples per 20-ms SILK frame at 48 kHz (`20 ms * 48 kHz = 960`).
const MAX_RESAMPLED_FRAME_SAMPLES: usize = 960;

/// Scale of 16-bit PCM to `f32` in `[-1.0, 1.0)`.
const INT16_TO_F32_SCALE: f32 = 1.0 / 32768.0;

/// Unit gain (`1.0`) in Q16 (RFC 6716 Section 4.2.7.9.2).
const UNIT_GAIN_Q16: i32 = 1 << 16;

/// Right shift turning a Q16 subframe gain into the Q10 synthesis gain (RFC 6716 Section 4.2.7.9.2).
const GAIN_Q16_TO_Q10_SHIFT: u32 = 6;

/// Left shift turning a Q7 LTP tap into Q14 (RFC 6716 Section 4.2.7.9.1).
const LTP_COEFF_Q7_TO_Q14_SHIFT: u32 = 7;

/// Precision argument of the inverse subframe gain, yielding Q31 from a Q16 gain
/// (RFC 6716 Section 4.2.7.9.1).
const INV_GAIN_RECIPROCAL_Q: i32 = 47;

/// Left shift restoring Q31 after scaling the inverse gain by the Q14 LTP scaling parameter
/// (RFC 6716 Section 4.2.7.9.1).
const LTP_SCALE_Q14_RESTORE_SHIFT: u32 = 2;

/// Left shift turning the Q13 LTP prediction into Q14 (RFC 6716 Section 4.2.7.9.1).
const LTP_PRED_Q13_TO_Q14_SHIFT: u32 = 1;

/// Left shift turning the Q14 LTP residual into the Q15 LTP state (RFC 6716 Section 4.2.7.9.1).
const RES_Q14_TO_Q15_SHIFT: u32 = 1;

/// Taps of the 5-tap LTP filter on either side of the pitch lag (RFC 6716 Section 4.2.7.9.1).
const LTP_HALF_ORDER: usize = LTP_ORDER / 2;

/// Fraction bits of the Q12 LPC analysis filter (RFC 6716 Section 4.2.7.9.1).
const LPC_ANALYSIS_Q12_SHIFT: u32 = 12;

/// Duration of the stereo weight interpolation at the start of a frame (RFC 6716 Section 4.2.8).
const STEREO_INTERP_LEN_MS: usize = 8;

/// Mid and side samples kept from the previous frame for the one-sample prediction delay and
/// the three-tap low-pass filter (RFC 6716 Section 4.2.8).
pub(crate) const STEREO_HISTORY_SAMPLES: usize = 2;

/// Left shift turning the three-tap low-pass sum of mid samples into Q11 (RFC 6716 Section 4.2.8).
const MID_LPF_TO_Q11_SHIFT: u32 = 9;

/// Left shift turning a side sample into Q8 (RFC 6716 Section 4.2.8).
const SIDE_TO_Q8_SHIFT: u32 = 8;

/// Left shift turning a mid sample into Q11 (RFC 6716 Section 4.2.8).
const MID_TO_Q11_SHIFT: u32 = 11;

/// Right shift turning the Q8 side prediction back into PCM (RFC 6716 Section 4.2.8).
const SIDE_Q8_TO_PCM_SHIFT: u32 = 8;

/// Runs the LPC analysis (whitening) filter with Q12 coefficients `a_q12` over `input`
/// ([RFC 6716 Section 4.2.7.9.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9.1>)).
///
/// The first `a_q12.len()` output samples are set to zero.
pub(crate) fn lpc_analysis_filter(out: &mut [i16], input: &[i16], a_q12: &[i16]) {
    let order = a_q12.len();
    let len = out.len().min(input.len());
    if len <= order || order == 0 {
        out[..len].fill(0);
        return;
    }

    out[..order].fill(0);
    for i in order..len {
        let prediction_q12 = a_q12.iter().enumerate().fold(0i32, |acc, (j, &a_tap)| {
            acc.wrapping_add((input[i - 1 - j] as i32) * (a_tap as i32))
        });
        let residual_q12 =
            ((input[i] as i32) << LPC_ANALYSIS_Q12_SHIFT).wrapping_sub(prediction_q12);
        out[i] = saturate_i16(round_shift(residual_q12, LPC_ANALYSIS_Q12_SHIFT));
    }
}

/// Per-subframe context shared across voiced LTP synthesis and rewhitening.
#[derive(Clone, Copy)]
struct LtpSynthesisCtx {
    k: usize,
    lpc_order: usize,
    interpolated: bool,
    ltp_scale_q14: i16,
}

impl ChannelState {
    /// Synthesizes `frame.output_pcm` from `frame.exc_q14` through LTP rewhitening, LTP
    /// synthesis and LPC synthesis, and updates the synthesis history
    /// ([RFC 6716 Section 4.2.7.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>)).
    pub(crate) fn synthesize_frame(&mut self, frame: &mut Frame) -> Result<()> {
        let subframes = frame.num_subframes;
        let frame_length = frame.sample_count;
        if subframes == 0 || frame_length == 0 || frame_length > MAX_FRAME_SAMPLES {
            return Err(Error::MismatchFrameSubframes.into());
        }
        let subframe_samples = frame_length / subframes;
        let lpc_order = frame.lpc_order;
        if (lpc_order != 10 && lpc_order != 16) || subframe_samples > MAX_SUBFRAME_SAMPLES {
            return Err(Error::InvalidLSFCoefficients.into());
        }

        let mut ltp_state_q15 = [0i32; MAX_LTP_MEM_SAMPLES + MAX_FRAME_SAMPLES];
        let mut lpc_state_q14 = [0i32; MAX_LPC_ORDER + MAX_SUBFRAME_SAMPLES];
        lpc_state_q14[..MAX_LPC_ORDER].copy_from_slice(&self.lpc_state_q14);
        let voiced = frame.frame_type.signal_type == SignalType::Voiced;
        let Frame {
            subframes: ref frame_subframes,
            exc_q14: ref frame_exc_q14,
            output_pcm: ref mut frame_output_pcm,
            lsf_interpolation_index,
            ltp_scale_q14,
            ..
        } = *frame;
        let interpolated = lsf_interpolation_index.unwrap_or(4) < 4;

        for (k, ((subframe, exc_chunk), out_chunk)) in frame_subframes[..subframes]
            .iter()
            .zip(frame_exc_q14[..frame_length].chunks_exact(subframe_samples))
            .zip(frame_output_pcm[..frame_length].chunks_exact_mut(subframe_samples))
            .enumerate()
        {
            let gain_q16 = subframe.gain_q16;
            let gain_adj_q16 =
                self.rescale_lpc_state(gain_q16, &mut lpc_state_q14[..MAX_LPC_ORDER]);

            let residual_q14 = &mut lpc_state_q14[MAX_LPC_ORDER..MAX_LPC_ORDER + subframe_samples];
            match voiced {
                true => {
                    let ctx = LtpSynthesisCtx { k, lpc_order, interpolated, ltp_scale_q14 };
                    self.ltp_synthesis_subframe(
                        ctx,
                        subframe,
                        gain_adj_q16,
                        exc_chunk,
                        &mut ltp_state_q15,
                        residual_q14,
                    );
                }
                false => residual_q14.copy_from_slice(exc_chunk),
            }

            lpc_synthesize_q14(
                &mut lpc_state_q14[..MAX_LPC_ORDER + subframe_samples],
                &subframe.lpc_coeffs_q12[..lpc_order],
                gain_q16 >> GAIN_Q16_TO_Q10_SHIFT,
                out_chunk,
            );
            if voiced && interpolated && subframes > 2 && k < 2 {
                let dst_start = self.ltp_mem_length + k * subframe_samples;
                self.output_history[dst_start..dst_start + subframe_samples]
                    .copy_from_slice(out_chunk);
            }
            lpc_state_q14.copy_within(subframe_samples..subframe_samples + MAX_LPC_ORDER, 0);
        }

        self.lpc_state_q14.copy_from_slice(&lpc_state_q14[..MAX_LPC_ORDER]);
        self.push_output_history(&frame.output_pcm[..frame_length]);
        self.prev_pitch_lag = frame.subframes[subframes - 1].pitch_lag as i32;
        return Ok(());
    }

    /// Scales the LPC synthesis history to a changed subframe gain and returns the Q16 ratio
    /// of the previous gain to `gain_q16`, unit when the gain is unchanged
    /// ([RFC 6716 Section 4.2.7.9.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9.2>)).
    fn rescale_lpc_state(&mut self, gain_q16: i32, lpc_state_q14: &mut [i32]) -> i32 {
        if gain_q16 == self.prev_gain_q16 {
            return UNIT_GAIN_Q16;
        }
        let adj_q16 = div_q(self.prev_gain_q16, gain_q16, 16);
        lpc_state_q14.iter_mut().for_each(|state| *state = mul_q16(adj_q16, *state));
        self.prev_gain_q16 = gain_q16;
        return adj_q16;
    }

    /// LTP synthesis of subframe `ctx.k`: brings the LTP state up to date with the output history
    /// (rewhitened on the first subframe and on the third when the NLSFs are interpolated,
    /// rescaled on a gain change otherwise) and writes the Q14 residual, excitation plus
    /// pitch prediction, into `residual_q14`
    /// ([RFC 6716 Section 4.2.7.9.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9.1>)).
    fn ltp_synthesis_subframe(
        &mut self,
        ctx: LtpSynthesisCtx,
        subframe: &crate::silk::Subframe,
        gain_adj_q16: i32,
        excitation_q14: &[i32],
        ltp_state_q15: &mut [i32],
        residual_q14: &mut [i32],
    ) {
        let subframe_samples = residual_q14.len();
        let lag = subframe.pitch_lag as usize;
        let state_end = self.ltp_mem_length + ctx.k * subframe_samples;
        let rewhiten = ctx.k == 0 || (ctx.k == 2 && ctx.interpolated);

        match (rewhiten, gain_adj_q16 != UNIT_GAIN_Q16) {
            (true, _) => self.rewhiten_ltp_state(
                ctx,
                subframe,
                subframe_samples,
                &mut ltp_state_q15[..state_end],
            ),
            (false, true) => ltp_state_q15[state_end - (lag + LTP_HALF_ORDER)..state_end]
                .iter_mut()
                .for_each(|state| *state = mul_q16(gain_adj_q16, *state)),
            (false, false) => {}
        }

        let b_q14: [i16; LTP_ORDER] =
            std::array::from_fn(|j| (subframe.ltp_coeffs[j] as i16) << LTP_COEFF_Q7_TO_Q14_SHIFT);
        predict_ltp_subframe(ltp_state_q15, state_end, lag, &b_q14, excitation_q14, residual_q14);
    }

    /// Rewhitens the output history preceding subframe `ctx.k` through the subframe LPC analysis
    /// filter and scales it by the inverse subframe gain (and, on the first subframe, by the
    /// LTP scaling parameter) into the tail of `ltp_state_q15`
    /// ([RFC 6716 Section 4.2.7.9.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9.1>)).
    fn rewhiten_ltp_state(
        &mut self,
        ctx: LtpSynthesisCtx,
        subframe: &crate::silk::Subframe,
        subframe_samples: usize,
        ltp_state_q15: &mut [i32],
    ) {
        let lag = subframe.pitch_lag as usize;
        let ltp_mem_length = self.ltp_mem_length;
        let state_end = ltp_state_q15.len();

        let start = ltp_mem_length - lag - ctx.lpc_order - LTP_HALF_ORDER;
        let history_start = start + ctx.k * subframe_samples;
        let mut whitened = [0i16; MAX_LTP_MEM_SAMPLES];
        lpc_analysis_filter(
            &mut whitened[start..ltp_mem_length],
            &self.output_history[history_start..history_start + (ltp_mem_length - start)],
            &subframe.lpc_coeffs_q12[..ctx.lpc_order],
        );

        let mut inv_gain_q31 = reciprocal_q(subframe.gain_q16, INV_GAIN_RECIPROCAL_Q);
        if ctx.k == 0 {
            inv_gain_q31 =
                mul_q16_i16(inv_gain_q31, ctx.ltp_scale_q14) << LTP_SCALE_Q14_RESTORE_SHIFT;
        }
        let scaled = lag + LTP_HALF_ORDER;
        ltp_state_q15[state_end - scaled..]
            .iter_mut()
            .zip(whitened[ltp_mem_length - scaled..ltp_mem_length].iter())
            .for_each(|(state, &sample)| *state = mul_q16_i16(inv_gain_q31, sample));
    }
}

/// Adds the 5-tap pitch prediction to every excitation sample of a subframe and appends the
/// Q15 residual to the LTP state at `state_end`
/// ([RFC 6716 Section 4.2.7.9.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9.1>)).
fn predict_ltp_subframe(
    ltp_state_q15: &mut [i32],
    state_end: usize,
    lag: usize,
    b_q14: &[i16; LTP_ORDER],
    excitation_q14: &[i32],
    residual_q14: &mut [i32],
) {
    let pred_lag_base = state_end - lag + LTP_HALF_ORDER;
    for (i, (&exc_q14, res_q14)) in excitation_q14.iter().zip(residual_q14.iter_mut()).enumerate() {
        let ltp_pred_q13 = ltp_predict_q13(&ltp_state_q15[..=pred_lag_base + i], b_q14);
        *res_q14 = exc_q14.wrapping_add(ltp_pred_q13 << LTP_PRED_Q13_TO_Q14_SHIFT);
        ltp_state_q15[state_end + i] = *res_q14 << RES_Q14_TO_Q15_SHIFT;
    }
}

/// Converts adaptive Mid-Side channels (`mid`, `side`, each of length `frame_length + 2` with
/// the decoded frame samples at `2..2 + frame_length`) into Left and Right channels at
/// `1..1 + frame_length`, interpolating the prediction weights over the first 8 ms
/// ([RFC 6716 Section 4.2.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.8>)).
pub(crate) fn unmix_mid_side_to_left_right(
    stereo_state: &mut StereoState,
    mid: &mut [i16],
    side: &mut [i16],
    weights: StereoWeights,
    sample_rate_khz: usize,
    frame_length: usize,
) {
    mid[..STEREO_HISTORY_SAMPLES].copy_from_slice(&stereo_state.mid_history);
    side[..STEREO_HISTORY_SAMPLES].copy_from_slice(&stereo_state.side_history);
    stereo_state
        .mid_history
        .copy_from_slice(&mid[frame_length..frame_length + STEREO_HISTORY_SAMPLES]);
    stereo_state
        .side_history
        .copy_from_slice(&side[frame_length..frame_length + STEREO_HISTORY_SAMPLES]);

    let interp_samples = STEREO_INTERP_LEN_MS * sample_rate_khz;
    let denom_q16 = UNIT_GAIN_Q16 / (interp_samples as i32);
    let [prev0_q13, prev1_q13] = stereo_state.prediction_prev_q13;
    let delta0_q13 =
        round_shift(((weights.w0_q13 - prev0_q13) as i16 as i32) * (denom_q16 as i16 as i32), 16);
    let delta1_q13 =
        round_shift(((weights.w1_q13 - prev1_q13) as i16 as i32) * (denom_q16 as i16 as i32), 16);

    let target_q13 = (weights.w0_q13, weights.w1_q13);
    let (mut pred0_q13, mut pred1_q13) = (prev0_q13, prev1_q13);
    for (i, (mid_window, side_out)) in mid[..frame_length + STEREO_HISTORY_SAMPLES]
        .windows(3)
        .zip(&mut side[1..=frame_length])
        .enumerate()
    {
        if i < interp_samples {
            pred0_q13 += delta0_q13;
            pred1_q13 += delta1_q13;
        }
        let (w0, w1) = if i < interp_samples { (pred0_q13, pred1_q13) } else { target_q13 };
        *side_out = predict_side_sample(mid_window, *side_out, w0, w1);
    }
    stereo_state.prediction_prev_q13 = [weights.w0_q13, weights.w1_q13];

    mid[1..=frame_length].iter_mut().zip(side[1..=frame_length].iter_mut()).for_each(
        |(mid_sample, side_sample)| {
            let (mid_val, side_val) = (*mid_sample as i32, *side_sample as i32);
            *mid_sample = saturate_i16(mid_val + side_val);
            *side_sample = saturate_i16(mid_val - side_val);
        },
    );
}

/// Adds the prediction from the low-pass filtered mid (`w0`) and the delayed mid (`w1`) to one
/// side sample; `mid_window` holds the mid samples around the predicted position
/// ([RFC 6716 Section 4.2.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.8>)).
#[inline]
fn predict_side_sample(mid_window: &[i16], side: i16, pred0_q13: i32, pred1_q13: i32) -> i16 {
    let sum_lpf_q11 = (((mid_window[0] as i32) + (mid_window[2] as i32))
        + ((mid_window[1] as i32) << 1))
        << MID_LPF_TO_Q11_SHIFT;
    let sum_q8 = mul_add_q16_i16((side as i32) << SIDE_TO_Q8_SHIFT, sum_lpf_q11, pred0_q13 as i16);
    let sum_q8 =
        mul_add_q16_i16(sum_q8, (mid_window[1] as i32) << MID_TO_Q11_SHIFT, pred1_q13 as i16);
    return saturate_i16(round_shift(sum_q8, SIDE_Q8_TO_PCM_SHIFT));
}

/// Prepends the two-sample mid history to a mono SILK frame so that mono and stereo output
/// share the one-sample prediction delay of the stereo unmixing, placing the resampler input
/// at `mid[1..1 + frame_length]`
/// ([RFC 6716 Section 4.2.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.8>)).
pub(crate) fn buffer_mono_mid(
    stereo_state: &mut StereoState,
    mid: &mut [i16],
    frame_length: usize,
) {
    mid[..STEREO_HISTORY_SAMPLES].copy_from_slice(&stereo_state.mid_history);
    stereo_state
        .mid_history
        .copy_from_slice(&mid[frame_length..frame_length + STEREO_HISTORY_SAMPLES]);
}

/// Resamples one channel frame from the internal rate to the output rate and writes it as
/// normalized `f32` PCM into `out`, returning the number of samples written
/// ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
pub(crate) fn resample_to_f32(
    resampler: &mut ResamplerState,
    input: &[i16],
    out: &mut [f32],
) -> Result<usize> {
    let mut resampled = [0i16; MAX_RESAMPLED_FRAME_SAMPLES];
    let n_out = resampler.resample(&mut resampled, input)?;
    if out.len() < n_out {
        return Err(Error::BufferOverflow.into());
    }
    out[..n_out].iter_mut().zip(resampled[..n_out].iter()).for_each(|(dst, &src)| {
        *dst = (src as f32) * INT16_TO_F32_SCALE;
    });
    return Ok(n_out);
}

#[cfg(test)]
mod tests {
    // Fixtures: packets and expected values recorded from libopus 1.6.1. The behaviour they pin is
    // normative (RFC 6716 Section 4.2.8) and is verified against the RFC 8251 test vectors by
    // `tests/rfc6716_conformance.rs`.
    use super::*;

    #[test]
    fn stereo_mid_side_to_left_right_matches_recorded_values() {
        const EXPECTED_STEREO_LEFT: [i16; 16] = [
            -594, -1014, 1478, -861, 406, -183, -967, 1568, -465, 673, -3352, 3939, -1276, 1470,
            -5677, 6250,
        ];
        const EXPECTED_STEREO_RIGHT: [i16; 16] = [
            -246, -1386, 288, -271, 92, 319, 197, -164, -1573, 1999, 46, 1, -3298, 3738, -165, 226,
        ];

        let mut stereo = StereoState::new();
        stereo.prediction_prev_q13 = [-1524, 2816];
        stereo.mid_history = [310, -420];
        stereo.side_history = [95, -110];

        let mut x_mid = [0i16; 18];
        let mut x_side = [0i16; 18];
        for i in 0..16 {
            let sign_m = if (i & 1) != 0 { -1 } else { 1 };
            let sign_s = if (i & 2) != 0 { 1 } else { -1 };
            x_mid[i + 2] = ((i as i32 * 317 - 1200) * sign_m) as i16;
            x_side[i + 2] = ((i as i32 * 193 - 500) * sign_s) as i16;
        }
        unmix_mid_side_to_left_right(
            &mut stereo,
            &mut x_mid,
            &mut x_side,
            StereoWeights { w0_q13: 3120, w1_q13: -1840 },
            12,
            16,
        );
        assert_eq!(&x_mid[1..17], &EXPECTED_STEREO_LEFT);
        assert_eq!(&x_side[1..17], &EXPECTED_STEREO_RIGHT);
    }

    /// Resampled output is rejected, not truncated, when the output plane is shorter than the
    /// frame ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
    #[test]
    fn resampled_output_needs_room_for_the_whole_frame() {
        let mut resampler = ResamplerState::default();
        resampler.init(16_000, 48_000).unwrap();
        let result = resample_to_f32(&mut resampler, &[0; 160], &mut [0.0; 10]);
        assert!(Error::BufferOverflow.matches(&result));
    }
}
