//! SILK packet loss concealment, paired with the comfort noise of [`super::cng`]
//! ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
//!
//! RFC 6716 Section 4.4 leaves concealment to the decoder. A lost frame is extrapolated from the
//! last decoded pitch lag, LTP and LPC filters with attenuated pseudorandom excitation through
//! the synthesis of [Section 4.2.7.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>),
//! gaps are filled with comfort noise shaped by smoothed NLSFs and gains, and the first decoded
//! frame after a loss is faded in to the energy of the concealed one.
//!
//! ```text
//!                     +-----------------------------------+
//! Valid Decoded ----> | PlcState::update_from_decoded     |
//! SILK Frame          | CngState::update_comfort_noise    |
//!                     +-----------------+-----------------+
//!                                       |
//!                                       v
//!                     +-----------------------------------+
//! Lost / DTX    ----> | PlcState::conceal_lost_frame      |
//! Frame               |   1. Rewhiten LTP history         |
//!                     |   2. Harmonic LTP + LCG noise     |
//!                     |   3. BWE LPC synthesis            |
//!                     +-----------------+-----------------+
//!                                       |
//!                                       v
//!                     +-----------------------------------+
//!                     | CngState::generate_comfort_noise  |
//!                     |   Smoothed NLSF -> LPC + LCG exc  |
//!                     +-----------------+-----------------+
//!                                       |
//!                                       v
//!                     +-----------------------------------+
//!                     | PlcState::glue_frames             |
//!                     |   Energy-matched onset fade-in    |
//!                     +-----------------------------------+
//! ```

use crate::fixed::{
    mul_add_q16_i16, mul_q16, mul_q16_i16, reciprocal_q, round_shift, saturate_i16, sqrt_approx,
};
use crate::silk::excitation::advance_lcg_seed;
use crate::silk::lpc::{inverse_prediction_gain_q30, lpc_synthesize_q14, ltp_predict_q13};
use crate::silk::synthesis::lpc_analysis_filter;
use crate::silk::{
    ChannelState, Frame, LTP_ORDER, MAX_FRAME_SAMPLES, MAX_LPC_ORDER, MAX_LTP_MEM_SAMPLES,
    MAX_SUBFRAME_SAMPLES, MAX_SUBFRAMES, SignalType, SilkLayout,
};

/// Number of loss-stage attenuation entries (`loss_count == 0` vs `loss_count >= 1`).
const NUM_ATTENUATION_STAGES: usize = 2;

/// Unit gain (`1.0`) in Q16 fixed-point representation (`65_536`).
const UNIT_GAIN_Q16: i32 = 1 << 16;

/// Unit scale (`1.0`) in Q14 fixed-point representation (`16_384`).
const UNIT_SCALE_Q14: i16 = 1 << 14;

/// Fixed-point bit shift for Q16 quantities (`16`).
pub(super) const Q16_SHIFT: u32 = 16;

/// Fixed-point bit shift for Q15 quantities (`15`).
const Q15_SHIFT: u32 = 15;

/// Fixed-point bit shift for Q14 quantities (`14`).
const Q14_SHIFT: u32 = 14;

/// Fixed-point bit shift for Q8 quantities (`8`).
pub(super) const Q8_SHIFT: u32 = 8;

/// Bit shift converting Q16 gains down to Q10 (`16 - 10 = 6`).
pub(super) const Q16_TO_Q10_SHIFT: u32 = 6;

/// Bit shift converting Q7 LTP tap coefficients up to Q14 (`14 - 7 = 7`).
const LTP_COEFF_Q7_TO_Q14_SHIFT: u32 = 7;

/// Bit shift converting integer pitch lag into Q8 (`8`).
const PITCH_LAG_Q8_SHIFT: u32 = 8;

/// Bit shift for minimum voiced LTP gain clamping factor (`Q10`).
const MIN_CLAMP_SCALE_SHIFT: u32 = 10;

/// Bit shift for maximum voiced LTP gain clamping factor (`Q14`).
const MAX_CLAMP_SCALE_SHIFT: u32 = 14;

/// Target fractional bit precision (`Q24`) for energy ratio in `glue_frames`.
const GLUE_FRAC_Q24_SHIFT: i32 = 24;

/// Bit shift converting `sqrt_approx(Q24)` (`Q12`) up to `Q16` (`16 - 12 = 4`).
const GLUE_SQRT_Q12_TO_Q16_SHIFT: u32 = 4;

/// Bit shift applied to the per-sample linear fade-in slope in `glue_frames` (`2`).
const GLUE_SLOPE_Q16_SHIFT: u32 = 2;

/// Harmonic (voiced LTP) attenuation factors in Q15 (`0.99` and `0.95`).
const HARMONIC_ATTENUATION_Q15: [i16; NUM_ATTENUATION_STAGES] = [32_440, 31_130];

/// Random excitation attenuation factors in Q15 for voiced concealment (`0.95` and `0.80`).
const VOICED_NOISE_ATTENUATION_Q15: [i16; NUM_ATTENUATION_STAGES] = [31_130, 26_214];

/// Random excitation attenuation factors in Q15 for unvoiced concealment (`0.99` and `0.90`).
const UNVOICED_NOISE_ATTENUATION_Q15: [i16; NUM_ATTENUATION_STAGES] = [32_440, 29_491];

/// LPC chirp bandwidth expansion factor applied per lost frame (`0.99` in Q16).
const PLC_BWE_CHIRP_Q16: i32 = 64_881;

/// LPC chirp bandwidth expansion factor applied to the first valid frame after loss (`0.97` in Q16).
pub(crate) const BWE_AFTER_LOSS_Q16: i32 = 63_570;

/// Minimum starting voiced LTP pitch gain in Q14 (`0.70` in Q14).
const VOICED_PITCH_GAIN_MIN_Q14: i32 = 11_469;

/// Maximum starting voiced LTP pitch gain in Q14 (`0.95` in Q14).
const VOICED_PITCH_GAIN_MAX_Q14: i32 = 15_565;

/// Minimum initial random excitation scaling for voiced PLC in Q14 (`0.20` in Q14).
const VOICED_MIN_RAND_SCALE_Q14: i16 = 3_277;

/// Maximum pitch lag in milliseconds during PLC pitch drift (`18 ms`).
const MAX_PITCH_LAG_MS: i32 = 18;

/// Subframe pitch drift factor in Q16 (`0.01` in Q16).
const PITCH_DRIFT_FACTOR_Q16: i16 = 655;

/// Size of the random excitation lookup window in samples (`128`).
const RAND_BUF_SIZE: usize = 128;

/// Bitmask for indexing into the 128-sample random excitation window (`0x7F`).
const RAND_BUF_MASK: i32 = (RAND_BUF_SIZE as i32) - 1;

