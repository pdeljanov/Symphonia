//! SILK comfort noise generation: the smoothed NLSF and gain state updated from every decoded
//! frame and the noise excitation synthesised through it to fill DTX gaps and lost frames
//! ([RFC 6716 Section 4.2.7.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>),
//! [Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).

use crate::fixed::{
    mul_add_q16_i16, mul_q16, mul_q16_i16, round_shift, saturate_i16, shl_saturating, sqrt_approx,
};
use crate::silk::excitation::advance_lcg_seed;
use crate::silk::lpc::nlsf_to_lpc_q12;
use crate::silk::plc::{PlcState, Q8_SHIFT, Q16_SHIFT, Q16_TO_Q10_SHIFT};
use crate::silk::{
    ChannelState, MAX_FRAME_SAMPLES, MAX_LPC_ORDER, MAX_SUBFRAME_SAMPLES, MAX_SUBFRAMES, SignalType,
};
use symphonia_core::errors::Result;

/// High PLC noise gain threshold in Q16 (`32.0` in Q16 = `1 << 21`) above which integer
/// square roots are computed at unscaled integer precision to avoid 32-bit overflow.
const CNG_HIGH_PLC_GAIN_THRES_Q16: i32 = 1 << 21;

/// High smoothed CNG gain threshold in Q16 (`128.0` in Q16 = `1 << 23`) above which integer
/// square roots are computed at unscaled integer precision to avoid 32-bit overflow.
const CNG_HIGH_SMTH_GAIN_THRES_Q16: i32 = 1 << 23;

/// Energy penalty bit shift (`* 32`) applied to squared PLC gain when computing CNG gain.
const CNG_PLC_ENERGY_PENALTY_SHIFT: u32 = 5;

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

/// Per-channel SILK Comfort Noise Generation (CNG) state (RFC 6716 Section 4.4).
///
/// Tracks exponentially smoothed Normalized Line Spectral Frequencies (NLSFs), smoothed
/// subframe gain, historical inactive excitation samples, and LPC synthesis filter state
/// across DTX and packet-loss intervals.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>
#[derive(Debug, Clone, Copy)]
pub(crate) struct CngState {
    /// Excitation history of inactive frames in Q14, the comfort noise source.
    cng_exc_buf_q14: [i32; MAX_FRAME_SAMPLES],
    /// Smoothed NLSFs of inactive frames in Q15.
    cng_smth_nlsf_q15: [i16; MAX_LPC_ORDER],
    /// LPC synthesis filter history of the comfort noise in Q14.
    cng_synth_state: [i32; MAX_LPC_ORDER],
    /// Smoothed subframe gain of inactive frames in Q16.
    cng_smth_gain_q16: i32,
    /// Seed of the comfort noise generator.
    rand_seed: i32,
    /// Internal sample rate in kHz the state was set up for.
    sample_rate_khz: usize,
}

impl Default for CngState {
    fn default() -> Self {
        return Self::new();
    }
}

impl CngState {
    /// Creates a new `CngState` with uniformly spaced initial NLSFs and the RFC 6716 LCG seed.
    pub(crate) fn new() -> Self {
        let step_q15 = (i16::MAX as i32) / ((MAX_LPC_ORDER as i32) + 1);
        let smth_nlsf_q15 = std::array::from_fn(|i| (((i as i32) + 1) * step_q15) as i16);

        return Self {
            cng_exc_buf_q14: [0; MAX_FRAME_SAMPLES],
            cng_smth_nlsf_q15: smth_nlsf_q15,
            cng_synth_state: [0; MAX_LPC_ORDER],
            cng_smth_gain_q16: 0,
            rand_seed: CNG_INITIAL_RAND_SEED,
            sample_rate_khz: 0,
        };
    }

