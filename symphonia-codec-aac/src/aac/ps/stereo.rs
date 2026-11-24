// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Stereo processing (ISO/IEC 14496-3:2009, 8.6.4.6): mapping of the stereo parameters to the
//! stereo bands, dequantization, the mixing procedures R_a and R_b, the phase parameters, the
//! interpolation of the mixing matrices, and incomplete parameter sets.

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, SQRT_2};

use symphonia_core::dsp::complex::Complex;

use crate::aac::sbr::types::QMF_BANDS;

use super::decorrelation::TransientGains;
use super::syntax::{ParamSet, PsFrame};
use super::tables::{
    ICC_GRID, ICC_MAX_INDEX, IID_GRID_DEFAULT, IID_GRID_FINE, IID_MAX_INDEX_DEFAULT,
    IID_MAX_INDEX_FINE, MAP_20_TO_34, MAP_34_TO_20, MAX_NR_BANDS, MAX_NR_PAR_BANDS, MAX_NUM_ENV,
    NR_ICC_PAR_TAB, NR_IID_PAR_TAB, NUM_QMF_SLOTS, StereoBands,
};

/// The mixing coefficients h11, h12, h21, h22 of one stereo band, or H11, H12, H21, H22 of one
/// hybrid band (8.6.4.6.2).
pub(super) type MixCoeffs = [Complex<f32>; 4];

/// The mixing coefficients of a mono output: l = r = s.
const IDENTITY: MixCoeffs = [
    Complex::new(1.0, 0.0),
    Complex::new(1.0, 0.0),
    Complex::new(0.0, 0.0),
    Complex::new(0.0, 0.0),
];

const ZERO: MixCoeffs = [Complex::new(0.0, 0.0); 4];

/// Map parameter indices given for `nr_src` (10, 20, or 34) parameters to the number of stereo
/// bands of `config` (8.6.4.6.1, Table 8.45, Table 8.46). Averages use integer arithmetic with
/// truncation towards zero, as ANSI-C.
pub(super) fn map_indices(
    src: &[i8; MAX_NR_PAR_BANDS],
    nr_src: usize,
    config: StereoBands,
    out: &mut [i32; MAX_NR_PAR_BANDS],
) {
    // First map 10 parameters to 20 by duplication.
    let mut grid: [i32; MAX_NR_PAR_BANDS] = std::array::from_fn(|b| i32::from(src[b]));
    let mut nr = nr_src;
    if nr == 10 {
        grid = std::array::from_fn(|b| if b < 20 { i32::from(src[b / 2]) } else { 0 });
        nr = 20;
    }

    *out = match (nr, config) {
        (20, StereoBands::Bands34) => std::array::from_fn(|b| {
            let (i, j) = MAP_20_TO_34[b];
            (grid[usize::from(i)] + grid[usize::from(j)]) / 2
        }),
        (34, StereoBands::Bands20) => std::array::from_fn(|b| match MAP_34_TO_20.get(b) {
            Some(row) => {
                let sum: i32 = row.iter().map(|&(i, w)| i32::from(w) * grid[usize::from(i)]).sum();
                let weights: i32 = row.iter().map(|&(_, w)| i32::from(w)).sum();
                sum / weights.max(1)
            }
            None => 0,
        }),
        _ => grid,
    };
}

/// Map mixing coefficients between the stereo band configurations (8.6.4.6.1, Table 8.47), using
/// Table 8.45 or Table 8.46 with the indices substituted by the coefficients.
pub(super) fn map_coeffs(
    h: &[MixCoeffs; MAX_NR_PAR_BANDS],
    from: StereoBands,
    to: StereoBands,
) -> [MixCoeffs; MAX_NR_PAR_BANDS] {
    match (from, to) {
        (StereoBands::Bands20, StereoBands::Bands34) => std::array::from_fn(|b| {
            let (i, j) = MAP_20_TO_34[b];
            let (hi, hj) = (h[usize::from(i)], h[usize::from(j)]);
            std::array::from_fn(|c| (hi[c] + hj[c]) * 0.5)
        }),
        (StereoBands::Bands34, StereoBands::Bands20) => {
            std::array::from_fn(|b| match MAP_34_TO_20.get(b) {
                Some(row) => {
                    let weights: f32 = row.iter().map(|&(_, w)| f32::from(w)).sum();
                    std::array::from_fn(|c| {
                        let sum: Complex<f32> =
                            row.iter().map(|&(i, w)| h[usize::from(i)][c] * f32::from(w)).sum();
                        sum / weights.max(1.0)
                    })
                }
                None => ZERO,
            })
        }
        _ => *h,
    }
}

