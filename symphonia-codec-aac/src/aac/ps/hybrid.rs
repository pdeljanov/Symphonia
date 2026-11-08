// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Hybrid analysis (low frequency filtering) and hybrid synthesis filterbanks
//! (ISO/IEC 14496-3:2009, 8.6.4.3 and 8.6.4.7, with the timing of Annex 8.A.3).
//!
//! When combined with SBR, the 13-tap linear-phase filters that split the lowest QMF subbands use 6
//! QMF samples of look-ahead (Annex 8.A.3), so the hybrid filterbank introduces no delay and the
//! remaining QMF subbands are not delayed.

use std::f64::consts::PI;

use lazy_static::lazy_static;
use symphonia_core::dsp::complex::Complex;

use crate::aac::sbr::types::QmfFrame;

use super::tables::{
    HYBRID_FILTER_DELAY, HYBRID_FILTER_LEN, MAX_NUM_SUB_QMF_BANDS, MAX_SPLIT_QMF_BANDS,
    NUM_QMF_SLOTS, PROTO_20_Q2, PROTO_20_Q8, PROTO_34_Q4, PROTO_34_Q8, PROTO_34_Q12, StereoBands,
};

/// The complex filters of the hybrid analysis filterbank, one per sub subband.
struct HybridFilters {
    /// The filters for the 10, 20 stereo bands configuration (Figure 8.20).
    bands20: [[Complex<f32>; HYBRID_FILTER_LEN]; 10],
    /// The filters for the 34 stereo bands configuration (Figure 8.22).
    bands34: [[Complex<f32>; HYBRID_FILTER_LEN]; 32],
}

/// Type A filter G^p_q[n] = g^p[n] exp(j 2π/Q^p (q + 1/2) (n - 6)) (8.6.4.3).
fn filter_type_a(g: &[f64; HYBRID_FILTER_LEN], q_p: usize, q: usize) -> [Complex<f64>; 13] {
    let mut filter = [Complex::new(0.0, 0.0); HYBRID_FILTER_LEN];
    for (n, (f, &g_n)) in filter.iter_mut().zip(g).enumerate() {
        let phase = 2.0 * PI / q_p as f64 * (q as f64 + 0.5) * (n as f64 - 6.0);
        *f = Complex::from_polar(g_n, phase);
    }
    filter
}

/// Type B filter G^p_q[n] = g^p[n] cos(2π/Q^p q (n - 6)) (8.6.4.3).
fn filter_type_b(g: &[f64; HYBRID_FILTER_LEN], q_p: usize, q: usize) -> [Complex<f64>; 13] {
    let mut filter = [Complex::new(0.0, 0.0); HYBRID_FILTER_LEN];
    for (n, (f, &g_n)) in filter.iter_mut().zip(g).enumerate() {
        let phase = 2.0 * PI / q_p as f64 * q as f64 * (n as f64 - 6.0);
        *f = Complex::new(g_n * phase.cos(), 0.0);
    }
    filter
}

fn sum_filters(a: [Complex<f64>; 13], b: [Complex<f64>; 13]) -> [Complex<f64>; 13] {
    let mut sum = a;
    for (s, b) in sum.iter_mut().zip(b) {
        *s += b;
    }
    sum
}

fn to_f32(filter: [Complex<f64>; 13]) -> [Complex<f32>; 13] {
    filter.map(|c| Complex::new(c.re as f32, c.im as f32))
}

