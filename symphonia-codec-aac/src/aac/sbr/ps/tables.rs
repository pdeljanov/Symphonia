// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Parametric Stereo lookup tables and constants.
//!
//! Contains pre-computed and runtime-initialized tables for PS decoding as
//! defined in ISO/IEC 14496-3 Subpart 4, Section 8.6.4:
//! IID/ICC dequantization, stereo mixing matrices, hybrid filter prototypes,
//! decorrelation filter coefficients, and Huffman codebook data.

use std::f32::consts::{FRAC_1_SQRT_2, FRAC_PI_2, PI, SQRT_2};

use lazy_static::lazy_static;

/// Maximum number of PS envelopes per frame.
pub const PS_MAX_NUM_ENV: usize = 5;
/// Maximum number of IID/ICC parameter bands (34-band mode).
pub const PS_MAX_NR_IIDICC: usize = 34;
/// Maximum number of sub-subbands (hybrid + QMF) in 34-band mode.
pub const PS_MAX_SSB: usize = 91;
/// Maximum number of all-pass filter bands (34-band mode).
pub const PS_MAX_AP_BANDS: usize = 50;
/// Number of QMF time slots per PS frame (= SBR time slots).
pub const PS_QMF_TIME_SLOTS: usize = 32;
/// Maximum decorrelation delay in QMF slots.
pub const PS_MAX_DELAY: usize = 14;
/// Number of cascaded all-pass filter links (ISO/IEC 14496-3:2009, 8.6.4.6.2).
pub const PS_AP_LINKS: usize = 3;
/// Maximum all-pass delay.
pub const PS_MAX_AP_DELAY: usize = 5;

/// Number of parameter bands for each mode: [20-band, 34-band].
pub const NR_PAR_BANDS: [usize; 2] = [20, 34];
/// Number of IPD/OPD parameter bands: [20-band, 34-band].
pub const NR_IPDOPD_BANDS: [usize; 2] = [11, 17];
/// Number of sub-subbands (hybrid + QMF): [20-band, 34-band].
pub const NR_BANDS: [usize; 2] = [71, 91];
/// Start frequency band for the all-pass filter decay slope.
pub const DECAY_CUTOFF: [usize; 2] = [10, 32];
/// Number of all-pass filter bands.
pub const NR_ALLPASS_BANDS: [usize; 2] = [30, 50];
/// First stereo band using the short (1-sample) delay.
pub const SHORT_DELAY_BAND: [usize; 2] = [42, 62];

/// All-pass filter decay slope per sample (ISO/IEC 14496-3:2009, 8.6.4.6.2).
pub const DECAY_SLOPE: f32 = 0.05;

/// Number of IID/ICC parameter bands for each iid_mode / icc_mode (0..5).
/// Derived from ISO/IEC 14496-3:2009, Table 8.48.
pub const NR_IIDICC_PAR_TAB: [usize; 6] = [10, 20, 34, 10, 20, 34];

/// Number of IPD/OPD parameter bands for each iid_mode (0..5).
/// Derived from ISO/IEC 14496-3:2009, Table 8.48.
pub const NR_IIDOPD_PAR_TAB: [usize; 6] = [5, 11, 17, 5, 11, 17];

/// Number of envelopes per frame: [frame_class][num_env_idx].
/// Derived from ISO/IEC 14496-3:2009, Table 8.49.
pub const NUM_ENV_TAB: [[usize; 4]; 2] = [[0, 1, 2, 4], [1, 2, 3, 4]];

/// log2 lookup for border position computation (fixed frame class).
pub const FF_LOG2_TAB: [u32; 5] = [0, 0, 1, 2, 2];

/// IID parameter dequantization table (46 entries: 15 for coarse + 31 for fine).
/// Index 0..14 = coarse (iid_quant=0), 15..45 = fine (iid_quant=1).
/// Values are 10^(IID/20), derived from ISO/IEC 14496-3:2009, Table 8.50.
#[rustfmt::skip]
pub const IID_PAR_DEQUANT: [f32; 46] = [
    // Coarse quantization (15 levels)
    0.05623413251903, 0.12589254117942, 0.19952623149689, 0.31622776601684,
    0.44668359215096, 0.63095734448019, 0.79432823472428, 1.0,
    1.25892541179417, 1.58489319246111, 2.23872113856834, 3.16227766016838,
    5.01187233627272, 7.94328234724282, 17.7827941003892,
    // Fine quantization (31 levels)
    0.00316227766017, 0.00562341325190, 0.01, 0.01778279410039,
    0.03162277660168, 0.05623413251903, 0.07943282347243, 0.11220184543020,
    0.15848931924611, 0.22387211385683, 0.31622776601684, 0.39810717055350,
    0.50118723362727, 0.63095734448019, 0.79432823472428, 1.0,
    1.25892541179417, 1.58489319246111, 1.99526231496888, 2.51188643150958,
    3.16227766016838, 4.46683592150963, 6.30957344480193, 8.91250938133745,
    12.5892541179417, 17.7827941003892, 31.6227766016838, 56.2341325190349,
    100.0, 177.827941003892, 316.227766016837,
];

