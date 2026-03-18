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

// ---------------------------------------------------------------------------
// Hybrid analysis / synthesis (ISO/IEC 14496-3:2009, Section 8.6.4.6.1)
// ---------------------------------------------------------------------------

/// Apply a 2-band real-valued symmetric prototype filter, splitting a single QMF
/// subband into two sub-subbands (lowpass and highpass).
///
/// ISO/IEC 14496-3:2009, 8.6.4.6.1 -- real-valued 2-band hybrid filter.
fn split_2band_real(
    samples: &[[f32; 2]],
    hybrid_out: &mut [[[f32; 2]; 32]; PS_MAX_SSB],
    low_idx: usize,
    high_idx: usize,
    proto: &[f32; 7],
    num_slots: usize,
) {
    for slot in 0..num_slots {
        // Centre tap contribution (index 6 of 13 taps).
        let centre_re = proto[6] * samples[slot + 6][0];
        let centre_im = proto[6] * samples[slot + 6][1];

        // Accumulate odd-indexed symmetric tap pairs.
        let mut sym_re = 0.0f32;
        let mut sym_im = 0.0f32;
        for tap in (0..6).step_by(2) {
            let coeff = proto[tap + 1];
            sym_re += coeff * (samples[slot + tap + 1][0] + samples[slot + 12 - tap - 1][0]);
            sym_im += coeff * (samples[slot + tap + 1][1] + samples[slot + 12 - tap - 1][1]);
        }

        // Lowpass = centre + symmetric; highpass = centre - symmetric.
        hybrid_out[low_idx][slot][0] = centre_re + sym_re;
        hybrid_out[low_idx][slot][1] = centre_im + sym_im;
        hybrid_out[high_idx][slot][0] = centre_re - sym_re;
        hybrid_out[high_idx][slot][1] = centre_im - sym_im;
    }
}

/// Apply an N-band complex-valued prototype filter, splitting one QMF subband
/// into multiple sub-subbands via complex modulated analysis.
///
/// ISO/IEC 14496-3:2009, 8.6.4.6.1 -- complex N-band hybrid filter bank.
fn split_nband_complex(
    samples: &[[f32; 2]],
    hybrid_out: &mut [[[f32; 2]; 32]; PS_MAX_SSB],
    first_subband: usize,
    proto_filter: &[[[f32; 2]; 8]],
    subband_count: usize,
    output_stride: usize,
    num_slots: usize,
) {
    for slot in 0..num_slots {
        // Pre-compute symmetric sums and differences for the 13-tap window.
        // ISO/IEC 14496-3:2009, 8.6.4.6.1 -- exploit conjugate symmetry.
        let mut sum_re = [0.0f32; 6];
        let mut diff_im = [0.0f32; 6];
        let mut sum_im = [0.0f32; 6];
        let mut diff_re = [0.0f32; 6];

        for tap in 0..6 {
            sum_re[tap] = samples[slot + tap][0] + samples[slot + 12 - tap][0];
            diff_im[tap] = samples[slot + tap][1] - samples[slot + 12 - tap][1];
            sum_im[tap] = samples[slot + tap][1] + samples[slot + 12 - tap][1];
            diff_re[tap] = samples[slot + tap][0] - samples[slot + 12 - tap][0];
        }

        for band in 0..subband_count {
            // Centre tap (real-only coefficient at tap index 6).
            let mut acc_re = proto_filter[band][6][0] * samples[slot + 6][0];
            let mut acc_im = proto_filter[band][6][0] * samples[slot + 6][1];

            // Symmetric taps with complex coefficients.
            for tap in 0..6 {
                let coeff_re = proto_filter[band][tap][0];
                let coeff_im = proto_filter[band][tap][1];
                acc_re += coeff_re * sum_re[tap] - coeff_im * diff_im[tap];
                acc_im += coeff_re * sum_im[tap] + coeff_im * diff_re[tap];
            }

            hybrid_out[first_subband + band][slot * output_stride][0] = acc_re;
            hybrid_out[first_subband + band][slot * output_stride][1] = acc_im;
        }
    }
}

/// Perform 6-band complex hybrid analysis for QMF subband 0 in 20-band mode.
///
/// This uses an 8-point prototype and then maps the 8 analysis outputs down
/// to 6 hybrid sub-subbands by merging certain pairs.
///
/// ISO/IEC 14496-3:2009, 8.6.4.6.1 -- 6-band hybrid filter for subband 0.
fn split_6band_complex(
    hybrid_out: &mut [[[f32; 2]; 32]; PS_MAX_SSB],
    samples: &[[f32; 2]],
    proto_8band: &[[[f32; 2]; 8]; 8],
    num_slots: usize,
) {
    for slot in 0..num_slots {
        // Pre-compute symmetric sums/differences across the 13-tap window.
        let mut sum_re = [0.0f32; 6];
        let mut diff_im = [0.0f32; 6];
        let mut sum_im = [0.0f32; 6];
        let mut diff_re = [0.0f32; 6];
        for tap in 0..6 {
            sum_re[tap] = samples[slot + tap][0] + samples[slot + 12 - tap][0];
            diff_im[tap] = samples[slot + tap][1] - samples[slot + 12 - tap][1];
            sum_im[tap] = samples[slot + tap][1] + samples[slot + 12 - tap][1];
            diff_re[tap] = samples[slot + tap][0] - samples[slot + 12 - tap][0];
        }

        // Compute all 8 analysis sub-subbands.
        let mut analysis = [[0.0f32; 2]; 8];
        for band in 0..8 {
            let mut acc_re = proto_8band[band][6][0] * samples[slot + 6][0];
            let mut acc_im = proto_8band[band][6][0] * samples[slot + 6][1];

            for tap in 0..6 {
                let cr = proto_8band[band][tap][0];
                let ci = proto_8band[band][tap][1];
                acc_re += cr * sum_re[tap] - ci * diff_im[tap];
                acc_im += cr * sum_im[tap] + ci * diff_re[tap];
            }

            analysis[band][0] = acc_re;
            analysis[band][1] = acc_im;
        }

        // Map 8 analysis channels to 6 hybrid sub-subbands.
        // Bands 4 and 5 each merge two analysis channels.
        hybrid_out[0][slot][0] = analysis[6][0];
        hybrid_out[0][slot][1] = analysis[6][1];
        hybrid_out[1][slot][0] = analysis[7][0];
        hybrid_out[1][slot][1] = analysis[7][1];
        hybrid_out[2][slot][0] = analysis[0][0];
        hybrid_out[2][slot][1] = analysis[0][1];
        hybrid_out[3][slot][0] = analysis[1][0];
        hybrid_out[3][slot][1] = analysis[1][1];
        hybrid_out[4][slot][0] = analysis[2][0] + analysis[5][0];
        hybrid_out[4][slot][1] = analysis[2][1] + analysis[5][1];
        hybrid_out[5][slot][0] = analysis[3][0] + analysis[4][0];
        hybrid_out[5][slot][1] = analysis[3][1] + analysis[4][1];
    }
}

