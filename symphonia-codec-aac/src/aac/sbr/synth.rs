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

use super::Complex;

use super::dsp::{SbrAnalysis, SbrDsp, SbrSynthesis};
use super::tables;
use super::{
    sq_modulus, FrameClass, QuantMode, SbrChannel, SbrHeader, SbrState, HF_ADJ, MAX_SLOTS,
    NUM_ENVELOPES, QMF_DELAY, SBR_BANDS, SMOOTH_DELAY,
};

const ZERO: Complex = Complex { re: 0.0, im: 0.0 };

/// Limiter gain ceiling per bs_limiter_gains index (ISO/IEC 14496-3, 4.6.18.7.3).
const LIMITER_GAINS: [f32; 4] = [0.70795, 1.0, 1.41254, 10_000_000_000.0];

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
/// (ISO/IEC 14496-3:2019, Table 4.194).
#[rustfmt::skip]
const CHIRP_COEF: [[f32; 4]; 4] = [
    [0.0,  0.6,  0.9,  0.98],
    [0.6,  0.75, 0.9,  0.98],
    [0.0,  0.75, 0.9,  0.98],
    [0.0,  0.75, 0.9,  0.98],
];

/// Energy computation epsilon to avoid division by zero.
const EPSILON: f32 = 1.0;
/// Tiny epsilon for summation stability.
const EPSILON_0: f32 = 1.0e-12;
/// Full-scale range used to map ISO SBR envelope energies to normalized PCM.
const E_RANGE: f32 = 32768.0;

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
pub fn synthesis(
    ch: &mut SbrChannel,
    sbr_s: &mut SbrSynthesis,
    dsp: &mut SbrDsp,
    active_bands: usize,
    dst: &mut [f32],
) {
    for (x_slot, out_chunk) in ch.x.iter_mut().zip(dst.chunks_mut(64)) {
        sbr_s.synthesis(dsp, x_slot, active_bands, out_chunk);
    }
}

/// Bypass mode: copy analysis subbands directly to synthesis input.
pub fn bypass(ch: &mut SbrChannel, num_time_slots: usize) {
    let n_qmf = (num_time_slots * 2).min(ch.x.len());
    for t in 0..n_qmf {
        if HF_ADJ + t < ch.w.len() {
            ch.x[t] = ch.w[HF_ADJ + t];
        }
    }
}

/// Copy low-frequency subbands (below k_x) into the synthesis input buffer.
pub fn x_gen(ch: &mut SbrChannel, state: &SbrState, num_time_slots: usize) {
    let n_qmf = (num_time_slots * 2).min(ch.x.len());
    let kx = state.k_x.min(SBR_BANDS);
    for t in 0..n_qmf {
        if HF_ADJ + t < ch.w.len() {
            ch.x[t][..kx].copy_from_slice(&ch.w[HF_ADJ + t][..kx]);
        }
    }
}

// ---------------------------------------------------------------------------
// HF generation (ISO/IEC 14496-3, 4.6.18.6)
// ---------------------------------------------------------------------------

