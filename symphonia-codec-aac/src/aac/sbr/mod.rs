// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Spectral Band Replication (SBR) core types and frequency table derivation.
//!
//! Implements the SBR frequency band table algorithms defined in
//! ISO/IEC 14496-3:2009, subpart 4, section 4.6.18.3.

pub mod bs;
pub mod dsp;
pub mod ps;
pub mod synth;
mod tables;

use symphonia_core::dsp::complex::Complex;
use symphonia_core::errors::{decode_error, Result};

/// Maximum number of envelopes per SBR frame (ISO/IEC 14496-3, Table 4.155).
pub const NUM_ENVELOPES: usize = 8;

/// Maximum number of HF reconstruction patches.
pub const NUM_PATCHES: usize = 5;

/// Number of QMF subbands (analysis: 32, synthesis: 64).
pub const SBR_BANDS: usize = 64;

/// QMF delay slots for overlap-add.
pub const QMF_DELAY: usize = 8;

/// HF adjustment border offset (T_HFAdj).
pub const HF_ADJ: usize = 2;

/// Maximum time slots per SBR frame.
pub const MAX_SLOTS: usize = 16;

/// Smoothing filter delay for gain interpolation.
const SMOOTH_DELAY: usize = 4;

/// SBR time-frequency grid classification (ISO/IEC 14496-3, Table 4.160).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameClass {
    FixFix,
    FixVar,
    VarFix,
    VarVar,
}

/// Quantization coupling mode for envelope and noise floor data.
#[derive(Clone, Copy)]
pub enum QuantMode {
    Single,
    Left,
    Right,
}

/// SBR header parameters (ISO/IEC 14496-3, 4.6.18.2.2).
///
/// Controls frequency band table derivation, HF reconstruction, and envelope
/// processing. These parameters change infrequently and trigger a full
/// re-initialization of derived tables when modified.
#[derive(Clone, Copy, Debug)]
pub struct SbrHeader {
    pub amp_res: bool,
    pub start_freq: usize,
    pub stop_freq: usize,
    pub xover_band: usize,
    pub freq_scale: u8,
    pub alter_scale: bool,
    pub noise_bands: u8,
    pub limiter_bands: u8,
    pub limiter_gains: u8,
    pub interpol_freq: bool,
    pub smoothing_mode: bool,
}

impl SbrHeader {
    pub fn new() -> Self {
        Self {
            amp_res: false,
            start_freq: 0,
            stop_freq: 0,
            xover_band: 0,
            freq_scale: 2,
            alter_scale: true,
            noise_bands: 2,
            limiter_bands: 2,
            limiter_gains: 2,
            interpol_freq: true,
            smoothing_mode: true,
        }
    }

    /// Returns `true` if the frequency-affecting parameters differ from `other`,
    /// which requires re-derivation of all frequency band tables.
    pub fn differs_from(&self, other: &Self) -> bool {
        self.start_freq != other.start_freq
            || self.stop_freq != other.stop_freq
            || self.xover_band != other.xover_band
            || self.freq_scale != other.freq_scale
            || self.alter_scale != other.alter_scale
            || self.noise_bands != other.noise_bands
    }
}

/// Derived SBR frequency band tables and patch configuration.
///
/// All tables are computed from `SbrHeader` parameters and the core codec
/// sample rate via the `init()` method, following ISO/IEC 14496-3, 4.6.18.3.
#[derive(Clone)]
pub struct SbrState {
    /// Number of envelope scale factor bands: [N_low, N_high].
    pub num_env_bands: [usize; 2],
    /// Number of bands in the master frequency band table (N_master).
    pub num_master: usize,
    /// Number of noise floor scale factor bands (N_Q).
    pub num_noise_bands: usize,
    /// Number of limiter bands (N_L).
    pub num_lim: usize,
    /// Crossover subband index k_x — first subband in the SBR range.
    pub k_x: usize,
    /// Map: low-resolution band index -> high-resolution band index.
    pub low_to_high_res: [usize; SBR_BANDS],
    /// Map: high-resolution band index -> low-resolution band index.
    pub high_to_low_res: [usize; SBR_BANDS],
    /// Master frequency band table f_master[0..=N_master].
    pub f: [usize; SBR_BANDS],
    /// Low-resolution frequency band table f_TableLow[0..=N_low].
    pub f_low: [usize; SBR_BANDS],
    /// Noise floor frequency band table f_TableNoise[0..=N_Q].
    pub f_noise: [usize; SBR_BANDS],
    /// Limiter frequency band table f_TableLim[0..=N_L].
    pub f_lim: [usize; SBR_BANDS],
    /// Number of subbands in each HF patch.
    pub patch_num_subbands: [usize; SBR_BANDS],
    /// Starting subband index for each HF patch.
    pub patch_start_subband: [usize; SBR_BANDS],
    /// Number of active HF patches.
    pub num_patches: usize,
}