/// The dequantized IID value iid(b) in dB (Table 8.25, Table 8.26).
fn dequantize_iid(index: i32, fine: bool) -> f32 {
    if fine {
        let i = index.clamp(-IID_MAX_INDEX_FINE, IID_MAX_INDEX_FINE) + IID_MAX_INDEX_FINE;
        IID_GRID_FINE[i as usize]
    }
    else {
        let i = index.clamp(-IID_MAX_INDEX_DEFAULT, IID_MAX_INDEX_DEFAULT) + IID_MAX_INDEX_DEFAULT;
        IID_GRID_DEFAULT[i as usize]
    }
}

/// The dequantized ICC value ρ(b) (Table 8.28).
fn dequantize_icc(index: i32) -> f32 {
    ICC_GRID[index.clamp(0, ICC_MAX_INDEX) as usize]
}

/// Mixing procedure R_a (8.6.4.6.2.1). Returns h11, h12, h21, h22.
pub(super) fn mixing_ra(c: f32, rho: f32) -> [f32; 4] {
    let norm = (1.0 + c * c).sqrt();
    let c1 = SQRT_2 / norm;
    let c2 = SQRT_2 * c / norm;
    let alpha = 0.5 * rho.clamp(-1.0, 1.0).acos();
    let beta = alpha * (c1 - c2) / SQRT_2;
    [
        (alpha + beta).cos() * c2,
        (beta - alpha).cos() * c1,
        (alpha + beta).sin() * c2,
        (beta - alpha).sin() * c1,
    ]
}

/// Mixing procedure R_b (8.6.4.6.2.2). `c_is_one` is true if c(b) = 1, i.e. the IID is 0 dB.
/// Returns h11, h12, h21, h22.
pub(super) fn mixing_rb(c: f32, rho: f32, c_is_one: bool) -> [f32; 4] {
    // Prevent instabilities.
    let rho = rho.max(0.05);
    let alpha = if c_is_one { FRAC_PI_4 } else { 0.5 * (2.0 * c * rho / (c * c - 1.0)).atan() };
    // Modulo correction.
    let alpha = alpha - (alpha / FRAC_PI_2).floor() * FRAC_PI_2;
    let c_sum = c + 1.0 / c;
    let mu = (1.0 + (4.0 * rho * rho - 4.0) / (c_sum * c_sum)).clamp(0.0, 1.0);
    let sqrt_mu = mu.sqrt();
    let gamma = ((1.0 - sqrt_mu) / (1.0 + sqrt_mu)).sqrt().atan();
    let (sin_a, cos_a) = alpha.sin_cos();
    let (sin_g, cos_g) = gamma.sin_cos();
    [
        SQRT_2 * cos_a * cos_g,
        SQRT_2 * sin_a * cos_g,
        -SQRT_2 * sin_a * sin_g,
        SQRT_2 * cos_a * sin_g,
    ]
}

/// exp(j idx π/4), the dequantized IPD/OPD value as a unit phasor (Table 8.31).
fn phase_phasor(index: i32) -> Complex<f32> {
    let phase = (index.rem_euclid(8) as f32) * FRAC_PI_4;
    Complex::from_polar(1.0, phase)
}

/// The parameters that determine the mixing coefficients of a parameter position.
#[derive(Clone, Copy, Debug, PartialEq)]
struct HoldKey {
    params: ParamSet,
    use_rb: bool,
    enable_ipdopd: bool,
}