/// ICC inverse quantization table (8 levels).
/// Derived from ISO/IEC 14496-3:2009, Table 8.51.
pub const ICC_INVQ: [f32; 8] = [1.0, 0.937, 0.84118, 0.60092, 0.36764, 0.0, -0.589, -1.0];

/// Pre-computed acos(ICC_INVQ[i]) for stereo mixing matrix computation.
pub const ACOS_ICC_INVQ: [f32; 8] =
    [0.0, 0.35685527, 0.57133466, 0.92614472, 1.1943263, FRAC_PI_2, 2.2006171, PI];

/// Frequency center values for 20-band fractional delay (ISO/IEC 14496-3:2009, 8.6.4.6.2).
pub const F_CENTER_20: [i8; 10] = [-3, -1, 1, 3, 5, 7, 10, 14, 18, 22];

/// Frequency center values for 34-band fractional delay (ISO/IEC 14496-3:2009, 8.6.4.6.2).
#[rustfmt::skip]
pub const F_CENTER_34: [i8; 32] = [
     2,  6, 10, 14, 18, 22, 26, 30,
    34,-10, -6, -2, 51, 57, 15, 21,
    27, 33, 39, 45, 54, 66, 78, 42,
   102, 66, 78, 90,102,114,126, 90,
];

/// Fractional delay parameters for each all-pass link (ISO/IEC 14496-3:2009, 8.6.4.6.2).
pub const FRACTIONAL_DELAY_LINKS: [f32; 3] = [0.43, 0.75, 0.347];
/// Fractional delay gain for phi_fract (ISO/IEC 14496-3:2009, 8.6.4.6.2).
pub const FRACTIONAL_DELAY_GAIN: f32 = 0.39;

/// Real symmetric filter for 2-band hybrid split (ISO/IEC 14496-3:2009, 8.6.4.2, Table 8.46).
pub const G1_Q2: [f32; 7] =
    [0.0, 0.01899487526049, 0.0, -0.07293139167538, 0.0, 0.30596630545168, 0.5];

/// Prototype filter for 8-band, QMF band 0, 20-band mode (ISO/IEC 14496-3:2009, Table 8.46).
pub const G0_Q8: [f32; 7] = [
    0.00746082949812,
    0.02270420949825,
    0.04546865930473,
    0.07266113929591,
    0.09885108575264,
    0.11793710567217,
    0.125,
];

/// Prototype filter for 12-band, QMF band 0, 34-band mode (ISO/IEC 14496-3:2009, Table 8.46).
pub const G0_Q12: [f32; 7] = [
    0.04081179924692,
    0.03812810994926,
    0.05144908135699,
    0.06399831151592,
    0.07428313801106,
    0.08100347892914,
    0.08333333333333,
];

/// Prototype filter for 8-band, QMF band 1, 34-band mode (ISO/IEC 14496-3:2009, Table 8.46).
pub const G1_Q8: [f32; 7] = [
    0.01565675600122,
    0.03752716391991,
    0.05417891378782,
    0.08417044116767,
    0.10307344158036,
    0.12222452249753,
    0.125,
];

/// Prototype filter for 4-band, QMF bands 2-4, 34-band mode (ISO/IEC 14496-3:2009, Table 8.46).
pub const G2_Q4: [f32; 7] = [
    -0.05908211155639,
    -0.04871498374946,
    0.0,
    0.07778723915851,
    0.16486303567403,
    0.23279856662996,
    0.25,
];

/// Mapping from sub-subband index k to parameter band i (20-band mode).
/// 71 entries for 71 sub-subbands (ISO/IEC 14496-3:2009, Table 8.52).
#[rustfmt::skip]
pub const K_TO_I_20: [usize; 71] = [
    1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9,
   10,11,12,13,14,14,15,15,15,16,16,16,
   16,17,17,17,17,17,18,18,18,18,18,18,
   18,18,18,18,18,18,19,19,19,19,19,19,
   19,19,19,19,19,19,19,19,19,19,19,19,
   19,19,19,19,19,19,19,19,19,19,19,
];

