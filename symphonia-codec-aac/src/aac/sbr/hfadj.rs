// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! HF adjustment (4.6.18.7).
//!
//! The envelope adjuster maps the envelope and noise floor data to the QMF subbands of the SBR
//! range (4.6.18.7.2), estimates the envelope of the HF generated signal (4.6.18.7.3), calculates
//! the levels of the additional noise and sinusoids (4.6.18.7.4) and the limited and compensated
//! gains (4.6.18.7.5), and assembles the output Y from the smoothed gains, the noise, and the
//! sinusoids (4.6.18.7.6).

mod noise;

use symphonia_core::dsp::complex::Complex;

use crate::aac::sbr::hfgen::XHigh;
use crate::aac::sbr::types::{
    FreqBandTables, MAX_ENVELOPES, NUM_TIME_SLOTS, QMF_BANDS, RATE, SbrChannelFrame, SbrHeader,
    T_HF_ADJ,
};

pub use noise::NOISE_TABLE;

/// The maximum SBR envelope time border, t_E(L_E) <= numTimeSlots + 3, since bs_var_bord_1 is a
/// 2-bit field (4.6.18.3.3).
pub const MAX_TIME_BORDER: usize = NUM_TIME_SLOTS + 3;

/// The number of QMF subsamples of the matrix Y, RATE * MAX_TIME_BORDER.
pub const Y_SLOTS: usize = RATE * MAX_TIME_BORDER;

/// The output matrix Y of the HF adjuster, indexed `[i][k]`, where row `i` holds
/// Y(k, i + t_HFAdj), 0 <= i < RATE * MAX_TIME_BORDER.
pub type YMatrix = [[Complex<f32>; QMF_BANDS]; Y_SLOTS];

/// The constant epsilon to avoid division by zero (4.6.18.2.5).
const EPS: f64 = 1.0;
/// The constant epsilon_0 of the limiter (4.6.18.7.5).
const EPS0: f64 = 1e-12;
/// limGain, indexed by bs_limiter_gains (4.6.18.7.5).
static LIM_GAIN: [f64; 4] = [0.70795, 1.0, 1.41254, 1e10];
/// The upper bound of G_Max (4.6.18.7.5).
const G_MAX_LIMIT: f64 = 1e5;
/// The upper bound of G_Boost (4.6.18.7.5).
const G_BOOST_LIMIT: f64 = 1.584893192;
/// h_SL if smoothing is used (bs_smoothing_mode = 0) (4.6.18.7.6).
const SMOOTHING_LEN: usize = 4;
/// The smoothing filter h_Smooth (4.6.18.7.6).
#[allow(clippy::excessive_precision)]
static H_SMOOTH: [f32; SMOOTHING_LEN + 1] =
    [0.33333333333333, 0.30150283239582, 0.21816949906249, 0.11516383427084, 0.03183050093751];
/// phi_Re,sin (4.6.18.7.6).
static PHI_RE_SIN: [f32; 4] = [1.0, 0.0, -1.0, 0.0];
/// phi_Im,sin (4.6.18.7.6).
static PHI_IM_SIN: [f32; 4] = [0.0, 1.0, 0.0, -1.0];
/// Envelope and noise floor scalefactors are limited to this value to keep all intermediate
/// values finite. Valid bitstreams yield values that are many orders of magnitude smaller.
const MAX_SCALEFACTOR: f64 = 1e30;

/// Persistent state and work buffers of the HF adjuster of one channel.
pub struct HfAdjuster {
    /// The last h_SL columns of the G_Temp matrix, a ring buffer indexed `[column][m]`. Column
    /// `temp_pos` is the oldest.
    g_temp: [[f32; QMF_BANDS]; SMOOTHING_LEN],
    /// The last h_SL columns of the Q_Temp matrix, a ring buffer like `g_temp`.
    q_temp: [[f32; QMF_BANDS]; SMOOTHING_LEN],
    /// The column of `g_temp` and `q_temp` holding the oldest values.
    temp_pos: usize,
    /// index_Noise.
    index_noise: usize,
    /// index_Sine.
    index_sine: usize,
    /// True if l'_A = L'_E in the previous SBR frame, i.e., l_APrev = 0.
    prev_la_at_end: bool,
    /// S'_IndexMapped(m - k'_x, L'_E - 1) of the previous SBR frame, indexed by QMF subband m.
    prev_sine: [bool; QMF_BANDS],
    /// k_x and M of the previous SBR frame.
    prev_range: Option<(usize, usize)>,
    /// G_LimBoost(m, l) of the current SBR frame, indexed `[l][m]`.
    g_lim_boost: [[f32; QMF_BANDS]; MAX_ENVELOPES],
    /// Q_M,LimBoost(m, l) of the current SBR frame, indexed `[l][m]`.
    q_lim_boost: [[f32; QMF_BANDS]; MAX_ENVELOPES],
    /// S_M,Boost(m, l) of the current SBR frame, indexed `[l][m]`.
    s_boost: [[f32; QMF_BANDS]; MAX_ENVELOPES],
    /// S_IndexMapped(m, L_E - 1) of the current SBR frame.
    s_index_last: [bool; QMF_BANDS],
}

impl Default for HfAdjuster {
    fn default() -> Self {
        Self::new()
    }
}

impl HfAdjuster {
    pub fn new() -> Self {
        HfAdjuster {
            g_temp: [[0.0; QMF_BANDS]; SMOOTHING_LEN],
            q_temp: [[0.0; QMF_BANDS]; SMOOTHING_LEN],
            temp_pos: 0,
            index_noise: 0,
            index_sine: 0,
            prev_la_at_end: false,
            prev_sine: [false; QMF_BANDS],
            prev_range: None,
            g_lim_boost: [[0.0; QMF_BANDS]; MAX_ENVELOPES],
            q_lim_boost: [[0.0; QMF_BANDS]; MAX_ENVELOPES],
            s_boost: [[0.0; QMF_BANDS]; MAX_ENVELOPES],
            s_index_last: [false; QMF_BANDS],
        }
    }

