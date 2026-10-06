// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! De-correlation (ISO/IEC 14496-3:2009, 8.6.4.5): all-pass filtering with fractional delays,
//! delays, transient detection, and transient reduction.
//!
//! The de-correlator processes one QMF slot n of all hybrid bands k at a time. All hybrid bands
//! share the same delays, so the filters of all bands are evaluated together, band after band, by
//! the inner loops.

use std::f64::consts::PI;

use lazy_static::lazy_static;
use symphonia_core::dsp::complex::Complex;

use super::tables::{
    A_SMOOTH, ALLPASS_A, ALLPASS_D, ALLPASS_PRE_DELAY, DECAY_SLOPE, FRACT_DELAY_Q,
    FRACT_DELAY_Q_PHI, LONG_DELAY, MAX_NR_BANDS, MAX_NR_PAR_BANDS, MAX_NUM_SUB_QMF_BANDS,
    NR_ALLPASS_LINKS, NUM_QMF_SLOTS, PEAK_DECAY_ALPHA, SHORT_DELAY, StereoBands,
    TRANSIENT_IMPACT_GAMMA, f_center,
};
use crate::aac::sbr::types::{QMF_BANDS, QmfFrame};

/// The transient attenuator G_TransientRatio(i, n) of a frame, indexed `[i][n]`.
pub(super) type TransientGains = [[f32; NUM_QMF_SLOTS]; MAX_NR_PAR_BANDS];

/// The maximum number of all-pass filtered bands, NR_ALLPASS_BANDS of the 34 stereo bands
/// configuration.
pub(super) const MAX_ALLPASS_BANDS: usize = 50;

/// The number of QMF slots of s_k(n) kept by the de-correlator, the current slot and the past
/// slots. A power of two greater than LONG_DELAY.
const S_HISTORY: usize = 16;

/// The number of QMF slots of the state w of each all-pass filter link kept by the de-correlator.
/// A power of two greater than the longest link delay.
const W_HISTORY: usize = 8;

const _: () = assert!(
    LONG_DELAY < S_HISTORY
        && ALLPASS_PRE_DELAY < S_HISTORY
        && SHORT_DELAY < S_HISTORY
        && ALLPASS_D[0] < W_HISTORY
        && ALLPASS_D[1] < W_HISTORY
        && ALLPASS_D[2] < W_HISTORY
);

/// The real and imaginary parts of complex values of all all-pass filtered bands.
#[derive(Clone, Copy)]
struct SplitComplex {
    re: [f32; MAX_ALLPASS_BANDS],
    im: [f32; MAX_ALLPASS_BANDS],
}

impl SplitComplex {
    const ZERO: SplitComplex =
        SplitComplex { re: [0.0; MAX_ALLPASS_BANDS], im: [0.0; MAX_ALLPASS_BANDS] };

    fn from_complex(x: &[Complex<f32>; MAX_ALLPASS_BANDS]) -> Self {
        SplitComplex { re: x.map(|x| x.re), im: x.map(|x| x.im) }
    }
}

/// The time invariant coefficients of the all-pass filters of one configuration (8.6.4.5.2).
pub(super) struct AllpassCoeffs {
    /// φ_Fract(k).
    pub phi_fract: [Complex<f32>; MAX_ALLPASS_BANDS],
    /// Q_Fract_allpass(k, m), indexed `[m][k]`.
    pub q_fract: [[Complex<f32>; MAX_ALLPASS_BANDS]; NR_ALLPASS_LINKS],
    /// a(m) g_DecaySlope(k), indexed `[m][k]`.
    pub a_g: [[f32; MAX_ALLPASS_BANDS]; NR_ALLPASS_LINKS],
    /// φ_Fract(k), with separate real and imaginary parts.
    phi_fract_split: SplitComplex,
    /// Q_Fract_allpass(k, m), with separate real and imaginary parts, indexed `[m]`.
    q_fract_split: [SplitComplex; NR_ALLPASS_LINKS],
}

