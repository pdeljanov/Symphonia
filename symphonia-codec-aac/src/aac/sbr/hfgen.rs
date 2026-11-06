// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! HF generation (4.6.18.6): inverse filtering (4.6.18.6.2) and the HF generator (4.6.18.6.3).

use std::ops::Range;

use symphonia_core::dsp::complex::Complex;

use crate::aac::sbr::types::{
    MAX_NOISE_BANDS, MAX_PATCHES, Patch, QMF_ANALYSIS_BANDS, QMF_BANDS, QMF_SLOTS, T_HF_ADJ,
    T_HF_GEN,
};

/// Number of QMF subsamples of the matrices X_Low and X_High: numTimeSlots * RATE + t_HFGen
/// (4.6.18.5).
pub const X_SLOTS: usize = QMF_SLOTS + T_HF_GEN;

/// The matrix X_Low (4.6.18.5), indexed `[l][k]`, 0 <= l < numTimeSlots * RATE + t_HFGen and
/// 0 <= k < 32.
pub type XLow = [[Complex<f32>; QMF_ANALYSIS_BANDS]; X_SLOTS];

/// The matrix X_High (4.6.18.6.3), indexed `[l][k]` like X_Low, 0 <= k < 64.
pub type XHigh = [[Complex<f32>; QMF_BANDS]; X_SLOTS];

/// The relaxation parameter epsilon_Inv of the covariance method (4.6.18.6.2).
const EPS_INV: f64 = 1e-6;

/// The function newBw (Table 4.175) indexed by `[bs_invf_mode'(i)][bs_invf_mode(i)]`.
static NEW_BW: [[f32; 4]; 4] =
    [[0.0, 0.6, 0.9, 0.98], [0.6, 0.75, 0.9, 0.98], [0.0, 0.75, 0.9, 0.98], [0.0, 0.75, 0.9, 0.98]];

/// Persistent state and work buffers of the HF generator of one channel.
pub struct HfGenerator {
    /// bwArray(i) of the current SBR frame, and bwArray'(i) of the previous SBR frame before
    /// [`HfGenerator::update_chirp_factors`] is called.
    bw_array: [f32; MAX_NOISE_BANDS],
    /// bs_invf_mode'(i) of the previous SBR frame.
    prev_invf_mode: [u8; MAX_NOISE_BANDS],
    /// alpha_0(k) for each low band subband k.
    alpha0: [Complex<f32>; QMF_ANALYSIS_BANDS],
    /// alpha_1(k) for each low band subband k.
    alpha1: [Complex<f32>; QMF_ANALYSIS_BANDS],
}

impl Default for HfGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl HfGenerator {
    pub fn new() -> Self {
        let zero = Complex::new(0.0, 0.0);
        HfGenerator {
            bw_array: [0.0; MAX_NOISE_BANDS],
            prev_invf_mode: [0; MAX_NOISE_BANDS],
            alpha0: [zero; QMF_ANALYSIS_BANDS],
            alpha1: [zero; QMF_ANALYSIS_BANDS],
        }
    }

    /// Reset to the state before the first SBR frame: bwArray' and bs_invf_mode' are zero.
    pub fn reset(&mut self) {
        self.bw_array = [0.0; MAX_NOISE_BANDS];
        self.prev_invf_mode = [0; MAX_NOISE_BANDS];
    }

    /// Calculate the chirp factors bwArray(i) of the current SBR frame from bs_invf_mode(i),
    /// 0 <= i < N_Q, where N_Q is the length of `invf_mode` (4.6.18.6.2).
    ///
    /// Must be called once per SBR frame. Chirp factors and modes of noise floor bands that are not
    /// present in the current frame are considered zero for the next frame. Invalid modes (> 3) are
    /// treated as strong inverse filtering.
    pub fn update_chirp_factors(&mut self, invf_mode: &[u8]) {
        for i in 0..MAX_NOISE_BANDS {
            let Some(&mode) = invf_mode.get(i)
            else {
                self.bw_array[i] = 0.0;
                self.prev_invf_mode[i] = 0;
                continue;
            };

            let mode = mode.min(3);
            let new_bw = NEW_BW[usize::from(self.prev_invf_mode[i])][usize::from(mode)];
            let prev_bw = self.bw_array[i];

            let temp_bw = if new_bw < prev_bw {
                0.75 * new_bw + 0.25 * prev_bw
            }
            else {
                0.90625 * new_bw + 0.09375 * prev_bw
            };

            self.bw_array[i] = if temp_bw < 0.015625 { 0.0 } else { temp_bw };
            self.prev_invf_mode[i] = mode;
        }
    }