    /// Reset to the state before the first SBR frame: all quantities of the previous SBR frame are
    /// zero (4.6.18.7.1).
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Update the state for a frame without SBR processing.
    ///
    /// The quantities that describe the end of the previous SBR frame and that only apply if the
    /// next SBR frame immediately follows it (l_APrev and S'_IndexMapped) are cleared. The
    /// smoothing filter states, index_Noise, and index_Sine are kept.
    pub fn skip_frame(&mut self) {
        self.prev_la_at_end = false;
        self.prev_sine = [false; QMF_BANDS];
    }

    /// Perform HF adjustment of one SBR frame (4.6.18.7).
    ///
    /// `x_high` holds X_High for the QMF subsamples RATE * t_E(0) + t_HFAdj to
    /// RATE * t_E(L_E) + t_HFAdj - 1. The subbands of the SBR range of all rows of `y` are
    /// written: rows RATE * t_E(0) to RATE * t_E(L_E) - 1 with Y, and all other rows with zero.
    /// The other subbands of `y` are not modified.
    ///
    /// `reset` is the SBR reset flag. A change of k_x or M is also treated as a reset.
    ///
    /// The header, frequency band tables, and frame data must be consistent and within the bounds
    /// of the matrices, as checked by `channel::validate_sbr_data`.
    pub fn adjust(
        &mut self,
        header: &SbrHeader,
        bands: &FreqBandTables,
        frame: &SbrChannelFrame,
        reset: bool,
        x_high: &XHigh,
        y: &mut YMatrix,
    ) {
        let reset = reset || self.prev_range != Some((bands.kx, bands.m));

        if reset {
            self.index_noise = 0;
        }

        for l in 0..frame.num_env {
            self.calculate_gains(header, bands, frame, l, x_high);
        }

        let sbr_range = bands.kx..bands.kx + bands.m;
        let rows = RATE * frame.t_env[0]..RATE * frame.t_env[frame.num_env];
        for (i, row) in y.iter_mut().enumerate() {
            if !rows.contains(&i) {
                row[sbr_range.clone()].fill(Complex::new(0.0, 0.0));
            }
        }

        self.assemble(header, bands, frame, reset, x_high, y);

        // Save the quantities needed by the next SBR frame.
        self.prev_la_at_end = frame.l_a == Some(frame.num_env);
        self.prev_sine = [false; QMF_BANDS];
        self.prev_sine[bands.kx..bands.kx + bands.m].copy_from_slice(&self.s_index_last[..bands.m]);
        self.prev_range = Some((bands.kx, bands.m));
    }

