// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SBR HF generation and HF adjustment (spectral envelope shaping).
//!
//! Implements the core SBR signal processing pipeline:
//!   - QMF analysis/synthesis wrappers
//!   - HF generation via inverse filtering and patching (ISO/IEC 14496-3, 4.6.18.6)
//!   - HF adjustment: spectral envelope mapping, gain computation,
//!     noise floor insertion, and sinusoidal addition (ISO/IEC 14496-3, 4.6.18.7)

use symphonia_core::dsp::complex::Complex;

use super::dsp::{SbrAnalysis, SbrDsp, SbrSynthesis};
use super::tables;
use super::{
    sq_modulus, FrameClass, QuantMode, SbrChannel, SbrHeader, SbrState, HF_ADJ, MAX_SLOTS,
    NUM_ENVELOPES, QMF_DELAY, SBR_BANDS, SMOOTH_DELAY,
};

const ZERO: Complex = Complex { re: 0.0, im: 0.0 };

/// Limiter gain ceiling per bs_limiter_gains index (ISO/IEC 14496-3, 4.6.18.7.3).
const LIMITER_GAINS: [f32; 4] = [0.70795, 1.0, 1.41254, 10000.0];

/// Smoothing filter coefficients h_smooth[i], i = 0..4 (ISO/IEC 14496-3, 4.6.18.7.3).
#[rustfmt::skip]
const SMOOTH_COEFS: [f32; 5] = [
    1.0 / 3.0,
    0.30150283239582,
    0.21816949906249,
    0.11516383427084,
    0.03183050093751,
];

/// Chirp control factor lookup indexed by [old_mode][new_mode]
/// (ISO/IEC 14496-3, Table 4.158).
#[rustfmt::skip]
const CHIRP_COEF: [[f32; 4]; 4] = [
    [0.0,  0.6,  0.9,  0.98],
    [0.6,  0.75, 0.9,  0.98],
    [0.0,  0.75, 0.9,  0.98],
    [0.0,  0.75, 0.9,  0.98],
];

/// Phase rotation vectors for sinusoidal addition, indexed by (index_sine & 3).
const SINE_PHASE: [Complex; 4] = [
    Complex { re: 1.0, im: 0.0 },
    Complex { re: 0.0, im: 1.0 },
    Complex { re: -1.0, im: 0.0 },
    Complex { re: 0.0, im: -1.0 },
];

/// Energy computation epsilon to avoid division by zero.
const EPSILON: f32 = 1.0;
/// Tiny epsilon for summation stability.
const EPSILON_0: f32 = 1.0e-12;
/// Energy range scaling factor 2^16.
const E_RANGE: f32 = 65536.0;

// ---------------------------------------------------------------------------
// Public interface: wrappers around analysis/synthesis/processing
// ---------------------------------------------------------------------------

/// Feed core codec samples through the 32-band QMF analysis filterbank.
pub fn analysis(ch: &mut SbrChannel, sbr_a: &mut SbrAnalysis, dsp: &mut SbrDsp, src: &[f32]) {
    for (chunk, w_slot) in src.chunks(32).zip(ch.w[QMF_DELAY..].iter_mut()) {
        sbr_a.analysis(dsp, chunk, w_slot);
    }
}

/// Reconstruct time-domain output from QMF subbands via the 64-band synthesis filterbank.
pub fn synthesis(ch: &mut SbrChannel, sbr_s: &mut SbrSynthesis, dsp: &mut SbrDsp, dst: &mut [f32]) {
    for (x_slot, out_chunk) in ch.x.iter_mut().zip(dst.chunks_mut(64)) {
        sbr_s.synthesis(dsp, x_slot, out_chunk);
    }
}

/// Bypass mode: copy analysis subbands directly to synthesis input.
pub fn bypass(ch: &mut SbrChannel, num_time_slots: usize) {
    let n_qmf = (num_time_slots * 2).min(ch.x.len());
    for t in 0..n_qmf {
        if QMF_DELAY + t < ch.w.len() {
            ch.x[t] = ch.w[QMF_DELAY + t];
        }
    }
}

