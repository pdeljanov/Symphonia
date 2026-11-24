// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A reference implementation of the parametric stereo signal processing for the tests.
//!
//! The reference processes one hybrid band k at a time over all QMF slots n of a frame, following
//! the formulas of 8.6.4.3 to 8.6.4.7 band by band: hybrid analysis into s_k(n), transient
//! detection, de-correlation into d_k(n), mixing with mixing matrices interpolated per band, and
//! hybrid synthesis by accumulation. The optimized implementation processes one QMF slot of all
//! hybrid bands at a time and must produce the same output.

use symphonia_core::dsp::complex::Complex;

use crate::aac::sbr::types::{QmfFrame, QmfSlots};

use super::PsDecoder;
use super::decorrelation::allpass_coeffs;
use super::hybrid;
use super::stereo::MixCoeffs;
use super::tables::{
    A_SMOOTH, ALLPASS_D, ALLPASS_PRE_DELAY, HYBRID_FILTER_DELAY, HYBRID_FILTER_LEN, LONG_DELAY,
    MAX_NR_BANDS, MAX_NR_PAR_BANDS, MAX_NUM_ENV, MAX_SPLIT_QMF_BANDS, NR_ALLPASS_LINKS,
    NUM_QMF_SLOTS, PEAK_DECAY_ALPHA, SHORT_DELAY, StereoBands, TRANSIENT_IMPACT_GAMMA,
};

/// The length of the delay line state of one hybrid band. An all-pass filtered band holds the
/// pre-delay and the three filter links (2 + 3 + 4 + 5 samples), a delayed band holds up to
/// LONG_DELAY samples.
const STATE_LEN: usize = 14;

/// The offsets of the filter link states within the state of an all-pass filtered band.
const LINK_OFFSETS: [usize; NR_ALLPASS_LINKS + 1] = [
    ALLPASS_PRE_DELAY,
    ALLPASS_PRE_DELAY + ALLPASS_D[0],
    ALLPASS_PRE_DELAY + ALLPASS_D[0] + ALLPASS_D[1],
    ALLPASS_PRE_DELAY + ALLPASS_D[0] + ALLPASS_D[1] + ALLPASS_D[2],
];

/// Delay `x` in place by `state.len()` samples. `state` holds the most recent past input samples,
/// oldest first.
fn delay_in_place(state: &mut [Complex<f32>], x: &mut [Complex<f32>; NUM_QMF_SLOTS]) {
    let d = state.len();
    let mut ext = [Complex::new(0.0, 0.0); STATE_LEN + NUM_QMF_SLOTS];
    ext[..d].copy_from_slice(state);
    ext[d..d + NUM_QMF_SLOTS].copy_from_slice(x);
    x.copy_from_slice(&ext[..NUM_QMF_SLOTS]);
    state.copy_from_slice(&ext[NUM_QMF_SLOTS..NUM_QMF_SLOTS + d]);
}

/// Filter `x` in place with one all-pass filter link (Q z^-d - a g) / (1 - a g Q z^-d)
/// (8.6.4.5.2), where d = `state.len()`, as w(n) = x(n) + a g Q w(n - d) and y(n) = Q w(n - d) -
/// a g w(n). `state` holds the past values of w, oldest first.
fn allpass_link(
    state: &mut [Complex<f32>],
    q: Complex<f32>,
    a_g: f32,
    x: &mut [Complex<f32>; NUM_QMF_SLOTS],
) {
    let d = state.len();
    let mut w = [Complex::new(0.0, 0.0); STATE_LEN + NUM_QMF_SLOTS];
    w[..d].copy_from_slice(state);
    for (n, x_n) in x.iter_mut().enumerate() {
        let q_w_delayed = q * w[n];
        let w_n = *x_n + q_w_delayed * a_g;
        w[n + d] = w_n;
        *x_n = q_w_delayed - w_n * a_g;
    }
    state.copy_from_slice(&w[NUM_QMF_SLOTS..NUM_QMF_SLOTS + d]);
}