    /// Resets the comfort noise state (RFC 6716 Section 4.4).
    pub(crate) fn reset(&mut self) {
        let lpc_order = if self.sample_rate_khz == 8 || self.sample_rate_khz == 12 {
            10
        } else {
            MAX_LPC_ORDER
        };
        *self = Self::new();
        self.reset_for_lpc_order(lpc_order);
        return;
    }

    /// Resets smoothed NLSFs, smoothed gain, and LCG seed for the given `lpc_order`
    /// (RFC 6716 Section 4.4).
    pub(crate) fn reset_for_lpc_order(&mut self, lpc_order: usize) {
        let order = lpc_order.clamp(1, MAX_LPC_ORDER);
        let step_q15 = (i16::MAX as i32) / ((order as i32) + 1);
        self.cng_smth_nlsf_q15[..order]
            .iter_mut()
            .enumerate()
            .for_each(|(i, nlsf)| *nlsf = (((i as i32) + 1) * step_q15) as i16);
        self.cng_smth_gain_q16 = 0;
        self.rand_seed = CNG_INITIAL_RAND_SEED;
        return;
    }

    /// Synchronizes `self.sample_rate_khz` with the channel's internal sample rate, resetting
    /// smoothed NLSFs and gain when the rate changes (RFC 6716 Section 4.4).
    #[inline]
    pub(crate) fn ensure_sample_rate(&mut self, sample_rate_khz: usize, lpc_order: usize) {
        if self.sample_rate_khz != sample_rate_khz {
            self.reset_for_lpc_order(lpc_order);
            self.sample_rate_khz = sample_rate_khz;
        }
        return;
    }

    /// Updates smoothed NLSFs, excitation history, and smoothed gain from a valid
    /// inactive (`SignalType::Inactive`) SILK frame (RFC 6716 Section 4.4).
    pub(crate) fn update_comfort_noise(
        &mut self,
        prev_nlsf_q15: &[i16],
        subframe_gains_q16: &[i32],
        exc_q14: &[i32],
        subfr_length: usize,
        nb_subfr: usize,
        lpc_order: usize,
    ) {
        let order = lpc_order.min(MAX_LPC_ORDER).min(prev_nlsf_q15.len());
        self.cng_smth_nlsf_q15[..order].iter_mut().zip(prev_nlsf_q15[..order].iter()).for_each(
            |(smth_nlsf, &target_nlsf)| {
                let delta = (target_nlsf as i32) - (*smth_nlsf as i32);
                *smth_nlsf =
                    (*smth_nlsf).wrapping_add(mul_q16_i16(delta, CNG_NLSF_SMOOTH_Q16) as i16);
            },
        );

        let active_subfr = nb_subfr.min(subframe_gains_q16.len());
        let (_, max_subfr) = subframe_gains_q16[..active_subfr].iter().enumerate().fold(
            (0i32, 0usize),
            |(max_gain, best_i), (i, &gain_q16)| {
                if gain_q16 > max_gain { (gain_q16, i) } else { (max_gain, best_i) }
            },
        );

        if active_subfr > 0 && subfr_length > 0 {
            let move_samples = (active_subfr - 1) * subfr_length;
            if move_samples > 0 && move_samples + subfr_length <= MAX_FRAME_SAMPLES {
                self.cng_exc_buf_q14.copy_within(0..move_samples, subfr_length);
            }
            let src_start = max_subfr * subfr_length;
            if src_start + subfr_length <= exc_q14.len() && subfr_length <= MAX_FRAME_SAMPLES {
                self.cng_exc_buf_q14[..subfr_length]
                    .copy_from_slice(&exc_q14[src_start..src_start + subfr_length]);
            }
        }

        for &subfr_gain_q16 in subframe_gains_q16[..active_subfr].iter() {
            let gain_diff = subfr_gain_q16.wrapping_sub(self.cng_smth_gain_q16);
            self.cng_smth_gain_q16 =
                self.cng_smth_gain_q16.wrapping_add(mul_q16_i16(gain_diff, CNG_GAIN_SMOOTH_Q16));
            if mul_q16(self.cng_smth_gain_q16, CNG_GAIN_SMOOTH_THRESHOLD_Q16) > subfr_gain_q16 {
                self.cng_smth_gain_q16 = subfr_gain_q16;
            }
        }
        return;
    }

