// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SBR HF generation and HF adjustment (envelope adjustment).
//!
//! Implements the core SBR signal processing:
//! - QMF analysis/synthesis wrappers for the channel
//! - HF generation via inverse filtering and patching (ISO 14496-3 4.6.18.6)
//! - HF adjustment: envelope mapping, gain computation, smoothing (ISO 14496-3 4.6.18.7)

use symphonia_core::dsp::complex::Complex;

use super::dsp::{SbrAnalysis, SbrDsp, SbrSynthesis};
use super::tables;
use super::{
    sq_modulus, FrameClass, QuantMode, SbrChannel, SbrHeader, SbrState, HF_ADJ, MAX_SLOTS,
    NUM_ENVELOPES, QMF_DELAY, SBR_BANDS, SMOOTH_DELAY,
};

const COMPLEX_ZERO: Complex = Complex { re: 0.0, im: 0.0 };

/// Scale factor range for energy computations (2^16, ISO/IEC 14496-3, 4.6.18.7.3).
const RANGE: f32 = 65536.0;

/// Bandwidth expansion coefficient table (ISO/IEC 14496-3, Table 4.158).
/// Indexed by [old_invf_mode][new_invf_mode].
#[rustfmt::skip]
const NEW_BW: [[f32; 4]; 4] = [
    [ 0.0, 0.6,  0.9, 0.98 ],
    [ 0.6, 0.75, 0.9, 0.98 ],
    [ 0.0, 0.75, 0.9, 0.98 ],
    [ 0.0, 0.75, 0.9, 0.98 ],
];

/// Limiter gain values for the four limiter_gains settings (ISO/IEC 14496-3, 4.6.18.7.3).
const LIM_GAIN: [f32; 4] = [0.70795, 1.0, 1.41254, 10000.0];

/// Smoothing filter coefficients, 5-tap (ISO/IEC 14496-3, 4.6.18.7.3).
#[rustfmt::skip]
const H_SMOOTH: [f32; 5] = [
    1.0 / 3.0, 0.30150283239582, 0.21816949906249, 0.11516383427084, 0.03183050093751,
];

/// Phase factors for sinusoidal addition (ISO/IEC 14496-3, 4.6.18.7.5).
const PHI: [Complex; 4] = [
    Complex { re: 1.0, im: 0.0 },
    Complex { re: 0.0, im: 1.0 },
    Complex { re: -1.0, im: 0.0 },
    Complex { re: 0.0, im: -1.0 },
];

const EPS: f32 = 1.0;
const EPS0: f32 = 1.0e-12;

/// Run QMF analysis on the channel's input samples.
///
/// Splits 32*num_slots input samples into QMF subbands stored in `ch.w`.
pub fn analysis(ch: &mut SbrChannel, sbr_a: &mut SbrAnalysis, dsp: &mut SbrDsp, src: &[f32]) {
    for (src_chunk, dst) in src.chunks(32).zip(ch.w[QMF_DELAY..].iter_mut()) {
        sbr_a.analysis(dsp, src_chunk, dst);
    }
}

/// Run QMF synthesis on the channel's output subbands.
///
/// Reconstructs 64*num_slots output samples from `ch.x`.
pub fn synthesis(ch: &mut SbrChannel, sbr_s: &mut SbrSynthesis, dsp: &mut SbrDsp, dst: &mut [f32]) {
    for (src_slot, dst_chunk) in ch.x.iter_mut().zip(dst.chunks_mut(64)) {
        sbr_s.synthesis(dsp, src_slot, dst_chunk);
    }
}

/// Copy current-frame analysis subbands to synthesis input (bypass mode).
pub fn bypass(ch: &mut SbrChannel, num_time_slots: usize) {
    let num_qmf_slots = num_time_slots * 2;
    for slot in 0..num_qmf_slots {
        ch.x[slot] = ch.w[QMF_DELAY + slot];
    }
}

/// Realign low-band subbands in synthesis input after HF adjustment.
pub fn x_gen(ch: &mut SbrChannel, state: &SbrState, num_time_slots: usize) {
    let num_qmf_slots = num_time_slots * 2;
    for slot in 0..num_qmf_slots {
        ch.x[slot][..state.k_x].copy_from_slice(&ch.w[QMF_DELAY + slot][..state.k_x]);
    }
}

