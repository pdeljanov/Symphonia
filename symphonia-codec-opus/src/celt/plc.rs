//! CELT packet loss concealment helpers: LPC analysis, FIR/IIR filtering, and pitch search
//! ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).

use crate::celt::constant::{CELT_WINDOW, EBANDS, band_width};
use crate::celt::postfilter::ChannelHistory;
use crate::celt::vq::renormalise_vector;
use crate::celt::{
    BlockSize, DECODE_BUFFER_SIZE, MAX_CHANNELS, MAX_FRAME_SIZE, OVERLAP, PITCH_HISTORY_LEN,
    lcg_next,
};

/// Saturation value of the loss duration counter, in 2.5 ms units.
const MAX_PLC_LOSS_DURATION: usize = 10_000;

/// Loss duration, in 2.5 ms units (`40 * 2.5 ms = 100 ms`), from which concealment switches from
/// pitch-based extrapolation to noise ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
const MAX_PERIODIC_PLC_DURATION: usize = 40;

/// Energy decay, in 6 dB base-2 units, of the first noise-concealed frame.
const FIRST_LOSS_ENERGY_DECAY: f32 = 1.5;

/// Energy decay, in 6 dB base-2 units, of every further noise-concealed frame.
const LATER_LOSS_ENERGY_DECAY: f32 = 0.5;

/// Excitation gain of the first pitch-concealed frame.
const FIRST_PERIODIC_FADE: f32 = 1.0;

/// Excitation gain of every further pitch-concealed frame.
const LATER_PERIODIC_FADE: f32 = 0.8;

/// White noise floor (`-40 dB`) added to the zero-lag autocorrelation before LPC analysis.
const LPC_NOISE_FLOOR: f32 = 1.0001;

/// Gaussian lag-window coefficient applied to the autocorrelation before LPC analysis.
const LPC_LAG_WINDOW: f32 = 0.008;

/// Ratio of reference to synthesized energy below which the extrapolation is muted.
const MAX_SYNTHESIS_GAIN_RATIO: f32 = 0.2;

/// Concealment state carried between frames: the LPC filter fitted at the onset of a loss, the
/// loss duration and the pitch of the last pitch-based concealment
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlcState {
    /// 24th-order LPC coefficients per channel fitted at the onset of a loss.
    pub(crate) lpc: [[f32; LPC_ORDER]; MAX_CHANNELS],
    /// Consecutive lost duration in 2.5 ms units, saturating at `10_000`.
    pub(crate) loss_duration: usize,
    /// Pitch lag (`100..=720`) found at the onset of a loss.
    pub(crate) last_pitch_index: usize,
    /// Whether pitch-based concealment is ruled out until two frames in a row decode.
    pub(crate) skip_plc: bool,
    /// Whether the previous frame was concealed by pitch-based extrapolation.
    pub(crate) last_frame_periodic_plc: bool,
}

impl Default for PlcState {
    fn default() -> Self {
        return Self {
            lpc: [[0.0; LPC_ORDER]; MAX_CHANNELS],
            loss_duration: 0,
            last_pitch_index: 0,
            skip_plc: true,
            last_frame_periodic_plc: false,
        };
    }
}