    /// Generates pseudo-random comfort noise excitation by randomly indexing into
    /// `self.cng_exc_buf_q14` using a power-of-two mask (RFC 6716 Section 4.4).
    pub(crate) fn generate_excitation(&mut self, out_exc_q14: &mut [i32]) {
        let length = out_exc_q14.len();
        let mut exc_mask = CNG_BUF_MASK_MAX;
        while exc_mask > length {
            exc_mask >>= 1;
        }

        let mut seed = self.rand_seed;
        out_exc_q14.iter_mut().for_each(|dst| {
            seed = advance_lcg_seed(seed);
            let i = ((seed >> 24) as usize) & exc_mask;
            *dst = self.cng_exc_buf_q14[i];
        });
        self.rand_seed = seed;
        return;
    }

    /// Computes the Q10 comfort noise synthesis gain from the smoothed CNG gain and
    /// the current PLC random noise gain (RFC 6716 Section 4.4).
    fn compute_comfort_noise_gain_q10(&self, plc_state: &PlcState) -> i32 {
        let plc_gain_q16 = mul_q16(plc_state.rand_scale_q14 as i32, plc_state.prev_gain_q16[1]);
        let use_unscaled_int_sq = plc_gain_q16 >= CNG_HIGH_PLC_GAIN_THRES_Q16
            || self.cng_smth_gain_q16 > CNG_HIGH_SMTH_GAIN_THRES_Q16;

        let (plc_gain_sq, smth_gain_sq, sqrt_to_q16_shift) = if use_unscaled_int_sq {
            let plc_int = plc_gain_q16 >> Q16_SHIFT;
            let smth_int = self.cng_smth_gain_q16 >> Q16_SHIFT;
            (plc_int * plc_int, smth_int * smth_int, Q16_SHIFT)
        } else {
            (
                mul_q16(plc_gain_q16, plc_gain_q16),
                mul_q16(self.cng_smth_gain_q16, self.cng_smth_gain_q16),
                Q8_SHIFT,
            )
        };

        let diff =
            smth_gain_sq.wrapping_sub(plc_gain_sq.wrapping_shl(CNG_PLC_ENERGY_PENALTY_SHIFT));
        let cng_gain_q16 = sqrt_approx(diff).wrapping_shl(sqrt_to_q16_shift);

        return cng_gain_q16 >> Q16_TO_Q10_SHIFT;
    }

    /// Synthesizes comfort noise through the smoothed NLSF-derived LPC filter and
    /// saturating-adds it to `frame_pcm` during packet loss or DTX (RFC 6716 Section 4.4).
    pub(crate) fn generate_comfort_noise(
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

        let a_q12 = nlsf_to_lpc_q12(&self.cng_smth_nlsf_q15[..order])?;

        for (i, sample) in frame_pcm[..length].iter_mut().enumerate() {
            let lpc_pred_q10 = a_q12[..order].iter().enumerate().fold(
                (order >> 1) as i32,
                |acc, (tap, &coeff_q12)| {
                    mul_add_q16_i16(acc, cng_sig_q14[MAX_LPC_ORDER + i - 1 - tap], coeff_q12)
                },
            );

            let synth_q14 =
                cng_sig_q14[MAX_LPC_ORDER + i].saturating_add(shl_saturating(lpc_pred_q10, 4));
            cng_sig_q14[MAX_LPC_ORDER + i] = synth_q14;

            let cng_sample = saturate_i16(round_shift(mul_q16(synth_q14, gain_q10), Q8_SHIFT));
            *sample = sample.saturating_add(cng_sample);
        }

        self.cng_synth_state.copy_from_slice(&cng_sig_q14[length..length + MAX_LPC_ORDER]);
        return Ok(());
    }

