// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Per-channel SBR signal processing: QMF analysis of the AAC core signal, HF generation
//! (4.6.18.6), and HF adjustment (4.6.18.7).

use symphonia_core::dsp::complex::Complex;

use crate::aac::sbr::hfadj::{HfAdjuster, MAX_TIME_BORDER, Y_SLOTS, YMatrix};
use crate::aac::sbr::hfgen::{HfGenerator, X_SLOTS, XHigh, XLow};
use crate::aac::sbr::qmf::QmfAnalysis;
use crate::aac::sbr::types::{
    FreqBandTables, MAX_ENVELOPES, MAX_NOISE_BANDS, MAX_NOISE_FLOORS, MAX_PATCHES, PS_LOOKAHEAD,
    PS_LOOKAHEAD_BANDS, QMF_ANALYSIS_BANDS, QMF_BANDS, QMF_SLOTS, QmfFrame, RATE, SbrChannelFrame,
    SbrHeader, T_HF_ADJ, T_HF_GEN,
};

const ZERO: Complex<f32> = Complex { re: 0.0, im: 0.0 };

/// The maximum number of QMF subsamples by which the SBR envelopes of a frame may extend into the
/// next frame, l_Temp <= RATE * (MAX_TIME_BORDER - numTimeSlots).
const MAX_OVERLAP: usize = Y_SLOTS - QMF_SLOTS;

/// The SBR signal processing state of one audio channel.
pub struct SbrChannelProcessor {
    /// The 32-band analysis QMF bank (4.6.18.4.1).
    analysis: QmfAnalysis,
    /// W'(k, l), the analysis output of the last t_HFGen QMF subsamples of the previous frame for
    /// all 32 subbands.
    w_prev: [[Complex<f32>; QMF_ANALYSIS_BANDS]; T_HF_GEN],
    /// The matrix X_Low of the current frame (4.6.18.5).
    x_low: Box<XLow>,
    /// The matrix X_High of the current frame (4.6.18.6.3).
    x_high: Box<XHigh>,
    /// The matrix Y of the current frame (4.6.18.7.6).
    y: Box<YMatrix>,
    /// The part of Y' of the previous frame that extends into the current frame, i.e.,
    /// Y'(k, l + t_HFAdj + numTimeSlots * RATE) for 0 <= l < l_Temp.
    y_prev: [[Complex<f32>; QMF_BANDS]; MAX_OVERLAP],
    /// l_Temp = RATE * t'_E(L'_E) - numTimeSlots * RATE.
    l_temp: usize,
    /// k'_x, the first subband of the SBR range of the previous frame. After a frame without SBR
    /// processing, all 32 low band subbands are passed on, as in the pure upsampling case.
    kx_prev: usize,
    /// M', the number of subbands of the SBR range of the previous frame.
    m_prev: usize,
    /// The HF generator (4.6.18.6).
    hf_gen: HfGenerator,
    /// The HF adjuster (4.6.18.7).
    hf_adj: HfAdjuster,
}

impl Default for SbrChannelProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl SbrChannelProcessor {
    pub fn new() -> Self {
        SbrChannelProcessor {
            analysis: QmfAnalysis::new(),
            w_prev: [[ZERO; QMF_ANALYSIS_BANDS]; T_HF_GEN],
            x_low: Box::new([[ZERO; QMF_ANALYSIS_BANDS]; X_SLOTS]),
            x_high: Box::new([[ZERO; QMF_BANDS]; X_SLOTS]),
            y: Box::new([[ZERO; QMF_BANDS]; Y_SLOTS]),
            y_prev: [[ZERO; QMF_BANDS]; MAX_OVERLAP],
            // At start-up, l_Temp and k'_x are zero (4.6.18.5).
            l_temp: 0,
            kx_prev: 0,
            m_prev: 0,
            hf_gen: HfGenerator::new(),
            hf_adj: HfAdjuster::new(),
        }
    }

    /// Reset all state, as if no audio was ever processed.
    pub fn reset(&mut self) {
        self.analysis.reset();
        self.w_prev = [[ZERO; QMF_ANALYSIS_BANDS]; T_HF_GEN];
        self.l_temp = 0;
        self.kx_prev = 0;
        self.m_prev = 0;
        self.hf_gen.reset();
        self.hf_adj.reset();
    }