impl PlcState {
    /// Returns the state to its initial value, which rules out pitch-based concealment
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
        return;
    }

    /// Allows pitch-based concealment again once a frame decodes right after a decoded frame
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    pub(crate) fn begin_frame(&mut self) {
        if self.loss_duration == 0 {
            self.skip_plc = false;
        }
        return;
    }

    /// Ends a loss after a decoded frame
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    pub(crate) fn end_loss(&mut self) {
        self.loss_duration = 0;
        self.last_frame_periodic_plc = false;
        return;
    }

    /// Adds one concealed frame of `lm` to the loss duration
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    pub(crate) fn record_loss(&mut self, lm: BlockSize) {
        self.loss_duration = (self.loss_duration + lm.blocks()).min(MAX_PLC_LOSS_DURATION);
        return;
    }

    /// Whether a lost frame is concealed with noise rather than pitch extrapolation: after
    /// 100 ms of loss, in Hybrid mode (`start_band != 0`), or while pitch concealment is ruled out
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    pub(crate) fn uses_noise(&self, start_band: usize) -> bool {
        return self.loss_duration >= MAX_PERIODIC_PLC_DURATION || start_band != 0 || self.skip_plc;
    }

    /// Band energy decay of a noise-concealed frame
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    pub(crate) fn noise_energy_decay(&self) -> f32 {
        if self.loss_duration == 0 {
            return FIRST_LOSS_ENERGY_DECAY;
        }
        return LATER_LOSS_ENERGY_DECAY;
    }

    /// Records a noise-concealed frame, which rules out pitch concealment until the stream
    /// recovers ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    pub(crate) fn end_noise_concealment(&mut self) {
        self.skip_plc = true;
        self.last_frame_periodic_plc = false;
        return;
    }

    /// Conceals `n` samples of the first `channels` channels of `decode_mem` by repeating the
    /// LPC excitation of the last pitch period with a decaying gain, then running it through the
    /// LPC synthesis filter ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    pub(crate) fn conceal_pitch_based(
        &mut self,
        decode_mem: &mut [ChannelHistory; MAX_CHANNELS],
        channels: usize,
        n: usize,
    ) {
        let (pitch_index, fade) = match self.last_frame_periodic_plc {
            false => {
                let ch_refs =
                    [&decode_mem[0][..DECODE_BUFFER_SIZE], &decode_mem[1][..DECODE_BUFFER_SIZE]];
                let found_pitch = estimate_pitch_period(&ch_refs[..channels]);
                self.last_pitch_index = found_pitch;
                (found_pitch, FIRST_PERIODIC_FADE)
            }
            true => (self.last_pitch_index, LATER_PERIODIC_FADE),
        };

        for (channel, lpc) in decode_mem.iter_mut().zip(self.lpc.iter_mut()).take(channels) {
            let mut exc_buf = [0.0_f32; PITCH_HISTORY_LEN + LPC_ORDER];
            let hist_start = DECODE_BUFFER_SIZE - PITCH_HISTORY_LEN - LPC_ORDER;
            exc_buf.copy_from_slice(&channel[hist_start..DECODE_BUFFER_SIZE]);
            if !self.last_frame_periodic_plc {
                estimate_lpc(lpc, &exc_buf[LPC_ORDER..]);
            }
            let exc_length = (2 * pitch_index).min(PITCH_HISTORY_LEN);
            whiten_excitation(&mut exc_buf, lpc, exc_length);
            let exc_only = &exc_buf[LPC_ORDER..];
            let decay = excitation_decay(exc_only, exc_length);

            channel.copy_within(n..DECODE_BUFFER_SIZE, 0);
            let s1 = extrapolate_excitation(channel, exc_only, pitch_index, fade * decay, decay, n);
            synthesize_extrapolation(channel, lpc, s1, n);
        }

        self.last_frame_periodic_plc = true;
        return;
    }
}

/// Fits the 24th-order LPC filter to the last `PITCH_HISTORY_LEN` history samples `exc`
/// after windowing, a noise floor and lag windowing of the autocorrelation
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
fn estimate_lpc(lpc: &mut [f32; LPC_ORDER], exc: &[f32]) {
    let mut ac = [0.0_f32; LPC_ORDER + 1];
    autocorrelation(&exc[..PITCH_HISTORY_LEN], &mut ac, Some(&CELT_WINDOW));
    ac[0] *= LPC_NOISE_FLOOR;
    for (i, ac_val) in ac.iter_mut().enumerate().skip(1).take(LPC_ORDER) {
        let i_f = i as f32;
        *ac_val -= *ac_val * (LPC_LAG_WINDOW * LPC_LAG_WINDOW) * i_f * i_f;
    }
    levinson_durbin(lpc, &ac);
    return;
}

/// Replaces the last `exc_length` samples of `exc_buf` (history behind `LPC_ORDER` leading
/// samples) with their LPC analysis residual
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
fn whiten_excitation(
    exc_buf: &mut [f32; PITCH_HISTORY_LEN + LPC_ORDER],
    lpc: &[f32; LPC_ORDER],
    exc_length: usize,
) {
    let mut fir_tmp = [0.0_f32; PITCH_HISTORY_LEN];
    let fir_input_start = PITCH_HISTORY_LEN - exc_length;
    fir_filter(
        &exc_buf[fir_input_start..(LPC_ORDER + PITCH_HISTORY_LEN)],
        lpc,
        &mut fir_tmp[..exc_length],
    );
    exc_buf[(LPC_ORDER + fir_input_start)..(LPC_ORDER + PITCH_HISTORY_LEN)]
        .copy_from_slice(&fir_tmp[..exc_length]);
    return;
}

