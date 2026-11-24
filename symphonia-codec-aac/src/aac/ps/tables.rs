// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Constant tables and configuration of the parametric stereo tool (ISO/IEC 14496-3:2009, 8.5.2
//! and 8.6.4).

use lazy_static::lazy_static;

use crate::aac::sbr::types::QMF_SLOTS;

/// numQMFSlots, the number of QMF subband samples per stereo frame. When combined with SBR, a
/// stereo frame is identical to an SBR frame (Annex 8.A.3).
pub(super) const NUM_QMF_SLOTS: usize = QMF_SLOTS;

/// Maximum number of parameter sets (envelopes) per ps_data() element (Table 8.29).
pub(super) const MAX_NUM_ENV: usize = 4;

/// Maximum number of stereo bands (parameters) per parameter type, NR_PAR_BANDS for the 34 stereo
/// bands configuration (8.6.4.5.1).
pub(super) const MAX_NR_PAR_BANDS: usize = 34;

/// Maximum number of hybrid sub subbands, NR_BANDS for the 34 stereo bands configuration
/// (8.6.4.5.1).
pub(super) const MAX_NR_BANDS: usize = 91;

/// Number of QMF subbands that are split by the hybrid analysis filterbank in the 34 stereo bands
/// configuration (Table 8.36).
pub(super) const MAX_SPLIT_QMF_BANDS: usize = 5;

/// Maximum number of hybrid sub subbands generated from the split QMF subbands, in the 34 stereo
/// bands configuration (Figure 8.22).
pub(super) const MAX_NUM_SUB_QMF_BANDS: usize = 32;

/// Length of the hybrid filterbank prototype filters (8.6.4.3).
pub(super) const HYBRID_FILTER_LEN: usize = 13;

/// Delay of the hybrid filterbank prototype filters in QMF samples (8.6.4.3).
pub(super) const HYBRID_FILTER_DELAY: usize = 6;

/// The hybrid filterbank configuration, which is selected by the number of stereo bands
/// (Table 8.36, Table 8.44).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StereoBands {
    /// The "10, 20 stereo bands" configuration.
    Bands20,
    /// The "34 stereo bands" configuration.
    Bands34,
}

impl StereoBands {
    /// Select the configuration from the number of IID and ICC parameters (Table 8.44).
    pub fn from_num_params(nr_iid_par: usize, nr_icc_par: usize) -> Self {
        if nr_iid_par == 34 || nr_icc_par == 34 {
            StereoBands::Bands34
        }
        else {
            StereoBands::Bands20
        }
    }

    /// NR_PAR_BANDS (8.6.4.5.1).
    pub fn nr_par_bands(self) -> usize {
        match self {
            StereoBands::Bands20 => 20,
            StereoBands::Bands34 => 34,
        }
    }

    /// NR_BANDS, the number of hybrid sub subbands (8.6.4.5.1).
    pub fn nr_bands(self) -> usize {
        match self {
            StereoBands::Bands20 => 71,
            StereoBands::Bands34 => 91,
        }
    }

    /// DECAY_CUTOFF (8.6.4.5.1).
    pub fn decay_cutoff(self) -> usize {
        match self {
            StereoBands::Bands20 => 10,
            StereoBands::Bands34 => 32,
        }
    }

    /// NR_ALLPASS_BANDS (8.6.4.5.1).
    pub fn nr_allpass_bands(self) -> usize {
        match self {
            StereoBands::Bands20 => 30,
            StereoBands::Bands34 => 50,
        }
    }

    /// SHORT_DELAY_BAND (8.6.4.5.1).
    pub fn short_delay_band(self) -> usize {
        match self {
            StereoBands::Bands20 => 42,
            StereoBands::Bands34 => 62,
        }
    }

    /// The number of QMF subbands that are split by the hybrid analysis filterbank (Table 8.36).
    pub fn num_split_qmf_bands(self) -> usize {
        match self {
            StereoBands::Bands20 => 3,
            StereoBands::Bands34 => 5,
        }
    }