impl SbrState {
    pub fn new() -> Self {
        Self {
            num_env_bands: [0; 2],
            num_master: 0,
            num_noise_bands: 0,
            num_lim: 0,
            k_x: 0,
            low_to_high_res: [0; SBR_BANDS],
            high_to_low_res: [0; SBR_BANDS],
            f: [0; SBR_BANDS],
            f_low: [0; SBR_BANDS],
            f_noise: [0; SBR_BANDS],
            f_lim: [0; SBR_BANDS],
            patch_num_subbands: [0; SBR_BANDS],
            patch_start_subband: [0; SBR_BANDS],
            num_patches: 0,
        }
    }

    /// Derive all frequency band tables from the SBR header and core sample rate.
    ///
    /// `sample_rate` is the AAC-LC output rate prior to SBR doubling.
    /// Implements ISO/IEC 14496-3, 4.6.18.3.
    pub fn init(&mut self, hdr: &SbrHeader, sample_rate: u32) -> Result<()> {
        // Step 1: Derive start and stop subbands (Tables 4.64, 4.65).
        let k0 = compute_start_subband(hdr.start_freq, sample_rate);
        let k2 = compute_stop_subband(hdr.stop_freq, k0, sample_rate);

        // Validate against sample-rate-dependent maximum (Table 4.66).
        let band_limit = match sample_rate {
            0..=32000 => 48,
            32001..=47999 => 35,
            _ => 32,
        };
        if k2 - k0 > band_limit {
            return decode_error("sbr: frequency range exceeds maximum for sample rate");
        }

        // Step 2: Master frequency band table (4.6.18.3.2).
        self.num_master = if hdr.freq_scale == 0 {
            build_master_linear(&mut self.f, k0, k2, hdr.alter_scale)
        }
        else {
            build_master_logarithmic(&mut self.f, k0, k2, hdr.freq_scale, hdr.alter_scale)
        };

        // Step 3: Derive high and low resolution tables (4.6.18.3.5).
        let n_high = self.num_master - hdr.xover_band;
        let n_low = (n_high + 1) >> 1;
        self.num_env_bands = [n_low, n_high];

        let f_high_start = hdr.xover_band;
        self.k_x = self.f[f_high_start];

        // f_TableLow from f_TableHigh per 4.6.18.3.5.
        self.f_low = [0; SBR_BANDS];
        let even_high = (n_high & 1) == 0;
        for i in 0..=n_low {
            self.f_low[i] = if even_high {
                self.f[f_high_start + 2 * i]
            }
            else if i == 0 {
                self.f[f_high_start]
            }
            else {
                self.f[f_high_start + 2 * i - 1]
            };
        }

        // Resolution cross-reference maps.
        self.build_resolution_maps(f_high_start, n_high, n_low)?;

        // Step 4: Noise floor bands (4.6.18.3.6).
        self.compute_noise_table(hdr.noise_bands, k2);

        // Step 5: Patch configuration (4.6.18.6.2).
        self.compute_patches(k0, sample_rate)?;

        // Step 6: Limiter band table (4.6.18.3.7).
        self.compute_limiter_table(hdr.limiter_bands, n_low);

        Ok(())
    }

    /// Build bidirectional maps between high and low resolution band indices.
    fn build_resolution_maps(
        &mut self,
        f_high_start: usize,
        n_high: usize,
        n_low: usize,
    ) -> Result<()> {
        let high_table = &self.f[f_high_start..=f_high_start + n_high];
        let low_table = &self.f_low[..=n_low];

        // For each low-res boundary, find its index in the high-res table.
        for (li, &lf) in low_table.iter().enumerate() {
            match high_table.binary_search(&lf) {
                Ok(hi) => self.high_to_low_res[li] = hi,
                Err(_) => return decode_error("sbr: low/high resolution table inconsistency"),
            }
        }

        // For each high-res boundary, find the nearest low-res index.
        for (hi, &hf) in high_table.iter().enumerate() {
            self.low_to_high_res[hi] = match low_table.binary_search(&hf) {
                Ok(li) => li,
                Err(li) => li.saturating_sub(1),
            };
        }

        Ok(())
    }