/// High threshold (`2^3 = 8 dB`) for inverse LPC prediction gain down-scaling during unvoiced PLC.
const LOG2_INV_LPC_GAIN_HIGH_THRES: u32 = 3;

/// Low threshold (`2^8 = 24 dB`) for inverse LPC prediction gain down-scaling during unvoiced PLC.
const LOG2_INV_LPC_GAIN_LOW_THRES: u32 = 8;

/// Taps of the 5-tap LTP filter on either side of the pitch lag.
const LTP_HALF_ORDER: usize = LTP_ORDER / 2;

/// Rounding bias the Q12 pitch prediction accumulator starts from; an unvoiced concealment adds
/// no pitch prediction to it (`2`).
const PREDICTION_ROUNDING_BIAS_Q12: i32 = 2;

/// Right shift selecting the top seven seed bits, an index into the noise window (`25`).
const RAND_INDEX_SHIFT: u32 = 25;

/// Left shift turning the Q12 concealed excitation into the Q14 LTP state (`2`).
const Q12_TO_Q14_SHIFT: u32 = 2;

/// Precision argument of the inverse of a Q16 gain that yields Q30 (`46`).
const INV_GAIN_Q30_RECIPROCAL_Q: i32 = 46;

/// Unit inverse prediction gain (`1.0`) in Q30.
const UNIT_INV_GAIN_Q30: i32 = 1 << 30;

/// Applies progressive chirp bandwidth expansion (`chirp_q16`) in-place to Q12 LPC
/// coefficients, the 16-bit form of the chirp in RFC 6716 Section 4.2.7.5.7.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.7>
///
/// Uses rounded `32 x 16` products rather than floor-truncated `mul_q16_i16` to avoid
/// bias that could destabilize high-order synthesis filters.
pub(crate) fn chirp_bandwidth_expand(coeffs_q12: &mut [i16], mut factor_q16: i32) {
    let Some((last_coeff, head_coeffs)) = coeffs_q12.split_last_mut() else {
        return;
    };

    let step_q16 = factor_q16 - UNIT_GAIN_Q16;
    for coeff in head_coeffs.iter_mut() {
        *coeff = round_shift(factor_q16 * (*coeff as i32), Q16_SHIFT) as i16;
        factor_q16 += round_shift(factor_q16 * step_q16, Q16_SHIFT);
    }
    *last_coeff = round_shift(factor_q16 * (*last_coeff as i32), Q16_SHIFT) as i16;
    return;
}

/// Computes a right-shifted sum of squares of `samples` that fits into a signed 32-bit
/// integer with two bits of headroom, for the PLC energy comparisons (RFC 6716 Section 4.4).
///
/// Returns `(energy, shift)` such that `sum(x[i]^2) approx energy << shift`.
pub(crate) fn sum_sqr_shift(samples: &[i16]) -> (i32, i32) {
    if samples.is_empty() {
        return (0, 0);
    }

    let len = samples.len() as i32;
    let mut shift = 31 - ((len as u32).leading_zeros() as i32);

    let first_pass_energy = accumulate_shifted_pair_squares(samples, len, shift as u32);
    let leading_zeros = (first_pass_energy as u32).leading_zeros() as i32;
    shift = (shift + 3 - leading_zeros).max(0);

    let energy = accumulate_shifted_pair_squares(samples, 0, shift as u32);
    return (energy, shift);
}

/// Accumulates pairwise sums of squares right-shifted by `shift` starting from `initial`.
#[inline]
fn accumulate_shifted_pair_squares(samples: &[i16], initial: i32, shift: u32) -> i32 {
    let mut acc = initial as u32;
    let mut chunks = samples.chunks_exact(2);

    for pair in chunks.by_ref() {
        let sq0 = ((pair[0] as i32) * (pair[0] as i32)) as u32;
        let sq1 = ((pair[1] as i32) * (pair[1] as i32)) as u32;
        let pair_sum = sq0.wrapping_add(sq1);
        acc = acc.wrapping_add(pair_sum >> shift);
    }

    if let Some(&rem) = chunks.remainder().first() {
        let sq = ((rem as i32) * (rem as i32)) as u32;
        acc = acc.wrapping_add(sq >> shift);
    }

    return acc as i32;
}

/// Per-channel SILK Packet Loss Concealment (PLC) state (RFC 6716 Section 4.4).
///
/// Tracks voiced pitch lag, simplified single-tap LTP gain, previous LPC synthesis
/// coefficients, subframe quantization gains, noise excitation history, and concealed
/// frame energy across packet loss bursts.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>
#[derive(Debug, Clone, Copy)]
pub(crate) struct PlcState {
    /// Pitch lag the concealment repeats, in Q8 samples.
    pub(super) pitch_l_q8: i32,
    /// Single-tap (centre) LTP filter of the concealment in Q14.
    pub(super) ltp_coef_q14: [i16; LTP_ORDER],
    /// LTP scaling of the last decoded voiced frame in Q14.
    ltp_scale_q14: i16,
    /// Gains of the last two decoded subframes in Q16.
    pub(super) prev_gain_q16: [i32; 2],
    /// Seed of the noise generator.
    rand_seed: i32,
    /// Current noise scale in Q14.
    pub(super) rand_scale_q14: i16,
    /// Energy of the last concealed frame, right-shifted by `conc_energy_shift`.
    conc_energy: i32,
    /// Right shift applied to `conc_energy`.
    conc_energy_shift: i32,
    /// LPC filter of the last decoded subframe in Q12.
    prev_lpc_q12: [i16; MAX_LPC_ORDER],
    /// Excitation of the last decoded frame in Q14, the noise source.
    prev_exc_q14: [i32; MAX_FRAME_SAMPLES],
    /// Subframe length of the last decoded frame.
    subfr_length: usize,
    /// Subframe count of the last decoded frame.
    nb_subfr: usize,
    /// Internal sample rate in kHz the state was set up for.
    sample_rate_khz: usize,
    /// Lost frames since the last decoded frame.
    pub(super) loss_count: usize,
    /// Whether the previous frame was concealed, so the next decoded frame is glued to it.
    last_frame_lost: bool,
}

impl Default for PlcState {
    fn default() -> Self {
        return Self::new();
    }
}