    /// The number of hybrid sub subbands generated from the split QMF subbands (Figure 8.20,
    /// Figure 8.22).
    pub fn num_sub_qmf_bands(self) -> usize {
        match self {
            StereoBands::Bands20 => 10,
            StereoBands::Bands34 => 32,
        }
    }

    /// The difference between the index of a hybrid band that is an unsplit QMF subband and the
    /// index of that QMF subband. This is also the offset used for k_max (Annex 8.A.3).
    pub fn qmf_to_hybrid_offset(self) -> usize {
        match self {
            StereoBands::Bands20 => 7,
            StereoBands::Bands34 => 27,
        }
    }

    /// The parameter index b(k) of each hybrid sub subband k (Table 8.48, Table 8.49).
    pub fn param_index_table(self) -> &'static [u8] {
        match self {
            StereoBands::Bands20 => &PARAM_INDEX_20,
            StereoBands::Bands34 => &PARAM_INDEX_34,
        }
    }

    /// The runs of consecutive hybrid bands that are unsplit QMF subbands and have the same
    /// parameter index b(k) (Table 8.48, Table 8.49), as `(first k, last k + 1, b)`.
    pub fn unsplit_runs(self) -> &'static [(usize, usize, usize)] {
        let (runs, num_runs) = match self {
            StereoBands::Bands20 => &UNSPLIT_RUNS[0],
            StereoBands::Bands34 => &UNSPLIT_RUNS[1],
        };
        &runs[..*num_runs]
    }

    /// Returns true if the complex conjugate of the mixing coefficients is used for hybrid sub
    /// subband k (indices denoted with a * in Table 8.48 and Table 8.49).
    pub fn is_conjugated(self, k: usize) -> bool {
        match self {
            StereoBands::Bands20 => k < 2,
            StereoBands::Bands34 => (9..12).contains(&k),
        }
    }

    /// The QMF subband which hybrid sub subband k belongs to (Table 8.48, Table 8.49).
    #[cfg(test)]
    pub fn qmf_band(self, k: usize) -> usize {
        let sub = self.num_sub_qmf_bands();
        if k < sub {
            usize::from(self.sub_qmf_band_table()[k])
        }
        else {
            k - self.qmf_to_hybrid_offset()
        }
    }

    /// The QMF subband of each sub subband of the split QMF subbands.
    pub fn sub_qmf_band_table(self) -> &'static [u8] {
        match self {
            StereoBands::Bands20 => &SUB_QMF_BAND_20,
            StereoBands::Bands34 => &SUB_QMF_BAND_34,
        }
    }
}

/// The maximum number of runs returned by `StereoBands::unsplit_runs`.
const MAX_UNSPLIT_RUNS: usize = 18;

type UnsplitRuns = ([(usize, usize, usize); MAX_UNSPLIT_RUNS], usize);

/// Find the runs of consecutive unsplit hybrid bands with the same parameter index.
fn find_unsplit_runs(config: StereoBands) -> UnsplitRuns {
    let mut runs = [(0, 0, 0); MAX_UNSPLIT_RUNS];
    let mut num_runs = 0;
    let table = config.param_index_table();
    for k in config.num_sub_qmf_bands()..config.nr_bands() {
        let b = usize::from(table[k]);
        match runs[..num_runs].last_mut() {
            Some((_, end, last_b)) if *last_b == b && *end == k => *end = k + 1,
            _ => {
                runs[num_runs] = (k, k + 1, b);
                num_runs += 1;
            }
        }
    }
    (runs, num_runs)
}

lazy_static! {
    static ref UNSPLIT_RUNS: [UnsplitRuns; 2] =
        [find_unsplit_runs(StereoBands::Bands20), find_unsplit_runs(StereoBands::Bands34)];
}

/// nr_iid_par_tab, indexed by iid_mode (Table 8.24).
pub(super) static NR_IID_PAR_TAB: [usize; 6] = [10, 20, 34, 10, 20, 34];

