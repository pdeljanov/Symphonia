// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Spectral Band Replication (SBR) decoder for HE-AAC.
//!
//! Implements SBR as defined in ISO/IEC 14496-3 Subpart 4.
//! SBR extends AAC-LC by replicating high-frequency content from the decoded
//! low-frequency signal, doubling the effective bandwidth and output sample rate.

pub mod bs;
pub mod dsp;
pub mod ps;
pub mod synth;
mod tables;

use symphonia_core::dsp::complex::Complex;
use symphonia_core::errors::{decode_error, Result};

/// Maximum number of envelopes per SBR frame.
pub const NUM_ENVELOPES: usize = 8;

/// Maximum number of HF reconstruction patches.
pub const NUM_PATCHES: usize = 5;

/// Number of QMF subbands (analysis: 32, synthesis: 64).
pub const SBR_BANDS: usize = 64;

/// QMF delay slots for overlap.
pub const QMF_DELAY: usize = 8;

/// HF adjustment border offset.
pub const HF_ADJ: usize = 2;

/// Maximum time slots per SBR frame.
pub const MAX_SLOTS: usize = 16;

/// Smoothing filter delay.
const SMOOTH_DELAY: usize = 4;

/// SBR header data parsed from the extension payload.
/// Contains configuration for frequency band tables, noise, and limiter.
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

    /// Check if this header differs from another in ways that require re-initialization.
    pub fn differs_from(&self, other: &Self) -> bool {
        self.start_freq != other.start_freq
            || self.stop_freq != other.stop_freq
            || self.xover_band != other.xover_band
            || self.freq_scale != other.freq_scale
            || self.alter_scale != other.alter_scale
            || self.noise_bands != other.noise_bands
    }
}

/// SBR frequency band state computed from the header.
/// Contains the master frequency band table, derived tables, and patch configuration.
#[derive(Clone)]
pub struct SbrState {
    /// Number of envelope bands at [low, high] resolution.
    pub num_env_bands: [usize; 2],
    /// Number of master frequency bands.
    pub num_master: usize,
    /// Number of noise floor bands.
    pub num_noise_bands: usize,
    /// Number of limiter bands.
    pub num_lim: usize,
    /// Crossover subband index (start of HF region).
    pub k_x: usize,
    /// Mapping from low to high resolution band index.
    pub low_to_high_res: [usize; SBR_BANDS],
    /// Mapping from high to low resolution band index.
    pub high_to_low_res: [usize; SBR_BANDS],
    /// Master frequency band table f_master.
    pub f: [usize; SBR_BANDS],
    /// Low-resolution frequency band table.
    pub f_low: [usize; SBR_BANDS],
    /// Noise floor frequency band table.
    pub f_noise: [usize; SBR_BANDS],
    /// Limiter frequency band table.
    pub f_lim: [usize; SBR_BANDS],
    /// Number of subbands per patch.
    pub patch_num_subbands: [usize; SBR_BANDS],
    /// Starting subband for each patch.
    pub patch_start_subband: [usize; SBR_BANDS],
    /// Number of active patches.
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