/// The band-wise de-correlator (8.6.4.5).
pub(super) struct ReferenceDecorrelator {
    delay: Vec<[Complex<f32>; STATE_LEN]>,
    peak_decay_nrg: [f32; MAX_NR_PAR_BANDS],
    smooth_nrg: [f32; MAX_NR_PAR_BANDS],
    smooth_peak_decay_diff_nrg: [f32; MAX_NR_PAR_BANDS],
}

impl ReferenceDecorrelator {
    pub fn new() -> Self {
        ReferenceDecorrelator {
            delay: vec![[Complex::new(0.0, 0.0); STATE_LEN]; MAX_NR_BANDS],
            peak_decay_nrg: [0.0; MAX_NR_PAR_BANDS],
            smooth_nrg: [0.0; MAX_NR_PAR_BANDS],
            smooth_peak_decay_diff_nrg: [0.0; MAX_NR_PAR_BANDS],
        }
    }

    pub fn reset(&mut self) {
        self.reset_bands(0);
        self.peak_decay_nrg = [0.0; MAX_NR_PAR_BANDS];
        self.smooth_nrg = [0.0; MAX_NR_PAR_BANDS];
        self.smooth_peak_decay_diff_nrg = [0.0; MAX_NR_PAR_BANDS];
    }

    pub fn reset_bands(&mut self, k_max: usize) {
        for state in self.delay.iter_mut().skip(k_max) {
            *state = [Complex::new(0.0, 0.0); STATE_LEN];
        }
    }

    /// Transient detection (8.6.4.5.3) of a frame: G_TransientRatio(i, n) is written to
    /// `gain[i][n]`.
    pub fn transient_gains(
        &mut self,
        config: StereoBands,
        s: &[[Complex<f32>; NUM_QMF_SLOTS]],
        gain: &mut [[f32; NUM_QMF_SLOTS]; MAX_NR_PAR_BANDS],
    ) {
        for g in gain.iter_mut() {
            g.fill(0.0);
        }
        for (s_k, &b) in s.iter().zip(config.param_index_table()) {
            let p = &mut gain[usize::from(b)];
            for (p_n, s_kn) in p.iter_mut().zip(s_k) {
                *p_n += s_kn.norm_sqr();
            }
        }

        let alpha = PEAK_DECAY_ALPHA as f32;
        let a = A_SMOOTH;
        let gamma = TRANSIENT_IMPACT_GAMMA;

        for i in 0..config.nr_par_bands() {
            for g_n in gain[i].iter_mut() {
                let p = *g_n;
                let decayed = alpha * self.peak_decay_nrg[i];
                self.peak_decay_nrg[i] = if decayed < p { p } else { decayed };
                self.smooth_nrg[i] = a * p + (1.0 - a) * self.smooth_nrg[i];
                self.smooth_peak_decay_diff_nrg[i] = a * (self.peak_decay_nrg[i] - p)
                    + (1.0 - a) * self.smooth_peak_decay_diff_nrg[i];
                let diff = self.smooth_peak_decay_diff_nrg[i];
                *g_n = if gamma * diff > self.smooth_nrg[i] {
                    self.smooth_nrg[i] / (gamma * diff)
                }
                else {
                    1.0
                };
            }
        }
    }

    /// De-correlation (8.6.4.5.2, 8.6.4.5.4) of hybrid band `k` for a frame.
    pub fn decorrelate(
        &mut self,
        config: StereoBands,
        k: usize,
        s_k: &[Complex<f32>; NUM_QMF_SLOTS],
        gain: &[f32; NUM_QMF_SLOTS],
        d_k: &mut [Complex<f32>; NUM_QMF_SLOTS],
    ) {
        d_k.copy_from_slice(s_k);
        let state = &mut self.delay[k];

        if k < config.nr_allpass_bands() {
            let coeffs = allpass_coeffs(config);
            delay_in_place(&mut state[..ALLPASS_PRE_DELAY], d_k);
            let phi = coeffs.phi_fract[k];
            for x in d_k.iter_mut() {
                *x *= phi;
            }
            for m in 0..NR_ALLPASS_LINKS {
                let link_state = &mut state[LINK_OFFSETS[m]..LINK_OFFSETS[m + 1]];
                allpass_link(link_state, coeffs.q_fract[m][k], coeffs.a_g[m][k], d_k);
            }
        }
        else {
            let len = if k < config.short_delay_band() { LONG_DELAY } else { SHORT_DELAY };
            delay_in_place(&mut state[..len], d_k);
        }

        for (x, &g) in d_k.iter_mut().zip(gain) {
            *x *= g;
        }
    }
}