    /// Derive noise floor frequency band table (ISO/IEC 14496-3, 4.6.18.3.6).
    fn compute_noise_table(&mut self, noise_bands: u8, k2: usize) {
        let kx = self.k_x;
        let n_low = self.num_env_bands[0];

        // N_Q = max(1, round(noiseBands * log2(k2 / k_x)))
        let n_q = if noise_bands == 0 || kx == 0 {
            1
        }
        else {
            ((noise_bands as f32) * ((k2 as f32) / (kx as f32)).log2()).round().max(1.0) as usize
        };
        self.num_noise_bands = n_q;

        // Distribute noise bands evenly across the low-resolution table indices.
        self.f_noise = [0; SBR_BANDS];
        self.f_noise[0] = self.f_low[0];
        let mut lo_idx = 0usize;
        for q in 1..=n_q {
            let step = (n_low - lo_idx) / (n_q + 1 - q);
            lo_idx += step;
            self.f_noise[q] = self.f_low[lo_idx];
        }
    }

    /// Derive HF generation patch configuration (ISO/IEC 14496-3, 4.6.18.6.2).
    fn compute_patches(&mut self, k0: usize, sample_rate: u32) -> Result<()> {
        let kx = self.k_x;
        let sbr_top = self.f[self.num_master]; // k_x + M
        let m = sbr_top - kx;

        // goalSb = round(2.048e6 / f_s)
        let goal_sb = ((2_048_000u64 + sample_rate as u64 / 2) / sample_rate as u64) as usize;

        // Find master-table index k where f_master[k] first reaches goalSb.
        let mut k = if goal_sb < kx + m {
            self.f[..self.num_master]
                .iter()
                .position(|&val| val >= goal_sb)
                .unwrap_or(self.num_master)
        }
        else {
            self.num_master
        };

        self.patch_num_subbands = [0; SBR_BANDS];
        self.patch_start_subband = [0; SBR_BANDS];
        let mut count = 0usize;
        let mut msb = k0;
        let mut usb = kx;

        loop {
            // Scan downward for a patch boundary that fits.
            let mut j = k;
            let (sb, parity) = loop {
                let sb = self.f[j];
                let parity = (sb + k0) & 1;
                if sb <= k0 + msb - 1 - parity {
                    break (sb, parity);
                }
                j -= 1;
            };

            let width = sb.saturating_sub(usb);
            self.patch_num_subbands[count] = width;
            self.patch_start_subband[count] = k0 - parity - width;

            if width > 0 {
                usb = sb;
                msb = sb;
                count += 1;
            }
            else {
                msb = kx;
            }

            if self.f[k] < sb + 3 {
                k = self.num_master;
            }
            if sb == sbr_top {
                break;
            }
        }

        // Discard a trailing narrow patch.
        if count > 1 && self.patch_num_subbands[count - 1] < 3 {
            count -= 1;
        }
        if count > NUM_PATCHES {
            return decode_error("sbr: patch count exceeds maximum");
        }

        self.num_patches = count;
        Ok(())
    }