/// Generate high-frequency QMF subbands from low-frequency content via patching.
pub fn hf_generate(ch: &mut SbrChannel, state: &SbrState, num_time_slots: usize) {
    let kx = state.k_x.min(SBR_BANDS);
    let prev_kx = ch.prev_k_x.min(SBR_BANDS);

    // Copy low-band subbands and clear high-band.
    for (t, (x_slot, w_slot)) in ch.x.iter_mut().zip(ch.w.iter()).enumerate() {
        *x_slot = [ZERO; SBR_BANDS];
        let slot_kx = if t < QMF_DELAY { prev_kx } else { kx };
        x_slot[..slot_kx].copy_from_slice(&w_slot[..slot_kx]);
    }

    // Compute auto/cross-correlation terms for inverse filtering (4.6.18.6.2).
    let k0 = state.f[0].min(SBR_BANDS);
    let n_slots = num_time_slots * 2;
    let corr_len = (n_slots + QMF_DELAY).min(ch.x.len());

    // Solve for prediction coefficients a0, a1 per subband.
    let mut coef_a0 = [ZERO; SBR_BANDS];
    let mut coef_a1 = [ZERO; SBR_BANDS];

    if corr_len < 3 {
        return;
    }

    for k in 0..k0 {
        let phi_01 = delayed_correlation(ch, k, 1, corr_len - 1, 1);
        let phi_02 = delayed_correlation(ch, k, 0, corr_len - 2, 2);
        let phi_12 = delayed_correlation(ch, k, 0, corr_len - 2, 1);
        let phi_11_re = delayed_energy(ch, k, 1, corr_len - 1);
        let phi_22_re = delayed_energy(ch, k, 0, corr_len - 2);

        let det = phi_22_re * phi_11_re - sq_modulus(phi_12) / (1.0 + 1.0e-6);
        if det != 0.0 {
            coef_a1[k] = (phi_01 * phi_12 - phi_02 * phi_11_re) * (1.0 / det);
        }
        if phi_11_re != 0.0 {
            coef_a0[k] = (phi_01 + coef_a1[k] * phi_12.conj()) * (-1.0 / phi_11_re);
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
                    + (coef_a0[src_band] * bw) * ch.x[t - 1][src_band]
                    + (coef_a1[src_band] * (bw * bw)) * ch.x[t - 2][src_band];
            }
            dst_band += p_width;
        }
    }
}

#[inline]
fn delayed_correlation(
    ch: &SbrChannel,
    band: usize,
    start: usize,
    end: usize,
    delay: usize,
) -> Complex {
    let mut sum = ZERO;
    for t in start..end {
        sum += ch.x[t][band].conj() * ch.x[t + delay][band];
    }
    sum
}