    /// Process one frame of one channel.
    ///
    /// `core` holds the 1024 time-domain samples of the AAC core decoder. If `header`, `bands`, and
    /// `frame` are all provided, HF generation and HF adjustment are performed. Otherwise, or if the
    /// SBR data is invalid, X contains only the low band. `reset` is the value of `SbrFrame::reset`
    /// for this frame. The result is written to `out` as documented by [`QmfFrame`].
    pub fn process(
        &mut self,
        core: &[f32],
        header: Option<&SbrHeader>,
        bands: Option<&FreqBandTables>,
        frame: Option<&SbrChannelFrame>,
        reset: bool,
        out: &mut QmfFrame,
    ) {
        let sbr = match (header, bands, frame) {
            (Some(header), Some(bands), Some(frame)) if validate_sbr_data(header, bands, frame) => {
                Some((header, bands, frame))
            }
            _ => None,
        };

        // The first subband of the SBR range of the current frame. Without SBR processing, all 32
        // subbands of the low band are used (pure upsampling).
        let kx = sbr.map_or(QMF_ANALYSIS_BANDS, |(_, bands, _)| bands.kx);

        // Build X_Low (4.6.18.5). The first t_HFGen subsamples are the last t_HFGen subsamples of
        // the analysis of the previous frame, W', for the subbands below k'_x. Without SBR
        // processing (pure upsampling), all subbands of W' are used.
        let kx_prev = if sbr.is_some() { self.kx_prev } else { QMF_ANALYSIS_BANDS };

        for (x_low, w_prev) in self.x_low[..T_HF_GEN].iter_mut().zip(self.w_prev.iter()) {
            x_low.fill(ZERO);
            x_low[..kx_prev].copy_from_slice(&w_prev[..kx_prev]);
        }

        // The remaining numTimeSlots * RATE subsamples are the analysis of the current frame, W.
        for (l, x_low) in self.x_low[T_HF_GEN..].iter_mut().enumerate() {
            let start = (QMF_ANALYSIS_BANDS * l).min(core.len());
            let end = (QMF_ANALYSIS_BANDS * (l + 1)).min(core.len());
            self.analysis.analyze(&core[start..end], x_low);
        }

        // Save W for the next frame before removing the subbands at and above k_x.
        self.w_prev.copy_from_slice(&self.x_low[QMF_SLOTS..]);

        for x_low in self.x_low[T_HF_GEN..].iter_mut() {
            x_low[kx..].fill(ZERO);
        }

        // HF generation and HF adjustment.
        if let Some((header, bands, frame)) = sbr {
            let slots = RATE * frame.t_env[0]..RATE * frame.t_env[frame.num_env];
            let num_noise_bands = bands.f_table_noise.len() - 1;

            // The HF adjuster only uses X_High for the subbands of the SBR range and the QMF
            // subsamples of the envelopes. The patches cover the SBR range from k_x on, the
            // remaining subbands of the SBR range are zero.
            let num_patched: usize = bands.patches.iter().map(|patch| patch.num_subbands).sum();
            let unpatched = bands.kx + num_patched.min(bands.m)..bands.kx + bands.m;
            for row in &mut self.x_high[slots.start + T_HF_ADJ..slots.end + T_HF_ADJ] {
                row[unpatched.clone()].fill(ZERO);
            }

            self.hf_gen.update_chirp_factors(&frame.invf_mode[..num_noise_bands]);
            self.hf_gen.generate(
                &self.x_low,
                bands.kx,
                &bands.patches,
                &bands.f_table_noise,
                slots,
                &mut self.x_high,
            );

            self.hf_adj.adjust(header, bands, frame, reset, &self.x_high, &mut self.y);
        }
        else {
            self.hf_adj.skip_frame();
        }

        // Build X (4.6.18.5). For the first l_Temp subsamples, the HF signal is the part of Y' of
        // the previous frame that extends into the current frame, with the SBR range of the
        // previous frame. Without SBR processing, X is the low band only (pure upsampling).
        let (l_temp, m) = match sbr {
            Some((_, bands, _)) => (self.l_temp, bands.m),
            None => (0, 0),
        };

        for (l, x) in out[..QMF_SLOTS].iter_mut().enumerate() {
            let (kx, m, y) = if l < l_temp {
                (self.kx_prev, self.m_prev, &self.y_prev[l])
            }
            else {
                (kx, m, &self.y[l])
            };

            x[..kx].copy_from_slice(&self.x_low[l + T_HF_ADJ][..kx]);
            x[kx..kx + m].copy_from_slice(&y[kx..kx + m]);
            x[kx + m..].fill(ZERO);
        }

        // The low band look-ahead for the PS tool (Annex 8.A.3).
        for (l, x) in out[QMF_SLOTS..QMF_SLOTS + PS_LOOKAHEAD].iter_mut().enumerate() {
            let x_low = &self.x_low[QMF_SLOTS + l + T_HF_ADJ];
            x[..PS_LOOKAHEAD_BANDS].copy_from_slice(&x_low[..PS_LOOKAHEAD_BANDS]);
            x[PS_LOOKAHEAD_BANDS..].fill(ZERO);
        }

        // Save the state needed by the next frame.
        match sbr {
            Some((_, bands, frame)) => {
                let end = RATE * frame.t_env[frame.num_env];
                self.l_temp = end.saturating_sub(QMF_SLOTS);
                self.y_prev[..self.l_temp]
                    .copy_from_slice(&self.y[QMF_SLOTS..QMF_SLOTS + self.l_temp]);
                self.kx_prev = bands.kx;
                self.m_prev = bands.m;
            }
            None => {
                self.l_temp = 0;
                self.kx_prev = QMF_ANALYSIS_BANDS;
                self.m_prev = 0;
            }
        }
    }
}

/// Check that a frequency band table has at least one band, is strictly increasing, and is within
/// `[start, end]`. If `exact` is true, the first and last borders must be `start` and `end`.
fn validate_table(table: &[usize], start: usize, end: usize, exact: bool) -> bool {
    let (Some(&first), Some(&last)) = (table.first(), table.last())
    else {
        return false;
    };

    table.len() >= 2
        && table.windows(2).all(|w| w[0] < w[1])
        && first >= start
        && last <= end
        && (!exact || (first == start && last == end))
}