impl PlcState {
    /// Creates a new `PlcState` initialized to default pre-reset values.
    pub(crate) const fn new() -> Self {
        return Self {
            pitch_l_q8: 0,
            ltp_coef_q14: [0; LTP_ORDER],
            ltp_scale_q14: 0,
            prev_gain_q16: [UNIT_GAIN_Q16, UNIT_GAIN_Q16],
            rand_seed: 0,
            rand_scale_q14: 0,
            conc_energy: 0,
            conc_energy_shift: 0,
            prev_lpc_q12: [0; MAX_LPC_ORDER],
            prev_exc_q14: [0; MAX_FRAME_SAMPLES],
            subfr_length: 20,
            nb_subfr: 2,
            sample_rate_khz: 0,
            loss_count: 0,
            last_frame_lost: false,
        };
    }

    /// Resets only the frame-dependent pitch lag, unit gains, and default subframe geometry
    /// when the internal SILK sample rate changes (RFC 6716 Section 4.4).
    pub(crate) fn reset_frame_parameters(&mut self, frame_length: usize) {
        self.pitch_l_q8 = (frame_length as i32) << (PITCH_LAG_Q8_SHIFT - 1);
        self.prev_gain_q16 = [UNIT_GAIN_Q16, UNIT_GAIN_Q16];
        self.subfr_length = 20;
        self.nb_subfr = 2;
        return;
    }

    /// Synchronizes `self.sample_rate_khz` with the channel's internal sample rate, resetting
    /// frame parameters if the rate changed (RFC 6716 Section 4.4).
    #[inline]
    pub(crate) fn ensure_sample_rate(&mut self, sample_rate_khz: usize, frame_length: usize) {
        if self.sample_rate_khz != sample_rate_khz {
            self.reset_frame_parameters(frame_length);
            self.sample_rate_khz = sample_rate_khz;
        }
        return;
    }

    /// Applies post-loss bandwidth expansion (`BWE_AFTER_LOSS_Q16`) to all active subframe
    /// LPC coefficient sets when recovering from one or more lost frames
    /// (RFC 6716 Section 4.4).
    pub(crate) fn expand_lpc_after_loss(&self, frame: &mut Frame) {
        if self.loss_count == 0 {
            return;
        }

        let lpc_order = frame.lpc_order.min(MAX_LPC_ORDER);
        for subframe in frame.active_subframes_mut() {
            chirp_bandwidth_expand(&mut subframe.lpc_coeffs_q12[..lpc_order], BWE_AFTER_LOSS_Q16);
        }
        return;
    }

    /// Updates the PLC state from a validly decoded SILK frame (RFC 6716 Section 4.4).
    ///
    /// For voiced frames, scans backward from the final subframe across the pitch period to
    /// locate the subframe with maximum summed LTP gain, collapses the 5-tap LTP filter into a
    /// single center tap clamped to `[0.70, 0.95]` in Q14, and records its pitch lag in Q8.
    /// For unvoiced or inactive frames, sets the pitch lag to `18 ms * sample_rate_khz` and zeroes the
    /// LTP filter.
    pub(crate) fn update_from_decoded_frame(&mut self, ch_state: &mut ChannelState, frame: &Frame) {
        let frame_length = frame.sample_count.min(MAX_FRAME_SAMPLES);
        let nb_subfr = frame.num_subframes.clamp(2, MAX_SUBFRAMES);
        let subfr_length = (frame_length / nb_subfr).min(MAX_SUBFRAME_SAMPLES);
        let lpc_order = frame.lpc_order.min(MAX_LPC_ORDER);
        let sample_rate_khz = ch_state.sample_rate_khz;

        self.ensure_sample_rate(sample_rate_khz, frame_length);

        let signal_type = frame.frame_type.signal_type;

        match signal_type {
            SignalType::Voiced => {
                self.select_voiced_pitch_and_ltp_gain(frame, subfr_length, nb_subfr);
                self.ltp_scale_q14 = frame.ltp_scale_q14;
                ch_state.prev_pitch_lag = frame.subframes[nb_subfr - 1].pitch_lag as i32;
            }
            SignalType::Inactive | SignalType::Unvoiced => {
                self.pitch_l_q8 =
                    ((sample_rate_khz as i32) * MAX_PITCH_LAG_MS) << PITCH_LAG_Q8_SHIFT;
                self.ltp_coef_q14 = [0; LTP_ORDER];
                self.ltp_scale_q14 = 0;
                ch_state.prev_pitch_lag = 0;
            }
        }

        self.prev_lpc_q12[..lpc_order]
            .copy_from_slice(&frame.subframes[nb_subfr - 1].lpc_coeffs_q12[..lpc_order]);
        self.prev_gain_q16 =
            [frame.subframes[nb_subfr - 2].gain_q16, frame.subframes[nb_subfr - 1].gain_q16];
        self.prev_exc_q14[..frame_length].copy_from_slice(&frame.exc_q14[..frame_length]);
        self.subfr_length = subfr_length;
        self.nb_subfr = nb_subfr;
        self.loss_count = 0;
        return;
    }

    /// Selects the voiced pitch lag and clamped center-tap LTP coefficient from the last
    /// pitch period of `frame` (RFC 6716 Section 4.4).
    fn select_voiced_pitch_and_ltp_gain(
        &mut self,
        frame: &Frame,
        subfr_length: usize,
        nb_subfr: usize,
    ) {
        let last_pitch_lag = frame.subframes[nb_subfr - 1].pitch_lag as usize;
        let (max_ltp_gain_q14, selected_lag_q8) = frame.subframes[..nb_subfr]
            .iter()
            .rev()
            .enumerate()
            .take_while(|&(i, _)| i * subfr_length < last_pitch_lag)
            .fold((0i32, self.pitch_l_q8), |(max_gain, best_lag_q8), (_, subfr)| {
                let gain_q14: i32 =
                    subfr.ltp_coeffs.iter().map(|&c| (c as i32) << LTP_COEFF_Q7_TO_Q14_SHIFT).sum();
                return match gain_q14 > max_gain {
                    true => (gain_q14, (subfr.pitch_lag as i32) << PITCH_LAG_Q8_SHIFT),
                    false => (max_gain, best_lag_q8),
                };
            });

        self.pitch_l_q8 = selected_lag_q8;
        self.ltp_coef_q14 = [0; LTP_ORDER];
        self.ltp_coef_q14[LTP_ORDER / 2] = max_ltp_gain_q14 as i16;

        let clamp_scale = match max_ltp_gain_q14 {
            g if g < VOICED_PITCH_GAIN_MIN_Q14 => Some((
                (VOICED_PITCH_GAIN_MIN_Q14 << MIN_CLAMP_SCALE_SHIFT) / g.max(1),
                MIN_CLAMP_SCALE_SHIFT,
            )),
            g if g > VOICED_PITCH_GAIN_MAX_Q14 => Some((
                (VOICED_PITCH_GAIN_MAX_Q14 << MAX_CLAMP_SCALE_SHIFT) / g.max(1),
                MAX_CLAMP_SCALE_SHIFT,
            )),
            _ => None,
        };

        if let Some((scale, shift)) = clamp_scale {
            let scale_i32 = scale as i16 as i32;
            self.ltp_coef_q14
                .iter_mut()
                .for_each(|coeff| *coeff = (((*coeff as i32) * scale_i32) >> shift) as i16);
        }
        return;
    }