    /// Orchestrates Comfort Noise Generation for a decoded or lost SILK frame (RFC 6716 Section 4.4).
    ///
    /// - When `plc_state.loss_count == 0` and `ch_state.prev_signal_type == SignalType::Inactive`,
    ///   updates smoothed NLSFs, excitation history, and smoothed gain from `frame`.
    /// - When `plc_state.loss_count > 0`, synthesizes comfort noise and adds it in-place to `frame_pcm`.
    /// - When `plc_state.loss_count == 0`, clears the CNG synthesis filter memory.
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>
    pub(crate) fn apply(
        &mut self,
        ch_state: &ChannelState,
        plc_state: &PlcState,
        frame: Option<CngFrameInput<'_>>,
        lpc_order: usize,
        frame_pcm: &mut [i16],
    ) -> Result<()> {
        let order = lpc_order.min(MAX_LPC_ORDER);
        self.ensure_sample_rate(ch_state.sample_rate_khz, order);

        let estimate = plc_state.loss_count == 0
            && ch_state.references.prev_signal_type == SignalType::Inactive;
        if let Some(decoded) = frame.filter(|_| estimate) {
            let nb_subfr = decoded.subframes.min(MAX_SUBFRAMES);
            let subfr_length =
                decoded.exc_q14.len().checked_div(nb_subfr).unwrap_or(0).min(MAX_SUBFRAME_SAMPLES);
            self.update_comfort_noise(
                &ch_state.references.prev_nlsf_q15[..order],
                &decoded.gains_q16[..nb_subfr],
                decoded.exc_q14,
                subfr_length,
                nb_subfr,
                order,
            );
        }

        if plc_state.loss_count > 0 {
            self.generate_comfort_noise(plc_state, order, frame_pcm)?;
        } else {
            self.cng_synth_state[..order].fill(0);
        }

        return Ok(());
    }
}

/// The fields of a decoded frame that comfort noise estimation reads
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
#[derive(Debug, Clone, Copy)]
pub(crate) struct CngFrameInput<'a> {
    /// Q16 gain of every subframe.
    pub(crate) gains_q16: [i32; MAX_SUBFRAMES],
    /// Q14 excitation of the frame.
    pub(crate) exc_q14: &'a [i32],
    /// Number of subframes in the frame.
    pub(crate) subframes: usize,
}

#[cfg(test)]
mod tests {
    // Regression pins: expected values recorded from libopus 1.6.1 for concealment, which
    // RFC 6716 Section 4.4 leaves to the implementation; a change here is a design decision, not
    // a conformance failure.
    use super::*;
    use crate::silk::Frame;
    use crate::silk::plc::PlcState;
    use crate::silk::plc::test_support::*;
    use crate::silk::{FrameType, QuantizationOffsetType};