    /// Initialize frequency tables from the SBR header and core codec sample rate.
    /// The `srate` parameter is the AAC-LC output sample rate (before SBR doubling).
    pub fn init(&mut self, hdr: &SbrHeader, srate: u32) -> Result<()> {
        let offset_tab = match srate {
            0..=16000 => &tables::SBR_OFFSETS[0],
            16001..=22050 => &tables::SBR_OFFSETS[1],
            22051..=24000 => &tables::SBR_OFFSETS[2],
            24001..=32000 => &tables::SBR_OFFSETS[3],
            32001..=64000 => &tables::SBR_OFFSETS[4],
            _ => &tables::SBR_OFFSETS[5],
        };
        let smult = match srate {
            0..=31999 => 3000u32,
            32000..=63999 => 4000,
            _ => 5000,
        };
        let start_min = (128 * smult + srate / 2) / srate;
        let stop_min = (128 * smult * 2 + srate / 2) / srate;

        let k0 = ((start_min as i32) + i32::from(offset_tab[hdr.start_freq])).max(0) as usize;
        let k2 = (match hdr.stop_freq {
            14 => 2 * k0,
            15 => 3 * k0,
            _ => {
                let mut stop_dk = [0usize; 14];
                generate_vdk(&mut stop_dk, stop_min as usize, SBR_BANDS, 13);
                let dk_sum: usize = stop_dk[..hdr.stop_freq].iter().sum();
                (stop_min as usize) + dk_sum
            }
        })
        .min(SBR_BANDS);

        let max_bands = match srate {
            0..=32000 => 48,
            32001..=47999 => 35,
            _ => 32,
        };
        if k2 - k0 > max_bands {
            return decode_error("sbr: too many bands");
        }

        self.num_master = calculate_master_frequencies(&mut self.f, k0, k2, hdr);
        let num_high = self.num_master - hdr.xover_band;
        let num_low = (num_high + 1) / 2;

        self.num_env_bands = [num_low, num_high];

        let f_high = &self.f[hdr.xover_band..];
        let k_x = f_high[0];
        self.k_x = k_x;

        self.f_low = [0; SBR_BANDS];
        if (num_high & 1) == 0 {
            for k in 0..=num_low {
                self.f_low[k] = f_high[k * 2];
            }
        }
        else {
            self.f_low[0] = f_high[0];
            for k in 1..=num_low {
                self.f_low[k] = f_high[k * 2 - 1];
            }
        }

        let high_src = &f_high[..=num_high];
        let low_src = &self.f_low[..=num_low];
        for (dst, low) in self.high_to_low_res.iter_mut().zip(low_src.iter()) {
            if let Ok(idx) = high_src.binary_search(low) {
                *dst = idx;
            }
            else {
                return decode_error("sbr: resolution mapping error");
            }
        }
        for (dst, high) in self.low_to_high_res.iter_mut().zip(high_src.iter()) {
            *dst = match low_src.binary_search(high) {
                Ok(idx) => idx,
                Err(idx) => idx.saturating_sub(1),
            };
        }

        let num_q = (((hdr.noise_bands as f32) * ((k2 as f32) / (k_x as f32)).log2()).round()
            as usize)
            .max(1);
        self.num_noise_bands = num_q;
        self.f_noise = [0; SBR_BANDS];
        let mut prev = 0;
        self.f_noise[0] = self.f_low[0];
        for k in 1..=num_q {
            let idx = prev + ((num_low - prev) / (num_q + 1 - k));
            self.f_noise[k] = self.f_low[idx];
            prev = idx;
        }

        let mut num_patches = 0;
        self.patch_num_subbands = [0; SBR_BANDS];
        self.patch_start_subband = [0; SBR_BANDS];
        let mut msb = k0;
        let mut usb = k_x;
        let m = f_high[num_high] - f_high[0];
        let goal_sb = ((2048000 + srate / 2) / srate) as usize;
        let last_band = k_x + m;
        let mut k = if goal_sb < last_band {
            let mut kk = 0;
            for i in 0..self.num_master {
                if self.f[i] >= goal_sb {
                    break;
                }
                kk = i + 1;
            }
            kk
        }
        else {
            self.num_master
        };

        loop {
            let mut sb;
            let mut odd;
            let mut j = k;
            loop {
                sb = self.f[j];
                odd = (sb - 2 + k0) & 1;
                if sb <= k0 + msb - 1 - odd {
                    break;
                }
                j -= 1;
            }

            self.patch_num_subbands[num_patches] = sb.saturating_sub(usb);
            self.patch_start_subband[num_patches] = k0 - odd - self.patch_num_subbands[num_patches];

            if self.patch_num_subbands[num_patches] > 0 {
                usb = sb;
                msb = sb;
                num_patches += 1;
            }
            else {
                msb = k_x;
            }

            if self.f[k] < sb + 3 {
                k = self.num_master;
            }

            if sb == last_band {
                break;
            }
        }
        if (num_patches > 1) && (self.patch_num_subbands[num_patches - 1] < 3) {
            num_patches -= 1;
        }
        if num_patches > NUM_PATCHES {
            return decode_error("sbr: too many patches");
        }
        self.num_patches = num_patches;

        self.f_lim = [0; SBR_BANDS];
        let num_l = if hdr.limiter_bands == 0 {
            self.f_lim[0] = self.f_low[0];
            self.f_lim[1] = self.f_low[num_low];
            1
        }
        else {
            let lim_bands = match hdr.limiter_bands {
                1 => 1.2f32,
                2 => 2.0,
                _ => 3.0,
            };
            let mut patch_borders = [0usize; NUM_PATCHES + 1];
            patch_borders[0] = k_x;
            for kk in 0..num_patches {
                patch_borders[kk + 1] = patch_borders[kk] + self.patch_num_subbands[kk];
            }
            self.f_lim = self.f_low;
            let total = num_low + num_patches;
            for &pborder in &patch_borders[1..num_patches] {
                let mut i = 0;
                for &el in self.f_lim[..total].iter() {
                    if el > pborder {
                        break;
                    }
                    i += 1;
                }
                for jj in (i..total - 1).rev() {
                    self.f_lim[jj + 1] = self.f_lim[jj];
                }
                self.f_lim[i] = pborder;
            }
            let mut nr_lim = total - 1;
            let mut kk = 1;
            let pbord = &patch_borders[..=num_patches];
            while kk <= nr_lim {
                let n_octaves = ((self.f_lim[kk] as f32) / (self.f_lim[kk - 1] as f32)).log2();
                if (n_octaves * lim_bands) < 0.49 {
                    if self.f_lim[kk] == self.f_lim[kk - 1] || !pbord.contains(&self.f_lim[kk]) {
                        for l in kk..nr_lim {
                            self.f_lim[l] = self.f_lim[l + 1];
                        }
                        nr_lim -= 1;
                    }
                    else if !pbord.contains(&self.f_lim[kk - 1]) {
                        for l in (kk - 1)..nr_lim {
                            self.f_lim[l] = self.f_lim[l + 1];
                        }
                        nr_lim -= 1;
                    }
                    else {
                        kk += 1;
                    }
                }
                else {
                    kk += 1;
                }
            }
            nr_lim
        };
        self.num_lim = num_l;

        Ok(())
    }
}