    /// Conceals one lost frame of `layout` into `out_pcm` from the channel history: the LTP
    /// history is rewhitened with the bandwidth-expanded last LPC filter, an excitation of
    /// attenuated pitch prediction (voiced) and noise is synthesised, and the LPC synthesis
    /// filter turns it into PCM that extends the output history
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    pub(crate) fn conceal_lost_frame(
        &mut self,
        ch_state: &mut ChannelState,
        layout: &SilkLayout,
        out_pcm: &mut [i16],
    ) {
        let frame_length = layout.frame_samples().min(MAX_FRAME_SAMPLES).min(out_pcm.len());
        let subframes = layout.subframes.clamp(2, MAX_SUBFRAMES);
        let subframe_samples = (frame_length / subframes).min(MAX_SUBFRAME_SAMPLES);
        let lpc_order = layout.lpc_order.min(MAX_LPC_ORDER);
        let sample_rate_khz = ch_state.sample_rate_khz;
        self.ensure_sample_rate(sample_rate_khz, frame_length);
        if ch_state.references.first_frame_after_reset {
            self.prev_lpc_q12.fill(0);
        }

        let prev_gain_q10 = self.prev_gain_q16.map(|gain_q16| gain_q16 >> Q16_TO_Q10_SHIFT);
        let noise_window = self.noise_window(prev_gain_q10, subframe_samples, subframes);
        let voiced = ch_state.references.prev_signal_type == SignalType::Voiced;
        chirp_bandwidth_expand(&mut self.prev_lpc_q12[..lpc_order], PLC_BWE_CHIRP_Q16);
        let a_q12 = self.prev_lpc_q12;
        let shape = self.excitation_shape(voiced, lpc_order);

        let geometry = ConcealmentGeometry {
            ltp_mem_length: ch_state.ltp_mem_length.min(MAX_LTP_MEM_SAMPLES),
            subframes,
            subframe_samples,
            initial_lag: round_shift(self.pitch_l_q8, PITCH_LAG_Q8_SHIFT) as usize,
            max_pitch_lag_q8: (MAX_PITCH_LAG_MS * (sample_rate_khz as i32)) << PITCH_LAG_Q8_SHIFT,
        };
        let mut ltp_state_q14 = [0i32; MAX_LTP_MEM_SAMPLES + MAX_FRAME_SAMPLES];
        self.rewhiten_ltp_history(
            &ch_state.output_history,
            &a_q12[..lpc_order],
            geometry,
            &mut ltp_state_q14,
        );
        let final_lag =
            self.synthesise_excitation(&mut ltp_state_q14, &noise_window, geometry, shape);

        let lpc_start = geometry.ltp_mem_length - MAX_LPC_ORDER;
        synthesise_lpc_frame(
            &mut ch_state.lpc_state_q14,
            &mut ltp_state_q14[lpc_start..lpc_start + MAX_LPC_ORDER + frame_length],
            &a_q12[..lpc_order],
            prev_gain_q10[1],
            &mut out_pcm[..frame_length],
        );
        ch_state.push_output_history(&out_pcm[..frame_length]);
        ch_state.prev_pitch_lag = final_lag as i32;
        self.loss_count += 1;
        return;
    }

    /// Copies the 128 excitation samples that concealment draws its noise from, taken from the
    /// end of the louder of the last two decoded subframes
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    fn noise_window(
        &self,
        prev_gain_q10: [i32; 2],
        subframe_samples: usize,
        subframes: usize,
    ) -> [i32; RAND_BUF_SIZE] {
        let offset =
            self.select_noise_excitation_offset(prev_gain_q10, subframe_samples, subframes);
        let len = RAND_BUF_SIZE.min(MAX_FRAME_SAMPLES.saturating_sub(offset));
        let mut window = [0i32; RAND_BUF_SIZE];
        window[..len].copy_from_slice(&self.prev_exc_q14[offset..offset + len]);
        return window;
    }

    /// Compares scaled excitation energy across the last two subframes and returns the
    /// starting offset of the 128-sample random noise window (RFC 6716 Section 4.4).
    fn select_noise_excitation_offset(
        &self,
        prev_gain_q10: [i32; 2],
        subfr_length: usize,
        nb_subfr: usize,
    ) -> usize {
        let mut scaled_exc = [0i16; 2 * MAX_SUBFRAME_SAMPLES];

        for (i, &gain_q10) in prev_gain_q10.iter().enumerate() {
            let src_start = (i + nb_subfr - 2) * subfr_length;
            let dst_start = i * subfr_length;
            scaled_exc[dst_start..dst_start + subfr_length]
                .iter_mut()
                .zip(self.prev_exc_q14[src_start..src_start + subfr_length].iter())
                .for_each(|(dst, &exc_q14)| {
                    *dst = saturate_i16(mul_q16(exc_q14, gain_q10) >> Q8_SHIFT);
                });
        }

        let (energy1, shift1) = sum_sqr_shift(&scaled_exc[..subfr_length]);
        let (energy2, shift2) = sum_sqr_shift(&scaled_exc[subfr_length..2 * subfr_length]);

        let drop_last_subframe = (energy1 >> shift2) < (energy2 >> shift1);
        let end_subframe = self.nb_subfr.saturating_sub(usize::from(drop_last_subframe));

        return (end_subframe * self.subfr_length).saturating_sub(RAND_BUF_SIZE);
    }

    /// Gains of the concealed excitation at the current loss count: the attenuation stage of the
    /// LTP taps and of the noise, and the initial noise scale of a new loss burst
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    fn excitation_shape(&self, voiced: bool, lpc_order: usize) -> ExcitationShape {
        let stage = self.loss_count.min(NUM_ATTENUATION_STAGES - 1);
        let base_noise_attenuation_q15 = match voiced {
            true => VOICED_NOISE_ATTENUATION_Q15[stage] as i32,
            false => UNVOICED_NOISE_ATTENUATION_Q15[stage] as i32,
        };
        let (rand_scale_q14, noise_attenuation_q15) = self
            .compute_initial_noise_scale_and_attenuation(
                voiced,
                lpc_order,
                base_noise_attenuation_q15,
            );
        return ExcitationShape {
            voiced,
            rand_scale_q14,
            harmonic_attenuation_q15: HARMONIC_ATTENUATION_Q15[stage] as i32,
            noise_attenuation_q15,
        };
    }