impl HybridFilters {
    fn new() -> Self {
        // 10, 20 stereo bands (Table 8.36, Figure 8.20). QMF subband 0 is split into 8 sub subbands
        // of which some are combined, QMF subbands 1 and 2 into 2 sub subbands each.
        let a = |q| filter_type_a(&PROTO_20_Q8, 8, q);
        let b = |q| filter_type_b(&PROTO_20_Q2, 2, q);
        let bands20 = [
            a(6),
            a(7),
            a(0),
            a(1),
            sum_filters(a(2), a(5)),
            sum_filters(a(3), a(4)),
            b(1),
            b(0),
            b(0),
            b(1),
        ]
        .map(to_f32);

        // 34 stereo bands (Table 8.36, Figure 8.22).
        let mut bands34 = [[Complex::new(0.0, 0.0); HYBRID_FILTER_LEN]; 32];
        for (q, f) in bands34[0..12].iter_mut().enumerate() {
            *f = to_f32(filter_type_a(&PROTO_34_Q12, 12, q));
        }
        for (q, f) in bands34[12..20].iter_mut().enumerate() {
            *f = to_f32(filter_type_a(&PROTO_34_Q8, 8, q));
        }
        for (i, f) in bands34[20..32].iter_mut().enumerate() {
            *f = to_f32(filter_type_a(&PROTO_34_Q4, 4, i % 4));
        }

        HybridFilters { bands20, bands34 }
    }

    fn get(&self, config: StereoBands) -> &[[Complex<f32>; HYBRID_FILTER_LEN]] {
        match config {
            StereoBands::Bands20 => &self.bands20,
            StereoBands::Bands34 => &self.bands34,
        }
    }
}

lazy_static! {
    static ref HYBRID_FILTERS: HybridFilters = HybridFilters::new();
}

/// The filters of the sub subbands of the split QMF subbands of a configuration.
#[cfg(test)]
pub(super) fn filters(config: StereoBands) -> &'static [[Complex<f32>; HYBRID_FILTER_LEN]] {
    HYBRID_FILTERS.get(config)
}

/// The number of QMF samples of a split QMF subband available to the hybrid filters: the history,
/// the frame, and the look-ahead.
const EXT_LEN: usize = HYBRID_FILTER_DELAY + NUM_QMF_SLOTS + HYBRID_FILTER_DELAY;

/// The hybrid analysis filterbank state.
pub(super) struct HybridAnalysis {
    /// The last HYBRID_FILTER_DELAY QMF samples of the previous frame of each QMF subband that may
    /// be split, oldest first.
    history: [[Complex<f32>; HYBRID_FILTER_DELAY]; MAX_SPLIT_QMF_BANDS],
}

impl HybridAnalysis {
    pub fn new() -> Self {
        HybridAnalysis {
            history: [[Complex::new(0.0, 0.0); HYBRID_FILTER_DELAY]; MAX_SPLIT_QMF_BANDS],
        }
    }

    pub fn reset(&mut self) {
        *self = HybridAnalysis::new();
    }

    /// Compute the hybrid sub subband samples s_k(n) of the sub subbands of the split QMF subbands
    /// of one frame (8.6.4.3, Annex 8.A.3) and write them to `sub[n][k]` for `k <
    /// num_sub_qmf_bands`. The history is not updated, see `update_history`.
    pub fn analyze(
        &self,
        config: StereoBands,
        input: &QmfFrame,
        sub: &mut [[Complex<f32>; MAX_NUM_SUB_QMF_BANDS]; NUM_QMF_SLOTS],
    ) {
        // The QMF samples X_input(p, l) for -6 <= l < 38 of each split QMF subband p.
        let mut ext = [[Complex::new(0.0, 0.0); EXT_LEN]; MAX_SPLIT_QMF_BANDS];
        for (p, x) in ext.iter_mut().enumerate().take(config.num_split_qmf_bands()) {
            x[..HYBRID_FILTER_DELAY].copy_from_slice(&self.history[p]);
            for (x, row) in x[HYBRID_FILTER_DELAY..].iter_mut().zip(input.iter()) {
                *x = row[p];
            }
        }

        // Split QMF subbands. With the filter delay compensated by the look-ahead:
        // s_k(n) = sum_i G[i] X(p, n + 6 - i), where X(p, n + 6 - i) is ext[n + 12 - i]. The sum is
        // accumulated for all n at once, in the order of increasing i.
        let filters = HYBRID_FILTERS.get(config);
        let sub_qmf = config.sub_qmf_band_table();
        for (k, (filter, &p)) in filters.iter().zip(sub_qmf).enumerate() {
            let x = &ext[usize::from(p)];
            let mut s_k = [Complex::new(0.0, 0.0); NUM_QMF_SLOTS];
            for (i, &g) in filter.iter().enumerate() {
                let start = HYBRID_FILTER_LEN - 1 - i;
                for (s, &x) in s_k.iter_mut().zip(&x[start..start + NUM_QMF_SLOTS]) {
                    *s += g * x;
                }
            }
            for (row, s) in sub.iter_mut().zip(s_k) {
                row[k] = s;
            }
        }
    }