/// The stereo processing state.
pub(super) struct StereoState {
    /// The stereo band configuration of the previous frame.
    config: StereoBands,
    /// h_ij(b) at the end of the previous frame, from which H_ij(k, n_-1) is derived, for the
    /// stereo bands of `config`.
    h_prev: [MixCoeffs; MAX_NR_PAR_BANDS],
    /// The IPD indices at the parameter positions n_e-1 and n_e for smoothing, mapped to the stereo
    /// bands of `config`.
    ipd_hist: [[i32; MAX_NR_PAR_BANDS]; 2],
    /// The OPD indices at the parameter positions n_e-1 and n_e.
    opd_hist: [[i32; MAX_NR_PAR_BANDS]; 2],
    /// The parameters `h_prev` was derived from, if the previous frame was processed with them.
    last_key: Option<HoldKey>,
    /// The parameter positions n_e of the current frame.
    positions: [usize; MAX_NUM_ENV],
    /// h_ij(b) at the parameter positions of the current frame.
    h_env: [[MixCoeffs; MAX_NR_PAR_BANDS]; MAX_NUM_ENV],
    /// The number of parameter positions of the current frame.
    num_pos: usize,
    /// True until the first stereo frame after a reset or flush was processed.
    first_frame: bool,
}

impl StereoState {
    pub fn new() -> Self {
        StereoState {
            config: StereoBands::Bands20,
            // H(k, n_-1) is initialized to zero for the first stereo frame (8.6.4.6.4).
            h_prev: [ZERO; MAX_NR_PAR_BANDS],
            ipd_hist: [[0; MAX_NR_PAR_BANDS]; 2],
            opd_hist: [[0; MAX_NR_PAR_BANDS]; 2],
            last_key: None,
            positions: [0; MAX_NUM_ENV],
            h_env: [[ZERO; MAX_NR_PAR_BANDS]; MAX_NUM_ENV],
            num_pos: 0,
            first_frame: true,
        }
    }

    pub fn reset(&mut self) {
        *self = StereoState::new();
    }

    /// The stereo band configuration of the previous frame.
    pub fn config(&self) -> StereoBands {
        self.config
    }

    /// Change the stereo band configuration (Table 8.47): map the mixing coefficients at the end of
    /// the previous frame to the new configuration and reset the IPD/OPD smoothing state.
    pub fn set_config(&mut self, config: StereoBands) {
        if config != self.config {
            self.h_prev = map_coeffs(&self.h_prev, self.config, config);
            self.ipd_hist = [[0; MAX_NR_PAR_BANDS]; 2];
            self.opd_hist = [[0; MAX_NR_PAR_BANDS]; 2];
            self.last_key = None;
            self.config = config;
        }
    }

    /// Record that the frame was output as mono, which is equivalent to stereo processing with all
    /// parameters at their default values.
    ///
    /// Mono frames before the first stereo frame (after a reset or flush) do not change the initial
    /// state of the first stereo frame: H(k, n_-1) remains zero (8.6.4.6.4).
    pub fn set_mono(&mut self) {
        if !self.first_frame {
            self.h_prev = [IDENTITY; MAX_NR_PAR_BANDS];
        }
        self.ipd_hist = [[0; MAX_NR_PAR_BANDS]; 2];
        self.opd_hist = [[0; MAX_NR_PAR_BANDS]; 2];
        self.last_key = None;
        self.num_pos = 0;
    }