    /// Calculate G_LimBoost, Q_M,LimBoost and S_M,Boost of envelope `l` (4.6.18.7.2 to
    /// 4.6.18.7.5).
    fn calculate_gains(
        &mut self,
        header: &SbrHeader,
        bands: &FreqBandTables,
        frame: &SbrChannelFrame,
        l: usize,
        x_high: &XHigh,
    ) {
        let kx = bands.kx;
        let m_len = bands.m;

        // The frequency band table F(., r(l)) of the envelope.
        let table = bands.envelope_table(frame.freq_res[l]);

        let start = RATE * frame.t_env[l];
        let end = RATE * frame.t_env[l + 1];
        let num_slots = (end - start) as f64;

        // True if l = l_A or l = l_APrev.
        let transient = is_transient(frame, l, self.prev_la_at_end);

        // 4.6.18.7.2 Mapping.
        //
        // E_OrigMapped(m - k_x, l) = E_Orig(i, l), F(i, r(l)) <= m < F(i + 1, r(l)).
        let mut e_orig = [0.0f64; QMF_BANDS];
        for (band, &e) in table.windows(2).zip(frame.e_orig[l].iter()) {
            e_orig[band[0] - kx..band[1] - kx].fill(sanitize(e));
        }

        // Q_Mapped(m - k_x, l) = Q_Orig(i, k(l)), f_TableNoise(i) <= m < f_TableNoise(i + 1).
        let mut q_mapped = [0.0f64; QMF_BANDS];
        let noise_floor = noise_floor_index(frame, l);
        for (band, &q) in bands.f_table_noise.windows(2).zip(frame.q_orig[noise_floor].iter()) {
            q_mapped[band[0] - kx..band[1] - kx].fill(sanitize(q));
        }

        // S_IndexMapped(m - k_x, l): an additional sinusoid is placed in the middle subband of each
        // high frequency resolution band with bs_add_harmonic(i) = 1, starting from envelope l_A
        // unless the sinusoid was already present at the end of the previous frame.
        let after_la = frame.l_a.is_none_or(|l_a| l >= l_a);

        let mut s_index = [false; QMF_BANDS];
        for (band, &add) in bands.f_table_high.windows(2).zip(frame.add_harmonic.iter()) {
            let mid = (band[0] + band[1]) / 2;
            if add && (after_la || self.prev_sine[mid]) {
                s_index[mid - kx] = true;
            }
        }

        // S_Mapped(m - k_x, l): one for all subbands of the envelope frequency bands containing an
        // additional sinusoid.
        let mut s_mapped = [false; QMF_BANDS];
        for band in table.windows(2) {
            let range = band[0] - kx..band[1] - kx;
            if s_index[range.clone()].iter().any(|&s| s) {
                s_mapped[range].fill(true);
            }
        }

        // 4.6.18.7.3 Estimation of the current envelope.
        let rows = &x_high[start + T_HF_ADJ..end + T_HF_ADJ];

        let mut e_curr = [0.0f64; QMF_BANDS];
        if header.bs_interpol_freq {
            // The energies of all subbands are summed together, in the order of increasing time.
            for row in rows {
                for (e, &x) in e_curr[..m_len].iter_mut().zip(&row[kx..kx + m_len]) {
                    *e += energy(x);
                }
            }
            for e in e_curr[..m_len].iter_mut() {
                *e /= num_slots;
            }
        }
        else {
            for band in table.windows(2) {
                let sum: f64 = rows
                    .iter()
                    .flat_map(|row| row[band[0]..band[1]].iter())
                    .map(|&x| energy(x))
                    .sum();
                let e = sum / (num_slots * (band[1] - band[0]) as f64);
                e_curr[band[0] - kx..band[1] - kx].fill(e);
            }
        }

        // 4.6.18.7.4 Levels of the additional noise, Q_M, and sinusoids, S_M.
        // 4.6.18.7.5 The gain G.
        let delta = if transient { 0.0 } else { 1.0 };

        let mut q_m = [0.0f64; QMF_BANDS];
        let mut s_m = [0.0f64; QMF_BANDS];
        let mut g = [0.0f64; QMF_BANDS];

        for m in 0..m_len {
            let (e, q, c) = (e_orig[m], q_mapped[m], e_curr[m]);

            q_m[m] = (e * q / (1.0 + q)).sqrt();
            s_m[m] = if s_index[m] { (e / (1.0 + q)).sqrt() } else { 0.0 };
            g[m] = if s_mapped[m] {
                (e / (EPS + c) * (q / (1.0 + q))).sqrt()
            }
            else {
                (e / ((EPS + c) * (1.0 + delta * q))).sqrt()
            };
        }

        // The limiter band k(m) of each subband, f_TableLim(k(m)) <= m + k_x <
        // f_TableLim(k(m) + 1). Subbands outside of the limiter frequency band table are assigned
        // to the first or last limiter band.
        let f_lim = &bands.f_table_lim;
        let num_lim = f_lim.len() - 1;

        let mut lim_band = [0usize; QMF_BANDS];
        let mut k = 0;
        for (m, lim_band) in lim_band[..m_len].iter_mut().enumerate() {
            while k + 1 < num_lim && f_lim[k + 1] <= m + kx {
                k += 1;
            }
            *lim_band = k;
        }

        // G_Max for each limiter band.
        let lim_gain = LIM_GAIN[usize::from(header.bs_limiter_gains.min(3))];

        let mut g_max = [0.0f64; QMF_BANDS];
        for (g_max, band) in g_max.iter_mut().zip(f_lim.windows(2)) {
            let range = band[0] - kx..band[1] - kx;
            let sum_orig: f64 = e_orig[range.clone()].iter().sum();
            let sum_curr: f64 = e_curr[range].iter().sum();
            *g_max = (((EPS0 + sum_orig) / (EPS0 + sum_curr)).sqrt() * lim_gain).min(G_MAX_LIMIT);
        }

        // The limited noise level Q_M,Lim and gain G_Lim.
        let mut q_m_lim = [0.0f64; QMF_BANDS];
        let mut g_lim = [0.0f64; QMF_BANDS];

        for m in 0..m_len {
            let g_max = g_max[lim_band[m]];

            // min(Q_M, Q_M * G_Max / G) and min(G, G_Max).
            if g[m] > g_max {
                q_m_lim[m] = q_m[m] * (g_max / g[m]);
                g_lim[m] = g_max;
            }
            else {
                q_m_lim[m] = q_m[m];
                g_lim[m] = g[m];
            }
        }

        // The compensation G_Boost for each limiter band.
        let mut g_boost = [0.0f64; QMF_BANDS];
        for (g_boost, band) in g_boost.iter_mut().zip(f_lim.windows(2)) {
            let mut num = EPS0;
            let mut den = EPS0;

            for m in band[0] - kx..band[1] - kx {
                num += e_orig[m];
                den += e_curr[m] * g_lim[m] * g_lim[m] + s_m[m] * s_m[m];

                // delta(S_M(m, l), l) is zero if S_M(m, l) != 0, l = l_A or l = l_APrev.
                if s_m[m] == 0.0 && !transient {
                    den += q_m_lim[m] * q_m_lim[m];
                }
            }

            *g_boost = (num / den).sqrt().min(G_BOOST_LIMIT);
        }

        // Non-finite values can only result from a non-finite HF generated signal. They are
        // replaced by zero to keep them out of the smoothing filter state.
        for m in 0..m_len {
            let g_boost = g_boost[lim_band[m]];

            self.g_lim_boost[l][m] = finite_or_zero(g_lim[m] * g_boost);
            self.q_lim_boost[l][m] = finite_or_zero(q_m_lim[m] * g_boost);
            self.s_boost[l][m] = finite_or_zero(s_m[m] * g_boost);
        }

        if l + 1 == frame.num_env {
            self.s_index_last = s_index;
        }
    }