    /// Generate the high band X_High(k, l + t_HFAdj) for `slots.start <= l < slots.end` from X_Low
    /// by patching (4.6.18.6.3).
    ///
    /// `slots` is RATE * t_E(0)..RATE * t_E(L_E). The chirp factors must have been calculated for
    /// the current frame with [`HfGenerator::update_chirp_factors`]. Subbands of X_High that are
    /// not covered by a patch are left unchanged. The subbands covered by a patch are also
    /// modified in the two QMF subsamples before `slots.start + t_HFAdj`, which serve as work
    /// space. Patches, or parts of patches, that would read or write outside the matrices are
    /// ignored.
    pub fn generate(
        &mut self,
        x_low: &XLow,
        kx: usize,
        patches: &[Patch],
        f_table_noise: &[usize],
        slots: Range<usize>,
        x_high: &mut XHigh,
    ) {
        // The QMF subsamples X_Low(p, l + t_HFAdj - 2) to X_Low(p, l + t_HFAdj) must be available.
        let slots = slots.start..slots.end.min(X_SLOTS - T_HF_ADJ);

        // The parts of the patches within the matrices: the first target subband k, the first
        // source subband p, and the number of subbands. The first target subband of patch i is
        // k_x + sum_{q<i} patchNumSubbands(q).
        let mut segments = [(0, 0, 0); MAX_PATCHES];
        let mut num_segments = 0;
        let mut sources: Option<Range<usize>> = None;

        let mut k_start = kx;
        for patch in patches {
            let p_start = patch.start_subband;
            let len = patch
                .num_subbands
                .min(QMF_BANDS.saturating_sub(k_start))
                .min(QMF_ANALYSIS_BANDS.saturating_sub(p_start));

            if len > 0 {
                if let Some(segment) = segments.get_mut(num_segments) {
                    *segment = (k_start, p_start, len);
                    num_segments += 1;
                }
                sources = Some(match sources {
                    Some(r) => r.start.min(p_start)..r.end.max(p_start + len),
                    None => p_start..p_start + len,
                });
            }

            k_start = k_start.saturating_add(patch.num_subbands);
        }

        let Some(sources) = sources
        else {
            return;
        };

        // The prediction coefficients of the source subbands.
        prediction_coefficients(x_low, sources, &mut self.alpha0, &mut self.alpha1);

        // The filter coefficients alpha_0(p) * bwArray(g(k)) and alpha_1(p) * bwArray(g(k))^2 of
        // each target subband k, where g(k) is the noise floor band containing subband k.
        let zero = Complex::new(0.0, 0.0);
        let mut a0 = [zero; QMF_BANDS];
        let mut a1 = [zero; QMF_BANDS];

        for &(k_start, p_start, len) in &segments[..num_segments.min(MAX_PATCHES)] {
            for x in 0..len {
                let (k, p) = (k_start + x, p_start + x);
                let bw = self.bw_array[noise_band(f_table_noise, k)];
                a0[k] = self.alpha0[p] * bw;
                a1[k] = self.alpha1[p] * (bw * bw);
            }
        }

        // X_High(k, l + t_HFAdj) = X_Low(p, l + t_HFAdj) + a0(k) * X_Low(p, l + t_HFAdj - 1) +
        // a1(k) * X_Low(p, l + t_HFAdj - 2).
        //
        // First, the source subbands of all patches are copied to the target subbands of X_High,
        // for the QMF subsamples l + t_HFAdj - 2 to l + t_HFAdj. Then, the filter is applied to
        // the target subbands in place, in the order of decreasing QMF subsample, such that every
        // QMF subsample only uses copied samples of earlier QMF subsamples.
        if slots.is_empty() {
            return;
        }

        let segments = &segments[..num_segments.min(MAX_PATCHES)];

        for l in slots.start..slots.end + T_HF_ADJ {
            let (src, dst) = (&x_low[l], &mut x_high[l]);
            for &(k_start, p_start, len) in segments {
                dst[k_start..k_start + len].copy_from_slice(&src[p_start..p_start + len]);
            }
        }

        // The target subbands as ranges of consecutive subbands. Usually, the patches cover a
        // single range.
        let mut targets = [(0, 0); MAX_PATCHES];
        let mut num_targets = 0;
        for &(k_start, _, len) in segments {
            match targets[..num_targets].last_mut() {
                Some(last) if last.1 == k_start => last.1 += len,
                _ => {
                    targets[num_targets] = (k_start, k_start + len);
                    num_targets += 1;
                }
            }
        }

        for l in slots.rev() {
            let (rows, cur) = x_high.split_at_mut(l + T_HF_ADJ);
            let (x1, x2) = (&rows[l + T_HF_ADJ - 1], &rows[l + T_HF_ADJ - 2]);

            for k in targets[..num_targets].iter().map(|&(start, end)| start..end) {
                predict(
                    &mut cur[0][k.clone()],
                    &x1[k.clone()],
                    &x2[k.clone()],
                    &a0[k.clone()],
                    &a1[k],
                );
            }
        }
    }
}