/// nr_ipdopd_par_tab, indexed by iid_mode (Table 8.24).
pub(super) static NR_IPDOPD_PAR_TAB: [usize; 6] = [5, 11, 17, 5, 11, 17];

/// nr_icc_par_tab, indexed by icc_mode (Table 8.27).
pub(super) static NR_ICC_PAR_TAB: [usize; 6] = [10, 20, 34, 10, 20, 34];

/// num_env_tab[frame_class][num_env_idx] (Table 8.29).
pub(super) static NUM_ENV_TAB: [[usize; 4]; 2] = [[0, 1, 2, 4], [1, 2, 3, 4]];

/// The largest IID index magnitude for iid_quant = 0 (Table 8.24).
pub(super) const IID_MAX_INDEX_DEFAULT: i32 = 7;

/// The largest IID index magnitude for iid_quant = 1 (Table 8.24).
pub(super) const IID_MAX_INDEX_FINE: i32 = 15;

/// The largest ICC index (Table 8.27).
pub(super) const ICC_MAX_INDEX: i32 = 7;

/// Default quantization grid for IID in dB, indices -7..=7 (Table 8.25).
pub(super) static IID_GRID_DEFAULT: [f32; 15] =
    [-25.0, -18.0, -14.0, -10.0, -7.0, -4.0, -2.0, 0.0, 2.0, 4.0, 7.0, 10.0, 14.0, 18.0, 25.0];

/// Fine quantization grid for IID in dB, indices -15..=15 (Table 8.26).
#[rustfmt::skip]
pub(super) static IID_GRID_FINE: [f32; 31] = [
    -50.0, -45.0, -40.0, -35.0, -30.0, -25.0, -22.0, -19.0, -16.0, -13.0, -10.0, -8.0, -6.0, -4.0,
    -2.0, 0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 13.0, 16.0, 19.0, 22.0, 25.0, 30.0, 35.0, 40.0, 45.0,
    50.0,
];

/// Quantization grid for ICC, ρ, indices 0..=7 (Table 8.28).
pub(super) static ICC_GRID: [f32; 8] = [1.0, 0.937, 0.84118, 0.60092, 0.36764, 0.0, -0.589, -1.0];

/// Mapping from 20 to 34 parameters (Table 8.45). For each of the 34 parameters, the two indices
/// `(a, b)` of the 20 parameter grid whose average `(idx_a + idx_b) / 2` gives the parameter. A
/// plain copy has `a == b`.
#[rustfmt::skip]
pub(super) static MAP_20_TO_34: [(u8, u8); 34] = [
    (0, 0), (0, 1), (1, 1), (2, 2), (2, 3), (3, 3), (4, 4), (4, 4), (5, 5), (5, 5), (6, 6),
    (7, 7), (8, 8), (8, 8), (9, 9), (9, 9), (10, 10), (11, 11), (12, 12), (13, 13), (14, 14),
    (14, 14), (15, 15), (15, 15), (16, 16), (16, 16), (17, 17), (17, 17), (18, 18), (18, 18),
    (18, 18), (18, 18), (19, 19), (19, 19),
];

/// Mapping from 34 to 20 parameters (Table 8.46). For each of the 20 parameters, up to four pairs
/// `(index, weight)` of the 34 parameter grid. The parameter is the weighted sum divided by the sum
/// of the weights. Unused pairs have a weight of 0.
#[rustfmt::skip]
pub(super) static MAP_34_TO_20: [[(u8, u8); 4]; 20] = [
    [(0, 2), (1, 1), (0, 0), (0, 0)],
    [(1, 1), (2, 2), (0, 0), (0, 0)],
    [(3, 2), (4, 1), (0, 0), (0, 0)],
    [(4, 1), (5, 2), (0, 0), (0, 0)],
    [(6, 1), (7, 1), (0, 0), (0, 0)],
    [(8, 1), (9, 1), (0, 0), (0, 0)],
    [(10, 1), (0, 0), (0, 0), (0, 0)],
    [(11, 1), (0, 0), (0, 0), (0, 0)],
    [(12, 1), (13, 1), (0, 0), (0, 0)],
    [(14, 1), (15, 1), (0, 0), (0, 0)],
    [(16, 1), (0, 0), (0, 0), (0, 0)],
    [(17, 1), (0, 0), (0, 0), (0, 0)],
    [(18, 1), (0, 0), (0, 0), (0, 0)],
    [(19, 1), (0, 0), (0, 0), (0, 0)],
    [(20, 1), (21, 1), (0, 0), (0, 0)],
    [(22, 1), (23, 1), (0, 0), (0, 0)],
    [(24, 1), (25, 1), (0, 0), (0, 0)],
    [(26, 1), (27, 1), (0, 0), (0, 0)],
    [(28, 1), (29, 1), (30, 1), (31, 1)],
    [(32, 1), (33, 1), (0, 0), (0, 0)],
];