impl AllpassCoeffs {
    fn new(config: StereoBands) -> Self {
        let mut coeffs = AllpassCoeffs {
            phi_fract: [Complex::new(0.0, 0.0); MAX_ALLPASS_BANDS],
            q_fract: [[Complex::new(0.0, 0.0); MAX_ALLPASS_BANDS]; NR_ALLPASS_LINKS],
            a_g: [[0.0; MAX_ALLPASS_BANDS]; NR_ALLPASS_LINKS],
            phi_fract_split: SplitComplex::ZERO,
            q_fract_split: [SplitComplex::ZERO; NR_ALLPASS_LINKS],
        };

        let cis = |phase: f64| Complex::new(phase.cos() as f32, phase.sin() as f32);
        let decay_cutoff = config.decay_cutoff();

        for k in 0..config.nr_allpass_bands() {
            let f = f_center(config, k);
            let g_decay_slope = if k > decay_cutoff {
                (1.0 - DECAY_SLOPE * (k - decay_cutoff) as f64).max(0.0)
            }
            else {
                1.0
            };
            coeffs.phi_fract[k] = cis(-PI * FRACT_DELAY_Q_PHI * f);
            for m in 0..NR_ALLPASS_LINKS {
                coeffs.q_fract[m][k] = cis(-PI * FRACT_DELAY_Q[m] * f);
                coeffs.a_g[m][k] = (ALLPASS_A[m] * g_decay_slope) as f32;
            }
        }

        coeffs.phi_fract_split = SplitComplex::from_complex(&coeffs.phi_fract);
        coeffs.q_fract_split = coeffs.q_fract.each_ref().map(SplitComplex::from_complex);

        coeffs
    }
}

lazy_static! {
    static ref ALLPASS_COEFFS: [AllpassCoeffs; 2] =
        [AllpassCoeffs::new(StereoBands::Bands20), AllpassCoeffs::new(StereoBands::Bands34)];
}

pub(super) fn allpass_coeffs(config: StereoBands) -> &'static AllpassCoeffs {
    match config {
        StereoBands::Bands20 => &ALLPASS_COEFFS[0],
        StereoBands::Bands34 => &ALLPASS_COEFFS[1],
    }
}

/// Get a shared reference to row `read` and a mutable reference to row `write` of `rows`. The rows
/// must be distinct.
fn split_rows<T>(rows: &mut [T], read: usize, write: usize) -> (&T, &mut T) {
    if read < write {
        let (head, tail) = rows.split_at_mut(write);
        (&head[read], &mut tail[0])
    }
    else {
        let (head, tail) = rows.split_at_mut(read);
        (&tail[0], &mut head[write])
    }
}

/// Evaluate one all-pass filter link (Q z^-d - a g) / (1 - a g Q z^-d) (8.6.4.5.2) for one QMF
/// slot of all all-pass filtered bands as w(n) = x(n) + a g Q w(n - d) and y(n) = Q w(n - d) -
/// a g w(n).
///
/// The real and imaginary parts are given in separate slices: the input x(n), which is replaced by
/// the output y(n), the new state w(n), the state w(n - d), the coefficients Q, and a g. The
/// complex arithmetic is that of `Complex`.
///
/// The function is not inlined: its slice arguments do not alias, which allows the loop to be
/// vectorized.
#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn allpass_link(
    x_re: &mut [f32],
    x_im: &mut [f32],
    w_n_re: &mut [f32],
    w_n_im: &mut [f32],
    w_d_re: &[f32],
    w_d_im: &[f32],
    q_re: &[f32],
    q_im: &[f32],
    a_g: &[f32],
) {
    let x = x_re.iter_mut().zip(x_im.iter_mut());
    let w_n = w_n_re.iter_mut().zip(w_n_im.iter_mut());
    let w_d = w_d_re.iter().zip(w_d_im);
    let q = q_re.iter().zip(q_im).zip(a_g);

    for (((x_re, x_im), (w_n_re, w_n_im)), ((&w_d_re, &w_d_im), ((&q_re, &q_im), &a_g))) in
        x.zip(w_n).zip(w_d.zip(q))
    {
        let q_w_re = q_re * w_d_re - q_im * w_d_im;
        let q_w_im = q_re * w_d_im + q_im * w_d_re;
        let w_re = *x_re + q_w_re * a_g;
        let w_im = *x_im + q_w_im * a_g;
        *w_n_re = w_re;
        *w_n_im = w_im;
        *x_re = q_w_re - w_re * a_g;
        *x_im = q_w_im - w_im * a_g;
    }
}

