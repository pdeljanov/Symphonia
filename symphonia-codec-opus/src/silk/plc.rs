//! SILK Packet Loss Concealment (PLC) and Comfort Noise Generation (CNG)
//!
//! Implements stateful time-domain packet loss concealment, post-loss energy gluing,
//! post-loss LPC bandwidth expansion, and comfort noise estimation and synthesis for the
//! SILK LP layer (RFC 6716 Sections 4.2.7.9 and 4.2.7.10).
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
//!
//! <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.10>

use symphonia_core::errors::Result;

use crate::silk::decoder::Decoder;
use crate::silk::{
    ChannelState, Frame, LTP_ORDER, MAX_FRAME_SAMPLES, MAX_LPC_ORDER, MAX_LTP_MEM_SAMPLES,
    MAX_SUBFRAME_SAMPLES, MAX_SUBFRAMES, SignalType, add_sat32, inverse32_var_q, lshift_sat32,
    mla_q16_i16, mul_q16, mul_q16_i16, rshift_round, sat16,
};

/// Number of loss-stage attenuation entries (`loss_count == 0` vs `loss_count >= 1`).
const NUM_ATTENUATION_STAGES: usize = 2;

/// Harmonic (voiced LTP) attenuation factors in Q15 (`0.99` and `0.95`).
const HARMONIC_ATTENUATION_Q15: [i16; NUM_ATTENUATION_STAGES] = [32_440, 31_130];

/// Random excitation attenuation factors in Q15 for voiced concealment (`0.95` and `0.80`).
const VOICED_NOISE_ATTENUATION_Q15: [i16; NUM_ATTENUATION_STAGES] = [31_130, 26_214];

/// Random excitation attenuation factors in Q15 for unvoiced concealment (`0.99` and `0.90`).
const UNVOICED_NOISE_ATTENUATION_Q15: [i16; NUM_ATTENUATION_STAGES] = [32_440, 29_491];

/// LPC chirp bandwidth expansion factor applied per lost frame (`0.99` in Q16).
const PLC_BWE_CHIRP_Q16: i32 = 64_881;

/// LPC chirp bandwidth expansion factor applied to the first valid frame after loss (`0.97` in Q16).
pub const BWE_AFTER_LOSS_Q16: i32 = 63_570;

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

/// Linear Congruential Generator (LCG) multiplier (`silk_RAND`).
const LCG_MULTIPLIER: i32 = 196_314_165;

/// Linear Congruential Generator (LCG) additive increment (`silk_RAND`).
const LCG_INCREMENT: i32 = 907_633_515;

/// Maximum mask for indexing the comfort noise excitation buffer (`255`).
const CNG_BUF_MASK_MAX: usize = 255;

/// Comfort noise subframe gain smoothing factor in Q16 (`0.25^(1/4)`).
const CNG_GAIN_SMOOTH_Q16: i16 = 4_634;

/// Comfort noise rapid-adaptation gain threshold in Q16 (`-3 dB`).
const CNG_GAIN_SMOOTH_THRESHOLD_Q16: i32 = 46_396;

/// Comfort noise NLSF smoothing factor in Q16 (`0.25`).
const CNG_NLSF_SMOOTH_Q16: i16 = 16_348;

/// Initial pseudo-random LCG seed for Comfort Noise Generation (`3176576`).
const CNG_INITIAL_RAND_SEED: i32 = 3_176_576;

/// Advances the SILK 32-bit Linear Congruential Generator (`silk_RAND`).
#[inline(always)]
fn advance_lcg_seed(seed: i32) -> i32 {
    return seed.wrapping_mul(LCG_MULTIPLIER).wrapping_add(LCG_INCREMENT);
}

/// Approximates the integer square root of `x` (`silk_SQRT_APPROX` in `silk/Inlines.h`).
///
/// Extracts the leading-zero count and the 7 fractional bits immediately following the
/// most significant set bit, then refines a base `2^(lz/2)` estimate linearly.
#[inline]
pub(crate) fn sqrt_approx(x: i32) -> i32 {
    if x <= 0 {
        return 0;
    }

    let leading_zeros = (x as u32).leading_zeros() as i32;
    let frac_q7 = ((x as u32).rotate_right((24 - leading_zeros) as u32) & 0x7F) as i32;

    let base = if (leading_zeros & 1) != 0 {
        32_768
    } else {
        46_214
    };
    let scaled = base >> (leading_zeros >> 1);

    return mla_q16_i16(scaled, scaled, (213 * frac_q7) as i16);
}

/// Applies progressive chirp bandwidth expansion (`chirp_q16`) in-place to Q12 LPC
/// coefficients (`silk_bwexpander` in `silk/bwexpander.c`).
///
/// Uses rounded `32 x 16` products rather than floor-truncated `mul_q16_i16` to avoid
/// bias that could destabilize high-order synthesis filters.
pub(crate) fn chirp_bandwidth_expand(coeffs_q12: &mut [i16], mut factor_q16: i32) {
    let Some((last_coeff, head_coeffs)) = coeffs_q12.split_last_mut() else {
        return;
    };

    let step_q16 = factor_q16 - 65_536;
    for coeff in head_coeffs.iter_mut() {
        *coeff = rshift_round(factor_q16 * (*coeff as i32), 16) as i16;
        factor_q16 += rshift_round(factor_q16 * step_q16, 16);
    }
    *last_coeff = rshift_round(factor_q16 * (*last_coeff as i32), 16) as i16;
    return;
}

/// Computes a right-shifted sum of squares of `samples` that fits into a signed 32-bit
/// integer with two bits of headroom (`silk_sum_sqr_shift` in `silk/sum_sqr_shift.c`).
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

/// Per-channel SILK Packet Loss Concealment (PLC) state (`silk_PLC_struct` in `silk/structs.h`).
///
/// Tracks voiced pitch lag, simplified single-tap LTP gain, previous LPC synthesis
/// coefficients, subframe quantization gains, noise excitation history, and concealed
/// frame energy across packet loss bursts (RFC 6716 Section 4.2.7.10).
#[derive(Debug, Clone, Copy)]
pub struct PlcState {
    pub pitch_l_q8: i32,
    pub ltp_coef_q14: [i16; LTP_ORDER],
    pub ltp_scale_q14: i16,
    pub prev_gain_q16: [i32; 2],
    pub rand_seed: i32,
    pub rand_scale_q14: i16,
    pub conc_energy: i32,
    pub conc_energy_shift: i32,
    pub prev_lpc_q12: [i16; MAX_LPC_ORDER],
    pub prev_exc_q14: [i32; MAX_FRAME_SAMPLES],
    pub subfr_length: usize,
    pub nb_subfr: usize,
    pub fs_khz: u32,
    pub loss_count: usize,
    pub last_frame_lost: bool,
}

impl Default for PlcState {
    fn default() -> Self {
        return Self::new();
    }
}

impl PlcState {
    /// Creates a new `PlcState` initialized to default pre-reset values.
    pub const fn new() -> Self {
        return Self {
            pitch_l_q8: 0,
            ltp_coef_q14: [0; LTP_ORDER],
            ltp_scale_q14: 0,
            prev_gain_q16: [1 << 16, 1 << 16],
            rand_seed: 0,
            rand_scale_q14: 0,
            conc_energy: 0,
            conc_energy_shift: 0,
            prev_lpc_q12: [0; MAX_LPC_ORDER],
            prev_exc_q14: [0; MAX_FRAME_SAMPLES],
            subfr_length: 20,
            nb_subfr: 2,
            fs_khz: 0,
            loss_count: 0,
            last_frame_lost: false,
        };
    }

    /// Resets the entire PLC state and sets initial pitch lag for `frame_length`
    /// (`silk_reset_decoder` + `silk_PLC_Reset`).
    pub fn reset(&mut self, frame_length: usize) {
        *self = Self::new();
        self.reset_frame_parameters(frame_length);
        return;
    }

    /// Resets only the frame-dependent pitch lag, unit gains, and default subframe geometry
    /// when the internal SILK sample rate changes (`silk_PLC_Reset` in `silk/PLC.c`).
    pub fn reset_frame_parameters(&mut self, frame_length: usize) {
        self.pitch_l_q8 = (frame_length as i32) << 7;
        self.prev_gain_q16 = [1 << 16, 1 << 16];
        self.subfr_length = 20;
        self.nb_subfr = 2;
        return;
    }

    /// Synchronizes `self.fs_khz` with the channel's internal sample rate, resetting
    /// frame parameters if the rate changed (`silk/PLC.c:84-87`).
    #[inline]
    pub fn ensure_sample_rate(&mut self, fs_khz: u32, frame_length: usize) {
        if self.fs_khz != fs_khz {
            self.reset_frame_parameters(frame_length);
            self.fs_khz = fs_khz;
        }
        return;
    }