/// Mapping from sub-subband index k to parameter band i (34-band mode).
/// 91 entries for 91 sub-subbands (ISO/IEC 14496-3:2009, Table 8.52).
#[rustfmt::skip]
pub const K_TO_I_34: [usize; 91] = [
    0, 1, 2, 3, 4, 5, 6, 6, 7, 2, 1, 0,
   10,10, 4, 5, 6, 7, 8, 9,10,11,12, 9,
   14,11,12,13,14,15,16,13,16,17,18,19,
   20,21,22,22,23,23,24,24,25,25,26,26,
   27,27,27,28,28,28,29,29,29,30,30,30,
   31,31,31,31,32,32,32,32,33,33,33,33,
   33,33,33,33,33,33,33,33,33,33,33,33,
   33,33,33,33,33,33,33,
];

/// Fixed allpass filter feedback coefficients for the 3 cascaded links
/// (ISO/IEC 14496-3:2009, 8.6.4.6.2).
pub const AP_COEFF: [f32; 3] = [0.65143905753106, 0.56471812200776, 0.48954165955695];

// (ISO/IEC 14496-3:2009, 8.6.4.5, Tables 8.54–8.57)

/// Sizes of each Huffman table.
pub const HUFF_SIZES: [usize; 10] = [61, 61, 29, 29, 15, 15, 8, 8, 8, 8];

/// Offsets for converting Huffman indices to signed values.
pub const HUFF_OFFSETS: [i8; 10] = [-30, -30, -14, -14, -7, -7, 0, 0, 0, 0];

/// All Huffman table entries concatenated as (symbol, code_length) pairs.
/// Ordered as: [huff_iid_df1(61), huff_iid_dt1(61), huff_iid_df0(29),
/// huff_iid_dt0(29), huff_icc_df(15), huff_icc_dt(15), huff_ipd_df(8),
/// huff_ipd_dt(8), huff_opd_df(8), huff_opd_dt(8)].
/// Codewords are generated canonically from these pairs at init time.
/// Derived from ISO/IEC 14496-3:2009, Tables 8.54, 8.55, 8.56, and 8.57.
#[rustfmt::skip]
pub const AACPS_HUFF_TABS: [(u8, u8); 242] = [
    // huff_iid_df1 (61 entries): symbol, code_length
    (28, 4), (32, 4), (29, 3), (31, 3), (27, 5), (33, 5), (26, 6), (34, 6),
    (25, 7), (35, 7), (24, 8), (36, 8), (37, 9), (40,11), (19,12), (41,12),
    (22,10), (38,10), ( 9,17), (51,17), (11,17), (49,17), (13,16), (47,16),
    (16,14), (18,13), (42,13), (44,14), (12,17), (48,17), ( 4,18), ( 5,18),
    ( 2,18), ( 3,18), (15,15), (21,11), (39,11), (45,15), ( 8,18), (52,18),
    ( 6,18), ( 7,18), (55,18), (56,18), (53,18), (54,18), (17,14), (43,14),
    (59,18), (60,18), (57,18), (58,18), ( 0,18), ( 1,18), (10,18), (50,18),
    (14,16), (46,16), (20,12), (23,10), (30, 1),
    // huff_iid_dt1 (61 entries)
    (31, 2), (26, 7), (34, 7), (27, 6), (33, 6), (35, 8), (24, 9), (36, 9),
    (39,11), (41,12), ( 9,15), (10,15), (48,15), (49,15), (17,13), (23,10),
    (37,10), (43,13), (11,15), (12,15), ( 4,16), (56,16), ( 2,16), ( 3,16),
    (59,16), (60,16), (57,16), (58,16), ( 0,16), ( 1,16), ( 5,16), (55,16),
    ( 6,16), (54,16), (13,15), (15,14), (20,12), (40,12), (22,11), (38,11),
    (45,14), (47,15), ( 7,16), (53,16), (18,13), (42,13), (16,14), (44,14),
    ( 8,16), (52,16), (14,15), (46,15), (50,16), (51,16), (19,13), (21,12),
    (25, 9), (28, 5), (32, 5), (29, 3), (30, 1),
    // huff_iid_df0 (29 entries)
    (14, 1), (15, 3), (13, 3), (16, 4), (12, 4), (17, 5), (11, 5), (10, 6),
    (18, 6), (19, 6), ( 9, 7), (20, 8), ( 8, 9), ( 7,10), (21,11), (22,13),
    ( 6,13), (23,14), (24,14), ( 5,15), (25,15), ( 4,16), ( 3,17), ( 0,17),
    ( 1,17), ( 2,17), (26,17), (27,18), (28,18),
    // huff_iid_dt0 (29 entries)
    (14, 1), (13, 2), (15, 3), (12, 4), (16, 5), (11, 6), (17, 7), (10, 8),
    (18, 9), ( 9,10), (19,11), ( 8,12), (20,13), (21,14), ( 7,15), (22,17),
    ( 6,17), (23,19), ( 0,19), ( 1,19), ( 2,19), ( 3,20), ( 4,20), ( 5,20),
    (24,20), (25,20), (26,20), (27,20), (28,20),
    // huff_icc_df (15 entries)
    ( 7, 1), ( 8, 2), ( 6, 3), ( 9, 4), ( 5, 5), (10, 6), ( 4, 7), (11, 8),
    (12, 9), ( 3,10), (13,11), ( 2,12), (14,13), ( 1,14), ( 0,14),
    // huff_icc_dt (15 entries)
    ( 7, 1), ( 8, 2), ( 6, 3), ( 9, 4), ( 5, 5), (10, 6), ( 4, 7), (11, 8),
    ( 3, 9), (12,10), ( 2,11), (13,12), ( 1,13), ( 0,14), (14,14),
    // huff_ipd_df (8 entries)
    ( 1, 3), ( 4, 4), ( 5, 4), ( 3, 4), ( 6, 4), ( 2, 4), ( 7, 4), ( 0, 1),
    // huff_ipd_dt (8 entries)
    ( 5, 4), ( 4, 5), ( 3, 5), ( 2, 4), ( 6, 4), ( 1, 3), ( 7, 3), ( 0, 1),
    // huff_opd_df (8 entries)
    ( 7, 3), ( 1, 3), ( 3, 4), ( 6, 4), ( 2, 4), ( 5, 5), ( 4, 5), ( 0, 1),
    // huff_opd_dt (8 entries)
    ( 5, 4), ( 2, 4), ( 6, 4), ( 4, 5), ( 3, 5), ( 1, 3), ( 7, 3), ( 0, 1),
];