/// Apply the prediction filter x0 = x0 + a0 * x1 + a1 * x2 element-wise.
///
/// The function is not inlined: its slice arguments do not alias, which allows the loop to be
/// vectorized.
#[inline(never)]
fn predict(
    x0: &mut [Complex<f32>],
    x1: &[Complex<f32>],
    x2: &[Complex<f32>],
    a0: &[Complex<f32>],
    a1: &[Complex<f32>],
) {
    let coeffs = a0.iter().zip(a1);
    for ((x0, (&a0, &a1)), (&x1, &x2)) in x0.iter_mut().zip(coeffs).zip(x1.iter().zip(x2)) {
        *x0 = *x0 + a0 * x1 + a1 * x2;
    }
}

/// Get the noise floor band g(k) such that f_TableNoise(g(k)) <= k < f_TableNoise(g(k) + 1).
///
/// Subbands below or above the noise floor band table are assigned to the first or last band.
fn noise_band(f_table_noise: &[usize], k: usize) -> usize {
    let num_bands = f_table_noise.len().saturating_sub(1).clamp(1, MAX_NOISE_BANDS);
    let inner = f_table_noise.get(1..num_bands).unwrap_or(&[]);
    inner.iter().filter(|&&border| border <= k).count()
}

/// The covariance sums phi_k(i, j) of the covariance method (4.6.18.6.2) of all low band subbands
/// k, with the real and imaginary parts in separate arrays indexed by k.
struct Covariance {
    phi01: [[f64; QMF_ANALYSIS_BANDS]; 2],
    phi02: [[f64; QMF_ANALYSIS_BANDS]; 2],
    phi12: [[f64; QMF_ANALYSIS_BANDS]; 2],
    phi11: [f64; QMF_ANALYSIS_BANDS],
    phi22: [f64; QMF_ANALYSIS_BANDS],
}

impl Covariance {
    /// Add the terms of the QMF subsample l of the subbands `range`, where `x0`, `x1`, and `x2`
    /// are the QMF subsamples l, l + 1, and l + 2 of X_Low. The terms are added to the sums of
    /// phi_k(1, 2), phi_k(2, 2), and phi_k(0, 2) if `FIRST` is true, and to the sums of
    /// phi_k(0, 1) and phi_k(1, 1) if `LAST` is true.
    fn add<const FIRST: bool, const LAST: bool>(
        &mut self,
        range: Range<usize>,
        x0: &[Complex<f32>; QMF_ANALYSIS_BANDS],
        x1: &[Complex<f32>; QMF_ANALYSIS_BANDS],
        x2: &[Complex<f32>; QMF_ANALYSIS_BANDS],
    ) {
        let [phi01_re, phi01_im] = &mut self.phi01;
        let [phi02_re, phi02_im] = &mut self.phi02;
        let [phi12_re, phi12_im] = &mut self.phi12;
        let r = range;

        covariance_terms::<FIRST, LAST>(
            [&mut phi01_re[r.clone()], &mut phi01_im[r.clone()]],
            [&mut phi02_re[r.clone()], &mut phi02_im[r.clone()]],
            [&mut phi12_re[r.clone()], &mut phi12_im[r.clone()]],
            &mut self.phi11[r.clone()],
            &mut self.phi22[r.clone()],
            [&x0[r.clone()], &x1[r.clone()], &x2[r]],
        );
    }
}