    /// Applies post-loss bandwidth expansion (`BWE_AFTER_LOSS_Q16`) to all active subframe
    /// LPC coefficient sets when recovering from one or more lost frames
    /// (`silk/decode_parameters.c:81-84`).
    pub fn expand_lpc_after_loss(&self, frame: &mut Frame) {
        if self.loss_count == 0 {
            return;
        }

        let lpc_order = frame.lpc_order.min(MAX_LPC_ORDER);
        for subframe in frame.active_subframes_mut() {
            chirp_bandwidth_expand(&mut subframe.lpc_coeffs_q12[..lpc_order], BWE_AFTER_LOSS_Q16);
        }
        return;
    }

    /// Updates the PLC state from a validly decoded SILK frame (`silk_PLC_update` in `silk/PLC.c`).
    ///
    /// For voiced frames, scans backward from the final subframe across the pitch period to
    /// locate the subframe with maximum summed LTP gain, collapses the 5-tap LTP filter into a
    /// single center tap clamped to `[0.70, 0.95]` in Q14, and records its pitch lag in Q8.
    /// For unvoiced or inactive frames, sets the pitch lag to `18 ms * fs_khz` and zeroes the
    /// LTP filter.
    pub fn update_from_decoded_frame(&mut self, ch_state: &mut ChannelState, frame: &Frame) {
        let frame_length = frame.sample_count.min(MAX_FRAME_SAMPLES);
        let nb_subfr = frame.num_subframes.clamp(2, MAX_SUBFRAMES);
        let subfr_length = (frame_length / nb_subfr).min(MAX_SUBFRAME_SAMPLES);
        let lpc_order = frame.lpc_order.min(MAX_LPC_ORDER);
        let fs_khz = ch_state.fs_khz as u32;

        self.ensure_sample_rate(fs_khz, frame_length);

        let signal_type = frame.frame_type.signal_type;
        ch_state.prev_signal_type = signal_type;

        if signal_type == SignalType::Voiced {
            self.select_voiced_pitch_and_ltp_gain(frame, subfr_length, nb_subfr);
            self.ltp_scale_q14 = frame.ltp_scale_q14;
            ch_state.lag_prev = frame.subframes[nb_subfr - 1].pitch_lag as i32;
        } else {
            self.pitch_l_q8 = ((fs_khz as i32) * MAX_PITCH_LAG_MS) << 8;
            self.ltp_coef_q14 = [0; LTP_ORDER];
            self.ltp_scale_q14 = 0;
            ch_state.lag_prev = 0;
        }

        self.prev_lpc_q12[..lpc_order]
            .copy_from_slice(&frame.subframes[nb_subfr - 1].lpc_coeffs_q12[..lpc_order]);
        self.prev_gain_q16 = [
            frame.subframes[nb_subfr - 2].gain_q16,
            frame.subframes[nb_subfr - 1].gain_q16,
        ];
        self.prev_exc_q14[..frame_length].copy_from_slice(&frame.exc_q14[..frame_length]);
        self.subfr_length = subfr_length;
        self.nb_subfr = nb_subfr;
        self.loss_count = 0;
        ch_state.first_frame_after_reset = false;
        return;
    }

    /// Selects the voiced pitch lag and clamped center-tap LTP coefficient from the last
    /// pitch period of `frame` (`silk/PLC.c:133-175`).
    fn select_voiced_pitch_and_ltp_gain(
        &mut self,
        frame: &Frame,
        subfr_length: usize,
        nb_subfr: usize,
    ) {
        let last_pitch_lag = frame.subframes[nb_subfr - 1].pitch_lag as usize;
        let mut max_ltp_gain_q14 = 0i32;

        for j in 0..nb_subfr {
            if j * subfr_length >= last_pitch_lag {
                break;
            }
            let subfr = &frame.subframes[nb_subfr - 1 - j];
            let gain_q14: i32 = subfr.ltp_coeffs.iter().map(|&c| (c as i32) << 7).sum();
            if gain_q14 > max_ltp_gain_q14 {
                max_ltp_gain_q14 = gain_q14;
                self.pitch_l_q8 = (subfr.pitch_lag as i32) << 8;
            }
        }

        self.ltp_coef_q14 = [0; LTP_ORDER];
        self.ltp_coef_q14[LTP_ORDER / 2] = max_ltp_gain_q14 as i16;

        if max_ltp_gain_q14 < VOICED_PITCH_GAIN_MIN_Q14 {
            let scale_q10 = (VOICED_PITCH_GAIN_MIN_Q14 << 10) / max_ltp_gain_q14.max(1);
            for coeff in self.ltp_coef_q14.iter_mut() {
                *coeff = (((*coeff as i32) * (scale_q10 as i16 as i32)) >> 10) as i16;
            }
        } else if max_ltp_gain_q14 > VOICED_PITCH_GAIN_MAX_Q14 {
            let scale_q14 = (VOICED_PITCH_GAIN_MAX_Q14 << 14) / max_ltp_gain_q14.max(1);
            for coeff in self.ltp_coef_q14.iter_mut() {
                *coeff = (((*coeff as i32) * (scale_q14 as i16 as i32)) >> 14) as i16;
            }
        }
        return;
    }

    /// Synthesizes a concealed time-domain PCM signal for a lost SILK frame and updates
    /// `ch_state.s_lpc_q14_buf` and `ch_state.out_buf` (`silk_PLC_conceal` in `silk/PLC.c`).
    pub fn conceal_lost_frame(
        &mut self,
        ch_state: &mut ChannelState,
        frame_length: usize,
        nb_subfr: usize,
        lpc_order: usize,
        out_pcm: &mut [i16],
    ) {
        let frame_length = frame_length.min(MAX_FRAME_SAMPLES).min(out_pcm.len());
        let nb_subfr = nb_subfr.clamp(2, MAX_SUBFRAMES);
        let subfr_length = (frame_length / nb_subfr).min(MAX_SUBFRAME_SAMPLES);
        let lpc_order = lpc_order.min(MAX_LPC_ORDER);
        let fs_khz = ch_state.fs_khz as u32;

        self.ensure_sample_rate(fs_khz, frame_length);

        if ch_state.first_frame_after_reset {
            self.prev_lpc_q12.fill(0);
        }

        let prev_gain_q10 = [self.prev_gain_q16[0] >> 6, self.prev_gain_q16[1] >> 6];
        let noise_offset =
            self.select_noise_excitation_offset(prev_gain_q10, subfr_length, nb_subfr);
        let mut noise_window = [0i32; RAND_BUF_SIZE];
        let copy_len = RAND_BUF_SIZE.min(MAX_FRAME_SAMPLES.saturating_sub(noise_offset));
        noise_window[..copy_len]
            .copy_from_slice(&self.prev_exc_q14[noise_offset..noise_offset + copy_len]);

        let att_stage = self.loss_count.min(NUM_ATTENUATION_STAGES - 1);
        let harmonic_attenuation_q15 = HARMONIC_ATTENUATION_Q15[att_stage] as i32;
        let base_noise_attenuation_q15 = if ch_state.prev_signal_type == SignalType::Voiced {
            VOICED_NOISE_ATTENUATION_Q15[att_stage] as i32
        } else {
            UNVOICED_NOISE_ATTENUATION_Q15[att_stage] as i32
        };

        chirp_bandwidth_expand(&mut self.prev_lpc_q12[..lpc_order], PLC_BWE_CHIRP_Q16);
        let mut a_q12 = [0i16; MAX_LPC_ORDER];
        a_q12[..lpc_order].copy_from_slice(&self.prev_lpc_q12[..lpc_order]);

        let (initial_rand_scale_q14, noise_attenuation_q15) = self
            .compute_initial_noise_scale_and_attenuation(
                ch_state.prev_signal_type,
                lpc_order,
                base_noise_attenuation_q15,
            );

        let initial_lag = rshift_round(self.pitch_l_q8, 8) as usize;
        let ltp_mem_length = ch_state.ltp_mem_length.min(MAX_LTP_MEM_SAMPLES);
        let mut s_ltp_q14 = [0i32; MAX_LTP_MEM_SAMPLES + MAX_FRAME_SAMPLES];

        self.rewhiten_ltp_history(
            &ch_state.out_buf,
            &a_q12[..lpc_order],
            initial_lag,
            ltp_mem_length,
            lpc_order,
            &mut s_ltp_q14,
        );

        let final_lag = if ch_state.prev_signal_type == SignalType::Voiced {
            self.synthesise_voiced_excitation(
                &mut s_ltp_q14,
                &noise_window,
                ltp_mem_length,
                subfr_length,
                nb_subfr,
                fs_khz,
                harmonic_attenuation_q15,
                noise_attenuation_q15,
                initial_lag,
                initial_rand_scale_q14,
            )
        } else {
            self.synthesise_unvoiced_excitation(
                &mut s_ltp_q14,
                &noise_window,
                ltp_mem_length,
                subfr_length,
                nb_subfr,
                fs_khz,
                noise_attenuation_q15,
                initial_lag,
                initial_rand_scale_q14,
            )
        };

        let lpc_slice_start = ltp_mem_length - MAX_LPC_ORDER;
        self.synthesise_lpc_frame(
            &mut ch_state.s_lpc_q14_buf,
            &mut s_ltp_q14[lpc_slice_start..lpc_slice_start + MAX_LPC_ORDER + frame_length],
            &a_q12[..lpc_order],
            prev_gain_q10[1],
            frame_length,
            &mut out_pcm[..frame_length],
        );

        let mv_len = ltp_mem_length.saturating_sub(frame_length);
        if mv_len > 0 {
            ch_state
                .out_buf
                .copy_within(frame_length..frame_length + mv_len, 0);
        }
        ch_state.out_buf[mv_len..mv_len + frame_length]
            .copy_from_slice(&out_pcm[..frame_length]);

        ch_state.lag_prev = final_lag as i32;
        self.loss_count += 1;
        return;
    }