/// PS tables computed at initialization time.
///
/// Contains stereo mixing matrices (HA/HB), fractional delay coefficients,
/// phase smoothing tables, and hybrid filter coefficients, all derived from
/// the formulas in ISO/IEC 14496-3:2009, 8.6.4.6.
pub struct PsTables {
    /// Stereo mixing matrix type A: [iid_index][icc_index] → [h11, h12, h21, h22].
    /// Computed per ISO/IEC 14496-3:2009, 8.6.4.6.3 (baseline stereo mixing).
    pub ha: [[[f32; 4]; 8]; 46],
    /// Stereo mixing matrix type B: [iid_index][icc_index] → [h11, h12, h21, h22].
    /// Computed per ISO/IEC 14496-3:2009, 8.6.4.6.3 (alternative stereo mixing).
    pub hb: [[[f32; 4]; 8]; 46],
    /// Phase difference smoothing (real part): [pd0*64 + pd1*8 + pd2].
    /// Used for IPD/OPD smoothing (ISO/IEC 14496-3:2009, 8.6.4.6.3).
    pub pd_re_smooth: [f32; 512],
    /// Phase difference smoothing (imaginary part).
    pub pd_im_smooth: [f32; 512],
    /// Fractional delay phase factors: [is34][k] → [cos, sin].
    /// Computed per ISO/IEC 14496-3:2009, 8.6.4.6.2.
    pub phi_fract: [[[f32; 2]; 50]; 2],
    /// All-pass fractional delay coefficients: [is34][k][link] → [cos, sin].
    /// Computed per ISO/IEC 14496-3:2009, 8.6.4.6.2.
    pub q_fract_allpass: [[[[f32; 2]; 3]; 50]; 2],
    /// Hybrid filter: 20-band mode, QMF band 0, 8 subbands.
    pub f20_0_8: [[[f32; 2]; 8]; 8],
    /// Hybrid filter: 34-band mode, QMF band 0, 12 subbands.
    pub f34_0_12: [[[f32; 2]; 8]; 12],
    /// Hybrid filter: 34-band mode, QMF band 1, 8 subbands.
    pub f34_1_8: [[[f32; 2]; 8]; 8],
    /// Hybrid filter: 34-band mode, QMF bands 2-4, 4 subbands.
    pub f34_2_4: [[[f32; 2]; 8]; 4],
}