    /// Compute the mixing coefficients h_ij(b) for a parameter set, including the IPD/OPD phase
    /// smoothing which advances the smoothing state (8.6.4.6.2, 8.6.4.6.3).
    fn compute_coeffs(&mut self, key: &HoldKey) -> [MixCoeffs; MAX_NR_PAR_BANDS] {
        let config = self.config;
        let params = &key.params;

        let mut iid = [0; MAX_NR_PAR_BANDS];
        let mut icc = [0; MAX_NR_PAR_BANDS];
        let mut ipd = [0; MAX_NR_PAR_BANDS];
        let mut opd = [0; MAX_NR_PAR_BANDS];

        let mut iid_fine = false;
        if let Some(mode) = params.iid_mode {
            map_indices(&params.iid_par, NR_IID_PAR_TAB[usize::from(mode)], config, &mut iid);
            iid_fine = mode >= 3;
        }
        if let Some(mode) = params.icc_mode {
            map_indices(&params.icc_par, NR_ICC_PAR_TAB[usize::from(mode)], config, &mut icc);
        }
        if key.enable_ipdopd {
            if let Some(mode) = params.ipdopd_mode {
                // IPD/OPD parameters follow the mapping of the IID parameters (8.6.4.6.1).
                let nr = NR_IID_PAR_TAB[usize::from(mode)];
                map_indices(&params.ipd_par, nr, config, &mut ipd);
                map_indices(&params.opd_par, nr, config, &mut opd);
            }
        }

        let mut h = [ZERO; MAX_NR_PAR_BANDS];
        for (b, h_b) in h.iter_mut().enumerate().take(config.nr_par_bands()) {
            let c = 10f32.powf(dequantize_iid(iid[b], iid_fine) / 20.0);
            let rho = dequantize_icc(icc[b]);
            let mix = if key.use_rb { mixing_rb(c, rho, iid[b] == 0) } else { mixing_ra(c, rho) };
            *h_b = mix.map(|x| Complex::new(x, 0.0));

            if key.enable_ipdopd {
                // Smoothing over the parameter positions n_e-1, n_e, and n_e+1 (8.6.4.6.3.2).
                let smooth = |hist: &[[i32; MAX_NR_PAR_BANDS]; 2], cur: i32| {
                    (phase_phasor(hist[0][b]) * 0.25
                        + phase_phasor(hist[1][b]) * 0.5
                        + phase_phasor(cur))
                    .arg()
                };
                let phi_opd = smooth(&self.opd_hist, opd[b]);
                let phi_ipd = smooth(&self.ipd_hist, ipd[b]);
                let rot1 = Complex::from_polar(1.0, phi_opd);
                let rot2 = Complex::from_polar(1.0, phi_opd - phi_ipd);
                h_b[0] *= rot1;
                h_b[1] *= rot2;
                h_b[2] *= rot1;
                h_b[3] *= rot2;
            }
        }

        // Advance the IPD/OPD smoothing state. Without IPD/OPD, the parameters are 0 (8.5.2).
        self.ipd_hist = [self.ipd_hist[1], ipd];
        self.opd_hist = [self.opd_hist[1], opd];

        h
    }

    /// Compute the parameter positions and mixing coefficients of a frame (8.6.4.6.2 to 8.6.4.6.5).
    pub fn prepare(&mut self, frame: &PsFrame) {
        let use_rb = frame.header.use_mixing_rb();
        let enable_ipdopd = frame.enable_ipdopd;

        if frame.num_env == 0 {
            let key = HoldKey { params: frame.params[0], use_rb, enable_ipdopd };
            if self.last_key == Some(key) {
                // All parameters are held: H(k, n) is that of the end of the previous frame for
                // the whole frame (8.6.4.6.5).
                self.num_pos = 0;
            }
            else {
                // Some parameters changed to their default value, or the parameters to hold were
                // not applied in the previous frame: apply them at the end of this frame.
                self.positions[0] = NUM_QMF_SLOTS - 1;
                self.h_env[0] = self.compute_coeffs(&key);
                self.num_pos = 1;
                self.last_key = Some(key);
            }
            return;
        }

        let num_env = frame.num_env.min(MAX_NUM_ENV);
        for e in 0..num_env {
            let key = HoldKey { params: frame.params[e], use_rb, enable_ipdopd };
            self.positions[e] = frame.border_position[e].min(NUM_QMF_SLOTS - 1);
            self.h_env[e] = self.compute_coeffs(&key);
            self.last_key = Some(key);
        }
        self.num_pos = num_env;
    }

    /// Returns true if the mixing coefficients of all QMF slots of the current frame are
    /// real-valued, i.e., if no phase parameters are applied.
    pub fn is_real(&self) -> bool {
        let num_bands = self.config.nr_par_bands();
        std::iter::once(&self.h_prev)
            .chain(&self.h_env[..self.num_pos.min(MAX_NUM_ENV)])
            .all(|h| h[..num_bands].iter().flatten().all(|x| x.im == 0.0))
    }

    /// Finish the frame: the mixing coefficients of the last parameter position become those of
    /// the end of the previous frame.
    pub fn finish(&mut self) {
        if self.num_pos > 0 {
            self.h_prev = self.h_env[self.num_pos - 1];
        }
        self.first_frame = false;
    }

