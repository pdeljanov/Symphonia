// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Parametric Stereo (PS) decoder for HE-AAC v2.
//!
//! Implements the PS tool as defined in ISO/IEC 14496-3 Subpart 4.
//! PS reconstructs a stereo signal from a mono SBR output using spatial
//! parameters: IID (intensity), ICC (coherence), and optionally IPD/OPD (phase).
//!
//! Processing pipeline:
//! 1. Hybrid analysis: split low QMF subbands into finer hybrid subbands
//! 2. Decorrelation: generate an anticorrelated side signal
//! 3. Stereo processing: mix mono + side using IID/ICC parameters
//! 4. Hybrid synthesis: merge hybrid subbands back to QMF domain

pub mod bs;
mod tables;

use tables::*;

/// Parsed PS parameter data.
#[derive(Clone)]
pub struct PsCommonContext {
    /// Whether PS processing should be applied this frame.
    pub start: bool,
    /// Enable IID (Inter-channel Intensity Difference).
    pub enable_iid: bool,
    /// IID quantization mode (false = coarse/fine, true = fine).
    pub iid_quant: bool,
    /// Number of IID parameter bands.
    pub nr_iid_par: usize,
    /// Number of IPD/OPD parameter bands.
    pub nr_ipdopd_par: usize,
    /// Enable ICC (Inter-channel Coherence).
    pub enable_icc: bool,
    /// ICC mode index.
    pub icc_mode: usize,
    /// Number of ICC parameter bands.
    pub nr_icc_par: usize,
    /// Enable extension data.
    pub enable_ext: bool,
    /// Frame class (false = fixed, true = variable).
    pub frame_class: bool,
    /// Previous frame's envelope count.
    pub num_env_old: usize,
    /// Number of envelopes in this frame.
    pub num_env: usize,
    /// Enable IPD/OPD processing.
    pub enable_ipdopd: bool,
    /// Envelope border positions (up to PS_MAX_NUM_ENV+1 borders).
    pub border_position: [i32; PS_MAX_NUM_ENV + 1],
    /// IID parameters [envelope][band].
    pub iid_par: [[i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV],
    /// ICC parameters [envelope][band].
    pub icc_par: [[i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV],
    /// IPD parameters [envelope][band].
    pub ipd_par: [[i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV],
    /// OPD parameters [envelope][band].
    pub opd_par: [[i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV],
    /// Whether 34-band mode is active.
    pub is34bands: bool,
    /// Previous frame's band mode.
    pub is34bands_old: bool,
}

impl PsCommonContext {
    pub fn new() -> Self {
        Self {
            start: false,
            enable_iid: false,
            iid_quant: false,
            nr_iid_par: 0,
            nr_ipdopd_par: 0,
            enable_icc: false,
            icc_mode: 0,
            nr_icc_par: 0,
            enable_ext: false,
            frame_class: false,
            num_env_old: 0,
            num_env: 0,
            enable_ipdopd: false,
            border_position: [0; PS_MAX_NUM_ENV + 1],
            iid_par: [[0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV],
            icc_par: [[0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV],
            ipd_par: [[0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV],
            opd_par: [[0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV],
            is34bands: false,
            is34bands_old: false,
        }
    }
}

/// Full PS processing state including delay lines and history.
pub struct PsContext {
    /// Parsed parameters.
    pub common: PsCommonContext,
    /// Hybrid analysis input buffer history: [qmf_band][sample][re/im].
    in_buf: [[[f32; 2]; 44]; 5],
    /// Decorrelation delay lines: [subband][slot+delay][re/im].
    delay: Box<[[[f32; 2]; PS_QMF_TIME_SLOTS + PS_MAX_DELAY]; PS_MAX_SSB]>,
    /// All-pass decorrelation delay: [band][link][slot+delay][re/im].
    ap_delay:
        Box<[[[[f32; 2]; PS_QMF_TIME_SLOTS + PS_MAX_AP_DELAY]; PS_AP_LINKS]; PS_MAX_AP_BANDS]>,
    /// Peak decay energy per parameter band.
    peak_decay_nrg: [f32; 34],
    /// Smoothed power per parameter band.
    power_smooth: [f32; 34],
    /// Smoothed peak-to-power difference.
    peak_decay_diff_smooth: [f32; 34],
    /// Stereo mixing matrices: [re/im][envelope+1][band].
    h11: [[[f32; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV + 1]; 2],
    h12: [[[f32; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV + 1]; 2],
    h21: [[[f32; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV + 1]; 2],
    h22: [[[f32; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV + 1]; 2],
    /// Hybrid domain left buffer: [subband][slot][re/im].
    l_buf: Box<[[[f32; 2]; 32]; PS_MAX_SSB]>,
    /// Hybrid domain right buffer: [subband][slot][re/im].
    r_buf: Box<[[[f32; 2]; 32]; PS_MAX_SSB]>,
    /// OPD history for phase smoothing.
    opd_hist: [i8; PS_MAX_NR_IIDICC],
    /// IPD history for phase smoothing.
    ipd_hist: [i8; PS_MAX_NR_IIDICC],
}

impl PsContext {
    pub fn new() -> Self {
        Self {
            common: PsCommonContext::new(),
            in_buf: [[[0.0; 2]; 44]; 5],
            delay: Box::new([[[0.0; 2]; PS_QMF_TIME_SLOTS + PS_MAX_DELAY]; PS_MAX_SSB]),
            ap_delay: Box::new(
                [[[[0.0; 2]; PS_QMF_TIME_SLOTS + PS_MAX_AP_DELAY]; PS_AP_LINKS]; PS_MAX_AP_BANDS],
            ),
            peak_decay_nrg: [0.0; 34],
            power_smooth: [0.0; 34],
            peak_decay_diff_smooth: [0.0; 34],
            h11: [[[0.0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV + 1]; 2],
            h12: [[[0.0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV + 1]; 2],
            h21: [[[0.0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV + 1]; 2],
            h22: [[[0.0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV + 1]; 2],
            l_buf: Box::new([[[0.0; 2]; 32]; PS_MAX_SSB]),
            r_buf: Box::new([[[0.0; 2]; 32]; PS_MAX_SSB]),
            opd_hist: [0; PS_MAX_NR_IIDICC],
            ipd_hist: [0; PS_MAX_NR_IIDICC],
        }
    }
}

/// Split one QMF subband into 2 sub-subbands with a real symmetric filter.
fn hybrid2_re(
    input: &[[f32; 2]], // 13 input samples [re, im]
    out: &mut [[[f32; 2]; 32]; PS_MAX_SSB],
    out_idx0: usize,
    out_idx1: usize,
    filter: &[f32; 7],
    len: usize,
) {
    for i in 0..len {
        let re_in = filter[6] * input[i + 6][0];
        let im_in = filter[6] * input[i + 6][1];
        let mut re_op = 0.0f32;
        let mut im_op = 0.0f32;
        for j in (0..6).step_by(2) {
            re_op += filter[j + 1] * (input[i + j + 1][0] + input[i + 12 - j - 1][0]);
            im_op += filter[j + 1] * (input[i + j + 1][1] + input[i + 12 - j - 1][1]);
        }
        out[out_idx0][i][0] = re_in + re_op;
        out[out_idx0][i][1] = im_in + im_op;
        out[out_idx1][i][0] = re_in - re_op;
        out[out_idx1][i][1] = im_in - im_op;
    }
}

/// Split one QMF subband into N sub-subbands with a complex filter (hybrid analysis).
fn hybrid_analysis_cx(
    input: &[[f32; 2]], // 13 input samples [re, im]
    out: &mut [[[f32; 2]; 32]; PS_MAX_SSB],
    out_start: usize,
    filter: &[[[f32; 2]; 8]], // [subband][tap][re/im]
    n_bands: usize,
    stride: usize,
    len: usize,
) {
    for i in 0..len {
        // Precompute symmetric sums/differences.
        let mut inre0 = [0.0f32; 6];
        let mut inre1 = [0.0f32; 6];
        let mut inim0 = [0.0f32; 6];
        let mut inim1 = [0.0f32; 6];
        for j in 0..6 {
            inre0[j] = input[i + j][0] + input[i + 12 - j][0];
            inre1[j] = input[i + j][1] - input[i + 12 - j][1];
            inim0[j] = input[i + j][1] + input[i + 12 - j][1];
            inim1[j] = input[i + j][0] - input[i + 12 - j][0];
        }

        for q in 0..n_bands {
            let mut sum_re = filter[q][6][0] * input[i + 6][0];
            let mut sum_im = filter[q][6][0] * input[i + 6][1];

            for j in 0..6 {
                sum_re += filter[q][j][0] * inre0[j] - filter[q][j][1] * inre1[j];
                sum_im += filter[q][j][0] * inim0[j] + filter[q][j][1] * inim1[j];
            }

            out[out_start + q][i * stride][0] = sum_re;
            out[out_start + q][i * stride][1] = sum_im;
        }
    }
}

/// Hybrid analysis: convert lowest 5 QMF subbands into hybrid representation.
fn hybrid_analysis(
    out: &mut [[[f32; 2]; 32]; PS_MAX_SSB],
    in_buf: &mut [[[f32; 2]; 44]; 5],
    l: &[[[f32; 64]; 38]; 2],
    is34: bool,
    len: usize,
) {
    let tables = &*PS_TABLES;

    // Fill input buffers: copy QMF data into in_buf with 6-sample history.
    for i in 0..5 {
        for j in 0..38usize.min(len + 6) {
            if j >= 6 && (j - 6) < len {
                in_buf[i][j][0] = l[0][j - 6][i];
                in_buf[i][j][1] = l[1][j - 6][i];
            }
        }
    }

    if is34 {
        // 34-band mode: 12+8+4+4+4 = 32 hybrid subbands
        hybrid_analysis_cx(&in_buf[0], out, 0, &tables.f34_0_12, 12, 1, len);
        hybrid_analysis_cx(&in_buf[1], out, 12, &tables.f34_1_8, 8, 1, len);
        hybrid_analysis_cx(&in_buf[2], out, 20, &tables.f34_2_4, 4, 1, len);
        hybrid_analysis_cx(&in_buf[3], out, 24, &tables.f34_2_4, 4, 1, len);
        hybrid_analysis_cx(&in_buf[4], out, 28, &tables.f34_2_4, 4, 1, len);

        // Remaining QMF subbands pass through directly.
        for i in 5..64 {
            for j in 0..len {
                out[i + 27][j][0] = l[0][j][i];
                out[i + 27][j][1] = l[1][j][i];
            }
        }
    }
    else {
        // 20-band mode: 6+2+2 = 10 hybrid subbands from QMF bands 0-2
        // QMF band 0: 6-band complex hybrid
        hybrid6_cx(out, &in_buf[0], &tables.f20_0_8, len);
        // QMF band 1: 2-band real hybrid (reversed order)
        hybrid2_re(&in_buf[1], out, 7, 6, &G1_Q2, len);
        // QMF band 2: 2-band real hybrid
        hybrid2_re(&in_buf[2], out, 8, 9, &G1_Q2, len);

        // Remaining QMF subbands pass through.
        for i in 3..64 {
            for j in 0..len {
                out[i + 7][j][0] = l[0][j][i];
                out[i + 7][j][1] = l[1][j][i];
            }
        }
    }

    // Update in_buf history: shift last 6 samples to the beginning.
    for i in 0..5 {
        for j in 0..6 {
            in_buf[i][j] = in_buf[i][len + j];
        }
    }
}

/// 6-band complex hybrid filter for QMF band 0 (20-band mode).
fn hybrid6_cx(
    out: &mut [[[f32; 2]; 32]; PS_MAX_SSB],
    input: &[[f32; 2]],
    filter: &[[[f32; 2]; 8]; 8],
    len: usize,
) {
    for i in 0..len {
        let mut temp = [[0.0f32; 2]; 8];

        // Apply 8-point analysis filter
        let mut inre0 = [0.0f32; 6];
        let mut inre1 = [0.0f32; 6];
        let mut inim0 = [0.0f32; 6];
        let mut inim1 = [0.0f32; 6];
        for j in 0..6 {
            inre0[j] = input[i + j][0] + input[i + 12 - j][0];
            inre1[j] = input[i + j][1] - input[i + 12 - j][1];
            inim0[j] = input[i + j][1] + input[i + 12 - j][1];
            inim1[j] = input[i + j][0] - input[i + 12 - j][0];
        }

        for q in 0..8 {
            let mut sum_re = filter[q][6][0] * input[i + 6][0];
            let mut sum_im = filter[q][6][0] * input[i + 6][1];
            for j in 0..6 {
                sum_re += filter[q][j][0] * inre0[j] - filter[q][j][1] * inre1[j];
                sum_im += filter[q][j][0] * inim0[j] + filter[q][j][1] * inim1[j];
            }
            temp[q][0] = sum_re;
            temp[q][1] = sum_im;
        }

        // Map 8 analysis outputs to 6 hybrid subbands.
        out[0][i][0] = temp[6][0];
        out[0][i][1] = temp[6][1];
        out[1][i][0] = temp[7][0];
        out[1][i][1] = temp[7][1];
        out[2][i][0] = temp[0][0];
        out[2][i][1] = temp[0][1];
        out[3][i][0] = temp[1][0];
        out[3][i][1] = temp[1][1];
        out[4][i][0] = temp[2][0] + temp[5][0];
        out[4][i][1] = temp[2][1] + temp[5][1];
        out[5][i][0] = temp[3][0] + temp[4][0];
        out[5][i][1] = temp[3][1] + temp[4][1];
    }
}

/// Hybrid synthesis: merge hybrid subbands back into QMF domain.
fn hybrid_synthesis(
    out: &mut [[[f32; 64]; 38]; 2],
    buf: &[[[f32; 2]; 32]; PS_MAX_SSB],
    is34: bool,
    len: usize,
) {
    if is34 {
        for n in 0..len {
            // QMF band 0: sum of 12 hybrid subbands.
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for i in 0..12 {
                re += buf[i][n][0];
                im += buf[i][n][1];
            }
            out[0][n][0] = re;
            out[1][n][0] = im;

            // QMF band 1: sum of 8 hybrid subbands.
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for i in 0..8 {
                re += buf[12 + i][n][0];
                im += buf[12 + i][n][1];
            }
            out[0][n][1] = re;
            out[1][n][1] = im;

            // QMF bands 2-4: sum of 4 hybrid subbands each.
            for band in 0..3 {
                let (mut re, mut im) = (0.0f32, 0.0f32);
                for i in 0..4 {
                    re += buf[20 + band * 4 + i][n][0];
                    im += buf[20 + band * 4 + i][n][1];
                }
                out[0][n][2 + band] = re;
                out[1][n][2 + band] = im;
            }

            // QMF bands 5-63: direct copy from hybrid buffer.
            for i in 5..64 {
                out[0][n][i] = buf[i + 27][n][0];
                out[1][n][i] = buf[i + 27][n][1];
            }
        }
    }
    else {
        for n in 0..len {
            // QMF band 0: sum of 6 hybrid subbands.
            out[0][n][0] = buf[0][n][0]
                + buf[1][n][0]
                + buf[2][n][0]
                + buf[3][n][0]
                + buf[4][n][0]
                + buf[5][n][0];
            out[1][n][0] = buf[0][n][1]
                + buf[1][n][1]
                + buf[2][n][1]
                + buf[3][n][1]
                + buf[4][n][1]
                + buf[5][n][1];

            // QMF band 1: sum of 2 hybrid subbands.
            out[0][n][1] = buf[6][n][0] + buf[7][n][0];
            out[1][n][1] = buf[6][n][1] + buf[7][n][1];

            // QMF band 2: sum of 2 hybrid subbands.
            out[0][n][2] = buf[8][n][0] + buf[9][n][0];
            out[1][n][2] = buf[8][n][1] + buf[9][n][1];

            // QMF bands 3-63: direct copy.
            for i in 3..64 {
                out[0][n][i] = buf[i + 7][n][0];
                out[1][n][i] = buf[i + 7][n][1];
            }
        }
    }
}

/// Generate a decorrelated (anticorrelated) signal from the mono input.
///
/// Implements the PS decorrelation process: transient detection, 3-link
/// cascaded all-pass filtering with fractional delay for low bands, and
/// simple delay-based decorrelation for mid/high bands.
fn decorrelation(ps: &mut PsContext, is34: bool, len: usize) {
    let tables = &*PS_TABLES;
    let is34_idx = is34 as usize;
    let k_to_i = if is34 { &K_TO_I_34[..] } else { &K_TO_I_20[..] };
    let nr_bands = NR_BANDS[is34_idx];
    let nr_par = NR_PAR_BANDS[is34_idx];
    let nr_allpass = NR_ALLPASS_BANDS[is34_idx];
    let short_delay = SHORT_DELAY_BAND[is34_idx];
    let decay_cutoff = DECAY_CUTOFF[is34_idx];

    let peak_decay_factor: f32 = 0.76592833836465;
    let a_smooth: f32 = 0.25;
    let transient_impact: f32 = 1.5;

    // Reset state if band mode changed.
    if is34 != ps.common.is34bands_old {
        ps.peak_decay_nrg = [0.0; 34];
        ps.power_smooth = [0.0; 34];
        ps.peak_decay_diff_smooth = [0.0; 34];
        for slot in ps.delay.iter_mut() {
            *slot = [[0.0; 2]; PS_QMF_TIME_SLOTS + PS_MAX_DELAY];
        }
        for band in ps.ap_delay.iter_mut() {
            *band = [[[0.0; 2]; PS_QMF_TIME_SLOTS + PS_MAX_AP_DELAY]; PS_AP_LINKS];
        }
    }

    // Compute power per parameter band.
    let mut power = [[0.0f32; PS_QMF_TIME_SLOTS]; 34];
    for k in 0..nr_bands {
        let i = k_to_i[k];
        for n in 0..len {
            power[i][n] +=
                ps.l_buf[k][n][0] * ps.l_buf[k][n][0] + ps.l_buf[k][n][1] * ps.l_buf[k][n][1];
        }
    }

    // Transient detection: compute per-band gain.
    let mut transient_gain = [[0.0f32; PS_QMF_TIME_SLOTS]; 34];
    for i in 0..nr_par {
        for n in 0..len {
            let decayed_peak = peak_decay_factor * ps.peak_decay_nrg[i];
            ps.peak_decay_nrg[i] = decayed_peak.max(power[i][n]);
            ps.power_smooth[i] += a_smooth * (power[i][n] - ps.power_smooth[i]);
            ps.peak_decay_diff_smooth[i] +=
                a_smooth * (ps.peak_decay_nrg[i] - power[i][n] - ps.peak_decay_diff_smooth[i]);

            let denom = transient_impact * ps.peak_decay_diff_smooth[i];
            transient_gain[i][n] =
                if denom > ps.power_smooth[i] { ps.power_smooth[i] / denom } else { 1.0 };
        }
    }

    // All-pass decorrelation for low-frequency bands.
    for k in 0..nr_allpass {
        let b = k_to_i[k];
        let g_decay_slope = (1.0 - DECAY_SLOPE * (k as f32 - decay_cutoff as f32)).clamp(0.0, 1.0);

        // Shift delay history.
        for j in 0..PS_MAX_DELAY {
            ps.delay[k][j] = ps.delay[k][len + j];
        }
        for j in 0..len {
            ps.delay[k][PS_MAX_DELAY + j] = ps.l_buf[k][j];
        }

        // Shift all-pass delay history.
        for m in 0..PS_AP_LINKS {
            for j in 0..5 {
                ps.ap_delay[k][m][j] = ps.ap_delay[k][m][len + j];
            }
        }

        // Compute ag[m] = a[m] * g_decay_slope.
        let mut ag = [0.0f32; PS_AP_LINKS];
        for m in 0..PS_AP_LINKS {
            ag[m] = AP_COEFF[m] * g_decay_slope;
        }

        // All-pass filter processing.
        let phi = &tables.phi_fract[is34_idx][k];
        for n in 0..len {
            // Apply fractional delay (phi_fract rotation).
            let mut in_re = ps.delay[k][PS_MAX_DELAY + n - 2][0] * phi[0]
                - ps.delay[k][PS_MAX_DELAY + n - 2][1] * phi[1];
            let mut in_im = ps.delay[k][PS_MAX_DELAY + n - 2][0] * phi[1]
                + ps.delay[k][PS_MAX_DELAY + n - 2][1] * phi[0];

            // 3 cascaded all-pass filters.
            for m in 0..PS_AP_LINKS {
                let a_re = ag[m] * in_re;
                let a_im = ag[m] * in_im;
                let link_delay_idx = n + 2 - m; // ap_delay has 5 slots of history
                let ld_re = ps.ap_delay[k][m][link_delay_idx][0];
                let ld_im = ps.ap_delay[k][m][link_delay_idx][1];
                let q_re = tables.q_fract_allpass[is34_idx][k][m][0];
                let q_im = tables.q_fract_allpass[is34_idx][k][m][1];

                let apd_re = in_re;
                let apd_im = in_im;

                // Fractional delay on link delay.
                in_re = ld_re * q_re - ld_im * q_im - a_re;
                in_im = ld_re * q_im + ld_im * q_re - a_im;

                // Store to all-pass delay line.
                ps.ap_delay[k][m][n + 5][0] = apd_re + ag[m] * in_re;
                ps.ap_delay[k][m][n + 5][1] = apd_im + ag[m] * in_im;
            }

            // Apply transient gain.
            ps.r_buf[k][n][0] = transient_gain[b][n] * in_re;
            ps.r_buf[k][n][1] = transient_gain[b][n] * in_im;
        }
    }

    // Simple delay decorrelation for mid-frequency bands (delay 14).
    for k in nr_allpass..short_delay {
        let i = k_to_i[k];
        for j in 0..PS_MAX_DELAY {
            ps.delay[k][j] = ps.delay[k][len + j];
        }
        for j in 0..len {
            ps.delay[k][PS_MAX_DELAY + j] = ps.l_buf[k][j];
        }
        for n in 0..len {
            ps.r_buf[k][n][0] = transient_gain[i][n] * ps.delay[k][PS_MAX_DELAY + n - 14][0];
            ps.r_buf[k][n][1] = transient_gain[i][n] * ps.delay[k][PS_MAX_DELAY + n - 14][1];
        }
    }

    // Short delay decorrelation for high-frequency bands (delay 1).
    for k in short_delay..nr_bands {
        let i = k_to_i[k];
        for j in 0..PS_MAX_DELAY {
            ps.delay[k][j] = ps.delay[k][len + j];
        }
        for j in 0..len {
            ps.delay[k][PS_MAX_DELAY + j] = ps.l_buf[k][j];
        }
        for n in 0..len {
            ps.r_buf[k][n][0] = transient_gain[i][n] * ps.delay[k][PS_MAX_DELAY + n - 1][0];
            ps.r_buf[k][n][1] = transient_gain[i][n] * ps.delay[k][PS_MAX_DELAY + n - 1][1];
        }
    }
}

fn map_idx_10_to_20(
    par_mapped: &mut [i8; PS_MAX_NR_IIDICC],
    par: &[i8; PS_MAX_NR_IIDICC],
    full: bool,
) {
    if full {
        for b in (0..=9).rev() {
            par_mapped[2 * b + 1] = par[b];
            par_mapped[2 * b] = par[b];
        }
    }
    else {
        for b in (0..=4).rev() {
            par_mapped[2 * b + 1] = par[b];
            par_mapped[2 * b] = par[b];
        }
        par_mapped[10] = 0;
    }
}

fn map_idx_34_to_20(
    par_mapped: &mut [i8; PS_MAX_NR_IIDICC],
    par: &[i8; PS_MAX_NR_IIDICC],
    full: bool,
) {
    par_mapped[0] = ((2i16 * par[0] as i16 + par[1] as i16) / 3) as i8;
    par_mapped[1] = ((par[1] as i16 + 2i16 * par[2] as i16) / 3) as i8;
    par_mapped[2] = ((2i16 * par[3] as i16 + par[4] as i16) / 3) as i8;
    par_mapped[3] = ((par[4] as i16 + 2i16 * par[5] as i16) / 3) as i8;
    par_mapped[4] = ((par[6] as i16 + par[7] as i16) / 2) as i8;
    par_mapped[5] = ((par[8] as i16 + par[9] as i16) / 2) as i8;
    par_mapped[6] = par[10];
    par_mapped[7] = par[11];
    par_mapped[8] = ((par[12] as i16 + par[13] as i16) / 2) as i8;
    par_mapped[9] = ((par[14] as i16 + par[15] as i16) / 2) as i8;
    par_mapped[10] = par[16];
    if full {
        par_mapped[11] = par[17];
        par_mapped[12] = par[18];
        par_mapped[13] = par[19];
        par_mapped[14] = ((par[20] as i16 + par[21] as i16) / 2) as i8;
        par_mapped[15] = ((par[22] as i16 + par[23] as i16) / 2) as i8;
        par_mapped[16] = ((par[24] as i16 + par[25] as i16) / 2) as i8;
        par_mapped[17] = ((par[26] as i16 + par[27] as i16) / 2) as i8;
        par_mapped[18] =
            ((par[28] as i16 + par[29] as i16 + par[30] as i16 + par[31] as i16) / 4) as i8;
        par_mapped[19] = ((par[32] as i16 + par[33] as i16) / 2) as i8;
    }
}

fn map_idx_10_to_34(
    par_mapped: &mut [i8; PS_MAX_NR_IIDICC],
    par: &[i8; PS_MAX_NR_IIDICC],
    full: bool,
) {
    if full {
        for i in 28..34 {
            par_mapped[i] = par[9];
        }
        par_mapped[27] = par[8];
        par_mapped[26] = par[8];
        par_mapped[25] = par[8];
        par_mapped[24] = par[8];
        for i in 20..24 {
            par_mapped[i] = par[7];
        }
        par_mapped[19] = par[6];
        par_mapped[18] = par[6];
        par_mapped[17] = par[5];
        par_mapped[16] = par[5];
    }
    else {
        par_mapped[16] = 0;
    }
    for i in 12..16 {
        par_mapped[i] = par[4];
    }
    par_mapped[11] = par[3];
    par_mapped[10] = par[3];
    for i in 6..10 {
        par_mapped[i] = par[2];
    }
    for i in 3..6 {
        par_mapped[i] = par[1];
    }
    par_mapped[2] = par[0];
    par_mapped[1] = par[0];
    par_mapped[0] = par[0];
}

fn map_idx_20_to_34(
    par_mapped: &mut [i8; PS_MAX_NR_IIDICC],
    par: &[i8; PS_MAX_NR_IIDICC],
    full: bool,
) {
    if full {
        par_mapped[33] = par[19];
        par_mapped[32] = par[19];
        for i in 28..32 {
            par_mapped[i] = par[18];
        }
        par_mapped[27] = par[17];
        par_mapped[26] = par[17];
        par_mapped[25] = par[16];
        par_mapped[24] = par[16];
        par_mapped[23] = par[15];
        par_mapped[22] = par[15];
        par_mapped[21] = par[14];
        par_mapped[20] = par[14];
        par_mapped[19] = par[13];
        par_mapped[18] = par[12];
        par_mapped[17] = par[11];
    }
    par_mapped[16] = par[10];
    par_mapped[15] = par[9];
    par_mapped[14] = par[9];
    par_mapped[13] = par[8];
    par_mapped[12] = par[8];
    par_mapped[11] = par[7];
    par_mapped[10] = par[6];
    par_mapped[9] = par[5];
    par_mapped[8] = par[5];
    par_mapped[7] = par[4];
    par_mapped[6] = par[4];
    par_mapped[5] = par[3];
    par_mapped[4] = ((par[2] as i16 + par[3] as i16) / 2) as i8;
    par_mapped[3] = par[2];
    par_mapped[2] = par[1];
    par_mapped[1] = ((par[0] as i16 + par[1] as i16) / 2) as i8;
    par_mapped[0] = par[0];
}

/// Map float values from 20-band to 34-band representation (for H matrices).
fn map_val_20_to_34(par: &mut [f32; PS_MAX_NR_IIDICC]) {
    par[33] = par[19];
    par[32] = par[19];
    par[31] = par[18];
    par[30] = par[18];
    par[29] = par[18];
    par[28] = par[18];
    par[27] = par[17];
    par[26] = par[17];
    par[25] = par[16];
    par[24] = par[16];
    par[23] = par[15];
    par[22] = par[15];
    par[21] = par[14];
    par[20] = par[14];
    par[19] = par[13];
    par[18] = par[12];
    par[17] = par[11];
    par[16] = par[10];
    par[15] = par[9];
    par[14] = par[9];
    par[13] = par[8];
    par[12] = par[8];
    par[11] = par[7];
    par[10] = par[6];
    par[9] = par[5];
    par[8] = par[5];
    par[7] = par[4];
    par[6] = par[4];
    par[5] = par[3];
    par[4] = (par[2] + par[3]) * 0.5;
    par[3] = par[2];
    par[2] = par[1];
    par[1] = (par[0] + par[1]) * 0.5;
}

/// Map float values from 34-band to 20-band representation (for H matrices).
fn map_val_34_to_20(par: &mut [f32; PS_MAX_NR_IIDICC]) {
    par[0] = (2.0 * par[0] + par[1]) * 0.33333333;
    par[1] = (par[1] + 2.0 * par[2]) * 0.33333333;
    par[2] = (2.0 * par[3] + par[4]) * 0.33333333;
    par[3] = (par[4] + 2.0 * par[5]) * 0.33333333;
    par[4] = (par[6] + par[7]) * 0.5;
    par[5] = (par[8] + par[9]) * 0.5;
    par[6] = par[10];
    par[7] = par[11];
    par[8] = (par[12] + par[13]) * 0.5;
    par[9] = (par[14] + par[15]) * 0.5;
    par[10] = par[16];
    par[11] = par[17];
    par[12] = par[18];
    par[13] = par[19];
    par[14] = (par[20] + par[21]) * 0.5;
    par[15] = (par[22] + par[23]) * 0.5;
    par[16] = (par[24] + par[25]) * 0.5;
    par[17] = (par[26] + par[27]) * 0.5;
    par[18] = (par[28] + par[29] + par[30] + par[31]) * 0.25;
    par[19] = (par[32] + par[33]) * 0.5;
}

/// Remap integer parameter arrays (for 34-band target).
fn remap34(
    par: &[[i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV],
    num_par: usize,
    num_env: usize,
    full: bool,
) -> [[i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV] {
    let mut mapped = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
    if num_par == 20 || num_par == 11 {
        for e in 0..num_env {
            map_idx_20_to_34(&mut mapped[e], &par[e], full);
        }
    }
    else if num_par == 10 || num_par == 5 {
        for e in 0..num_env {
            map_idx_10_to_34(&mut mapped[e], &par[e], full);
        }
    }
    else {
        // Already 34 bands, copy as-is.
        mapped[..num_env].copy_from_slice(&par[..num_env]);
    }
    mapped
}

/// Remap integer parameter arrays (for 20-band target).
fn remap20(
    par: &[[i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV],
    num_par: usize,
    num_env: usize,
    full: bool,
) -> [[i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV] {
    let mut mapped = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
    if num_par == 34 || num_par == 17 {
        for e in 0..num_env {
            map_idx_34_to_20(&mut mapped[e], &par[e], full);
        }
    }
    else if num_par == 10 || num_par == 5 {
        for e in 0..num_env {
            map_idx_10_to_20(&mut mapped[e], &par[e], full);
        }
    }
    else {
        mapped[..num_env].copy_from_slice(&par[..num_env]);
    }
    mapped
}

/// Apply stereo mixing using IID/ICC (and optionally IPD/OPD) parameters.
///
/// Remaps parameters to the active band resolution, computes the stereo
/// mixing matrices H11/H12/H21/H22, and applies interpolated mixing:
/// `L[k] = h11*S[k] + h12*D[k]`, `R[k] = h21*S[k] + h22*D[k]`.
fn stereo_processing(ps: &mut PsContext, is34: bool) {
    let tables = &*PS_TABLES;
    let is34_idx = is34 as usize;
    let k_to_i = if is34 { &K_TO_I_34[..] } else { &K_TO_I_20[..] };
    let nr_par = NR_PAR_BANDS[is34_idx];
    let nr_bands = NR_BANDS[is34_idx];

    let ps2 = &ps.common;

    // Use HA (baseline) or HB depending on ICC mode.
    let h_lut = if ps2.icc_mode < 3 { &tables.ha } else { &tables.hb };

    // Copy previous envelope's H values.
    if ps2.num_env_old > 0 {
        for ri in 0..2 {
            ps.h11[ri][0] = ps.h11[ri][ps2.num_env_old];
            ps.h12[ri][0] = ps.h12[ri][ps2.num_env_old];
            ps.h21[ri][0] = ps.h21[ri][ps2.num_env_old];
            ps.h22[ri][0] = ps.h22[ri][ps2.num_env_old];
        }
    }

    // Remap H values if band mode changed.
    if is34 && !ps2.is34bands_old {
        for ri in 0..2 {
            map_val_20_to_34(&mut ps.h11[ri][0]);
            map_val_20_to_34(&mut ps.h12[ri][0]);
            map_val_20_to_34(&mut ps.h21[ri][0]);
            map_val_20_to_34(&mut ps.h22[ri][0]);
        }
        ps.opd_hist = [0; PS_MAX_NR_IIDICC];
        ps.ipd_hist = [0; PS_MAX_NR_IIDICC];
    }
    else if !is34 && ps2.is34bands_old {
        for ri in 0..2 {
            map_val_34_to_20(&mut ps.h11[ri][0]);
            map_val_34_to_20(&mut ps.h12[ri][0]);
            map_val_34_to_20(&mut ps.h21[ri][0]);
            map_val_34_to_20(&mut ps.h22[ri][0]);
        }
        ps.opd_hist = [0; PS_MAX_NR_IIDICC];
        ps.ipd_hist = [0; PS_MAX_NR_IIDICC];
    }

    // Remap parameters.
    let iid_mapped = if is34 {
        remap34(&ps2.iid_par, ps2.nr_iid_par, ps2.num_env, true)
    }
    else {
        remap20(&ps2.iid_par, ps2.nr_iid_par, ps2.num_env, true)
    };
    let icc_mapped = if is34 {
        remap34(&ps2.icc_par, ps2.nr_icc_par, ps2.num_env, true)
    }
    else {
        remap20(&ps2.icc_par, ps2.nr_icc_par, ps2.num_env, true)
    };
    let ipd_mapped = if is34 {
        remap34(&ps2.ipd_par, ps2.nr_ipdopd_par, ps2.num_env, false)
    }
    else {
        remap20(&ps2.ipd_par, ps2.nr_ipdopd_par, ps2.num_env, false)
    };
    let opd_mapped = if is34 {
        remap34(&ps2.opd_par, ps2.nr_ipdopd_par, ps2.num_env, false)
    }
    else {
        remap20(&ps2.opd_par, ps2.nr_ipdopd_par, ps2.num_env, false)
    };

    let num_env = ps.common.num_env;
    let iid_quant = ps.common.iid_quant;
    let enable_ipdopd = ps.common.enable_ipdopd;
    let nr_ipdopd = NR_IPDOPD_BANDS[is34_idx];

    // Compute mixing matrix for each envelope.
    for e in 0..num_env {
        for b in 0..nr_par {
            // Clamp indices to valid ranges. Malicious bitstreams can produce arbitrary
            // i8 values via wrapping_add of delta-coded parameters, which would cause
            // out-of-bounds panics without clamping.
            // h_lut dimensions: [46][8][4]
            let raw_iid = iid_mapped[e][b] as i32 + 7 + 23 * iid_quant as i32;
            let iid_idx = raw_iid.clamp(0, 45) as usize;
            let icc_idx = icc_mapped[e][b].clamp(0, 7) as usize;

            let h11 = h_lut[iid_idx][icc_idx][0];
            let h12 = h_lut[iid_idx][icc_idx][1];
            let h21 = h_lut[iid_idx][icc_idx][2];
            let h22 = h_lut[iid_idx][icc_idx][3];

            if enable_ipdopd && b < nr_ipdopd {
                let opd_idx = (ps.opd_hist[b] as usize * 8 + opd_mapped[e][b] as usize) & 0x1FF;
                let ipd_idx = (ps.ipd_hist[b] as usize * 8 + ipd_mapped[e][b] as usize) & 0x1FF;

                let opd_re = tables.pd_re_smooth[opd_idx];
                let opd_im = tables.pd_im_smooth[opd_idx];
                let ipd_re = tables.pd_re_smooth[ipd_idx];
                let ipd_im = tables.pd_im_smooth[ipd_idx];

                ps.opd_hist[b] = (opd_idx & 0x3F) as i8;
                ps.ipd_hist[b] = (ipd_idx & 0x3F) as i8;

                let ipd_adj_re = opd_re * ipd_re + opd_im * ipd_im;
                let ipd_adj_im = opd_im * ipd_re - opd_re * ipd_im;

                ps.h11[1][e + 1][b] = h11 * opd_im;
                ps.h11[0][e + 1][b] = h11 * opd_re;
                ps.h12[1][e + 1][b] = h12 * ipd_adj_im;
                ps.h12[0][e + 1][b] = h12 * ipd_adj_re;
                ps.h21[1][e + 1][b] = h21 * opd_im;
                ps.h21[0][e + 1][b] = h21 * opd_re;
                ps.h22[1][e + 1][b] = h22 * ipd_adj_im;
                ps.h22[0][e + 1][b] = h22 * ipd_adj_re;
            }
            else {
                ps.h11[0][e + 1][b] = h11;
                ps.h12[0][e + 1][b] = h12;
                ps.h21[0][e + 1][b] = h21;
                ps.h22[0][e + 1][b] = h22;
                ps.h11[1][e + 1][b] = 0.0;
                ps.h12[1][e + 1][b] = 0.0;
                ps.h21[1][e + 1][b] = 0.0;
                ps.h22[1][e + 1][b] = 0.0;
            }
        }

        // Apply stereo mixing with interpolation across each envelope.
        let border = &ps.common.border_position;
        for k in 0..nr_bands {
            let b = k_to_i[k];
            let start = (border[e] + 1) as usize;
            let stop = (border[e + 1] + 1) as usize;
            let width = if stop > start { stop - start } else { 1 };
            let inv_width = 1.0 / width as f32;

            // ISO/IEC 14496-3 stereo mixing: L = H11*S + H21*D, R = H12*S + H22*D
            // h0/h1 are signal/decorr for LEFT, h2/h3 are signal/decorr for RIGHT.
            let mut h0 = ps.h11[0][e][b];
            let mut h1 = ps.h21[0][e][b];
            let mut h2 = ps.h12[0][e][b];
            let mut h3 = ps.h22[0][e][b];

            let hs0 = (ps.h11[0][e + 1][b] - h0) * inv_width;
            let hs1 = (ps.h21[0][e + 1][b] - h1) * inv_width;
            let hs2 = (ps.h12[0][e + 1][b] - h2) * inv_width;
            let hs3 = (ps.h22[0][e + 1][b] - h3) * inv_width;

            if enable_ipdopd {
                // Same H12/H21 swap as real path above.
                let mut h0i = ps.h11[1][e][b];
                let mut h1i = ps.h21[1][e][b];
                let mut h2i = ps.h12[1][e][b];
                let mut h3i = ps.h22[1][e][b];

                let hs0i = (ps.h11[1][e + 1][b] - h0i) * inv_width;
                let hs1i = (ps.h21[1][e + 1][b] - h1i) * inv_width;
                let hs2i = (ps.h12[1][e + 1][b] - h2i) * inv_width;
                let hs3i = (ps.h22[1][e + 1][b] - h3i) * inv_width;

                // Negate imaginary part for certain bands (per spec).
                let negate = if is34 { k >= 9 && k <= 13 } else { k <= 1 };

                for n in start..stop {
                    if n >= 32 {
                        break;
                    }
                    let l_re = ps.l_buf[k][n][0];
                    let l_im = ps.l_buf[k][n][1];
                    let r_re = ps.r_buf[k][n][0];
                    let r_im = ps.r_buf[k][n][1];

                    h0 += hs0;
                    h1 += hs1;
                    h2 += hs2;
                    h3 += hs3;
                    h0i += hs0i;
                    h1i += hs1i;
                    h2i += hs2i;
                    h3i += hs3i;

                    let (h0i_eff, h1i_eff, h2i_eff, h3i_eff) =
                        if negate { (-h0i, -h1i, -h2i, -h3i) } else { (h0i, h1i, h2i, h3i) };

                    ps.l_buf[k][n][0] = h0 * l_re + h1 * r_re - h0i_eff * l_im - h1i_eff * r_im;
                    ps.l_buf[k][n][1] = h0 * l_im + h1 * r_im + h0i_eff * l_re + h1i_eff * r_re;
                    ps.r_buf[k][n][0] = h2 * l_re + h3 * r_re - h2i_eff * l_im - h3i_eff * r_im;
                    ps.r_buf[k][n][1] = h2 * l_im + h3 * r_im + h2i_eff * l_re + h3i_eff * r_re;
                }
            }
            else {
                for n in start..stop {
                    if n >= 32 {
                        break;
                    }
                    let l_re = ps.l_buf[k][n][0];
                    let l_im = ps.l_buf[k][n][1];
                    let r_re = ps.r_buf[k][n][0];
                    let r_im = ps.r_buf[k][n][1];

                    h0 += hs0;
                    h1 += hs1;
                    h2 += hs2;
                    h3 += hs3;

                    ps.l_buf[k][n][0] = h0 * l_re + h1 * r_re;
                    ps.l_buf[k][n][1] = h0 * l_im + h1 * r_im;
                    ps.r_buf[k][n][0] = h2 * l_re + h3 * r_re;
                    ps.r_buf[k][n][1] = h2 * l_im + h3 * r_im;
                }
            }
        }
    }
}

/// Apply Parametric Stereo processing.
///
/// Implements the full PS decoding pipeline per ISO/IEC 14496-3:2009, 8.6.4.6:
/// hybrid analysis → decorrelation → stereo processing → hybrid synthesis.
///
/// Takes the mono QMF representation in `l` and produces stereo output in `l` (left)
/// and `r` (right). The QMF format is `[re_or_im][time_slot][qmf_band]` where
/// index 0 = real, index 1 = imaginary.
///
/// `top` is the highest QMF band in use (k_x + m from SBR).
pub fn ps_apply(
    ps: &mut PsContext,
    l: &mut [[[f32; 64]; 38]; 2],
    r: &mut [[[f32; 64]; 38]; 2],
    top: usize,
    num_qmf_slots: usize,
) {
    let is34 = ps.common.is34bands;
    let is34_idx = is34 as usize;
    let len: usize = num_qmf_slots;

    // Clear unused bands above top.
    let nr_bands = NR_BANDS[is34_idx];
    let top_hybrid = top + nr_bands - 64;
    for k in top_hybrid..nr_bands {
        ps.delay[k] = [[0.0; 2]; PS_QMF_TIME_SLOTS + PS_MAX_DELAY];
    }
    if top_hybrid < NR_ALLPASS_BANDS[is34_idx] {
        for k in top_hybrid..NR_ALLPASS_BANDS[is34_idx] {
            ps.ap_delay[k] = [[[0.0; 2]; PS_QMF_TIME_SLOTS + PS_MAX_AP_DELAY]; PS_AP_LINKS];
        }
    }

    // Step 1: Hybrid analysis of the mono (left) signal.
    hybrid_analysis(&mut ps.l_buf, &mut ps.in_buf, l, is34, len);

    // Step 2: Decorrelation — generate anticorrelated right signal.
    decorrelation(ps, is34, len);

    // Step 3: Stereo processing — mix mono + decorrelated using IID/ICC.
    stereo_processing(ps, is34);

    // Step 4: Hybrid synthesis — merge back to QMF domain for both L and R.
    hybrid_synthesis(l, &ps.l_buf, is34, len);
    hybrid_synthesis(r, &ps.r_buf, is34, len);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_ps_common_context_defaults() {
        let ctx = PsCommonContext::new();
        assert!(!ctx.start);
        assert!(!ctx.enable_iid);
        assert!(!ctx.enable_icc);
        assert!(!ctx.enable_ext);
        assert!(!ctx.enable_ipdopd);
        assert_eq!(ctx.num_env, 0);
        assert_eq!(ctx.num_env_old, 0);
        assert!(!ctx.is34bands);
        assert_eq!(ctx.nr_iid_par, 0);
        assert_eq!(ctx.nr_icc_par, 0);
    }

    #[test]
    fn verify_ps_context_construction() {
        // PsContext allocates large buffers on heap — verify it doesn't panic.
        let ctx = PsContext::new();
        assert!(!ctx.common.start);
        assert_eq!(ctx.peak_decay_nrg, [0.0; 34]);
        assert_eq!(ctx.power_smooth, [0.0; 34]);
        assert_eq!(ctx.opd_hist, [0; PS_MAX_NR_IIDICC]);
        assert_eq!(ctx.ipd_hist, [0; PS_MAX_NR_IIDICC]);
    }

    #[test]
    fn verify_map_idx_10_to_20_full() {
        let mut src = [0i8; PS_MAX_NR_IIDICC];
        for i in 0..10 {
            src[i] = (i + 1) as i8;
        }

        let mut dst = [0i8; PS_MAX_NR_IIDICC];
        map_idx_10_to_20(&mut dst, &src, true);

        // Each 10-band value should be duplicated into two 20-band values.
        for b in 0..10 {
            assert_eq!(dst[2 * b], src[b], "map_idx_10_to_20 even[{}]", b);
            assert_eq!(dst[2 * b + 1], src[b], "map_idx_10_to_20 odd[{}]", b);
        }
    }

    #[test]
    fn verify_map_idx_10_to_20_partial() {
        let mut src = [0i8; PS_MAX_NR_IIDICC];
        for i in 0..5 {
            src[i] = (i + 1) as i8;
        }

        let mut dst = [0i8; PS_MAX_NR_IIDICC];
        map_idx_10_to_20(&mut dst, &src, false);

        // Only first 5 bands are duplicated, band 10 is set to 0.
        for b in 0..5 {
            assert_eq!(dst[2 * b], src[b]);
            assert_eq!(dst[2 * b + 1], src[b]);
        }
        assert_eq!(dst[10], 0);
    }

    #[test]
    fn verify_map_idx_10_to_34_full() {
        let mut src = [0i8; PS_MAX_NR_IIDICC];
        for i in 0..10 {
            src[i] = (i + 1) as i8;
        }

        let mut dst = [0i8; PS_MAX_NR_IIDICC];
        map_idx_10_to_34(&mut dst, &src, true);

        // Verify low bands: 0,1,2 should map to par[0].
        assert_eq!(dst[0], src[0]);
        assert_eq!(dst[1], src[0]);
        assert_eq!(dst[2], src[0]);
        // Bands 3-5 should map to par[1].
        assert_eq!(dst[3], src[1]);
        assert_eq!(dst[4], src[1]);
        assert_eq!(dst[5], src[1]);
        // High bands (28-33) should all map to par[9].
        for i in 28..34 {
            assert_eq!(dst[i], src[9], "band {} should map to par[9]", i);
        }
    }

    #[test]
    fn verify_map_idx_20_to_34_full() {
        let mut src = [0i8; PS_MAX_NR_IIDICC];
        for i in 0..20 {
            src[i] = (i + 1) as i8;
        }

        let mut dst = [0i8; PS_MAX_NR_IIDICC];
        map_idx_20_to_34(&mut dst, &src, true);

        // First band should be par[0].
        assert_eq!(dst[0], src[0]);
        // Band 1 is average of par[0] and par[1].
        let expected = ((src[0] as i16 + src[1] as i16) / 2) as i8;
        assert_eq!(dst[1], expected);
        // Band 16 should be par[10].
        assert_eq!(dst[16], src[10]);
        // Last two bands should be par[19].
        assert_eq!(dst[32], src[19]);
        assert_eq!(dst[33], src[19]);
    }

    #[test]
    fn verify_map_idx_34_to_20_full() {
        let mut src = [0i8; PS_MAX_NR_IIDICC];
        for i in 0..34 {
            src[i] = (i + 1) as i8;
        }

        let mut dst = [0i8; PS_MAX_NR_IIDICC];
        map_idx_34_to_20(&mut dst, &src, true);

        // Band 0 is (2*par[0] + par[1])/3.
        let expected = ((2i16 * src[0] as i16 + src[1] as i16) / 3) as i8;
        assert_eq!(dst[0], expected);
        // Band 6 is par[10].
        assert_eq!(dst[6], src[10]);
        // Band 7 is par[11].
        assert_eq!(dst[7], src[11]);
    }

    #[test]
    fn verify_remap34_passthrough_34bands() {
        let mut par = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        for e in 0..2 {
            for b in 0..34 {
                par[e][b] = (b as i8) + (e as i8 * 10);
            }
        }

        let result = remap34(&par, 34, 2, true);

        // 34→34 should be identity copy.
        for e in 0..2 {
            for b in 0..34 {
                assert_eq!(result[e][b], par[e][b]);
            }
        }
    }

    #[test]
    fn verify_remap20_passthrough_20bands() {
        let mut par = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        for e in 0..2 {
            for b in 0..20 {
                par[e][b] = (b as i8) + (e as i8 * 10);
            }
        }

        let result = remap20(&par, 20, 2, true);

        // 20→20 should be identity copy.
        for e in 0..2 {
            for b in 0..20 {
                assert_eq!(result[e][b], par[e][b]);
            }
        }
    }

    #[test]
    fn verify_remap34_from_10bands() {
        let mut par = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        for b in 0..10 {
            par[0][b] = (b + 1) as i8;
        }

        let result = remap34(&par, 10, 1, true);

        // Low bands should use map_idx_10_to_34 values.
        assert_eq!(result[0][0], par[0][0]); // par[0] → bands 0-2
        assert_eq!(result[0][1], par[0][0]);
        assert_eq!(result[0][2], par[0][0]);
    }

    #[test]
    fn verify_remap20_from_34bands() {
        let mut par = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        for b in 0..34 {
            par[0][b] = (b + 1) as i8;
        }

        let result = remap20(&par, 34, 1, true);

        // Should use map_idx_34_to_20.
        let expected_6 = par[0][10]; // Band 6 = par[10]
        assert_eq!(result[0][6], expected_6);
    }

    #[test]
    fn verify_map_val_20_to_34() {
        let mut par = [0.0f32; PS_MAX_NR_IIDICC];
        for i in 0..20 {
            par[i] = (i + 1) as f32;
        }

        map_val_20_to_34(&mut par);

        // Band 0 should remain par[0] = 1.0.
        assert_eq!(par[0], 1.0);
        // Band 1 is average of original par[0] and par[1] = (1+2)/2 = 1.5.
        assert!((par[1] - 1.5).abs() < 1e-6);
        // Last two bands should equal original par[19] = 20.0.
        assert_eq!(par[32], 20.0);
        assert_eq!(par[33], 20.0);
    }

    #[test]
    fn verify_map_val_34_to_20() {
        let mut par = [0.0f32; PS_MAX_NR_IIDICC];
        for i in 0..34 {
            par[i] = (i + 1) as f32;
        }

        map_val_34_to_20(&mut par);

        // Band 6 = par[10] = 11.0.
        assert_eq!(par[6], 11.0);
        // Band 7 = par[11] = 12.0.
        assert_eq!(par[7], 12.0);
        // Band 0 = (2*1 + 2)/3 = 4/3 ≈ 1.333.
        assert!((par[0] - 4.0 / 3.0).abs() < 1e-5);
    }

    #[test]
    fn verify_hybrid_synthesis_20band_sums_subbands() {
        let mut buf = Box::new([[[0.0f32; 2]; 32]; PS_MAX_SSB]);

        // Set hybrid subbands for QMF band 0 (6 subbands: indices 0-5).
        for sub in 0..6 {
            buf[sub][0][0] = 1.0; // re
            buf[sub][0][1] = 0.5; // im
        }

        let mut out = [[[0.0f32; 64]; 38]; 2];
        hybrid_synthesis(&mut out, &buf, false, 1);

        // QMF band 0 should be the sum of 6 hybrid subbands.
        assert!((out[0][0][0] - 6.0).abs() < 1e-6, "QMF 0 re should be 6.0");
        assert!((out[1][0][0] - 3.0).abs() < 1e-6, "QMF 0 im should be 3.0");
    }

    #[test]
    fn verify_hybrid_synthesis_20band_direct_copy() {
        let mut buf = Box::new([[[0.0f32; 2]; 32]; PS_MAX_SSB]);

        // Set QMF band 3 (hybrid index = 3 + 7 = 10) to known values.
        buf[10][0][0] = 42.0;
        buf[10][0][1] = -7.5;

        let mut out = [[[0.0f32; 64]; 38]; 2];
        hybrid_synthesis(&mut out, &buf, false, 1);

        // QMF band 3 should be directly copied from hybrid index 10.
        assert!((out[0][0][3] - 42.0).abs() < 1e-6);
        assert!((out[1][0][3] - (-7.5)).abs() < 1e-6);
    }

    #[test]
    fn verify_hybrid_synthesis_34band_sums_subbands() {
        let mut buf = Box::new([[[0.0f32; 2]; 32]; PS_MAX_SSB]);

        // QMF band 0: 12 hybrid subbands (indices 0-11).
        for sub in 0..12 {
            buf[sub][0][0] = 1.0;
            buf[sub][0][1] = 2.0;
        }

        let mut out = [[[0.0f32; 64]; 38]; 2];
        hybrid_synthesis(&mut out, &buf, true, 1);

        assert!((out[0][0][0] - 12.0).abs() < 1e-6, "QMF 0 re should be 12.0");
        assert!((out[1][0][0] - 24.0).abs() < 1e-6, "QMF 0 im should be 24.0");
    }

    #[test]
    fn verify_hybrid_synthesis_34band_qmf1_sum() {
        let mut buf = Box::new([[[0.0f32; 2]; 32]; PS_MAX_SSB]);

        // QMF band 1: 8 hybrid subbands (indices 12-19).
        for sub in 12..20 {
            buf[sub][0][0] = 0.5;
            buf[sub][0][1] = -0.25;
        }

        let mut out = [[[0.0f32; 64]; 38]; 2];
        hybrid_synthesis(&mut out, &buf, true, 1);

        assert!((out[0][0][1] - 4.0).abs() < 1e-6, "QMF 1 re should be 4.0");
        assert!((out[1][0][1] - (-2.0)).abs() < 1e-6, "QMF 1 im should be -2.0");
    }

    #[test]
    fn verify_hybrid_synthesis_34band_direct_copy() {
        let mut buf = Box::new([[[0.0f32; 2]; 32]; PS_MAX_SSB]);

        // QMF band 5 in 34-band mode: hybrid index = 5 + 27 = 32.
        buf[32][0][0] = 99.0;
        buf[32][0][1] = -1.0;

        let mut out = [[[0.0f32; 64]; 38]; 2];
        hybrid_synthesis(&mut out, &buf, true, 1);

        assert!((out[0][0][5] - 99.0).abs() < 1e-6);
        assert!((out[1][0][5] - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn verify_hybrid2_re_produces_output() {
        // Feed a DC signal through the 2-band real hybrid filter.
        let mut input = [[0.0f32; 2]; 44];
        for i in 0..13 {
            input[i][0] = 1.0; // re = 1.0
        }

        let mut out = Box::new([[[0.0f32; 2]; 32]; PS_MAX_SSB]);
        hybrid2_re(&input, &mut out, 0, 1, &G1_Q2, 1);

        // With DC input, band 0 (lowpass) should have energy,
        // band 1 (highpass) should have less or zero for this filter.
        let sum_sq_0 = out[0][0][0] * out[0][0][0] + out[0][0][1] * out[0][0][1];
        // Just verify it produces non-trivial output.
        assert!(sum_sq_0 > 0.0 || out[1][0][0].abs() > 0.0, "hybrid2 should produce output");
    }

    #[test]
    fn verify_decorrelation_zero_input_zero_output() {
        let mut ps = PsContext::new();
        ps.common.is34bands = false;
        ps.common.is34bands_old = false;
        ps.common.num_env = 1;

        // Zero input in l_buf → decorrelation should produce zero in r_buf.
        decorrelation(&mut ps, false, PS_QMF_TIME_SLOTS);

        for k in 0..NR_BANDS[0] {
            for n in 0..PS_QMF_TIME_SLOTS {
                assert!(
                    ps.r_buf[k][n][0].abs() < 1e-10,
                    "r_buf[{}][{}][0] should be zero for zero input",
                    k,
                    n
                );
                assert!(
                    ps.r_buf[k][n][1].abs() < 1e-10,
                    "r_buf[{}][{}][1] should be zero for zero input",
                    k,
                    n
                );
            }
        }
    }
}