#[inline]
fn delayed_energy(ch: &SbrChannel, band: usize, start: usize, end: usize) -> f32 {
    let mut sum = 0.0;
    for t in start..end {
        sum += sq_modulus(ch.x[t][band]);
    }
    sum
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

    let s_mapped = compute_s_mapped(ch, f_hi, f_lo, kx);

    // Dequantize envelope scalefactors to linear energy E_orig (4.6.18.7.2).
    let mut e_orig = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];
    let (step, pan_mid) = if ch.amp_res { (1.0f32, 12.0f32) } else { (0.5, 24.0) };
    dequant_envelope(&mut e_orig, ch, f_hi, f_lo, kx, step, pan_mid);

    // Dequantize noise floor scalefactors to linear energy Q_orig.
    let mut q_orig = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];
    dequant_noise(&mut q_orig, ch, state);

    // Measure current HF energy E_curr per envelope (4.6.18.7.3).
    let mut e_curr = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];
    if hdr.interpol_freq {
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
    }
    else {
        let mut t_lo = t_env_start;
        for (env, &t_hi) in ch.env_border[1..=ch.num_env].iter().enumerate() {
            let n_slots = ((t_hi - t_lo) * 2).max(1) as f32;
            let hi_start = (HF_ADJ + t_lo * 2).min(ch.x_high.len());
            let hi_end = (HF_ADJ + t_hi * 2).min(ch.x_high.len());
            let bands = if ch.freq_res[env] { f_hi } else { f_lo };

            for window in bands.windows(2) {
                let band_lo = window[0].max(kx).min(k_upper);
                let band_hi = window[1].max(kx).min(k_upper);
                if band_lo >= band_hi {
                    continue;
                }

                let mut sum = 0.0f32;
                for slot in &ch.x_high[hi_start..hi_end] {
                    for k in band_lo..band_hi {
                        sum += sq_modulus(slot[k]);
                    }
                }

                let scale = E_RANGE * E_RANGE / (n_slots * (band_hi - band_lo) as f32);
                let avg = sum * scale;
                for k in band_lo..band_hi {
                    e_curr[env][k] = avg;
                }
            }
            t_lo = t_hi;
        }
    }

    // Gain computation (4.6.18.7.3).
    let la_prev: i8 = if ch.prev_l_a == ch.prev_num_env as i8 { 0 } else { -1 };
    let g_lim_max = LIMITER_GAINS[hdr.limiter_gains as usize];
    let mut is_attack_env = [false; NUM_ENVELOPES];

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

        // Pre-compute per-envelope flags (constant across all subbands).
        let is_attack = env as i8 == l_a || env as i8 == la_prev;
        is_attack_env[env] = is_attack;

        // Per-subband gain, noise component, and sinusoidal component.
        for k in kx..k_upper {
            let eo = e_orig[env][k];
            let qo = q_orig[env][k];
            let ec = e_curr[env][k];

            // Precompute shared factors to avoid redundant division/sqrt.
            let inv_1pq = 1.0 / (1.0 + qo);
            let eo_norm = eo * inv_1pq; // eo / (1 + qo)

            q_m[k] = (eo_norm * qo).sqrt();
            s_m[k] = if ch.s_idx_mapped[env][k] { eo_norm.sqrt() } else { 0.0 };

            let inv_ec = 1.0 / (EPSILON + ec);
            g_raw[k] = if !s_mapped[env][k] {
                let q_add = if !is_attack { qo } else { 0.0 };
                (eo * inv_ec / (1.0 + q_add)).sqrt()
            }
            else {
                (eo * inv_ec * qo * inv_1pq).sqrt()
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
            let env = slot_to_env[t];
            if is_attack_env[env] {
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
    let mut index_noise = ch.index_noise;
    for t in (t_env_start * 2)..(t_env_end * 2) {
        let y = &mut ch.y[HF_ADJ + t];
        let env = slot_to_env[t];
        let is_attack = is_attack_env[env];
        for k in kx..k_upper {
            // Gain-scaled HF signal.
            y[k] = ch.x_high[HF_ADJ + t][k] * g_filt[t][k];

            let sm = s_boost_env[env][k] / E_RANGE;
            if sm != 0.0 {
                // Add sinusoidal component with phase alternation.
                y[k] += sine_component(ch.index_sine, k, sm);
            }
            else if !is_attack {
                // Add noise component from the pseudo-random noise table.
                let ni = (index_noise + k - kx + 1) & 511;
                let noise = tables::NOISE_TABLE[ni];
                y[k] += noise * (q_filt[t][k] / E_RANGE);
            }
        }
        index_noise = (index_noise + k_upper - kx) & 511;
        ch.index_sine = (ch.index_sine + 1) & 3;
    }
    ch.index_noise = index_noise;

    // Copy adjusted HF subbands into synthesis input buffer.
    let prev_overlap =
        if ch.last_env_end > num_time_slots { (ch.last_env_end - num_time_slots) * 2 } else { 0 };
    ch.last_env_end = t_env_end;

    let x_len = ch.x.len();
    let prev_y_len = ch.prev_y.len();
    let y_len = ch.y.len();
    let prev_kx = ch.prev_k_x.min(SBR_BANDS);
    let prev_k_upper = ch.prev_k_upper.min(SBR_BANDS).max(prev_kx);
    for t in 0..prev_overlap.min(x_len) {
        let src_idx = HF_ADJ + num_time_slots * 2 + t;
        if src_idx < prev_y_len {
            ch.x[t] = [ZERO; SBR_BANDS];
            ch.x[t][..prev_kx].copy_from_slice(&ch.w[HF_ADJ + t][..prev_kx]);
            ch.x[t][prev_kx..prev_k_upper]
                .copy_from_slice(&ch.prev_y[src_idx][prev_kx..prev_k_upper]);
        }
    }
    for t in prev_overlap..(t_env_end * 2).min(x_len) {
        let src_idx = HF_ADJ + t;
        if src_idx < y_len {
            ch.x[t][kx..].fill(ZERO);
            ch.x[t][kx..k_upper].copy_from_slice(&ch.y[src_idx][kx..k_upper]);
        }
    }

    ch.prev_l_a = l_a;
    ch.prev_k_x = kx;
    ch.prev_k_upper = k_upper;
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

#[inline]
fn sine_component(index_sine: usize, k: usize, magnitude: f32) -> Complex {
    match index_sine & 3 {
        0 => Complex { re: magnitude, im: 0.0 },
        1 => Complex { re: 0.0, im: if (k & 1) == 0 { magnitude } else { -magnitude } },
        2 => Complex { re: -magnitude, im: 0.0 },
        _ => Complex { re: 0.0, im: if (k & 1) == 0 { -magnitude } else { magnitude } },
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

/// Map active sinusoidal indices to the envelope scalefactor resolution.
fn compute_s_mapped(
    ch: &SbrChannel,
    f_hi: &[usize],
    f_lo: &[usize],
    kx: usize,
) -> [[bool; SBR_BANDS]; NUM_ENVELOPES] {
    let mut s_mapped = [[false; SBR_BANDS]; NUM_ENVELOPES];

    for env in 0..ch.num_env {
        let mut band_lo = kx;
        let bands = if ch.freq_res[env] { f_hi } else { f_lo };
        for &band_hi in bands[1..].iter() {
            let active = ch.s_idx_mapped[env][band_lo..band_hi].contains(&true);
            for b in band_lo..band_hi {
                s_mapped[env][b] = active;
            }
            band_lo = band_hi;
        }
    }

    s_mapped
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
                    let val = f32::exp2(6.0 + f32::from(ch.data_env[env][i]) * step);
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
                    let val =
                        f32::exp2(6.0 + e0 * step + 1.0) / (1.0 + f32::exp2((pan_mid - e1) * step));
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
                    let val =
                        f32::exp2(6.0 + e0 * step + 1.0) / (1.0 + f32::exp2((e1 - pan_mid) * step));
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
                QuantMode::Single => f32::exp2(6.0 - f32::from(ch.data_noise[nenv][nband])),
                QuantMode::Left => {
                    let n0 = f32::from(ch.data_noise[nenv][nband]);
                    let n1 = f32::from(ch.data_noise2[nenv][nband]);
                    f32::exp2(6.0 - n0 + 1.0) / (1.0 + f32::exp2(12.0 - n1))
                }
                QuantMode::Right => {
                    let n0 = f32::from(ch.data_noise2[nenv][nband]);
                    let n1 = f32::from(ch.data_noise[nenv][nband]);
                    f32::exp2(6.0 - n0 + 1.0) / (1.0 + f32::exp2(n1 - 12.0))
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

        // If the current leading border is delayed, the synthesis matrix uses
        // the previous frame's Y for those leading slots. The smoothing buffers
        // need the matching previous GTemp/QTemp columns because the first
        // current envelope can still read them through the FIR history.
        let lead_slots = (ch.env_border[0] * 2).min(prev_tail).min(g_body.len());
        if lead_slots > 0 {
            let src_start = prev_tail - lead_slots;
            g_body.copy_within(src_start..prev_tail, 0);
            q_body.copy_within(src_start..prev_tail, 0);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn c(re: f32, im: f32) -> Complex {
        Complex { re, im }
    }

    #[test]
    fn x_gen_copies_low_band_at_envelope_adjustment_delay() {
        let mut ch = SbrChannel::new();
        let mut state = SbrState::new();
        state.k_x = 2;

        ch.w[HF_ADJ][0] = c(1.0, 2.0);
        ch.w[HF_ADJ][1] = c(3.0, 4.0);
        ch.w[QMF_DELAY][0] = c(10.0, 20.0);
        ch.w[QMF_DELAY][1] = c(30.0, 40.0);

        x_gen(&mut ch, &state, 1);

        assert_eq!(ch.x[0][0], c(1.0, 2.0));
        assert_eq!(ch.x[0][1], c(3.0, 4.0));
    }

    #[test]
    fn bypass_uses_envelope_adjustment_delay() {
        let mut ch = SbrChannel::new();
        ch.w[HF_ADJ][0] = c(1.0, 2.0);
        ch.w[HF_ADJ][1] = c(3.0, 4.0);
        ch.w[QMF_DELAY][0] = c(10.0, 20.0);
        ch.w[QMF_DELAY][1] = c(30.0, 40.0);

        bypass(&mut ch, 1);

        assert_eq!(ch.x[0][0], c(1.0, 2.0));
        assert_eq!(ch.x[0][1], c(3.0, 4.0));
    }

    #[test]
    fn delayed_correlation_uses_requested_window_and_delay() {
        let mut ch = SbrChannel::new();
        for t in 0..6 {
            ch.x[t][0] = c(t as f32 + 1.0, 0.0);
        }

        // Sum over t=1..3 of conj(x[t]) * x[t + 2]:
        // 2*4 + 3*5 + 4*6 = 47.
        assert_eq!(delayed_correlation(&ch, 0, 1, 4, 2), c(47.0, 0.0));
    }

    #[test]
    fn delayed_energy_uses_requested_window() {
        let mut ch = SbrChannel::new();
        ch.x[0][0] = c(3.0, 4.0);
        ch.x[1][0] = c(1.0, 2.0);
        ch.x[2][0] = c(2.0, 0.0);

        assert_eq!(delayed_energy(&ch, 0, 0, 2), 30.0);
    }

    #[test]
    fn smooth_buffers_preserve_previous_slots_before_delayed_border() {
        let mut ch = SbrChannel::new();
        let mut g_env = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];
        let mut q_env = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];

        ch.last_env_end = 4;
        ch.num_env = 1;
        ch.env_border[0] = 2;
        ch.env_border[1] = 8;
        g_env[0][0] = 100.0;
        q_env[0][0] = 200.0;

        for i in 0..4 {
            ch.g_temp[SMOOTH_DELAY + 4 + i][0] = 10.0 + i as f32;
            ch.q_temp[SMOOTH_DELAY + 4 + i][0] = 20.0 + i as f32;
        }

        populate_smooth_buffers(&mut ch, &g_env, &q_env);

        for i in 0..4 {
            assert_eq!(ch.g_temp[i][0], 10.0 + i as f32);
            assert_eq!(ch.q_temp[i][0], 20.0 + i as f32);
            assert_eq!(ch.g_temp[SMOOTH_DELAY + i][0], 10.0 + i as f32);
            assert_eq!(ch.q_temp[SMOOTH_DELAY + i][0], 20.0 + i as f32);
        }
        assert_eq!(ch.g_temp[SMOOTH_DELAY + 4][0], 100.0);
        assert_eq!(ch.q_temp[SMOOTH_DELAY + 4][0], 200.0);
    }

    #[test]
    fn hf_generate_uses_previous_kx_for_delay_slots() {
        let mut ch = SbrChannel::new();
        let mut state = SbrState::new();
        state.k_x = 4;

        ch.prev_k_x = 2;
        ch.w[0][0] = c(1.0, 0.0);
        ch.w[0][1] = c(2.0, 0.0);
        ch.w[0][2] = c(3.0, 0.0);
        ch.w[0][3] = c(4.0, 0.0);
        ch.w[QMF_DELAY][0] = c(10.0, 0.0);
        ch.w[QMF_DELAY][1] = c(20.0, 0.0);
        ch.w[QMF_DELAY][2] = c(30.0, 0.0);
        ch.w[QMF_DELAY][3] = c(40.0, 0.0);

        hf_generate(&mut ch, &state, 1);

        assert_eq!(ch.x[0][0], c(1.0, 0.0));
        assert_eq!(ch.x[0][1], c(2.0, 0.0));
        assert_eq!(ch.x[0][2], ZERO);
        assert_eq!(ch.x[0][3], ZERO);
        assert_eq!(ch.x[QMF_DELAY][0], c(10.0, 0.0));
        assert_eq!(ch.x[QMF_DELAY][1], c(20.0, 0.0));
        assert_eq!(ch.x[QMF_DELAY][2], c(30.0, 0.0));
        assert_eq!(ch.x[QMF_DELAY][3], c(40.0, 0.0));
    }

    #[test]
    fn sine_component_matches_sbr_phase_sequence() {
        assert_eq!(sine_component(0, 10, 2.0), c(2.0, 0.0));
        assert_eq!(sine_component(1, 10, 2.0), c(0.0, 2.0));
        assert_eq!(sine_component(1, 11, 2.0), c(0.0, -2.0));
        assert_eq!(sine_component(2, 10, 2.0), c(-2.0, 0.0));
        assert_eq!(sine_component(3, 10, 2.0), c(0.0, -2.0));
        assert_eq!(sine_component(3, 11, 2.0), c(0.0, 2.0));
    }

    #[test]
    fn limiter_gain_table_uses_limiter_off_value() {
        assert_eq!(LIMITER_GAINS[0], 0.70795);
        assert_eq!(LIMITER_GAINS[1], 1.0);
        assert_eq!(LIMITER_GAINS[2], 1.41254);
        assert_eq!(LIMITER_GAINS[3], 10_000_000_000.0);
    }

    #[test]
    fn envelope_energy_range_uses_pcm_full_scale() {
        assert_eq!(E_RANGE, 32768.0);
    }

    #[test]
    fn s_mapped_uses_gated_sinusoidal_indices_for_high_res_envelopes() {
        let mut ch = SbrChannel::new();
        ch.num_env = 2;
        ch.freq_res[0] = true;
        ch.freq_res[1] = true;
        ch.add_harmonic[0] = true;
        ch.s_idx_mapped[1][3] = true;

        let f_hi = [2, 4, 6];
        let f_lo = [2, 6];
        let s_mapped = compute_s_mapped(&ch, &f_hi, &f_lo, 2);

        assert!(!s_mapped[0][2]);
        assert!(!s_mapped[0][3]);
        assert!(s_mapped[1][2]);
        assert!(s_mapped[1][3]);
        assert!(!s_mapped[1][4]);
    }

    #[test]
    fn overlap_uses_previous_frame_sbr_band_limits() {
        let mut ch = SbrChannel::new();
        let mut state = SbrState::new();
        let hdr = SbrHeader::new();

        state.k_x = 4;
        state.num_master = 1;
        state.f[0] = 4;
        state.f[1] = 10;
        state.num_env_bands = [1, 1];
        state.f_low[0] = 4;
        state.f_low[1] = 10;
        state.num_noise_bands = 1;
        state.f_noise[0] = 4;
        state.f_noise[1] = 10;
        state.num_lim = 1;
        state.f_lim[0] = 4;
        state.f_lim[1] = 10;

        ch.num_env = 1;
        ch.num_noise = 1;
        ch.env_border[0] = 0;
        ch.env_border[1] = 8;
        ch.noise_env_border[0] = 0;
        ch.noise_env_border[1] = 8;
        ch.prev_k_x = 3;
        ch.prev_k_upper = 6;
        ch.last_env_end = 10;

        ch.x[0] = [c(99.0, 0.0); SBR_BANDS];
        ch.w[HF_ADJ][0] = c(1.0, 0.0);
        ch.w[HF_ADJ][1] = c(2.0, 0.0);
        ch.w[HF_ADJ][2] = c(3.0, 0.0);

        let prev_src = HF_ADJ + 8 * 2;
        ch.prev_y[prev_src][3] = c(30.0, 0.0);
        ch.prev_y[prev_src][4] = c(40.0, 0.0);
        ch.prev_y[prev_src][5] = c(50.0, 0.0);
        ch.prev_y[prev_src][6] = c(60.0, 0.0);

        hf_adjust(&mut ch, &state, &hdr, 8);

        assert_eq!(ch.x[0][0], c(1.0, 0.0));
        assert_eq!(ch.x[0][1], c(2.0, 0.0));
        assert_eq!(ch.x[0][2], c(3.0, 0.0));
        assert_eq!(ch.x[0][3], c(30.0, 0.0));
        assert_eq!(ch.x[0][4], c(40.0, 0.0));
        assert_eq!(ch.x[0][5], c(50.0, 0.0));
        assert_eq!(ch.x[0][6], ZERO);
        assert_eq!(ch.prev_k_x, 4);
        assert_eq!(ch.prev_k_upper, 10);
    }
}