    /// Computes the initial random noise scale (`rand_scale_q14`) and adjusted noise
    /// attenuation (`noise_attenuation_q15`) at the start of a packet loss burst (RFC 6716 Section 4.4).
    fn compute_initial_noise_scale_and_attenuation(
        &self,
        voiced: bool,
        lpc_order: usize,
        base_noise_attenuation_q15: i32,
    ) -> (i16, i32) {
        if self.loss_count > 0 {
            return (self.rand_scale_q14, base_noise_attenuation_q15);
        }

        if voiced {
            let residual_scale_q14 = self
                .ltp_coef_q14
                .iter()
                .fold(UNIT_SCALE_Q14, |acc, &tap_q14| acc.wrapping_sub(tap_q14))
                .max(VOICED_MIN_RAND_SCALE_Q14);
            let rand_scale_q14 =
                (((residual_scale_q14 as i32) * (self.ltp_scale_q14 as i32)) >> Q14_SHIFT) as i16;
            return (rand_scale_q14, base_noise_attenuation_q15);
        }

        let inv_gain_q30 = inverse_prediction_gain_q30(&self.prev_lpc_q12[..lpc_order]);
        let high_limit_q30 = UNIT_INV_GAIN_Q30 >> LOG2_INV_LPC_GAIN_HIGH_THRES;
        let low_limit_q30 = UNIT_INV_GAIN_Q30 >> LOG2_INV_LPC_GAIN_LOW_THRES;
        let down_scale_q30 =
            inv_gain_q30.clamp(low_limit_q30, high_limit_q30) << LOG2_INV_LPC_GAIN_HIGH_THRES;
        let noise_attenuation_q15 =
            mul_q16_i16(down_scale_q30, base_noise_attenuation_q15 as i16) >> Q14_SHIFT;

        return (UNIT_SCALE_Q14, noise_attenuation_q15);
    }

    /// Rewhitens the tail of `output_history` through the LPC analysis filter `a_q12` and scales
    /// it by the inverse of the last subframe gain into `ltp_state_q14`
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    fn rewhiten_ltp_history(
        &self,
        output_history: &[i16],
        a_q12: &[i16],
        geometry: ConcealmentGeometry,
        ltp_state_q14: &mut [i32],
    ) {
        let ltp_mem_length = geometry.ltp_mem_length;
        let lpc_order = a_q12.len();
        let rewhiten_start =
            ltp_mem_length.saturating_sub(geometry.initial_lag + lpc_order + LTP_HALF_ORDER);
        let mut whitened = [0i16; MAX_LTP_MEM_SAMPLES];
        lpc_analysis_filter(
            &mut whitened[rewhiten_start..ltp_mem_length],
            &output_history[rewhiten_start..ltp_mem_length],
            a_q12,
        );

        let inv_gain_q30 =
            reciprocal_q(self.prev_gain_q16[1], INV_GAIN_Q30_RECIPROCAL_Q).min(i32::MAX >> 1);
        let scale_start = rewhiten_start + lpc_order;
        ltp_state_q14[scale_start..ltp_mem_length]
            .iter_mut()
            .zip(whitened[scale_start..ltp_mem_length].iter())
            .for_each(|(state, &sample)| *state = mul_q16_i16(inv_gain_q30, sample));
        return;
    }

    /// Synthesises the concealed excitation of every subframe into `ltp_state_q14` after the
    /// rewhitened history and returns the final pitch lag: each sample is the pitch prediction of
    /// the LTP taps (voiced; an unvoiced frame keeps only the accumulator's rounding bias) plus
    /// noise from `noise_window`, and per subframe the taps and the noise scale are attenuated and
    /// the pitch lag drifts up by 1 %
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    fn synthesise_excitation(
        &mut self,
        ltp_state_q14: &mut [i32],
        noise_window: &[i32; RAND_BUF_SIZE],
        geometry: ConcealmentGeometry,
        shape: ExcitationShape,
    ) -> usize {
        let mut lag = geometry.initial_lag;
        let mut rand_seed = self.rand_seed;
        let mut rand_scale_q14 = shape.rand_scale_q14;
        let mut write = geometry.ltp_mem_length;

        for _ in 0..geometry.subframes {
            for _ in 0..geometry.subframe_samples {
                let prediction_q12 = match shape.voiced {
                    true => {
                        let history = &ltp_state_q14[..=write + LTP_HALF_ORDER - lag];
                        ltp_predict_q13(history, &self.ltp_coef_q14)
                    }
                    false => PREDICTION_ROUNDING_BIAS_Q12,
                };
                rand_seed = advance_lcg_seed(rand_seed);
                let noise_q14 =
                    noise_window[((rand_seed >> RAND_INDEX_SHIFT) & RAND_BUF_MASK) as usize];
                ltp_state_q14[write] = mul_add_q16_i16(prediction_q12, noise_q14, rand_scale_q14)
                    .wrapping_shl(Q12_TO_Q14_SHIFT);
                write += 1;
            }

            if shape.voiced {
                let attenuation_q15 = shape.harmonic_attenuation_q15 as i16 as i32;
                self.ltp_coef_q14.iter_mut().for_each(|tap_q14| {
                    *tap_q14 = ((attenuation_q15 * (*tap_q14 as i32)) >> Q15_SHIFT) as i16;
                });
            }
            rand_scale_q14 = (((rand_scale_q14 as i32)
                * (shape.noise_attenuation_q15 as i16 as i32))
                >> Q15_SHIFT) as i16;
            self.pitch_l_q8 =
                mul_add_q16_i16(self.pitch_l_q8, self.pitch_l_q8, PITCH_DRIFT_FACTOR_Q16)
                    .min(geometry.max_pitch_lag_q8);
            lag = round_shift(self.pitch_l_q8, PITCH_LAG_Q8_SHIFT) as usize;
        }

        self.rand_seed = rand_seed;
        self.rand_scale_q14 = rand_scale_q14;
        return lag;
    }