/// Mix one hybrid band with the mixing matrices interpolated between the parameter positions
/// (8.6.4.6.2, 8.6.4.6.4). `start` is H(k, n_-1), and `envs` holds the parameter positions n_e
/// with H(k, n_e).
pub(super) fn mix_band(
    start: &MixCoeffs,
    envs: &[(usize, MixCoeffs)],
    s_k: &[Complex<f32>; NUM_QMF_SLOTS],
    d_k: &[Complex<f32>; NUM_QMF_SLOTS],
    l_k: &mut [Complex<f32>; NUM_QMF_SLOTS],
    r_k: &mut [Complex<f32>; NUM_QMF_SLOTS],
) {
    let mut apply = |n: usize, h: &MixCoeffs| {
        l_k[n] = h[0] * s_k[n] + h[2] * d_k[n];
        r_k[n] = h[1] * s_k[n] + h[3] * d_k[n];
    };

    // Slots are numbered from 1 here, such that slot 0 is the last slot of the previous frame,
    // where `start` applies (8.6.4.4).
    let mut n = 0;
    let mut from = *start;
    let mut n_from = 0;
    for &(n_to, to) in envs {
        let n_to = n_to.min(NUM_QMF_SLOTS - 1) + 1;
        let span = n_to.saturating_sub(n_from);
        while n < n_to {
            let h = if span == 0 {
                to
            }
            else {
                let t = (n + 1).saturating_sub(n_from) as f32 / span as f32;
                std::array::from_fn(|c| from[c] + (to[c] - from[c]) * t)
            };
            apply(n, &h);
            n += 1;
        }
        from = to;
        n_from = n_to;
    }

    while n < NUM_QMF_SLOTS {
        apply(n, &from);
        n += 1;
    }
}

/// Hybrid synthesis (8.6.4.7): add l_k(n) and r_k(n) of hybrid band `k` to its QMF subband.
fn synthesize_band(
    config: StereoBands,
    k: usize,
    l_k: &[Complex<f32>; NUM_QMF_SLOTS],
    r_k: &[Complex<f32>; NUM_QMF_SLOTS],
    left: &mut QmfSlots,
    right: &mut QmfSlots,
) {
    let p = config.qmf_band(k);
    for (row, &x) in left.iter_mut().zip(l_k) {
        row[p] += x;
    }
    for (row, &x) in right.iter_mut().zip(r_k) {
        row[p] += x;
    }
}

/// The band-wise reference implementation of the PS signal processing. The bitstream decoding and
/// the stereo parameter state are those of a `PsDecoder`.
pub(super) struct ReferencePs {
    pub dec: PsDecoder,
    decorrelator: ReferenceDecorrelator,
    history: [[Complex<f32>; HYBRID_FILTER_DELAY]; MAX_SPLIT_QMF_BANDS],
}

impl ReferencePs {
    pub fn new() -> Self {
        ReferencePs {
            dec: PsDecoder::new(),
            decorrelator: ReferenceDecorrelator::new(),
            history: [[Complex::new(0.0, 0.0); HYBRID_FILTER_DELAY]; MAX_SPLIT_QMF_BANDS],
        }
    }