/// The de-correlator state.
pub(super) struct Decorrelator {
    /// s_k(n) of the current and past QMF slots, a ring buffer indexed `[n mod S_HISTORY][k]`.
    s: Box<[[Complex<f32>; MAX_NR_BANDS]; S_HISTORY]>,
    /// The state w of each all-pass filter link (see `process_slot`), a ring buffer indexed
    /// `[m][n mod W_HISTORY]`.
    w: Box<[[SplitComplex; W_HISTORY]; NR_ALLPASS_LINKS]>,
    /// The index n of the next QMF slot, modulo a multiple of the ring buffer lengths.
    slot: usize,
    /// P_PeakDecayNrg(i, n - 1).
    peak_decay_nrg: [f32; MAX_NR_PAR_BANDS],
    /// P_SmoothNrg(i, n - 1).
    smooth_nrg: [f32; MAX_NR_PAR_BANDS],
    /// P_SmoothPeakDecayDiffNrg(i, n - 1).
    smooth_peak_decay_diff_nrg: [f32; MAX_NR_PAR_BANDS],
}

impl Decorrelator {
    pub fn new() -> Self {
        Decorrelator {
            s: Box::new([[Complex::new(0.0, 0.0); MAX_NR_BANDS]; S_HISTORY]),
            w: Box::new([[SplitComplex::ZERO; W_HISTORY]; NR_ALLPASS_LINKS]),
            slot: 0,
            peak_decay_nrg: [0.0; MAX_NR_PAR_BANDS],
            smooth_nrg: [0.0; MAX_NR_PAR_BANDS],
            smooth_peak_decay_diff_nrg: [0.0; MAX_NR_PAR_BANDS],
        }
    }

    /// Full reset of the de-correlator state: all past samples s_k(n) and d_k(n) are zero
    /// (Annex 8.A.3, 8.6.4.6.1).
    pub fn reset(&mut self) {
        self.reset_bands(0);
        self.peak_decay_nrg = [0.0; MAX_NR_PAR_BANDS];
        self.smooth_nrg = [0.0; MAX_NR_PAR_BANDS];
        self.smooth_peak_decay_diff_nrg = [0.0; MAX_NR_PAR_BANDS];
    }

    /// Partial reset of the de-correlator state: all past samples s_k(n) and d_k(n) of hybrid bands
    /// k >= `k_max` are zero (Annex 8.A.3).
    ///
    /// The transient detector state is kept: it is held per stereo band and is shared with hybrid
    /// bands below k_max.
    pub fn reset_bands(&mut self, k_max: usize) {
        for row in self.s.iter_mut() {
            if let Some(row) = row.get_mut(k_max..) {
                row.fill(Complex::new(0.0, 0.0));
            }
        }
        for w in self.w.iter_mut().flatten() {
            for part in [&mut w.re, &mut w.im] {
                if let Some(part) = part.get_mut(k_max..) {
                    part.fill(0.0);
                }
            }
        }
    }

    /// Returns true if the delay line state of all hybrid bands k >= `k_min` is zero.
    #[cfg(test)]
    pub fn is_zero_from(&self, k_min: usize) -> bool {
        let zero =
            |row: &[Complex<f32>]| row.iter().skip(k_min).all(|x| x.re == 0.0 && x.im == 0.0);
        let zero_split = |w: &SplitComplex| {
            w.re.iter().skip(k_min).chain(w.im.iter().skip(k_min)).all(|&x| x == 0.0)
        };
        self.s.iter().all(|row| zero(row)) && self.w.iter().flatten().all(zero_split)
    }