    /// Smooths the energy transition between a concealed loss burst and the first recovered
    /// good frame (RFC 6716 Section 4.4).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>
    pub(crate) fn glue_frames(&mut self, frame_pcm: &mut [i16]) {
        if frame_pcm.is_empty() {
            return;
        }

        if self.loss_count > 0 {
            let (energy, shift) = sum_sqr_shift(frame_pcm);
            self.conc_energy = energy;
            self.conc_energy_shift = shift;
            self.last_frame_lost = true;
            return;
        }

        if !self.last_frame_lost {
            return;
        }
        self.last_frame_lost = false;

        let (mut energy, energy_shift) = sum_sqr_shift(frame_pcm);
        match energy_shift.cmp(&self.conc_energy_shift) {
            std::cmp::Ordering::Greater => {
                let shift_diff = (energy_shift - self.conc_energy_shift) as u32;
                self.conc_energy = self.conc_energy.checked_shr(shift_diff).unwrap_or(0);
            }
            std::cmp::Ordering::Less => {
                let shift_diff = (self.conc_energy_shift - energy_shift) as u32;
                energy = energy.checked_shr(shift_diff).unwrap_or(0);
            }
            std::cmp::Ordering::Equal => {}
        }

        if energy <= self.conc_energy {
            return;
        }

        let leading_zeros = ((self.conc_energy as u32).leading_zeros() as i32) - 1;
        self.conc_energy = self.conc_energy.wrapping_shl(leading_zeros as u32);
        let energy_rshift = (GLUE_FRAC_Q24_SHIFT - leading_zeros).max(0) as u32;
        energy = energy.checked_shr(energy_rshift).unwrap_or(0);

        let frac_q24 = self.conc_energy / energy.max(1);
        let gain_q16 = sqrt_approx(frac_q24).wrapping_shl(GLUE_SQRT_Q12_TO_Q16_SHIFT);
        let slope_q16 = ((UNIT_GAIN_Q16 - gain_q16) / (frame_pcm.len() as i16 as i32))
            .wrapping_shl(GLUE_SLOPE_Q16_SHIFT);

        frame_pcm
            .iter_mut()
            .enumerate()
            .map(|(i, sample)| (sample, gain_q16 + (i as i32) * slope_q16))
            .take_while(|&(_, g_q16)| g_q16 <= UNIT_GAIN_Q16)
            .for_each(|(sample, g_q16)| {
                *sample = mul_q16_i16(g_q16, *sample) as i16;
            });
        return;
    }
}

/// Layout of one concealed frame: LTP history length, subframe geometry, the pitch lag the
/// excitation starts from and its upper bound in Q8
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
#[derive(Debug, Clone, Copy)]
pub(super) struct ConcealmentGeometry {
    ltp_mem_length: usize,
    subframes: usize,
    subframe_samples: usize,
    initial_lag: usize,
    max_pitch_lag_q8: i32,
}

/// Gains of the concealed excitation of one lost frame
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
#[derive(Debug, Clone, Copy)]
pub(super) struct ExcitationShape {
    /// The last decoded frame was voiced, so the LTP taps predict the excitation.
    voiced: bool,
    /// Noise scale of the first subframe in Q14.
    rand_scale_q14: i16,
    /// Per-subframe attenuation of the LTP taps in Q15.
    harmonic_attenuation_q15: i32,
    /// Per-subframe attenuation of the noise scale in Q15.
    noise_attenuation_q15: i32,
}

/// Runs the LPC synthesis filter of RFC 6716 Section 4.2.7.9.2 over the concealed excitation
/// in `excitation_and_state_q14`, starting from and updating `lpc_state_q14`, and writes the
/// gain-scaled result to `out_pcm`
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
fn synthesise_lpc_frame(
    lpc_state_q14: &mut [i32; MAX_LPC_ORDER],
    excitation_and_state_q14: &mut [i32],
    a_q12: &[i16],
    gain_q10: i32,
    out_pcm: &mut [i16],
) {
    let frame_length = out_pcm.len();
    excitation_and_state_q14[..MAX_LPC_ORDER].copy_from_slice(lpc_state_q14);
    lpc_synthesize_q14(
        &mut excitation_and_state_q14[..MAX_LPC_ORDER + frame_length],
        a_q12,
        gain_q10,
        out_pcm,
    );
    lpc_state_q14
        .copy_from_slice(&excitation_and_state_q14[frame_length..frame_length + MAX_LPC_ORDER]);
    return;
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::silk::cng::CngFrameInput;

    /// Layout of a concealed frame of `frame_len` samples in `nb_subfr` subframes with an LPC
    /// filter of `lpc_order`.
    pub(crate) fn concealment_layout(
        frame_len: usize,
        nb_subfr: usize,
        lpc_order: usize,
    ) -> SilkLayout {
        return SilkLayout {
            subframes: nb_subfr,
            subframe_samples: frame_len / nb_subfr,
            lpc_order,
            ..SilkLayout::UNCONFIGURED
        };
    }

    /// Records `frame` as decoded the way the decoder does: the frame bookkeeping of the coding
    /// references, then the concealment update.
    pub(crate) fn record_decoded(plc: &mut PlcState, ch_state: &mut ChannelState, frame: &Frame) {
        ch_state.references.finish_frame(frame.frame_type.signal_type);
        plc.update_from_decoded_frame(ch_state, frame);
    }

    /// Comfort noise view of a decoded `frame`.
    pub(crate) fn cng_input(frame: &Frame) -> CngFrameInput<'_> {
        return CngFrameInput {
            gains_q16: std::array::from_fn(|i| frame.subframes[i].gain_q16),
            exc_q14: &frame.exc_q14[..frame.sample_count],
            subframes: frame.num_subframes,
        };
    }
}

#[cfg(test)]
mod tests {
    // Regression pins: expected values recorded from libopus 1.6.1 for concealment, which
    // RFC 6716 Section 4.4 leaves to the implementation; a change here is a design decision, not
    // a conformance failure.
    use super::test_support::*;
    use super::*;
    use crate::silk::cng::CngState;
    use crate::silk::{FrameType, QuantizationOffsetType};

    /// Executes one lost-frame concealment + CNG + glue step and asserts the resulting
    /// PLC state and PCM head/tail slices against the recorded values.
    macro_rules! assert_plc_loss_step {
        (
            $plc:expr,
            $ch:expr,
            $cng:expr,
            $pcm:expr,
            frame_len: $frame_len:expr,
            nb_subfr: $nb_subfr:expr,
            lpc_order: $lpc_order:expr,
            loss_count: $loss_count:expr,
            rand_seed: $rand_seed:expr,
            rand_scale_q14: $rand_scale:expr,
            conc_energy: $conc_energy:expr,
            head: $expected_head:expr,
            tail: $expected_tail:expr
            $(, pitch_l_q8: $pitch_l_q8:expr)?
            $(, conc_energy_shift: $conc_shift:expr)?
            $(, lag_prev: $lag_prev:expr)?
            $(, s_lpc: $expected_slpc:expr)?
        ) => {{
            let layout = concealment_layout($frame_len, $nb_subfr, $lpc_order);
            $plc.conceal_lost_frame(&mut $ch, &layout, &mut $pcm[..$frame_len]);
            $cng.apply(&$ch, &$plc, None, $lpc_order, &mut $pcm[..$frame_len]).unwrap();
            $plc.glue_frames(&mut $pcm[..$frame_len]);

            assert_eq!($plc.loss_count, $loss_count);
            assert_eq!($plc.rand_seed, $rand_seed);
            assert_eq!($plc.rand_scale_q14, $rand_scale);
            assert_eq!($plc.conc_energy, $conc_energy);
            assert_eq!(&$pcm[..16], &$expected_head);
            assert_eq!(&$pcm[$frame_len - 16..$frame_len], &$expected_tail);
            $(assert_eq!($plc.pitch_l_q8, $pitch_l_q8);)?
            $(assert_eq!($plc.conc_energy_shift, $conc_shift);)?
            $(assert_eq!($ch.prev_pitch_lag, $lag_prev);)?
            $(assert_eq!(&$ch.lpc_state_q14, &$expected_slpc);)?
        }};
    }