/// Per-period amplitude decay of the excitation: the energy ratio of the last two half
/// windows of `exc_length` samples, capped at one
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
fn excitation_decay(exc_only: &[f32], exc_length: usize) -> f32 {
    let decay_length = exc_length >> 1;
    let mut e1 = 1.0_f32;
    let mut e2 = 1.0_f32;
    for i in 0..decay_length {
        let ex1 = exc_only[PITCH_HISTORY_LEN - decay_length + i];
        e1 += ex1 * ex1;
        let ex2 = exc_only[PITCH_HISTORY_LEN - 2 * decay_length + i];
        e2 += ex2 * ex2;
    }
    e1 = e1.min(e2);
    return (e1 / e2).sqrt();
}

/// Writes `n + OVERLAP` samples repeating the last `pitch_index` excitation samples into the
/// concealed region of `channel`, starting at `attenuation` and multiplying by `decay` every
/// period. Returns the energy of the history samples one period earlier, the synthesis reference
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
fn extrapolate_excitation(
    channel: &mut ChannelHistory,
    exc_only: &[f32],
    pitch_index: usize,
    mut attenuation: f32,
    decay: f32,
    n: usize,
) -> f32 {
    let extrapolation_offset = PITCH_HISTORY_LEN - pitch_index;
    let mut s1 = 0.0_f32;
    let mut j = 0usize;

    for i in 0..(n + OVERLAP) {
        if j >= pitch_index {
            j -= pitch_index;
            attenuation *= decay;
        }
        channel[DECODE_BUFFER_SIZE - n + i] = attenuation * exc_only[extrapolation_offset + j];
        let prev_sample =
            channel[DECODE_BUFFER_SIZE - PITCH_HISTORY_LEN - n + extrapolation_offset + j];
        s1 += prev_sample * prev_sample;
        j += 1;
    }
    return s1;
}

/// Runs the extrapolated excitation through the LPC synthesis filter and attenuates the result
/// when its energy exceeds the reference energy `s1`
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
fn synthesize_extrapolation(
    channel: &mut ChannelHistory,
    lpc: &[f32; LPC_ORDER],
    s1: f32,
    n: usize,
) {
    let extrapolation_len = n + OVERLAP;
    let mut lpc_mem: [f32; LPC_ORDER] =
        std::array::from_fn(|i| channel[DECODE_BUFFER_SIZE - n - 1 - i]);
    let mut iir_in = [0.0_f32; MAX_FRAME_SIZE + OVERLAP];
    let extrap_slice =
        &mut channel[(DECODE_BUFFER_SIZE - n)..(DECODE_BUFFER_SIZE - n + extrapolation_len)];
    iir_in[..extrapolation_len].copy_from_slice(extrap_slice);
    iir_filter(&iir_in[..extrapolation_len], lpc, extrap_slice, &mut lpc_mem);

    let s2 = extrap_slice.iter().fold(0.0_f32, |acc, &sample| acc + sample * sample);
    match (s1 <= MAX_SYNTHESIS_GAIN_RATIO * s2 || s2.is_nan(), s1 < s2) {
        (true, _) => extrap_slice.fill(0.0_f32),
        (false, true) => {
            let ratio = ((s1 + 1.0_f32) / (s2 + 1.0_f32)).sqrt();
            for (sample, &w) in extrap_slice[..OVERLAP].iter_mut().zip(CELT_WINDOW.iter()) {
                let tmp_g = 1.0_f32 - w * (1.0_f32 - ratio);
                *sample *= tmp_g;
            }
            for sample in extrap_slice[OVERLAP..extrapolation_len].iter_mut() {
                *sample *= ratio;
            }
        }
        (false, false) => {}
    }
    return;
}

/// Fills bands `start..end` of the first `channels` channels of `x_norm` (`n` bins per channel)
/// with unit-norm pseudo-random noise from the spreading LCG starting at `seed`. Returns the
/// generator state ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
pub(crate) fn fill_noise_spectrum(
    x_norm: &mut [f32],
    channels: usize,
    n: usize,
    (start, end): (usize, usize),
    lm: BlockSize,
    mut seed: u32,
) -> u32 {
    for ch in 0..channels {
        for (band, &band_start) in EBANDS.iter().enumerate().take(end).skip(start) {
            let boffs = ch * n + lm.scale(band_start);
            let blen = band_width(band, lm);
            let band_slice = &mut x_norm[boffs..(boffs + blen)];
            for coeff in band_slice.iter_mut() {
                seed = lcg_next(seed);
                *coeff = ((seed as i32) >> 20) as f32;
            }
            renormalise_vector(band_slice, blen, 1.0_f32);
        }
    }
    return seed;
}