/// Parameter index b(k) for the 10, 20 stereo bands configuration (Table 8.48).
#[rustfmt::skip]
static PARAM_INDEX_20: [u8; 71] = [
    // Sub QMF (k = 0..=9).
    1, 0, 0, 1, 2, 3, 4, 5, 6, 7,
    // QMF only (k = 10..=70).
    8, 9, 10, 11, 12, 13, 14, 14, 15, 15, 15, 16, 16, 16, 16, 17, 17, 17, 17, 17,
    18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18,
    19, 19, 19, 19, 19, 19, 19, 19, 19, 19, 19, 19, 19, 19, 19, 19, 19, 19, 19, 19,
    19, 19, 19, 19, 19, 19, 19, 19, 19,
];

/// Parameter index b(k) for the 34 stereo bands configuration (Table 8.49).
#[rustfmt::skip]
static PARAM_INDEX_34: [u8; 91] = [
    // Sub QMF (k = 0..=31).
    0, 1, 2, 3, 4, 5, 6, 6, 7, 2, 1, 0,
    10, 10, 4, 5, 6, 7, 8, 9,
    10, 11, 12, 9,
    14, 11, 12, 13,
    14, 15, 16, 13,
    // QMF only (k = 32..=90).
    16, 17, 18, 19, 20, 21, 22, 22, 23, 23, 24, 24, 25, 25, 26, 26, 27, 27, 27, 28, 28, 28,
    29, 29, 29, 30, 30, 30, 31, 31, 31, 31, 32, 32, 32, 32,
    33, 33, 33, 33, 33, 33, 33, 33, 33, 33, 33, 33, 33, 33, 33, 33, 33, 33, 33, 33, 33, 33,
    33,
];

/// QMF subband of each sub subband for the 10, 20 stereo bands configuration (Figure 8.20).
static SUB_QMF_BAND_20: [u8; 10] = [0, 0, 0, 0, 0, 0, 1, 1, 2, 2];

/// QMF subband of each sub subband for the 34 stereo bands configuration (Figure 8.22).
#[rustfmt::skip]
static SUB_QMF_BAND_34: [u8; 32] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    1, 1, 1, 1, 1, 1, 1, 1,
    2, 2, 2, 2,
    3, 3, 3, 3,
    4, 4, 4, 4,
];

/// Prototype filter g^0[n], Q^0 = 8, for the 10, 20 stereo bands configuration (Table 8.37).
#[rustfmt::skip]
pub(super) static PROTO_20_Q8: [f64; HYBRID_FILTER_LEN] = [
    0.00746082949812, 0.02270420949825, 0.04546865930473, 0.07266113929591, 0.09885108575264,
    0.11793710567217, 0.125, 0.11793710567217, 0.09885108575264, 0.07266113929591,
    0.04546865930473, 0.02270420949825, 0.00746082949812,
];

/// Prototype filter g^{1,2}[n], Q^{1,2} = 2, for the 10, 20 stereo bands configuration
/// (Table 8.37).
#[rustfmt::skip]
pub(super) static PROTO_20_Q2: [f64; HYBRID_FILTER_LEN] = [
    0.0, 0.01899487526049, 0.0, -0.07293139167538, 0.0, 0.30596630545168, 0.5,
    0.30596630545168, 0.0, -0.07293139167538, 0.0, 0.01899487526049, 0.0,
];