/// Helper: generate logarithmically spaced band widths, sorted ascending.
fn generate_vdk(v_dk: &mut [usize], k0: usize, k1: usize, num_bands: usize) {
    let mut last = k0;
    let k0f = k0 as f64;
    let factor = (k1 as f64) / k0f;
    for k in 0..num_bands {
        let next = (k0f * factor.powf((k + 1) as f64 / (num_bands as f64))).round() as usize;
        let newval = next - last;
        last = next;
        let mut idx = k;
        for (j, &el) in v_dk[..k].iter().enumerate() {
            if newval < el {
                idx = j;
                break;
            }
        }
        for j in (idx..k).rev() {
            v_dk[j + 1] = v_dk[j];
        }
        v_dk[idx] = newval;
    }
}

/// Calculate the master frequency band table from header parameters.
/// Returns the number of master bands.
#[allow(clippy::comparison_chain)]
fn calculate_master_frequencies(
    f: &mut [usize; SBR_BANDS],
    k0: usize,
    k2: usize,
    hdr: &SbrHeader,
) -> usize {
    if hdr.freq_scale == 0 {
        let (dk, num_bands) = if !hdr.alter_scale {
            (1, 2 * ((k2 - k0) / 2))
        }
        else {
            (2, 2 * ((k2 - k0 + 2) / (2 * 2)))
        };
        let k2_achieved = k0 + num_bands * dk;
        let mut k2_diff = (k2 as isize) - (k2_achieved as isize);
        let mut v_dk = [dk; SBR_BANDS];
        if k2_diff < 0 {
            let mut kk = 0;
            while k2_diff != 0 {
                v_dk[kk] -= 1;
                kk += 1;
                k2_diff += 1;
            }
        }
        else if k2_diff > 0 {
            let mut kk = num_bands - 1;
            while k2_diff != 0 {
                v_dk[kk] += 1;
                kk -= 1;
                k2_diff -= 1;
            }
        }
        f[0] = k0;
        for i in 0..num_bands {
            f[i + 1] = f[i] + v_dk[i];
        }
        num_bands
    }
    else {
        let bands = 14 - hdr.freq_scale * 2;
        let warp = if !hdr.alter_scale { 1.0f32 } else { 1.3f32 };
        let two_regions = (k2 as f32) / (k0 as f32) > 2.2449;
        let k1 = if two_regions { 2 * k0 } else { k2 };
        let num_bands0 =
            2 * (((bands as f32) * ((k1 as f32) / (k0 as f32)).log2() / 2.0).round() as usize);

        let mut v_dk0 = [0; SBR_BANDS];
        generate_vdk(&mut v_dk0, k0, k1, num_bands0);
        let mut v_k0 = [0; SBR_BANDS];
        v_k0[0] = k0;
        for i in 0..num_bands0 {
            v_k0[i + 1] = v_k0[i] + v_dk0[i];
        }

        if two_regions {
            let num_bands1 = 2
                * (((bands as f32) * ((k2 as f32) / (k1 as f32)).log2() / (2.0 * warp)).round()
                    as usize);
            let mut v_dk1 = [0; SBR_BANDS];
            generate_vdk(&mut v_dk1, k1, k2, num_bands1);
            let max_vdk0 = v_dk0[num_bands0 - 1];
            if v_dk1[0] < max_vdk0 {
                let change = (max_vdk0 - v_dk1[0]).min((v_dk1[num_bands1 - 1] - v_dk1[0]) / 2);
                v_dk1[0] += change;
                v_dk1[num_bands1 - 1] -= change;
            }
            let mut v_k1 = [0; SBR_BANDS];
            v_k1[0] = k1;
            for i in 0..num_bands1 {
                v_k1[i + 1] = v_k1[i] + v_dk1[i];
            }
            f[..=num_bands0].copy_from_slice(&v_k0[..=num_bands0]);
            f[num_bands0 + 1..][..=num_bands1].copy_from_slice(&v_k1[1..][..=num_bands1]);
            num_bands0 + num_bands1
        }
        else {
            f[..=num_bands0].copy_from_slice(&v_k0[..=num_bands0]);
            num_bands0
        }
    }
}