/// Copy low-frequency subbands (below k_x) into the synthesis input buffer.
pub fn x_gen(ch: &mut SbrChannel, state: &SbrState, num_time_slots: usize) {
    let n_qmf = (num_time_slots * 2).min(ch.x.len());
    let kx = state.k_x.min(SBR_BANDS);
    for t in 0..n_qmf {
        if QMF_DELAY + t < ch.w.len() {
            ch.x[t][..kx].copy_from_slice(&ch.w[QMF_DELAY + t][..kx]);
        }
    }
}

// ---------------------------------------------------------------------------
// HF generation (ISO/IEC 14496-3, 4.6.18.6)
// ---------------------------------------------------------------------------

/// Generate high-frequency QMF subbands from low-frequency content via patching.
pub fn hf_generate(ch: &mut SbrChannel, state: &SbrState, num_time_slots: usize) {
    let kx = state.k_x.min(SBR_BANDS);

    // Copy low-band subbands and clear high-band.
    for (x_slot, w_slot) in ch.x.iter_mut().zip(ch.w.iter()) {
        x_slot[..kx].copy_from_slice(&w_slot[..kx]);
        for v in x_slot[kx..].iter_mut() {
            *v = ZERO;
        }
    }

    // Compute auto/cross-correlation matrices for inverse filtering (4.6.18.6.2).
    let k0 = state.f[0].min(SBR_BANDS);
    let n_slots = num_time_slots * 2;
    let mut phi = [[[ZERO; SBR_BANDS]; 3]; 3];

    for lag_a in 0..3u8 {
        for lag_b in (lag_a + 1)..3u8 {
            let start_a = HF_ADJ - lag_a as usize;
            let start_b = HF_ADJ - lag_b as usize;
            let len = (n_slots + 6 - 1).min(ch.x.len().saturating_sub(HF_ADJ));
            for t in 0..len {
                if start_a + t >= ch.x.len() || start_b + t >= ch.x.len() {
                    break;
                }
                for k in 0..k0 {
                    phi[lag_a as usize][lag_b as usize][k] +=
                        ch.x[start_a + t][k] * ch.x[start_b + t][k].conj();
                }
            }
        }
    }

    // Solve for prediction coefficients a0, a1 per subband.
    let mut coef_a0 = [ZERO; SBR_BANDS];
    let mut coef_a1 = [ZERO; SBR_BANDS];

    for k in 0..k0 {
        let phi_12 = phi[1][2][k];
        let phi_11_re = phi[1][1][k].re;
        let phi_22_re = phi[2][2][k].re;

        let det = phi_22_re * phi_11_re - sq_modulus(phi_12) / (1.0 + 1.0e-6);
        if det != 0.0 {
            coef_a1[k] = (phi[0][1][k] * phi_12 - phi[0][2][k] * phi_11_re).scale(1.0 / det);
        }
        if phi_11_re != 0.0 {
            coef_a0[k] = (phi[0][1][k] + coef_a1[k] * phi_12.conj()).scale(-1.0 / phi_11_re);
        }
        // Stability check.
        if sq_modulus(coef_a0[k]) >= 16.0 || sq_modulus(coef_a1[k]) >= 16.0 {
            coef_a0[k] = ZERO;
            coef_a1[k] = ZERO;
        }
    }

    // Update chirp control factors with smoothing (4.6.18.6.3).
    for q in 0..state.num_noise_bands {
        let target = CHIRP_COEF[ch.old_invf_mode[q] as usize][ch.invf_mode[q] as usize];
        let prev = ch.old_bw_array[q];
        let smoothed = if target < prev {
            0.75 * target + 0.25 * prev
        }
        else {
            0.90625 * target + 0.09375 * prev
        };
        ch.bw_array[q] = if smoothed >= 0.015625 { smoothed } else { 0.0 };
    }

    // Apply inverse filtering and patching to produce X_high.
    let num_env = ch.num_env.min(NUM_ENVELOPES);
    let t_start = HF_ADJ + ch.env_border[0] * 2;
    let t_end = (HF_ADJ + ch.env_border[num_env] * 2).min(ch.x_high.len());

    for t in t_start..t_end {
        ch.x_high[t] = [ZERO; SBR_BANDS];
        let mut dst_band = kx;

        for p in 0..state.num_patches {
            let p_start = state.patch_start_subband[p];
            let p_width = state.patch_num_subbands[p];

            for j in 0..p_width {
                let src_band = p_start + j;
                let out_band = dst_band + j;
                if src_band >= SBR_BANDS || out_band >= SBR_BANDS || t < 2 {
                    continue;
                }

                // Find the noise band index for this output subband.
                let n_noise = state.num_noise_bands.max(1);
                let q_idx = match state.f_noise[..n_noise].binary_search(&out_band) {
                    Ok(i) => i,
                    Err(i) => i.saturating_sub(1).min(n_noise - 1),
                };
                let bw = ch.bw_array[q_idx];

                // Filtered prediction: X_high = X + a0·bw·X[-1] + a1·bw²·X[-2]
                ch.x_high[t][out_band] = ch.x[t][src_band]
                    + coef_a0[src_band].scale(bw) * ch.x[t - 1][src_band]
                    + coef_a1[src_band].scale(bw * bw) * ch.x[t - 2][src_band];
            }
            dst_band += p_width;
        }
    }
}