    /// Verifies `CngState::apply` parameter smoothing across consecutive inactive frames
    /// (including the -3 dB rapid gain adaptation threshold) and comfort noise synthesis
    /// across consecutive DTX / lost frames against the recorded values.
    #[test]
    fn comfort_noise_smoothing_and_dtx_synthesis_regression_pin() {
        const NLSF_F0: [i16; 10] =
            [1547, 4508, 7814, 10871, 14261, 17641, 21363, 25184, 26336, 26339];
        const NLSF_F1: [i16; 10] =
            [1800, 4800, 8000, 11200, 14500, 17900, 21500, 25000, 26800, 28500];

        const EXPECTED_SMTH_NLSF_1: [i16; 10] =
            [2621, 5594, 8654, 11652, 14733, 17811, 20974, 24163, 26685, 28921];
        const EXPECTED_EXC_BUF_HEAD_1: [i32; 16] = [
            4768, 8960, -6080, -1888, 2304, 6496, -8544, -4352, -160, 4032, 8224, -6816, -2624,
            1568, 5760, -9280,
        ];

        const EXPECTED_SMTH_NLSF_2: [i16; 10] =
            [2416, 5395, 8490, 11539, 14674, 17833, 21105, 24371, 26713, 28815];
        const EXPECTED_EXC_BUF_HEAD_2: [i32; 16] = [
            4676, -3948, 1456, 6860, -1764, 3640, -4984, 420, 5824, -2800, 2604, -6020, -616, 4788,
            -3836, 1568,
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
        let mut plc = PlcState::new();
        let mut cng = CngState::new();

        // Inactive frame 1
        let mut frame1 = Frame::new(160, 4, 10);
        frame1.frame_type = FrameType::new(SignalType::Inactive, QuantizationOffsetType::Low);
        let a_f0 = nlsf_to_lpc_q12(&NLSF_F0).unwrap();
        ch_state.references.prev_nlsf_q15[..10].copy_from_slice(&NLSF_F0);
        let gains_f0 = [655_360, 983_040, 786_432, 524_288];
        for (i, &gain) in gains_f0.iter().enumerate() {
            frame1.subframes[i].gain_q16 = gain;
            frame1.subframes[i].lpc_coeffs_q12 = a_f0;
        }
        for i in 0..160 {
            frame1.exc_q14[i] = (((i as i32) * 131 + 17) % 601 - 300) * 32;
            frame1.output_pcm[i] = ((((i as i32) * 37 + 11) % 201) - 100) as i16;
            ch_state.output_history[i] = frame1.output_pcm[i];
        }

        let mut pcm_buf = [0i16; 160];
        pcm_buf.copy_from_slice(&frame1.output_pcm[..160]);
        record_decoded(&mut plc, &mut ch_state, &frame1);
        cng.apply(&ch_state, &plc, Some(cng_input(&frame1)), 10, &mut pcm_buf).unwrap();
        plc.glue_frames(&mut pcm_buf);

        assert_eq!(cng.cng_smth_gain_q16, 185_963);
        assert_eq!(cng.rand_seed, 3_176_576);
        assert_eq!(cng.sample_rate_khz, 8);
        assert_eq!(&cng.cng_smth_nlsf_q15[..10], &EXPECTED_SMTH_NLSF_1);
        assert_eq!(&cng.cng_exc_buf_q14[..16], &EXPECTED_EXC_BUF_HEAD_1);

        // Inactive frame 2 (triggers -3 dB rapid gain adaptation)
        ch_state.references.prev_nlsf_q15[..10].copy_from_slice(&NLSF_F1);
        let gains_f1 = [262_144, 196_608, 327_680, 294_912];
        for (i, &gain) in gains_f1.iter().enumerate() {
            frame1.subframes[i].gain_q16 = gain;
        }
        for i in 0..160 {
            frame1.exc_q14[i] = (((i as i32) * 193 + 7) % 501 - 250) * 28;
        }

        record_decoded(&mut plc, &mut ch_state, &frame1);
        cng.apply(&ch_state, &plc, Some(cng_input(&frame1)), 10, &mut pcm_buf).unwrap();
        plc.glue_frames(&mut pcm_buf);

        assert_eq!(cng.cng_smth_gain_q16, 207_949);
        assert_eq!(&cng.cng_smth_nlsf_q15[..10], &EXPECTED_SMTH_NLSF_2);
        assert_eq!(&cng.cng_exc_buf_q14[..16], &EXPECTED_EXC_BUF_HEAD_2);

        // DTX / Lost frame 1
        plc.conceal_lost_frame(&mut ch_state, &concealment_layout(160, 4, 10), &mut pcm_buf);
        cng.apply(&ch_state, &plc, None, 10, &mut pcm_buf).unwrap();
        plc.glue_frames(&mut pcm_buf);

        assert_eq!(cng.rand_seed, 502_295_712);
        assert_eq!(&pcm_buf[..16], &EXPECTED_DTX_1_HEAD);
        assert_eq!(&pcm_buf[160 - 16..160], &EXPECTED_DTX_1_TAIL);
        assert_eq!(&cng.cng_synth_state[..10], &EXPECTED_SYNTH_STATE_1);

        // DTX / Lost frame 2
        plc.conceal_lost_frame(&mut ch_state, &concealment_layout(160, 4, 10), &mut pcm_buf);
        cng.apply(&ch_state, &plc, None, 10, &mut pcm_buf).unwrap();
        plc.glue_frames(&mut pcm_buf);

        assert_eq!(cng.rand_seed, -726_478_656);
        assert_eq!(&pcm_buf[..16], &EXPECTED_DTX_2_HEAD);
        assert_eq!(&pcm_buf[160 - 16..160], &EXPECTED_DTX_2_TAIL);
        assert_eq!(&cng.cng_synth_state[..10], &EXPECTED_SYNTH_STATE_2);
    }

    /// Verifies voiced LTP gain clamping bounds (`< 11469` and `> 15565`) and high-gain
    /// Wideband CNG synthesis (`cng_smth_gain_q16 > (1 << 23)`) against the recorded values.
    #[test]
    fn ltp_clamping_and_wb_high_gain_comfort_noise_regression_pin() {
        let mut ch_state = ChannelState::new();
        ch_state.set_sample_rate(16, 48_000).unwrap();
        let mut plc = PlcState::new();

        let mut frame = Frame::new(320, 4, 16);
        frame.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);
        frame.ltp_scale_q14 = 15_565;
        for i in 0..4 {
            frame.subframes[i].pitch_lag = 60;
            frame.subframes[i].ltp_coeffs = [0, 0, 20, 0, 0];
            frame.subframes[i].gain_q16 = 65_536;
        }
        record_decoded(&mut plc, &mut ch_state, &frame);
        assert_eq!(plc.pitch_l_q8, 15_360);
        assert_eq!(plc.ltp_coef_q14, [0, 0, 11_467, 0, 0]);

        for i in 0..4 {
            frame.subframes[i].ltp_coeffs = [0, 40, 60, 40, 0];
        }
        record_decoded(&mut plc, &mut ch_state, &frame);
        assert_eq!(plc.ltp_coef_q14, [0, 0, 15_564, 0, 0]);

        // High-gain WB CNG
        const NLSF_WB: [i16; 16] = [
            674, 1292, 2214, 4949, 6390, 9088, 11264, 13184, 15232, 17536, 19712, 21888, 24192,
            26240, 28416, 30336,
        ];
        let mut cng = CngState::new();
        ch_state.references.prev_nlsf_q15 = NLSF_WB;
        frame.frame_type = FrameType::new(SignalType::Inactive, QuantizationOffsetType::Low);
        let a_wb = nlsf_to_lpc_q12(&NLSF_WB).unwrap();
        for i in 0..4 {
            frame.subframes[i].gain_q16 = 50_000_000;
            frame.subframes[i].lpc_coeffs_q12 = a_wb;
        }
        for i in 0..320 {
            frame.exc_q14[i] = (((i as i32) * 97 + 13) % 401 - 200) * 64;
        }

        let mut pcm_buf = [0i16; 320];
        record_decoded(&mut plc, &mut ch_state, &frame);
        cng.apply(&ch_state, &plc, Some(cng_input(&frame)), 16, &mut pcm_buf).unwrap();
        assert_eq!(cng.cng_smth_gain_q16, 12_711_362);

        plc.loss_count = 1;
        plc.rand_scale_q14 = 4_096;
        plc.prev_gain_q16[1] = 1_000_000;
        cng.apply(&ch_state, &plc, None, 16, &mut pcm_buf).unwrap();
        assert_eq!(&pcm_buf[..8], &[1, 2, 3, 3, 3, 75, 39, 28]);
    }
}