    /// Get the mixing coefficients H_ij(b, n) of all stereo bands b of the current configuration
    /// for QMF slot `n`, interpolated between the parameter positions (8.6.4.6.4), and write them
    /// to `h[b]`.
    ///
    /// The slots up to the first parameter position n_0 interpolate from H(b, n_-1), which applies
    /// to the last slot of the previous frame (8.6.4.4), to H(b, n_0). The following slots
    /// interpolate from H(b, n_e) to H(b, n_e+1), and the slots after the last parameter position
    /// hold its mixing coefficients (special case b).
    /// Parameter positions that are not after the previous one only update the coefficients the
    /// next interpolation starts from.
    pub fn slot_coeffs(&self, n: usize, h: &mut [MixCoeffs; MAX_NR_PAR_BANDS]) {
        let num_bands = self.config.nr_par_bands();

        // Slots are numbered from 1 here, such that slot 0 is the last slot of the previous frame.
        let mut from = &self.h_prev;
        let mut n_from = 0;

        for (&n_e, to) in self.positions.iter().zip(&self.h_env).take(self.num_pos) {
            let n_to = n_e.min(NUM_QMF_SLOTS - 1) + 1;

            if n < n_to {
                let span = n_to.saturating_sub(n_from);
                if span == 0 {
                    h[..num_bands].copy_from_slice(&to[..num_bands]);
                }
                else {
                    let t = (n + 1).saturating_sub(n_from) as f32 / span as f32;
                    for ((h, from), to) in h.iter_mut().zip(from).zip(to).take(num_bands) {
                        *h = std::array::from_fn(|c| from[c] + (to[c] - from[c]) * t);
                    }
                }
                return;
            }

            from = to;
            n_from = n_to;
        }

        h[..num_bands].copy_from_slice(&from[..num_bands]);
    }

    /// Get H_ij(k, n_-1) and H_ij(k, n_e) for the parameter positions of the current frame, for a
    /// hybrid band with stereo band `b` (8.6.4.6.3). Returns the number of parameter positions.
    #[cfg(test)]
    pub fn band_coeffs(
        &self,
        b: usize,
        conjugate: bool,
        start: &mut MixCoeffs,
        envs: &mut [(usize, MixCoeffs); MAX_NUM_ENV],
    ) -> usize {
        let fix = |h: &MixCoeffs| if conjugate { h.map(|x| x.conj()) } else { *h };
        *start = self.h_prev.get(b).map_or(ZERO, fix);
        for (env, (&n_e, h)) in envs.iter_mut().zip(self.positions.iter().zip(&self.h_env)) {
            *env = (n_e, h.get(b).map_or(ZERO, fix));
        }
        self.num_pos
    }
}

/// The mixing (8.6.4.6.2) and hybrid synthesis (8.6.4.7) of one QMF slot n of all hybrid bands
/// k:
///
/// l_k(n) = H11(k, n) s_k(n) + H21(k, n) d_k(n),
/// r_k(n) = H12(k, n) s_k(n) + H22(k, n) d_k(n),
///
/// where H(k, n) is `h[b(k)]`, conjugated for the hybrid bands that use the conjugated mixing
/// coefficients, and d_k(n) is the de-correlated signal after transient reduction (8.6.4.5.4),
/// `gain[b(k)][n] * d[k]`. The sub subbands of each split QMF subband are summed, all other hybrid
/// bands are QMF subbands.
pub(super) struct SlotMix<'a> {
    pub config: StereoBands,
    pub h: &'a [MixCoeffs; MAX_NR_PAR_BANDS],
    pub s: &'a [Complex<f32>; MAX_NR_BANDS],
    pub d: &'a [Complex<f32>; MAX_NR_BANDS],
    pub gain: &'a TransientGains,
    pub n: usize,
}