// ---------------------------------------------------------------------------
// HF adjustment (ISO/IEC 14496-3, 4.6.18.7)
// ---------------------------------------------------------------------------

/// Apply spectral envelope shaping, noise floor, and sinusoidal components.
pub fn hf_adjust(ch: &mut SbrChannel, state: &SbrState, hdr: &SbrHeader, num_time_slots: usize) {
    let kx = state.k_x.min(SBR_BANDS);
    let k_upper = state.f[state.num_master.min(SBR_BANDS - 1)].min(SBR_BANDS);
    let t_env_start = ch.env_border[0];
    let t_env_end = ch.env_border[ch.num_env.min(NUM_ENVELOPES)];

    // Frequency band tables for high and low resolution.
    let hi_offset = state.f[..=state.num_master].binary_search(&kx).unwrap_or(0);
    let f_hi = &state.f[..=state.num_master][hi_offset..];
    let f_lo = &state.f_low[..=state.num_env_bands[0]];

    // Determine l_A (last envelope in an attack transition).
    let l_a: i8 = compute_l_a(ch);

    // Map sinusoidal coding flags to per-subband per-envelope.
    compute_sine_mapping(ch, f_hi, l_a);

    // Build s_mapped: whether any sinusoidal is active in each scalefactor band.
    let mut s_mapped = [[false; SBR_BANDS]; NUM_ENVELOPES];
    for env in 0..ch.num_env {
        let mut band_lo = kx;
        if ch.freq_res[env] {
            for (i, &band_hi) in f_hi[1..].iter().enumerate() {
                let active = ch.add_harmonic[i];
                for b in band_lo..band_hi {
                    s_mapped[env][b] = active;
                }
                band_lo = band_hi;
            }
        }
        else {
            for &band_hi in f_lo[1..].iter() {
                let active = ch.s_idx_mapped[env][band_lo..band_hi].contains(&true);
                for b in band_lo..band_hi {
                    s_mapped[env][b] = active;
                }
                band_lo = band_hi;
            }
        }
    }

    // Dequantize envelope scalefactors to linear energy E_orig (4.6.18.7.2).
    let mut e_orig = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];
    let (step, pan_mid) = if ch.amp_res { (1.0f32, 12.0f32) } else { (0.5, 24.0) };
    dequant_envelope(&mut e_orig, ch, f_hi, f_lo, kx, step, pan_mid);

    // Dequantize noise floor scalefactors to linear energy Q_orig.
    let mut q_orig = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];
    dequant_noise(&mut q_orig, ch, state);

    // Measure current HF energy E_curr per envelope (4.6.18.7.3).
    let mut e_curr = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];
    let mut t_lo = t_env_start;
    for (env, &t_hi) in ch.env_border[1..=ch.num_env].iter().enumerate() {
        let n_slots = ((t_hi - t_lo) * 2).max(1) as f32;
        let scale = E_RANGE * E_RANGE / n_slots;
        let hi_start = (HF_ADJ + t_lo * 2).min(ch.x_high.len());
        let hi_end = (HF_ADJ + t_hi * 2).min(ch.x_high.len());
        for slot in &ch.x_high[hi_start..hi_end] {
            for k in kx..k_upper {
                e_curr[env][k] += sq_modulus(slot[k]);
            }
        }
        for k in kx..k_upper {
            e_curr[env][k] *= scale;
        }
        t_lo = t_hi;
    }

    // Gain computation (4.6.18.7.3).
    let la_prev: i8 = if ch.prev_l_a == ch.prev_num_env as i8 { 0 } else { -1 };
    let g_lim_max = LIMITER_GAINS[hdr.limiter_gains as usize];

    let mut g_boost_env = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];
    let mut q_boost_env = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];
    let mut s_boost_env = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];

    for env in 0..ch.num_env {
        let mut g_raw = [0.0f32; SBR_BANDS];
        let mut q_m = [0.0f32; SBR_BANDS];
        let mut s_m = [0.0f32; SBR_BANDS];
        let mut g_limited = [0.0f32; SBR_BANDS];
        let mut q_limited = [0.0f32; SBR_BANDS];

        // Compute per-limiter-band maximum gain.
        let mut g_max = [0.0f32; SBR_BANDS];
        let mut lim_lo = kx;
        for lim in 0..state.num_lim {
            let lim_hi = state.f_lim[lim + 1];
            let mut sum_orig = EPSILON_0;
            let mut sum_curr = EPSILON_0;
            for k in lim_lo..lim_hi {
                sum_orig += e_orig[env][k];
                sum_curr += e_curr[env][k];
            }
            let val = (sum_orig / sum_curr).sqrt() * g_lim_max;
            for k in lim_lo..lim_hi {
                g_max[k] = val.min(1.0e5);
            }
            lim_lo = lim_hi;
        }

        // Per-subband gain, noise component, and sinusoidal component.
        for k in kx..k_upper {
            let eo = e_orig[env][k];
            let qo = q_orig[env][k];
            let ec = e_curr[env][k];

            q_m[k] = (eo * qo / (1.0 + qo)).sqrt();
            s_m[k] = if ch.s_idx_mapped[env][k] { (eo / (1.0 + qo)).sqrt() } else { 0.0 };

            g_raw[k] = if !s_mapped[env][k] {
                let q_add = if env as i8 != l_a && env as i8 != la_prev { qo } else { 0.0 };
                (eo / ((EPSILON + ec) * (1.0 + q_add))).sqrt()
            }
            else {
                (eo / (EPSILON + ec) * qo / (1.0 + qo)).sqrt()
            };

            // Apply limiter ceiling.
            g_limited[k] = g_raw[k].min(g_max[k]);
            q_limited[k] = if g_raw[k] > EPSILON_0 {
                q_m[k].min(q_m[k] * g_max[k] / g_raw[k])
            }
            else {
                q_m[k]
            };
        }

        // Compensatory gain boost per limiter band.
        lim_lo = kx;
        for lim in 0..state.num_lim {
            let lim_hi = state.f_lim[lim + 1];
            let mut numer = EPSILON_0;
            let mut denom = EPSILON_0;
            for k in lim_lo..lim_hi {
                numer += e_orig[env][k];
                denom += e_curr[env][k] * g_limited[k] * g_limited[k];
                if s_m[k] != 0.0 || env as i8 == l_a || env as i8 == la_prev {
                    denom += s_m[k] * s_m[k];
                }
                else {
                    denom += q_limited[k] * q_limited[k];
                }
            }
            // g_boost clamped to 10^(1/5) ≈ 1.584893 (ISO limit).
            let g_boost = (numer / denom).sqrt().min(1.584893192);
            for k in lim_lo..lim_hi {
                g_boost_env[env][k] = g_limited[k] * g_boost;
                q_boost_env[env][k] = q_limited[k] * g_boost;
                s_boost_env[env][k] = s_m[k] * g_boost;
            }
            lim_lo = lim_hi;
        }
    }

    // Map QMF time slots to envelope indices.
    let mut slot_to_env = [0usize; MAX_SLOTS * 2 + QMF_DELAY];
    {
        let mut t_lo = t_env_start;
        for (env, &t_hi) in ch.env_border[1..=ch.num_env].iter().enumerate() {
            for t in (t_lo * 2)..(t_hi * 2) {
                slot_to_env[t] = env;
            }
            t_lo = t_hi;
        }
    }

    // Fill gain/noise interpolation buffers (g_temp, q_temp) for smoothing.
    populate_smooth_buffers(ch, &g_boost_env, &q_boost_env);

    // Apply smoothing filter or direct gains.
    let mut g_filt = [[0.0f32; SBR_BANDS]; MAX_SLOTS * 2 + QMF_DELAY];
    let mut q_filt = [[0.0f32; SBR_BANDS]; MAX_SLOTS * 2 + QMF_DELAY];

    let filt_slots = g_filt.len();
    let temp_slots = ch.g_temp.len();
    if !hdr.smoothing_mode {
        // 5-tap FIR smoothing (4.6.18.7.3).
        for t in (t_env_start * 2)..(t_env_end * 2).min(filt_slots) {
            let ti = t + SMOOTH_DELAY;
            if ti >= temp_slots {
                break;
            }
            if t as i8 == la_prev * 2 {
                g_filt[t].copy_from_slice(&ch.g_temp[ti]);
                q_filt[t].copy_from_slice(&ch.q_temp[ti]);
                continue;
            }
            for k in kx..k_upper {
                let mut gs = 0.0f32;
                let mut qs = 0.0f32;
                for (tap, &coef) in SMOOTH_COEFS.iter().enumerate() {
                    gs += ch.g_temp[ti - tap][k] * coef;
                    qs += ch.q_temp[ti - tap][k] * coef;
                }
                g_filt[t][k] = gs;
                q_filt[t][k] = qs;
            }
        }
    }
    else {
        // No smoothing: use gain values directly.
        let src = &ch.g_temp[SMOOTH_DELAY..];
        g_filt[..src.len()].copy_from_slice(src);
        let src = &ch.q_temp[SMOOTH_DELAY..];
        q_filt[..src.len()].copy_from_slice(src);
    }

    // Assemble final output: scaled HF + noise or sinusoidal (4.6.18.7.5).
    let noise_base = ch.index_noise.wrapping_sub(t_env_start * 2) & 511;
    for t in (t_env_start * 2)..(t_env_end * 2) {
        let y = &mut ch.y[HF_ADJ + t];
        let env = slot_to_env[t];
        for k in kx..k_upper {
            // Gain-scaled HF signal.
            y[k] = ch.x_high[HF_ADJ + t][k].scale(g_filt[t][k]);

            let sm = s_boost_env[env][k] / E_RANGE;
            if sm != 0.0 {
                // Add sinusoidal component with phase alternation.
                let sign = if (k & 1) != 0 { -sm } else { sm };
                let phase = SINE_PHASE[ch.index_sine];
                y[k].re += sign * phase.re;
                y[k].im += sign * phase.im;
            }
            else {
                // Add noise component from the pseudo-random noise table.
                let ni = (noise_base + t * SBR_BANDS + k - kx + 1) & 511;
                let noise = tables::NOISE_TABLE[ni];
                y[k] += noise.scale(q_filt[t][k] / E_RANGE);
            }
        }
        ch.index_sine = (ch.index_sine + 1) & 3;
    }
    ch.index_noise = (noise_base + k_upper - kx) & 511;

    // Copy adjusted HF subbands into synthesis input buffer.
    let prev_overlap =
        if ch.last_env_end > num_time_slots { (ch.last_env_end - num_time_slots) * 2 } else { 0 };
    ch.last_env_end = t_env_end;

    let x_len = ch.x.len();
    let prev_y_len = ch.prev_y.len();
    let y_len = ch.y.len();
    for t in 0..prev_overlap.min(x_len) {
        let src_idx = HF_ADJ + num_time_slots * 2 + t;
        if src_idx < prev_y_len {
            ch.x[t][kx..].copy_from_slice(&ch.prev_y[src_idx][kx..]);
        }
    }
    for t in prev_overlap..(t_env_end * 2).min(x_len) {
        let src_idx = HF_ADJ + t;
        if src_idx < y_len {
            ch.x[t][kx..].copy_from_slice(&ch.y[src_idx][kx..]);
        }
    }

    ch.prev_l_a = l_a;
}