    /// Derive limiter frequency band table (ISO/IEC 14496-3, 4.6.18.3.7).
    fn compute_limiter_table(&mut self, limiter_bands: u8, n_low: usize) {
        self.f_lim = [0; SBR_BANDS];

        if limiter_bands == 0 {
            // Single limiter band spanning the full SBR range.
            self.f_lim[0] = self.f_low[0];
            self.f_lim[1] = self.f_low[n_low];
            self.num_lim = 1;
            return;
        }

        // limBandsPerOctave from bs_limiter_bands (Table 4.63).
        let octave_res: f32 = match limiter_bands {
            1 => 1.2,
            2 => 2.0,
            _ => 3.0,
        };

        // Accumulate patch edges.
        let mut patch_edges = [0usize; NUM_PATCHES + 1];
        patch_edges[0] = self.k_x;
        for p in 0..self.num_patches {
            patch_edges[p + 1] = patch_edges[p] + self.patch_num_subbands[p];
        }
        let edge_set = &patch_edges[..=self.num_patches];

        // Merge f_TableLow boundaries with inner patch edges.
        let mut lim = [0usize; SBR_BANDS];
        lim[..=n_low].copy_from_slice(&self.f_low[..=n_low]);
        let mut n_lim = n_low; // number of bands (boundaries = n_lim + 1)

        // Insert inner patch edges (skip first and last) in sorted order.
        for &edge in &patch_edges[1..self.num_patches] {
            let pos = lim[..=n_lim].iter().position(|&b| b > edge).unwrap_or(n_lim + 1);
            // Shift right to make room.
            for s in (pos..=n_lim).rev() {
                lim[s + 1] = lim[s];
            }
            lim[pos] = edge;
            n_lim += 1;
        }

        // Iteratively thin bands narrower than the octave resolution limit.
        let mut i = 1;
        while i <= n_lim {
            let width_octaves = (lim[i] as f32 / lim[i - 1] as f32).log2();
            if width_octaves * octave_res < 0.49 {
                if lim[i] == lim[i - 1] || !edge_set.contains(&lim[i]) {
                    // Remove lim[i].
                    for s in i..n_lim {
                        lim[s] = lim[s + 1];
                    }
                    n_lim -= 1;
                }
                else if !edge_set.contains(&lim[i - 1]) {
                    // Remove lim[i-1].
                    for s in (i - 1)..n_lim {
                        lim[s] = lim[s + 1];
                    }
                    n_lim -= 1;
                }
                else {
                    i += 1;
                }
            }
            else {
                i += 1;
            }
        }

        self.f_lim[..=n_lim].copy_from_slice(&lim[..=n_lim]);
        self.num_lim = n_lim;
    }
}

/// Compute start subband k0 from bs_start_freq and sample rate.
///
/// ISO/IEC 14496-3, Table 4.64.
fn compute_start_subband(start_freq: usize, sample_rate: u32) -> usize {
    // Select the offset row for this sample rate.
    let row = match sample_rate {
        0..=16000 => 0,
        16001..=22050 => 1,
        22051..=24000 => 2,
        24001..=32000 => 3,
        32001..=64000 => 4,
        _ => 5,
    };
    let offset = i32::from(tables::START_FREQ_OFFSETS[row][start_freq]);

    // startMin = round(128 * f_startMin / f_s)
    let f_start_min: u32 = match sample_rate {
        0..=31999 => 3000,
        32000..=63999 => 4000,
        _ => 5000,
    };
    let start_min = ((128 * f_start_min + sample_rate / 2) / sample_rate) as i32;

    (start_min + offset).max(0) as usize
}

/// Compute stop subband k2 from bs_stop_freq, k0, and sample rate.
///
/// ISO/IEC 14496-3, Table 4.65.
fn compute_stop_subband(stop_freq: usize, k0: usize, sample_rate: u32) -> usize {
    let raw = match stop_freq {
        14 => 2 * k0,
        15 => 3 * k0,
        _ => {
            // stopMin = round(128 * 2 * f_startMin / f_s)
            let f_start_min: u32 = match sample_rate {
                0..=31999 => 3000,
                32000..=63999 => 4000,
                _ => 5000,
            };
            let stop_min = ((128 * 2 * f_start_min + sample_rate / 2) / sample_rate) as usize;

            // Logarithmically spaced widths for 13 stop bands.
            let mut widths = [0usize; 13];
            log_spaced_widths(&mut widths, stop_min, SBR_BANDS, 13);

            stop_min + widths[..stop_freq].iter().sum::<usize>()
        }
    };

    raw.min(SBR_BANDS)
}

/// Compute logarithmically spaced band widths, sorted in ascending order.
///
/// For `n` bands spanning [`k_start`, `k_stop`]:
///   w[i] = round(k_start * (k_stop/k_start)^((i+1)/n)) - round(k_start * (k_stop/k_start)^(i/n))
///
/// The resulting widths are sorted ascending, as required by
/// ISO/IEC 14496-3, 4.6.18.3.2.
fn log_spaced_widths(out: &mut [usize], k_start: usize, k_stop: usize, n: usize) {
    let base = k_start as f64;
    let ratio = (k_stop as f64) / base;
    let inv_n = 1.0 / (n as f64);

    for i in 0..n {
        let lower = (base * ratio.powf(i as f64 * inv_n)).round() as usize;
        let upper = (base * ratio.powf((i + 1) as f64 * inv_n)).round() as usize;
        out[i] = upper - lower;
    }

    out[..n].sort_unstable();
}