/// Hybrid analysis: decompose the 5 lowest QMF subbands into hybrid
/// sub-subbands for finer frequency resolution.
///
/// ISO/IEC 14496-3:2009, 8.6.4.6.1 -- hybrid analysis filterbank.
fn analyze_hybrid(
    hybrid_out: &mut [[[f32; 2]; 32]; PS_MAX_SSB],
    history_buf: &mut [[[f32; 2]; 44]; 5],
    qmf_input: &[[[f32; 64]; 38]; 2],
    use_34bands: bool,
    num_slots: usize,
) {
    let tables = &*PS_TABLES;

    // Populate input history buffers with current frame's QMF data.
    // The first 6 positions hold overlap from the previous frame.
    for qmf_band in 0..5 {
        for time_idx in 0..38usize.min(num_slots + 6) {
            if time_idx >= 6 && (time_idx - 6) < num_slots {
                let slot = time_idx - 6;
                history_buf[qmf_band][time_idx][0] = qmf_input[0][slot][qmf_band];
                history_buf[qmf_band][time_idx][1] = qmf_input[1][slot][qmf_band];
            }
        }
    }

    if use_34bands {
        // 34-band mode: 12 + 8 + 4 + 4 + 4 = 32 hybrid sub-subbands
        // from QMF bands 0..4.
        split_nband_complex(&history_buf[0], hybrid_out, 0, &tables.f34_0_12, 12, 1, num_slots);
        split_nband_complex(&history_buf[1], hybrid_out, 12, &tables.f34_1_8, 8, 1, num_slots);
        split_nband_complex(&history_buf[2], hybrid_out, 20, &tables.f34_2_4, 4, 1, num_slots);
        split_nband_complex(&history_buf[3], hybrid_out, 24, &tables.f34_2_4, 4, 1, num_slots);
        split_nband_complex(&history_buf[4], hybrid_out, 28, &tables.f34_2_4, 4, 1, num_slots);

        // QMF bands 5..63 pass through unmodified (offset by 27 in hybrid domain).
        for qmf_band in 5..64 {
            for slot in 0..num_slots {
                hybrid_out[qmf_band + 27][slot][0] = qmf_input[0][slot][qmf_band];
                hybrid_out[qmf_band + 27][slot][1] = qmf_input[1][slot][qmf_band];
            }
        }
    }
    else {
        // 20-band mode: 6 + 2 + 2 = 10 hybrid sub-subbands from QMF bands 0..2.
        // QMF band 0: 6-band complex hybrid analysis.
        split_6band_complex(hybrid_out, &history_buf[0], &tables.f20_0_8, num_slots);
        // QMF band 1: 2-band real hybrid (note: indices are reversed).
        split_2band_real(&history_buf[1], hybrid_out, 7, 6, &G1_Q2, num_slots);
        // QMF band 2: 2-band real hybrid.
        split_2band_real(&history_buf[2], hybrid_out, 8, 9, &G1_Q2, num_slots);

        // QMF bands 3..63 pass through (offset by 7 in hybrid domain).
        for qmf_band in 3..64 {
            for slot in 0..num_slots {
                hybrid_out[qmf_band + 7][slot][0] = qmf_input[0][slot][qmf_band];
                hybrid_out[qmf_band + 7][slot][1] = qmf_input[1][slot][qmf_band];
            }
        }
    }

    // Preserve the final 6 samples as overlap for the next frame.
    for qmf_band in 0..5 {
        for overlap_idx in 0..6 {
            history_buf[qmf_band][overlap_idx] = history_buf[qmf_band][num_slots + overlap_idx];
        }
    }
}