impl PsTables {
    fn new() -> Self {
        let mut t = Self {
            ha: [[[0.0; 4]; 8]; 46],
            hb: [[[0.0; 4]; 8]; 46],
            pd_re_smooth: [0.0; 512],
            pd_im_smooth: [0.0; 512],
            phi_fract: [[[0.0; 2]; 50]; 2],
            q_fract_allpass: [[[[0.0; 2]; 3]; 50]; 2],
            f20_0_8: [[[0.0; 2]; 8]; 8],
            f34_0_12: [[[0.0; 2]; 8]; 12],
            f34_1_8: [[[0.0; 2]; 8]; 8],
            f34_2_4: [[[0.0; 2]; 8]; 4],
        };
        t.init();
        t
    }

    fn init(&mut self) {
        self.init_pd_smooth();
        self.init_ha_hb();
        self.init_phi_fract();
        self.init_hybrid_filters();
    }

    /// Compute phase difference smoothing tables (ISO/IEC 14496-3:2009, 8.6.4.6.3).
    fn init_pd_smooth(&mut self) {
        let ipdopd_sin: [f32; 8] =
            [0.0, FRAC_1_SQRT_2, 1.0, FRAC_1_SQRT_2, 0.0, -FRAC_1_SQRT_2, -1.0, -FRAC_1_SQRT_2];
        let ipdopd_cos: [f32; 8] =
            [1.0, FRAC_1_SQRT_2, 0.0, -FRAC_1_SQRT_2, -1.0, -FRAC_1_SQRT_2, 0.0, FRAC_1_SQRT_2];

        for pd0 in 0..8 {
            for pd1 in 0..8 {
                for pd2 in 0..8 {
                    let re_smooth =
                        0.25 * ipdopd_cos[pd0] + 0.5 * ipdopd_cos[pd1] + ipdopd_cos[pd2];
                    let im_smooth =
                        0.25 * ipdopd_sin[pd0] + 0.5 * ipdopd_sin[pd1] + ipdopd_sin[pd2];
                    let mag = 1.0 / (re_smooth * re_smooth + im_smooth * im_smooth).sqrt();
                    let idx = pd0 * 64 + pd1 * 8 + pd2;
                    self.pd_re_smooth[idx] = re_smooth * mag;
                    self.pd_im_smooth[idx] = im_smooth * mag;
                }
            }
        }
    }

    /// Compute HA and HB stereo mixing matrices (ISO/IEC 14496-3:2009, 8.6.4.6.3).
    fn init_ha_hb(&mut self) {
        for iid in 0..46 {
            let c = IID_PAR_DEQUANT[iid];
            let c1 = SQRT_2 / (1.0 + c * c).sqrt();
            let c2 = c * c1;

            for icc in 0..8 {
                // HA matrix
                {
                    let alpha = 0.5 * ACOS_ICC_INVQ[icc];
                    let beta = alpha * (c1 - c2) * FRAC_1_SQRT_2;
                    self.ha[iid][icc][0] = c2 * (beta + alpha).cos();
                    self.ha[iid][icc][1] = c1 * (beta - alpha).cos();
                    self.ha[iid][icc][2] = c2 * (beta + alpha).sin();
                    self.ha[iid][icc][3] = c1 * (beta - alpha).sin();
                }

                // HB matrix
                {
                    let rho = ICC_INVQ[icc].max(0.05);
                    let alpha = 0.5 * (2.0 * c * rho).atan2(c * c - 1.0);
                    let mu = c + 1.0 / c;
                    let mu_val = (1.0 + (4.0 * rho * rho - 4.0) / (mu * mu)).sqrt();
                    let gamma = ((1.0 - mu_val) / (1.0 + mu_val)).sqrt().atan();

                    let alpha = if alpha < 0.0 { alpha + FRAC_PI_2 } else { alpha };

                    let (alpha_s, alpha_c) = alpha.sin_cos();
                    let (gamma_s, gamma_c) = gamma.sin_cos();

                    self.hb[iid][icc][0] = SQRT_2 * alpha_c * gamma_c;
                    self.hb[iid][icc][1] = SQRT_2 * alpha_s * gamma_c;
                    self.hb[iid][icc][2] = -SQRT_2 * alpha_s * gamma_s;
                    self.hb[iid][icc][3] = SQRT_2 * alpha_c * gamma_s;
                }
            }
        }
    }