    /// Assemble the HF signal Y from the smoothed gains, the noise, and the sinusoids
    /// (4.6.18.7.6).
    fn assemble(
        &mut self,
        header: &SbrHeader,
        bands: &FreqBandTables,
        frame: &SbrChannelFrame,
        reset: bool,
        x_high: &XHigh,
        y: &mut YMatrix,
    ) {
        let kx = bands.kx;
        let m_len = bands.m;

        // h_SL = 4 if bs_smoothing_mode = 0, otherwise 0.
        let smoothing = !header.bs_smoothing_mode;

        // On reset, the first h_SL columns of G_Temp and Q_Temp are G_LimBoost(m, 0) and
        // Q_M,LimBoost(m, 0). Otherwise, they are the last h_SL columns of the previous frame.
        if reset {
            for (g, q) in self.g_temp.iter_mut().zip(self.q_temp.iter_mut()) {
                g[..m_len].copy_from_slice(&self.g_lim_boost[0][..m_len]);
                q[..m_len].copy_from_slice(&self.q_lim_boost[0][..m_len]);
            }
        }

        let first = RATE * frame.t_env[0];

        // The smoothed gains G_Filt(m, i) and noise levels Q_Filt(m, i) of a QMF subsample.
        let mut g_filt = [0.0f32; QMF_BANDS];
        let mut q_filt = [0.0f32; QMF_BANDS];
        let g_filt = &mut g_filt[..m_len];
        let q_filt = &mut q_filt[..m_len];

        for l in 0..frame.num_env {
            let transient = is_transient(frame, l, self.prev_la_at_end);

            let g_lim_boost = &self.g_lim_boost[l][..m_len];
            let q_lim_boost = &self.q_lim_boost[l][..m_len];
            let s_boost = &self.s_boost[l][..m_len];
            let has_sines = s_boost.iter().any(|&s| s != 0.0);

            for i in RATE * frame.t_env[l]..RATE * frame.t_env[l + 1] {
                // The previous h_SL columns of G_Temp and Q_Temp, newest first.
                let pos = self.temp_pos;
                let [g1, g2, g3, g4] =
                    [(pos + 3) % 4, (pos + 2) % 4, (pos + 1) % 4, pos].map(|c| &self.g_temp[c]);
                let [q1, q2, q3, q4] =
                    [(pos + 3) % 4, (pos + 2) % 4, (pos + 1) % 4, pos].map(|c| &self.q_temp[c]);

                // G_Filt(m, i) = sum_j G_Temp(m, i - j) * h_Smooth(j).
                if smoothing && !transient {
                    let hist = g1.iter().zip(g2).zip(g3.iter().zip(g4));
                    for ((g, &g0), ((&g1, &g2), (&g3, &g4))) in
                        g_filt.iter_mut().zip(g_lim_boost).zip(hist)
                    {
                        let mut acc = H_SMOOTH[0] * g0;
                        acc += H_SMOOTH[1] * g1;
                        acc += H_SMOOTH[2] * g2;
                        acc += H_SMOOTH[3] * g3;
                        acc += H_SMOOTH[4] * g4;
                        *g = acc;
                    }
                }
                else {
                    g_filt.copy_from_slice(g_lim_boost);
                }

                // Q_Filt(m, i), which is zero in transient envelopes and in subbands with an
                // additional sinusoid.
                if transient {
                    q_filt.fill(0.0);
                }
                else if smoothing {
                    let hist = q1.iter().zip(q2).zip(q3.iter().zip(q4));
                    for (((q, &q0), &s), ((&q1, &q2), (&q3, &q4))) in
                        q_filt.iter_mut().zip(q_lim_boost).zip(s_boost).zip(hist)
                    {
                        let mut acc = H_SMOOTH[0] * q0;
                        acc += H_SMOOTH[1] * q1;
                        acc += H_SMOOTH[2] * q2;
                        acc += H_SMOOTH[3] * q3;
                        acc += H_SMOOTH[4] * q4;
                        *q = if s != 0.0 { 0.0 } else { acc };
                    }
                }
                else {
                    for ((q, &q0), &s) in q_filt.iter_mut().zip(q_lim_boost).zip(s_boost) {
                        *q = if s != 0.0 { 0.0 } else { q0 };
                    }
                }

                let x_row = &x_high[i + T_HF_ADJ][kx..kx + m_len];
                let y_row = &mut y[i][kx..kx + m_len];

                // W_1 = G_Filt * X_High, and W_2 = W_1 + Q_Filt * V, where V(m, i) is
                // NOISE_TABLE(f_IndexNoise(i)), f_IndexNoise(i) = (index_Noise + (i - RATE *
                // t_E(0)) * M + m + 1) mod 512. The noise table indices wrap around at most once.
                let noise_start = (self.index_noise + (i - first) * m_len + 1) % NOISE_TABLE.len();
                let wrap = m_len.min(NOISE_TABLE.len() - noise_start);
                let noise_parts =
                    [(0, &NOISE_TABLE[noise_start..noise_start + wrap]), (wrap, &NOISE_TABLE[..])];

                for (start, noise) in noise_parts {
                    let y = &mut y_row[start..];
                    let x = &x_row[start..];
                    let filt = g_filt[start..].iter().zip(&q_filt[start..]);
                    for (((y, &x), (&g, &q)), noise) in y.iter_mut().zip(x).zip(filt).zip(noise) {
                        let mut w = x * g;
                        w.re += q * noise[0];
                        w.im += q * noise[1];
                        *y = w;
                    }
                }

                // Y = W_2 + psi, where the sinusoids have the phase f_IndexSine(i) = (index_Sine +
                // i - RATE * t_E(0)) mod 4 and the sign of the imaginary part alternates with the
                // subband index.
                if has_sines {
                    let index_sine = (self.index_sine + i - first) % 4;
                    let (re, im) = (PHI_RE_SIN[index_sine], PHI_IM_SIN[index_sine]);
                    for (m, (y, &s)) in y_row.iter_mut().zip(s_boost).enumerate() {
                        if s != 0.0 {
                            let sign = if (m + kx) % 2 == 0 { 1.0 } else { -1.0 };
                            y.re += s * re;
                            y.im += s * sign * im;
                        }
                    }
                }

                // Append the current column to the G_Temp and Q_Temp history.
                self.g_temp[pos][..m_len].copy_from_slice(g_lim_boost);
                self.q_temp[pos][..m_len].copy_from_slice(q_lim_boost);
                self.temp_pos = (pos + 1) % SMOOTHING_LEN;
            }
        }

        // index_Noise is the last f_IndexNoise of this frame, and index_Sine is the last
        // f_IndexSine of this frame plus one.
        let num_slots = RATE * (frame.t_env[frame.num_env] - frame.t_env[0]);

        self.index_noise = (self.index_noise + num_slots * m_len) % NOISE_TABLE.len();
        self.index_sine = (self.index_sine + num_slots) % 4;
    }
}

/// Returns true if envelope `l` is the envelope l_A or l_APrev, where l_APrev = 0 if l'_A = L'_E,
/// otherwise -1 (4.6.18.7.5).
fn is_transient(frame: &SbrChannelFrame, l: usize, prev_la_at_end: bool) -> bool {
    frame.l_a == Some(l) || (l == 0 && prev_la_at_end)
}

/// Get the noise floor k(l) of envelope `l`, t_Q(k(l)) <= t_E(l) and t_E(l + 1) <= t_Q(k(l) + 1)
/// (4.6.18.7.2).
///
/// For an inconsistent time grid, the last noise floor starting at or before t_E(l) is used.
fn noise_floor_index(frame: &SbrChannelFrame, l: usize) -> usize {
    let t = frame.t_env[l];
    (1..frame.num_noise).rev().find(|&q| frame.t_noise[q] <= t).unwrap_or(0)
}