/// Advance overlap state for the next frame.
pub fn update_frame(ch: &mut SbrChannel, num_time_slots: usize) {
    let n_qmf = num_time_slots * 2;
    // Shift tail of W into the delay region.
    let (w_head, w_tail) = ch.w.split_at_mut(QMF_DELAY);
    let offset = n_qmf.saturating_sub(QMF_DELAY);
    if offset + QMF_DELAY <= w_tail.len() {
        w_head.copy_from_slice(&w_tail[offset..offset + QMF_DELAY]);
    }

    ch.prev_y = ch.y;
    ch.old_invf_mode = ch.invf_mode;
    ch.old_bw_array = ch.bw_array;
    ch.prev_num_env = ch.num_env;
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Determine l_A — the envelope index marking an attack transient.
fn compute_l_a(ch: &SbrChannel) -> i8 {
    match (ch.fclass, ch.pointer) {
        (_, 0) | (FrameClass::FixFix, _) => -1,
        (FrameClass::VarFix, 1) => -1,
        (FrameClass::VarFix, p) => p as i8 - 1,
        (FrameClass::FixVar, p) | (FrameClass::VarVar, p) => ch.num_env as i8 + 1 - p as i8,
    }
}

/// Map bs_add_harmonic flags to per-subband per-envelope sinusoidal indices.
fn compute_sine_mapping(ch: &mut SbrChannel, f_hi: &[usize], l_a: i8) {
    ch.s_idx_mapped = [[false; SBR_BANDS]; NUM_ENVELOPES];
    for env in 0..ch.num_env {
        let mut lo = f_hi[0];
        for (band, &hi) in f_hi[1..].iter().enumerate() {
            if ch.add_harmonic[band] {
                let mid = (lo + hi) / 2;
                if env as i8 >= l_a || ch.prev_s_idx_mapped[mid] {
                    ch.s_idx_mapped[env][mid] = true;
                }
            }
            lo = hi;
        }
    }
    ch.prev_s_idx_mapped = ch.s_idx_mapped[ch.num_env - 1];
}

/// Dequantize envelope scalefactors to linear domain (4.6.18.7.2).
fn dequant_envelope(
    e_out: &mut [[f32; SBR_BANDS]; NUM_ENVELOPES],
    ch: &SbrChannel,
    f_hi: &[usize],
    f_lo: &[usize],
    kx: usize,
    step: f32,
    pan_mid: f32,
) {
    for env in 0..ch.num_env {
        let bands = if ch.freq_res[env] { f_hi } else { f_lo };
        let mut band_lo = kx;

        match ch.qmode {
            QuantMode::Single => {
                for (i, &band_hi) in bands[1..].iter().enumerate() {
                    let val = 2.0f32.powf(6.0 + f32::from(ch.data_env[env][i]) * step);
                    for k in band_lo..band_hi {
                        e_out[env][k] = val;
                    }
                    band_lo = band_hi;
                }
            }
            QuantMode::Left => {
                for (i, &band_hi) in bands[1..].iter().enumerate() {
                    let e0 = f32::from(ch.data_env[env][i]);
                    let e1 = f32::from(ch.data_env2[env][i]);
                    let val = 2.0f32.powf(6.0 + e0 * step + 1.0)
                        / (1.0 + 2.0f32.powf((pan_mid - e1) * step));
                    for k in band_lo..band_hi {
                        e_out[env][k] = val;
                    }
                    band_lo = band_hi;
                }
            }
            QuantMode::Right => {
                for (i, &band_hi) in bands[1..].iter().enumerate() {
                    let e0 = f32::from(ch.data_env2[env][i]);
                    let e1 = f32::from(ch.data_env[env][i]);
                    let val = 2.0f32.powf(6.0 + e0 * step + 1.0)
                        / (1.0 + 2.0f32.powf((e1 - pan_mid) * step));
                    for k in band_lo..band_hi {
                        e_out[env][k] = val;
                    }
                    band_lo = band_hi;
                }
            }
        }
    }
}

/// Dequantize noise floor scalefactors to linear domain.
fn dequant_noise(q_out: &mut [[f32; SBR_BANDS]; NUM_ENVELOPES], ch: &SbrChannel, state: &SbrState) {
    let noise_borders = ch.noise_env_border;
    let mut t_lo = ch.env_border[0];

    for (env, &t_hi) in ch.env_border[1..=ch.num_env].iter().enumerate() {
        // Find which noise envelope this SBR envelope belongs to.
        let mut nenv = 0;
        for n in 0..ch.num_noise {
            if t_lo >= noise_borders[n] && t_hi <= noise_borders[n + 1] {
                nenv = n;
                break;
            }
        }

        let mut band_lo = state.f_noise[0];
        for (nband, &band_hi) in state.f_noise[1..=state.num_noise_bands].iter().enumerate() {
            let val = match ch.qmode {
                QuantMode::Single => 2.0f32.powf(6.0 - f32::from(ch.data_noise[nenv][nband])),
                QuantMode::Left => {
                    let n0 = f32::from(ch.data_noise[nenv][nband]);
                    let n1 = f32::from(ch.data_noise2[nenv][nband]);
                    2.0f32.powf(6.0 - n0 + 1.0) / (1.0 + 2.0f32.powf(12.0 - n1))
                }
                QuantMode::Right => {
                    let n0 = f32::from(ch.data_noise2[nenv][nband]);
                    let n1 = f32::from(ch.data_noise[nenv][nband]);
                    2.0f32.powf(6.0 - n0 + 1.0) / (1.0 + 2.0f32.powf(n1 - 12.0))
                }
            };
            for k in band_lo..band_hi {
                q_out[env][k] = val;
            }
            band_lo = band_hi;
        }
        t_lo = t_hi;
    }
}

/// Fill the gain and noise interpolation buffers (g_temp, q_temp) with
/// per-slot gain values, preserving the smoothing delay history.
fn populate_smooth_buffers(
    ch: &mut SbrChannel,
    g_env: &[[f32; SBR_BANDS]; NUM_ENVELOPES],
    q_env: &[[f32; SBR_BANDS]; NUM_ENVELOPES],
) {
    let (g_head, g_body) = ch.g_temp.split_at_mut(SMOOTH_DELAY);
    let (q_head, q_body) = ch.q_temp.split_at_mut(SMOOTH_DELAY);

    if ch.last_env_end > 0 {
        // Copy tail of previous frame's gains into the delay header.
        let prev_tail = ch.last_env_end * 2;
        g_head.copy_from_slice(&g_body[prev_tail - SMOOTH_DELAY..prev_tail]);
        q_head.copy_from_slice(&q_body[prev_tail - SMOOTH_DELAY..prev_tail]);
    }
    else {
        // First frame: fill delay with the first envelope's gains.
        for d in g_head.iter_mut() {
            *d = g_env[0];
        }
        for d in q_head.iter_mut() {
            *d = q_env[0];
        }
    }

    // Fill body with per-envelope gain values.
    let start = if ch.last_env_end > 0 { ch.env_border[0] } else { 0 };
    let mut t_lo = start;
    for (env, &t_hi) in ch.env_border[1..=ch.num_env].iter().enumerate() {
        for t in (t_lo * 2)..(t_hi * 2) {
            g_body[t] = g_env[env];
            q_body[t] = q_env[env];
        }
        t_lo = t_hi;
    }
}