    /// Compute fractional delay phase factors for decorrelation
    /// (ISO/IEC 14496-3:2009, 8.6.4.6.2).
    fn init_phi_fract(&mut self) {
        // 20-band mode
        for k in 0..NR_ALLPASS_BANDS[0] {
            let f_center = if k < F_CENTER_20.len() {
                f64::from(F_CENTER_20[k]) * 0.125
            }
            else {
                k as f64 - 6.5
            };

            for m in 0..PS_AP_LINKS {
                let theta = -PI as f64 * f64::from(FRACTIONAL_DELAY_LINKS[m]) * f_center;
                self.q_fract_allpass[0][k][m][0] = theta.cos() as f32;
                self.q_fract_allpass[0][k][m][1] = theta.sin() as f32;
            }

            let theta = -PI as f64 * f64::from(FRACTIONAL_DELAY_GAIN) * f_center;
            self.phi_fract[0][k][0] = theta.cos() as f32;
            self.phi_fract[0][k][1] = theta.sin() as f32;
        }

        // 34-band mode
        for k in 0..NR_ALLPASS_BANDS[1] {
            let f_center = if k < F_CENTER_34.len() {
                f64::from(F_CENTER_34[k]) / 24.0
            }
            else {
                k as f64 - 26.5
            };

            for m in 0..PS_AP_LINKS {
                let theta = -PI as f64 * f64::from(FRACTIONAL_DELAY_LINKS[m]) * f_center;
                self.q_fract_allpass[1][k][m][0] = theta.cos() as f32;
                self.q_fract_allpass[1][k][m][1] = theta.sin() as f32;
            }

            let theta = -PI as f64 * f64::from(FRACTIONAL_DELAY_GAIN) * f_center;
            self.phi_fract[1][k][0] = theta.cos() as f32;
            self.phi_fract[1][k][1] = theta.sin() as f32;
        }
    }

    /// Generate complex hybrid filter coefficients from a prototype filter
    /// (ISO/IEC 14496-3:2009, 8.6.4.6.1).
    fn make_filters_from_proto(filter: &mut [[[f32; 2]; 8]], proto: &[f32; 7], bands: usize) {
        for q in 0..bands {
            for n in 0..7 {
                let theta = 2.0 * PI * (q as f32 + 0.5) * (n as f32 - 6.0) / (bands as f32);
                filter[q][n][0] = proto[n] * theta.cos();
                filter[q][n][1] = proto[n] * (-theta.sin());
            }
        }
    }

    /// Initialize hybrid filter coefficient tables.
    fn init_hybrid_filters(&mut self) {
        Self::make_filters_from_proto(&mut self.f20_0_8, &G0_Q8, 8);
        Self::make_filters_from_proto(&mut self.f34_0_12, &G0_Q12, 12);
        Self::make_filters_from_proto(&mut self.f34_1_8, &G1_Q8, 8);
        Self::make_filters_from_proto(&mut self.f34_2_4, &G2_Q4, 4);
    }
}