    /// Verifies `update_from_decoded_frame`, `conceal_lost_frame`, `CngState::apply`, and
    /// `glue_frames` on a Wideband (16 kHz, 20 ms, 16-tap LPC) Voiced frame across 3 consecutive lost frames
    /// and a higher-energy recovery frame against the recorded values.
    #[test]
    fn voiced_wb_concealment_of_3_losses_and_glue_regression_pin() {
        const LTP_Q7: [[i8; LTP_ORDER]; 4] =
            [[4, 12, 62, 18, -3], [6, 15, 68, 20, -2], [5, 14, 72, 22, -4], [3, 10, 64, 16, -2]];
        const PITCH_LAGS: [u16; 4] = [74, 75, 76, 77];
        const GAINS_Q16: [i32; 4] = [3_276_800, 3_604_480, 3_932_160, 4_194_304];
        const A_Q12_WB: [i16; 16] =
            [7772, -3675, 43, -45, 264, -676, 460, -283, 541, -219, 61, -369, 301, -381, 139, 146];

        const EXPECTED_LOSS_1_HEAD: [i16; 16] = [
            326, 346, 298, 223, 170, -1007, -1043, -1066, -1078, -1075, -1068, -1051, -1023, -1000,
            -959, -904,
        ];
        const EXPECTED_LOSS_1_TAIL: [i16; 16] =
            [-122, -83, -45, -379, -334, -276, -220, -152, -75, -1, 57, 104, 112, 100, 64, 41];
        const EXPECTED_LOSS_1_SLPC: [i32; 16] = [
            -31324, -21188, -11404, -97084, -85392, -70780, -56228, -38832, -19180, -152, 14616,
            26680, 28648, 25724, 16348, 10552,
        ];

        const EXPECTED_LOSS_2_HEAD: [i16; 16] =
            [20, -233, -219, -186, -153, -125, -101, -86, -61, -33, -3, 27, 53, 94, 129, 168];
        const EXPECTED_LOSS_2_TAIL: [i16; 16] =
            [50, 67, 83, 103, 129, 144, 159, 173, 182, 130, 132, 134, 128, 126, 120, 112];
        const EXPECTED_LOSS_2_SLPC: [i32; 16] = [
            12752, 17180, 21364, 26340, 32904, 36772, 40792, 44212, 46680, 33392, 33796, 34388,
            32832, 32160, 30776, 28552,
        ];

        const EXPECTED_LOSS_3_HEAD: [i16; 16] =
            [103, 94, 73, 50, 32, 15, -8, -30, -43, -54, -88, -82, -81, -83, -78, -72];
        const EXPECTED_LOSS_3_TAIL: [i16; 16] =
            [-45, -42, -38, -35, -29, -23, -17, -10, -7, -5, -8, -5, -2, 0, -1, -1];
        const EXPECTED_LOSS_3_SLPC: [i32; 16] = [
            -11424, -10732, -9800, -8936, -7340, -5912, -4284, -2656, -1824, -1244, -1936, -1236,
            -404, 28, -336, -188,
        ];

        const EXPECTED_GLUED_HEAD: [i16; 16] = [
            -89, -127, -156, -179, -193, -200, -200, -192, -176, -153, -122, -83, -37, 16, 78, 146,
        ];
        const EXPECTED_GLUED_TAIL: [i16; 16] = [
            2583, 2894, 3205, 3516, 3827, -3863, -3552, -3241, -2930, -2619, -2308, -1997, -1686,
            -1375, -1064, -753,
        ];

        let mut ch_state = ChannelState::new();
        ch_state.set_sample_rate(16, 48_000).unwrap();
        let mut plc = PlcState::new();
        let mut cng = CngState::new();

        let mut frame = Frame::new(320, 4, 16);
        frame.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);
        frame.ltp_scale_q14 = 12_288;
        for i in 0..4 {
            frame.subframes[i].pitch_lag = PITCH_LAGS[i];
            frame.subframes[i].ltp_coeffs = LTP_Q7[i];
            frame.subframes[i].gain_q16 = GAINS_Q16[i];
            frame.subframes[i].lpc_coeffs_q12 = A_Q12_WB;
        }
        for i in 0..320 {
            frame.exc_q14[i] = (((i as i32) * 173 + 29) % 2001 - 1000) * 16;
            frame.output_pcm[i] = ((((i as i32) * 97 + 13) % 1601) - 800) as i16;
            ch_state.output_history[i] = frame.output_pcm[i];
        }
        for i in 0..16 {
            ch_state.lpc_state_q14[i] = ((i as i32) - 8) * 12_345;
        }

        let mut pcm_buf = frame.output_pcm;
        record_decoded(&mut plc, &mut ch_state, &frame);
        cng.apply(&ch_state, &plc, Some(cng_input(&frame)), 16, &mut pcm_buf[..320]).unwrap();
        plc.glue_frames(&mut pcm_buf[..320]);

        assert_eq!(plc.pitch_l_q8, 19_712);
        assert_eq!(plc.ltp_scale_q14, 12_288);
        assert_eq!(plc.prev_gain_q16, [3_932_160, 4_194_304]);
        assert_eq!(plc.ltp_coef_q14, [0, 0, 11_648, 0, 0]);
        assert_eq!(plc.prev_lpc_q12, A_Q12_WB);