    /// Store the last QMF samples of the frame for the hybrid analysis of the next frame.
    pub fn update_history(&mut self, input: &QmfFrame) {
        let rows = &input[NUM_QMF_SLOTS - HYBRID_FILTER_DELAY..NUM_QMF_SLOTS];
        for (p, history) in self.history.iter_mut().enumerate() {
            for (h, row) in history.iter_mut().zip(rows) {
                *h = row[p];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aac::sbr::types::{PS_LOOKAHEAD, PS_LOOKAHEAD_BANDS, QMF_BANDS};

    type SubSlots = [[Complex<f32>; MAX_NUM_SUB_QMF_BANDS]; NUM_QMF_SLOTS];

    /// A simple linear congruential generator.
    struct Lcg(u64);

    impl Lcg {
        fn next_f32(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
        }

        fn next_complex(&mut self) -> Complex<f32> {
            Complex::new(self.next_f32(), self.next_f32())
        }
    }

    /// Generate a continuous QMF signal of `num_frames` frames and split it into frames with the
    /// look-ahead rows of Annex 8.A.3.
    fn make_frames(rng: &mut Lcg, num_frames: usize) -> (Vec<[Complex<f32>; 64]>, Vec<QmfFrame>) {
        let total = num_frames * NUM_QMF_SLOTS + PS_LOOKAHEAD;
        let signal: Vec<[Complex<f32>; 64]> =
            (0..total).map(|_| std::array::from_fn(|_| rng.next_complex())).collect();
        let mut frames = Vec::new();
        for f in 0..num_frames {
            let mut frame = [[Complex::new(0.0, 0.0); QMF_BANDS]; NUM_QMF_SLOTS + PS_LOOKAHEAD];
            for (l, row) in frame.iter_mut().enumerate() {
                let src = &signal[f * NUM_QMF_SLOTS + l];
                for (k, x) in row.iter_mut().enumerate() {
                    if l < NUM_QMF_SLOTS || k < PS_LOOKAHEAD_BANDS {
                        *x = src[k];
                    }
                }
            }
            frames.push(frame);
        }
        (signal, frames)
    }

    #[test]
    fn verify_hybrid_analysis_synthesis_reconstruction() {
        let mut rng = Lcg(1);
        for config in [StereoBands::Bands20, StereoBands::Bands34] {
            let (_, frames) = make_frames(&mut rng, 4);
            let mut analysis = HybridAnalysis::new();
            let mut sub: Box<SubSlots> = Box::new([[Complex::new(0.0, 0.0); 32]; NUM_QMF_SLOTS]);
            for frame in frames.iter() {
                analysis.analyze(config, frame, &mut sub);
                analysis.update_history(frame);
                // The sum of the sub subband filters of a QMF subband is a delay of 6 samples,
                // which is compensated by the look-ahead, so the reconstruction has no delay.
                for n in 0..NUM_QMF_SLOTS {
                    let mut sum = [Complex::new(0.0, 0.0); MAX_SPLIT_QMF_BANDS];
                    for k in 0..config.num_sub_qmf_bands() {
                        sum[config.qmf_band(k)] += sub[n][k];
                    }
                    for p in 0..config.num_split_qmf_bands() {
                        let err = (sum[p] - frame[n][p]).norm();
                        assert!(err < 1e-5, "config {config:?}, n {n}, p {p}, err {err}");
                    }
                }
            }
        }
    }

    #[test]
    fn verify_hybrid_analysis_against_direct_formula() {
        // Evaluate the hybrid analysis directly from the definitions of 8.6.4.3 (Table 8.36,
        // Figures 8.20 and 8.22) in double precision, with the filter delay of 6 compensated.
        let mut rng = Lcg(7);
        let (signal, frames) = make_frames(&mut rng, 3);

        let g_a = |g: &[f64; 13], q_p: usize, q: usize, n: usize| {
            let phase = 2.0 * PI / q_p as f64 * (q as f64 + 0.5) * (n as f64 - 6.0);
            Complex::new(g[n] * phase.cos(), g[n] * phase.sin())
        };
        let g_b = |g: &[f64; 13], q_p: usize, q: usize, n: usize| {
            let phase = 2.0 * PI / q_p as f64 * q as f64 * (n as f64 - 6.0);
            Complex::new(g[n] * phase.cos(), 0.0)
        };
        // The filter(s) summed for each sub subband: (type A?, prototype, Q, q list, QMF band).
        type Spec = (bool, &'static [f64; 13], usize, &'static [usize], usize);
        let spec20: [Spec; 10] = [
            (true, &PROTO_20_Q8, 8, &[6], 0),
            (true, &PROTO_20_Q8, 8, &[7], 0),
            (true, &PROTO_20_Q8, 8, &[0], 0),
            (true, &PROTO_20_Q8, 8, &[1], 0),
            (true, &PROTO_20_Q8, 8, &[2, 5], 0),
            (true, &PROTO_20_Q8, 8, &[3, 4], 0),
            (false, &PROTO_20_Q2, 2, &[1], 1),
            (false, &PROTO_20_Q2, 2, &[0], 1),
            (false, &PROTO_20_Q2, 2, &[0], 2),
            (false, &PROTO_20_Q2, 2, &[1], 2),
        ];
        static Q: [usize; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        let mut spec34: Vec<Spec> = Vec::new();
        for q in 0..12 {
            spec34.push((true, &PROTO_34_Q12, 12, &Q[q..q + 1], 0));
        }
        for q in 0..8 {
            spec34.push((true, &PROTO_34_Q8, 8, &Q[q..q + 1], 1));
        }
        for p in 2..5 {
            for q in 0..4 {
                spec34.push((true, &PROTO_34_Q4, 4, &Q[q..q + 1], p));
            }
        }

        for config in [StereoBands::Bands20, StereoBands::Bands34] {
            let specs: &[Spec] = match config {
                StereoBands::Bands20 => &spec20,
                StereoBands::Bands34 => &spec34,
            };
            let mut analysis = HybridAnalysis::new();
            let mut sub: Box<SubSlots> = Box::new([[Complex::new(0.0, 0.0); 32]; NUM_QMF_SLOTS]);
            for (f, frame) in frames.iter().enumerate() {
                analysis.analyze(config, frame, &mut sub);
                analysis.update_history(frame);
                for (k, &(is_a, g, q_p, qs, p)) in specs.iter().enumerate() {
                    for n in 0..NUM_QMF_SLOTS {
                        let t = (f * NUM_QMF_SLOTS + n) as i64;
                        let mut acc = Complex::new(0.0f64, 0.0);
                        for i in 0..13 {
                            let l = t + 6 - i as i64;
                            // Samples before the start of the signal are zero.
                            let x = if l < 0 {
                                Complex::new(0.0, 0.0)
                            }
                            else {
                                let x = signal[l as usize][p];
                                Complex::new(f64::from(x.re), f64::from(x.im))
                            };
                            for &q in qs {
                                let c = if is_a { g_a(g, q_p, q, i) } else { g_b(g, q_p, q, i) };
                                acc += c * x;
                            }
                        }
                        let got = sub[n][k];
                        let err = ((f64::from(got.re) - acc.re).powi(2)
                            + (f64::from(got.im) - acc.im).powi(2))
                        .sqrt();
                        assert!(err < 1e-5, "config {config:?}, k {k}, n {n}, err {err}");
                    }
                }
            }
        }
    }
}