impl SlotMix<'_> {
    /// Mix the QMF slot and write the QMF slot of the left and right channel to `left` and
    /// `right`.
    pub fn mix(&self, left: &mut [Complex<f32>; QMF_BANDS], right: &mut [Complex<f32>; QMF_BANDS]) {
        self.mix_split(left, right);

        let offset = self.config.qmf_to_hybrid_offset();

        // The unsplit QMF subbands, in runs of hybrid bands of the same stereo band.
        for &(start, end, b) in self.config.unsplit_runs() {
            let [h11, h12, h21, h22] = self.h[b];
            let g = self.gain[b][self.n];

            let out = left[start - offset..end - offset]
                .iter_mut()
                .zip(&mut right[start - offset..end - offset]);
            let src = self.s[start..end].iter().zip(&self.d[start..end]);

            for ((l, r), (&s, &d)) in out.zip(src) {
                let d = d * g;
                *l = h11 * s + h21 * d;
                *r = h12 * s + h22 * d;
            }
        }
    }

    /// Like `mix`, for mixing coefficients that are all real-valued.
    pub fn mix_real(
        &self,
        left: &mut [Complex<f32>; QMF_BANDS],
        right: &mut [Complex<f32>; QMF_BANDS],
    ) {
        self.mix_split(left, right);

        let offset = self.config.qmf_to_hybrid_offset();

        for &(start, end, b) in self.config.unsplit_runs() {
            let [h11, h12, h21, h22] = self.h[b].map(|h| h.re);
            let g = self.gain[b][self.n];

            let out = left[start - offset..end - offset]
                .iter_mut()
                .zip(&mut right[start - offset..end - offset]);
            let src = self.s[start..end].iter().zip(&self.d[start..end]);

            for ((l, r), (&s, &d)) in out.zip(src) {
                let d = d * g;
                *l = s * h11 + d * h21;
                *r = s * h12 + d * h22;
            }
        }
    }

    /// Mix the sub subbands of the split QMF subbands, and sum them to their QMF subband.
    fn mix_split(
        &self,
        left: &mut [Complex<f32>; QMF_BANDS],
        right: &mut [Complex<f32>; QMF_BANDS],
    ) {
        let config = self.config;
        let num_split = config.num_split_qmf_bands();
        left[..num_split].fill(Complex::new(0.0, 0.0));
        right[..num_split].fill(Complex::new(0.0, 0.0));

        let bands = config.param_index_table().iter().zip(config.sub_qmf_band_table());
        let src = self.s.iter().zip(self.d);

        for (k, ((&b, &p), (&s, &d))) in bands.zip(src).enumerate() {
            let b = usize::from(b);
            let p = usize::from(p);
            let h = self.h[b];
            let h = if config.is_conjugated(k) { h.map(|x| x.conj()) } else { h };
            let d = d * self.gain[b][self.n];
            left[p] += h[0] * s + h[2] * d;
            right[p] += h[1] * s + h[3] * d;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_map_indices() {
        let mut src = [0i8; MAX_NR_PAR_BANDS];
        for (b, x) in src.iter_mut().enumerate() {
            *x = b as i8 - 10;
        }
        let mut out = [0; MAX_NR_PAR_BANDS];

        // 10 -> 20: duplication.
        map_indices(&src, 10, StereoBands::Bands20, &mut out);
        for b in 0..20 {
            assert_eq!(out[b], i32::from(src[b / 2]));
        }

        // 20 -> 34 with averages truncated towards zero: (idx2 + idx3) / 2 = (-8 + -7) / 2 = -7.
        map_indices(&src, 20, StereoBands::Bands34, &mut out);
        assert_eq!(out[0], -10);
        assert_eq!(out[1], -9); // (-10 + -9) / 2 = -9 (truncated towards zero).
        assert_eq!(out[4], -7);
        assert_eq!(out[33], 9);

        // 10 -> 34 equals the 10 column of Table 8.45.
        map_indices(&src, 10, StereoBands::Bands34, &mut out);
        assert_eq!(out[16], i32::from(src[5]));
        assert_eq!(out[33], i32::from(src[9]));

        // 34 -> 20 (Table 8.46): (2 idx0 + idx1) / 3 = (-20 - 9) / 3 = -9.
        map_indices(&src, 34, StereoBands::Bands20, &mut out);
        assert_eq!(out[0], -9);
        assert_eq!(out[18], (18 + 19 + 20 + 21) / 4);

        // Same resolution: copy.
        map_indices(&src, 34, StereoBands::Bands34, &mut out);
        assert_eq!(out[33], 23);
    }

    #[test]
    fn verify_map_coeffs_round_trip() {
        // Mapping constant coefficients 20 -> 34 -> 20 preserves them.
        let h: [MixCoeffs; MAX_NR_PAR_BANDS] = std::array::from_fn(|b| {
            let x = Complex::new(b as f32, -(b as f32));
            [x, x * 2.0, x * 3.0, x * 4.0]
        });
        let up = map_coeffs(&h, StereoBands::Bands20, StereoBands::Bands34);
        assert_eq!(up[1][0], (h[0][0] + h[1][0]) * 0.5);
        assert_eq!(up[33][3], h[19][3]);
        let identity = [IDENTITY; MAX_NR_PAR_BANDS];
        let down = map_coeffs(
            &map_coeffs(&identity, StereoBands::Bands20, StereoBands::Bands34),
            StereoBands::Bands34,
            StereoBands::Bands20,
        );
        for b in 0..20 {
            for c in 0..4 {
                assert!((down[b][c] - IDENTITY[c]).norm() < 1e-6);
            }
        }
    }

    #[test]
    fn verify_mixing_default_parameters() {
        // IID = 0 dB and ICC = 1: l = r = s for both mixing procedures.
        for h in [mixing_ra(1.0, 1.0), mixing_rb(1.0, 1.0, true)] {
            assert!((h[0] - 1.0).abs() < 1e-6);
            assert!((h[1] - 1.0).abs() < 1e-6);
            assert!(h[2].abs() < 1e-6);
            assert!(h[3].abs() < 1e-6);
        }
    }

    #[test]
    fn verify_mixing_procedures() {
        let iid_db = [-50.0f32, -25.0, -10.0, -2.0, 2.0, 7.0, 25.0, 50.0];
        for &iid in iid_db.iter() {
            let c = 10f32.powf(iid / 20.0);
            // With ICC = 1, R_a and R_b are equivalent.
            let ra = mixing_ra(c, 1.0);
            let rb = mixing_rb(c, 1.0, false);
            for i in 0..4 {
                assert!((ra[i] - rb[i]).abs() < 1e-4, "iid {iid}: {ra:?} {rb:?}");
            }
            for &rho in ICC_GRID.iter() {
                for h in [mixing_ra(c, rho), mixing_rb(c, rho, false)] {
                    // The left/right power ratio of the reconstruction equals c^2 (for
                    // uncorrelated s and d of equal power), and the total power is 2.
                    let pl = h[0] * h[0] + h[2] * h[2];
                    let pr = h[1] * h[1] + h[3] * h[3];
                    assert!((pl + pr - 2.0).abs() < 1e-3, "iid {iid}, rho {rho}");
                    assert!(((pl / pr).log10() * 10.0 - iid).abs() < 0.01, "iid {iid}, rho {rho}");
                }
                // The normalized correlation of l and r equals ρ (R_a), or ρ limited to 0.05 or
                // more (R_b).
                for (h, target) in
                    [(mixing_ra(c, rho), rho), (mixing_rb(c, rho, false), rho.max(0.05))]
                {
                    let pl = h[0] * h[0] + h[2] * h[2];
                    let pr = h[1] * h[1] + h[3] * h[3];
                    let corr = (h[0] * h[1] + h[2] * h[3]) / (pl * pr).sqrt();
                    assert!((corr - target).abs() < 2e-3, "iid {iid}, rho {rho}, corr {corr}");
                }
            }
        }
    }

    #[test]
    fn verify_slot_coeffs_against_mix_band() {
        use super::super::reference::mix_band;

        // The mixing coefficients of each slot equal those that the band-wise mixing applies:
        // with s = 1 and d = 0, l = H11 and r = H12, and with s = 0 and d = 1, l = H21 and r = H22.
        let one = [Complex::new(1.0, 0.0); NUM_QMF_SLOTS];
        let zero = [Complex::new(0.0, 0.0); NUM_QMF_SLOTS];
        let coeffs = |seed: f32| -> [MixCoeffs; MAX_NR_PAR_BANDS] {
            std::array::from_fn(|b| {
                std::array::from_fn(|c| {
                    let x = seed + b as f32 * 0.37 + c as f32 * 1.91;
                    Complex::new(x.sin(), (1.3 * x).cos())
                })
            })
        };

        let position_sets: [&[usize]; 8] =
            [&[], &[0], &[31], &[7, 15], &[3, 9, 20, 31], &[5, 5, 12], &[20, 10, 25], &[40, 50]];

        for (i, positions) in position_sets.iter().enumerate() {
            for config in [StereoBands::Bands20, StereoBands::Bands34] {
                let mut state = StereoState::new();
                state.config = config;
                state.h_prev = coeffs(i as f32);
                for (e, &n_e) in positions.iter().enumerate() {
                    state.positions[e] = n_e;
                    state.h_env[e] = coeffs(10.0 * (e + 1) as f32 + i as f32);
                }
                state.num_pos = positions.len();

                let mut slot = vec![[ZERO; MAX_NR_PAR_BANDS]; NUM_QMF_SLOTS];
                for (n, h) in slot.iter_mut().enumerate() {
                    state.slot_coeffs(n, h);
                }

                for b in 0..config.nr_par_bands() {
                    for conjugate in [false, true] {
                        let mut start = ZERO;
                        let mut envs = [(0, ZERO); MAX_NUM_ENV];
                        let num_pos = state.band_coeffs(b, conjugate, &mut start, &mut envs);
                        let mut l = zero;
                        let mut r = zero;
                        mix_band(&start, &envs[..num_pos], &one, &zero, &mut l, &mut r);
                        let mut l2 = zero;
                        let mut r2 = zero;
                        mix_band(&start, &envs[..num_pos], &zero, &one, &mut l2, &mut r2);
                        for n in 0..NUM_QMF_SLOTS {
                            let h = slot[n][b];
                            let h = if conjugate { h.map(|x| x.conj()) } else { h };
                            assert_eq!([l[n], r[n], l2[n], r2[n]], h, "set {i}, b {b}, n {n}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn verify_real_mixing_path() {
        // With real-valued mixing coefficients, the real-valued mixing equals the complex-valued
        // mixing.
        let mut seed = 1u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as f32 / (1 << 24) as f32 - 0.5
        };
        for config in [StereoBands::Bands20, StereoBands::Bands34] {
            let h: [MixCoeffs; MAX_NR_PAR_BANDS] =
                std::array::from_fn(|_| std::array::from_fn(|_| Complex::new(rnd(), 0.0)));
            let s: [Complex<f32>; MAX_NR_BANDS] =
                std::array::from_fn(|_| Complex::new(rnd(), rnd()));
            let d: [Complex<f32>; MAX_NR_BANDS] =
                std::array::from_fn(|_| Complex::new(rnd(), rnd()));
            let gain: TransientGains =
                std::array::from_fn(|_| std::array::from_fn(|_| rnd() + 0.5));
            for n in [0, 17, 31] {
                let mix = SlotMix { config, h: &h, s: &s, d: &d, gain: &gain, n };
                let mut out = [[Complex::new(1.0, 1.0); QMF_BANDS]; 4];
                let [l, r, l_real, r_real] = &mut out;
                mix.mix(l, r);
                mix.mix_real(l_real, r_real);
                assert_eq!(l, l_real);
                assert_eq!(r, r_real);
            }
        }
    }

    #[test]
    fn verify_mix_band_interpolation() {
        use super::super::reference::mix_band;

        let s = [Complex::new(1.0, 0.0); NUM_QMF_SLOTS];
        let d = [Complex::new(0.0, 0.0); NUM_QMF_SLOTS];
        let mut l = [Complex::new(0.0, 0.0); NUM_QMF_SLOTS];
        let mut r = l;
        let h = |x: f32| [Complex::new(x, 0.0), Complex::new(-x, 0.0), ZERO[0], ZERO[0]];

        // Positions 7 and 15: ramp from -1 at the last slot of the previous frame to 7 at n = 7,
        // then to 15 at n = 15, then hold.
        let envs = [(7, h(7.0)), (15, h(15.0))];
        mix_band(&h(-1.0), &envs, &s, &d, &mut l, &mut r);
        for n in 0..NUM_QMF_SLOTS {
            let expected = n.min(15) as f32;
            assert!((l[n].re - expected).abs() < 1e-5, "n {n}");
            assert!((r[n].re + expected).abs() < 1e-5, "n {n}");
        }

        // A first position at n = 0 and no positions.
        mix_band(&h(5.0), &[(0, h(1.0))], &s, &d, &mut l, &mut r);
        assert!(l.iter().all(|x| (x.re - 1.0).abs() < 1e-6));
        mix_band(&h(5.0), &[], &s, &d, &mut l, &mut r);
        assert!(l.iter().all(|x| (x.re - 5.0).abs() < 1e-6));
    }
}