    /// Compares scaled excitation energy across the last two subframes and returns the
    /// starting offset of the 128-sample random noise window (`silk_PLC_energy` in `silk/PLC.c`).
    fn select_noise_excitation_offset(
        &self,
        prev_gain_q10: [i32; 2],
        subfr_length: usize,
        nb_subfr: usize,
    ) -> usize {
        let mut scaled_exc = [0i16; 2 * MAX_SUBFRAME_SAMPLES];

        for (k, &gain_q10) in prev_gain_q10.iter().enumerate() {
            let src_start = (k + nb_subfr - 2) * subfr_length;
            let dst_start = k * subfr_length;
            for (dst, &exc_q14) in scaled_exc[dst_start..dst_start + subfr_length]
                .iter_mut()
                .zip(self.prev_exc_q14[src_start..src_start + subfr_length].iter())
            {
                *dst = sat16(mul_q16(exc_q14, gain_q10) >> 8);
            }
        }

        let (energy1, shift1) = sum_sqr_shift(&scaled_exc[..subfr_length]);
        let (energy2, shift2) = sum_sqr_shift(&scaled_exc[subfr_length..2 * subfr_length]);

        let end_subframe = if (energy1 >> shift2) < (energy2 >> shift1) {
            self.nb_subfr.saturating_sub(1)
        } else {
            self.nb_subfr
        };

        return (end_subframe * self.subfr_length).saturating_sub(RAND_BUF_SIZE);
    }

    /// Computes the initial random noise scale (`rand_scale_q14`) and adjusted noise
    /// attenuation (`noise_attenuation_q15`) at the start of a packet loss burst (`silk/PLC.c:289-311`).
    fn compute_initial_noise_scale_and_attenuation(
        &self,
        signal_type: SignalType,
        lpc_order: usize,
        base_noise_attenuation_q15: i32,
    ) -> (i16, i32) {
        if self.loss_count > 0 {
            return (self.rand_scale_q14, base_noise_attenuation_q15);
        }

        let mut rand_scale_q14: i16 = 1 << 14;
        let mut noise_attenuation_q15 = base_noise_attenuation_q15;

        if signal_type == SignalType::Voiced {
            for &tap_q14 in self.ltp_coef_q14.iter() {
                rand_scale_q14 = rand_scale_q14.wrapping_sub(tap_q14);
            }
            rand_scale_q14 = rand_scale_q14.max(VOICED_MIN_RAND_SCALE_Q14);
            rand_scale_q14 =
                (((rand_scale_q14 as i32) * (self.ltp_scale_q14 as i32)) >> 14) as i16;
        } else {
            let inv_gain_q30 = Decoder::lpc_inverse_pred_gain(&self.prev_lpc_q12[..lpc_order]);
            let high_limit_q30 = (1i32 << 30) >> LOG2_INV_LPC_GAIN_HIGH_THRES;
            let low_limit_q30 = (1i32 << 30) >> LOG2_INV_LPC_GAIN_LOW_THRES;
            let down_scale_q30 = inv_gain_q30.clamp(low_limit_q30, high_limit_q30)
                << LOG2_INV_LPC_GAIN_HIGH_THRES;
            noise_attenuation_q15 =
                mul_q16_i16(down_scale_q30, noise_attenuation_q15 as i16) >> 14;
        }

        return (rand_scale_q14, noise_attenuation_q15);
    }

    /// Rewhitens the tail of `out_buf` through the LPC analysis filter `a_q12` and scales
    /// it by the inverse of `prev_gain_q16[1]` into `s_ltp_q14` (`silk/PLC.c:317-326`).
    fn rewhiten_ltp_history(
        &self,
        out_buf: &[i16],
        a_q12: &[i16],
        lag: usize,
        ltp_mem_length: usize,
        lpc_order: usize,
        s_ltp_q14: &mut [i32],
    ) {
        let mut s_ltp = [0i16; MAX_LTP_MEM_SAMPLES];
        let rewhiten_start = ltp_mem_length.saturating_sub(lag + lpc_order + (LTP_ORDER / 2));

        Decoder::lpc_analysis_filter(
            &mut s_ltp[rewhiten_start..ltp_mem_length],
            &out_buf[rewhiten_start..ltp_mem_length],
            a_q12,
            lpc_order,
        );

        let inv_gain_q30 = inverse32_var_q(self.prev_gain_q16[1], 46).min(i32::MAX >> 1);
        let scale_start = rewhiten_start + lpc_order;
        for (dst, &whitened) in s_ltp_q14[scale_start..ltp_mem_length]
            .iter_mut()
            .zip(s_ltp[scale_start..ltp_mem_length].iter())
        {
            *dst = mul_q16_i16(inv_gain_q30, whitened);
        }
        return;
    }

    /// Synthesizes voiced PLC excitation across all subframes by combining 5-tap LTP
    /// pitch prediction with attenuated pseudo-random excitation samples (`silk/PLC.c:331-363`).
    #[allow(clippy::too_many_arguments)]
    fn synthesise_voiced_excitation(
        &mut self,
        s_ltp_q14: &mut [i32],
        noise_window: &[i32; RAND_BUF_SIZE],
        ltp_mem_length: usize,
        subfr_length: usize,
        nb_subfr: usize,
        fs_khz: u32,
        harmonic_attenuation_q15: i32,
        noise_attenuation_q15: i32,
        initial_lag: usize,
        initial_rand_scale_q14: i16,
    ) -> usize {
        let mut s_ltp_buf_idx = ltp_mem_length;
        let mut lag = initial_lag;
        let mut rand_seed = self.rand_seed;
        let mut rand_scale_q14 = initial_rand_scale_q14;
        let max_pitch_lag_q8 = (MAX_PITCH_LAG_MS * (fs_khz as i32)) << 8;

        for _subfr in 0..nb_subfr {
            let pred_lag_base = s_ltp_buf_idx + (LTP_ORDER / 2) - lag;
            for i in 0..subfr_length {
                let pred_idx = pred_lag_base + i;
                let mut ltp_pred_q12 = 2i32;
                for (tap, &coeff_q14) in self.ltp_coef_q14.iter().enumerate() {
                    ltp_pred_q12 =
                        mla_q16_i16(ltp_pred_q12, s_ltp_q14[pred_idx - tap], coeff_q14);
                }

                rand_seed = advance_lcg_seed(rand_seed);
                let noise_idx = ((rand_seed >> 25) & RAND_BUF_MASK) as usize;
                s_ltp_q14[s_ltp_buf_idx] =
                    mla_q16_i16(ltp_pred_q12, noise_window[noise_idx], rand_scale_q14)
                        .wrapping_shl(2);
                s_ltp_buf_idx += 1;
            }

            for coeff_q14 in self.ltp_coef_q14.iter_mut() {
                *coeff_q14 =
                    (((harmonic_attenuation_q15 as i16 as i32) * (*coeff_q14 as i32)) >> 15) as i16;
            }
            rand_scale_q14 =
                (((rand_scale_q14 as i32) * (noise_attenuation_q15 as i16 as i32)) >> 15) as i16;

            self.pitch_l_q8 = mla_q16_i16(self.pitch_l_q8, self.pitch_l_q8, PITCH_DRIFT_FACTOR_Q16)
                .min(max_pitch_lag_q8);
            lag = rshift_round(self.pitch_l_q8, 8) as usize;
        }

        self.rand_seed = rand_seed;
        self.rand_scale_q14 = rand_scale_q14;
        return lag;
    }