/// Helper: squared modulus of a complex number.
#[inline(always)]
pub fn sq_modulus(c: Complex) -> f32 {
    c.re * c.re + c.im * c.im
}

/// SBR time-frequency grid class.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameClass {
    FixFix,
    FixVar,
    VarFix,
    VarVar,
}

/// Quantization mode for coupled/uncoupled channels.
#[derive(Clone, Copy)]
pub enum QuantMode {
    Single,
    Left,
    Right,
}

/// Per-channel SBR state for bitstream data and DSP processing.
#[derive(Clone)]
pub struct SbrChannel {
    pub w: [[Complex; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2],
    pub x: [[Complex; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2],
    pub x_high: [[Complex; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2],
    pub y: [[Complex; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2],
    pub prev_y: [[Complex; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2],

    pub bw_array: [f32; SBR_BANDS],
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

const COMPLEX_ZERO: Complex = Complex { re: 0.0, im: 0.0 };

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
        if self.fclass != FrameClass::FixFix || self.num_env != 1 {
            self.amp_res = amp_res;
        }
        else {
            self.amp_res = false;
        }
    }
}

/// Compute SBR CRC-10 over a bit stream (ISO/IEC 14496-3, 4.6.18.2).
///
/// Polynomial: 0x233 (x^10 + x^5 + x^4 + x + 1), init: 0.
/// `data` contains the payload bytes, `num_bits` is the number of valid bits
/// (MSB-first within each byte).
pub fn sbr_crc10(data: &[u8], num_bits: usize) -> u16 {
    let mut crc: u16 = 0;
    let mut bits_remaining = num_bits;

    for &byte in data.iter() {
        if bits_remaining == 0 {
            break;
        }
        let bits_in_byte = bits_remaining.min(8);
        for bit_pos in (8 - bits_in_byte..8).rev() {
            let new_bit = ((byte >> bit_pos) & 1) as u16;
            if (crc >> 9) & 1 != 0 {
                crc = ((crc << 1) | new_bit) ^ 0x233;
            }
            else {
                crc = (crc << 1) | new_bit;
            }
            crc &= 0x3FF;
        }
        bits_remaining -= bits_in_byte;
    }

    // Flush 10 zero bits through the CRC register.
    for _ in 0..10 {
        if (crc >> 9) & 1 != 0 {
            crc = (crc << 1) ^ 0x233;
        }
        else {
            crc <<= 1;
        }
        crc &= 0x3FF;
    }

    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_crc10_zero_input() {
        // All-zero input should produce CRC = 0 (no feedback triggered).
        let data = [0u8; 8];
        assert_eq!(sbr_crc10(&data, 64), 0);
    }

    #[test]
    fn verify_crc10_single_byte() {
        // Non-zero input should produce non-zero CRC.
        let data = [0xFF];
        let crc = sbr_crc10(&data, 8);
        assert_ne!(crc, 0);
        assert!(crc <= 0x3FF, "CRC-10 must be 10-bit");
    }

    #[test]
    fn verify_crc10_single_bit_flip() {
        // Flipping one bit must change the CRC.
        let data_a = [0x80, 0x00];
        let data_b = [0xC0, 0x00];
        let crc_a = sbr_crc10(&data_a, 16);
        let crc_b = sbr_crc10(&data_b, 16);
        assert_ne!(crc_a, crc_b);
    }

    #[test]
    fn verify_crc10_partial_byte() {
        // CRC over 12 bits should only use the top 4 bits of byte 1.
        let data = [0xFF, 0xF0];
        let crc_12 = sbr_crc10(&data, 12);
        // Same data but with junk in the lower 4 bits of byte 1 — should match.
        let data2 = [0xFF, 0xFF];
        let crc_12b = sbr_crc10(&data2, 12);
        assert_eq!(crc_12, crc_12b);
    }

    #[test]
    fn verify_crc10_self_check() {
        // Appending the CRC to the data and re-computing should give 0.
        let payload = [0xDE, 0xAD, 0xBE, 0xEF];
        let crc = sbr_crc10(&payload, 32);

        // Build extended data: payload bits + CRC bits (10 bits, MSB first).
        // Total: 32 + 10 = 42 bits = 6 bytes (last byte has 2 valid bits).
        let mut extended = [0u8; 6];
        extended[..4].copy_from_slice(&payload);
        // Place 10-bit CRC starting at bit 32.
        extended[4] = (crc >> 2) as u8;
        extended[5] = ((crc & 0x03) as u8) << 6;

        let check = sbr_crc10(&extended, 42);
        assert_eq!(check, 0, "CRC self-check must be zero");
    }
}