        assert_plc_loss_step!(
            plc, ch_state, cng, pcm_buf,
            frame_len: 320, nb_subfr: 4, lpc_order: 16,
            loss_count: 1, rand_seed: -1_783_179_200, rand_scale_q14: 2_891, conc_energy: 74_254_257,
            head: EXPECTED_LOSS_1_HEAD, tail: EXPECTED_LOSS_1_TAIL,
            pitch_l_q8: 20_509, conc_energy_shift: 0, lag_prev: 80, s_lpc: EXPECTED_LOSS_1_SLPC
        );
        assert_plc_loss_step!(
            plc, ch_state, cng, pcm_buf,
            frame_len: 320, nb_subfr: 4, lpc_order: 16,
            loss_count: 2, rand_seed: -451_448_704, rand_scale_q14: 1_183, conc_energy: 5_530_189,
            head: EXPECTED_LOSS_2_HEAD, tail: EXPECTED_LOSS_2_TAIL,
            pitch_l_q8: 21_340, conc_energy_shift: 0, lag_prev: 83, s_lpc: EXPECTED_LOSS_2_SLPC
        );
        assert_plc_loss_step!(
            plc, ch_state, cng, pcm_buf,
            frame_len: 320, nb_subfr: 4, lpc_order: 16,
            loss_count: 3, rand_seed: -765_343_552, rand_scale_q14: 483, conc_energy: 890_239,
            head: EXPECTED_LOSS_3_HEAD, tail: EXPECTED_LOSS_3_TAIL,
            pitch_l_q8: 22_204, conc_energy_shift: 0, lag_prev: 87, s_lpc: EXPECTED_LOSS_3_SLPC
        );

        // Recovery frame with higher energy triggers glue_frames ramp
        for (i, sample) in pcm_buf[..320].iter_mut().enumerate() {
            *sample = ((((i as i32) * 311 + 50) % 8001) - 4000) as i16;
        }
        record_decoded(&mut plc, &mut ch_state, &frame);
        cng.apply(&ch_state, &plc, Some(cng_input(&frame)), 16, &mut pcm_buf[..320]).unwrap();
        plc.glue_frames(&mut pcm_buf[..320]);

        assert!(!plc.last_frame_lost);
        assert_eq!(plc.conc_energy, 1_823_203_328);
        assert_eq!(&pcm_buf[..16], &EXPECTED_GLUED_HEAD);
        assert_eq!(&pcm_buf[320 - 16..320], &EXPECTED_GLUED_TAIL);
    }

    /// Verifies `update_from_decoded_frame`, `conceal_lost_frame`, and `glue_frames` on a
    /// Narrowband (8 kHz, 20 ms, 10-tap LPC) Unvoiced frame across 3 consecutive lost frames
    /// against the recorded values.
    #[test]
    fn unvoiced_nb_concealment_of_3_losses_regression_pin() {
        const GAINS_Q16: [i32; 4] = [2_097_152, 1_835_008, 1_572_864, 1_310_720];
        const A_Q12_NB: [i16; 10] = [1545, 716, 1663, -2049, 2207, -1542, 1286, -805, 595, -332];

        const EXPECTED_LOSS_1_HEAD: [i16; 16] =
            [1, 8, 4, 18, 12, 8, 9, 10, 15, 9, 20, 16, 12, 18, 0, -3];
        const EXPECTED_LOSS_1_TAIL: [i16; 16] =
            [-6, 7, 6, 3, 2, 0, -3, 7, -7, 7, 16, -4, 21, 1, 5, 13];

        const EXPECTED_LOSS_2_HEAD: [i16; 16] =
            [-19, -2, -21, -15, 0, -23, 11, -13, -11, 0, -14, 10, -8, -1, 4, -9];
        const EXPECTED_LOSS_2_TAIL: [i16; 16] =
            [-13, 1, -17, 1, -1, -5, 8, -18, 2, -7, -9, -1, -14, 11, -14, -3];

        const EXPECTED_LOSS_3_HEAD: [i16; 16] =
            [-3, -17, 9, -17, -1, -8, -15, 0, -18, 1, -15, -20, -2, -27, -3, -18];
        const EXPECTED_LOSS_3_TAIL: [i16; 16] =
            [1, -8, -6, -2, -3, 3, -3, -2, 0, -3, -5, -6, 1, -2, -2, -2];

        let mut ch_state = ChannelState::new();
        ch_state.set_sample_rate(8, 48_000).unwrap();
        let mut plc = PlcState::new();
        let mut cng = CngState::new();

        let mut frame = Frame::new(160, 4, 10);
        frame.frame_type = FrameType::new(SignalType::Unvoiced, QuantizationOffsetType::High);
        for (i, &gain) in GAINS_Q16.iter().enumerate() {
            frame.subframes[i].gain_q16 = gain;
            frame.subframes[i].lpc_coeffs_q12[..10].copy_from_slice(&A_Q12_NB);
        }
        for i in 0..160 {
            let scale = if (80..120).contains(&i) { 4 } else { 24 };
            frame.exc_q14[i] = (((i as i32) * 251 + 43) % 1001 - 500) * scale;
            frame.output_pcm[i] = ((((i as i32) * 67 + 19) % 801) - 400) as i16;
            ch_state.output_history[i] = frame.output_pcm[i];
        }
        for i in 0..10 {
            ch_state.lpc_state_q14[i] = (5 - (i as i32)) * 8192;
        }

        let mut pcm_buf = [0i16; 160];
        pcm_buf.copy_from_slice(&frame.output_pcm[..160]);
        record_decoded(&mut plc, &mut ch_state, &frame);
        cng.apply(&ch_state, &plc, Some(cng_input(&frame)), 10, &mut pcm_buf).unwrap();
        plc.glue_frames(&mut pcm_buf);

        assert_eq!(plc.pitch_l_q8, 36_864);
        assert_eq!(plc.ltp_scale_q14, 0);
        assert_eq!(plc.prev_gain_q16, [1_572_864, 1_310_720]);
        assert_eq!(ch_state.prev_pitch_lag, 0);

        assert_plc_loss_step!(
            plc, ch_state, cng, pcm_buf,
            frame_len: 160, nb_subfr: 4, lpc_order: 10,
            loss_count: 1, rand_seed: -932_563_936, rand_scale_q14: 15_736, conc_energy: 16_240,
            head: EXPECTED_LOSS_1_HEAD, tail: EXPECTED_LOSS_1_TAIL,
            pitch_l_q8: 36_864, lag_prev: 144
        );
        assert_plc_loss_step!(
            plc, ch_state, cng, pcm_buf,
            frame_len: 160, nb_subfr: 4, lpc_order: 10,
            loss_count: 2, rand_seed: -1_783_179_200, rand_scale_q14: 10_322, conc_energy: 20_320,
            head: EXPECTED_LOSS_2_HEAD, tail: EXPECTED_LOSS_2_TAIL
        );
        assert_plc_loss_step!(
            plc, ch_state, cng, pcm_buf,
            frame_len: 160, nb_subfr: 4, lpc_order: 10,
            loss_count: 3, rand_seed: 443_411_552, rand_scale_q14: 6_770, conc_energy: 9_662,
            head: EXPECTED_LOSS_3_HEAD, tail: EXPECTED_LOSS_3_TAIL
        );
    }
}