    /// Synthesizes unvoiced PLC excitation across all subframes using attenuated
    /// pseudo-random samples from the lowest-energy historical subframe (`silk/PLC.c:331-363`).
    #[allow(clippy::too_many_arguments)]
    fn synthesise_unvoiced_excitation(
        &mut self,
        s_ltp_q14: &mut [i32],
        noise_window: &[i32; RAND_BUF_SIZE],
        ltp_mem_length: usize,
        subfr_length: usize,
        nb_subfr: usize,
        fs_khz: u32,
        noise_attenuation_q15: i32,
        initial_lag: usize,
        initial_rand_scale_q14: i16,
    ) -> usize {
        let mut s_ltp_buf_idx = ltp_mem_length;
        let mut lag = initial_lag;
        let mut rand_seed = self.rand_seed;
        let mut rand_scale_q14 = initial_rand_scale_q14;
        let max_pitch_lag_q8 = (MAX_PITCH_LAG_MS * (fs_khz as i32)) << 8;

        for _subfr in 0..nb_subfr {
            for _i in 0..subfr_length {
                rand_seed = advance_lcg_seed(rand_seed);
                let noise_idx = ((rand_seed >> 25) & RAND_BUF_MASK) as usize;
                // Preserve the Q12 rounding bias of 2 from the zero-tap LTP accumulator.
                s_ltp_q14[s_ltp_buf_idx] =
                    mla_q16_i16(2, noise_window[noise_idx], rand_scale_q14).wrapping_shl(2);
                s_ltp_buf_idx += 1;
            }

            rand_scale_q14 =
                (((rand_scale_q14 as i32) * (noise_attenuation_q15 as i16 as i32)) >> 15) as i16;

            self.pitch_l_q8 = mla_q16_i16(self.pitch_l_q8, self.pitch_l_q8, PITCH_DRIFT_FACTOR_Q16)
                .min(max_pitch_lag_q8);
            lag = rshift_round(self.pitch_l_q8, 8) as usize;
        }

        self.rand_seed = rand_seed;
        self.rand_scale_q14 = rand_scale_q14;
        return lag;
    }

    /// Runs short-term all-pole LPC synthesis filtering over `excitation_and_state_q14`
    /// and updates `s_lpc_q14_buf` (`silk/PLC.c:368-420`).
    fn synthesise_lpc_frame(
        &self,
        s_lpc_q14_buf: &mut [i32; MAX_LPC_ORDER],
        excitation_and_state_q14: &mut [i32],
        a_q12: &[i16],
        gain_q10: i32,
        frame_length: usize,
        out_pcm: &mut [i16],
    ) {
        let lpc_order = a_q12.len();
        excitation_and_state_q14[..MAX_LPC_ORDER].copy_from_slice(s_lpc_q14_buf);

        for (i, out_sample) in out_pcm[..frame_length].iter_mut().enumerate() {
            let mut lpc_pred_q10 = (lpc_order >> 1) as i32;
            for (tap, &coeff_q12) in a_q12.iter().enumerate() {
                lpc_pred_q10 = mla_q16_i16(
                    lpc_pred_q10,
                    excitation_and_state_q14[MAX_LPC_ORDER + i - 1 - tap],
                    coeff_q12,
                );
            }

            let synthesized_q14 = add_sat32(
                excitation_and_state_q14[MAX_LPC_ORDER + i],
                lshift_sat32(lpc_pred_q10, 4),
            );
            excitation_and_state_q14[MAX_LPC_ORDER + i] = synthesized_q14;
            *out_sample = sat16(rshift_round(mul_q16(synthesized_q14, gain_q10), 8));
        }

        s_lpc_q14_buf
            .copy_from_slice(&excitation_and_state_q14[frame_length..frame_length + MAX_LPC_ORDER]);
        return;
    }

    /// Smooths the energy transition between a concealed loss burst and the first recovered
    /// good frame (`silk_PLC_glue_frames` in `silk/PLC.c`).
    pub fn glue_frames(&mut self, frame_pcm: &mut [i16]) {
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

        if self.last_frame_lost {
            let (mut energy, energy_shift) = sum_sqr_shift(frame_pcm);

            if energy_shift > self.conc_energy_shift {
                let shift_diff = (energy_shift - self.conc_energy_shift) as u32;
                self.conc_energy = if shift_diff < 32 {
                    self.conc_energy >> shift_diff
                } else {
                    0
                };
            } else if energy_shift < self.conc_energy_shift {
                let shift_diff = (self.conc_energy_shift - energy_shift) as u32;
                energy = if shift_diff < 32 {
                    energy >> shift_diff
                } else {
                    0
                };
            }

            if energy > self.conc_energy {
                let leading_zeros = ((self.conc_energy as u32).leading_zeros() as i32) - 1;
                self.conc_energy = self.conc_energy.wrapping_shl(leading_zeros as u32);
                let energy_rshift = (24 - leading_zeros).max(0) as u32;
                energy = if energy_rshift < 32 {
                    energy >> energy_rshift
                } else {
                    0
                };

                let frac_q24 = self.conc_energy / energy.max(1);
                let mut gain_q16 = sqrt_approx(frac_q24).wrapping_shl(4);
                let slope_q16 =
                    (((1i32 << 16) - gain_q16) / (frame_pcm.len() as i16 as i32)).wrapping_shl(2);

                for sample in frame_pcm.iter_mut() {
                    *sample = mul_q16_i16(gain_q16, *sample) as i16;
                    gain_q16 += slope_q16;
                    if gain_q16 > (1i32 << 16) {
                        break;
                    }
                }
            }
        }

        self.last_frame_lost = false;
        return;
    }
}

/// Per-channel SILK Comfort Noise Generation (CNG) state (`silk_CNG_struct` in `silk/structs.h`).
///
/// Tracks exponentially smoothed Normalized Line Spectral Frequencies (NLSFs), smoothed
/// subframe gain, historical inactive excitation samples, and LPC synthesis filter state
/// across DTX and packet-loss intervals (RFC 6716 Section 4.2.7.10).
#[derive(Debug, Clone, Copy)]
pub struct CngState {
    pub cng_exc_buf_q14: [i32; MAX_FRAME_SAMPLES],
    pub cng_smth_nlsf_q15: [i16; MAX_LPC_ORDER],
    pub cng_synth_state: [i32; MAX_LPC_ORDER],
    pub cng_smth_gain_q16: i32,
    pub rand_seed: i32,
    pub fs_khz: u32,
}

impl Default for CngState {
    fn default() -> Self {
        return Self::new();
    }
}

impl CngState {
    /// Creates a new `CngState` with uniformly spaced initial NLSFs and the RFC 6716 LCG seed.
    pub const fn new() -> Self {
        let step_q15 = (i16::MAX as i32) / ((MAX_LPC_ORDER as i32) + 1);
        let mut smth_nlsf_q15 = [0i16; MAX_LPC_ORDER];
        let mut i = 0;
        while i < MAX_LPC_ORDER {
            smth_nlsf_q15[i] = (((i as i32) + 1) * step_q15) as i16;
            i += 1;
        }

        return Self {
            cng_exc_buf_q14: [0; MAX_FRAME_SAMPLES],
            cng_smth_nlsf_q15: smth_nlsf_q15,
            cng_synth_state: [0; MAX_LPC_ORDER],
            cng_smth_gain_q16: 0,
            rand_seed: CNG_INITIAL_RAND_SEED,
            fs_khz: 0,
        };
    }

    /// Resets the comfort noise state (`silk_CNG_Reset` in `silk/CNG.c`).
    pub fn reset(&mut self) {
        let lpc_order = if self.fs_khz == 8 || self.fs_khz == 12 {
            10
        } else {
            MAX_LPC_ORDER
        };
        *self = Self::new();
        self.reset_for_lpc_order(lpc_order);
        return;
    }

    /// Resets smoothed NLSFs, smoothed gain, and LCG seed for the given `lpc_order`
    /// (`silk_CNG_Reset` in `silk/CNG.c`).
    pub fn reset_for_lpc_order(&mut self, lpc_order: usize) {
        let order = lpc_order.clamp(1, MAX_LPC_ORDER);
        let step_q15 = (i16::MAX as i32) / ((order as i32) + 1);
        let mut acc_q15 = 0i32;
        for nlsf in self.cng_smth_nlsf_q15[..order].iter_mut() {
            acc_q15 += step_q15;
            *nlsf = acc_q15 as i16;
        }
        self.cng_smth_gain_q16 = 0;
        self.rand_seed = CNG_INITIAL_RAND_SEED;
        return;
    }

    /// Synchronizes `self.fs_khz` with the channel's internal sample rate, resetting
    /// smoothed NLSFs and gain when the rate changes (`silk/CNG.c:92-97`).
    #[inline]
    pub fn ensure_sample_rate(&mut self, fs_khz: u32, lpc_order: usize) {
        if self.fs_khz != fs_khz {
            self.reset_for_lpc_order(lpc_order);
            self.fs_khz = fs_khz;
        }
        return;
    }