lazy_static! {
    /// Global PS tables, initialized once on first access.
    pub static ref PS_TABLES: PsTables = PsTables::new();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_constants() {
        assert_eq!(PS_MAX_NUM_ENV, 5);
        assert_eq!(PS_MAX_NR_IIDICC, 34);
        assert_eq!(PS_MAX_SSB, 91);
        assert_eq!(PS_QMF_TIME_SLOTS, 32);
        assert_eq!(PS_MAX_DELAY, 14);
        assert_eq!(PS_AP_LINKS, 3);

        assert_eq!(NR_PAR_BANDS, [20, 34]);
        assert_eq!(NR_BANDS, [71, 91]);
        assert_eq!(NR_ALLPASS_BANDS, [30, 50]);
        assert_eq!(SHORT_DELAY_BAND, [42, 62]);
        assert_eq!(DECAY_CUTOFF, [10, 32]);
    }

    #[test]
    fn verify_k_to_i_20_length_and_range() {
        assert_eq!(K_TO_I_20.len(), NR_BANDS[0]);
        for &v in &K_TO_I_20 {
            assert!(v < NR_PAR_BANDS[0], "K_TO_I_20 value {} >= NR_PAR_BANDS[0]", v);
        }
    }

    #[test]
    fn verify_k_to_i_34_length_and_range() {
        assert_eq!(K_TO_I_34.len(), NR_BANDS[1]);
        for &v in &K_TO_I_34 {
            assert!(v < NR_PAR_BANDS[1], "K_TO_I_34 value {} >= NR_PAR_BANDS[1]", v);
        }
    }

    #[test]
    fn verify_iid_par_dequant_positive() {
        for (i, &v) in IID_PAR_DEQUANT.iter().enumerate() {
            assert!(v > 0.0, "IID_PAR_DEQUANT[{}] = {} should be positive", i, v);
        }
    }

    #[test]
    fn verify_iid_par_dequant_symmetry() {
        // The coarse table (first 15) should be monotonically increasing.
        for i in 1..15 {
            assert!(
                IID_PAR_DEQUANT[i] > IID_PAR_DEQUANT[i - 1],
                "Coarse IID not monotonic at {}",
                i
            );
        }
        // The fine table (indices 15..46) should also be monotonically increasing.
        for i in 16..46 {
            assert!(IID_PAR_DEQUANT[i] > IID_PAR_DEQUANT[i - 1], "Fine IID not monotonic at {}", i);
        }
    }

    #[test]
    fn verify_icc_invq_range() {
        assert_eq!(ICC_INVQ.len(), 8);
        assert_eq!(ICC_INVQ[0], 1.0);
        assert_eq!(ICC_INVQ[7], -1.0);
        // Middle values should be decreasing.
        for i in 1..8 {
            assert!(ICC_INVQ[i] <= ICC_INVQ[i - 1], "ICC_INVQ not decreasing at {}", i);
        }
    }

    #[test]
    fn verify_acos_icc_invq() {
        assert_eq!(ACOS_ICC_INVQ.len(), 8);
        // acos(1.0) = 0.0
        assert!((ACOS_ICC_INVQ[0] - 0.0).abs() < 1e-6);
        // acos(-1.0) = PI
        assert!((ACOS_ICC_INVQ[7] - PI).abs() < 1e-6);
        // Should be monotonically increasing.
        for i in 1..8 {
            assert!(
                ACOS_ICC_INVQ[i] > ACOS_ICC_INVQ[i - 1],
                "ACOS_ICC_INVQ not monotonic at {}",
                i
            );
        }
    }

    #[test]
    fn verify_huff_sizes_sum() {
        assert_eq!(HUFF_SIZES.iter().sum::<usize>(), AACPS_HUFF_TABS.len());
    }

    #[test]
    fn verify_huff_tabs_code_lengths() {
        // All code lengths should be in a reasonable range (1..=20).
        for (i, &(_, len)) in AACPS_HUFF_TABS.iter().enumerate() {
            assert!(
                len >= 1 && len <= 20,
                "AACPS_HUFF_TABS[{}] has unreasonable code length {}",
                i,
                len
            );
        }
    }

    #[test]
    fn verify_huff_tabs_symbols_in_range() {
        let mut offset = 0;
        for (tab_idx, &sz) in HUFF_SIZES.iter().enumerate() {
            let entries = &AACPS_HUFF_TABS[offset..offset + sz];
            for &(sym, _) in entries {
                assert!(
                    (sym as usize) < sz,
                    "Table {} symbol {} >= table size {}",
                    tab_idx,
                    sym,
                    sz
                );
            }
            offset += sz;
        }
    }

    #[test]
    fn verify_num_env_tab() {
        // Fixed frame class: 0,1,2,4 envelopes.
        assert_eq!(NUM_ENV_TAB[0], [0, 1, 2, 4]);
        // Variable frame class: 1,2,3,4 envelopes.
        assert_eq!(NUM_ENV_TAB[1], [1, 2, 3, 4]);
    }

    #[test]
    fn verify_ap_coeff() {
        assert_eq!(AP_COEFF.len(), PS_AP_LINKS);
        // All coefficients should be between 0 and 1.
        for &c in &AP_COEFF {
            assert!(c > 0.0 && c < 1.0, "AP coefficient {} out of range", c);
        }
        // Should be decreasing.
        for i in 1..3 {
            assert!(AP_COEFF[i] < AP_COEFF[i - 1]);
        }
    }

    #[test]
    fn verify_ps_tables_init() {
        // Force initialization and verify tables are non-trivial.
        let tables = &*PS_TABLES;

        // HA should be non-zero for non-trivial IID/ICC combinations.
        // Use IID=10 (off-center) with ICC=3 (partially correlated).
        let ha = tables.ha[10][3];
        assert!(ha[0] != 0.0, "HA h11 should be non-zero for IID=10, ICC=3");
        assert!(ha[2] != 0.0, "HA h21 should be non-zero for IID=10, ICC=3");

        // For center IID=7 (c=1.0) with ICC=0 (fully correlated):
        // alpha=0, beta=0 → h11=cos(0)=1, h21=sin(0)=0. This is correct behavior.
        let ha_center = tables.ha[7][0];
        assert!((ha_center[0] - 1.0).abs() < 1e-6, "HA center h11 should be 1.0");
        assert!(ha_center[2].abs() < 1e-6, "HA center h21 should be 0.0 (fully correlated)");

        // Check that HA and HB differ (they use different formulas).
        let ha_sample = tables.ha[10][3];
        let hb_sample = tables.hb[10][3];
        assert!(
            (ha_sample[0] - hb_sample[0]).abs() > 1e-6,
            "HA and HB should differ for same indices"
        );
    }

    #[test]
    fn verify_ha_energy_conservation() {
        let tables = &*PS_TABLES;

        // For ICC=0 (fully correlated), the mixing matrix should roughly preserve energy.
        // h11^2 + h21^2 ≈ 1 and h12^2 + h22^2 ≈ 1 for center IID.
        let ha = tables.ha[7][0]; // center IID, fully correlated
        let energy_l = ha[0] * ha[0] + ha[2] * ha[2];
        let energy_r = ha[1] * ha[1] + ha[3] * ha[3];
        assert!(
            (energy_l - 1.0).abs() < 0.3,
            "L energy {} should be close to 1.0 for center IID",
            energy_l
        );
        assert!(
            (energy_r - 1.0).abs() < 0.3,
            "R energy {} should be close to 1.0 for center IID",
            energy_r
        );
    }

    #[test]
    fn verify_phi_fract_unit_magnitude() {
        let tables = &*PS_TABLES;

        // phi_fract should be unit-magnitude complex values: cos^2 + sin^2 = 1.
        for mode in 0..2 {
            let nr_ap = NR_ALLPASS_BANDS[mode];
            for k in 0..nr_ap {
                let re = tables.phi_fract[mode][k][0] as f64;
                let im = tables.phi_fract[mode][k][1] as f64;
                let mag = re * re + im * im;
                assert!(
                    (mag - 1.0).abs() < 1e-6,
                    "phi_fract[{}][{}] magnitude {} != 1.0",
                    mode,
                    k,
                    mag
                );
            }
        }
    }

    #[test]
    fn verify_q_fract_allpass_unit_magnitude() {
        let tables = &*PS_TABLES;

        for mode in 0..2 {
            let nr_ap = NR_ALLPASS_BANDS[mode];
            for k in 0..nr_ap {
                for m in 0..PS_AP_LINKS {
                    let re = tables.q_fract_allpass[mode][k][m][0] as f64;
                    let im = tables.q_fract_allpass[mode][k][m][1] as f64;
                    let mag = re * re + im * im;
                    assert!(
                        (mag - 1.0).abs() < 1e-6,
                        "q_fract_allpass[{}][{}][{}] magnitude {} != 1.0",
                        mode,
                        k,
                        m,
                        mag
                    );
                }
            }
        }
    }

    #[test]
    fn verify_pd_smooth_unit_magnitude() {
        let tables = &*PS_TABLES;

        // pd_re_smooth^2 + pd_im_smooth^2 should = 1.0 for all indices.
        for idx in 0..512 {
            let re = tables.pd_re_smooth[idx] as f64;
            let im = tables.pd_im_smooth[idx] as f64;
            let mag = re * re + im * im;
            assert!((mag - 1.0).abs() < 1e-5, "pd_smooth[{}] magnitude {} != 1.0", idx, mag);
        }
    }

    #[test]
    fn verify_hybrid_filter_symmetry() {
        let tables = &*PS_TABLES;

        // The 20-band 8-subband filter should have 8 subbands with 8 taps.
        assert_eq!(tables.f20_0_8.len(), 8);
        assert_eq!(tables.f20_0_8[0].len(), 8);

        // The 34-band 12-subband filter should have 12 subbands.
        assert_eq!(tables.f34_0_12.len(), 12);

        // The 34-band 4-subband filter should have 4 subbands.
        assert_eq!(tables.f34_2_4.len(), 4);

        // Verify filters are non-trivial (not all zero).
        let mut any_nonzero = false;
        for q in 0..8 {
            for n in 0..7 {
                if tables.f20_0_8[q][n][0].abs() > 1e-10 || tables.f20_0_8[q][n][1].abs() > 1e-10 {
                    any_nonzero = true;
                }
            }
        }
        assert!(any_nonzero, "f20_0_8 filter should have non-zero coefficients");
    }

    #[test]
    fn verify_g1_q2_symmetry() {
        // G1_Q2 is a real symmetric filter with specific zero pattern.
        assert_eq!(G1_Q2[0], 0.0);
        assert_eq!(G1_Q2[2], 0.0);
        assert_eq!(G1_Q2[4], 0.0);
        assert_eq!(G1_Q2[6], 0.5);
        // Non-zero odd taps
        assert!(G1_Q2[1].abs() > 0.0);
        assert!(G1_Q2[3].abs() > 0.0);
        assert!(G1_Q2[5].abs() > 0.0);
    }
}