/// HF generation: create high-frequency subbands from the low-frequency signal.
pub fn hf_generate(ch: &mut SbrChannel, state: &SbrState, num_time_slots: usize) {
    for (x, w) in ch.x.iter_mut().zip(ch.w.iter()) {
        x[..state.k_x].copy_from_slice(&w[..state.k_x]);
        for el in x[state.k_x..].iter_mut() {
            *el = COMPLEX_ZERO;
        }
    }

    let mut phi = [[[COMPLEX_ZERO; SBR_BANDS]; 3]; 3];
    let mut a0 = [COMPLEX_ZERO; SBR_BANDS];
    let mut a1 = [COMPLEX_ZERO; SBR_BANDS];
    let k0 = state.f[0];

    for (i, phi_i) in phi.iter_mut().enumerate() {
        for (j, phi_ij) in phi_i.iter_mut().enumerate().skip(1) {
            let src0 = &ch.x[HF_ADJ - i..][..num_time_slots * 2 + 6 - 1];
            let src1 = &ch.x[HF_ADJ - j..][..num_time_slots * 2 + 6 - 1];
            for (slot0, slot1) in src0.iter().zip(src1.iter()) {
                for (k, phi_val) in phi_ij.iter_mut().take(k0).enumerate() {
                    *phi_val += slot0[k] * slot1[k].conj();
                }
            }
        }
    }

    for (k, (a0_k, a1_k)) in a0.iter_mut().zip(a1.iter_mut()).take(k0).enumerate() {
        let phi12 = phi[1][2][k];
        let d_k = phi[2][2][k].re * phi[1][1][k].re - sq_modulus(phi12) / (1.0 + 1.0e-6);

        if d_k != 0.0 {
            let term1 = phi[0][1][k] * phi[1][2][k];
            let term2 = phi[0][2][k] * phi[1][1][k];
            *a1_k = (term1 - term2).scale(1.0 / d_k);
        }
        if phi[1][1][k].re != 0.0 {
            *a0_k = (phi[0][1][k] + *a1_k * phi[1][2][k].conj()).scale(-1.0 / phi[1][1][k].re);
        }
        if sq_modulus(*a0_k) >= 16.0 || sq_modulus(*a1_k) >= 16.0 {
            *a0_k = COMPLEX_ZERO;
            *a1_k = COMPLEX_ZERO;
        }
    }

    for k in 0..state.num_noise_bands {
        let new_bw = NEW_BW[ch.old_invf_mode[k] as usize][ch.invf_mode[k] as usize];
        let old_bw = ch.old_bw_array[k];
        let temp_bw = if new_bw < old_bw {
            0.75 * new_bw + 0.25 * old_bw
        }
        else {
            0.90625 * new_bw + 0.09375 * old_bw
        };
        ch.bw_array[k] = if temp_bw >= 0.015625 { temp_bw } else { 0.0 };
    }

    let env_start = ch.env_border[0];
    let env_end = ch.env_border[ch.num_env];
    for l in (HF_ADJ + env_start * 2)..(HF_ADJ + env_end * 2) {
        ch.x_high[l] = [COMPLEX_ZERO; SBR_BANDS];
        let mut dst_k = state.k_x;
        for (&patch_start, &patch_len) in state.patch_start_subband[..state.num_patches]
            .iter()
            .zip(state.patch_num_subbands.iter())
        {
            for k in 0..patch_len {
                let p = patch_start + k;
                let cur_x = ch.x[l][p];
                let prev_x = ch.x[l - 1][p];
                let pprev_x = ch.x[l - 2][p];

                let g_k = match state.f_noise[..state.num_noise_bands].binary_search(&(dst_k + k)) {
                    Ok(idx) => idx,
                    Err(idx) => idx.saturating_sub(1).min(state.num_noise_bands - 1),
                };
                let bw = ch.bw_array[g_k];

                ch.x_high[l][dst_k + k] =
                    cur_x + a0[p].scale(bw) * prev_x + a1[p].scale(bw * bw) * pprev_x;
            }
            dst_k += patch_len;
        }
    }
}