/// Limit an envelope or noise floor scalefactor to a non-negative finite value.
fn sanitize(value: f32) -> f64 {
    let value = f64::from(value);
    if value > 0.0 { value.min(MAX_SCALEFACTOR) } else { 0.0 }
}

/// Convert a level or gain to `f32`, replacing non-finite values by zero.
fn finite_or_zero(value: f64) -> f32 {
    let value = value as f32;
    if value.is_finite() { value } else { 0.0 }
}

/// The energy of a complex QMF subband sample.
fn energy(x: Complex<f32>) -> f64 {
    let re = f64::from(x.re);
    let im = f64::from(x.im);
    re * re + im * im
}

#[cfg(test)]
mod tests {
    use symphonia_core::dsp::complex::Complex;

    use super::{HfAdjuster, NOISE_TABLE, Y_SLOTS, YMatrix, noise_floor_index};
    use crate::aac::sbr::hfgen::{X_SLOTS, XHigh};
    use crate::aac::sbr::types::{FreqBandTables, QMF_BANDS, SbrChannelFrame, SbrHeader};

    type C64 = Complex<f64>;

    struct Lcg(u64);

    impl Lcg {
        /// A uniformly distributed value in [0, 1).
        fn uniform(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }

        /// A uniformly distributed integer in [0, n).
        fn below(&mut self, n: usize) -> usize {
            ((self.uniform() * n as f64) as usize).min(n - 1)
        }

        fn chance(&mut self, p: f64) -> bool {
            self.uniform() < p
        }
    }

    fn header(interpol: bool, smoothing_mode: bool, limiter_gains: u8) -> SbrHeader {
        SbrHeader {
            bs_amp_res: true,
            bs_start_freq: 5,
            bs_stop_freq: 9,
            bs_xover_band: 0,
            bs_freq_scale: 2,
            bs_alter_scale: true,
            bs_noise_bands: 2,
            bs_limiter_bands: 2,
            bs_limiter_gains: limiter_gains,
            bs_interpol_freq: interpol,
            bs_smoothing_mode: smoothing_mode,
        }
    }

    /// Synthetic frequency band tables for k_x = 21 and M = 23. f_TableLow and f_TableNoise are
    /// derived from f_TableHigh as in 4.6.18.3.2.2 (N_High = 10, N_Low = 5, N_Q = 2).
    fn tables_odd(lim: &[usize]) -> FreqBandTables {
        FreqBandTables {
            k0: 13,
            k2: 44,
            kx: 21,
            m: 23,
            f_master: vec![21, 23, 25, 27, 29, 31, 33, 35, 37, 40, 44],
            f_table_high: vec![21, 23, 25, 27, 29, 31, 33, 35, 37, 40, 44],
            f_table_low: vec![21, 25, 29, 33, 37, 44],
            f_table_noise: vec![21, 29, 44],
            f_table_lim: lim.to_vec(),
            patches: vec![],
        }
    }

    /// Synthetic frequency band tables for k_x = 20 and M = 24. f_TableLow and f_TableNoise are
    /// derived from f_TableHigh as in 4.6.18.3.2.2 (N_High = 11, N_Low = 6, N_Q = 2).
    fn tables(lim: &[usize]) -> FreqBandTables {
        FreqBandTables {
            k0: 14,
            k2: 44,
            kx: 20,
            m: 24,
            f_master: vec![20, 22, 24, 26, 28, 30, 32, 34, 36, 38, 41, 44],
            f_table_high: vec![20, 22, 24, 26, 28, 30, 32, 34, 36, 38, 41, 44],
            f_table_low: vec![20, 22, 26, 30, 34, 38, 44],
            f_table_noise: vec![20, 30, 44],
            f_table_lim: lim.to_vec(),
            patches: vec![],
        }
    }

    /// A literal (slow) evaluation of 4.6.18.7 using the matrices of the specification.
    struct Reference {
        /// The full G_Temp and Q_Temp matrices of the previous frame, indexed `[m][column]`.
        g_temp_prev: Vec<Vec<f64>>,
        q_temp_prev: Vec<Vec<f64>>,
        index_noise: usize,
        index_sine: usize,
        /// l'_A (-1 if not present) and L'_E.
        la_prev: i64,
        le_prev: i64,
        /// S'_IndexMapped(., L'_E - 1) indexed by QMF subband.
        s_index_mapped_prev: [f64; 64],
        /// Counters of the exercised cases: limited gains, capped boosts, sinusoids, transient
        /// envelopes, and smoothed gains.
        num_limited: usize,
        num_boost_capped: usize,
        num_sines: usize,
        num_transients: usize,
        num_smoothed: usize,
    }

    impl Reference {
        fn new() -> Self {
            Reference {
                g_temp_prev: vec![vec![]; 64],
                q_temp_prev: vec![vec![]; 64],
                index_noise: 0,
                index_sine: 0,
                la_prev: -1,
                le_prev: 0,
                s_index_mapped_prev: [0.0; 64],
                num_limited: 0,
                num_boost_capped: 0,
                num_sines: 0,
                num_transients: 0,
                num_smoothed: 0,
            }
        }