/// Linear Predictive Coding (LPC) order used for CELT Packet Loss Concealment (`LPC_ORDER = 24`).
pub(crate) const LPC_ORDER: usize = 24;

/// Maximum pitch period searched during CELT Packet Loss Concealment (`PLC_PITCH_LAG_MAX = 720`).
pub(crate) const PLC_PITCH_LAG_MAX: usize = 720;

/// Minimum pitch period searched during CELT Packet Loss Concealment (`PLC_PITCH_LAG_MIN = 100`).
pub(crate) const PLC_PITCH_LAG_MIN: usize = 100;

/// Computes the `ac.len()` autocorrelation coefficients (lags `0..ac.len()`) of `x`, optionally
/// tapering the first and last `window.len()` samples of `x` with the rising and falling halves
/// of `window` before the sums
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
pub(crate) fn autocorrelation(x: &[f32], ac: &mut [f32], window: Option<&[f32]>) {
    let n = x.len();
    let lag = ac.len() - 1;
    debug_assert!(n > lag);

    let mut xx = [0.0f32; DECODE_BUFFER_SIZE];
    let buf: &[f32] = match window {
        Some(win) => {
            let overlap = win.len();
            debug_assert!(n <= DECODE_BUFFER_SIZE);
            debug_assert!(overlap <= n);
            xx[..n].copy_from_slice(x);
            for i in 0..overlap {
                xx[i] *= win[i];
                xx[n - overlap + i] *= win[overlap - 1 - i];
            }
            &xx[..n]
        }
        None => x,
    };

    let fast_n = n - lag;
    for k in 0..=lag {
        let mut d = 0.0f32;
        for i in k..fast_n + k {
            d += buf[i] * buf[i - k];
        }
        for i in fast_n + k..n {
            d += buf[i] * buf[i - k];
        }
        ac[k] = d;
    }
    return;
}

/// Solves the Yule-Walker equations by the Levinson-Durbin recursion, writing the `lpc.len()`
/// linear prediction coefficients fitted to the autocorrelation `ac` (which holds one more lag
/// than the filter order), and stops early once the residual energy falls below 0.1% of `ac[0]`
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
pub(crate) fn levinson_durbin(lpc: &mut [f32], ac: &[f32]) {
    let p = lpc.len();
    debug_assert!(ac.len() > p);

    lpc.fill(0.0);
    if ac[0] <= 1.0e-10 {
        return;
    }

    let mut error = ac[0];
    for i in 0..p {
        let mut rr = 0.0f32;
        for j in 0..i {
            rr += lpc[j] * ac[i - j];
        }
        rr += ac[i + 1];
        let r = -rr / error;
        lpc[i] = r;

        let half = (i + 1) >> 1;
        for j in 0..half {
            let tmp1 = lpc[j];
            let tmp2 = lpc[i - 1 - j];
            lpc[j] = tmp1 + r * tmp2;
            lpc[i - 1 - j] = tmp2 + r * tmp1;
        }

        error -= r * r * error;
        // Stop early if prediction residual energy drops below 0.1% of `ac[0]`
        if error <= 0.001 * ac[0] {
            break;
        }
    }
    return;
}

/// Applies the causal FIR analysis filter with taps `coeffs` (the LPC analysis filter, which
/// whitens the history into its prediction residual)
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)):
///
/// ```text
/// y[i] = x[i] + sum_{j = 0}^{order - 1} coeffs[j] * x[i - 1 - j]
/// ```
///
/// `x_with_history` holds `coeffs.len()` past input samples followed by the `y.len()` samples to
/// filter, so `x_with_history[coeffs.len()]` is `x[0]`.
pub(crate) fn fir_filter(x_with_history: &[f32], coeffs: &[f32], y: &mut [f32]) {
    let order = coeffs.len();
    debug_assert!(x_with_history.len() >= order + y.len());

    for (i, y_slot) in y.iter_mut().enumerate() {
        *y_slot = coeffs.iter().enumerate().fold(x_with_history[order + i], |acc, (j, &c)| {
            acc + c * x_with_history[order + i - 1 - j]
        });
    }
    return;
}