/// HF adjustment: apply spectral envelope, noise floor, and sinusoidal components.
pub fn hf_adjust(ch: &mut SbrChannel, state: &SbrState, hdr: &SbrHeader, num_time_slots: usize) {
    let kx = state.k_x;
    let km = state.f[state.num_master];
    let envelope_start = ch.env_border[0];
    let envelope_end = ch.env_border[ch.num_env];

    let high_start = state.f[..=state.num_master].binary_search(&state.k_x).unwrap_or(0);
    let f_high = &state.f[..=state.num_master][high_start..];
    let f_low = &state.f_low[..=state.num_env_bands[0]];

    let l_a: i8 = match (ch.fclass, ch.pointer) {
        (_, 0) => -1,
        (FrameClass::FixFix, _) => -1,
        (FrameClass::FixVar, _) | (FrameClass::VarVar, _) => {
            (ch.num_env as i8) + 1 - (ch.pointer as i8)
        }
        (FrameClass::VarFix, 1) => -1,
        (FrameClass::VarFix, _) => (ch.pointer as i8) - 1,
    };

    ch.s_idx_mapped = [[false; SBR_BANDS]; NUM_ENVELOPES];
    for (_l, s_idx_mapped) in ch.s_idx_mapped[..ch.num_env].iter_mut().enumerate() {
        let mut start = f_high[0];
        for (i, &end) in f_high.iter().skip(1).enumerate() {
            if ch.add_harmonic[i] {
                let mid = (start + end) / 2;
                if ((_l as i8) >= l_a) || ch.prev_s_idx_mapped[mid] {
                    s_idx_mapped[mid] = true;
                }
            }
            start = end;
        }
    }
    ch.prev_s_idx_mapped = ch.s_idx_mapped[ch.num_env - 1];

    let mut s_mapped = [[false; SBR_BANDS]; NUM_ENVELOPES];
    for ((s_map, s_idx), &freq_res) in
        s_mapped.iter_mut().zip(ch.s_idx_mapped[..ch.num_env].iter()).zip(ch.freq_res.iter())
    {
        let mut band_start = kx;
        if freq_res {
            for (&add_sine, &band_end) in ch.add_harmonic.iter().zip(f_high[1..].iter()) {
                for el in s_map[band_start..band_end].iter_mut() {
                    *el = add_sine;
                }
                band_start = band_end;
            }
        }
        else {
            for &band_end in f_low[1..].iter() {
                let add_sine = s_idx[band_start..band_end].contains(&true);
                for el in s_map[band_start..band_end].iter_mut() {
                    *el = add_sine;
                }
                band_start = band_end;
            }
        }
    }

    let mut e_orig_mapped = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];
    let (a, pan_offset) = if ch.amp_res { (1.0, 12.0) } else { (0.5, 24.0) };

    match ch.qmode {
        QuantMode::Single => {
            for (dst, (src, &freq_res)) in e_orig_mapped[..ch.num_env]
                .iter_mut()
                .zip(ch.data_env.iter().zip(ch.freq_res.iter()))
            {
                let bands = if freq_res { f_high } else { f_low };
                let mut start = kx;
                for (&val, &band_end) in src.iter().zip(bands.iter().skip(1)) {
                    let scale = 2.0f32.powf(6.0 + f32::from(val) * a);
                    for d in dst[start..band_end].iter_mut() {
                        *d = scale;
                    }
                    start = band_end;
                }
            }
        }
        QuantMode::Left => {
            for (dst, ((e0, e1), &freq_res)) in e_orig_mapped[..ch.num_env]
                .iter_mut()
                .zip(ch.data_env.iter().zip(ch.data_env2.iter()).zip(ch.freq_res.iter()))
            {
                let bands = if freq_res { f_high } else { f_low };
                let mut start = kx;
                for ((&e0v, &e1v), &band_end) in e0.iter().zip(e1.iter()).zip(bands.iter().skip(1))
                {
                    let scale = 2.0f32.powf(6.0 + f32::from(e0v) * a + 1.0)
                        / (1.0 + 2.0f32.powf((pan_offset - f32::from(e1v)) * a));
                    for d in dst[start..band_end].iter_mut() {
                        *d = scale;
                    }
                    start = band_end;
                }
            }
        }
        QuantMode::Right => {
            for (dst, ((e0, e1), &freq_res)) in e_orig_mapped[..ch.num_env]
                .iter_mut()
                .zip(ch.data_env2.iter().zip(ch.data_env.iter()).zip(ch.freq_res.iter()))
            {
                let bands = if freq_res { f_high } else { f_low };
                let mut start = kx;
                for ((&e0v, &e1v), &band_end) in e0.iter().zip(e1.iter()).zip(bands.iter().skip(1))
                {
                    let scale = 2.0f32.powf(6.0 + f32::from(e0v) * a + 1.0)
                        / (1.0 + 2.0f32.powf((f32::from(e1v) - pan_offset) * a));
                    for d in dst[start..band_end].iter_mut() {
                        *d = scale;
                    }
                    start = band_end;
                }
            }
        }
    }

    let mut q_mapped = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];
    let mut env_start_border = ch.env_border[0];
    let noise_env = ch.noise_env_border;

    match ch.qmode {
        QuantMode::Single => {
            for (env_no, &env_end) in ch.env_border[1..=ch.num_env].iter().enumerate() {
                let mut noise_env_no = 0;
                for nenv in 0..ch.num_noise {
                    if (env_start_border >= noise_env[nenv]) && (env_end <= noise_env[nenv + 1]) {
                        noise_env_no = nenv;
                        break;
                    }
                }
                let mut band_start = state.f_noise[0];
                for (noise_band, &band_end) in
                    state.f_noise[1..=state.num_noise_bands].iter().enumerate()
                {
                    let scale =
                        2.0f32.powf(6.0 - f32::from(ch.data_noise[noise_env_no][noise_band]));
                    for el in q_mapped[env_no][band_start..band_end].iter_mut() {
                        *el = scale;
                    }
                    band_start = band_end;
                }
                env_start_border = env_end;
            }
        }
        QuantMode::Left => {
            for (env_no, &env_end) in ch.env_border[1..=ch.num_env].iter().enumerate() {
                let mut noise_env_no = 0;
                for nenv in 0..ch.num_noise {
                    if (env_start_border >= noise_env[nenv]) && (env_end <= noise_env[nenv + 1]) {
                        noise_env_no = nenv;
                        break;
                    }
                }
                let mut band_start = state.f_noise[0];
                for (noise_band, &band_end) in
                    state.f_noise[1..=state.num_noise_bands].iter().enumerate()
                {
                    let n0 = ch.data_noise[noise_env_no][noise_band];
                    let n1 = ch.data_noise2[noise_env_no][noise_band];
                    let scale = 2.0f32.powf(6.0 - f32::from(n0) + 1.0)
                        / (1.0 + 2.0f32.powf(12.0 - f32::from(n1)));
                    for el in q_mapped[env_no][band_start..band_end].iter_mut() {
                        *el = scale;
                    }
                    band_start = band_end;
                }
                env_start_border = env_end;
            }
        }
        QuantMode::Right => {
            for (env_no, &env_end) in ch.env_border[1..=ch.num_env].iter().enumerate() {
                let mut noise_env_no = 0;
                for nenv in 0..ch.num_noise {
                    if (env_start_border >= noise_env[nenv]) && (env_end <= noise_env[nenv + 1]) {
                        noise_env_no = nenv;
                        break;
                    }
                }
                let mut band_start = state.f_noise[0];
                for (noise_band, &band_end) in
                    state.f_noise[1..=state.num_noise_bands].iter().enumerate()
                {
                    let n0 = ch.data_noise2[noise_env_no][noise_band];
                    let n1 = ch.data_noise[noise_env_no][noise_band];
                    let scale = 2.0f32.powf(6.0 - f32::from(n0) + 1.0)
                        / (1.0 + 2.0f32.powf(f32::from(n1) - 12.0));
                    for el in q_mapped[env_no][band_start..band_end].iter_mut() {
                        *el = scale;
                    }
                    band_start = band_end;
                }
                env_start_border = env_end;
            }
        }
    }

    let mut e_curr = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];
    let mut border_start = ch.env_border[0];
    for (e_c, &env_end) in e_curr.iter_mut().zip(ch.env_border[1..=ch.num_env].iter()) {
        for slot in ch.x_high[HF_ADJ..][(border_start * 2)..(env_end * 2)].iter() {
            for (dst, x) in e_c[kx..km].iter_mut().zip(slot[kx..km].iter()) {
                *dst += sq_modulus(*x);
            }
        }
        let num_slots = ((env_end - border_start) * 2) as f32;
        for el in e_c[kx..km].iter_mut() {
            *el *= RANGE * RANGE;
            *el /= num_slots;
        }
        border_start = env_end;
    }

    let la_prev: i8 = if ch.prev_l_a == (ch.prev_num_env as i8) { 0 } else { -1 };
    let mut g_max_tmp;
    let mut g_boost_tmp;
    let mut g = [0.0f32; SBR_BANDS];
    let mut q_m = [0.0f32; SBR_BANDS];
    let mut s_m = [0.0f32; SBR_BANDS];
    let mut q_m_lim = [0.0f32; SBR_BANDS];
    let mut g_lim = [0.0f32; SBR_BANDS];
    let mut g_lim_boost = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];
    let mut q_m_lim_boost = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];
    let mut s_m_boost = [[0.0f32; SBR_BANDS]; NUM_ENVELOPES];

    for env in 0..ch.num_env {
        let mut start = kx;
        g_max_tmp = [0.0f32; SBR_BANDS];
        g_boost_tmp = [0.0f32; SBR_BANDS];

        for (dst, &end) in g_max_tmp.iter_mut().zip(state.f_lim[1..=state.num_lim].iter()) {
            let mut e_o_sum = EPS0;
            let mut e_c_sum = EPS0;
            for k in start..end {
                e_o_sum += e_orig_mapped[env][k];
                e_c_sum += e_curr[env][k];
            }
            *dst = (e_o_sum / e_c_sum).sqrt() * LIM_GAIN[hdr.limiter_gains as usize];
            start = end;
        }

        for k in kx..km {
            let e_orig = e_orig_mapped[env][k];
            let q_orig = q_mapped[env][k];
            let e_cur = e_curr[env][k];

            q_m[k] = (e_orig * q_orig / (1.0 + q_orig)).sqrt();
            s_m[k] = if ch.s_idx_mapped[env][k] { (e_orig / (1.0 + q_orig)).sqrt() } else { 0.0 };

            g[k] = if !s_mapped[env][k] {
                let q_add = if (env as i8) != l_a && (env as i8) != la_prev { q_orig } else { 0.0 };
                (e_orig / ((EPS + e_cur) * (1.0 + q_add))).sqrt()
            }
            else {
                (e_orig / (EPS + e_cur) * q_orig / (1.0 + q_orig)).sqrt()
            };

            let mut lidx = 0;
            for i in 0..state.num_lim {
                if (state.f_lim[i] <= k) && (k < state.f_lim[i + 1]) {
                    lidx = i;
                    break;
                }
            }

            let g_max = g_max_tmp[lidx].min(1.0e5);
            q_m_lim[k] = q_m[k].min(q_m[k] * g_max / g[k]);
            g_lim[k] = g[k].min(g_max);
        }

        let mut start = kx;
        for (lim_no, dst) in g_boost_tmp[..state.num_lim].iter_mut().enumerate() {
            let end = state.f_lim[lim_no + 1];
            let mut nsum = EPS0;
            let mut dsum = EPS0;
            for k in start..end {
                nsum += e_orig_mapped[env][k];
                dsum += e_curr[env][k] * g_lim[k] * g_lim[k];
                if s_m[k] != 0.0 || (env as i8) == l_a || (env as i8) == la_prev {
                    dsum += s_m[k] * s_m[k];
                }
                else {
                    dsum += q_m_lim[k] * q_m_lim[k];
                }
            }
            *dst = (nsum / dsum).sqrt();
            let g_boost = dst.min(1.584893192); // 10^(1/5), ISO/IEC 14496-3, 4.6.18.7.3
            for k in start..end {
                g_lim_boost[env][k] = g_lim[k] * g_boost;
                q_m_lim_boost[env][k] = q_m_lim[k] * g_boost;
                s_m_boost[env][k] = s_m[k] * g_boost;
            }
            start = end;
        }
    }

    let mut env_map = [0usize; MAX_SLOTS * 2 + QMF_DELAY];
    let mut start = ch.env_border[0];
    for (env, &env_end) in ch.env_border[1..=ch.num_env].iter().enumerate() {
        for l in (start * 2)..(env_end * 2) {
            env_map[l] = env;
        }
        start = env_end;
    }

    let (ghead, gcur) = ch.g_temp.split_at_mut(SMOOTH_DELAY);
    let (qhead, qcur) = ch.q_temp.split_at_mut(SMOOTH_DELAY);
    if ch.last_env_end > 0 {
        let prev_end_qmf = ch.last_env_end * 2;
        ghead.copy_from_slice(&gcur[prev_end_qmf - SMOOTH_DELAY..][..SMOOTH_DELAY]);
        qhead.copy_from_slice(&qcur[prev_end_qmf - SMOOTH_DELAY..][..SMOOTH_DELAY]);
        let mut start = ch.env_border[0];
        for (&env_end, (g_l, q_l)) in
            ch.env_border[1..=ch.num_env].iter().zip(g_lim_boost.iter().zip(q_m_lim_boost.iter()))
        {
            for slot in (start * 2)..(env_end * 2) {
                gcur[slot] = *g_l;
                qcur[slot] = *q_l;
            }
            start = env_end;
        }
    }
    else {
        for dst in ghead.iter_mut() {
            *dst = g_lim_boost[0];
        }
        for dst in qhead.iter_mut() {
            *dst = q_m_lim_boost[0];
        }
        let mut start = 0;
        for (&env_end, (g_l, q_l)) in
            ch.env_border[1..=ch.num_env].iter().zip(g_lim_boost.iter().zip(q_m_lim_boost.iter()))
        {
            for slot in (start * 2)..(env_end * 2) {
                gcur[slot] = *g_l;
                qcur[slot] = *q_l;
            }
            start = env_end;
        }
    }

    let mut g_filt = [[0.0f32; SBR_BANDS]; MAX_SLOTS * 2 + QMF_DELAY];
    let mut q_filt = [[0.0f32; SBR_BANDS]; MAX_SLOTS * 2 + QMF_DELAY];
    if !hdr.smoothing_mode {
        for slot in (envelope_start * 2)..(envelope_end * 2) {
            if (slot as i8) == (la_prev * 2) {
                g_filt[slot].copy_from_slice(&ch.g_temp[slot + SMOOTH_DELAY]);
                q_filt[slot].copy_from_slice(&ch.q_temp[slot + SMOOTH_DELAY]);
                continue;
            }
            for k in kx..km {
                let mut gsum = 0.0f32;
                let mut qsum = 0.0f32;
                for (i, &coef) in H_SMOOTH.iter().enumerate() {
                    gsum += ch.g_temp[slot + SMOOTH_DELAY - i][k] * coef;
                    qsum += ch.q_temp[slot + SMOOTH_DELAY - i][k] * coef;
                }
                g_filt[slot][k] = gsum;
                q_filt[slot][k] = qsum;
            }
        }
    }
    else {
        g_filt.copy_from_slice(gcur);
        q_filt.copy_from_slice(qcur);
    }

    let index_noise = ch.index_noise.wrapping_sub(ch.env_border[0] * 2) & 511;
    for (slot, y) in
        ch.y.iter_mut().skip(HF_ADJ).enumerate().take(envelope_end * 2).skip(envelope_start * 2)
    {
        for (k, y_val) in y.iter_mut().enumerate().skip(kx).take(km - kx) {
            *y_val = ch.x_high[HF_ADJ + slot][k].scale(g_filt[slot][k]);

            let smb = s_m_boost[env_map[slot]][k] / RANGE;
            if smb != 0.0 {
                let mut s = Complex { re: smb, im: smb };
                if (k & 1) != 0 {
                    s.re = -s.re;
                    s.im = -s.im;
                }
                y_val.re += s.re * PHI[ch.index_sine].re;
                y_val.im += s.im * PHI[ch.index_sine].im;
            }
            else {
                let noise_idx = (index_noise + slot * SBR_BANDS + k - kx + 1) & 511;
                let noise = tables::SBR_NOISE_TABLE[noise_idx];
                *y_val += noise.scale(q_filt[slot][k] / RANGE);
            }
        }
        ch.index_sine = (ch.index_sine + 1) & 3;
    }
    ch.index_noise = (index_noise + km - kx) & 511;

    let end = if ch.last_env_end != 0 { (ch.last_env_end - num_time_slots) * 2 } else { 0 };
    ch.last_env_end = envelope_end;

    for (i, x) in ch.x[..end].iter_mut().enumerate() {
        x[state.k_x..].copy_from_slice(&ch.prev_y[HF_ADJ + num_time_slots * 2 + i][state.k_x..]);
    }
    for (x, y) in ch.x[end..].iter_mut().zip(ch.y[HF_ADJ + end..].iter()) {
        x[state.k_x..].copy_from_slice(&y[state.k_x..]);
    }

    ch.prev_l_a = l_a;
}

/// Shift overlap buffers for the next frame.
pub fn update_frame(ch: &mut SbrChannel, num_time_slots: usize) {
    let num_qmf_slots = num_time_slots * 2;
    let start_copy = num_qmf_slots;
    let (dst, src_region) = ch.w.split_at_mut(QMF_DELAY);
    let src_offset = start_copy - QMF_DELAY;
    dst.copy_from_slice(&src_region[src_offset..src_offset + QMF_DELAY]);

    ch.prev_y = ch.y;
    ch.old_invf_mode = ch.invf_mode;
    ch.old_bw_array = ch.bw_array;
    ch.prev_num_env = ch.num_env;
}