/// Add the terms of one QMF subsample l of the covariance sums phi(0, 1), phi(0, 2), phi(1, 2),
/// phi(1, 1), and phi(2, 2) of the subbands of the slices (see `Covariance::add`), where `x`
/// holds the QMF subsamples l, l + 1, and l + 2 of X_Low.
///
/// The products are evaluated like the complex multiplications x1 * conj(x0) and x2 * conj(x0) in
/// double precision. The function is not inlined: its slice arguments do not alias, which allows
/// the loop to be vectorized.
#[inline(never)]
fn covariance_terms<const FIRST: bool, const LAST: bool>(
    [phi01_re, phi01_im]: [&mut [f64]; 2],
    [phi02_re, phi02_im]: [&mut [f64]; 2],
    [phi12_re, phi12_im]: [&mut [f64]; 2],
    phi11: &mut [f64],
    phi22: &mut [f64],
    [x0, x1, x2]: [&[Complex<f32>]; 3],
) {
    let len = x0.len();
    let (phi01_re, phi01_im) = (&mut phi01_re[..len], &mut phi01_im[..len]);
    let (phi02_re, phi02_im) = (&mut phi02_re[..len], &mut phi02_im[..len]);
    let (phi12_re, phi12_im) = (&mut phi12_re[..len], &mut phi12_im[..len]);
    let (phi11, phi22) = (&mut phi11[..len], &mut phi22[..len]);
    let (x1, x2) = (&x1[..len], &x2[..len]);

    for k in 0..len {
        let (a_re, a_im) = (f64::from(x0[k].re), -f64::from(x0[k].im));
        let (b_re, b_im) = (f64::from(x1[k].re), f64::from(x1[k].im));

        // x1 * conj(x0) and |x0|^2.
        let lag1_re = b_re * a_re - b_im * a_im;
        let lag1_im = b_re * a_im + b_im * a_re;
        let energy = a_re * a_re + a_im * a_im;

        if FIRST {
            // x2 * conj(x0).
            let (c_re, c_im) = (f64::from(x2[k].re), f64::from(x2[k].im));
            phi02_re[k] += c_re * a_re - c_im * a_im;
            phi02_im[k] += c_re * a_im + c_im * a_re;
            phi12_re[k] += lag1_re;
            phi12_im[k] += lag1_im;
            phi22[k] += energy;
        }

        if LAST {
            phi01_re[k] += lag1_re;
            phi01_im[k] += lag1_im;
            phi11[k] += energy;
        }
    }
}

/// Calculate the prediction coefficients alpha_0(k) and alpha_1(k) of the low band subbands k in
/// `subbands` with the covariance method (4.6.18.6.2), and write them to `alpha0[k]` and
/// `alpha1[k]`. `subbands` must be within 0..32.
fn prediction_coefficients(
    x_low: &XLow,
    subbands: Range<usize>,
    alpha0: &mut [Complex<f32>; QMF_ANALYSIS_BANDS],
    alpha1: &mut [Complex<f32>; QMF_ANALYSIS_BANDS],
) {
    const N: usize = QMF_ANALYSIS_BANDS;

    // phi_k(i, j) = sum_{n=0}^{numTimeSlots * RATE + 6 - 1} X_Low(k, n - i + t_HFAdj) *
    //               conj(X_Low(k, n - j + t_HFAdj))
    //
    // With x(l) = X_Low(k, l) and t_HFAdj = 2, the products x(l + 1) * conj(x(l)) contribute to
    // phi_k(1, 2) for 0 <= l < 38 and to phi_k(0, 1) for 1 <= l < 39, and the energies |x(l)|^2
    // to phi_k(2, 2) for 0 <= l < 38 and to phi_k(1, 1) for 1 <= l < 39. Each product is
    // computed once. The sums of all subbands are accumulated together, in the order of
    // increasing l.
    const NUM_TERMS: usize = QMF_SLOTS + 6;

    let range = subbands.start.min(N)..subbands.end.min(N);

    let mut cov = Covariance {
        phi01: [[0.0; N]; 2],
        phi02: [[0.0; N]; 2],
        phi12: [[0.0; N]; 2],
        phi11: [0.0; N],
        phi22: [0.0; N],
    };

    cov.add::<true, false>(range.clone(), &x_low[0], &x_low[1], &x_low[2]);
    for l in 1..NUM_TERMS {
        cov.add::<true, true>(range.clone(), &x_low[l], &x_low[l + 1], &x_low[l + 2]);
    }
    let l = NUM_TERMS;
    cov.add::<false, true>(range.clone(), &x_low[l], &x_low[l + 1], &x_low[l + 1]);

    let zero = Complex::new(0.0f64, 0.0);

    for k in range {
        let phi01 = Complex::new(cov.phi01[0][k], cov.phi01[1][k]);
        let phi02 = Complex::new(cov.phi02[0][k], cov.phi02[1][k]);
        let phi12 = Complex::new(cov.phi12[0][k], cov.phi12[1][k]);
        let (phi11, phi22) = (cov.phi11[k], cov.phi22[k]);

        let d = phi22 * phi11 - phi12.norm_sqr() / (1.0 + EPS_INV);

        let a1 = if d != 0.0 { (phi01 * phi12 - phi02 * phi11) / d } else { zero };

        let a0 = if phi11 != 0.0 { -(phi01 + a1 * phi12.conj()) / phi11 } else { zero };

        // If either magnitude is greater than or equal to 4, both coefficients are set to zero. A
        // non-finite coefficient is treated the same way.
        let valid = |a: Complex<f64>| a.norm() < 4.0;

        (alpha0[k], alpha1[k]) = if valid(a0) && valid(a1) {
            (Complex::new(a0.re as f32, a0.im as f32), Complex::new(a1.re as f32, a1.im as f32))
        }
        else {
            (Complex::new(0.0, 0.0), Complex::new(0.0, 0.0))
        };
    }
}