    /// Updates smoothed NLSFs, excitation history, and smoothed gain from a valid
    /// inactive (`SignalType::Inactive`) SILK frame (`silk/CNG.c:98-126`).
    pub fn update_comfort_noise(
        &mut self,
        prev_nlsf_q15: &[i16],
        subframe_gains_q16: &[i32],
        exc_q14: &[i32],
        subfr_length: usize,
        nb_subfr: usize,
        lpc_order: usize,
    ) {
        let order = lpc_order.min(MAX_LPC_ORDER).min(prev_nlsf_q15.len());
        for (smth_nlsf, &target_nlsf) in self.cng_smth_nlsf_q15[..order]
            .iter_mut()
            .zip(prev_nlsf_q15[..order].iter())
        {
            let delta = (target_nlsf as i32) - (*smth_nlsf as i32);
            *smth_nlsf = (*smth_nlsf).wrapping_add(mul_q16_i16(delta, CNG_NLSF_SMOOTH_Q16) as i16);
        }

        let active_subfr = nb_subfr.min(subframe_gains_q16.len());
        let mut max_gain_q16 = 0i32;
        let mut max_subfr = 0usize;
        for (idx, &gain_q16) in subframe_gains_q16[..active_subfr].iter().enumerate() {
            if gain_q16 > max_gain_q16 {
                max_gain_q16 = gain_q16;
                max_subfr = idx;
            }
        }

        if active_subfr > 0 && subfr_length > 0 {
            let move_samples = (active_subfr - 1) * subfr_length;
            if move_samples > 0 && move_samples + subfr_length <= MAX_FRAME_SAMPLES {
                self.cng_exc_buf_q14
                    .copy_within(0..move_samples, subfr_length);
            }
            let src_start = max_subfr * subfr_length;
            if src_start + subfr_length <= exc_q14.len() && subfr_length <= MAX_FRAME_SAMPLES {
                self.cng_exc_buf_q14[..subfr_length]
                    .copy_from_slice(&exc_q14[src_start..src_start + subfr_length]);
            }
        }

        for &subfr_gain_q16 in subframe_gains_q16[..active_subfr].iter() {
            let gain_diff = subfr_gain_q16.wrapping_sub(self.cng_smth_gain_q16);
            self.cng_smth_gain_q16 = self
                .cng_smth_gain_q16
                .wrapping_add(mul_q16_i16(gain_diff, CNG_GAIN_SMOOTH_Q16));
            if mul_q16(self.cng_smth_gain_q16, CNG_GAIN_SMOOTH_THRESHOLD_Q16) > subfr_gain_q16 {
                self.cng_smth_gain_q16 = subfr_gain_q16;
            }
        }
        return;
    }

    /// Generates pseudo-random comfort noise excitation by randomly indexing into
    /// `self.cng_exc_buf_q14` using a power-of-two mask (`silk_CNG_exc` in `silk/CNG.c`).
    pub fn generate_excitation(&mut self, out_exc_q14: &mut [i32]) {
        let length = out_exc_q14.len();
        let mut exc_mask = CNG_BUF_MASK_MAX;
        while exc_mask > length {
            exc_mask >>= 1;
        }

        let mut seed = self.rand_seed;
        for dst in out_exc_q14.iter_mut() {
            seed = advance_lcg_seed(seed);
            let idx = ((seed >> 24) as usize) & exc_mask;
            *dst = self.cng_exc_buf_q14[idx];
        }
        self.rand_seed = seed;
        return;
    }

    /// Computes the Q10 comfort noise synthesis gain from the smoothed CNG gain and
    /// the current PLC random noise gain (`silk/CNG.c:134-144`).
    fn compute_comfort_noise_gain_q10(&self, plc_state: &PlcState) -> i32 {
        let plc_gain_q16 = mul_q16(plc_state.rand_scale_q14 as i32, plc_state.prev_gain_q16[1]);

        let cng_gain_q16 = if plc_gain_q16 >= (1 << 21) || self.cng_smth_gain_q16 > (1 << 23) {
            let plc_gain_sq = (plc_gain_q16 >> 16) * (plc_gain_q16 >> 16);
            let smth_gain_sq = (self.cng_smth_gain_q16 >> 16) * (self.cng_smth_gain_q16 >> 16);
            let diff = smth_gain_sq.wrapping_sub(plc_gain_sq.wrapping_shl(5));
            sqrt_approx(diff).wrapping_shl(16)
        } else {
            let plc_gain_sq = mul_q16(plc_gain_q16, plc_gain_q16);
            let smth_gain_sq = mul_q16(self.cng_smth_gain_q16, self.cng_smth_gain_q16);
            let diff = smth_gain_sq.wrapping_sub(plc_gain_sq.wrapping_shl(5));
            sqrt_approx(diff).wrapping_shl(8)
        };

        return cng_gain_q16 >> 6;
    }

    /// Synthesizes comfort noise through the smoothed NLSF-derived LPC filter and
    /// saturating-adds it to `frame_pcm` during packet loss or DTX (`silk/CNG.c:129-183`).
    pub fn generate_comfort_noise(
        &mut self,
        plc_state: &PlcState,
        lpc_order: usize,
        frame_pcm: &mut [i16],
    ) -> Result<()> {
        let length = frame_pcm.len().min(MAX_FRAME_SAMPLES);
        let order = lpc_order.min(MAX_LPC_ORDER);
        let gain_q10 = self.compute_comfort_noise_gain_q10(plc_state);

        let mut cng_sig_q14 = [0i32; MAX_LPC_ORDER + MAX_FRAME_SAMPLES];
        cng_sig_q14[..MAX_LPC_ORDER].copy_from_slice(&self.cng_synth_state);
        self.generate_excitation(&mut cng_sig_q14[MAX_LPC_ORDER..MAX_LPC_ORDER + length]);

        let a_q12 = Decoder::lsf_to_lpc(&self.cng_smth_nlsf_q15[..order])?;

        for (i, sample) in frame_pcm[..length].iter_mut().enumerate() {
            let mut lpc_pred_q10 = (order >> 1) as i32;
            for (tap, &coeff_q12) in a_q12[..order].iter().enumerate() {
                lpc_pred_q10 = mla_q16_i16(
                    lpc_pred_q10,
                    cng_sig_q14[MAX_LPC_ORDER + i - 1 - tap],
                    coeff_q12,
                );
            }

            let synth_q14 = add_sat32(
                cng_sig_q14[MAX_LPC_ORDER + i],
                lshift_sat32(lpc_pred_q10, 4),
            );
            cng_sig_q14[MAX_LPC_ORDER + i] = synth_q14;

            let cng_sample = sat16(rshift_round(mul_q16(synth_q14, gain_q10), 8));
            *sample = sample.saturating_add(cng_sample);
        }

        self.cng_synth_state
            .copy_from_slice(&cng_sig_q14[length..length + MAX_LPC_ORDER]);
        return Ok(());
    }

    /// Orchestrates Comfort Noise Generation for a decoded or lost SILK frame (`silk_CNG` in `silk/CNG.c`).
    ///
    /// - When `plc_state.loss_count == 0` and `ch_state.prev_signal_type == SignalType::Inactive`,
    ///   updates smoothed NLSFs, excitation history, and smoothed gain from `frame`.
    /// - When `plc_state.loss_count > 0`, synthesizes comfort noise and adds it in-place to `frame_pcm`.
    /// - When `plc_state.loss_count == 0`, clears the CNG synthesis filter memory.
    pub fn apply(
        &mut self,
        ch_state: &ChannelState,
        plc_state: &PlcState,
        frame: Option<&Frame>,
        lpc_order: usize,
        frame_pcm: &mut [i16],
    ) -> Result<()> {
        let order = lpc_order.min(MAX_LPC_ORDER);
        self.ensure_sample_rate(ch_state.fs_khz as u32, order);

        if plc_state.loss_count == 0 && ch_state.prev_signal_type == SignalType::Inactive {
            if let Some(decoded_frame) = frame {
                let nb_subfr = decoded_frame.num_subframes.min(MAX_SUBFRAMES);
                let subfr_length = decoded_frame
                    .sample_count
                    .checked_div(nb_subfr)
                    .unwrap_or(0)
                    .min(MAX_SUBFRAME_SAMPLES);
                let gains_q16: [i32; MAX_SUBFRAMES] =
                    std::array::from_fn(|k| decoded_frame.subframes[k].gain_q16);
                self.update_comfort_noise(
                    &ch_state.prev_nlsf_q15[..order],
                    &gains_q16[..nb_subfr],
                    &decoded_frame.exc_q14[..decoded_frame.sample_count.min(MAX_FRAME_SAMPLES)],
                    subfr_length,
                    nb_subfr,
                    order,
                );
            }
        }

        if plc_state.loss_count > 0 {
            self.generate_comfort_noise(plc_state, order, frame_pcm)?;
        } else {
            self.cng_synth_state[..order].fill(0);
        }

        return Ok(());
    }
}

/// Updates `plc_state` and `ch_state` after a valid decoded SILK frame
/// (`silk_PLC_update` in `silk/PLC.c`).
pub fn silk_plc_update(plc_state: &mut PlcState, ch_state: &mut ChannelState, frame: &Frame) {
    plc_state.update_from_decoded_frame(ch_state, frame);
    return;
}

/// Generates a concealed time-domain `i16` signal in `out_pcm` for a lost SILK frame
/// and updates `ch_state.s_lpc_q14_buf` and `ch_state.out_buf` (`silk_PLC` in `silk/PLC.c`).
pub fn silk_plc_conceal(
    plc_state: &mut PlcState,
    ch_state: &mut ChannelState,
    frame_length: usize,
    nb_subfr: usize,
    lpc_order: usize,
    out_pcm: &mut [i16],
) {
    plc_state.conceal_lost_frame(ch_state, frame_length, nb_subfr, lpc_order, out_pcm);
    return;
}