/// Prototype filter g^0[n], Q^0 = 12, for the 34 stereo bands configuration (Table 8.38).
#[rustfmt::skip]
pub(super) static PROTO_34_Q12: [f64; HYBRID_FILTER_LEN] = [
    0.04081179924692, 0.03812810994926, 0.05144908135699, 0.06399831151592, 0.07428313801106,
    0.08100347892914, 0.08333333333333, 0.08100347892914, 0.07428313801106, 0.06399831151592,
    0.05144908135699, 0.03812810994926, 0.04081179924692,
];

/// Prototype filter g^1[n], Q^1 = 8, for the 34 stereo bands configuration (Table 8.38).
#[rustfmt::skip]
pub(super) static PROTO_34_Q8: [f64; HYBRID_FILTER_LEN] = [
    0.01565675600122, 0.03752716391991, 0.05417891378782, 0.08417044116767, 0.10307344158036,
    0.12222452249753, 0.125, 0.12222452249753, 0.10307344158036, 0.08417044116767,
    0.05417891378782, 0.03752716391991, 0.01565675600122,
];

/// Prototype filter g^{2,3,4}[n], Q^{2,3,4} = 4, for the 34 stereo bands configuration
/// (Table 8.38).
#[rustfmt::skip]
pub(super) static PROTO_34_Q4: [f64; HYBRID_FILTER_LEN] = [
    -0.05908211155639, -0.04871498374946, 0.0, 0.07778723915851, 0.16486303567403,
    0.23279856662996, 0.25, 0.23279856662996, 0.16486303567403, 0.07778723915851, 0.0,
    -0.04871498374946, -0.05908211155639,
];

/// DECAY_SLOPE, the all-pass filter decay slope (8.6.4.5.1).
pub(super) const DECAY_SLOPE: f64 = 0.05;

/// NR_ALLPASS_LINKS, the number of filter links of the all-pass filter (8.6.4.5.1).
pub(super) const NR_ALLPASS_LINKS: usize = 3;

/// a_Smooth, the smoothing coefficient (8.6.4.5.1).
pub(super) const A_SMOOTH: f32 = 0.25;

/// The all-pass filter coefficient vector a(m) (Table 8.39).
pub(super) static ALLPASS_A: [f64; NR_ALLPASS_LINKS] =
    [0.65143905753106, 0.56471812200776, 0.48954165955695];

/// The all-pass filter delay length vector d(m) (Table 8.39).
pub(super) const ALLPASS_D: [usize; NR_ALLPASS_LINKS] = [3, 4, 5];

/// The fractional delay length vector q(m) (Table 8.42).
pub(super) static FRACT_DELAY_Q: [f64; NR_ALLPASS_LINKS] = [0.43, 0.75, 0.347];

/// The fractional delay length constant q_φ (8.6.4.5.2).
pub(super) const FRACT_DELAY_Q_PHI: f64 = 0.39;

/// The pre-delay of the all-pass filtered bands in QMF samples (z^-2 in 8.6.4.5.2).
pub(super) const ALLPASS_PRE_DELAY: usize = 2;

/// The delay D(k) for NR_ALLPASS_BANDS <= k < SHORT_DELAY_BAND (8.6.4.5.2).
pub(super) const LONG_DELAY: usize = 14;

/// The delay D(k) for SHORT_DELAY_BAND <= k < NR_BANDS (8.6.4.5.2).
pub(super) const SHORT_DELAY: usize = 1;

/// The peak decay factor α (Table 8.43).
pub(super) const PEAK_DECAY_ALPHA: f64 = 0.76592833836465;

/// The transient impact factor γ (8.6.4.5.3).
pub(super) const TRANSIENT_IMPACT_GAMMA: f32 = 1.5;