#[cfg(test)]
mod tests {
    use symphonia_core::dsp::complex::Complex;

    use super::{HfGenerator, X_SLOTS, XHigh, XLow, noise_band, prediction_coefficients};
    use crate::aac::sbr::types::{Patch, QMF_ANALYSIS_BANDS, QMF_BANDS};

    fn zero_low() -> Box<XLow> {
        Box::new([[Complex::new(0.0, 0.0); QMF_ANALYSIS_BANDS]; X_SLOTS])
    }

    fn zero_high() -> Box<XHigh> {
        Box::new([[Complex::new(0.0, 0.0); QMF_BANDS]; X_SLOTS])
    }

    #[test]
    fn verify_chirp_factors() {
        let mut hf_gen = HfGenerator::new();

        // First frame: bs_invf_mode' = 0 and bwArray' = 0.
        hf_gen.update_chirp_factors(&[0, 1, 2, 3]);
        let bw = hf_gen.bw_array;
        assert_eq!(bw[0], 0.0);
        assert!((bw[1] - 0.90625 * 0.6).abs() < 1e-6);
        assert!((bw[2] - 0.90625 * 0.9).abs() < 1e-6);
        assert!((bw[3] - 0.90625 * 0.98).abs() < 1e-6);
        assert_eq!(bw[4], 0.0);

        // Second frame: modes go down, newBw < bwArray' for band 3 (newBw = 0.0).
        hf_gen.update_chirp_factors(&[1, 1, 2, 0]);
        let bw2 = hf_gen.bw_array;
        // Band 0: Off -> Low, newBw = 0.6, bwArray' = 0.
        assert!((bw2[0] - 0.90625 * 0.6).abs() < 1e-6);
        // Band 1: Low -> Low, newBw = 0.75 >= bwArray'.
        assert!((bw2[1] - (0.90625 * 0.75 + 0.09375 * bw[1])).abs() < 1e-6);
        // Band 2: Intermediate -> Intermediate, newBw = 0.9 >= bwArray'.
        assert!((bw2[2] - (0.90625 * 0.9 + 0.09375 * bw[2])).abs() < 1e-6);
        // Band 3: Strong -> Off, newBw = 0.0 < bwArray'.
        assert!((bw2[3] - 0.25 * bw[3]).abs() < 1e-6);

        // Decay below 0.015625 results in zero.
        for _ in 0..10 {
            hf_gen.update_chirp_factors(&[0, 0, 0, 0]);
        }
        assert_eq!(hf_gen.bw_array, [0.0; 5]);

        // Low -> Off maps to newBw = 0.6 (Table 4.175).
        hf_gen.update_chirp_factors(&[1]);
        hf_gen.update_chirp_factors(&[0]);
        let b = hf_gen.bw_array[0];
        assert!(b > 0.5 && b < 0.6);

        hf_gen.reset();
        assert_eq!(hf_gen.bw_array, [0.0; 5]);
    }