        fn process(
            &mut self,
            h: &SbrHeader,
            b: &FreqBandTables,
            f: &SbrChannelFrame,
            reset: bool,
            x_high: &XHigh,
        ) -> Vec<Vec<C64>> {
            const RATE: usize = 2;
            const T_HFADJ: usize = 2;
            let kx = b.kx;
            let mm = b.m;
            let le = f.num_env;
            let t_e = &f.t_env[..=le];
            let t_q = &f.t_noise[..=f.num_noise];
            let l_a: i64 = f.l_a.map_or(-1, |v| v as i64);
            let l_aprev: i64 = if self.la_prev == self.le_prev { 0 } else { -1 };
            let table = |l: usize| if f.freq_res[l] { &b.f_table_high } else { &b.f_table_low };
            let n_q = b.f_table_noise.len() - 1;
            let fh = &b.f_table_high;
            let n_high = fh.len() - 1;

            if reset {
                self.index_noise = 0;
            }

            // 4.6.18.7.2 Mapping.
            let mut e_orig_mapped = vec![vec![0.0; le]; mm];
            let mut q_mapped = vec![vec![0.0; le]; mm];
            for l in 0..le {
                let t = table(l);
                for i in 0..t.len() - 1 {
                    for m in t[i]..t[i + 1] {
                        e_orig_mapped[m - kx][l] = f64::from(f.e_orig[l][i]);
                    }
                }
                let k = (0..f.num_noise)
                    .find(|&k| {
                        RATE * t_e[l] >= RATE * t_q[k] && RATE * t_e[l + 1] <= RATE * t_q[k + 1]
                    })
                    .expect("noise floor");
                for i in 0..n_q {
                    for m in b.f_table_noise[i]..b.f_table_noise[i + 1] {
                        q_mapped[m - kx][l] = f64::from(f.q_orig[k][i]);
                    }
                }
            }

            let mut s_index_mapped = vec![vec![0.0; le]; mm];
            for l in 0..le {
                for i in 0..n_high {
                    let m = (fh[i + 1] + fh[i]) / 2;
                    let s_index = if f.add_harmonic[i] { 1.0 } else { 0.0 };
                    let delta_step = if l as i64 >= l_a || self.s_index_mapped_prev[m] == 1.0 {
                        1.0
                    }
                    else {
                        0.0
                    };
                    s_index_mapped[m - kx][l] = s_index * delta_step;
                }
            }

            let mut s_mapped = vec![vec![0.0; le]; mm];
            for l in 0..le {
                let t = table(l);
                for i in 0..t.len() - 1 {
                    let delta_s = (t[i]..t[i + 1]).any(|j| s_index_mapped[j - kx][l] == 1.0);
                    for m in t[i]..t[i + 1] {
                        s_mapped[m - kx][l] = if delta_s { 1.0 } else { 0.0 };
                    }
                }
            }

            // 4.6.18.7.3 Estimation of current envelope.
            let x = |k: usize, i: usize| {
                C64::new(f64::from(x_high[i][k].re), f64::from(x_high[i][k].im))
            };
            let mut e_curr = vec![vec![0.0; le]; mm];
            for l in 0..le {
                let i0 = RATE * t_e[l] + T_HFADJ;
                let i1 = RATE * t_e[l + 1] - 1 + T_HFADJ;
                let dt = (RATE * t_e[l + 1] - RATE * t_e[l]) as f64;
                if h.bs_interpol_freq {
                    for m in 0..mm {
                        let sum: f64 = (i0..=i1).map(|i| x(m + kx, i).norm_sqr()).sum();
                        e_curr[m][l] = sum / dt;
                    }
                }
                else {
                    let t = table(l);
                    for p in 0..t.len() - 1 {
                        let k_l = t[p];
                        let k_h = t[p + 1] - 1;
                        let mut sum = 0.0;
                        for i in i0..=i1 {
                            for j in k_l..=k_h {
                                sum += x(j, i).norm_sqr();
                            }
                        }
                        for k in k_l..=k_h {
                            e_curr[k - kx][l] = sum / (dt * (k_h - k_l + 1) as f64);
                        }
                    }
                }
            }

            // 4.6.18.7.4 Calculation of levels of additional HF signal components.
            let mut q_m = vec![vec![0.0; le]; mm];
            let mut s_m = vec![vec![0.0; le]; mm];
            for m in 0..mm {
                for l in 0..le {
                    let (e, q) = (e_orig_mapped[m][l], q_mapped[m][l]);
                    q_m[m][l] = (e * q / (1.0 + q)).sqrt();
                    s_m[m][l] = (e * s_index_mapped[m][l] / (1.0 + q)).sqrt();
                }
            }

            // 4.6.18.7.5 Calculation of gain.
            let delta = |l: usize| if l as i64 == l_a || l as i64 == l_aprev { 0.0 } else { 1.0 };
            let mut g = vec![vec![0.0; le]; mm];
            for m in 0..mm {
                for l in 0..le {
                    let (e, c, q) = (e_orig_mapped[m][l], e_curr[m][l], q_mapped[m][l]);
                    g[m][l] = if s_mapped[m][l] == 0.0 {
                        (e / ((1.0 + c) * (1.0 + delta(l) * q))).sqrt()
                    }
                    else {
                        (e / (1.0 + c) * q / (1.0 + q)).sqrt()
                    };
                }
            }

            let lim = &b.f_table_lim;
            let n_l = lim.len() - 1;
            let lim_gain = [0.70795, 1.0, 1.41254, 1e10][usize::from(h.bs_limiter_gains)];
            let k_of_m = |m: usize| {
                (0..n_l).find(|&k| lim[k] <= m + kx && m + kx < lim[k + 1]).expect("limiter band")
            };
            let eps0 = 1e-12;

            let mut g_max_temp = vec![vec![0.0; le]; n_l];
            for k in 0..n_l {
                for l in 0..le {
                    let mut num = eps0;
                    let mut den = eps0;
                    for i in lim[k] - kx..=lim[k + 1] - 1 - kx {
                        num += e_orig_mapped[i][l];
                        den += e_curr[i][l];
                    }
                    g_max_temp[k][l] = (num / den).sqrt() * lim_gain;
                }
            }

            let mut q_m_lim = vec![vec![0.0; le]; mm];
            let mut g_lim = vec![vec![0.0; le]; mm];
            for m in 0..mm {
                for l in 0..le {
                    let g_max = g_max_temp[k_of_m(m)][l].min(1e5);
                    if g[m][l] > g_max {
                        self.num_limited += 1;
                    }
                    q_m_lim[m][l] = q_m[m][l].min(q_m[m][l] * g_max / g[m][l]);
                    g_lim[m][l] = g[m][l].min(g_max);
                }
            }

            let mut g_boost_temp = vec![vec![0.0; le]; n_l];
            for k in 0..n_l {
                for l in 0..le {
                    let mut num = eps0;
                    let mut den = eps0;
                    for i in lim[k] - kx..=lim[k + 1] - 1 - kx {
                        num += e_orig_mapped[i][l];
                        let no_noise = s_m[i][l] != 0.0 || l as i64 == l_a || l as i64 == l_aprev;
                        let delta_s = if no_noise { 0.0 } else { 1.0 };
                        den += e_curr[i][l] * g_lim[i][l].powi(2)
                            + s_m[i][l].powi(2)
                            + delta_s * q_m_lim[i][l].powi(2);
                    }
                    g_boost_temp[k][l] = (num / den).sqrt();
                }
            }

            let mut g_lim_boost = vec![vec![0.0; le]; mm];
            let mut q_m_lim_boost = vec![vec![0.0; le]; mm];
            let mut s_m_boost = vec![vec![0.0; le]; mm];
            for m in 0..mm {
                for l in 0..le {
                    let g_boost = g_boost_temp[k_of_m(m)][l].min(1.584893192);
                    if g_boost_temp[k_of_m(m)][l] > 1.584893192 {
                        self.num_boost_capped += 1;
                    }
                    if s_m[m][l] != 0.0 {
                        self.num_sines += 1;
                    }
                    g_lim_boost[m][l] = g_lim[m][l] * g_boost;
                    q_m_lim_boost[m][l] = q_m_lim[m][l] * g_boost;
                    s_m_boost[m][l] = s_m[m][l] * g_boost;
                }
            }

            // 4.6.18.7.6 Assembling HF signals.
            let h_sl = if h.bs_smoothing_mode { 0 } else { 4 };
            let h_smooth = [
                0.33333333333333,
                0.30150283239582,
                0.21816949906249,
                0.11516383427084,
                0.03183050093751,
            ];
            let i_start = RATE * t_e[0];
            let i_end = RATE * t_e[le];
            let n = i_end - i_start;

            // G_Temp(m, i + h_SL) is stored at column i + h_SL - RATE * t_E(0).
            let mut g_temp = vec![vec![0.0; n + h_sl]; mm];
            let mut q_temp = vec![vec![0.0; n + h_sl]; mm];
            for m in 0..mm {
                for c in 0..h_sl {
                    if reset {
                        g_temp[m][c] = g_lim_boost[m][0];
                        q_temp[m][c] = q_m_lim_boost[m][0];
                    }
                    else {
                        let prev = &self.g_temp_prev[m];
                        g_temp[m][c] = prev[prev.len() - h_sl + c];
                        let prev = &self.q_temp_prev[m];
                        q_temp[m][c] = prev[prev.len() - h_sl + c];
                    }
                }
                for l in 0..le {
                    for i in RATE * t_e[l]..RATE * t_e[l + 1] {
                        g_temp[m][i + h_sl - i_start] = g_lim_boost[m][l];
                        q_temp[m][i + h_sl - i_start] = q_m_lim_boost[m][l];
                    }
                }
            }

            let mut y = vec![vec![C64::new(0.0, 0.0); QMF_BANDS]; Y_SLOTS];
            let mut last_noise = self.index_noise;
            let mut last_sine = (self.index_sine + 3) % 4;

            for l in 0..le {
                let li = l as i64;
                if li == l_a || li == l_aprev {
                    self.num_transients += 1;
                }
                else if h_sl != 0 {
                    self.num_smoothed += 1;
                }
                for i in RATE * t_e[l]..RATE * t_e[l + 1] {
                    for m in 0..mm {
                        let c = i + h_sl - i_start;
                        let g_filt = if li != l_a && h_sl != 0 && li != l_aprev {
                            (0..=h_sl).map(|j| g_temp[m][c - j] * h_smooth[j]).sum()
                        }
                        else {
                            g_temp[m][c]
                        };
                        let q_filt = if li != l_a && li != l_aprev && s_m_boost[m][l] == 0.0 {
                            if h_sl == 0 {
                                q_temp[m][i - i_start]
                            }
                            else {
                                (0..=h_sl).map(|j| q_temp[m][c - j] * h_smooth[j]).sum()
                            }
                        }
                        else {
                            0.0
                        };

                        let w1 = x(m + kx, i + T_HFADJ) * g_filt;
                        let f_noise = (self.index_noise + (i - i_start) * mm + m + 1) % 512;
                        let v = NOISE_TABLE[f_noise];
                        let w2 = w1 + C64::new(q_filt * f64::from(v[0]), q_filt * f64::from(v[1]));
                        let f_sine = (self.index_sine + i - i_start) % 4;
                        let sign = if (m + kx) % 2 == 0 { 1.0 } else { -1.0 };
                        let psi = C64::new(
                            s_m_boost[m][l] * [1.0, 0.0, -1.0, 0.0][f_sine],
                            s_m_boost[m][l] * sign * [0.0, 1.0, 0.0, -1.0][f_sine],
                        );
                        y[i][m + kx] = w2 + psi;
                        last_noise = f_noise;
                        last_sine = f_sine;
                    }
                }
            }

            self.index_noise = last_noise;
            self.index_sine = (last_sine + 1) % 4;
            self.g_temp_prev = g_temp;
            self.q_temp_prev = q_temp;
            self.g_temp_prev.resize(64, vec![]);
            self.q_temp_prev.resize(64, vec![]);
            self.la_prev = l_a;
            self.le_prev = le as i64;
            self.s_index_mapped_prev = [0.0; 64];
            for m in 0..mm {
                self.s_index_mapped_prev[m + kx] = s_index_mapped[m][le - 1];
            }

            y
        }
    }