/// Build master frequency table with linear spacing (bs_freq_scale == 0).
///
/// ISO/IEC 14496-3, 4.6.18.3.2, case freq_scale == 0.
fn build_master_linear(
    f_master: &mut [usize; SBR_BANDS],
    k0: usize,
    k2: usize,
    alter_scale: bool,
) -> usize {
    let (dk, num_bands) = if !alter_scale {
        (1usize, 2 * ((k2 - k0) / 2))
    }
    else {
        (2usize, 2 * ((k2 - k0 + 2) / 4))
    };

    // Start with uniform widths, then adjust endpoints to hit k2 exactly.
    let mut widths = [dk; SBR_BANDS];
    let achieved = k0 + num_bands * dk;
    let mut deficit = k2 as isize - achieved as isize;

    if deficit > 0 {
        // Widen the topmost bands.
        let mut idx = num_bands - 1;
        while deficit > 0 {
            widths[idx] += 1;
            idx = idx.wrapping_sub(1);
            deficit -= 1;
        }
    }
    else {
        // Narrow the bottommost bands.
        let mut idx = 0;
        while deficit < 0 {
            widths[idx] -= 1;
            idx += 1;
            deficit += 1;
        }
    }

    // Accumulate to frequency boundaries.
    f_master[0] = k0;
    for i in 0..num_bands {
        f_master[i + 1] = f_master[i] + widths[i];
    }

    num_bands
}

/// Build master frequency table with logarithmic spacing (bs_freq_scale > 0).
///
/// ISO/IEC 14496-3, 4.6.18.3.2, case freq_scale > 0.
/// May use one or two frequency regions depending on the k2/k0 ratio.
fn build_master_logarithmic(
    f_master: &mut [usize; SBR_BANDS],
    k0: usize,
    k2: usize,
    freq_scale: u8,
    alter_scale: bool,
) -> usize {
    // bands_per_octave: freq_scale {1,2,3} -> {12,10,8}
    let bands_per_oct = (14 - 2 * freq_scale as usize) as f32;
    let warp: f32 = if alter_scale { 1.3 } else { 1.0 };

    // Determine if a two-region split is needed (4.6.18.3.2).
    let use_two_regions = (k2 as f32) / (k0 as f32) > 2.2449;
    let k1 = if use_two_regions { 2 * k0 } else { k2 };

    // Region 0: [k0, k1]
    let n0 = 2 * ((bands_per_oct * (k1 as f32 / k0 as f32).log2() / 2.0).round() as usize);

    let mut w0 = [0usize; SBR_BANDS];
    log_spaced_widths(&mut w0, k0, k1, n0);

    f_master[0] = k0;
    for i in 0..n0 {
        f_master[i + 1] = f_master[i] + w0[i];
    }

    if !use_two_regions {
        return n0;
    }

    // Region 1: [k1, k2]
    let n1 = 2 * ((bands_per_oct * (k2 as f32 / k1 as f32).log2() / (2.0 * warp)).round() as usize);

    let mut w1 = [0usize; SBR_BANDS];
    log_spaced_widths(&mut w1, k1, k2, n1);

    // Ensure continuity: first width of region 1 >= last width of region 0.
    if w1[0] < w0[n0 - 1] {
        let headroom = (w1[n1 - 1] - w1[0]) / 2;
        let adjustment = (w0[n0 - 1] - w1[0]).min(headroom);
        w1[0] += adjustment;
        w1[n1 - 1] -= adjustment;
    }

    // Append region 1 boundaries after region 0.
    let mut freq = k1;
    for i in 0..n1 {
        freq += w1[i];
        f_master[n0 + 1 + i] = freq;
    }

    n0 + n1
}

/// Squared magnitude of a complex value: |c|^2 = re^2 + im^2.
#[inline(always)]
pub fn sq_modulus(c: Complex) -> f32 {
    c.re * c.re + c.im * c.im
}

const COMPLEX_ZERO: Complex = Complex { re: 0.0, im: 0.0 };