/// Applies the all-pole IIR synthesis filter with denominator `coeffs` (the LPC synthesis
/// filter, which shapes the extrapolated excitation)
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)):
///
/// ```text
/// y[i] = x[i] - sum_{j = 0}^{order - 1} coeffs[j] * y[i - 1 - j]
/// ```
///
/// `mem` holds the `coeffs.len()` most recent outputs, newest first (`mem[0] = y[-1]`), and is
/// updated in place so that filtering can continue across calls.
pub(crate) fn iir_filter(x: &[f32], coeffs: &[f32], y: &mut [f32], mem: &mut [f32]) {
    let order = coeffs.len();
    debug_assert!(y.len() >= x.len());
    debug_assert!(mem.len() >= order);

    for (&x_val, y_slot) in x.iter().zip(y.iter_mut()) {
        let sum = coeffs.iter().zip(&mem[..order]).fold(x_val, |acc, (&d, &m)| acc - d * m);
        if order > 1 {
            mem.copy_within(0..order - 1, 1);
        }
        if order > 0 {
            mem[0] = sum;
        }
        *y_slot = sum;
    }
    return;
}

/// Order of the whitening filter applied to the decimated history before the pitch search.
const PITCH_WHITENING_ORDER: usize = 4;

/// Lags on either side of a coarse pitch candidate that the refinement correlates.
const PITCH_REFINE_RADIUS: usize = 2;

/// Fraction of the correlation rise towards the best lag above which a neighbouring lag moves
/// the estimate by one sample.
const PITCH_OFFSET_THRESHOLD: f32 = 0.7;

/// Scale applied to a correlation before it is squared, keeping the square within `f32` range.
const XCORR_SQUARE_SCALE: f32 = 1.0e-12;

/// Decimates one or two channels of 48 kHz history by two (summing the channels) into
/// `x_lp`, then whitens the result with a bandwidth-expanded LPC analysis filter so that the
/// pitch correlation is not dominated by the spectral envelope
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
///
/// `x_lp.len()` is half the history length, and every channel of `x` holds at least
/// `2 * x_lp.len()` samples.
pub(crate) fn decimate_and_whiten_history(x: &[&[f32]], x_lp: &mut [f32]) {
    let half_len = x_lp.len();
    debug_assert!(!x.is_empty() && x[0].len() >= 2 * half_len);

    x_lp[0] = 0.25 * x[0][1] + 0.5 * x[0][0];
    for i in 1..half_len {
        x_lp[i] = 0.25 * (x[0][2 * i - 1] + x[0][2 * i + 1]) + 0.5 * x[0][2 * i];
    }

    if x.len() >= 2 {
        debug_assert!(x[1].len() >= 2 * half_len);
        x_lp[0] += 0.25 * x[1][1] + 0.5 * x[1][0];
        for i in 1..half_len {
            x_lp[i] += 0.25 * (x[1][2 * i - 1] + x[1][2 * i + 1]) + 0.5 * x[1][2 * i];
        }
    }

    let mut ac = [0.0f32; PITCH_WHITENING_ORDER + 1];
    autocorrelation(x_lp, &mut ac, None);

    // Lag windowing and bandwidth expansion (`lpc_bw = 0.9^k` and `0.8` first-difference blend)
    ac[0] *= 1.0001;
    for (i, ac_slot) in ac.iter_mut().enumerate().skip(1) {
        let frac = 0.008 * (i as f32);
        *ac_slot -= *ac_slot * frac * frac;
    }

    let mut lpc = [0.0f32; PITCH_WHITENING_ORDER];
    levinson_durbin(&mut lpc, &ac);

    let mut tmp = 1.0f32;
    for coeff in &mut lpc {
        tmp *= 0.9;
        *coeff *= tmp;
    }

    let c1 = 0.8f32;
    let lpc2 = [
        lpc[0] + c1,
        lpc[1] + c1 * lpc[0],
        lpc[2] + c1 * lpc[1],
        lpc[3] + c1 * lpc[2],
        c1 * lpc[3],
    ];

    // Apply 5-tap in-place FIR whitening filter using a 5-sample history shift register
    let mut mem = [0.0f32; 5];
    for sample in &mut x_lp[..half_len] {
        let x0 = *sample;
        let out = lpc2.iter().zip(&mem).fold(x0, |acc, (&c, &m)| acc + c * m);
        mem.copy_within(0..4, 1);
        mem[0] = x0;
        *sample = out;
    }
    return;
}