    /// Process the next QMF slot n.
    ///
    /// The hybrid sub subband samples s_k(n) of the slot are the samples `sub` of the sub subbands
    /// of the split QMF subbands, followed by the unsplit QMF subbands of the QMF slot `qmf`.
    ///
    /// Computes the all-pass filtered or delayed signal of each hybrid band k (8.6.4.5.2) and
    /// writes it to `d[k]`. The transient reduction (8.6.4.5.4), d_k(n) = G_TransientRatio(b(k),
    /// n) * `d[k]`, is left to the caller (see [`Decorrelator::transient_gains`]).
    ///
    /// Returns s_k(n).
    pub fn process_slot(
        &mut self,
        config: StereoBands,
        sub: &[Complex<f32>; MAX_NUM_SUB_QMF_BANDS],
        qmf: &[Complex<f32>; QMF_BANDS],
        d: &mut [Complex<f32>; MAX_NR_BANDS],
    ) -> &[Complex<f32>; MAX_NR_BANDS] {
        let n = self.slot;
        self.slot = (n + 1) % (S_HISTORY * W_HISTORY);

        let nr_bands = config.nr_bands();
        let num_sub = config.num_sub_qmf_bands();
        let offset = config.qmf_to_hybrid_offset();

        // Store s_k(n).
        let s = &mut self.s[n % S_HISTORY];
        s[..num_sub].copy_from_slice(&sub[..num_sub]);
        s[num_sub..nr_bands].copy_from_slice(&qmf[num_sub - offset..nr_bands - offset]);

        let s_hist = &*self.s;
        let s_at = |delay: usize| &s_hist[(n + S_HISTORY - delay) % S_HISTORY];

        // All-pass filtered bands, k < NR_ALLPASS_BANDS:
        //
        // H_k(z) = z^-2 φ_Fract(k) Π_m (Q(k, m) z^-d(m) - a(m) g(k)) /
        //                            (1 - a(m) g(k) Q(k, m) z^-d(m)).
        //
        // Each link is implemented as w(n) = x(n) + a g Q w(n - d), y(n) = Q w(n - d) - a g w(n).
        let num_allpass = config.nr_allpass_bands();
        let coeffs = allpass_coeffs(config);

        let (d_allpass, d_delay) = d.split_at_mut(num_allpass);

        // The real and imaginary parts of the signal, the filter states, and the coefficients are
        // held in separate arrays. The complex arithmetic is that of `Complex`.
        let na = num_allpass;
        let mut x = SplitComplex::ZERO;

        // x = s(n - 2) * φ_Fract.
        let phi = &coeffs.phi_fract_split;
        let src = s_at(ALLPASS_PRE_DELAY)[..na].iter().zip(phi.re[..na].iter().zip(&phi.im[..na]));
        for ((x_re, x_im), (s, (&phi_re, &phi_im))) in
            x.re[..na].iter_mut().zip(x.im[..na].iter_mut()).zip(src)
        {
            *x_re = s.re * phi_re - s.im * phi_im;
            *x_im = s.re * phi_im + s.im * phi_re;
        }

        // The filter links.
        for (m, w) in self.w.iter_mut().enumerate() {
            let read = (n + W_HISTORY - ALLPASS_D[m]) % W_HISTORY;
            let (w_d, w_n) = split_rows(w, read, n % W_HISTORY);
            let q = &coeffs.q_fract_split[m];

            allpass_link(
                &mut x.re[..na],
                &mut x.im[..na],
                &mut w_n.re[..na],
                &mut w_n.im[..na],
                &w_d.re[..na],
                &w_d.im[..na],
                &q.re[..na],
                &q.im[..na],
                &coeffs.a_g[m][..na],
            );
        }

        for ((d, &x_re), &x_im) in d_allpass.iter_mut().zip(&x.re).zip(&x.im) {
            *d = Complex::new(x_re, x_im);
        }

        // Delayed bands, H_k(z) = z^-D(k).
        let short_delay_band = config.short_delay_band();
        let (d_long, d_short) = d_delay.split_at_mut(short_delay_band - num_allpass);
        d_long.copy_from_slice(&s_at(LONG_DELAY)[num_allpass..short_delay_band]);
        d_short[..nr_bands - short_delay_band]
            .copy_from_slice(&s_at(SHORT_DELAY)[short_delay_band..nr_bands]);

        &self.s[n % S_HISTORY]
    }