/// Per-channel SBR processing state.
///
/// Contains QMF subband buffers, bitstream-parsed envelope and noise floor
/// parameters, and inter-frame state needed for continuity across frames.
#[derive(Clone)]
pub struct SbrChannel {
    /// QMF analysis filterbank output W[t][k].
    pub w: [[Complex; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2],
    /// Low-frequency QMF subbands X_low[t][k].
    pub x: [[Complex; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2],
    /// High-frequency generated QMF subbands X_high[t][k].
    pub x_high: [[Complex; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2],
    /// Combined output subbands Y[t][k] for QMF synthesis.
    pub y: [[Complex; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2],
    /// Previous frame Y for overlap continuity.
    pub prev_y: [[Complex; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2],

    /// Current chirp control factors alpha_q (4.6.18.6.3).
    pub bw_array: [f32; SBR_BANDS],
    /// Previous frame chirp factors.
    pub old_bw_array: [f32; SBR_BANDS],

    pub qmode: QuantMode,
    pub fclass: FrameClass,
    pub amp_res: bool,
    pub num_env: usize,
    pub prev_num_env: usize,
    pub freq_res: [bool; NUM_ENVELOPES],
    pub env_border: [usize; NUM_ENVELOPES + 1],
    pub noise_env_border: [usize; 3],
    pub pointer: u8,
    pub num_noise: usize,
    pub last_env_end: usize,

    pub df_env: [bool; NUM_ENVELOPES],
    pub df_noise: [bool; 2],

    pub invf_mode: [u8; NUM_PATCHES],
    pub old_invf_mode: [u8; NUM_PATCHES],

    pub data_env: [[i8; SBR_BANDS]; NUM_ENVELOPES],
    pub data_noise: [[i8; SBR_BANDS]; 2],
    pub data_env2: [[i8; SBR_BANDS]; NUM_ENVELOPES],
    pub data_noise2: [[i8; SBR_BANDS]; 2],
    pub last_envelope: [i8; SBR_BANDS],
    pub last_noise_env: [i8; SBR_BANDS],
    pub last_freq_res: bool,

    pub add_harmonic: [bool; SBR_BANDS],
    pub prev_l_a: i8,

    pub s_idx_mapped: [[bool; SBR_BANDS]; NUM_ENVELOPES],
    pub prev_s_idx_mapped: [bool; SBR_BANDS],
    pub index_sine: usize,
    pub index_noise: usize,
    pub g_temp: [[f32; SBR_BANDS]; MAX_SLOTS * 2 + QMF_DELAY + SMOOTH_DELAY],
    pub q_temp: [[f32; SBR_BANDS]; MAX_SLOTS * 2 + QMF_DELAY + SMOOTH_DELAY],
}

impl SbrChannel {
    pub fn new() -> Self {
        Self {
            w: [[COMPLEX_ZERO; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2],
            x: [[COMPLEX_ZERO; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2],
            x_high: [[COMPLEX_ZERO; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2],
            y: [[COMPLEX_ZERO; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2],
            prev_y: [[COMPLEX_ZERO; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2],
            bw_array: [0.0; SBR_BANDS],
            old_bw_array: [0.0; SBR_BANDS],
            qmode: QuantMode::Single,
            fclass: FrameClass::FixFix,
            amp_res: false,
            num_env: 0,
            prev_num_env: 0,
            freq_res: [false; NUM_ENVELOPES],
            env_border: [0; NUM_ENVELOPES + 1],
            noise_env_border: [0; 3],
            pointer: 0,
            num_noise: 0,
            last_env_end: 0,
            df_env: [false; NUM_ENVELOPES],
            df_noise: [false; 2],
            invf_mode: [0; NUM_PATCHES],
            old_invf_mode: [0; NUM_PATCHES],
            data_env: [[0; SBR_BANDS]; NUM_ENVELOPES],
            data_noise: [[0; SBR_BANDS]; 2],
            data_env2: [[0; SBR_BANDS]; NUM_ENVELOPES],
            data_noise2: [[0; SBR_BANDS]; 2],
            last_envelope: [0; SBR_BANDS],
            last_noise_env: [0; SBR_BANDS],
            last_freq_res: false,
            add_harmonic: [false; SBR_BANDS],
            prev_l_a: -1,
            s_idx_mapped: [[false; SBR_BANDS]; NUM_ENVELOPES],
            prev_s_idx_mapped: [false; SBR_BANDS],
            index_sine: 0,
            index_noise: 0,
            g_temp: [[0.0; SBR_BANDS]; MAX_SLOTS * 2 + QMF_DELAY + SMOOTH_DELAY],
            q_temp: [[0.0; SBR_BANDS]; MAX_SLOTS * 2 + QMF_DELAY + SMOOTH_DELAY],
        }
    }

    pub fn reset(&mut self) {
        self.prev_y = [[COMPLEX_ZERO; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2];
        self.old_bw_array = [0.0; SBR_BANDS];
        self.last_envelope = [0; SBR_BANDS];
        self.last_noise_env = [0; SBR_BANDS];
        self.last_freq_res = false;
        self.last_env_end = 0;
        self.prev_num_env = 0;
        self.old_invf_mode = [0; NUM_PATCHES];
        self.prev_s_idx_mapped = [false; SBR_BANDS];
        self.index_sine = 0;
        self.index_noise = 0;
    }

    pub fn set_amp_res(&mut self, amp_res: bool) {
        // For FIXFIX grids with a single envelope, force 1.5 dB resolution
        // per ISO/IEC 14496-3, 4.6.18.3.3.
        if self.fclass == FrameClass::FixFix && self.num_env == 1 {
            self.amp_res = false;
        }
        else {
            self.amp_res = amp_res;
        }
    }
}

/// Compute the SBR CRC-10 check value (ISO/IEC 14496-3, 4.6.18.2).
///
/// Generator polynomial: x^10 + x^5 + x^4 + x + 1 (0x233).
/// Initial register value: 0. Processes `num_bits` bits from `data`
/// (MSB-first within each byte), then flushes the 10-bit register.
pub fn sbr_crc10(data: &[u8], num_bits: usize) -> u16 {
    const GENERATOR: u16 = 0x233;
    let mut reg: u16 = 0;
    let mut bits_left = num_bits;

    for &byte in data {
        if bits_left == 0 {
            break;
        }
        let n = bits_left.min(8);
        for shift in (8 - n..8).rev() {
            let input = u16::from((byte >> shift) & 1);
            let msb = reg >> 9;
            reg = ((reg << 1) | input) & 0x3FF;
            if msb != 0 {
                reg ^= GENERATOR;
            }
        }
        bits_left -= n;
    }

    // Flush: feed 10 zero bits to clear the register.
    for _ in 0..10 {
        let msb = reg >> 9;
        reg = (reg << 1) & 0x3FF;
        if msb != 0 {
            reg ^= GENERATOR;
        }
    }

    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc10_all_zeros_is_zero() {
        assert_eq!(sbr_crc10(&[0u8; 8], 64), 0);
    }

    #[test]
    fn crc10_nonzero_result() {
        let val = sbr_crc10(&[0xFF], 8);
        assert_ne!(val, 0);
        assert!(val <= 0x3FF);
    }

    #[test]
    fn crc10_bit_sensitivity() {
        let a = sbr_crc10(&[0x80, 0x00], 16);
        let b = sbr_crc10(&[0xC0, 0x00], 16);
        assert_ne!(a, b);
    }

    #[test]
    fn crc10_ignores_trailing_bits() {
        // 12-bit input: only top 4 bits of byte 1 count.
        let a = sbr_crc10(&[0xFF, 0xF0], 12);
        let b = sbr_crc10(&[0xFF, 0xFF], 12);
        assert_eq!(a, b);
    }

    #[test]
    fn crc10_self_check_property() {
        let payload = [0xDE, 0xAD, 0xBE, 0xEF];
        let crc = sbr_crc10(&payload, 32);

        // Append 10-bit CRC to make a 42-bit message; re-computing should yield 0.
        let mut extended = [0u8; 6];
        extended[..4].copy_from_slice(&payload);
        extended[4] = (crc >> 2) as u8;
        extended[5] = ((crc & 0x03) as u8) << 6;

        assert_eq!(sbr_crc10(&extended, 42), 0);
    }

    #[test]
    fn log_widths_sum_to_range() {
        let mut w = [0usize; 10];
        log_spaced_widths(&mut w, 20, 64, 10);
        let total: usize = w[..10].iter().sum();
        assert_eq!(total, 64 - 20);
    }

    #[test]
    fn log_widths_ascending() {
        let mut w = [0usize; 8];
        log_spaced_widths(&mut w, 10, 50, 8);
        for pair in w[..8].windows(2) {
            assert!(pair[0] <= pair[1]);
        }
    }
}