/// f_center_20(k) for 0 <= k < 10 as fractions (numerator, denominator) (Table 8.40).
pub(super) static F_CENTER_20: [(i32, i32); 10] =
    [(-3, 8), (-1, 8), (1, 8), (3, 8), (5, 8), (7, 8), (5, 4), (7, 4), (9, 4), (11, 4)];

/// f_center_34(k) for 0 <= k < 32 as fractions (numerator, denominator) (Table 8.41).
#[rustfmt::skip]
pub(super) static F_CENTER_34: [(i32, i32); 32] = [
    (1, 12), (3, 12), (5, 12), (7, 12), (9, 12), (11, 12), (13, 12), (15, 12), (17, 12),
    (-5, 12), (-3, 12), (-1, 12),
    (17, 8), (19, 8), (5, 8), (7, 8), (9, 8), (11, 8), (13, 8), (15, 8),
    (9, 4), (11, 4), (13, 4), (7, 4),
    (17, 4), (11, 4), (13, 4), (15, 4),
    (17, 4), (19, 4), (21, 4), (15, 4),
];

/// The fractional delay length vector f_center(k) for 0 <= k < NR_ALLPASS_BANDS (Table 8.40,
/// Table 8.41, and the formulas of 8.6.4.5.2).
pub(super) fn f_center(config: StereoBands, k: usize) -> f64 {
    let (table, offset): (&[(i32, i32)], f64) = match config {
        StereoBands::Bands20 => (&F_CENTER_20, 7.0),
        StereoBands::Bands34 => (&F_CENTER_34, 27.0),
    };
    match table.get(k) {
        Some(&(num, den)) => f64::from(num) / f64::from(den),
        None => k as f64 + 0.5 - offset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_param_index_tables() {
        // Table 8.48: the QMF only part maps QMF channel ranges to parameters.
        let ranges_20: [(usize, usize, u8); 6] =
            [(16, 17, 14), (18, 20, 15), (21, 24, 16), (25, 29, 17), (30, 41, 18), (42, 70, 19)];
        for (lo, hi, b) in ranges_20 {
            for k in lo..=hi {
                assert_eq!(PARAM_INDEX_20[k], b, "k = {k}");
            }
        }
        for k in 10..16 {
            assert_eq!(usize::from(PARAM_INDEX_20[k]), k - 2);
        }
        assert!(PARAM_INDEX_20.iter().all(|&b| b < 20));

        // Table 8.49: the QMF only part.
        let ranges_34: [(usize, usize, u8); 14] = [
            (32, 32, 16),
            (37, 37, 21),
            (38, 39, 22),
            (40, 41, 23),
            (42, 43, 24),
            (44, 45, 25),
            (46, 47, 26),
            (48, 50, 27),
            (51, 53, 28),
            (54, 56, 29),
            (57, 59, 30),
            (60, 63, 31),
            (64, 67, 32),
            (68, 90, 33),
        ];
        for (lo, hi, b) in ranges_34 {
            for k in lo..=hi {
                assert_eq!(PARAM_INDEX_34[k], b, "k = {k}");
            }
        }
        assert!(PARAM_INDEX_34.iter().all(|&b| b < 34));

        // Every parameter band has at least one hybrid band.
        for b in 0..20u8 {
            assert!(PARAM_INDEX_20.contains(&b));
        }
        for b in 0..34u8 {
            assert!(PARAM_INDEX_34.contains(&b));
        }
    }

    #[test]
    fn verify_unsplit_runs() {
        for config in [StereoBands::Bands20, StereoBands::Bands34] {
            let runs = config.unsplit_runs();
            assert_eq!(runs.first().map(|r| r.0), Some(config.num_sub_qmf_bands()));
            assert_eq!(runs.last().map(|r| r.1), Some(config.nr_bands()));
            for (i, &(start, end, b)) in runs.iter().enumerate() {
                assert!(start < end);
                assert!(
                    config.param_index_table()[start..end].iter().all(|&x| usize::from(x) == b)
                );
                if let Some(next) = runs.get(i + 1) {
                    assert_eq!(next.0, end);
                    assert_ne!(next.2, b);
                }
            }
        }
        assert_eq!(StereoBands::Bands20.unsplit_runs().len(), 12);
        assert_eq!(StereoBands::Bands34.unsplit_runs().len(), 18);
    }

    #[test]
    fn verify_qmf_band_mapping() {
        for config in [StereoBands::Bands20, StereoBands::Bands34] {
            let nr_bands = config.nr_bands();
            let mut count = [0usize; 64];
            for k in 0..nr_bands {
                count[config.qmf_band(k)] += 1;
            }
            // Every QMF band is covered, the split bands by Q^p sub subbands (Table 8.36).
            let expected: &[usize] = match config {
                StereoBands::Bands20 => &[6, 2, 2],
                StereoBands::Bands34 => &[12, 8, 4, 4, 4],
            };
            for (p, &c) in count.iter().enumerate() {
                let e = expected.get(p).copied().unwrap_or(1);
                assert_eq!(c, e, "p = {p}");
            }
            assert_eq!(config.qmf_band(nr_bands - 1), 63);
        }
    }

    #[test]
    fn verify_map_10_to_34_composition() {
        // The 10 column of Table 8.45 is the composition of 10 -> 20 (duplication) and 20 -> 34.
        let table_10_col: [usize; 34] = [
            0, 0, 0, 1, 1, 1, 2, 2, 2, 2, 3, 3, 4, 4, 4, 4, 5, 5, 6, 6, 7, 7, 7, 7, 8, 8, 8, 8, 9,
            9, 9, 9, 9, 9,
        ];
        for (i, &(a, b)) in MAP_20_TO_34.iter().enumerate() {
            assert_eq!(usize::from(a) / 2, table_10_col[i]);
            assert_eq!(usize::from(b) / 2, table_10_col[i]);
        }
    }

    #[test]
    fn verify_map_34_to_20() {
        // Every 34 grid index is used, and the weights sum to 2, 3, or 4 (or 1 for copies).
        let mut used = [false; 34];
        for row in MAP_34_TO_20.iter() {
            let sum: u32 = row.iter().map(|&(_, w)| u32::from(w)).sum();
            assert!(matches!(sum, 1..=4));
            for &(i, w) in row.iter() {
                if w > 0 {
                    used[usize::from(i)] = true;
                }
            }
        }
        assert!(used.iter().all(|&u| u));
    }

    #[test]
    fn verify_f_center() {
        assert_eq!(f_center(StereoBands::Bands20, 0), -0.375);
        assert_eq!(f_center(StereoBands::Bands20, 10), 3.5);
        assert_eq!(f_center(StereoBands::Bands20, 29), 22.5);
        assert_eq!(f_center(StereoBands::Bands34, 11), -1.0 / 12.0);
        assert_eq!(f_center(StereoBands::Bands34, 32), 5.5);
        // The sub subbands of the unsplit QMF subbands are centred in their QMF subband.
        for config in [StereoBands::Bands20, StereoBands::Bands34] {
            for k in config.num_sub_qmf_bands()..config.nr_allpass_bands() {
                assert_eq!(f_center(config, k), config.qmf_band(k) as f64 + 0.5);
            }
        }
    }

    #[test]
    fn verify_grids() {
        // The quantization grids are antisymmetric around index 0.
        for i in 0..IID_GRID_DEFAULT.len() {
            assert_eq!(IID_GRID_DEFAULT[i], -IID_GRID_DEFAULT[IID_GRID_DEFAULT.len() - 1 - i]);
        }
        for i in 0..IID_GRID_FINE.len() {
            assert_eq!(IID_GRID_FINE[i], -IID_GRID_FINE[IID_GRID_FINE.len() - 1 - i]);
        }
        assert_eq!(IID_GRID_DEFAULT.len() as i32, 2 * IID_MAX_INDEX_DEFAULT + 1);
        assert_eq!(IID_GRID_FINE.len() as i32, 2 * IID_MAX_INDEX_FINE + 1);
        assert_eq!(ICC_GRID.len() as i32, ICC_MAX_INDEX + 1);
    }
}