/// Scans `xcorr[..max_pitch]` and `y` with window length `window_len` to find the top 2
/// normalized cross-correlation pitch candidates.
fn find_top2_pitch_candidates(
    xcorr: &[f32],
    y: &[f32],
    window_len: usize,
    max_pitch: usize,
) -> [usize; 2] {
    let mut best_num = [-1.0f32, -1.0f32];
    let mut best_ener = [0.0f32, 1.0f32];
    let mut best_pitch = [0usize, 1usize];

    let mut syy = y[..window_len].iter().fold(1.0f32, |acc, &yv| acc + yv * yv);

    for (i, &xc) in xcorr[..max_pitch].iter().enumerate() {
        if xc > 0.0 {
            let xcorr16 = xc * XCORR_SQUARE_SCALE;
            let num = xcorr16 * xcorr16;
            if num * best_ener[1] > best_num[1] * syy {
                match num * best_ener[0] > best_num[0] * syy {
                    true => {
                        best_num[1] = best_num[0];
                        best_ener[1] = best_ener[0];
                        best_pitch[1] = best_pitch[0];
                        best_num[0] = num;
                        best_ener[0] = syy;
                        best_pitch[0] = i;
                    }
                    false => {
                        best_num[1] = num;
                        best_ener[1] = syy;
                        best_pitch[1] = i;
                    }
                }
            }
        }
        let y_enter = y[i + window_len];
        let y_leave = y[i];
        syy = (syy + y_enter * y_enter - y_leave * y_leave).max(1.0);
    }

    return best_pitch;
}

/// Finds the lag in `0..max_pitch` (in 2x-decimated samples) at which the `x_lp.len()` most
/// recent decimated samples `x_lp` best correlate with the earlier history `y`, by a coarse
/// search over a further 2x decimation refined around the two best candidates, with a
/// one-sample parabolic correction; the result is in 48 kHz samples
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
pub(crate) fn correlate_pitch_lag(x_lp: &[f32], y: &[f32], max_pitch: usize) -> usize {
    let half_max_pitch = max_pitch >> 1;
    let candidates = coarse_pitch_candidates(x_lp, y, max_pitch);
    let xcorr = refine_pitch_correlation(x_lp, y, candidates, half_max_pitch);
    let best = find_top2_pitch_candidates(&xcorr, y, x_lp.len(), half_max_pitch)[0];
    return ((2 * best) as isize - parabolic_offset(&xcorr, best, half_max_pitch)) as usize;
}

/// Correlates `x_lp` and `y` decimated by two once more over the lags `0..max_pitch / 4` and
/// returns the two best lags in 2x-decimated samples
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
fn coarse_pitch_candidates(x_lp: &[f32], y: &[f32], max_pitch: usize) -> [usize; 2] {
    // Lengths are in 48 kHz samples; `x_lp` and `y` are already decimated by two.
    let len = 2 * x_lp.len();
    let len_quarter = len >> 2;
    let lag_quarter = (len + max_pitch) >> 2;
    let max_pitch_quarter = max_pitch >> 2;

    let mut x_lp4 = [0.0f32; DECODE_BUFFER_SIZE >> 2];
    let mut y_lp4 = [0.0f32; DECODE_BUFFER_SIZE >> 2];
    x_lp4[..len_quarter].iter_mut().zip(x_lp.iter().step_by(2)).for_each(|(slot, &x)| *slot = x);
    y_lp4[..lag_quarter].iter_mut().zip(y.iter().step_by(2)).for_each(|(slot, &v)| *slot = v);

    let mut xcorr = [0.0f32; PLC_PITCH_LAG_MAX >> 1];
    xcorr[..max_pitch_quarter].iter_mut().enumerate().for_each(|(i, slot)| {
        *slot = correlation(&x_lp4[..len_quarter], &y_lp4[i..i + len_quarter]).max(-1.0);
    });
    let best = find_top2_pitch_candidates(&xcorr, &y_lp4, len_quarter, max_pitch_quarter);
    return best.map(|lag| 2 * lag);
}

/// Correlates `x_lp` with `y` at every lag within [`PITCH_REFINE_RADIUS`] of a coarse
/// candidate, below `half_max_pitch`; the other lags stay zero
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
fn refine_pitch_correlation(
    x_lp: &[f32],
    y: &[f32],
    candidates: [usize; 2],
    half_max_pitch: usize,
) -> [f32; PLC_PITCH_LAG_MAX >> 1] {
    let mut xcorr = [0.0f32; PLC_PITCH_LAG_MAX >> 1];
    for candidate in candidates {
        let lower = candidate.saturating_sub(PITCH_REFINE_RADIUS);
        let upper = (candidate + PITCH_REFINE_RADIUS).min(half_max_pitch.saturating_sub(1));
        for i in lower..=upper {
            if xcorr[i] == 0.0 {
                xcorr[i] = correlation(x_lp, &y[i..i + x_lp.len()]).max(-1.0);
            }
        }
    }
    return xcorr;
}