/// Hybrid synthesis: recombine hybrid sub-subbands back into QMF subbands.
///
/// ISO/IEC 14496-3:2009, 8.6.4.6.1 -- hybrid synthesis filterbank.
fn synthesize_hybrid(
    qmf_output: &mut [[[f32; 64]; 38]; 2],
    hybrid_in: &[[[f32; 2]; 32]; PS_MAX_SSB],
    use_34bands: bool,
    num_slots: usize,
) {
    if use_34bands {
        for slot in 0..num_slots {
            // QMF band 0: sum 12 hybrid sub-subbands (indices 0..11).
            let (mut acc_re, mut acc_im) = (0.0f32, 0.0f32);
            for sub in 0..12 {
                acc_re += hybrid_in[sub][slot][0];
                acc_im += hybrid_in[sub][slot][1];
            }
            qmf_output[0][slot][0] = acc_re;
            qmf_output[1][slot][0] = acc_im;

            // QMF band 1: sum 8 hybrid sub-subbands (indices 12..19).
            let (mut acc_re, mut acc_im) = (0.0f32, 0.0f32);
            for sub in 0..8 {
                acc_re += hybrid_in[12 + sub][slot][0];
                acc_im += hybrid_in[12 + sub][slot][1];
            }
            qmf_output[0][slot][1] = acc_re;
            qmf_output[1][slot][1] = acc_im;

            // QMF bands 2..4: each sums 4 hybrid sub-subbands.
            for group in 0..3 {
                let (mut acc_re, mut acc_im) = (0.0f32, 0.0f32);
                for sub in 0..4 {
                    acc_re += hybrid_in[20 + group * 4 + sub][slot][0];
                    acc_im += hybrid_in[20 + group * 4 + sub][slot][1];
                }
                qmf_output[0][slot][2 + group] = acc_re;
                qmf_output[1][slot][2 + group] = acc_im;
            }

            // QMF bands 5..63: direct passthrough from hybrid domain.
            for qmf_band in 5..64 {
                qmf_output[0][slot][qmf_band] = hybrid_in[qmf_band + 27][slot][0];
                qmf_output[1][slot][qmf_band] = hybrid_in[qmf_band + 27][slot][1];
            }
        }
    }
    else {
        for slot in 0..num_slots {
            // QMF band 0: sum of 6 hybrid sub-subbands.
            qmf_output[0][slot][0] = hybrid_in[0][slot][0]
                + hybrid_in[1][slot][0]
                + hybrid_in[2][slot][0]
                + hybrid_in[3][slot][0]
                + hybrid_in[4][slot][0]
                + hybrid_in[5][slot][0];
            qmf_output[1][slot][0] = hybrid_in[0][slot][1]
                + hybrid_in[1][slot][1]
                + hybrid_in[2][slot][1]
                + hybrid_in[3][slot][1]
                + hybrid_in[4][slot][1]
                + hybrid_in[5][slot][1];

            // QMF band 1: sum of 2 hybrid sub-subbands.
            qmf_output[0][slot][1] = hybrid_in[6][slot][0] + hybrid_in[7][slot][0];
            qmf_output[1][slot][1] = hybrid_in[6][slot][1] + hybrid_in[7][slot][1];

            // QMF band 2: sum of 2 hybrid sub-subbands.
            qmf_output[0][slot][2] = hybrid_in[8][slot][0] + hybrid_in[9][slot][0];
            qmf_output[1][slot][2] = hybrid_in[8][slot][1] + hybrid_in[9][slot][1];

            // QMF bands 3..63: direct passthrough.
            for qmf_band in 3..64 {
                qmf_output[0][slot][qmf_band] = hybrid_in[qmf_band + 7][slot][0];
                qmf_output[1][slot][qmf_band] = hybrid_in[qmf_band + 7][slot][1];
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Decorrelation (ISO/IEC 14496-3:2009, Section 8.6.4.6.2)
// ---------------------------------------------------------------------------

/// Generate the decorrelated (anticorrelated) side signal from the mono input.
///
/// Implements transient detection, cascaded 3-link all-pass filtering with
/// fractional delay for low-frequency bands, and simple delay-based
/// decorrelation for mid- and high-frequency bands.
///
/// ISO/IEC 14496-3:2009, 8.6.4.6.2 -- PS decorrelation.
fn generate_decorrelated_signal(ps: &mut PsContext, use_34bands: bool, num_slots: usize) {
    let tables = &*PS_TABLES;
    let mode_idx = use_34bands as usize;

    let subband_to_param = if use_34bands { &K_TO_I_34[..] } else { &K_TO_I_20[..] };
    let total_subbands = NR_BANDS[mode_idx];
    let num_param_bands = NR_PAR_BANDS[mode_idx];
    let num_allpass_bands = NR_ALLPASS_BANDS[mode_idx];
    let short_delay_start = SHORT_DELAY_BAND[mode_idx];
    let decay_cutoff_band = DECAY_CUTOFF[mode_idx];

    // ISO/IEC 14496-3:2009, 8.6.4.6.2 -- decorrelation constants.
    let peak_decay_coeff: f32 = 0.76592833836465;
    let smoothing_coeff: f32 = 0.25;
    let transient_scale: f32 = 1.5;

    // When the band configuration changes, reset all decorrelation state.
    if use_34bands != ps.common.is34bands_old {
        ps.peak_decay_nrg = [0.0; 34];
        ps.power_smooth = [0.0; 34];
        ps.peak_decay_diff_smooth = [0.0; 34];
        ps.delay.iter_mut().for_each(|entry| {
            *entry = [[0.0; 2]; PS_QMF_TIME_SLOTS + PS_MAX_DELAY];
        });
        ps.ap_delay.iter_mut().for_each(|entry| {
            *entry = [[[0.0; 2]; PS_QMF_TIME_SLOTS + PS_MAX_AP_DELAY]; PS_AP_LINKS];
        });
    }

    // Measure signal power in each parameter band.
    // ISO/IEC 14496-3:2009, 8.6.4.6.2 -- power estimation.
    let mut band_power = [[0.0f32; PS_QMF_TIME_SLOTS]; 34];
    for subband in 0..total_subbands {
        let param_band = subband_to_param[subband];
        for slot in 0..num_slots {
            let re = ps.l_buf[subband][slot][0];
            let im = ps.l_buf[subband][slot][1];
            band_power[param_band][slot] += re * re + im * im;
        }
    }

    // Transient detection: derive a per-band attenuation factor.
    // ISO/IEC 14496-3:2009, 8.6.4.6.2 -- transient detection.
    let mut attenuation = [[0.0f32; PS_QMF_TIME_SLOTS]; 34];
    for param_band in 0..num_param_bands {
        for slot in 0..num_slots {
            let decayed = peak_decay_coeff * ps.peak_decay_nrg[param_band];
            let current_power = band_power[param_band][slot];

            ps.peak_decay_nrg[param_band] = decayed.max(current_power);
            ps.power_smooth[param_band] +=
                smoothing_coeff * (current_power - ps.power_smooth[param_band]);
            ps.peak_decay_diff_smooth[param_band] += smoothing_coeff
                * (ps.peak_decay_nrg[param_band]
                    - current_power
                    - ps.peak_decay_diff_smooth[param_band]);

            let scaled_diff = transient_scale * ps.peak_decay_diff_smooth[param_band];
            attenuation[param_band][slot] = if scaled_diff > ps.power_smooth[param_band] {
                ps.power_smooth[param_band] / scaled_diff
            }
            else {
                1.0
            };
        }
    }

    // All-pass decorrelation for low-frequency bands.
    // ISO/IEC 14496-3:2009, 8.6.4.6.2 -- all-pass decorrelation filter.
    for subband in 0..num_allpass_bands {
        let param_idx = subband_to_param[subband];
        let slope_gain =
            (1.0 - DECAY_SLOPE * (subband as f32 - decay_cutoff_band as f32)).clamp(0.0, 1.0);

        // Shift delay line history forward.
        for d in 0..PS_MAX_DELAY {
            ps.delay[subband][d] = ps.delay[subband][num_slots + d];
        }
        for s in 0..num_slots {
            ps.delay[subband][PS_MAX_DELAY + s] = ps.l_buf[subband][s];
        }

        // Shift all-pass delay history forward.
        for link in 0..PS_AP_LINKS {
            for d in 0..5 {
                ps.ap_delay[subband][link][d] = ps.ap_delay[subband][link][num_slots + d];
            }
        }

        // Compute scaled all-pass coefficients.
        let scaled_ap: [f32; PS_AP_LINKS] =
            [AP_COEFF[0] * slope_gain, AP_COEFF[1] * slope_gain, AP_COEFF[2] * slope_gain];

        // Process each time slot through the 3-link cascaded all-pass chain.
        let fractional_phase = &tables.phi_fract[mode_idx][subband];
        for slot in 0..num_slots {
            // Apply fractional delay rotation to the delayed input.
            let delayed_re = ps.delay[subband][PS_MAX_DELAY + slot - 2][0];
            let delayed_im = ps.delay[subband][PS_MAX_DELAY + slot - 2][1];
            let mut signal_re = delayed_re * fractional_phase[0] - delayed_im * fractional_phase[1];
            let mut signal_im = delayed_re * fractional_phase[1] + delayed_im * fractional_phase[0];

            // Cascade through 3 all-pass filter links.
            for link in 0..PS_AP_LINKS {
                let feedback_re = scaled_ap[link] * signal_re;
                let feedback_im = scaled_ap[link] * signal_im;
                let delay_idx = slot + 2 - link;
                let link_re = ps.ap_delay[subband][link][delay_idx][0];
                let link_im = ps.ap_delay[subband][link][delay_idx][1];
                let frac_re = tables.q_fract_allpass[mode_idx][subband][link][0];
                let frac_im = tables.q_fract_allpass[mode_idx][subband][link][1];

                let input_snapshot_re = signal_re;
                let input_snapshot_im = signal_im;

                // Fractional delay on the link delay line output.
                signal_re = link_re * frac_re - link_im * frac_im - feedback_re;
                signal_im = link_re * frac_im + link_im * frac_re - feedback_im;

                // Write to the all-pass delay line.
                ps.ap_delay[subband][link][slot + 5][0] =
                    input_snapshot_re + scaled_ap[link] * signal_re;
                ps.ap_delay[subband][link][slot + 5][1] =
                    input_snapshot_im + scaled_ap[link] * signal_im;
            }

            // Attenuate according to the transient detector.
            ps.r_buf[subband][slot][0] = attenuation[param_idx][slot] * signal_re;
            ps.r_buf[subband][slot][1] = attenuation[param_idx][slot] * signal_im;
        }
    }

    // Mid-frequency bands: simple decorrelation with 14-sample delay.
    // ISO/IEC 14496-3:2009, 8.6.4.6.2 -- delay-based decorrelation.
    for subband in num_allpass_bands..short_delay_start {
        let param_idx = subband_to_param[subband];

        for d in 0..PS_MAX_DELAY {
            ps.delay[subband][d] = ps.delay[subband][num_slots + d];
        }
        for s in 0..num_slots {
            ps.delay[subband][PS_MAX_DELAY + s] = ps.l_buf[subband][s];
        }
        for slot in 0..num_slots {
            ps.r_buf[subband][slot][0] =
                attenuation[param_idx][slot] * ps.delay[subband][PS_MAX_DELAY + slot - 14][0];
            ps.r_buf[subband][slot][1] =
                attenuation[param_idx][slot] * ps.delay[subband][PS_MAX_DELAY + slot - 14][1];
        }
    }

    // High-frequency bands: short decorrelation with 1-sample delay.
    for subband in short_delay_start..total_subbands {
        let param_idx = subband_to_param[subband];

        for d in 0..PS_MAX_DELAY {
            ps.delay[subband][d] = ps.delay[subband][num_slots + d];
        }
        for s in 0..num_slots {
            ps.delay[subband][PS_MAX_DELAY + s] = ps.l_buf[subband][s];
        }
        for slot in 0..num_slots {
            ps.r_buf[subband][slot][0] =
                attenuation[param_idx][slot] * ps.delay[subband][PS_MAX_DELAY + slot - 1][0];
            ps.r_buf[subband][slot][1] =
                attenuation[param_idx][slot] * ps.delay[subband][PS_MAX_DELAY + slot - 1][1];
        }
    }
}

// ---------------------------------------------------------------------------
// Parameter band mapping (ISO/IEC 14496-3:2009, Section 8.6.4.6.3)
// ---------------------------------------------------------------------------

/// Expand 10-band parameters to 20-band representation by duplicating entries.
///
/// ISO/IEC 14496-3:2009, 8.6.4.6.3 -- parameter mapping.
fn expand_10_to_20(
    dst: &mut [i8; PS_MAX_NR_IIDICC],
    src: &[i8; PS_MAX_NR_IIDICC],
    full_range: bool,
) {
    if full_range {
        // Duplicate each of the 10 source bands into 2 destination bands.
        for idx in (0..=9).rev() {
            dst[2 * idx + 1] = src[idx];
            dst[2 * idx] = src[idx];
        }
    }
    else {
        // Only the lower 5 source bands are duplicated.
        for idx in (0..=4).rev() {
            dst[2 * idx + 1] = src[idx];
            dst[2 * idx] = src[idx];
        }
        dst[10] = 0;
    }
}

/// Contract 34-band parameters to 20-band representation by averaging groups.
///
/// ISO/IEC 14496-3:2009, 8.6.4.6.3 -- parameter mapping.
fn contract_34_to_20(
    dst: &mut [i8; PS_MAX_NR_IIDICC],
    src: &[i8; PS_MAX_NR_IIDICC],
    full_range: bool,
) {
    // Lower bands: weighted averages of adjacent 34-band parameters.
    dst[0] = ((2i16 * src[0] as i16 + src[1] as i16) / 3) as i8;
    dst[1] = ((src[1] as i16 + 2i16 * src[2] as i16) / 3) as i8;
    dst[2] = ((2i16 * src[3] as i16 + src[4] as i16) / 3) as i8;
    dst[3] = ((src[4] as i16 + 2i16 * src[5] as i16) / 3) as i8;
    dst[4] = ((src[6] as i16 + src[7] as i16) / 2) as i8;
    dst[5] = ((src[8] as i16 + src[9] as i16) / 2) as i8;
    dst[6] = src[10];
    dst[7] = src[11];
    dst[8] = ((src[12] as i16 + src[13] as i16) / 2) as i8;
    dst[9] = ((src[14] as i16 + src[15] as i16) / 2) as i8;
    dst[10] = src[16];

    if full_range {
        // Upper bands.
        dst[11] = src[17];
        dst[12] = src[18];
        dst[13] = src[19];
        dst[14] = ((src[20] as i16 + src[21] as i16) / 2) as i8;
        dst[15] = ((src[22] as i16 + src[23] as i16) / 2) as i8;
        dst[16] = ((src[24] as i16 + src[25] as i16) / 2) as i8;
        dst[17] = ((src[26] as i16 + src[27] as i16) / 2) as i8;
        dst[18] = ((src[28] as i16 + src[29] as i16 + src[30] as i16 + src[31] as i16) / 4) as i8;
        dst[19] = ((src[32] as i16 + src[33] as i16) / 2) as i8;
    }
}

/// Expand 10-band parameters to 34-band representation.
///
/// ISO/IEC 14496-3:2009, 8.6.4.6.3 -- parameter mapping.
fn expand_10_to_34(
    dst: &mut [i8; PS_MAX_NR_IIDICC],
    src: &[i8; PS_MAX_NR_IIDICC],
    full_range: bool,
) {
    if full_range {
        // High bands.
        for pos in 28..34 {
            dst[pos] = src[9];
        }
        dst[27] = src[8];
        dst[26] = src[8];
        dst[25] = src[8];
        dst[24] = src[8];
        for pos in 20..24 {
            dst[pos] = src[7];
        }
        dst[19] = src[6];
        dst[18] = src[6];
        dst[17] = src[5];
        dst[16] = src[5];
    }
    else {
        dst[16] = 0;
    }

    // Mid and low bands.
    for pos in 12..16 {
        dst[pos] = src[4];
    }
    dst[11] = src[3];
    dst[10] = src[3];
    for pos in 6..10 {
        dst[pos] = src[2];
    }
    for pos in 3..6 {
        dst[pos] = src[1];
    }
    dst[2] = src[0];
    dst[1] = src[0];
    dst[0] = src[0];
}

/// Expand 20-band parameters to 34-band representation, with interpolation
/// at boundary crossings.
///
/// ISO/IEC 14496-3:2009, 8.6.4.6.3 -- parameter mapping.
fn expand_20_to_34(
    dst: &mut [i8; PS_MAX_NR_IIDICC],
    src: &[i8; PS_MAX_NR_IIDICC],
    full_range: bool,
) {
    if full_range {
        // Upper bands.
        dst[33] = src[19];
        dst[32] = src[19];
        for pos in 28..32 {
            dst[pos] = src[18];
        }
        dst[27] = src[17];
        dst[26] = src[17];
        dst[25] = src[16];
        dst[24] = src[16];
        dst[23] = src[15];
        dst[22] = src[15];
        dst[21] = src[14];
        dst[20] = src[14];
        dst[19] = src[13];
        dst[18] = src[12];
        dst[17] = src[11];
    }

    // Lower bands (always mapped).
    dst[16] = src[10];
    dst[15] = src[9];
    dst[14] = src[9];
    dst[13] = src[8];
    dst[12] = src[8];
    dst[11] = src[7];
    dst[10] = src[6];
    dst[9] = src[5];
    dst[8] = src[5];
    dst[7] = src[4];
    dst[6] = src[4];
    dst[5] = src[3];
    dst[4] = ((src[2] as i16 + src[3] as i16) / 2) as i8;
    dst[3] = src[2];
    dst[2] = src[1];
    dst[1] = ((src[0] as i16 + src[1] as i16) / 2) as i8;
    dst[0] = src[0];
}

/// Expand float-valued 20-band H-matrix data to 34-band representation.
///
/// ISO/IEC 14496-3:2009, 8.6.4.6.3 -- mixing matrix remapping.
fn expand_val_20_to_34(values: &mut [f32; PS_MAX_NR_IIDICC]) {
    values[33] = values[19];
    values[32] = values[19];
    values[31] = values[18];
    values[30] = values[18];
    values[29] = values[18];
    values[28] = values[18];
    values[27] = values[17];
    values[26] = values[17];
    values[25] = values[16];
    values[24] = values[16];
    values[23] = values[15];
    values[22] = values[15];
    values[21] = values[14];
    values[20] = values[14];
    values[19] = values[13];
    values[18] = values[12];
    values[17] = values[11];
    values[16] = values[10];
    values[15] = values[9];
    values[14] = values[9];
    values[13] = values[8];
    values[12] = values[8];
    values[11] = values[7];
    values[10] = values[6];
    values[9] = values[5];
    values[8] = values[5];
    values[7] = values[4];
    values[6] = values[4];
    values[5] = values[3];
    values[4] = (values[2] + values[3]) * 0.5;
    values[3] = values[2];
    values[2] = values[1];
    values[1] = (values[0] + values[1]) * 0.5;
}

/// Contract float-valued 34-band H-matrix data to 20-band representation.
///
/// ISO/IEC 14496-3:2009, 8.6.4.6.3 -- mixing matrix remapping.
fn contract_val_34_to_20(values: &mut [f32; PS_MAX_NR_IIDICC]) {
    values[0] = (2.0 * values[0] + values[1]) * 0.33333333;
    values[1] = (values[1] + 2.0 * values[2]) * 0.33333333;
    values[2] = (2.0 * values[3] + values[4]) * 0.33333333;
    values[3] = (values[4] + 2.0 * values[5]) * 0.33333333;
    values[4] = (values[6] + values[7]) * 0.5;
    values[5] = (values[8] + values[9]) * 0.5;
    values[6] = values[10];
    values[7] = values[11];
    values[8] = (values[12] + values[13]) * 0.5;
    values[9] = (values[14] + values[15]) * 0.5;
    values[10] = values[16];
    values[11] = values[17];
    values[12] = values[18];
    values[13] = values[19];
    values[14] = (values[20] + values[21]) * 0.5;
    values[15] = (values[22] + values[23]) * 0.5;
    values[16] = (values[24] + values[25]) * 0.5;
    values[17] = (values[26] + values[27]) * 0.5;
    values[18] = (values[28] + values[29] + values[30] + values[31]) * 0.25;
    values[19] = (values[32] + values[33]) * 0.5;
}

/// Remap integer parameter arrays to 34-band target resolution.
///
/// ISO/IEC 14496-3:2009, 8.6.4.6.3 -- parameter remapping.
fn remap_to_34(
    params: &[[i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV],
    source_band_count: usize,
    envelope_count: usize,
    full_range: bool,
) -> [[i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV] {
    let mut remapped = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];

    if source_band_count == 20 || source_band_count == 11 {
        for env in 0..envelope_count {
            expand_20_to_34(&mut remapped[env], &params[env], full_range);
        }
    }
    else if source_band_count == 10 || source_band_count == 5 {
        for env in 0..envelope_count {
            expand_10_to_34(&mut remapped[env], &params[env], full_range);
        }
    }
    else {
        // Already at 34 bands -- copy unchanged.
        remapped[..envelope_count].copy_from_slice(&params[..envelope_count]);
    }

    remapped
}

/// Remap integer parameter arrays to 20-band target resolution.
///
/// ISO/IEC 14496-3:2009, 8.6.4.6.3 -- parameter remapping.
fn remap_to_20(
    params: &[[i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV],
    source_band_count: usize,
    envelope_count: usize,
    full_range: bool,
) -> [[i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV] {
    let mut remapped = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];

    if source_band_count == 34 || source_band_count == 17 {
        for env in 0..envelope_count {
            contract_34_to_20(&mut remapped[env], &params[env], full_range);
        }
    }
    else if source_band_count == 10 || source_band_count == 5 {
        for env in 0..envelope_count {
            expand_10_to_20(&mut remapped[env], &params[env], full_range);
        }
    }
    else {
        // Already at 20 bands -- copy unchanged.
        remapped[..envelope_count].copy_from_slice(&params[..envelope_count]);
    }

    remapped
}

// ---------------------------------------------------------------------------
// Stereo processing (ISO/IEC 14496-3:2009, Section 8.6.4.6.3)
// ---------------------------------------------------------------------------

/// Apply stereo mixing using IID/ICC (and optionally IPD/OPD) parameters.
///
/// Remaps parameters to the active band resolution, computes the stereo
/// mixing matrices H11/H12/H21/H22, and applies interpolated mixing:
/// `L[k] = h11*S[k] + h12*D[k]`, `R[k] = h21*S[k] + h22*D[k]`.
///
/// ISO/IEC 14496-3:2009, 8.6.4.6.3 -- stereo processing.
fn apply_stereo_mixing(ps: &mut PsContext, use_34bands: bool) {
    let tables = &*PS_TABLES;
    let mode_idx = use_34bands as usize;

    let subband_to_param = if use_34bands { &K_TO_I_34[..] } else { &K_TO_I_20[..] };
    let num_param_bands = NR_PAR_BANDS[mode_idx];
    let total_subbands = NR_BANDS[mode_idx];

    let ctx = &ps.common;

    // Select mixing matrix (HA for baseline, HB for alternative modes).
    // ISO/IEC 14496-3:2009, 8.6.4.6.3, Table 8.52.
    let mixing_lut = if ctx.icc_mode < 3 { &tables.ha } else { &tables.hb };

    // Carry forward the previous frame's final envelope H-values as the
    // starting point for interpolation in the current frame.
    if ctx.num_env_old > 0 {
        for component in 0..2 {
            ps.h11[component][0] = ps.h11[component][ctx.num_env_old];
            ps.h12[component][0] = ps.h12[component][ctx.num_env_old];
            ps.h21[component][0] = ps.h21[component][ctx.num_env_old];
            ps.h22[component][0] = ps.h22[component][ctx.num_env_old];
        }
    }

    // Remap the H-values when band resolution switches between frames.
    if use_34bands && !ctx.is34bands_old {
        for component in 0..2 {
            expand_val_20_to_34(&mut ps.h11[component][0]);
            expand_val_20_to_34(&mut ps.h12[component][0]);
            expand_val_20_to_34(&mut ps.h21[component][0]);
            expand_val_20_to_34(&mut ps.h22[component][0]);
        }
        ps.opd_hist = [0; PS_MAX_NR_IIDICC];
        ps.ipd_hist = [0; PS_MAX_NR_IIDICC];
    }
    else if !use_34bands && ctx.is34bands_old {
        for component in 0..2 {
            contract_val_34_to_20(&mut ps.h11[component][0]);
            contract_val_34_to_20(&mut ps.h12[component][0]);
            contract_val_34_to_20(&mut ps.h21[component][0]);
            contract_val_34_to_20(&mut ps.h22[component][0]);
        }
        ps.opd_hist = [0; PS_MAX_NR_IIDICC];
        ps.ipd_hist = [0; PS_MAX_NR_IIDICC];
    }

    // Remap decoded parameters to the current band resolution.
    let iid_remapped = if use_34bands {
        remap_to_34(&ctx.iid_par, ctx.nr_iid_par, ctx.num_env, true)
    }
    else {
        remap_to_20(&ctx.iid_par, ctx.nr_iid_par, ctx.num_env, true)
    };
    let icc_remapped = if use_34bands {
        remap_to_34(&ctx.icc_par, ctx.nr_icc_par, ctx.num_env, true)
    }
    else {
        remap_to_20(&ctx.icc_par, ctx.nr_icc_par, ctx.num_env, true)
    };
    let ipd_remapped = if use_34bands {
        remap_to_34(&ctx.ipd_par, ctx.nr_ipdopd_par, ctx.num_env, false)
    }
    else {
        remap_to_20(&ctx.ipd_par, ctx.nr_ipdopd_par, ctx.num_env, false)
    };
    let opd_remapped = if use_34bands {
        remap_to_34(&ctx.opd_par, ctx.nr_ipdopd_par, ctx.num_env, false)
    }
    else {
        remap_to_20(&ctx.opd_par, ctx.nr_ipdopd_par, ctx.num_env, false)
    };

    let envelope_count = ps.common.num_env;
    let quant_fine = ps.common.iid_quant;
    let phase_enabled = ps.common.enable_ipdopd;
    let num_phase_bands = NR_IPDOPD_BANDS[mode_idx];

    // Compute and apply the mixing matrix for each envelope.
    // ISO/IEC 14496-3:2009, 8.6.4.6.3 -- stereo mixing matrix computation.
    for env in 0..envelope_count {
        for band in 0..num_param_bands {
            // Clamp look-up indices to valid ranges.
            // Malformed bitstreams can produce arbitrary i8 values through
            // wrapping delta-coded parameters.
            let raw_iid_offset = iid_remapped[env][band] as i32 + 7 + 23 * quant_fine as i32;
            let clamped_iid = raw_iid_offset.clamp(0, 45) as usize;
            let clamped_icc = icc_remapped[env][band].clamp(0, 7) as usize;

            let lut_h11 = mixing_lut[clamped_iid][clamped_icc][0];
            let lut_h12 = mixing_lut[clamped_iid][clamped_icc][1];
            let lut_h21 = mixing_lut[clamped_iid][clamped_icc][2];
            let lut_h22 = mixing_lut[clamped_iid][clamped_icc][3];

            if phase_enabled && band < num_phase_bands {
                // IPD/OPD processing.
                // ISO/IEC 14496-3:2009, 8.6.4.6.3 -- phase parameters.
                let opd_idx =
                    (ps.opd_hist[band] as usize * 8 + opd_remapped[env][band] as usize) & 0x1FF;
                let ipd_idx =
                    (ps.ipd_hist[band] as usize * 8 + ipd_remapped[env][band] as usize) & 0x1FF;

                let opd_cos = tables.pd_re_smooth[opd_idx];
                let opd_sin = tables.pd_im_smooth[opd_idx];
                let ipd_cos = tables.pd_re_smooth[ipd_idx];
                let ipd_sin = tables.pd_im_smooth[ipd_idx];

                ps.opd_hist[band] = (opd_idx & 0x3F) as i8;
                ps.ipd_hist[band] = (ipd_idx & 0x3F) as i8;

                let adj_cos = opd_cos * ipd_cos + opd_sin * ipd_sin;
                let adj_sin = opd_sin * ipd_cos - opd_cos * ipd_sin;

                ps.h11[1][env + 1][band] = lut_h11 * opd_sin;
                ps.h11[0][env + 1][band] = lut_h11 * opd_cos;
                ps.h12[1][env + 1][band] = lut_h12 * adj_sin;
                ps.h12[0][env + 1][band] = lut_h12 * adj_cos;
                ps.h21[1][env + 1][band] = lut_h21 * opd_sin;
                ps.h21[0][env + 1][band] = lut_h21 * opd_cos;
                ps.h22[1][env + 1][band] = lut_h22 * adj_sin;
                ps.h22[0][env + 1][band] = lut_h22 * adj_cos;
            }
            else {
                ps.h11[0][env + 1][band] = lut_h11;
                ps.h12[0][env + 1][band] = lut_h12;
                ps.h21[0][env + 1][band] = lut_h21;
                ps.h22[0][env + 1][band] = lut_h22;
                ps.h11[1][env + 1][band] = 0.0;
                ps.h12[1][env + 1][band] = 0.0;
                ps.h21[1][env + 1][band] = 0.0;
                ps.h22[1][env + 1][band] = 0.0;
            }
        }

        // Interpolate mixing coefficients across each envelope and apply mixing.
        // ISO/IEC 14496-3:2009, 8.6.4.6.3 -- stereo mixing with linear interpolation.
        let borders = &ps.common.border_position;
        for subband in 0..total_subbands {
            let band = subband_to_param[subband];
            let env_start = (borders[env] + 1) as usize;
            let env_stop = (borders[env + 1] + 1) as usize;
            let span = if env_stop > env_start { env_stop - env_start } else { 1 };
            let inv_span = 1.0 / span as f32;

            // Current interpolated values for left: (signal, decorr),
            // and right: (signal, decorr).
            let mut left_sig = ps.h11[0][env][band];
            let mut left_dec = ps.h21[0][env][band];
            let mut right_sig = ps.h12[0][env][band];
            let mut right_dec = ps.h22[0][env][band];

            // Per-sample increment.
            let left_sig_step = (ps.h11[0][env + 1][band] - left_sig) * inv_span;
            let left_dec_step = (ps.h21[0][env + 1][band] - left_dec) * inv_span;
            let right_sig_step = (ps.h12[0][env + 1][band] - right_sig) * inv_span;
            let right_dec_step = (ps.h22[0][env + 1][band] - right_dec) * inv_span;

            if phase_enabled {
                // Complex mixing with IPD/OPD phase rotation.
                let mut left_sig_im = ps.h11[1][env][band];
                let mut left_dec_im = ps.h21[1][env][band];
                let mut right_sig_im = ps.h12[1][env][band];
                let mut right_dec_im = ps.h22[1][env][band];

                let left_sig_im_step = (ps.h11[1][env + 1][band] - left_sig_im) * inv_span;
                let left_dec_im_step = (ps.h21[1][env + 1][band] - left_dec_im) * inv_span;
                let right_sig_im_step = (ps.h12[1][env + 1][band] - right_sig_im) * inv_span;
                let right_dec_im_step = (ps.h22[1][env + 1][band] - right_dec_im) * inv_span;

                // Certain subbands negate the imaginary mixing component per the spec.
                let negate_phase =
                    if use_34bands { subband >= 9 && subband <= 13 } else { subband <= 1 };

                for slot in env_start..env_stop {
                    if slot >= 32 {
                        break;
                    }
                    let mono_re = ps.l_buf[subband][slot][0];
                    let mono_im = ps.l_buf[subband][slot][1];
                    let side_re = ps.r_buf[subband][slot][0];
                    let side_im = ps.r_buf[subband][slot][1];

                    left_sig += left_sig_step;
                    left_dec += left_dec_step;
                    right_sig += right_sig_step;
                    right_dec += right_dec_step;
                    left_sig_im += left_sig_im_step;
                    left_dec_im += left_dec_im_step;
                    right_sig_im += right_sig_im_step;
                    right_dec_im += right_dec_im_step;

                    let (ls_im, ld_im, rs_im, rd_im) = if negate_phase {
                        (-left_sig_im, -left_dec_im, -right_sig_im, -right_dec_im)
                    }
                    else {
                        (left_sig_im, left_dec_im, right_sig_im, right_dec_im)
                    };

                    ps.l_buf[subband][slot][0] =
                        left_sig * mono_re + left_dec * side_re - ls_im * mono_im - ld_im * side_im;
                    ps.l_buf[subband][slot][1] =
                        left_sig * mono_im + left_dec * side_im + ls_im * mono_re + ld_im * side_re;
                    ps.r_buf[subband][slot][0] = right_sig * mono_re + right_dec * side_re
                        - rs_im * mono_im
                        - rd_im * side_im;
                    ps.r_buf[subband][slot][1] = right_sig * mono_im
                        + right_dec * side_im
                        + rs_im * mono_re
                        + rd_im * side_re;
                }
            }
            else {
                // Real-only mixing (no IPD/OPD).
                for slot in env_start..env_stop {
                    if slot >= 32 {
                        break;
                    }
                    let mono_re = ps.l_buf[subband][slot][0];
                    let mono_im = ps.l_buf[subband][slot][1];
                    let side_re = ps.r_buf[subband][slot][0];
                    let side_im = ps.r_buf[subband][slot][1];

                    left_sig += left_sig_step;
                    left_dec += left_dec_step;
                    right_sig += right_sig_step;
                    right_dec += right_dec_step;

                    ps.l_buf[subband][slot][0] = left_sig * mono_re + left_dec * side_re;
                    ps.l_buf[subband][slot][1] = left_sig * mono_im + left_dec * side_im;
                    ps.r_buf[subband][slot][0] = right_sig * mono_re + right_dec * side_re;
                    ps.r_buf[subband][slot][1] = right_sig * mono_im + right_dec * side_im;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Apply Parametric Stereo processing.
///
/// Implements the full PS decoding pipeline per ISO/IEC 14496-3:2009, 8.6.4.6:
/// hybrid analysis -> decorrelation -> stereo processing -> hybrid synthesis.
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
    let use_34bands = ps.common.is34bands;
    let mode_idx = use_34bands as usize;
    let num_slots: usize = num_qmf_slots;

    // Zero out delay lines for unused bands above the SBR top frequency.
    let total_subbands = NR_BANDS[mode_idx];
    let top_hybrid = top + total_subbands - 64;
    for subband in top_hybrid..total_subbands {
        ps.delay[subband] = [[0.0; 2]; PS_QMF_TIME_SLOTS + PS_MAX_DELAY];
    }
    if top_hybrid < NR_ALLPASS_BANDS[mode_idx] {
        for subband in top_hybrid..NR_ALLPASS_BANDS[mode_idx] {
            ps.ap_delay[subband] = [[[0.0; 2]; PS_QMF_TIME_SLOTS + PS_MAX_AP_DELAY]; PS_AP_LINKS];
        }
    }

    // Step 1: Hybrid analysis -- decompose mono signal into hybrid sub-subbands.
    analyze_hybrid(&mut ps.l_buf, &mut ps.in_buf, l, use_34bands, num_slots);

    // Step 2: Decorrelation -- generate anticorrelated side signal.
    generate_decorrelated_signal(ps, use_34bands, num_slots);

    // Step 3: Stereo processing -- mix mono and decorrelated using IID/ICC.
    apply_stereo_mixing(ps, use_34bands);

    // Step 4: Hybrid synthesis -- merge back to QMF domain for left and right.
    synthesize_hybrid(l, &ps.l_buf, use_34bands, num_slots);
    synthesize_hybrid(r, &ps.r_buf, use_34bands, num_slots);
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
        // PsContext allocates large buffers on heap -- verify it doesn't panic.
        let ctx = PsContext::new();
        assert!(!ctx.common.start);
        assert_eq!(ctx.peak_decay_nrg, [0.0; 34]);
        assert_eq!(ctx.power_smooth, [0.0; 34]);
        assert_eq!(ctx.opd_hist, [0; PS_MAX_NR_IIDICC]);
        assert_eq!(ctx.ipd_hist, [0; PS_MAX_NR_IIDICC]);
    }

    #[test]
    fn verify_expand_10_to_20_full() {
        let mut src = [0i8; PS_MAX_NR_IIDICC];
        for i in 0..10 {
            src[i] = (i + 1) as i8;
        }

        let mut dst = [0i8; PS_MAX_NR_IIDICC];
        expand_10_to_20(&mut dst, &src, true);

        // Each 10-band value should be duplicated into two 20-band values.
        for b in 0..10 {
            assert_eq!(dst[2 * b], src[b], "expand_10_to_20 even[{}]", b);
            assert_eq!(dst[2 * b + 1], src[b], "expand_10_to_20 odd[{}]", b);
        }
    }

    #[test]
    fn verify_expand_10_to_20_partial() {
        let mut src = [0i8; PS_MAX_NR_IIDICC];
        for i in 0..5 {
            src[i] = (i + 1) as i8;
        }

        let mut dst = [0i8; PS_MAX_NR_IIDICC];
        expand_10_to_20(&mut dst, &src, false);

        // Only first 5 bands are duplicated, band 10 is set to 0.
        for b in 0..5 {
            assert_eq!(dst[2 * b], src[b]);
            assert_eq!(dst[2 * b + 1], src[b]);
        }
        assert_eq!(dst[10], 0);
    }

    #[test]
    fn verify_expand_10_to_34_full() {
        let mut src = [0i8; PS_MAX_NR_IIDICC];
        for i in 0..10 {
            src[i] = (i + 1) as i8;
        }

        let mut dst = [0i8; PS_MAX_NR_IIDICC];
        expand_10_to_34(&mut dst, &src, true);

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
    fn verify_expand_20_to_34_full() {
        let mut src = [0i8; PS_MAX_NR_IIDICC];
        for i in 0..20 {
            src[i] = (i + 1) as i8;
        }

        let mut dst = [0i8; PS_MAX_NR_IIDICC];
        expand_20_to_34(&mut dst, &src, true);

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
    fn verify_contract_34_to_20_full() {
        let mut src = [0i8; PS_MAX_NR_IIDICC];
        for i in 0..34 {
            src[i] = (i + 1) as i8;
        }

        let mut dst = [0i8; PS_MAX_NR_IIDICC];
        contract_34_to_20(&mut dst, &src, true);

        // Band 0 is (2*par[0] + par[1])/3.
        let expected = ((2i16 * src[0] as i16 + src[1] as i16) / 3) as i8;
        assert_eq!(dst[0], expected);
        // Band 6 is par[10].
        assert_eq!(dst[6], src[10]);
        // Band 7 is par[11].
        assert_eq!(dst[7], src[11]);
    }

    #[test]
    fn verify_remap_to_34_passthrough_34bands() {
        let mut params = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        for env in 0..2 {
            for band in 0..34 {
                params[env][band] = (band as i8) + (env as i8 * 10);
            }
        }

        let result = remap_to_34(&params, 34, 2, true);

        // 34->34 should be identity copy.
        for env in 0..2 {
            for band in 0..34 {
                assert_eq!(result[env][band], params[env][band]);
            }
        }
    }

    #[test]
    fn verify_remap_to_20_passthrough_20bands() {
        let mut params = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        for env in 0..2 {
            for band in 0..20 {
                params[env][band] = (band as i8) + (env as i8 * 10);
            }
        }

        let result = remap_to_20(&params, 20, 2, true);

        // 20->20 should be identity copy.
        for env in 0..2 {
            for band in 0..20 {
                assert_eq!(result[env][band], params[env][band]);
            }
        }
    }

    #[test]
    fn verify_remap_to_34_from_10bands() {
        let mut params = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        for band in 0..10 {
            params[0][band] = (band + 1) as i8;
        }

        let result = remap_to_34(&params, 10, 1, true);

        // Low bands should use expand_10_to_34 values.
        assert_eq!(result[0][0], params[0][0]); // par[0] -> bands 0-2
        assert_eq!(result[0][1], params[0][0]);
        assert_eq!(result[0][2], params[0][0]);
    }

    #[test]
    fn verify_remap_to_20_from_34bands() {
        let mut params = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        for band in 0..34 {
            params[0][band] = (band + 1) as i8;
        }

        let result = remap_to_20(&params, 34, 1, true);

        // Should use contract_34_to_20.
        let expected_6 = params[0][10]; // Band 6 = par[10]
        assert_eq!(result[0][6], expected_6);
    }

    #[test]
    fn verify_expand_val_20_to_34() {
        let mut values = [0.0f32; PS_MAX_NR_IIDICC];
        for i in 0..20 {
            values[i] = (i + 1) as f32;
        }

        expand_val_20_to_34(&mut values);

        // Band 0 should remain par[0] = 1.0.
        assert_eq!(values[0], 1.0);
        // Band 1 is average of original par[0] and par[1] = (1+2)/2 = 1.5.
        assert!((values[1] - 1.5).abs() < 1e-6);
        // Last two bands should equal original par[19] = 20.0.
        assert_eq!(values[32], 20.0);
        assert_eq!(values[33], 20.0);
    }

    #[test]
    fn verify_contract_val_34_to_20() {
        let mut values = [0.0f32; PS_MAX_NR_IIDICC];
        for i in 0..34 {
            values[i] = (i + 1) as f32;
        }

        contract_val_34_to_20(&mut values);

        // Band 6 = par[10] = 11.0.
        assert_eq!(values[6], 11.0);
        // Band 7 = par[11] = 12.0.
        assert_eq!(values[7], 12.0);
        // Band 0 = (2*1 + 2)/3 = 4/3 ~ 1.333.
        assert!((values[0] - 4.0 / 3.0).abs() < 1e-5);
    }

    #[test]
    fn verify_synthesize_hybrid_20band_sums_subbands() {
        let mut hybrid_buf = Box::new([[[0.0f32; 2]; 32]; PS_MAX_SSB]);

        // Set hybrid subbands for QMF band 0 (6 subbands: indices 0-5).
        for sub in 0..6 {
            hybrid_buf[sub][0][0] = 1.0; // re
            hybrid_buf[sub][0][1] = 0.5; // im
        }

        let mut qmf_out = [[[0.0f32; 64]; 38]; 2];
        synthesize_hybrid(&mut qmf_out, &hybrid_buf, false, 1);

        // QMF band 0 should be the sum of 6 hybrid subbands.
        assert!((qmf_out[0][0][0] - 6.0).abs() < 1e-6, "QMF 0 re should be 6.0");
        assert!((qmf_out[1][0][0] - 3.0).abs() < 1e-6, "QMF 0 im should be 3.0");
    }

    #[test]
    fn verify_synthesize_hybrid_20band_direct_copy() {
        let mut hybrid_buf = Box::new([[[0.0f32; 2]; 32]; PS_MAX_SSB]);

        // Set QMF band 3 (hybrid index = 3 + 7 = 10) to known values.
        hybrid_buf[10][0][0] = 42.0;
        hybrid_buf[10][0][1] = -7.5;

        let mut qmf_out = [[[0.0f32; 64]; 38]; 2];
        synthesize_hybrid(&mut qmf_out, &hybrid_buf, false, 1);

        // QMF band 3 should be directly copied from hybrid index 10.
        assert!((qmf_out[0][0][3] - 42.0).abs() < 1e-6);
        assert!((qmf_out[1][0][3] - (-7.5)).abs() < 1e-6);
    }

    #[test]
    fn verify_synthesize_hybrid_34band_sums_subbands() {
        let mut hybrid_buf = Box::new([[[0.0f32; 2]; 32]; PS_MAX_SSB]);

        // QMF band 0: 12 hybrid subbands (indices 0-11).
        for sub in 0..12 {
            hybrid_buf[sub][0][0] = 1.0;
            hybrid_buf[sub][0][1] = 2.0;
        }

        let mut qmf_out = [[[0.0f32; 64]; 38]; 2];
        synthesize_hybrid(&mut qmf_out, &hybrid_buf, true, 1);

        assert!((qmf_out[0][0][0] - 12.0).abs() < 1e-6, "QMF 0 re should be 12.0");
        assert!((qmf_out[1][0][0] - 24.0).abs() < 1e-6, "QMF 0 im should be 24.0");
    }

    #[test]
    fn verify_synthesize_hybrid_34band_qmf1_sum() {
        let mut hybrid_buf = Box::new([[[0.0f32; 2]; 32]; PS_MAX_SSB]);

        // QMF band 1: 8 hybrid subbands (indices 12-19).
        for sub in 12..20 {
            hybrid_buf[sub][0][0] = 0.5;
            hybrid_buf[sub][0][1] = -0.25;
        }

        let mut qmf_out = [[[0.0f32; 64]; 38]; 2];
        synthesize_hybrid(&mut qmf_out, &hybrid_buf, true, 1);

        assert!((qmf_out[0][0][1] - 4.0).abs() < 1e-6, "QMF 1 re should be 4.0");
        assert!((qmf_out[1][0][1] - (-2.0)).abs() < 1e-6, "QMF 1 im should be -2.0");
    }

    #[test]
    fn verify_synthesize_hybrid_34band_direct_copy() {
        let mut hybrid_buf = Box::new([[[0.0f32; 2]; 32]; PS_MAX_SSB]);

        // QMF band 5 in 34-band mode: hybrid index = 5 + 27 = 32.
        hybrid_buf[32][0][0] = 99.0;
        hybrid_buf[32][0][1] = -1.0;

        let mut qmf_out = [[[0.0f32; 64]; 38]; 2];
        synthesize_hybrid(&mut qmf_out, &hybrid_buf, true, 1);

        assert!((qmf_out[0][0][5] - 99.0).abs() < 1e-6);
        assert!((qmf_out[1][0][5] - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn verify_split_2band_real_produces_output() {
        // Feed a DC signal through the 2-band real hybrid filter.
        let mut input = [[0.0f32; 2]; 44];
        for i in 0..13 {
            input[i][0] = 1.0; // re = 1.0
        }

        let mut hybrid_out = Box::new([[[0.0f32; 2]; 32]; PS_MAX_SSB]);
        split_2band_real(&input, &mut hybrid_out, 0, 1, &G1_Q2, 1);

        // With DC input, band 0 (lowpass) should have energy,
        // band 1 (highpass) should have less or zero for this filter.
        let sum_sq_0 =
            hybrid_out[0][0][0] * hybrid_out[0][0][0] + hybrid_out[0][0][1] * hybrid_out[0][0][1];
        // Just verify it produces non-trivial output.
        assert!(
            sum_sq_0 > 0.0 || hybrid_out[1][0][0].abs() > 0.0,
            "split_2band_real should produce output"
        );
    }

    #[test]
    fn verify_decorrelation_zero_input_zero_output() {
        let mut ps = PsContext::new();
        ps.common.is34bands = false;
        ps.common.is34bands_old = false;
        ps.common.num_env = 1;

        // Zero input in l_buf -> decorrelation should produce zero in r_buf.
        generate_decorrelated_signal(&mut ps, false, PS_QMF_TIME_SLOTS);

        for subband in 0..NR_BANDS[0] {
            for slot in 0..PS_QMF_TIME_SLOTS {
                assert!(
                    ps.r_buf[subband][slot][0].abs() < 1e-10,
                    "r_buf[{}][{}][0] should be zero for zero input",
                    subband,
                    slot
                );
                assert!(
                    ps.r_buf[subband][slot][1].abs() < 1e-10,
                    "r_buf[{}][{}][1] should be zero for zero input",
                    subband,
                    slot
                );
            }
        }
    }
}