/// Validate the SBR data of a frame against the constraints of the specification and the limits
/// of the signal processing buffers.
///
/// Returns false if the data cannot be processed, in which case the frame is processed as if no
/// SBR data was present.
pub fn validate_sbr_data(
    header: &SbrHeader,
    bands: &FreqBandTables,
    frame: &SbrChannelFrame,
) -> bool {
    if header.bs_limiter_gains > 3 {
        return false;
    }

    // The SBR range: k_x <= 32 and k_x + M <= 64 (4.6.18.3.6).
    let kx = bands.kx;
    let Some(end) = kx.checked_add(bands.m)
    else {
        return false;
    };

    if kx > QMF_ANALYSIS_BANDS || bands.m == 0 || end > QMF_BANDS {
        return false;
    }

    // The frequency band tables must cover the SBR range with non-empty bands. The limiter
    // frequency band table must be within the SBR range.
    if !validate_table(&bands.f_table_high, kx, end, true)
        || !validate_table(&bands.f_table_low, kx, end, true)
        || !validate_table(&bands.f_table_noise, kx, end, true)
        || !validate_table(&bands.f_table_lim, kx, end, false)
    {
        return false;
    }

    // N_Q <= 5.
    if bands.f_table_noise.len() - 1 > MAX_NOISE_BANDS {
        return false;
    }

    // numPatches <= 5, the patches must be within the SBR range, and the source subbands within
    // the low band.
    if bands.patches.len() > MAX_PATCHES {
        return false;
    }

    let mut num_patched = 0usize;
    for patch in &bands.patches {
        let Some(src_end) = patch.start_subband.checked_add(patch.num_subbands)
        else {
            return false;
        };
        if src_end > QMF_ANALYSIS_BANDS {
            return false;
        }
        num_patched = num_patched.saturating_add(patch.num_subbands);
    }

    if num_patched > bands.m {
        return false;
    }

    // 1 <= L_E <= 5 and strictly increasing time borders within the frame and its maximum
    // extension into the next frame.
    if frame.num_env == 0 || frame.num_env > MAX_ENVELOPES {
        return false;
    }

    let t_env = &frame.t_env[..=frame.num_env];
    if !t_env.windows(2).all(|w| w[0] < w[1]) || t_env[frame.num_env] > MAX_TIME_BORDER {
        return false;
    }

    // 1 <= L_Q <= 2.
    if frame.num_noise == 0 || frame.num_noise > MAX_NOISE_FLOORS {
        return false;
    }

    // bs_invf_mode is a 2-bit field.
    if frame.invf_mode[..bands.f_table_noise.len() - 1].iter().any(|&mode| mode > 3) {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use symphonia_core::dsp::complex::Complex;

    use super::{SbrChannelProcessor, validate_sbr_data};
    use crate::aac::sbr::hfadj::NOISE_TABLE;
    use crate::aac::sbr::qmf::QmfSynthesis;
    use crate::aac::sbr::types::{
        FreqBandTables, PS_LOOKAHEAD, PS_LOOKAHEAD_BANDS, Patch, QMF_BANDS, QMF_SLOTS, QmfFrame,
        SbrChannelFrame, SbrHeader,
    };

    const ZERO: Complex<f32> = Complex { re: 0.0, im: 0.0 };

    struct Lcg(u64);

    impl Lcg {
        /// A uniformly distributed value in [0, 1).
        fn uniform(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }

        /// A uniformly distributed integer in [0, n).
        fn below(&mut self, n: usize) -> usize {
            ((self.uniform() * n as f64) as usize).min(n.max(1) - 1)
        }

        fn chance(&mut self, p: f64) -> bool {
            self.uniform() < p
        }
    }

    /// NINT() for non-negative values.
    fn nint(x: f64) -> usize {
        (x + 0.5).floor() as usize
    }

    /// Derive the frequency band tables (4.6.18.3.2) and the patches (4.6.18.6.3) from an SBR
    /// header, for 32000 <= Fs_SBR < 64000.
    fn derive_tables(fs: u32, h: &SbrHeader) -> FreqBandTables {
        assert!((44100..64000).contains(&fs));
        let fs = f64::from(fs);

        // 4.6.18.3.2.1 Master frequency band table.
        let offset = [-4i32, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16, 20];
        let start_min = nint(4000.0 * 128.0 / fs) as i32;
        let k0 = (start_min + offset[usize::from(h.bs_start_freq)]) as usize;

        let stop_min = nint(8000.0 * 128.0 / fs);
        let k2 = match h.bs_stop_freq {
            14 => (2 * k0).min(64),
            15 => (3 * k0).min(64),
            stop_freq => {
                let sm = stop_min as f64;
                let mut stop_dk: Vec<usize> = (0..=12)
                    .map(|p| {
                        nint(sm * (64.0 / sm).powf((p + 1) as f64 / 13.0))
                            - nint(sm * (64.0 / sm).powf(p as f64 / 13.0))
                    })
                    .collect();
                stop_dk.sort();
                (stop_min + stop_dk[..usize::from(stop_freq)].iter().sum::<usize>()).min(64)
            }
        };
        assert!(k2 > k0);

        let f_master: Vec<usize> = if h.bs_freq_scale == 0 {
            // Figure 4.39.
            let (dk, num_bands) = if !h.bs_alter_scale {
                (1i64, 2 * ((k2 - k0) / 2) as i64)
            }
            else {
                (2i64, 2 * nint((k2 - k0) as f64 / 4.0) as i64)
            };
            let mut k2_diff = k2 as i64 - (k0 as i64 + num_bands * dk);
            let mut v_dk = vec![dk; num_bands as usize];
            let (incr, mut k) = if k2_diff < 0 { (1i64, 0i64) } else { (-1, num_bands - 1) };
            while k2_diff != 0 {
                v_dk[k as usize] -= incr;
                k += incr;
                k2_diff += incr;
            }
            let mut f = vec![k0];
            for d in v_dk {
                f.push((*f.last().expect("border") as i64 + d) as usize);
            }
            f
        }
        else {
            // Figure 4.40.
            let bands = [12.0, 10.0, 8.0][usize::from(h.bs_freq_scale - 1)];
            let warp = if h.bs_alter_scale { 1.3 } else { 1.0 };
            let two_regions = k2 as f64 / k0 as f64 > 2.2449;
            let k1 = if two_regions { 2 * k0 } else { k2 };

            let region = |ka: usize, kb: usize, num_bands: usize| -> Vec<usize> {
                (0..num_bands)
                    .map(|k| {
                        let r = kb as f64 / ka as f64;
                        nint(ka as f64 * r.powf((k + 1) as f64 / num_bands as f64))
                            - nint(ka as f64 * r.powf(k as f64 / num_bands as f64))
                    })
                    .collect()
            };

            let num_bands0 = 2 * nint(bands * (k1 as f64 / k0 as f64).ln() / (2.0 * 2f64.ln()));
            let mut v_dk0 = region(k0, k1, num_bands0);
            v_dk0.sort();
            let mut f = vec![k0];
            for d in &v_dk0 {
                f.push(f.last().expect("border") + d);
            }

            if two_regions {
                let num_bands1 =
                    2 * nint(bands * (k2 as f64 / k1 as f64).ln() / (2.0 * 2f64.ln() * warp));
                let mut v_dk1 = region(k1, k2, num_bands1);
                let max0 = *v_dk0.iter().max().expect("band");
                if *v_dk1.iter().min().expect("band") < max0 {
                    v_dk1.sort();
                    let mut change = max0 - v_dk1[0];
                    let limit = (v_dk1[num_bands1 - 1] - v_dk1[0]) / 2;
                    if change > limit {
                        change = limit;
                    }
                    v_dk1[0] += change;
                    v_dk1[num_bands1 - 1] -= change;
                }
                v_dk1.sort();
                for d in &v_dk1 {
                    f.push(f.last().expect("border") + d);
                }
            }
            f
        };
        let n_master = f_master.len() - 1;

        // 4.6.18.3.2.2 Derived frequency band tables.
        let xover = usize::from(h.bs_xover_band);
        let n_high = n_master - xover;
        let f_table_high: Vec<usize> = f_master[xover..].to_vec();
        let kx = f_table_high[0];
        let m = f_table_high[n_high] - kx;

        let n_low = n_high / 2 + (n_high - 2 * (n_high / 2));
        let f_table_low: Vec<usize> = (0..=n_low)
            .map(|k| if k == 0 { f_table_high[0] } else { f_table_high[2 * k - n_high % 2] })
            .collect();

        let n_q = nint(f64::from(h.bs_noise_bands) * (k2 as f64 / kx as f64).log2()).max(1);
        let mut i_prev = 0;
        let mut f_table_noise = vec![f_table_low[0]];
        for k in 1..=n_q {
            let i = i_prev + (n_low - i_prev) / (n_q + 1 - k);
            f_table_noise.push(f_table_low[i]);
            i_prev = i;
        }

        // 4.6.18.6.3 Patch construction (Figure 4.48).
        let mut patches: Vec<Patch> = vec![];
        let mut msb = k0;
        let mut usb = kx;
        let goal_sb = nint(2.048e6 / fs);
        let mut k = if goal_sb < kx + m {
            let mut k = 0;
            let mut i = 0;
            while f_master[i] < goal_sb {
                k = i + 1;
                i += 1;
            }
            k
        }
        else {
            n_master
        };
        loop {
            let mut j = k;
            let (sb, odd) = loop {
                let sb = f_master[j];
                let odd = (sb + k0 - 2) % 2;
                if sb + odd <= k0 - 1 + msb {
                    break (sb, odd);
                }
                j -= 1;
            };
            let num = sb.saturating_sub(usb);
            let start = k0 - odd - num;
            if num > 0 {
                patches.push(Patch { num_subbands: num, start_subband: start });
                usb = sb;
                msb = sb;
            }
            else {
                msb = kx;
            }
            if f_master[k] - sb < 3 {
                k = n_master;
            }
            if sb == kx + m {
                break;
            }
        }
        if patches.len() > 1 && patches.last().expect("patch").num_subbands < 3 {
            patches.pop();
        }

        // 4.6.18.3.2.3 Limiter frequency band table (Figure 4.41).
        let f_table_lim = if h.bs_limiter_bands == 0 {
            vec![f_table_low[0], f_table_low[n_low]]
        }
        else {
            let lim_bands = [1.2, 2.0, 3.0][usize::from(h.bs_limiter_bands - 1)];
            let mut patch_borders = vec![kx];
            for p in &patches {
                patch_borders.push(patch_borders.last().expect("border") + p.num_subbands);
            }
            let mut lim_table = f_table_low.clone();
            lim_table.extend_from_slice(&patch_borders[1..patches.len()]);
            lim_table.sort();
            let mut k = 1;
            while k < lim_table.len() {
                let n_octaves = (lim_table[k] as f64 / lim_table[k - 1] as f64).log2();
                if n_octaves * lim_bands < 0.49 {
                    if lim_table[k] == lim_table[k - 1] {
                        lim_table.remove(k);
                    }
                    else if patch_borders.contains(&lim_table[k]) {
                        if patch_borders.contains(&lim_table[k - 1]) {
                            k += 1;
                        }
                        else {
                            lim_table.remove(k - 1);
                        }
                    }
                    else {
                        lim_table.remove(k);
                    }
                }
                else {
                    k += 1;
                }
            }
            lim_table
        };

        FreqBandTables {
            k0,
            k2,
            kx,
            m,
            f_master,
            f_table_high,
            f_table_low,
            f_table_noise,
            f_table_lim,
            patches,
        }
    }

    fn header(start_freq: u8) -> SbrHeader {
        SbrHeader {
            bs_amp_res: true,
            bs_start_freq: start_freq,
            bs_stop_freq: 9,
            bs_xover_band: 0,
            bs_freq_scale: 2,
            bs_alter_scale: true,
            bs_noise_bands: 2,
            bs_limiter_bands: 2,
            bs_limiter_gains: 2,
            bs_interpol_freq: true,
            bs_smoothing_mode: true,
        }
    }

    /// A frame with the given time borders (L_E = borders.len() - 1), a single noise floor,
    /// envelope scalefactors `e` for every band, and noise floor scalefactors `q`.
    fn frame(
        b: &FreqBandTables,
        borders: &[usize],
        high_res: bool,
        e: f32,
        q: f32,
    ) -> SbrChannelFrame {
        let mut f = SbrChannelFrame { num_env: borders.len() - 1, ..Default::default() };
        f.t_env[..borders.len()].copy_from_slice(borders);
        f.num_noise = 1;
        f.t_noise[0] = borders[0];
        f.t_noise[1] = borders[f.num_env];
        for l in 0..f.num_env {
            f.freq_res[l] = high_res;
            let n = b.envelope_table(high_res).len() - 1;
            f.e_orig[l][..n].fill(e);
        }
        f.q_orig[0][..b.f_table_noise.len() - 1].fill(q);
        f
    }

    fn new_out() -> Box<QmfFrame> {
        Box::new([[ZERO; QMF_BANDS]; QMF_SLOTS + PS_LOOKAHEAD])
    }

    fn white_noise(lcg: &mut Lcg, amplitude: f64) -> Vec<f32> {
        (0..1024).map(|_| ((lcg.uniform() * 2.0 - 1.0) * amplitude) as f32).collect()
    }

    /// The energy of `out[l][k]` averaged over the QMF subsamples `l` of `rows`.
    fn subband_energy(out: &QmfFrame, k: usize, rows: std::ops::Range<usize>) -> f64 {
        let n = rows.len() as f64;
        rows.map(|l| f64::from(out[l][k].norm_sqr())).sum::<f64>() / n
    }

    #[test]
    fn verify_derived_tables() {
        let h = header(5);
        let b = derive_tables(44100, &h);
        // k0 = NINT(4000 * 128 / 44100) + offset(5) = 12 + 2.
        assert_eq!(b.k0, 14);
        assert_eq!(b.kx, 14);
        assert_eq!(b.f_table_high.first(), Some(&b.kx));
        assert_eq!(b.f_table_high.last(), Some(&(b.kx + b.m)));
        assert!(!b.patches.is_empty() && b.patches.len() <= 5);
        let num_patched: usize = b.patches.iter().map(|p| p.num_subbands).sum();
        assert!(num_patched <= b.m && num_patched + 3 > b.m);
        for p in &b.patches {
            assert!(p.start_subband + p.num_subbands <= b.k0);
        }
        assert!(validate_sbr_data(&h, &b, &frame(&b, &[0, 16], true, 1.0, 1.0)));

        // All configurations satisfying the requirements of 4.6.18.3.6 are accepted.
        let mut num_configs = 0;
        for start_freq in 0..16 {
            for &(fs, max_range) in &[(44100, 35), (48000, 32)] {
                let mut h = header(start_freq);
                for freq_scale in 0..4 {
                    h.bs_freq_scale = freq_scale;
                    h.bs_alter_scale = freq_scale % 2 == 1;
                    let b = derive_tables(fs, &h);
                    // Skip configurations violating the requirements: the SBR range is too
                    // large, too many patches, or bands of zero width in f_Master.
                    if b.k2 - b.k0 > max_range
                        || b.patches.len() > 5
                        || b.f_master.windows(2).any(|w| w[0] >= w[1])
                    {
                        continue;
                    }
                    let f = frame(&b, &[0, 16], true, 1.0, 1.0);
                    assert!(validate_sbr_data(&h, &b, &f), "{} {:?} {:?}", fs, h, b);
                    num_configs += 1;
                }
            }
        }
        assert!(num_configs > 50, "{}", num_configs);
    }

    /// Without SBR data, the low band passes through the analysis and synthesis filterbanks
    /// unchanged, delayed by the filterbank delay and t_HFGen - t_HFAdj QMF subsamples.
    #[test]
    fn verify_low_band_only_reconstruction() {
        let mut proc = SbrChannelProcessor::new();
        let mut synthesis = QmfSynthesis::new();
        let mut out = new_out();

        let freq = 0.0371;
        let num_frames = 12;
        let input: Vec<f32> = (0..1024 * num_frames)
            .map(|n| (0.5 * (2.0 * PI * freq * n as f64).sin()) as f32)
            .collect();
        let mut output = vec![0.0f32; 2048 * num_frames];

        for (i, core) in input.chunks(1024).enumerate() {
            proc.process(core, None, None, None, false, &mut out);
            for l in 0..QMF_SLOTS {
                let start = 2048 * i + 64 * l;
                synthesis.synthesize(&out[l], &mut output[start..start + 64]);
            }
        }

        // The filterbank delay (578) plus (t_HFGen - t_HFAdj) * 64 output samples.
        let delay = 578 + 6 * 64;
        let mut max_err = 0.0f64;
        for (n, &y) in output.iter().enumerate().skip(4096) {
            let expected = 0.5 * (2.0 * PI * freq / 2.0 * (n - delay) as f64).sin();
            max_err = max_err.max((f64::from(y) - expected).abs());
        }
        assert!(max_err < 2e-3, "max_err={}", max_err);
    }

    /// The PS look-ahead rows are the first QMF subsamples of the low band of the next frame.
    #[test]
    fn verify_ps_lookahead() {
        let mut lcg = Lcg(3);
        let h = header(5);
        let b = derive_tables(44100, &h);
        let f = frame(&b, &[0, 16], true, 1e6, 1e-3);

        let mut proc = SbrChannelProcessor::new();
        let mut prev = new_out();
        let mut out = new_out();

        for i in 0..6 {
            let core = white_noise(&mut lcg, 1000.0);
            let sbr = i % 3 != 2;
            if sbr {
                proc.process(&core, Some(&h), Some(&b), Some(&f), i == 0, &mut out);
            }
            else {
                proc.process(&core, None, None, None, false, &mut out);
            }

            for l in 0..PS_LOOKAHEAD {
                for k in 0..QMF_BANDS {
                    let la = out[QMF_SLOTS + l][k];
                    if k >= PS_LOOKAHEAD_BANDS {
                        assert_eq!(la, ZERO);
                    }
                    if i > 0 && k < PS_LOOKAHEAD_BANDS {
                        assert_eq!(out[l][k], prev[QMF_SLOTS + l][k]);
                    }
                }
            }
            assert!(out[QMF_SLOTS][0].norm() > 0.0);

            // Without SBR data, there is no signal above the low band.
            if !sbr {
                assert!(out[..QMF_SLOTS].iter().all(|row| row[32..].iter().all(|&x| x == ZERO)));
            }

            std::mem::swap(&mut prev, &mut out);
        }
    }

    /// The HF generator copies the low band subbands to the high band according to the patches.
    /// With strong inverse filtering, a tonal low band subband is whitened.
    #[test]
    fn verify_hf_generation() {
        let mut lcg = Lcg(4);
        let h = header(5);
        let b = derive_tables(44100, &h);
        let mut f = frame(&b, &[0, 16], true, 1e6, 1e-3);

        let mut proc = SbrChannelProcessor::new();
        let mut out = new_out();

        // Inverse filtering off: X_High is a copy of X_Low.
        let core = white_noise(&mut lcg, 1000.0);
        proc.process(&core, Some(&h), Some(&b), Some(&f), true, &mut out);

        let mut k = b.kx;
        for patch in &b.patches {
            for x in 0..patch.num_subbands {
                for l in 2..34 {
                    assert_eq!(proc.x_high[l][k], proc.x_low[l][patch.start_subband + x]);
                }
                k += 1;
            }
        }
        // Subbands above the patches are not generated.
        for kk in k..64 {
            assert!(proc.x_high.iter().all(|row| row[kk] == ZERO));
        }

        // A sine in low band subband 10. With strong inverse filtering, the energy of the patched
        // subbands is reduced by the prediction gain.
        let freq = 10.5 / 64.0;
        let sine = |frame: usize| -> Vec<f32> {
            (0..1024)
                .map(|n| (1000.0 * (2.0 * PI * freq * (1024 * frame + n) as f64).sin()) as f32)
                .collect()
        };

        f.invf_mode = [3; 5];
        let mut proc = SbrChannelProcessor::new();
        for i in 0..3 {
            proc.process(&sine(i), Some(&h), Some(&b), Some(&f), i == 0, &mut out);
        }

        let mut k = b.kx;
        let mut checked = false;
        for patch in &b.patches {
            if (patch.start_subband..patch.start_subband + patch.num_subbands).contains(&10) {
                let target = k + 10 - patch.start_subband;
                let e_low: f32 = proc.x_low[2..34].iter().map(|row| row[10].norm_sqr()).sum();
                let e_high: f32 = proc.x_high[2..34].iter().map(|row| row[target].norm_sqr()).sum();
                assert!(e_high < 1e-2 * e_low, "e_low={} e_high={}", e_low, e_high);
                checked = true;
            }
            k += patch.num_subbands;
        }
        assert!(checked);
    }

    /// The envelope adjuster restores the transmitted envelope: with frequency interpolation, the
    /// energy of every patched subband matches E_Orig; without, the energy of every band does.
    #[test]
    fn verify_envelope_energies() {
        for &interpol in &[true, false] {
            let mut lcg = Lcg(5);
            let mut h = header(5);
            h.bs_interpol_freq = interpol;
            h.bs_limiter_gains = 3;
            let b = derive_tables(44100, &h);

            let num_patched: usize = b.patches.iter().map(|p| p.num_subbands).sum();

            // Two envelopes with different resolution and levels for each band.
            let mut f = frame(&b, &[0, 8, 16], true, 0.0, 2f32.powi(-20));
            f.freq_res[1] = false;
            for l in 0..2 {
                let n = b.envelope_table(f.freq_res[l]).len() - 1;
                for k in 0..n {
                    f.e_orig[l][k] = 64.0 * 2f32.powi(10 + ((k * 7 + l * 3) % 11) as i32);
                }
            }

            let mut proc = SbrChannelProcessor::new();
            let mut out = new_out();
            for i in 0..3 {
                let core = white_noise(&mut lcg, 1000.0);
                proc.process(&core, Some(&h), Some(&b), Some(&f), i == 0, &mut out);
            }

            let mut num_checked = 0;
            for l in 0..2 {
                let table = b.envelope_table(f.freq_res[l]);
                let rows = 16 * l..16 * (l + 1);
                for (band, &e_orig) in table.windows(2).zip(f.e_orig[l].iter()) {
                    // Subbands above the last patch carry no HF generated signal.
                    if band[1] > b.kx + num_patched {
                        continue;
                    }
                    num_checked += 1;
                    let e_orig = f64::from(e_orig);
                    if interpol {
                        for k in band[0]..band[1] {
                            let e = subband_energy(&out, k, rows.clone());
                            assert!((e / e_orig - 1.0).abs() < 1e-2, "k={} {} {}", k, e, e_orig);
                        }
                    }
                    else {
                        let e: f64 = (band[0]..band[1])
                            .map(|k| subband_energy(&out, k, rows.clone()))
                            .sum::<f64>()
                            / (band[1] - band[0]) as f64;
                        assert!(
                            (e / e_orig - 1.0).abs() < 1e-2,
                            "band={:?} {} {}",
                            band,
                            e,
                            e_orig
                        );
                    }
                }
            }
            assert!(num_checked >= 10, "{}", num_checked);
        }
    }

    /// With the limiter at -3 dB, the gains are limited, but the energy of each limiter band is
    /// restored by the boost factor.
    #[test]
    fn verify_limiter_compensation() {
        let mut lcg = Lcg(6);
        let mut h = header(5);
        h.bs_limiter_gains = 0;
        let b = derive_tables(44100, &h);
        let num_patched: usize = b.patches.iter().map(|p| p.num_subbands).sum();
        let f = frame(&b, &[0, 16], true, 1e8, 2f32.powi(-20));

        let mut proc = SbrChannelProcessor::new();
        let mut out = new_out();
        for i in 0..3 {
            let core = white_noise(&mut lcg, 1000.0);
            proc.process(&core, Some(&h), Some(&b), Some(&f), i == 0, &mut out);
        }

        let mut checked = 0;
        for band in b.f_table_lim.windows(2) {
            if band[1] > b.kx + num_patched {
                continue;
            }
            let e: f64 = (band[0]..band[1]).map(|k| subband_energy(&out, k, 0..32)).sum();
            let e_orig = 1e8 * (band[1] - band[0]) as f64;
            assert!((e / e_orig - 1.0).abs() < 1e-2, "band={:?} {} {}", band, e, e_orig);
            checked += 1;
        }
        assert!(checked > 0);
    }

    /// With a silent low band, the output consists of the noise floor only. The noise sequence
    /// continues across frames.
    #[test]
    fn verify_noise_only() {
        let mut h = header(5);
        h.bs_smoothing_mode = false;
        let b = derive_tables(44100, &h);
        // E_Orig = 10^4 and Q_Orig = 1: Q_M = sqrt(E_Orig / 2) and the boost is sqrt(2), so the
        // noise level is sqrt(E_Orig) = 100.
        let f = frame(&b, &[0, 16], false, 1e4, 1.0);
        let m = b.m;

        let mut proc = SbrChannelProcessor::new();
        let mut out = new_out();
        let silence = vec![0.0f32; 1024];

        for i in 0..4 {
            proc.process(&silence, Some(&h), Some(&b), Some(&f), i == 0, &mut out);

            let mut energy = 0.0;
            for l in 0..QMF_SLOTS {
                // Global QMF subsample index since the reset.
                let n = 32 * i + l;
                for k in 0..QMF_BANDS {
                    let x = out[l][k];
                    if (b.kx..b.kx + m).contains(&k) {
                        let v = NOISE_TABLE[(n * m + (k - b.kx) + 1) % 512];
                        assert!((x.re - 100.0 * v[0]).abs() < 1e-2, "i={} l={} k={}", i, l, k);
                        assert!((x.im - 100.0 * v[1]).abs() < 1e-2, "i={} l={} k={}", i, l, k);
                        energy += f64::from(x.norm_sqr());
                    }
                    else {
                        assert_eq!(x, ZERO);
                    }
                }
            }
            let energy = energy / (QMF_SLOTS * m) as f64;
            assert!((energy / 1e4 - 1.0).abs() < 0.1, "energy={}", energy);
        }
    }

    /// Additional sinusoids are placed in the middle subband of a high resolution band, start at
    /// envelope l_A if not present in the previous frame, and have a continuous phase across
    /// frames.
    #[test]
    fn verify_sinusoids() {
        let h = header(5);
        let b = derive_tables(44100, &h);
        let band = 3;
        let mid = (b.f_table_high[band] + b.f_table_high[band + 1]) / 2;
        let e = 1e6f32;
        let q = 2f32.powi(-15);

        // The expected sinusoid level S_M,Boost (4.6.18.7.4 and 4.6.18.7.5). With a silent low
        // band, there is no limiting.
        let lim =
            b.f_table_lim.windows(2).find(|w| w[0] <= mid && mid < w[1]).expect("limiter band");
        let (e, q) = (f64::from(e), f64::from(q));
        let s_m = (e / (1.0 + q)).sqrt();
        let num = 1e-12 + e * (lim[1] - lim[0]) as f64;
        let den = 1e-12 + s_m * s_m + e * q / (1.0 + q) * (lim[1] - lim[0] - 1) as f64;
        let level = (s_m * (num / den).sqrt().min(1.584893192)) as f32;

        let sign = if mid % 2 == 0 { 1.0 } else { -1.0 };
        let check_sine = |x: Complex<f32>, n: usize| {
            let expected = match n % 4 {
                0 => Complex::new(level, 0.0),
                1 => Complex::new(0.0, sign * level),
                2 => Complex::new(-level, 0.0),
                _ => Complex::new(0.0, -sign * level),
            };
            assert!((x - expected).norm() < 1e-4 * level, "n={} x={} expected={}", n, x, expected);
        };

        let silence = vec![0.0f32; 1024];
        let mut proc = SbrChannelProcessor::new();
        let mut out = new_out();

        // Frame 0: two envelopes, l_A = 1. The sinusoid is new, so it starts at envelope 1. The
        // frame extends 1 time slot into the next frame.
        let mut f0 = frame(&b, &[0, 8, 17], true, e as f32, q as f32);
        f0.l_a = Some(1);
        f0.add_harmonic[band] = true;
        proc.process(&silence, Some(&h), Some(&b), Some(&f0), true, &mut out);

        for l in 0..16 {
            assert!(out[l][mid].norm() < 1e-2 * level);
        }
        for l in 16..32 {
            check_sine(out[l][mid], l);
        }

        // Frame 1: the sinusoid was present at the end of the previous frame, so it is present from
        // the start, despite l_A = 1.
        let mut f1 = frame(&b, &[1, 9, 16], true, e as f32, q as f32);
        f1.l_a = Some(1);
        f1.add_harmonic[band] = true;
        proc.process(&silence, Some(&h), Some(&b), Some(&f1), false, &mut out);
        for l in 0..32 {
            check_sine(out[l][mid], 32 + l);
        }

        // Frame 2: no additional sinusoid.
        let f2 = frame(&b, &[0, 16], true, e as f32, q as f32);
        proc.process(&silence, Some(&h), Some(&b), Some(&f2), false, &mut out);
        assert!(out[..QMF_SLOTS].iter().all(|row| row[mid].norm() < 1e-2 * level));
    }

    /// An SBR frame extending into the next frame provides the first QMF subsamples of the HF
    /// signal of the next frame.
    #[test]
    fn verify_frame_overlap() {
        let h = header(5);
        let b = derive_tables(44100, &h);
        let silence = vec![0.0f32; 1024];
        let mut proc = SbrChannelProcessor::new();
        let mut out = new_out();

        let band_energy = |out: &QmfFrame, rows: std::ops::Range<usize>| -> f64 {
            (b.kx..b.kx + b.m).map(|k| subband_energy(out, k, rows.clone())).sum::<f64>()
                / b.m as f64
        };

        // Frame 0 ends at time slot 18, 2 time slots (4 QMF subsamples) into the next frame.
        let f0 = frame(&b, &[0, 8, 18], false, 1e6, 1.0);
        proc.process(&silence, Some(&h), Some(&b), Some(&f0), true, &mut out);
        let e0 = band_energy(&out, 0..32);
        assert!((e0 / 1e6 - 1.0).abs() < 0.2, "{}", e0);

        // Frame 1 starts at time slot 2 with 100 times less energy.
        let f1 = frame(&b, &[2, 10, 16], false, 1e4, 1.0);
        proc.process(&silence, Some(&h), Some(&b), Some(&f1), false, &mut out);
        let e_overlap = band_energy(&out, 0..4);
        let e1 = band_energy(&out, 4..32);
        assert!((e_overlap / 1e6 - 1.0).abs() < 0.3, "{}", e_overlap);
        assert!((e1 / 1e4 - 1.0).abs() < 0.2, "{}", e1);

        // Frame 2 starts at time slot 0. Frame 1 did not extend into it.
        let f2 = frame(&b, &[0, 16], false, 1e2, 1.0);
        proc.process(&silence, Some(&h), Some(&b), Some(&f2), false, &mut out);
        let e2 = band_energy(&out, 0..32);
        assert!((e2 / 1e2 - 1.0).abs() < 0.2, "{}", e2);
    }

    /// A change of the frequency band tables: the overlapping QMF subsamples from the previous
    /// frame use the previous SBR range.
    #[test]
    fn verify_frequency_table_change() {
        let h0 = header(5);
        let b0 = derive_tables(44100, &h0);
        let h1 = header(8);
        let b1 = derive_tables(44100, &h1);
        assert!(b1.kx > b0.kx);

        let silence = vec![0.0f32; 1024];
        let mut proc = SbrChannelProcessor::new();
        let mut out = new_out();

        let f0 = frame(&b0, &[0, 17], false, 1e6, 1.0);
        proc.process(&silence, Some(&h0), Some(&b0), Some(&f0), true, &mut out);

        let f1 = frame(&b1, &[1, 16], false, 1e6, 1.0);
        proc.process(&silence, Some(&h1), Some(&b1), Some(&f1), true, &mut out);

        for (l, row) in out[..QMF_SLOTS].iter().enumerate() {
            let range = if l < 2 { b0.kx..b0.kx + b0.m } else { b1.kx..b1.kx + b1.m };
            for (k, x) in row.iter().enumerate() {
                assert_eq!(range.contains(&k), *x != ZERO, "l={} k={}", l, k);
            }
        }
    }

    /// Frames without SBR data contain no HF signal, and SBR processing resumes afterwards.
    #[test]
    fn verify_missing_frames() {
        let mut lcg = Lcg(7);
        let h = header(5);
        let b = derive_tables(44100, &h);
        let f = frame(&b, &[0, 16], true, 1e6, 1e-3);
        let mut f_long = frame(&b, &[0, 8, 19], true, 1e6, 1e-3);
        f_long.l_a = Some(2);

        let mut proc = SbrChannelProcessor::new();
        let mut out = new_out();
        for i in 0..12 {
            let core = white_noise(&mut lcg, 1000.0);
            let hf = |out: &QmfFrame| {
                out[..QMF_SLOTS].iter().any(|row| row[32..].iter().any(|&x| x != ZERO))
            };
            match i % 4 {
                0 => {
                    proc.process(&core, Some(&h), Some(&b), Some(&f_long), i == 0, &mut out);
                    assert!(hf(&out));
                }
                1 => {
                    proc.process(&core, Some(&h), Some(&b), None, false, &mut out);
                    assert!(!hf(&out));
                }
                2 => {
                    proc.process(&core, Some(&h), None, Some(&f), false, &mut out);
                    assert!(!hf(&out));
                }
                _ => {
                    proc.process(&core, Some(&h), Some(&b), Some(&f), false, &mut out);
                    assert!(hf(&out));
                }
            }
            assert!(out.iter().flatten().all(|x| x.re.is_finite() && x.im.is_finite()));
        }
    }

    /// After a reset, the processor produces the same output as a new processor.
    #[test]
    fn verify_reset() {
        let mut lcg = Lcg(8);
        let h = header(5);
        let b = derive_tables(44100, &h);
        let mut f = frame(&b, &[0, 8, 18], true, 1e6, 1e-1);
        f.l_a = Some(2);
        f.add_harmonic[2] = true;
        f.invf_mode = [2; 5];
        let cores: Vec<Vec<f32>> = (0..3).map(|_| white_noise(&mut lcg, 1000.0)).collect();

        let run = |proc: &mut SbrChannelProcessor| -> Vec<Box<QmfFrame>> {
            cores
                .iter()
                .enumerate()
                .map(|(i, core)| {
                    let mut out = new_out();
                    proc.process(core, Some(&h), Some(&b), Some(&f), i == 0, &mut out);
                    out
                })
                .collect()
        };

        let mut proc = SbrChannelProcessor::new();
        let first = run(&mut proc);
        proc.reset();
        let second = run(&mut proc);
        assert!(first == second);
    }

    /// Generate random, possibly invalid, frequency band tables.
    fn random_tables(lcg: &mut Lcg) -> FreqBandTables {
        let random_table = |lcg: &mut Lcg, lo: usize, hi: usize, max_len: usize| {
            let mut t = vec![lo];
            while t.len() < max_len {
                let next = t.last().expect("border") + 1 + lcg.below(6);
                if next >= hi {
                    break;
                }
                t.push(next);
            }
            t.push(hi);
            t
        };

        let kx = lcg.below(34);
        let m = 1 + lcg.below(64usize.saturating_sub(kx).max(1));
        let f_table_high = random_table(lcg, kx, kx + m, 64);
        let f_table_low = random_table(lcg, kx, kx + m, 64);
        let noise_len = 1 + lcg.below(6);
        let f_table_noise = random_table(lcg, kx, kx + m, noise_len);
        let lim_len = 1 + lcg.below(12);
        let f_table_lim = random_table(lcg, kx, kx + m, lim_len);

        let mut patches = vec![];
        let mut left = m;
        for _ in 0..lcg.below(7) {
            let num = lcg.below(left.min(20) + 1);
            let start = lcg.below(33usize.saturating_sub(num).max(1));
            patches.push(Patch { num_subbands: num, start_subband: start });
            left -= num.min(left);
        }

        let mut b = FreqBandTables {
            k0: kx,
            k2: kx + m,
            kx,
            m,
            f_master: f_table_high.clone(),
            f_table_high,
            f_table_low,
            f_table_noise,
            f_table_lim,
            patches,
        };

        // Occasionally corrupt the tables.
        if lcg.chance(0.2) {
            match lcg.below(8) {
                0 => b.kx = lcg.below(100),
                1 => b.m = lcg.below(100),
                2 => b.f_table_high.reverse(),
                3 => b.f_table_low.clear(),
                4 => b.f_table_noise.push(lcg.below(100)),
                5 => b.f_table_lim = vec![lcg.below(70)],
                6 => b.patches.push(Patch { num_subbands: usize::MAX, start_subband: 3 }),
                _ => b.m = usize::MAX,
            }
        }
        b
    }

    /// Generate a random, possibly invalid, SBR frame.
    fn random_frame(lcg: &mut Lcg) -> SbrChannelFrame {
        let mut f = SbrChannelFrame { num_env: lcg.below(7), ..Default::default() };
        let mut t = lcg.below(5);
        for l in 0..f.t_env.len() {
            f.t_env[l] = t;
            t += if lcg.chance(0.05) { 0 } else { 1 + lcg.below(8) };
        }
        if lcg.chance(0.1) {
            f.t_env.reverse();
        }
        f.num_noise = lcg.below(4);
        for l in 0..f.t_noise.len() {
            f.t_noise[l] = lcg.below(25);
        }
        f.l_a = if lcg.chance(0.5) { Some(lcg.below(8)) } else { None };
        for mode in f.invf_mode.iter_mut() {
            *mode = if lcg.chance(0.05) { 255 } else { lcg.below(4) as u8 };
        }
        for l in 0..f.e_orig.len() {
            f.freq_res[l] = lcg.chance(0.5);
            for e in f.e_orig[l].iter_mut() {
                *e = match lcg.below(20) {
                    0 => f32::NAN,
                    1 => f32::INFINITY,
                    2 => -1.0,
                    3 => f32::MAX,
                    4 => 0.0,
                    _ => 64.0 * 2f32.powf((lcg.uniform() * 60.0) as f32),
                };
            }
        }
        for row in f.q_orig.iter_mut() {
            for q in row.iter_mut() {
                *q = match lcg.below(20) {
                    0 => f32::NAN,
                    1 => f32::INFINITY,
                    2 => -1.0,
                    _ => 2f32.powi(6 - lcg.below(31) as i32),
                };
            }
        }
        for a in f.add_harmonic.iter_mut() {
            *a = lcg.chance(0.2);
        }
        f
    }

    #[test]
    fn verify_robustness() {
        let mut lcg = Lcg(9);
        let mut proc = SbrChannelProcessor::new();
        let mut out = new_out();
        let valid_tables = derive_tables(48000, &header(3));
        let mut num_valid = 0;
        // The number of consecutive frames with a core signal of moderate level.
        let mut num_moderate = 0;

        for i in 0..3000 {
            let mut h = header(lcg.below(16) as u8);
            h.bs_limiter_gains = lcg.below(5) as u8;
            h.bs_interpol_freq = lcg.chance(0.5);
            h.bs_smoothing_mode = lcg.chance(0.5);

            let b = if lcg.chance(0.3) { valid_tables.clone() } else { random_tables(&mut lcg) };
            let f = random_frame(&mut lcg);

            if validate_sbr_data(&h, &b, &f) {
                num_valid += 1;
            }

            let kind = lcg.below(10);
            let core: Vec<f32> = match kind {
                0 => vec![],
                1 => vec![f32::MAX; 1024],
                2 => vec![0.0; 1500],
                _ => {
                    let amplitude = 10f64.powf(lcg.uniform() * 5.0);
                    white_noise(&mut lcg, amplitude)
                }
            };

            let header = if lcg.chance(0.9) { Some(&h) } else { None };
            let bands = if lcg.chance(0.9) { Some(&b) } else { None };
            let frame = if lcg.chance(0.9) { Some(&f) } else { None };

            proc.process(&core, header, bands, frame, lcg.chance(0.2), &mut out);

            // Extreme core signals may produce non-finite values, which are flushed from all state
            // within 3 frames. Otherwise, the output is always finite, regardless of the SBR data.
            num_moderate = if kind == 1 { 0 } else { num_moderate + 1 };
            if num_moderate >= 3 {
                assert!(out.iter().flatten().all(|x| x.re.is_finite() && x.im.is_finite()));
            }

            if i % 500 == 0 {
                proc.reset();
            }
        }

        // A reasonable fraction of the random frames exercised the SBR processing.
        assert!(num_valid > 150, "{}", num_valid);
    }
}