/// One-sample correction of the pitch lag `best` towards the clearly larger of its two
/// neighbouring correlations (`0` at the ends of the search range)
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
fn parabolic_offset(xcorr: &[f32], best: usize, half_max_pitch: usize) -> isize {
    if best == 0 || best + 1 >= half_max_pitch {
        return 0;
    }
    let (a, b, c) = (xcorr[best - 1], xcorr[best], xcorr[best + 1]);
    return match (
        (c - a) > PITCH_OFFSET_THRESHOLD * (b - a),
        (a - c) > PITCH_OFFSET_THRESHOLD * (b - c),
    ) {
        (true, _) => 1,
        (false, true) => -1,
        (false, false) => 0,
    };
}

/// Sum of the products of `x` and `y`, accumulated in order.
fn correlation(x: &[f32], y: &[f32]) -> f32 {
    return x.iter().zip(y).fold(0.0f32, |acc, (&xv, &yv)| acc + xv * yv);
}

/// Estimates the pitch period (`PLC_PITCH_LAG_MIN..=PLC_PITCH_LAG_MAX` samples at 48 kHz) of
/// the `DECODE_BUFFER_SIZE` samples of synthesis history in every channel of `history`, for
/// pitch-based concealment of a lost frame
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
pub(crate) fn estimate_pitch_period(history: &[&[f32]]) -> usize {
    let mut lp_pitch_buf = [0.0f32; DECODE_BUFFER_SIZE >> 1];
    decimate_and_whiten_history(history, &mut lp_pitch_buf);
    let raw_index = correlate_pitch_lag(
        &lp_pitch_buf[(PLC_PITCH_LAG_MAX >> 1)..],
        &lp_pitch_buf,
        PLC_PITCH_LAG_MAX - PLC_PITCH_LAG_MIN,
    );
    return PLC_PITCH_LAG_MAX - raw_index;
}

#[cfg(test)]
#[allow(clippy::excessive_precision)] // recorded values keep the 9 significant digits of the dump
mod tests {
    // Regression pins: expected values recorded from libopus 1.6.1 for concealment, which
    // RFC 6716 Section 4.4 leaves to the implementation; a change here is a design decision, not
    // a conformance failure.
    use super::*;
    use crate::celt::constant::CELT_WINDOW;
    use crate::celt::mdct::tests::assert_f32_slice_close;

    const LPC24_RECORDED: [f32; 24] = [
        -1.96576309e+00,
        9.91539001e-01,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
    ];
    const FIR_OUT16_RECORDED: [f32; 16] = [
        -1.59914970e+00,
        -1.15696335e+00,
        -6.23615265e-01,
        -5.57785034e-02,
        4.86587524e-01,
        9.47113037e-01,
        1.27740479e+00,
        1.44265747e+00,
        1.42449951e+00,
        1.22282410e+00,
        8.56124878e-01,
        3.59603882e-01,
        -2.18490601e-01,
        -8.20785522e-01,
        -1.38717651e+00,
        -1.85980988e+00,
    ];
    const IIR_OUT16_RECORDED: [f32; 16] = [
        -9.59978676e+00,
        -3.19644890e+01,
        -5.39396667e+01,
        -7.43943481e+01,
        -9.22717972e+01,
        -1.06672478e+02,
        -1.16924332e+02,
        -1.22632957e+02,
        -1.23707802e+02,
        -1.20362053e+02,
        -1.13086052e+02,
        -1.02597115e+02,
        -8.97708817e+01,
        -7.55600357e+01,
        -6.09089737e+01,
        -4.66716995e+01,
    ];
    const IIR_MEM24_RECORDED: [f32; 24] = [
        -4.55824547e+01,
        -4.89341164e+01,
        -5.14271545e+01,
        -5.37007256e+01,
        -5.63234367e+01,
        -5.97234650e+01,
        -6.41346588e+01,
        -6.95632324e+01,
        -7.57788315e+01,
        -8.23309860e+01,
        -8.85891266e+01,
        -9.38030014e+01,
        -9.71768417e+01,
        -9.79515305e+01,
        -9.54852905e+01,
        -8.93258896e+01,
        -7.92673798e+01,
        -6.53853073e+01,
        -4.80463791e+01,
        -2.78929291e+01,
        -5.80063629e+00,
        1.71837959e+01,
        3.99220276e+01,
        6.12720642e+01,
    ];