    #[test]
    fn verify_noise_band() {
        let table = [10, 14, 20, 30];
        assert_eq!(noise_band(&table, 5), 0);
        assert_eq!(noise_band(&table, 10), 0);
        assert_eq!(noise_band(&table, 13), 0);
        assert_eq!(noise_band(&table, 14), 1);
        assert_eq!(noise_band(&table, 19), 1);
        assert_eq!(noise_band(&table, 20), 2);
        assert_eq!(noise_band(&table, 29), 2);
        assert_eq!(noise_band(&table, 40), 2);
        assert_eq!(noise_band(&[], 40), 0);
        assert_eq!(noise_band(&[3], 40), 0);
        assert_eq!(noise_band(&[0, 1, 2, 3, 4, 5, 6, 7, 8], 40), 4);
    }

    /// Direct evaluation of the covariance method of 4.6.18.6.2.
    fn direct_coefficients(x: &[Complex<f64>]) -> (Complex<f64>, Complex<f64>) {
        let phi = |i: usize, j: usize| {
            let mut acc = Complex::new(0.0, 0.0);
            for n in 0..38 {
                acc += x[n + 2 - i] * x[n + 2 - j].conj();
            }
            acc
        };
        let d = phi(2, 2) * phi(1, 1) - phi(1, 2).norm_sqr() / (1.0 + 1e-6);
        let a1 = if d.norm() != 0.0 {
            (phi(0, 1) * phi(1, 2) - phi(0, 2) * phi(1, 1)) / d
        }
        else {
            Complex::new(0.0, 0.0)
        };
        let a0 = if phi(1, 1).norm() != 0.0 {
            -(phi(0, 1) + a1 * phi(1, 2).conj()) / phi(1, 1)
        }
        else {
            Complex::new(0.0, 0.0)
        };
        if a0.norm() >= 4.0 || a1.norm() >= 4.0 {
            (Complex::new(0.0, 0.0), Complex::new(0.0, 0.0))
        }
        else {
            (a0, a1)
        }
    }

    /// The prediction coefficients of subband `k` alone.
    fn coefficients(x_low: &XLow, k: usize) -> (Complex<f32>, Complex<f32>) {
        let mut alpha0 = [Complex::new(0.0, 0.0); QMF_ANALYSIS_BANDS];
        let mut alpha1 = [Complex::new(0.0, 0.0); QMF_ANALYSIS_BANDS];
        prediction_coefficients(x_low, k..k + 1, &mut alpha0, &mut alpha1);
        (alpha0[k], alpha1[k])
    }

    /// The covariance method (4.6.18.6.2) of one subband, with the sums accumulated in double
    /// precision in the order of increasing n, as in the formulas.
    fn reference_coefficients(x_low: &XLow, k: usize) -> (Complex<f32>, Complex<f32>) {
        let zero = Complex::new(0.0f64, 0.0);
        let get = |l: usize| Complex::new(f64::from(x_low[l][k].re), f64::from(x_low[l][k].im));
        let (mut phi01, mut phi02, mut phi12) = (zero, zero, zero);
        let (mut phi11, mut phi22) = (0.0f64, 0.0f64);
        for n in 0..38 {
            let (x0, x1, x2) = (get(n + 2), get(n + 1), get(n));
            phi01 += x0 * x1.conj();
            phi02 += x0 * x2.conj();
            phi12 += x1 * x2.conj();
            phi11 += x1.norm_sqr();
            phi22 += x2.norm_sqr();
        }
        let d = phi22 * phi11 - phi12.norm_sqr() / (1.0 + 1e-6);
        let a1 = if d != 0.0 { (phi01 * phi12 - phi02 * phi11) / d } else { zero };
        let a0 = if phi11 != 0.0 { -(phi01 + a1 * phi12.conj()) / phi11 } else { zero };
        if a0.norm() < 4.0 && a1.norm() < 4.0 {
            (Complex::new(a0.re as f32, a0.im as f32), Complex::new(a1.re as f32, a1.im as f32))
        }
        else {
            (Complex::new(0.0, 0.0), Complex::new(0.0, 0.0))
        }
    }