/// Smooths the energy transition when recovering from packet loss (`silk_PLC_glue_frames` in `silk/PLC.c`).
pub fn silk_plc_glue_frames(plc_state: &mut PlcState, frame_pcm: &mut [i16]) {
    plc_state.glue_frames(frame_pcm);
    return;
}

/// Updates comfort noise parameters during valid inactive frames and synthesizes comfort
/// noise during lost or DTX frames (`silk_CNG` in `silk/CNG.c`).
pub fn silk_cng(
    cng_state: &mut CngState,
    ch_state: &ChannelState,
    plc_state: &PlcState,
    frame: Option<&Frame>,
    lpc_order: usize,
    frame_pcm: &mut [i16],
) -> Result<()> {
    return cng_state.apply(ch_state, plc_state, frame, lpc_order, frame_pcm);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::silk::{FrameType, QuantizationOffsetType};

    /// Verifies `silk_plc_update`, `silk_plc_conceal`, `silk_cng`, and `silk_plc_glue_frames`
    /// on a WideBand (16 kHz, 20 ms, 16-tap LPC) Voiced frame across 3 consecutive lost frames
    /// and a higher-energy recovery frame against the `libopus 1.6.1` fixed-point C oracle.
    #[test]
    fn silk_plc_voiced_wb_update_conceal_3_losses_and_glue_match_libopus() {
        const LTP_Q7: [[i8; LTP_ORDER]; 4] = [
            [4, 12, 62, 18, -3],
            [6, 15, 68, 20, -2],
            [5, 14, 72, 22, -4],
            [3, 10, 64, 16, -2],
        ];
        const PITCH_LAGS: [u16; 4] = [74, 75, 76, 77];
        const GAINS_Q16: [i32; 4] = [3_276_800, 3_604_480, 3_932_160, 4_194_304];
        const A_Q12_WB: [i16; 16] = [
            7772, -3675, 43, -45, 264, -676, 460, -283, 541, -219, 61, -369, 301, -381, 139, 146,
        ];

        const EXPECTED_LOSS_1_HEAD: [i16; 16] = [
            326, 346, 298, 223, 170, -1007, -1043, -1066, -1078, -1075, -1068, -1051, -1023, -1000,
            -959, -904,
        ];
        const EXPECTED_LOSS_1_TAIL: [i16; 16] = [
            -122, -83, -45, -379, -334, -276, -220, -152, -75, -1, 57, 104, 112, 100, 64, 41,
        ];
        const EXPECTED_LOSS_1_SLPC: [i32; 16] = [
            -31324, -21188, -11404, -97084, -85392, -70780, -56228, -38832, -19180, -152, 14616,
            26680, 28648, 25724, 16348, 10552,
        ];

        const EXPECTED_LOSS_2_HEAD: [i16; 16] = [
            20, -233, -219, -186, -153, -125, -101, -86, -61, -33, -3, 27, 53, 94, 129, 168,
        ];
        const EXPECTED_LOSS_2_TAIL: [i16; 16] = [
            50, 67, 83, 103, 129, 144, 159, 173, 182, 130, 132, 134, 128, 126, 120, 112,
        ];
        const EXPECTED_LOSS_2_SLPC: [i32; 16] = [
            12752, 17180, 21364, 26340, 32904, 36772, 40792, 44212, 46680, 33392, 33796, 34388,
            32832, 32160, 30776, 28552,
        ];

        const EXPECTED_LOSS_3_HEAD: [i16; 16] = [
            103, 94, 73, 50, 32, 15, -8, -30, -43, -54, -88, -82, -81, -83, -78, -72,
        ];
        const EXPECTED_LOSS_3_TAIL: [i16; 16] = [
            -45, -42, -38, -35, -29, -23, -17, -10, -7, -5, -8, -5, -2, 0, -1, -1,
        ];
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
        let mut plc_state = PlcState::new();
        let mut cng_state = CngState::new();

        let mut frame = Frame::new(320, 4, 16);
        frame.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);
        frame.ltp_scale_q14 = 12_288;
        for k in 0..4 {
            frame.subframes[k].pitch_lag = PITCH_LAGS[k];
            frame.subframes[k].ltp_coeffs = LTP_Q7[k];
            frame.subframes[k].gain_q16 = GAINS_Q16[k];
            frame.subframes[k].lpc_coeffs_q12 = A_Q12_WB;
        }
        for i in 0..320 {
            frame.exc_q14[i] = (((i as i32) * 173 + 29) % 2001 - 1000) * 16;
            frame.output_pcm[i] = ((((i as i32) * 97 + 13) % 1601) - 800) as i16;
            ch_state.out_buf[i] = frame.output_pcm[i];
        }
        for i in 0..16 {
            ch_state.s_lpc_q14_buf[i] = ((i as i32) - 8) * 12_345;
        }

        let mut pcm_buf = frame.output_pcm;
        silk_plc_update(&mut plc_state, &mut ch_state, &frame);
        silk_cng(
            &mut cng_state,
            &ch_state,
            &plc_state,
            Some(&frame),
            16,
            &mut pcm_buf[..320],
        )
        .unwrap();
        silk_plc_glue_frames(&mut plc_state, &mut pcm_buf[..320]);

        assert_eq!(plc_state.pitch_l_q8, 19_712);
        assert_eq!(plc_state.ltp_scale_q14, 12_288);
        assert_eq!(plc_state.prev_gain_q16, [3_932_160, 4_194_304]);
        assert_eq!(plc_state.ltp_coef_q14, [0, 0, 11_648, 0, 0]);
        assert_eq!(plc_state.prev_lpc_q12, A_Q12_WB);

        // Lost frame 1
        silk_plc_conceal(&mut plc_state, &mut ch_state, 320, 4, 16, &mut pcm_buf[..320]);
        silk_cng(&mut cng_state, &ch_state, &plc_state, None, 16, &mut pcm_buf[..320]).unwrap();
        silk_plc_glue_frames(&mut plc_state, &mut pcm_buf[..320]);

        assert_eq!(plc_state.loss_count, 1);
        assert_eq!(plc_state.pitch_l_q8, 20_509);
        assert_eq!(plc_state.rand_seed, -1_783_179_200);
        assert_eq!(plc_state.rand_scale_q14, 2_891);
        assert_eq!(plc_state.conc_energy, 74_254_257);
        assert_eq!(plc_state.conc_energy_shift, 0);
        assert_eq!(ch_state.lag_prev, 80);
        assert_eq!(&pcm_buf[..16], &EXPECTED_LOSS_1_HEAD);
        assert_eq!(&pcm_buf[320 - 16..320], &EXPECTED_LOSS_1_TAIL);
        assert_eq!(&ch_state.s_lpc_q14_buf, &EXPECTED_LOSS_1_SLPC);

        // Lost frame 2
        silk_plc_conceal(&mut plc_state, &mut ch_state, 320, 4, 16, &mut pcm_buf[..320]);
        silk_cng(&mut cng_state, &ch_state, &plc_state, None, 16, &mut pcm_buf[..320]).unwrap();
        silk_plc_glue_frames(&mut plc_state, &mut pcm_buf[..320]);

        assert_eq!(plc_state.loss_count, 2);
        assert_eq!(plc_state.pitch_l_q8, 21_340);
        assert_eq!(plc_state.rand_seed, -451_448_704);
        assert_eq!(plc_state.rand_scale_q14, 1_183);
        assert_eq!(plc_state.conc_energy, 5_530_189);
        assert_eq!(plc_state.conc_energy_shift, 0);
        assert_eq!(ch_state.lag_prev, 83);
        assert_eq!(&pcm_buf[..16], &EXPECTED_LOSS_2_HEAD);
        assert_eq!(&pcm_buf[320 - 16..320], &EXPECTED_LOSS_2_TAIL);
        assert_eq!(&ch_state.s_lpc_q14_buf, &EXPECTED_LOSS_2_SLPC);

        // Lost frame 3
        silk_plc_conceal(&mut plc_state, &mut ch_state, 320, 4, 16, &mut pcm_buf[..320]);
        silk_cng(&mut cng_state, &ch_state, &plc_state, None, 16, &mut pcm_buf[..320]).unwrap();
        silk_plc_glue_frames(&mut plc_state, &mut pcm_buf[..320]);

        assert_eq!(plc_state.loss_count, 3);
        assert_eq!(plc_state.pitch_l_q8, 22_204);
        assert_eq!(plc_state.rand_seed, -765_343_552);
        assert_eq!(plc_state.rand_scale_q14, 483);
        assert_eq!(plc_state.conc_energy, 890_239);
        assert_eq!(plc_state.conc_energy_shift, 0);
        assert_eq!(ch_state.lag_prev, 87);
        assert_eq!(&pcm_buf[..16], &EXPECTED_LOSS_3_HEAD);
        assert_eq!(&pcm_buf[320 - 16..320], &EXPECTED_LOSS_3_TAIL);
        assert_eq!(&ch_state.s_lpc_q14_buf, &EXPECTED_LOSS_3_SLPC);

        // Recovery frame with higher energy triggers glue_frames ramp
        for (i, sample) in pcm_buf[..320].iter_mut().enumerate() {
            *sample = ((((i as i32) * 311 + 50) % 8001) - 4000) as i16;
        }
        silk_plc_update(&mut plc_state, &mut ch_state, &frame);
        silk_cng(
            &mut cng_state,
            &ch_state,
            &plc_state,
            Some(&frame),
            16,
            &mut pcm_buf[..320],
        )
        .unwrap();
        silk_plc_glue_frames(&mut plc_state, &mut pcm_buf[..320]);

        assert!(!plc_state.last_frame_lost);
        assert_eq!(plc_state.conc_energy, 1_823_203_328);
        assert_eq!(&pcm_buf[..16], &EXPECTED_GLUED_HEAD);
        assert_eq!(&pcm_buf[320 - 16..320], &EXPECTED_GLUED_TAIL);
    }

    /// Verifies `silk_plc_update`, `silk_plc_conceal`, and `silk_plc_glue_frames` on a
    /// NarrowBand (8 kHz, 20 ms, 10-tap LPC) Unvoiced frame across 3 consecutive lost frames
    /// against the `libopus 1.6.1` fixed-point C oracle.
    #[test]
    fn silk_plc_unvoiced_nb_update_and_conceal_3_losses_match_libopus() {
        const GAINS_Q16: [i32; 4] = [2_097_152, 1_835_008, 1_572_864, 1_310_720];
        const A_Q12_NB: [i16; 10] = [1545, 716, 1663, -2049, 2207, -1542, 1286, -805, 595, -332];

        const EXPECTED_LOSS_1_HEAD: [i16; 16] =
            [1, 8, 4, 18, 12, 8, 9, 10, 15, 9, 20, 16, 12, 18, 0, -3];
        const EXPECTED_LOSS_1_TAIL: [i16; 16] =
            [-6, 7, 6, 3, 2, 0, -3, 7, -7, 7, 16, -4, 21, 1, 5, 13];

        const EXPECTED_LOSS_2_HEAD: [i16; 16] = [
            -19, -2, -21, -15, 0, -23, 11, -13, -11, 0, -14, 10, -8, -1, 4, -9,
        ];
        const EXPECTED_LOSS_2_TAIL: [i16; 16] = [
            -13, 1, -17, 1, -1, -5, 8, -18, 2, -7, -9, -1, -14, 11, -14, -3,
        ];

        const EXPECTED_LOSS_3_HEAD: [i16; 16] = [
            -3, -17, 9, -17, -1, -8, -15, 0, -18, 1, -15, -20, -2, -27, -3, -18,
        ];
        const EXPECTED_LOSS_3_TAIL: [i16; 16] =
            [1, -8, -6, -2, -3, 3, -3, -2, 0, -3, -5, -6, 1, -2, -2, -2];

        let mut ch_state = ChannelState::new();
        ch_state.set_sample_rate(8, 48_000).unwrap();
        let mut plc_state = PlcState::new();
        let mut cng_state = CngState::new();

        let mut frame = Frame::new(160, 4, 10);
        frame.frame_type = FrameType::new(SignalType::Unvoiced, QuantizationOffsetType::High);
        for (k, &gain) in GAINS_Q16.iter().enumerate() {
            frame.subframes[k].gain_q16 = gain;
            frame.subframes[k].lpc_coeffs_q12[..10].copy_from_slice(&A_Q12_NB);
        }
        for i in 0..160 {
            let scale = if (80..120).contains(&i) { 4 } else { 24 };
            frame.exc_q14[i] = (((i as i32) * 251 + 43) % 1001 - 500) * scale;
            frame.output_pcm[i] = ((((i as i32) * 67 + 19) % 801) - 400) as i16;
            ch_state.out_buf[i] = frame.output_pcm[i];
        }
        for i in 0..10 {
            ch_state.s_lpc_q14_buf[i] = (5 - (i as i32)) * 8192;
        }

        let mut pcm_buf = [0i16; 160];
        pcm_buf.copy_from_slice(&frame.output_pcm[..160]);
        silk_plc_update(&mut plc_state, &mut ch_state, &frame);
        silk_cng(
            &mut cng_state,
            &ch_state,
            &plc_state,
            Some(&frame),
            10,
            &mut pcm_buf,
        )
        .unwrap();
        silk_plc_glue_frames(&mut plc_state, &mut pcm_buf);

        assert_eq!(plc_state.pitch_l_q8, 36_864);
        assert_eq!(plc_state.ltp_scale_q14, 0);
        assert_eq!(plc_state.prev_gain_q16, [1_572_864, 1_310_720]);
        assert_eq!(ch_state.lag_prev, 0);

        // Lost frame 1
        silk_plc_conceal(&mut plc_state, &mut ch_state, 160, 4, 10, &mut pcm_buf);
        silk_cng(&mut cng_state, &ch_state, &plc_state, None, 10, &mut pcm_buf).unwrap();
        silk_plc_glue_frames(&mut plc_state, &mut pcm_buf);

        assert_eq!(plc_state.loss_count, 1);
        assert_eq!(plc_state.pitch_l_q8, 36_864);
        assert_eq!(plc_state.rand_seed, -932_563_936);
        assert_eq!(plc_state.rand_scale_q14, 15_736);
        assert_eq!(plc_state.conc_energy, 16_240);
        assert_eq!(ch_state.lag_prev, 144);
        assert_eq!(&pcm_buf[..16], &EXPECTED_LOSS_1_HEAD);
        assert_eq!(&pcm_buf[160 - 16..160], &EXPECTED_LOSS_1_TAIL);

        // Lost frame 2
        silk_plc_conceal(&mut plc_state, &mut ch_state, 160, 4, 10, &mut pcm_buf);
        silk_cng(&mut cng_state, &ch_state, &plc_state, None, 10, &mut pcm_buf).unwrap();
        silk_plc_glue_frames(&mut plc_state, &mut pcm_buf);

        assert_eq!(plc_state.loss_count, 2);
        assert_eq!(plc_state.rand_seed, -1_783_179_200);
        assert_eq!(plc_state.rand_scale_q14, 10_322);
        assert_eq!(plc_state.conc_energy, 20_320);
        assert_eq!(&pcm_buf[..16], &EXPECTED_LOSS_2_HEAD);
        assert_eq!(&pcm_buf[160 - 16..160], &EXPECTED_LOSS_2_TAIL);

        // Lost frame 3
        silk_plc_conceal(&mut plc_state, &mut ch_state, 160, 4, 10, &mut pcm_buf);
        silk_cng(&mut cng_state, &ch_state, &plc_state, None, 10, &mut pcm_buf).unwrap();
        silk_plc_glue_frames(&mut plc_state, &mut pcm_buf);

        assert_eq!(plc_state.loss_count, 3);
        assert_eq!(plc_state.rand_seed, 443_411_552);
        assert_eq!(plc_state.rand_scale_q14, 6_770);
        assert_eq!(plc_state.conc_energy, 9_662);
        assert_eq!(&pcm_buf[..16], &EXPECTED_LOSS_3_HEAD);
        assert_eq!(&pcm_buf[160 - 16..160], &EXPECTED_LOSS_3_TAIL);
    }

    /// Verifies `silk_cng` parameter smoothing across consecutive inactive frames
    /// (including the -3 dB rapid gain adaptation threshold) and comfort noise synthesis
    /// across consecutive DTX / lost frames against `libopus 1.6.1`.
    #[test]
    fn silk_cng_smoothing_and_dtx_synthesis_match_libopus() {
        const NLSF_F0: [i16; 10] = [
            1547, 4508, 7814, 10871, 14261, 17641, 21363, 25184, 26336, 26339,
        ];
        const NLSF_F1: [i16; 10] = [
            1800, 4800, 8000, 11200, 14500, 17900, 21500, 25000, 26800, 28500,
        ];

        const EXPECTED_SMTH_NLSF_1: [i16; 10] = [
            2621, 5594, 8654, 11652, 14733, 17811, 20974, 24163, 26685, 28921,
        ];
        const EXPECTED_EXC_BUF_HEAD_1: [i32; 16] = [
            4768, 8960, -6080, -1888, 2304, 6496, -8544, -4352, -160, 4032, 8224, -6816, -2624,
            1568, 5760, -9280,
        ];

        const EXPECTED_SMTH_NLSF_2: [i16; 10] = [
            2416, 5395, 8490, 11539, 14674, 17833, 21105, 24371, 26713, 28815,
        ];
        const EXPECTED_EXC_BUF_HEAD_2: [i32; 16] = [
            4676, -3948, 1456, 6860, -1764, 3640, -4984, 420, 5824, -2800, 2604, -6020, -616,
            4788, -3836, 1568,
        ];

        const EXPECTED_DTX_1_HEAD: [i16; 16] =
            [1, 0, 0, 1, 0, 0, 0, 0, 1, -2, 0, 1, -2, -1, -2, -2];
        const EXPECTED_DTX_1_TAIL: [i16; 16] =
            [-1, -2, 0, -5, -2, -1, -2, 3, -1, 2, 1, -3, 4, 0, 0, 3];
        const EXPECTED_SYNTH_STATE_1: [i32; 10] =
            [5548, 1536, 3504, -2032, 1328, 16, -80, 1056, 6220, -1808];

        const EXPECTED_DTX_2_HEAD: [i16; 16] =
            [-2, 3, 0, -2, 1, -3, -1, -2, -3, 2, -2, 0, -1, -3, 2, -2];
        const EXPECTED_DTX_2_TAIL: [i16; 16] =
            [0, -1, 0, 1, -2, 2, 0, -1, 1, -3, 1, -2, 0, 1, -4, 2];
        const EXPECTED_SYNTH_STATE_2: [i32; 10] =
            [1552, 7152, 2688, 208, 5708, 5424, 1472, 1296, 2704, 1396];

        let mut ch_state = ChannelState::new();
        ch_state.set_sample_rate(8, 48_000).unwrap();
        let mut plc_state = PlcState::new();
        let mut cng_state = CngState::new();

        // Inactive frame 1
        let mut frame1 = Frame::new(160, 4, 10);
        frame1.frame_type = FrameType::new(SignalType::Inactive, QuantizationOffsetType::Low);
        let a_f0 = Decoder::lsf_to_lpc(&NLSF_F0).unwrap();
        ch_state.prev_nlsf_q15[..10].copy_from_slice(&NLSF_F0);
        let gains_f0 = [655_360, 983_040, 786_432, 524_288];
        for (k, &gain) in gains_f0.iter().enumerate() {
            frame1.subframes[k].gain_q16 = gain;
            frame1.subframes[k].lpc_coeffs_q12 = a_f0;
        }
        for i in 0..160 {
            frame1.exc_q14[i] = (((i as i32) * 131 + 17) % 601 - 300) * 32;
            frame1.output_pcm[i] = ((((i as i32) * 37 + 11) % 201) - 100) as i16;
            ch_state.out_buf[i] = frame1.output_pcm[i];
        }

        let mut pcm_buf = [0i16; 160];
        pcm_buf.copy_from_slice(&frame1.output_pcm[..160]);
        silk_plc_update(&mut plc_state, &mut ch_state, &frame1);
        silk_cng(
            &mut cng_state,
            &ch_state,
            &plc_state,
            Some(&frame1),
            10,
            &mut pcm_buf,
        )
        .unwrap();
        silk_plc_glue_frames(&mut plc_state, &mut pcm_buf);

        assert_eq!(cng_state.cng_smth_gain_q16, 185_963);
        assert_eq!(cng_state.rand_seed, 3_176_576);
        assert_eq!(cng_state.fs_khz, 8);
        assert_eq!(&cng_state.cng_smth_nlsf_q15[..10], &EXPECTED_SMTH_NLSF_1);
        assert_eq!(&cng_state.cng_exc_buf_q14[..16], &EXPECTED_EXC_BUF_HEAD_1);

        // Inactive frame 2 (triggers -3 dB rapid gain adaptation)
        ch_state.prev_nlsf_q15[..10].copy_from_slice(&NLSF_F1);
        let gains_f1 = [262_144, 196_608, 327_680, 294_912];
        for (k, &gain) in gains_f1.iter().enumerate() {
            frame1.subframes[k].gain_q16 = gain;
        }
        for i in 0..160 {
            frame1.exc_q14[i] = (((i as i32) * 193 + 7) % 501 - 250) * 28;
        }

        silk_plc_update(&mut plc_state, &mut ch_state, &frame1);
        silk_cng(
            &mut cng_state,
            &ch_state,
            &plc_state,
            Some(&frame1),
            10,
            &mut pcm_buf,
        )
        .unwrap();
        silk_plc_glue_frames(&mut plc_state, &mut pcm_buf);

        assert_eq!(cng_state.cng_smth_gain_q16, 207_949);
        assert_eq!(&cng_state.cng_smth_nlsf_q15[..10], &EXPECTED_SMTH_NLSF_2);
        assert_eq!(&cng_state.cng_exc_buf_q14[..16], &EXPECTED_EXC_BUF_HEAD_2);

        // DTX / Lost frame 1
        silk_plc_conceal(&mut plc_state, &mut ch_state, 160, 4, 10, &mut pcm_buf);
        silk_cng(&mut cng_state, &ch_state, &plc_state, None, 10, &mut pcm_buf).unwrap();
        silk_plc_glue_frames(&mut plc_state, &mut pcm_buf);

        assert_eq!(cng_state.rand_seed, 502_295_712);
        assert_eq!(&pcm_buf[..16], &EXPECTED_DTX_1_HEAD);
        assert_eq!(&pcm_buf[160 - 16..160], &EXPECTED_DTX_1_TAIL);
        assert_eq!(&cng_state.cng_synth_state[..10], &EXPECTED_SYNTH_STATE_1);

        // DTX / Lost frame 2
        silk_plc_conceal(&mut plc_state, &mut ch_state, 160, 4, 10, &mut pcm_buf);
        silk_cng(&mut cng_state, &ch_state, &plc_state, None, 10, &mut pcm_buf).unwrap();
        silk_plc_glue_frames(&mut plc_state, &mut pcm_buf);

        assert_eq!(cng_state.rand_seed, -726_478_656);
        assert_eq!(&pcm_buf[..16], &EXPECTED_DTX_2_HEAD);
        assert_eq!(&pcm_buf[160 - 16..160], &EXPECTED_DTX_2_TAIL);
        assert_eq!(&cng_state.cng_synth_state[..10], &EXPECTED_SYNTH_STATE_2);
    }

    /// Verifies voiced LTP gain clamping bounds (`< 11469` and `> 15565`) and high-gain
    /// WideBand CNG synthesis (`cng_smth_gain_q16 > (1 << 23)`) against `libopus 1.6.1`.
    #[test]
    fn silk_plc_ltp_clamping_and_wb_high_gain_cng_match_libopus() {
        let mut ch_state = ChannelState::new();
        ch_state.set_sample_rate(16, 48_000).unwrap();
        let mut plc_state = PlcState::new();

        let mut frame = Frame::new(320, 4, 16);
        frame.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);
        frame.ltp_scale_q14 = 15_565;
        for k in 0..4 {
            frame.subframes[k].pitch_lag = 60;
            frame.subframes[k].ltp_coeffs = [0, 0, 20, 0, 0];
            frame.subframes[k].gain_q16 = 65_536;
        }
        silk_plc_update(&mut plc_state, &mut ch_state, &frame);
        assert_eq!(plc_state.pitch_l_q8, 15_360);
        assert_eq!(plc_state.ltp_coef_q14, [0, 0, 11_467, 0, 0]);

        for k in 0..4 {
            frame.subframes[k].ltp_coeffs = [0, 40, 60, 40, 0];
        }
        silk_plc_update(&mut plc_state, &mut ch_state, &frame);
        assert_eq!(plc_state.ltp_coef_q14, [0, 0, 15_564, 0, 0]);

        // High-gain WB CNG
        const NLSF_WB: [i16; 16] = [
            674, 1292, 2214, 4949, 6390, 9088, 11264, 13184, 15232, 17536, 19712, 21888, 24192,
            26240, 28416, 30336,
        ];
        let mut cng_state = CngState::new();
        ch_state.prev_nlsf_q15 = NLSF_WB;
        frame.frame_type = FrameType::new(SignalType::Inactive, QuantizationOffsetType::Low);
        let a_wb = Decoder::lsf_to_lpc(&NLSF_WB).unwrap();
        for k in 0..4 {
            frame.subframes[k].gain_q16 = 50_000_000;
            frame.subframes[k].lpc_coeffs_q12 = a_wb;
        }
        for i in 0..320 {
            frame.exc_q14[i] = (((i as i32) * 97 + 13) % 401 - 200) * 64;
        }

        let mut pcm_buf = [0i16; 320];
        silk_plc_update(&mut plc_state, &mut ch_state, &frame);
        silk_cng(
            &mut cng_state,
            &ch_state,
            &plc_state,
            Some(&frame),
            16,
            &mut pcm_buf,
        )
        .unwrap();
        assert_eq!(cng_state.cng_smth_gain_q16, 12_711_362);

        plc_state.loss_count = 1;
        plc_state.rand_scale_q14 = 4_096;
        plc_state.prev_gain_q16[1] = 1_000_000;
        silk_cng(&mut cng_state, &ch_state, &plc_state, None, 16, &mut pcm_buf).unwrap();
        assert_eq!(&pcm_buf[..8], &[1, 2, 3, 3, 3, 75, 39, 28]);
    }
}