    #[test]
    fn plc_state_reset_and_loss_tracking() {
        let mut plc = PlcState::default();
        assert!(plc.uses_noise(0));
        plc.begin_frame();
        assert!(!plc.uses_noise(0));
        assert!(plc.uses_noise(17));
        assert_eq!(plc.noise_energy_decay(), FIRST_LOSS_ENERGY_DECAY);

        // Five 20 ms losses reach the 100 ms limit of pitch-based concealment.
        (0..5).for_each(|_| plc.record_loss(BlockSize::new(3)));
        assert_eq!(plc.loss_duration, 40);
        assert!(plc.uses_noise(0));
        assert_eq!(plc.noise_energy_decay(), LATER_LOSS_ENERGY_DECAY);
        (0..2000).for_each(|_| plc.record_loss(BlockSize::new(3)));
        assert_eq!(plc.loss_duration, MAX_PLC_LOSS_DURATION);

        // A decoded frame ends the loss; a frame after a loss keeps pitch concealment off.
        plc.end_noise_concealment();
        plc.begin_frame();
        assert!(plc.skip_plc);
        plc.end_loss();
        assert_eq!((plc.loss_duration, plc.last_frame_periodic_plc), (0, false));

        plc.lpc[1][3] = 0.5;
        plc.reset();
        assert_eq!(plc, PlcState::default());
        return;
    }

    #[test]
    fn lpc_fir_iir_and_pitch_period_regression_pin() {
        let mut sig = [0.0f32; 512];
        for (i, slot) in sig.iter_mut().enumerate() {
            *slot = (i as f32 * 0.14).sin() * 100.0 + 0.5 * (i as f32 * 0.31).cos() * 60.0;
        }

        let mut ac = [0.0f32; LPC_ORDER + 1];
        let mut lpc = [0.0f32; LPC_ORDER];
        autocorrelation(&sig, &mut ac, Some(&CELT_WINDOW));
        levinson_durbin(&mut lpc, &ac);
        assert_f32_slice_close(&lpc, &LPC24_RECORDED, 1.0e-5, 1.0e-4);

        let mut fir_out = [0.0f32; 64];
        fir_filter(&sig[..LPC_ORDER + 64], &lpc, &mut fir_out);
        assert_f32_slice_close(&fir_out[..16], &FIR_OUT16_RECORDED, 1.0e-5, 5.0e-4);

        let mut iir_mem = [0.0f32; LPC_ORDER];
        for i in 0..LPC_ORDER {
            iir_mem[i] = sig[LPC_ORDER - 1 - i];
        }
        let mut iir_out = [0.0f32; 64];
        iir_filter(&fir_out, &lpc, &mut iir_out, &mut iir_mem);
        assert_f32_slice_close(&iir_out[..16], &IIR_OUT16_RECORDED, 1.0e-4, 5.0e-3);
        assert_f32_slice_close(&iir_mem, &IIR_MEM24_RECORDED, 1.0e-4, 5.0e-3);

        // Verify PLC pitch search on a 200-sample periodic stereo history (harmonics at 200, 400, 600)
        let mut decode_mem0 = [0.0f32; DECODE_BUFFER_SIZE];
        let mut decode_mem1 = [0.0f32; DECODE_BUFFER_SIZE];
        for i in 0..DECODE_BUFFER_SIZE {
            let phase = 2.0 * std::f32::consts::PI * (i as f32) / 200.0;
            decode_mem0[i] = phase.sin() * 500.0 + 0.3 * (2.0 * phase).sin() * 500.0;
            decode_mem1[i] = decode_mem0[i] * 0.8;
        }
        let history_refs: [&[f32]; 2] = [&decode_mem0, &decode_mem1];
        let pitch_period = estimate_pitch_period(&history_refs);
        assert!(
            pitch_period == 200 || pitch_period == 400 || pitch_period == 600,
            "expected harmonic of 200, got {}",
            pitch_period
        );

        // Verify unique fundamental pitch period (400 samples, since 2 * 400 = 800 > 720)
        for i in 0..DECODE_BUFFER_SIZE {
            let phase = 2.0 * std::f32::consts::PI * (i as f32) / 400.0;
            decode_mem0[i] = (phase.sin()
                + 0.7 * (2.0 * phase).cos()
                + 0.5 * (3.0 * phase).sin()
                + 0.4 * (4.0 * phase).cos()
                + 0.3 * (5.0 * phase).sin())
                * 300.0;
            decode_mem1[i] = decode_mem0[i];
        }
        let history_refs_400: [&[f32]; 2] = [&decode_mem0, &decode_mem1];
        let pitch_period_400 = estimate_pitch_period(&history_refs_400);
        assert_eq!(pitch_period_400, 400);
        return;
    }
}