    #[test]
    fn verify_prediction_coefficients_are_bit_exact() {
        // The covariance sums of all subbands are computed together, with each product computed
        // once, but in the same order and with the same rounding as the formulas.
        let mut seed = 11u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as f32 / (1 << 24) as f32 - 0.5
        };
        for trial in 0..20 {
            let mut x_low = zero_low();
            for (l, row) in x_low.iter_mut().enumerate() {
                for (k, x) in row.iter_mut().enumerate() {
                    // A tonal component plus noise, with some silent subbands.
                    let phase = 0.37 * (l * (k + 1)) as f32;
                    let tone = Complex::from_polar(1000.0, phase);
                    let noise = Complex::new(rnd(), rnd()) * (10.0 * trial as f32);
                    *x = if (k + trial) % 7 == 0 { Complex::new(0.0, 0.0) } else { tone + noise };
                }
            }
            let mut alpha0 = [Complex::new(0.0, 0.0); QMF_ANALYSIS_BANDS];
            let mut alpha1 = [Complex::new(0.0, 0.0); QMF_ANALYSIS_BANDS];
            let range = trial % 5..QMF_ANALYSIS_BANDS - trial % 3;
            prediction_coefficients(&x_low, range.clone(), &mut alpha0, &mut alpha1);
            for k in range {
                assert_eq!((alpha0[k], alpha1[k]), reference_coefficients(&x_low, k), "k={}", k);
            }
        }
    }

    #[test]
    fn verify_prediction_coefficients() {
        let mut seed = 7u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as f32 / (1 << 24) as f32 - 0.5
        };

        let mut x_low = zero_low();
        // Subband 1: noise. Subband 2: an AR(2) process with complex poles. Subband 3: zero.
        for l in 0..X_SLOTS {
            x_low[l][1] = Complex::new(rnd(), rnd());
        }
        let pole = Complex::from_polar(0.9f32, 0.3f32);
        for l in 0..X_SLOTS {
            let e = Complex::new(rnd(), rnd()) * 0.1;
            let p1 = if l >= 1 { x_low[l - 1][2] } else { Complex::new(0.0, 0.0) };
            let p2 = if l >= 2 { x_low[l - 2][2] } else { Complex::new(0.0, 0.0) };
            x_low[l][2] = e + p1 * (pole * 2.0) - p2 * (pole * pole);
        }

        for k in 0..4 {
            let col: Vec<Complex<f64>> = (0..X_SLOTS)
                .map(|l| Complex::new(f64::from(x_low[l][k].re), f64::from(x_low[l][k].im)))
                .collect();
            let (e0, e1) = direct_coefficients(&col);
            let (a0, a1) = coefficients(&x_low, k);
            assert!((f64::from(a0.re) - e0.re).abs() < 1e-5, "k={} {:?} {:?}", k, a0, e0);
            assert!((f64::from(a0.im) - e0.im).abs() < 1e-5, "k={} {:?} {:?}", k, a0, e0);
            assert!((f64::from(a1.re) - e1.re).abs() < 1e-5, "k={} {:?} {:?}", k, a1, e1);
            assert!((f64::from(a1.im) - e1.im).abs() < 1e-5, "k={} {:?} {:?}", k, a1, e1);
        }

        // The zero subband has zero coefficients.
        assert_eq!(coefficients(&x_low, 3), (Complex::new(0.0, 0.0), Complex::new(0.0, 0.0)));

        // The AR(2) subband has a prediction gain: the coefficients are non-trivial.
        let (a0, a1) = coefficients(&x_low, 2);

        // The coefficients of a subband do not depend on the other subbands computed with it.
        let mut alpha0 = [Complex::new(0.0, 0.0); QMF_ANALYSIS_BANDS];
        let mut alpha1 = [Complex::new(0.0, 0.0); QMF_ANALYSIS_BANDS];
        prediction_coefficients(&x_low, 0..QMF_ANALYSIS_BANDS, &mut alpha0, &mut alpha1);
        for k in 0..QMF_ANALYSIS_BANDS {
            assert_eq!((alpha0[k], alpha1[k]), coefficients(&x_low, k));
        }
        assert!(a0.norm() > 0.5 && a1.norm() > 0.2);
    }

    /// The direct evaluation of the covariance method (4.6.18.6.2) of one subband, in single
    /// precision input and double precision accumulation.
    fn direct_coefficients_f32(x_low: &XLow, k: usize) -> (Complex<f32>, Complex<f32>) {
        let col: Vec<Complex<f64>> = (0..X_SLOTS)
            .map(|l| Complex::new(f64::from(x_low[l][k].re), f64::from(x_low[l][k].im)))
            .collect();
        let (a0, a1) = direct_coefficients(&col);
        (Complex::new(a0.re as f32, a0.im as f32), Complex::new(a1.re as f32, a1.im as f32))
    }

    #[test]
    fn verify_generate_against_direct_patching() {
        // Patching with chirp factors and random X_Low, compared with a direct evaluation of
        // 4.6.18.6.3 one target subband at a time.
        let mut seed = 3u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as f32 / (1 << 24) as f32 - 0.5
        };

        let mut x_low = zero_low();
        for row in x_low.iter_mut() {
            for x in row.iter_mut() {
                *x = Complex::new(rnd() * 100.0, rnd() * 100.0);
            }
        }

        let mut hf_gen = HfGenerator::new();
        hf_gen.update_chirp_factors(&[1, 2, 3]);
        hf_gen.update_chirp_factors(&[3, 1, 2]);
        let bw = hf_gen.bw_array;

        let kx = 22;
        let f_table_noise = [22, 30, 40, 50];
        let patches = [
            Patch { num_subbands: 12, start_subband: 4 },
            Patch { num_subbands: 10, start_subband: 9 },
            Patch { num_subbands: 0, start_subband: 31 },
            Patch { num_subbands: 6, start_subband: 1 },
        ];
        let slots = 2..34;

        let mut x_high = zero_high();
        hf_gen.generate(&x_low, kx, &patches, &f_table_noise, slots.clone(), &mut x_high);

        let mut k = kx;
        for patch in &patches {
            for x in 0..patch.num_subbands {
                let p = patch.start_subband + x;
                let (alpha0, alpha1) = direct_coefficients_f32(&x_low, p);
                let bw = bw[noise_band(&f_table_noise, k)];
                let (a0, a1) = (alpha0 * bw, alpha1 * (bw * bw));
                // The QMF subsamples slots.start and slots.start + 1 are work space.
                for l in (0..X_SLOTS).filter(|l| !(slots.start..slots.start + 2).contains(l)) {
                    let expected = if l >= slots.start + 2 && l < slots.end + 2 {
                        x_low[l][p] + a0 * x_low[l - 1][p] + a1 * x_low[l - 2][p]
                    }
                    else {
                        Complex::new(0.0, 0.0)
                    };
                    let err = (x_high[l][k] - expected).norm();
                    assert!(err <= 1e-4 * expected.norm() + 1e-3, "l={} k={} err={}", l, k, err);
                }
                k += 1;
            }
        }
        assert!(
            x_high.iter().all(|row| row[..kx].iter().chain(&row[k..]).all(|x| x.norm() == 0.0))
        );
    }

    #[test]
    fn verify_generate() {
        let mut x_low = zero_low();
        for l in 0..X_SLOTS {
            for k in 0..QMF_ANALYSIS_BANDS {
                x_low[l][k] = Complex::new((l * 32 + k) as f32, -(k as f32));
            }
        }

        let mut hf_gen = HfGenerator::new();
        // Chirp factors zero: X_High is a pure copy.
        hf_gen.update_chirp_factors(&[0, 0]);

        let patches = [
            Patch { num_subbands: 4, start_subband: 2 },
            Patch { num_subbands: 3, start_subband: 5 },
        ];
        let mut x_high = zero_high();
        hf_gen.generate(&x_low, 20, &patches, &[20, 24, 27], 4..30, &mut x_high);

        for l in 0..X_SLOTS {
            // The QMF subsamples 4 and 5 are work space.
            if l == 4 || l == 5 {
                continue;
            }
            for k in 0..QMF_BANDS {
                let expected = if (6..32).contains(&l) && (20..27).contains(&k) {
                    let p = if k < 24 { k - 20 + 2 } else { k - 24 + 5 };
                    x_low[l][p]
                }
                else {
                    Complex::new(0.0, 0.0)
                };
                assert_eq!(x_high[l][k], expected, "l={} k={}", l, k);
            }
        }

        // Out of range patches and slots must not panic.
        let patches = [
            Patch { num_subbands: 40, start_subband: 30 },
            Patch { num_subbands: usize::MAX, start_subband: usize::MAX },
        ];
        hf_gen.generate(&x_low, 60, &patches, &[], 0..100, &mut x_high);
        let empty = std::ops::Range { start: 50, end: 10 };
        hf_gen.generate(&x_low, usize::MAX, &patches, &[1, 2], empty, &mut x_high);
    }
}