    /// Hybrid analysis (8.6.4.3) of all hybrid bands into `s[k][n]`.
    fn hybrid_analysis(
        &self,
        config: StereoBands,
        input: &QmfFrame,
        s: &mut [[Complex<f32>; NUM_QMF_SLOTS]],
    ) {
        const EXT_LEN: usize = 2 * HYBRID_FILTER_DELAY + NUM_QMF_SLOTS;
        let mut ext = [[Complex::new(0.0, 0.0); EXT_LEN]; MAX_SPLIT_QMF_BANDS];
        for (p, x) in ext.iter_mut().enumerate() {
            x[..HYBRID_FILTER_DELAY].copy_from_slice(&self.history[p]);
            for (x, row) in x[HYBRID_FILTER_DELAY..].iter_mut().zip(input.iter()) {
                *x = row[p];
            }
        }

        let filters = hybrid::filters(config);
        for ((s_k, filter), &p) in s.iter_mut().zip(filters).zip(config.sub_qmf_band_table()) {
            let x = &ext[usize::from(p)];
            for (n, s_kn) in s_k.iter_mut().enumerate() {
                let window = &x[n..n + HYBRID_FILTER_LEN];
                *s_kn = filter.iter().zip(window.iter().rev()).map(|(&g, &x)| g * x).sum();
            }
        }

        let offset = config.qmf_to_hybrid_offset();
        for k in config.num_sub_qmf_bands()..config.nr_bands() {
            for (s_kn, row) in s[k].iter_mut().zip(input.iter()) {
                *s_kn = row[k - offset];
            }
        }
    }

    fn update_history(&mut self, input: &QmfFrame) {
        let rows = &input[NUM_QMF_SLOTS - HYBRID_FILTER_DELAY..NUM_QMF_SLOTS];
        for (p, history) in self.history.iter_mut().enumerate() {
            for (h, row) in history.iter_mut().zip(rows) {
                *h = row[p];
            }
        }
    }

    /// Process one frame like `PsDecoder::process`.
    pub fn process(
        &mut self,
        input: &QmfFrame,
        ps_data_present: bool,
        kx_plus_m: usize,
        left: &mut QmfSlots,
        right: &mut QmfSlots,
    ) {
        let dec = &mut self.dec;
        let frame_valid = std::mem::replace(&mut dec.frame_valid, false);

        if !(ps_data_present && frame_valid && dec.syntax.synced) {
            left.copy_from_slice(&input[..NUM_QMF_SLOTS]);
            right.copy_from_slice(&input[..NUM_QMF_SLOTS]);
            self.update_history(input);
            self.dec.stereo.set_mono();
            self.dec.prev_frame_processed = false;
            return;
        }

        let config = dec.frame.header.stereo_bands().unwrap_or(dec.stereo.config());
        if config != dec.stereo.config() || !dec.prev_frame_processed {
            self.decorrelator.reset();
        }
        dec.stereo.set_config(config);
        let k_max = kx_plus_m.saturating_add(config.qmf_to_hybrid_offset());
        self.decorrelator.reset_bands(k_max);

        let zero = Complex::new(0.0, 0.0);
        let mut s = vec![[zero; NUM_QMF_SLOTS]; MAX_NR_BANDS];
        let mut gain = [[0.0; NUM_QMF_SLOTS]; MAX_NR_PAR_BANDS];

        self.hybrid_analysis(config, input, &mut s);
        self.update_history(input);
        self.decorrelator.transient_gains(config, &s, &mut gain);

        let dec = &mut self.dec;
        dec.stereo.prepare(&dec.frame);

        for row in left.iter_mut().chain(right.iter_mut()) {
            row.fill(zero);
        }

        for (k, (s_k, &b)) in s.iter().zip(config.param_index_table()).enumerate() {
            let b = usize::from(b);
            let mut d_k = [zero; NUM_QMF_SLOTS];
            self.decorrelator.decorrelate(config, k, s_k, &gain[b], &mut d_k);

            let mut start: MixCoeffs = [zero; 4];
            let mut envs = [(0, [zero; 4]); MAX_NUM_ENV];
            let num_pos = dec.stereo.band_coeffs(b, config.is_conjugated(k), &mut start, &mut envs);

            let mut l_k = [zero; NUM_QMF_SLOTS];
            let mut r_k = [zero; NUM_QMF_SLOTS];
            mix_band(&start, &envs[..num_pos], s_k, &d_k, &mut l_k, &mut r_k);
            synthesize_band(config, k, &l_k, &r_k, left, right);
        }

        dec.stereo.finish();
        dec.prev_frame_processed = true;
    }
}