    /// Generate a random, valid SBR frame for the synthetic tables.
    fn random_frame(lcg: &mut Lcg, b: &FreqBandTables) -> SbrChannelFrame {
        let mut f = SbrChannelFrame { num_env: 1 + lcg.below(4), ..Default::default() };

        // Strictly increasing time borders with t_E(0) <= 3 and t_E(L_E) <= 19 spanning at least 2
        // time slots.
        loop {
            let mut borders: Vec<usize> = vec![lcg.below(4)];
            for _ in 0..f.num_env {
                let last = *borders.last().expect("border");
                borders.push(last + 1 + lcg.below(6));
            }
            let last = *borders.last().expect("border");
            if last <= 19 && last - borders[0] >= 2 {
                f.t_env[..=f.num_env].copy_from_slice(&borders);
                break;
            }
        }

        f.num_noise = if f.num_env > 1 { 1 + lcg.below(2) } else { 1 };
        f.t_noise[0] = f.t_env[0];
        f.t_noise[f.num_noise] = f.t_env[f.num_env];
        if f.num_noise == 2 {
            f.t_noise[1] = f.t_env[1 + lcg.below(f.num_env - 1)];
        }

        f.l_a = if lcg.chance(0.5) { Some(lcg.below(f.num_env + 1)) } else { None };

        for l in 0..f.num_env {
            f.freq_res[l] = lcg.chance(0.5);
            let n = b.envelope_table(f.freq_res[l]).len() - 1;
            for k in 0..n {
                f.e_orig[l][k] =
                    if lcg.chance(0.05) { 0.0 } else { 10f64.powf(lcg.uniform() * 8.0) as f32 };
            }
        }
        for l in 0..f.num_noise {
            for k in 0..b.f_table_noise.len() - 1 {
                f.q_orig[l][k] = 2f64.powi(6 - lcg.below(31) as i32) as f32;
            }
        }
        for k in 0..b.f_table_high.len() - 1 {
            f.add_harmonic[k] = lcg.chance(0.2);
        }
        f
    }