    /// Transient detection (8.6.4.5.3): compute the transient attenuator G_TransientRatio(i, n) of
    /// all QMF slots n of a frame for all stereo bands i, and write it to `gain[i][n]`.
    ///
    /// The hybrid sub subband samples s_k(n) are `sub[n][k]` for the sub subbands of the split QMF
    /// subbands, and the unsplit QMF subbands of `input` for the other hybrid bands.
    pub fn transient_gains(
        &mut self,
        config: StereoBands,
        sub: &[[Complex<f32>; MAX_NUM_SUB_QMF_BANDS]; NUM_QMF_SLOTS],
        input: &QmfFrame,
        gain: &mut TransientGains,
    ) {
        // The input power P(i, n), accumulated in `gain` in the order of increasing k, for all n
        // at once.
        for g in gain.iter_mut() {
            g.fill(0.0);
        }

        let num_sub = config.num_sub_qmf_bands();
        let offset = config.qmf_to_hybrid_offset();

        for (k, &b) in config.param_index_table().iter().enumerate() {
            let p = &mut gain[usize::from(b)];
            if k < num_sub {
                for (p, sub) in p.iter_mut().zip(sub) {
                    *p += sub[k].norm_sqr();
                }
            }
            else {
                for (p, x) in p.iter_mut().zip(input) {
                    *p += x[k - offset].norm_sqr();
                }
            }
        }

        let alpha = PEAK_DECAY_ALPHA as f32;
        let a = A_SMOOTH;
        let gamma = TRANSIENT_IMPACT_GAMMA;

        let states = self
            .peak_decay_nrg
            .iter_mut()
            .zip(self.smooth_nrg.iter_mut())
            .zip(self.smooth_peak_decay_diff_nrg.iter_mut());

        for (g, ((peak_decay_nrg, smooth_nrg), smooth_diff_nrg)) in
            gain.iter_mut().zip(states).take(config.nr_par_bands())
        {
            for g_n in g.iter_mut() {
                let p = *g_n;
                // Peak decay.
                let decayed = alpha * *peak_decay_nrg;
                *peak_decay_nrg = if decayed < p { p } else { decayed };
                // Smoothing with H_Smooth(z) = a / (1 + (a - 1) z^-1).
                *smooth_nrg = a * p + (1.0 - a) * *smooth_nrg;
                *smooth_diff_nrg = a * (*peak_decay_nrg - p) + (1.0 - a) * *smooth_diff_nrg;
                // The transient attenuator.
                *g_n = if gamma * *smooth_diff_nrg > *smooth_nrg {
                    *smooth_nrg / (gamma * *smooth_diff_nrg)
                }
                else {
                    1.0
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::reference::ReferenceDecorrelator;
    use super::super::tables::NUM_QMF_SLOTS;
    use super::*;

    type Frame = Vec<[Complex<f32>; NUM_QMF_SLOTS]>;

    const ZERO: Complex<f32> = Complex::new(0.0, 0.0);

    /// A simple linear congruential generator.
    struct Lcg(u64);

    impl Lcg {
        fn next_f32(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
        }
    }

    /// Evaluate H_k(e^jω) of 8.6.4.5.2 directly from its definition.
    fn transfer_function(config: StereoBands, k: usize, omega: f64) -> Complex<f64> {
        let z_inv = |d: f64| Complex::from_polar(1.0, -omega * d);
        if k >= config.nr_allpass_bands() {
            let d = if k < config.short_delay_band() { 14.0 } else { 1.0 };
            return z_inv(d);
        }
        let f = f_center(config, k);
        let cutoff = config.decay_cutoff();
        let g = if k > cutoff { (1.0 - 0.05 * (k - cutoff) as f64).max(0.0) } else { 1.0 };
        let a = [0.65143905753106, 0.56471812200776, 0.48954165955695];
        let d = [3.0, 4.0, 5.0];
        let q = [0.43, 0.75, 0.347];
        let mut h = z_inv(2.0) * Complex::from_polar(1.0, -PI * 0.39 * f);
        for m in 0..3 {
            let q_fract = Complex::from_polar(1.0, -PI * q[m] * f);
            let num = q_fract * z_inv(d[m]) - a[m] * g;
            let den = Complex::new(1.0, 0.0) - a[m] * g * q_fract * z_inv(d[m]);
            h *= num / den;
        }
        h
    }

    /// Process a frame of hybrid sub subband samples `s[k][n]` slot by slot. Returns d_k(n) with
    /// the transient reduction applied, indexed `[k][n]`, and G_TransientRatio(i, n), indexed
    /// `[i][n]`.
    fn run_frame(
        dec: &mut Decorrelator,
        config: StereoBands,
        s: &[[Complex<f32>; NUM_QMF_SLOTS]],
    ) -> (Frame, TransientGains) {
        let num_sub = config.num_sub_qmf_bands();
        let offset = config.qmf_to_hybrid_offset();
        let param_index = config.param_index_table();

        // The frame in the layout of the PS decoder.
        let mut sub = Box::new([[ZERO; MAX_NUM_SUB_QMF_BANDS]; NUM_QMF_SLOTS]);
        let mut input: Box<QmfFrame> = Box::new([[ZERO; QMF_BANDS]; NUM_QMF_SLOTS + 6]);
        for n in 0..NUM_QMF_SLOTS {
            for k in 0..config.nr_bands() {
                if k < num_sub {
                    sub[n][k] = s[k][n];
                }
                else {
                    input[n][k - offset] = s[k][n];
                }
            }
        }

        let mut gain = [[0.0; NUM_QMF_SLOTS]; MAX_NR_PAR_BANDS];
        dec.transient_gains(config, &sub, &input, &mut gain);

        let mut d_out = vec![[ZERO; NUM_QMF_SLOTS]; MAX_NR_BANDS];
        for n in 0..NUM_QMF_SLOTS {
            let mut d = [ZERO; MAX_NR_BANDS];
            let s_n = dec.process_slot(config, &sub[n], &input[n], &mut d);
            for k in 0..config.nr_bands() {
                assert_eq!(s_n[k], s[k][n]);
                d_out[k][n] = d[k] * gain[usize::from(param_index[k])][n];
            }
        }

        (d_out, gain)
    }

    #[test]
    fn verify_decorrelator_frequency_response() {
        // Filter a complex exponential in each hybrid band and compare the steady state response
        // with H_k(e^jω).
        for config in [StereoBands::Bands20, StereoBands::Bands34] {
            let omega = |k: usize| 0.37 + 0.11 * k as f64;
            let mut dec = Decorrelator::new();
            let num_frames = 40;
            for f in 0..num_frames {
                let s: Frame = (0..MAX_NR_BANDS)
                    .map(|k| {
                        std::array::from_fn(|n| {
                            let t = (f * NUM_QMF_SLOTS + n) as f64;
                            Complex::new((omega(k) * t).cos() as f32, (omega(k) * t).sin() as f32)
                        })
                    })
                    .collect();
                let (d, gain) = run_frame(&mut dec, config, &s);
                // The input is stationary, so it is never attenuated.
                assert!(gain[..config.nr_par_bands()].iter().flatten().all(|&g| g == 1.0));
                if f == num_frames - 1 {
                    for k in 0..config.nr_bands() {
                        let h = transfer_function(config, k, omega(k));
                        for n in 0..NUM_QMF_SLOTS {
                            let x = Complex::new(f64::from(s[k][n].re), f64::from(s[k][n].im));
                            let expected = h * x;
                            let err = ((f64::from(d[k][n].re) - expected.re).powi(2)
                                + (f64::from(d[k][n].im) - expected.im).powi(2))
                            .sqrt();
                            assert!(err < 1e-3, "config {config:?}, k {k}, n {n}, err {err}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn verify_allpass_magnitude() {
        // The all-pass filters have unit magnitude response.
        for config in [StereoBands::Bands20, StereoBands::Bands34] {
            for k in 0..config.nr_bands() {
                for i in 0..16 {
                    let omega = -PI + 2.0 * PI * i as f64 / 16.0 + 0.01;
                    let mag = transfer_function(config, k, omega).norm();
                    assert!((mag - 1.0).abs() < 1e-9);
                }
            }
        }
    }

    #[test]
    fn verify_transient_attenuation() {
        let config = StereoBands::Bands20;
        let mut dec = Decorrelator::new();

        // A stationary signal is not attenuated.
        let s: Frame = vec![[Complex::new(0.5, -0.5); NUM_QMF_SLOTS]; MAX_NR_BANDS];
        for _ in 0..4 {
            let (_, gain) = run_frame(&mut dec, config, &s);
            assert!(gain[..20].iter().flatten().all(|&g| (g - 1.0).abs() < 1e-6));
        }

        // A sudden drop of the power after a loud attack is a transient: the peak decay energy
        // exceeds the smoothed energy and the attenuator drops below 1.
        let s: Frame = vec![
            std::array::from_fn(|n| {
                if n == 0 { Complex::new(100.0, 0.0) } else { Complex::new(0.01, 0.0) }
            });
            MAX_NR_BANDS
        ];
        let (_, gain) = run_frame(&mut dec, config, &s);
        assert!(gain[..20].iter().all(|g| g[8] < 1.0 && g[8] > 0.0));
        assert!(gain[20..].iter().flatten().all(|&g| g == 0.0));
    }

    #[test]
    fn verify_reset() {
        let config = StereoBands::Bands34;
        let mut dec = Decorrelator::new();
        let s: Frame = vec![[Complex::new(1.0, 1.0); NUM_QMF_SLOTS]; MAX_NR_BANDS];
        run_frame(&mut dec, config, &s);
        assert!(!dec.is_zero_from(0));
        dec.reset_bands(60);
        assert!(dec.is_zero_from(60));
        assert!(!dec.is_zero_from(59));
        dec.reset_bands(usize::MAX);
        dec.reset();
        assert!(dec.is_zero_from(0));

        // After a reset, the output starts from silence: the first two samples of an all-pass band
        // are zero (pre-delay), the first 14 samples of a long delay band are zero.
        let (d, _) = run_frame(&mut dec, config, &s);
        assert_eq!(d[3][0], ZERO);
        assert_eq!(d[3][1], ZERO);
        assert_ne!(d[3][2], ZERO);
        assert!(d[55][..14].iter().all(|x| *x == ZERO));
        assert_eq!(d[55][14], s[55][0]);
        assert_eq!(d[70][0], ZERO);
        assert_eq!(d[70][1], s[70][0]);
    }

    #[test]
    fn verify_against_band_wise_reference() {
        // The slot-wise de-correlator produces the same output as the band-wise reference, for a
        // signal with level changes that trigger the transient attenuation, configuration changes,
        // and partial resets.
        let mut rng = Lcg(43);
        let mut dec = Decorrelator::new();
        let mut reference = ReferenceDecorrelator::new();
        let configs = [StereoBands::Bands20, StereoBands::Bands34];

        for f in 0..24 {
            let config = configs[(f / 6) % 2];
            if f % 6 == 0 {
                dec.reset();
                reference.reset();
            }
            if f % 4 == 3 {
                let k_max = (rng.next_f32().abs() * 100.0) as usize;
                dec.reset_bands(k_max);
                reference.reset_bands(k_max);
            }

            let level = if f % 3 == 1 { 100.0 } else { 0.1 };
            let s: Frame = (0..MAX_NR_BANDS)
                .map(|_| {
                    std::array::from_fn(|n| {
                        let level = if n % 11 == 5 { level * 30.0 } else { level };
                        Complex::new(rng.next_f32() * level, rng.next_f32() * level)
                    })
                })
                .collect();

            let (d, gain) = run_frame(&mut dec, config, &s);

            let mut gain_ref = [[0.0; NUM_QMF_SLOTS]; MAX_NR_PAR_BANDS];
            reference.transient_gains(config, &s[..config.nr_bands()], &mut gain_ref);
            assert_eq!(gain[..config.nr_par_bands()], gain_ref[..config.nr_par_bands()]);

            for (k, &b) in config.param_index_table().iter().enumerate() {
                let mut d_ref = [ZERO; NUM_QMF_SLOTS];
                reference.decorrelate(config, k, &s[k], &gain_ref[usize::from(b)], &mut d_ref);
                assert_eq!(d[k], d_ref, "frame {f}, band {k}");
            }
        }
    }
}