    #[test]
    fn verify_against_direct_evaluation() {
        let mut lcg = Lcg(11);
        let mut adj = HfAdjuster::new();
        let mut reference = Reference::new();

        let mut x_high: Box<XHigh> = Box::new([[Complex::new(0.0, 0.0); QMF_BANDS]; X_SLOTS]);
        let mut y: Box<YMatrix> = Box::new([[Complex::new(0.0, 0.0); QMF_BANDS]; Y_SLOTS]);

        let all_tables =
            [tables(&[20, 26, 34, 44]), tables(&[20, 44]), tables_odd(&[21, 29, 37, 44])];

        for frame_idx in 0..300 {
            // Change the tables every 20 frames. A change of k_x or M is treated as a reset.
            let b = &all_tables[(frame_idx / 20) % all_tables.len()];
            let h = header(lcg.chance(0.5), lcg.chance(0.5), lcg.below(4) as u8);
            let f = random_frame(&mut lcg, b);
            let reset = frame_idx % 20 == 0 || lcg.chance(0.1);

            // Random HF generated signal with a different level in each subband, and some silent
            // subbands.
            for k in b.kx..b.kx + b.m {
                let scale = if lcg.chance(0.1) { 0.0 } else { 10f64.powf(lcg.uniform() * 4.0) };
                for row in x_high.iter_mut() {
                    row[k] = Complex::new(
                        ((lcg.uniform() - 0.5) * scale) as f32,
                        ((lcg.uniform() - 0.5) * scale) as f32,
                    );
                }
            }

            adj.adjust(&h, b, &f, reset, &x_high, &mut y);
            let expected = reference.process(&h, b, &f, reset, &x_high);

            // Y is defined for the subbands of the SBR range.
            for k in b.kx..b.kx + b.m {
                // The largest magnitude of the subband in this frame.
                let max = expected.iter().map(|row| row[k].norm()).fold(0.0, f64::max);
                for i in 0..Y_SLOTS {
                    let a = C64::new(f64::from(y[i][k].re), f64::from(y[i][k].im));
                    let e = expected[i][k];
                    let tol = 1e-4 * e.norm() + 1e-5 * max + 1e-9;
                    assert!(
                        (a - e).norm() <= tol,
                        "frame={} i={} k={} got={} expected={}",
                        frame_idx,
                        i,
                        k,
                        a,
                        e
                    );
                }
            }
        }

        // All relevant cases were exercised.
        assert!(reference.num_limited > 100, "{}", reference.num_limited);
        assert!(reference.num_boost_capped > 100, "{}", reference.num_boost_capped);
        assert!(reference.num_sines > 100, "{}", reference.num_sines);
        assert!(reference.num_transients > 20, "{}", reference.num_transients);
        assert!(reference.num_smoothed > 20, "{}", reference.num_smoothed);
    }

    #[test]
    fn verify_noise_floor_index() {
        let mut f = SbrChannelFrame {
            num_env: 3,
            t_env: [0, 4, 10, 16, 0, 0],
            num_noise: 2,
            t_noise: [0, 10, 16],
            ..Default::default()
        };
        assert_eq!(noise_floor_index(&f, 0), 0);
        assert_eq!(noise_floor_index(&f, 1), 0);
        assert_eq!(noise_floor_index(&f, 2), 1);
        f.num_noise = 1;
        assert_eq!(noise_floor_index(&f, 2), 0);
    }

    #[test]
    fn verify_limiter_table_not_covering_range() {
        // The limiter frequency band table may end below k_x + M (the last border can be removed by
        // the flowchart of Figure 4.41). The remaining subbands use the last limiter band.
        let b = tables(&[20, 30, 41]);
        let h = header(true, true, 0);
        let mut lcg = Lcg(5);
        let f = random_frame(&mut lcg, &b);
        let x_high: Box<XHigh> = Box::new([[Complex::new(1.0, 1.0); QMF_BANDS]; X_SLOTS]);
        let mut y: Box<YMatrix> = Box::new([[Complex::new(0.0, 0.0); QMF_BANDS]; Y_SLOTS]);
        let mut adj = HfAdjuster::new();
        adj.adjust(&h, &b, &f, true, &x_high, &mut y);
        assert!(y.iter().flatten().all(|v| v.re.is_finite() && v.im.is_finite()));
    }
}
