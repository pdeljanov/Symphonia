// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Derivation of the SBR frequency band tables (ISO/IEC 14496-3:2009, 4.6.18.3.2) and of the HF
//! generation patches (4.6.18.6.3) from an SBR header and the SBR sampling frequency.

use symphonia_core::errors::{Result, decode_error};

use crate::aac::sbr::types::{FreqBandTables, MAX_NOISE_BANDS, MAX_PATCHES, Patch, SbrHeader};

/// The number of QMF subbands of the synthesis filterbank.
const NUM_QMF_BANDS: usize = 64;

/// The maximum value of k_x, the first QMF subband of the SBR range (4.6.18.3.6).
const MAX_KX: usize = 32;

/// Map a sampling frequency to the sampling frequency whose tables are to be used (Table 4.82).
pub fn map_sampling_frequency(freq: u32) -> u32 {
    match freq {
        92017.. => 96000,
        75132.. => 88200,
        55426.. => 64000,
        46009.. => 48000,
        37566.. => 44100,
        27713.. => 32000,
        23004.. => 24000,
        18783.. => 22050,
        13856.. => 16000,
        11502.. => 12000,
        9391.. => 11025,
        _ => 8000,
    }
}

/// Get Fs_SBR, the internal sampling frequency of the SBR tool (4.6.18.2.6): twice the sampling
/// frequency of the core coder after the sampling frequency mapping of Table 4.82.
///
/// `sbr_rate` is twice the AAC core sampling frequency.
pub fn sbr_sampling_frequency(sbr_rate: u32) -> u32 {
    2 * map_sampling_frequency(sbr_rate / 2)
}

/// NINT(num / den) for non-negative integers (rounding to the nearest integer, halves upwards).
fn nint_ratio(num: u64, den: u64) -> u64 {
    (2 * num + den) / (2 * den)
}

/// NINT(x) for a non-negative real x (rounding to the nearest integer, halves upwards).
fn nint(x: f64) -> i64 {
    // The values rounded by this function are small (< 2^10), the conversion cannot overflow.
    (x + 0.5).floor() as i64
}

/// The offset vector used to compute k0 for the given Fs_SBR (4.6.18.3.2.1).
fn start_offsets(fs_sbr: u32) -> Option<&'static [i8; 16]> {
    static OFFSET_16000: [i8; 16] = [-8, -7, -6, -5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7];
    static OFFSET_22050: [i8; 16] = [-5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13];
    static OFFSET_24000: [i8; 16] = [-5, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16];
    static OFFSET_32000: [i8; 16] = [-6, -4, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16];
    static OFFSET_44100_64000: [i8; 16] = [-4, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16, 20];
    static OFFSET_ABOVE_64000: [i8; 16] = [-2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16, 20, 24];

    match fs_sbr {
        16000 => Some(&OFFSET_16000),
        22050 => Some(&OFFSET_22050),
        24000 => Some(&OFFSET_24000),
        32000 => Some(&OFFSET_32000),
        44100..=64000 => Some(&OFFSET_44100_64000),
        64001.. => Some(&OFFSET_ABOVE_64000),
        _ => None,
    }
}

/// startMin (4.6.18.3.2.1).
fn start_min(fs_sbr: u32) -> u64 {
    let freq = match fs_sbr {
        ..32000 => 3000,
        32000..64000 => 4000,
        _ => 5000,
    };
    nint_ratio(freq * 128, u64::from(fs_sbr))
}

/// stopMin (4.6.18.3.2.1).
fn stop_min(fs_sbr: u32) -> u64 {
    let freq = match fs_sbr {
        ..32000 => 6000,
        32000..64000 => 8000,
        _ => 10000,
    };
    nint_ratio(freq * 128, u64::from(fs_sbr))
}

/// Compute k0, the first QMF subband of the master frequency band table (4.6.18.3.2.1).
pub fn compute_k0(bs_start_freq: u8, fs_sbr: u32) -> Result<usize> {
    if fs_sbr == 0 {
        return decode_error("sbr: invalid sampling frequency");
    }

    let Some(offsets) = start_offsets(fs_sbr)
    else {
        return decode_error("sbr: unsupported sampling frequency");
    };

    let Some(&offset) = offsets.get(usize::from(bs_start_freq))
    else {
        return decode_error("sbr: invalid bs_start_freq");
    };

    let k0 = start_min(fs_sbr) as i64 + i64::from(offset);

    if !(1..NUM_QMF_BANDS as i64).contains(&k0) {
        return decode_error("sbr: k0 out of range");
    }

    Ok(k0 as usize)
}

/// Compute k2, the upper frequency boundary (exclusive) of the master frequency band table
/// (4.6.18.3.2.1).
pub fn compute_k2(bs_stop_freq: u8, k0: usize, fs_sbr: u32) -> Result<usize> {
    if fs_sbr == 0 {
        return decode_error("sbr: invalid sampling frequency");
    }

    let k2 = match bs_stop_freq {
        0..14 => {
            let stop_min = stop_min(fs_sbr);

            // stopDk(p), 0 <= p <= 12.
            let ratio = NUM_QMF_BANDS as f64 / stop_min as f64;
            let border = |p: u32| nint(stop_min as f64 * ratio.powf(f64::from(p) / 13.0));

            let mut stop_dk = [0i64; 13];
            for (p, dk) in (0u32..).zip(stop_dk.iter_mut()) {
                *dk = border(p + 1) - border(p);
            }

            // stopDkSort = sort(stopDk).
            stop_dk.sort_unstable();

            let sum: i64 = stop_dk[..usize::from(bs_stop_freq)].iter().sum();

            (stop_min as i64 + sum).clamp(0, NUM_QMF_BANDS as i64) as usize
        }
        14 => (2 * k0).min(NUM_QMF_BANDS),
        15 => (3 * k0).min(NUM_QMF_BANDS),
        _ => return decode_error("sbr: invalid bs_stop_freq"),
    };

    Ok(k2)
}

/// The maximum number of QMF subbands covered by SBR, k2 - k0 (4.6.18.3.6).
fn max_sbr_range(fs_sbr: u32) -> usize {
    match fs_sbr {
        ..=32000 => 48,
        32001..48000 => 35,
        _ => 32,
    }
}

/// Calculate f_Master for bs_freq_scale = 0 (Figure 4.39).
pub fn master_table_linear(k0: usize, k2: usize, bs_alter_scale: bool) -> Result<Vec<usize>> {
    if k2 <= k0 || k2 > NUM_QMF_BANDS {
        return decode_error("sbr: invalid master frequency band table range");
    }

    let span = k2 - k0;

    let (dk, num_bands) = if !bs_alter_scale {
        // dk = 1, numBands = 2 * INT((k2 - k0) / (dk * 2)).
        (1, 2 * (span / 2))
    }
    else {
        // dk = 2, numBands = 2 * NINT((k2 - k0) / (dk * 2)).
        (2, 2 * nint_ratio(span as u64, 4) as usize)
    };

    // The resulting number of bands shall be greater than zero (4.6.18.3.6).
    if num_bands == 0 {
        return decode_error("sbr: master frequency band table has no bands");
    }

    let mut v_dk = vec![dk as i64; num_bands];

    let k2_achieved = (k0 + num_bands * dk) as i64;
    let mut k2_diff = k2 as i64 - k2_achieved;

    let (incr, mut k) = if k2_diff < 0 { (1i64, 0i64) } else { (-1i64, num_bands as i64 - 1) };

    while k2_diff != 0 {
        let Some(v) = usize::try_from(k).ok().and_then(|k| v_dk.get_mut(k))
        else {
            return decode_error("sbr: invalid master frequency band table");
        };
        *v -= incr;
        k += incr;
        k2_diff += incr;
    }

    cumulative_borders(k0, &v_dk)
}

/// Calculate f_Master for bs_freq_scale > 0 (Figure 4.40).
pub fn master_table_log(
    k0: usize,
    k2: usize,
    bs_freq_scale: u8,
    bs_alter_scale: bool,
) -> Result<Vec<usize>> {
    if k0 == 0 || k2 <= k0 || k2 > NUM_QMF_BANDS {
        return decode_error("sbr: invalid master frequency band table range");
    }

    let bands = match bs_freq_scale {
        1 => 12.0,
        2 => 10.0,
        3 => 8.0,
        _ => return decode_error("sbr: invalid bs_freq_scale"),
    };

    let warp = if bs_alter_scale { 1.3 } else { 1.0 };

    let (two_regions, k1) =
        if k2 as f64 / k0 as f64 > 2.2449 { (true, 2 * k0) } else { (false, k2) };

    // Calculate the band widths of a region spanning [start, end) with the given number of bands.
    let band_widths = |start: usize, end: usize, num_bands: usize| -> Vec<i64> {
        let ratio = end as f64 / start as f64;
        let border = |k: usize| nint(start as f64 * ratio.powf(k as f64 / num_bands as f64));
        (0..num_bands).map(|k| border(k + 1) - border(k)).collect()
    };

    // Region 0, [k0, k1).
    let num_bands0 =
        2 * nint(bands * (k1 as f64 / k0 as f64).ln() / (2.0 * std::f64::consts::LN_2));

    // numBands0 > 0 (4.6.18.3.6). The number of bands can never exceed the number of subbands
    // spanned by the region if all band widths are greater than zero.
    if num_bands0 <= 0 || num_bands0 as usize > k1 - k0 {
        return decode_error("sbr: invalid number of master frequency bands");
    }

    let mut v_dk0 = band_widths(k0, k1, num_bands0 as usize);
    v_dk0.sort_unstable();

    // vDk0(i) > 0 (4.6.18.3.6).
    if v_dk0.first().is_none_or(|&dk| dk <= 0) {
        return decode_error("sbr: invalid master frequency band width");
    }

    let mut f_master = cumulative_borders(k0, &v_dk0)?;

    if !two_regions {
        return Ok(f_master);
    }

    // Region 1, [k1, k2).
    if k2 <= k1 {
        return decode_error("sbr: invalid master frequency band table range");
    }

    let num_bands1 =
        2 * nint(bands * (k2 as f64 / k1 as f64).ln() / (2.0 * std::f64::consts::LN_2 * warp));

    if num_bands1 <= 0 || num_bands1 as usize > k2 - k1 {
        return decode_error("sbr: invalid number of master frequency bands");
    }

    let mut v_dk1 = band_widths(k1, k2, num_bands1 as usize);

    let max_dk0 = v_dk0.iter().copied().max().unwrap_or(0);
    let min_dk1 = v_dk1.iter().copied().min().unwrap_or(0);

    if min_dk1 < max_dk0 {
        v_dk1.sort_unstable();

        let last = v_dk1.len() - 1;

        let mut change = max_dk0 - v_dk1[0];
        let half_range = (v_dk1[last] - v_dk1[0]) / 2;

        if change > half_range {
            change = half_range;
        }

        v_dk1[0] += change;
        v_dk1[last] -= change;
    }

    v_dk1.sort_unstable();

    // vDk1(i) > 0 (4.6.18.3.6).
    if v_dk1.first().is_none_or(|&dk| dk <= 0) {
        return decode_error("sbr: invalid master frequency band width");
    }

    let vk1 = cumulative_borders(k1, &v_dk1)?;

    // f_Master = [vk0(0..=numBands0), vk1(1..=numBands1)].
    f_master.extend_from_slice(&vk1[1..]);

    Ok(f_master)
}

/// Compute the borders [start, start + w(0), start + w(0) + w(1), ...] from a list of positive
/// band widths. Fails if a width is not positive or a border exceeds 64.
fn cumulative_borders(start: usize, widths: &[i64]) -> Result<Vec<usize>> {
    let mut borders = Vec::with_capacity(widths.len() + 1);
    borders.push(start);

    let mut border = start;

    for &width in widths {
        if width <= 0 {
            return decode_error("sbr: invalid frequency band width");
        }

        border += width as usize;

        if border > NUM_QMF_BANDS {
            return decode_error("sbr: frequency band border out of range");
        }

        borders.push(border);
    }

    Ok(borders)
}

/// N_Q, the number of noise floor bands (4.6.18.3.2.2).
pub fn num_noise_bands(bs_noise_bands: u8, k2: usize, kx: usize) -> usize {
    if kx == 0 || k2 <= kx {
        return 1;
    }
    let n_q = nint(f64::from(bs_noise_bands) * (k2 as f64 / kx as f64).log2());
    n_q.max(1) as usize
}

/// Calculate the HF generation patches (Figure 4.48).
///
/// Returns an error if the patch construction does not terminate properly or if the number of
/// patches exceeds the limit of 4.6.18.3.6.
pub fn compute_patches(
    f_master: &[usize],
    k0: usize,
    kx: usize,
    m: usize,
    fs_sbr: u32,
) -> Result<Vec<Patch>> {
    // Each iteration of the patch loop either adds a patch, or moves the source limit. Bound the
    // number of iterations to guarantee termination for any input.
    const MAX_ITERATIONS: usize = 2 * NUM_QMF_BANDS;

    let Some(n_master) = f_master.len().checked_sub(1)
    else {
        return decode_error("sbr: empty master frequency band table");
    };

    if fs_sbr == 0 || k0 == 0 {
        return decode_error("sbr: invalid patch parameters");
    }

    let top = kx + m;

    let mut msb = k0;
    let mut usb = kx;

    let goal_sb = nint_ratio(2_048_000, u64::from(fs_sbr)) as usize;

    let mut k = if goal_sb < top {
        let mut k = 0;
        let mut i = 0;
        while f_master.get(i).is_some_and(|&f| f < goal_sb) {
            k = i + 1;
            i += 1;
        }
        k
    }
    else {
        n_master
    };

    if k > n_master {
        return decode_error("sbr: invalid patch goal subband");
    }

    let mut patches: Vec<Patch> = Vec::with_capacity(MAX_PATCHES + 1);

    let mut iterations = 0;

    loop {
        iterations += 1;
        if iterations > MAX_ITERATIONS {
            return decode_error("sbr: patch construction does not terminate");
        }

        // Find the highest subband border that is reachable by the current source range.
        let mut j = k;

        let (sb, odd) = loop {
            let sb = f_master[j];
            let odd = (sb + k0) % 2;

            // sb <= k0 - 1 + msb - odd.
            if sb + 1 + odd <= k0 + msb {
                break (sb, odd);
            }

            if j == 0 {
                return decode_error("sbr: patch construction failed");
            }

            j -= 1;
        };

        // patchNumSubbands = max(sb - usb, 0).
        let num_subbands = sb.saturating_sub(usb);

        if num_subbands > 0 {
            // patchStartSubband = k0 - odd - patchNumSubbands.
            let Some(start_subband) = k0.checked_sub(odd + num_subbands)
            else {
                return decode_error("sbr: invalid patch start subband");
            };

            // At most one patch more than the limit is allowed before the last patch is possibly
            // removed below.
            if patches.len() > MAX_PATCHES {
                return decode_error("sbr: too many patches");
            }

            patches.push(Patch { num_subbands, start_subband });

            usb = sb;
            msb = sb;
        }
        else {
            msb = kx;
        }

        if f_master[k] < sb + 3 {
            k = n_master;
        }

        if sb == top {
            break;
        }
    }

    // Remove a narrow last patch.
    if patches.len() > 1 && patches.last().is_some_and(|p| p.num_subbands < 3) {
        patches.pop();
    }

    // numPatches <= 5 (4.6.18.3.6).
    if patches.is_empty() || patches.len() > MAX_PATCHES {
        return decode_error("sbr: invalid number of patches");
    }

    Ok(patches)
}

/// Calculate f_TableLim, the limiter frequency band table (4.6.18.3.2.3 and Figure 4.41).
pub fn limiter_table(
    bs_limiter_bands: u8,
    f_table_low: &[usize],
    kx: usize,
    patches: &[Patch],
) -> Result<Vec<usize>> {
    let (Some(&first), Some(&last)) = (f_table_low.first(), f_table_low.last())
    else {
        return decode_error("sbr: empty low resolution frequency band table");
    };

    let lim_bands = match bs_limiter_bands {
        0 => return Ok(vec![first, last]),
        1 => 1.2,
        2 => 2.0,
        3 => 3.0,
        _ => return decode_error("sbr: invalid bs_limiter_bands"),
    };

    if patches.is_empty() {
        return decode_error("sbr: no patches");
    }

    // patchBorders(0..=numPatches).
    let mut patch_borders = Vec::with_capacity(patches.len() + 1);
    patch_borders.push(kx);
    let mut border = kx;
    for patch in patches {
        border += patch.num_subbands;
        patch_borders.push(border);
    }

    // limTable = sort(f_TableLow(0..=N_Low), patchBorders(1..numPatches)).
    let mut lim_table = Vec::with_capacity(f_table_low.len() + patches.len());
    lim_table.extend_from_slice(f_table_low);
    lim_table.extend_from_slice(&patch_borders[1..patches.len()]);
    lim_table.sort_unstable();

    if lim_table[0] == 0 {
        return decode_error("sbr: invalid limiter band border");
    }

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

    // At least one limiter band is required.
    if lim_table.len() < 2 {
        return decode_error("sbr: no limiter bands");
    }

    Ok(lim_table)
}

/// Derive the frequency band tables (4.6.18.3.2) and the HF generation patches (4.6.18.6.3) from
/// an SBR header for the SBR sampling frequency `fs_sbr` (Fs_SBR, see [`sbr_sampling_frequency`]).
///
/// All requirements of 4.6.18.3.6 that concern the frequency band tables are verified.
pub fn derive_freq_band_tables(header: &SbrHeader, fs_sbr: u32) -> Result<FreqBandTables> {
    let k0 = compute_k0(header.bs_start_freq, fs_sbr)?;
    let k2 = compute_k2(header.bs_stop_freq, k0, fs_sbr)?;

    // The master frequency band table is only defined for k2 > k0.
    if k2 <= k0 {
        return decode_error("sbr: stop frequency below start frequency");
    }

    // k2 - k0 is limited depending on Fs_SBR (4.6.18.3.6).
    if k2 - k0 > max_sbr_range(fs_sbr) {
        return decode_error("sbr: sbr range too wide");
    }

    let f_master = if header.bs_freq_scale == 0 {
        master_table_linear(k0, k2, header.bs_alter_scale)?
    }
    else {
        master_table_log(k0, k2, header.bs_freq_scale, header.bs_alter_scale)?
    };

    let n_master = f_master.len() - 1;

    if f_master[n_master] != k2 {
        return decode_error("sbr: invalid master frequency band table");
    }

    // bs_xover_band < N_Master (4.6.18.3.6).
    let xover_band = usize::from(header.bs_xover_band);

    if xover_band >= n_master {
        return decode_error("sbr: bs_xover_band out of range");
    }

    // f_TableHigh(k) = f_Master(k + bs_xover_band), 0 <= k <= N_High.
    let f_table_high = f_master[xover_band..].to_vec();
    let n_high = f_table_high.len() - 1;

    let kx = f_table_high[0];
    let m = f_table_high[n_high] - kx;

    // k_x <= 32 and k_x + M <= 64 (4.6.18.3.6).
    if kx > MAX_KX || kx + m > NUM_QMF_BANDS {
        return decode_error("sbr: sbr range out of bounds");
    }

    // N_Low = INT(N_High / 2) + (N_High - 2 * INT(N_High / 2)).
    let n_low = n_high / 2 + (n_high - 2 * (n_high / 2));

    // f_TableLow(k) = f_TableHigh(i(k)), i(0) = 0, i(k) = 2k - (1 - (-1)^N_High) / 2.
    let odd_shift = n_high % 2;
    let f_table_low: Vec<usize> = (0..=n_low)
        .map(|k| if k == 0 { f_table_high[0] } else { f_table_high[2 * k - odd_shift] })
        .collect();

    // N_Q <= 5 (4.6.18.3.6).
    let n_q = num_noise_bands(header.bs_noise_bands, k2, kx);

    if n_q > MAX_NOISE_BANDS {
        return decode_error("sbr: too many noise floor bands");
    }

    // f_TableNoise(k) = f_TableLow(i(k)), i(0) = 0,
    // i(k) = i(k - 1) + INT((N_Low - i(k - 1)) / (N_Q + 1 - k)).
    let mut f_table_noise = Vec::with_capacity(n_q + 1);
    let mut i = 0;
    f_table_noise.push(f_table_low[0]);
    for k in 1..=n_q {
        i += (n_low - i) / (n_q + 1 - k);
        f_table_noise.push(f_table_low[i]);
    }

    let patches = compute_patches(&f_master, k0, kx, m, fs_sbr)?;

    let f_table_lim = limiter_table(header.bs_limiter_bands, &f_table_low, kx, &patches)?;

    Ok(FreqBandTables {
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(
        bs_start_freq: u8,
        bs_stop_freq: u8,
        bs_xover_band: u8,
        bs_freq_scale: u8,
        bs_alter_scale: bool,
        bs_noise_bands: u8,
        bs_limiter_bands: u8,
    ) -> SbrHeader {
        SbrHeader {
            bs_amp_res: true,
            bs_start_freq,
            bs_stop_freq,
            bs_xover_band,
            bs_freq_scale,
            bs_alter_scale,
            bs_noise_bands,
            bs_limiter_bands,
            bs_limiter_gains: 2,
            bs_interpol_freq: true,
            bs_smoothing_mode: true,
        }
    }

    /// Verify the structural properties of a set of tables.
    fn check_tables(t: &FreqBandTables) {
        let increasing = |v: &[usize]| v.windows(2).all(|w| w[0] < w[1]);

        assert!(increasing(&t.f_master));
        assert!(increasing(&t.f_table_high));
        assert!(increasing(&t.f_table_low));
        assert!(increasing(&t.f_table_lim));
        assert!(t.f_table_noise.windows(2).all(|w| w[0] <= w[1]));

        assert_eq!(t.f_master[0], t.k0);
        assert_eq!(*t.f_master.last().unwrap(), t.k2);
        assert_eq!(t.f_table_high[0], t.kx);
        assert_eq!(*t.f_table_high.last().unwrap(), t.kx + t.m);
        assert_eq!(t.f_table_low[0], t.kx);
        assert_eq!(*t.f_table_low.last().unwrap(), t.kx + t.m);
        assert_eq!(t.f_table_noise[0], t.kx);
        assert_eq!(*t.f_table_noise.last().unwrap(), t.kx + t.m);
        assert_eq!(t.f_table_lim[0], t.kx);
        assert!(*t.f_table_lim.last().unwrap() <= t.kx + t.m);

        // Every low resolution border is a high resolution border.
        assert!(t.f_table_low.iter().all(|f| t.f_table_high.contains(f)));

        // The patches are contiguous starting at k_x and lie within [k_x, k_x + M). The source
        // range of each patch lies within the low band [1, k0).
        assert!(!t.patches.is_empty() && t.patches.len() <= MAX_PATCHES);
        let mut dst = t.kx;
        for patch in &t.patches {
            assert!(patch.num_subbands > 0);
            assert!(patch.start_subband >= 1);
            assert!(patch.start_subband + patch.num_subbands <= t.k0);
            dst += patch.num_subbands;
        }
        assert!(dst <= t.kx + t.m);

        // The limiter table is a subset of the union of f_TableLow and the patch borders.
        let mut borders = vec![t.kx];
        for patch in &t.patches {
            borders.push(borders.last().unwrap() + patch.num_subbands);
        }
        assert!(t.f_table_lim.iter().all(|f| t.f_table_low.contains(f) || borders.contains(f)));
    }

    #[test]
    fn verify_sampling_frequency_mapping() {
        assert_eq!(sbr_sampling_frequency(44100), 44100);
        assert_eq!(sbr_sampling_frequency(48000), 48000);
        assert_eq!(sbr_sampling_frequency(32000), 32000);
        assert_eq!(sbr_sampling_frequency(96000), 96000);
        assert_eq!(sbr_sampling_frequency(16000), 16000);
        // A core sampling frequency of 22000 Hz maps to 22050 Hz.
        assert_eq!(sbr_sampling_frequency(44000), 44100);
        assert_eq!(sbr_sampling_frequency(0), 16000);
    }

    #[test]
    fn verify_k0_k2() {
        // Fs_SBR = 44100: startMin = NINT(4000 * 128 / 44100) = NINT(11.61) = 12.
        assert_eq!(compute_k0(0, 44100).unwrap(), 12 - 4);
        assert_eq!(compute_k0(5, 44100).unwrap(), 12 + 2);
        assert_eq!(compute_k0(15, 44100).unwrap(), 12 + 20);
        // Fs_SBR = 16000: startMin = NINT(3000 * 128 / 16000) = 24.
        assert_eq!(compute_k0(0, 16000).unwrap(), 24 - 8);
        // Fs_SBR = 96000: startMin = NINT(5000 * 128 / 96000) = NINT(6.67) = 7.
        assert_eq!(compute_k0(15, 96000).unwrap(), 7 + 24);
        assert!(compute_k0(16, 44100).is_err());

        // Fs_SBR = 44100: stopMin = NINT(8000 * 128 / 44100) = NINT(23.22) = 23. The borders
        // NINT(23 * (64 / 23)^(p / 13)) for p = 0..=13 are 23, 25, 27, 29, 32, 34, 37, 40, 43, 47,
        // 51, 55, 59, 64. Hence, stopDkSort = [2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5].
        assert_eq!(compute_k2(0, 14, 44100).unwrap(), 23);
        assert_eq!(compute_k2(8, 14, 44100).unwrap(), 23 + 20);
        assert_eq!(compute_k2(13, 14, 44100).unwrap(), 64);
        assert_eq!(compute_k2(14, 14, 44100).unwrap(), 28);
        assert_eq!(compute_k2(15, 14, 44100).unwrap(), 42);
        assert_eq!(compute_k2(15, 30, 44100).unwrap(), 64);
    }

    /// Fs_SBR = 44100, bs_start_freq = 5, bs_stop_freq = 8, bs_freq_scale = 2 (10 bands/octave),
    /// bs_alter_scale = 1, two regions.
    #[test]
    fn verify_tables_log_two_regions() {
        let hdr = header(5, 8, 0, 2, true, 2, 2);
        let t = derive_freq_band_tables(&hdr, 44100).unwrap();

        // k0 = 14, k2 = 43. k2 / k0 = 3.07 > 2.2449, therefore, k1 = 2 * k0 = 28.
        assert_eq!((t.k0, t.k2), (14, 43));

        // Region 0: numBands0 = 2 * NINT(10 * log2(28 / 14) / 2) = 10. The borders
        // NINT(14 * 2^(k / 10)) for k = 0..=10 are 14, 15, 16, 17, 18, 20, 21, 23, 24, 26, 28,
        // hence vDk0 = sort([1, 1, 1, 1, 2, 1, 2, 1, 2, 2]) = [1, 1, 1, 1, 1, 1, 2, 2, 2, 2].
        //
        // Region 1: numBands1 = 2 * NINT(10 * log2(43 / 28) / (2 * 1.3)) = 2 * NINT(2.38) = 4.
        // The borders NINT(28 * (43 / 28)^(k / 4)) for k = 0..=4 are 28, 31, 35, 39, 43, hence
        // vDk1 = [3, 4, 4, 4]. min(vDk1) >= max(vDk0), thus no change is applied.
        assert_eq!(t.f_master, [14, 15, 16, 17, 18, 19, 20, 22, 24, 26, 28, 31, 35, 39, 43]);

        // bs_xover_band = 0.
        assert_eq!(t.f_table_high, t.f_master);
        assert_eq!((t.kx, t.m), (14, 29));

        // N_High = 14 (even), N_Low = 7, i(k) = 2k.
        assert_eq!(t.f_table_low, [14, 16, 18, 20, 24, 28, 35, 43]);

        // N_Q = NINT(2 * log2(43 / 14)) = NINT(3.24) = 3. i = [0, 2, 4, 7].
        assert_eq!(t.f_table_noise, [14, 18, 24, 43]);

        // goalSb = NINT(2.048E6 / 44100) = 46 >= k_x + M, thus k = N_Master = 14.
        // Patch 0: sb = 26 (odd = 0), 12 subbands starting at 14 - 0 - 12 = 2.
        // Patch 1: sb = 35 (odd = 1), 9 subbands starting at 14 - 1 - 9 = 4.
        // Patch 2: sb = 43 (odd = 1), 8 subbands starting at 14 - 1 - 8 = 5.
        assert_eq!(
            t.patches,
            [
                Patch { num_subbands: 12, start_subband: 2 },
                Patch { num_subbands: 9, start_subband: 4 },
                Patch { num_subbands: 8, start_subband: 5 },
            ]
        );

        // limBands = 2. patchBorders = [14, 26, 35, 43].
        // limTable = sort([14, 16, 18, 20, 24, 28, 35, 43] + [26, 35]).
        //   = [14, 16, 18, 20, 24, 26, 28, 35, 35, 43]
        // k = 1: 2 * log2(16 / 14) = 0.39 < 0.49, 16 is not a patch border: remove 16.
        // k = 1: 2 * log2(18 / 14) = 0.73.
        // k = 2: 2 * log2(20 / 18) = 0.30, 20 is not a patch border: remove 20.
        // k = 2: 2 * log2(24 / 18) = 0.83.
        // k = 3: 2 * log2(26 / 24) = 0.23, 26 is a patch border, 24 is not: remove 24.
        // k = 3: 2 * log2(28 / 26) = 0.21, 28 is not a patch border: remove 28.
        // k = 3: 2 * log2(35 / 26) = 0.86.
        // k = 4: 35 == 35: remove 35.
        // k = 4: 2 * log2(43 / 35) = 0.59.
        assert_eq!(t.f_table_lim, [14, 18, 26, 35, 43]);

        check_tables(&t);
    }

    /// Fs_SBR = 48000, bs_start_freq = 7, bs_stop_freq = 14, bs_xover_band = 1, bs_freq_scale = 0,
    /// bs_alter_scale = 0.
    #[test]
    fn verify_tables_linear() {
        let hdr = header(7, 14, 1, 0, false, 1, 3);
        let t = derive_freq_band_tables(&hdr, 48000).unwrap();

        // startMin = NINT(4000 * 128 / 48000) = 11, k0 = 11 + 4 = 15, k2 = min(64, 2 * 15) = 30.
        assert_eq!((t.k0, t.k2), (15, 30));

        // dk = 1, numBands = 2 * INT(15 / 2) = 14, k2Achieved = 29, k2Diff = 1. Therefore, the
        // last band is widened by one.
        assert_eq!(t.f_master, [15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 30]);

        // bs_xover_band = 1: N_High = 13, k_x = 16, M = 14.
        assert_eq!(t.f_table_high, &t.f_master[1..]);
        assert_eq!((t.kx, t.m), (16, 14));

        // N_High is odd: N_Low = 7, i(k) = 2k - 1.
        assert_eq!(t.f_table_low, [16, 17, 19, 21, 23, 25, 27, 30]);

        // N_Q = max(1, NINT(1 * log2(30 / 16))) = 1.
        assert_eq!(t.f_table_noise, [16, 30]);

        // goalSb = NINT(2.048E6 / 48000) = 43 >= k_x + M, thus k = N_Master = 14.
        // Patch 0: sb = 28 (odd = 1), 12 subbands starting at 15 - 1 - 12 = 2.
        // Patch 1: sb = 30 (odd = 1), 2 subbands. It is removed since it is narrower than 3
        // subbands.
        assert_eq!(t.patches, [Patch { num_subbands: 12, start_subband: 2 }]);

        // limBands = 3. patchBorders = [16, 28]. limTable = [16, 17, 19, 21, 23, 25, 27, 30].
        // k = 1: 3 * log2(17 / 16) = 0.26, remove 17.
        // k = 1: 3 * log2(19 / 16) = 0.74.
        // k = 2: 3 * log2(21 / 19) = 0.43, remove 21.
        // k = 2: 3 * log2(23 / 19) = 0.83.
        // k = 3: 3 * log2(25 / 23) = 0.36, remove 25.
        // k = 3: 3 * log2(27 / 23) = 0.69.
        // k = 4: 3 * log2(30 / 27) = 0.46, 30 is not a patch border: remove 30.
        assert_eq!(t.f_table_lim, [16, 19, 23, 27]);

        check_tables(&t);
    }

    /// Fs_SBR = 48000, bs_start_freq = 13, bs_stop_freq = 14, bs_freq_scale = 1 (12
    /// bands/octave), one region.
    #[test]
    fn verify_tables_log_one_region() {
        let hdr = header(13, 14, 0, 1, false, 2, 0);
        let t = derive_freq_band_tables(&hdr, 48000).unwrap();

        // k0 = 11 + 13 = 24, k2 = 48. k2 / k0 = 2 <= 2.2449, therefore, k1 = k2.
        assert_eq!((t.k0, t.k2), (24, 48));

        // numBands0 = 2 * NINT(12 * log2(2) / 2) = 12. The borders NINT(24 * 2^(k / 12)) for
        // k = 0..=12 are 24, 25, 27, 29, 30, 32, 34, 36, 38, 40, 43, 45, 48, hence
        // vDk0 = sort([1, 2, 2, 1, 2, 2, 2, 2, 2, 3, 2, 3]).
        assert_eq!(t.f_master, [24, 25, 26, 28, 30, 32, 34, 36, 38, 40, 42, 45, 48]);

        // N_High = 12, N_Low = 6.
        assert_eq!(t.f_table_low, [24, 26, 30, 34, 38, 42, 48]);

        // N_Q = NINT(2 * log2(48 / 24)) = 2. i = [0, 3, 6].
        assert_eq!(t.f_table_noise, [24, 34, 48]);

        // goalSb = NINT(2.048E6 / 48000) = 43 < k_x + M, thus k = 11 (f_Master(11) = 45).
        // Patch 0: sb = 45 (odd = 1), 21 subbands starting at 24 - 1 - 21 = 2.
        // Patch 1: sb = 48 (odd = 0), 3 subbands starting at 24 - 0 - 3 = 21.
        assert_eq!(
            t.patches,
            [
                Patch { num_subbands: 21, start_subband: 2 },
                Patch { num_subbands: 3, start_subband: 21 },
            ]
        );

        // bs_limiter_bands = 0: a single limiter band.
        assert_eq!(t.f_table_lim, [24, 48]);

        check_tables(&t);
    }

    /// Fs_SBR = 32000, bs_start_freq = 0, bs_stop_freq = 9, bs_freq_scale = 3 (8 bands/octave),
    /// bs_alter_scale = 1, two regions. This header requires more than 5 patches.
    #[test]
    fn verify_tables_too_many_patches() {
        // startMin = NINT(4000 * 128 / 32000) = 16, k0 = 16 - 6 = 10.
        // stopMin = NINT(8000 * 128 / 32000) = 32. The borders NINT(32 * 2^(p / 13)) for
        // p = 0..=13 are 32, 34, 36, 38, 40, 42, 44, 46, 49, 52, 55, 58, 61, 64, hence
        // stopDkSort = [2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3] and k2 = 32 + 20 = 52.
        assert_eq!(compute_k0(0, 32000).unwrap(), 10);
        assert_eq!(compute_k2(9, 10, 32000).unwrap(), 52);

        // k1 = 20. numBands0 = 2 * NINT(8 * 1 / 2) = 8. The borders NINT(10 * 2^(k / 8)) are
        // 10, 11, 12, 13, 14, 15, 17, 18, 20, hence vDk0 = [1, 1, 1, 1, 1, 1, 2, 2] after sorting.
        //
        // numBands1 = 2 * NINT(8 * log2(52 / 20) / (2 * 1.3)) = 2 * NINT(4.24) = 8. The borders
        // NINT(20 * 2.6^(k / 8)) are 20, 23, 25, 29, 32, 36, 41, 46, 52, hence
        // vDk1 = [2, 3, 3, 4, 4, 5, 5, 6] after sorting. min(vDk1) >= max(vDk0): no change.
        assert_eq!(
            master_table_log(10, 52, 3, true).unwrap(),
            [10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 25, 28, 32, 36, 41, 46, 52]
        );

        // The patch construction yields 7 patches: (8, 2), (7, 2), (7, 3), (4, 6), (5, 4),
        // (5, 5), (6, 4), more than allowed by 4.6.18.3.6.
        let hdr = header(0, 9, 0, 3, true, 2, 1);
        assert!(derive_freq_band_tables(&hdr, 32000).is_err());

        let f_master = master_table_log(10, 52, 3, true).unwrap();
        assert!(compute_patches(&f_master, 10, 10, 42, 32000).is_err());
    }

    #[test]
    fn verify_master_table_flowchart_cases() {
        // bs_alter_scale = 1, k2 - k0 = 14: dk = 2, numBands = 2 * NINT(3.5) = 8,
        // k2Achieved = k0 + 16, k2Diff = -2: the first two bands are narrowed by one.
        assert_eq!(
            master_table_linear(10, 24, true).unwrap(),
            [10, 11, 12, 14, 16, 18, 20, 22, 24]
        );

        // bs_alter_scale = 1, k2 - k0 = 10: numBands = 2 * NINT(2.5) = 6, k2Diff = -2.
        assert_eq!(master_table_linear(10, 20, true).unwrap(), [10, 11, 12, 14, 16, 18, 20]);

        // bs_alter_scale = 1, k2 - k0 = 13: numBands = 2 * NINT(3.25) = 6, k2Diff = 1: the
        // last band is widened by one.
        assert_eq!(master_table_linear(10, 23, true).unwrap(), [10, 12, 14, 16, 18, 20, 23]);

        // bs_alter_scale = 1, k2 - k0 = 1: numBands = 2 * NINT(0.25) = 0 is not allowed.
        assert!(master_table_linear(10, 11, true).is_err());

        // bs_alter_scale = 0, k2 - k0 = 1: numBands = 2 * INT(0.5) = 0 is not allowed.
        assert!(master_table_linear(10, 11, false).is_err());

        // k0 = 16, k2 = 64, bs_freq_scale = 1 (12 bands/octave), warp = 1.0: k1 = 32,
        // numBands0 = 12, the borders NINT(16 * 2^(k / 12)) are 16, 17, 18, 19, 20, 21, 23, 24,
        // 25, 27, 29, 30, 32, so vDk0 = [1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2] after sorting.
        // numBands1 = 12, the borders NINT(32 * 2^(k / 12)) are 32, 34, 36, 38, 40, 43, 45, 48,
        // 51, 54, 57, 60, 64, so vDk1 = [2, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 4] after sorting.
        assert_eq!(
            master_table_log(16, 64, 1, false).unwrap(),
            [
                16, 17, 18, 19, 20, 21, 22, 23, 24, 26, 28, 30, 32, 34, 36, 38, 40, 42, 45, 48, 51,
                54, 57, 60, 64
            ]
        );

        // k0 = 9, k2 = 32, bs_freq_scale = 3 (8 bands/octave), warp = 1.3: k1 = 18,
        // numBands0 = 8. The borders NINT(9 * 2^(k / 8)) are 9, 10, 11, 12, 13, 14, 15, 17, 18,
        // hence vDk0 = [1, 1, 1, 1, 1, 1, 1, 2] and max(vDk0) = 2.
        // numBands1 = 2 * NINT(8 * log2(32 / 18) / 2.6) = 2 * NINT(2.55) = 6. The borders
        // NINT(18 * (32 / 18)^(k / 6)) are 18, 20, 22, 24, 26, 29, 32, hence
        // vDk1 = [2, 2, 2, 2, 3, 3]. min(vDk1) = 2 is not less than 2: no change.
        assert_eq!(
            master_table_log(9, 32, 3, true).unwrap(),
            [9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 26, 29, 32]
        );

        // k0 = 12, k2 = 32, bs_freq_scale = 3, warp = 1.0. k2 / k0 = 2.67 > 2.2449, k1 = 24.
        // numBands0 = 8. The borders NINT(12 * 2^(k / 8)) are 12, 13, 14, 16, 17, 19, 20, 22,
        // 24, hence vDk0 = [1, 1, 1, 1, 2, 2, 2, 2].
        // numBands1 = 2 * NINT(8 * log2(32 / 24) / 2) = 2 * NINT(1.66) = 4. The borders
        // NINT(24 * (4 / 3)^(k / 4)) are 24, 26, 28, 30, 32, hence vDk1 = [2, 2, 2, 2].
        assert_eq!(
            master_table_log(12, 32, 3, false).unwrap(),
            [12, 13, 14, 15, 16, 18, 20, 22, 24, 26, 28, 30, 32]
        );

        // k0 = 10, k2 = 25, bs_freq_scale = 3, warp = 1.0. k1 = 20, numBands0 = 8. The borders
        // NINT(10 * 2^(k / 8)) are 10, 11, 12, 13, 14, 15, 17, 18, 20, hence
        // vDk0 = [1, 1, 1, 1, 1, 1, 2, 2]. numBands1 = 2 * NINT(8 * log2(25 / 20) / 2)
        // = 2 * NINT(1.29) = 2. The borders NINT(20 * 1.25^(k / 2)) are 20, 22, 25, hence
        // vDk1 = [2, 3].
        assert_eq!(
            master_table_log(10, 25, 3, false).unwrap(),
            [10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 25]
        );
    }

    #[test]
    fn verify_master_table_band_width_change() {
        // k0 = 9, k2 = 30, bs_freq_scale = 3 (8 bands/octave), warp = 1.0. k1 = 18,
        // numBands0 = 8. The borders NINT(9 * 2^(k / 8)) are 9, 10, 11, 12, 13, 14, 15, 17, 18,
        // hence vDk0 = [1, 1, 1, 1, 1, 1, 1, 2] and max(vDk0) = 2.
        // numBands1 = 2 * NINT(8 * log2(30 / 18) / 2) = 2 * NINT(2.95) = 6. The borders
        // NINT(18 * (30 / 18)^(k / 6)) are 18, 20, 21, 23, 25, 28, 30, hence
        // vDk1 = [2, 1, 2, 2, 3, 2]. min(vDk1) = 1 < 2: vDk1 = sort(vDk1) = [1, 2, 2, 2, 2, 3],
        // change = 2 - 1 = 1 <= INT((3 - 1) / 2) = 1. vDk1 = [2, 2, 2, 2, 2, 2].
        assert_eq!(
            master_table_log(9, 30, 3, false).unwrap(),
            [9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 26, 28, 30]
        );

        // k0 = 14, k2 = 32, bs_freq_scale = 3, warp = 1.0. k1 = 28, numBands0 = 8. The borders
        // NINT(14 * 2^(k / 8)) are 14, 15, 17, 18, 20, 22, 24, 26, 28, hence
        // vDk0 = [1, 1, 2, 2, 2, 2, 2, 2] and max(vDk0) = 2. numBands1 = 2 * NINT(0.77) = 2. The
        // borders NINT(28 * (32 / 28)^(k / 2)) are 28, 30, 32, hence vDk1 = [2, 2]: no change.
        assert_eq!(
            master_table_log(14, 32, 3, false).unwrap(),
            [14, 15, 16, 18, 20, 22, 24, 26, 28, 30, 32]
        );
    }

    #[test]
    fn verify_master_table_invalid_band_widths() {
        // k0 = 4, k2 = 64, bs_freq_scale = 3 (8 bands/octave): k1 = 8, numBands0 = 8. The
        // borders NINT(4 * 2^(k / 8)) are 4, 4, 5, 5, 6, 6, 7, 7, 8, hence vDk0 contains zeros.
        assert!(master_table_log(4, 64, 3, false).is_err());

        // k0 = 5, k2 = 13, bs_freq_scale = 1 (12 bands/octave), warp = 1.3: k1 = 10,
        // numBands0 = 12 > k1 - k0.
        assert!(master_table_log(5, 13, 1, true).is_err());

        // k0 = 8, k2 = 21, bs_freq_scale = 1, warp = 1.3: k1 = 16, numBands0 = 12 > 8.
        assert!(master_table_log(8, 21, 1, true).is_err());

        // k0 = 7, k2 = 16, bs_freq_scale = 3, warp = 1.3: k1 = 14, numBands0 = 8 > 7.
        assert!(master_table_log(7, 16, 3, true).is_err());

        // k0 = 6, k2 = 15, bs_freq_scale = 2 (10 bands/octave), warp = 1.0: k1 = 12,
        // numBands0 = 10 > 6.
        assert!(master_table_log(6, 15, 2, false).is_err());

        // Invalid ranges.
        assert!(master_table_log(20, 20, 2, false).is_err());
        assert!(master_table_log(20, 65, 2, false).is_err());
        assert!(master_table_log(0, 20, 2, false).is_err());
        assert!(master_table_log(10, 20, 0, false).is_err());
        assert!(master_table_linear(20, 20, false).is_err());
    }

    #[test]
    fn verify_constraint_violations() {
        // bs_xover_band >= N_Master: Fs_SBR = 44100, k0 = 14, k2 = 23, bs_freq_scale = 3. One
        // region with numBands0 = 2 * NINT(8 * log2(23 / 14) / 2) = 6, hence N_Master = 6.
        let hdr = header(5, 0, 5, 3, true, 2, 2);
        let t = derive_freq_band_tables(&hdr, 44100).unwrap();
        assert_eq!(t.f_master, [14, 15, 16, 17, 19, 21, 23]);
        assert_eq!(t.f_table_high, [21, 23]);
        let hdr = header(5, 0, 6, 3, true, 2, 2);
        assert!(derive_freq_band_tables(&hdr, 44100).is_err());

        // k2 <= k0: Fs_SBR = 44100, k0 = 32 (bs_start_freq = 15), k2 = 23 (bs_stop_freq = 0).
        let hdr = header(15, 0, 0, 2, true, 2, 2);
        assert!(derive_freq_band_tables(&hdr, 44100).is_err());

        // k2 - k0 > 32 for Fs_SBR = 48000: k0 = 11 (bs_start_freq = 3), k2 = 21 + 24 = 45
        // (bs_stop_freq = 9, stopDkSort = [2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 5]).
        let hdr = header(3, 9, 0, 2, true, 2, 2);
        assert!(derive_freq_band_tables(&hdr, 48000).is_err());
        let hdr = header(3, 8, 0, 2, true, 2, 2);
        assert!(derive_freq_band_tables(&hdr, 48000).is_ok());

        // k2 - k0 > 35 for Fs_SBR = 44100: k0 = 11 (bs_start_freq = 2), k2 = 23 + 24 = 47.
        let hdr = header(2, 9, 0, 2, true, 2, 2);
        assert!(derive_freq_band_tables(&hdr, 44100).is_err());

        // k_x > 32: Fs_SBR = 16000, k0 = 24 + 7 = 31 (bs_start_freq = 15), k2 = 64
        // (bs_stop_freq = 15), bs_xover_band = 2.
        let hdr = header(15, 15, 2, 0, false, 2, 2);
        assert!(derive_freq_band_tables(&hdr, 16000).is_err());
        let hdr = header(15, 15, 1, 0, false, 2, 2);
        assert!(derive_freq_band_tables(&hdr, 16000).is_ok());

        // N_Q > 5: bs_noise_bands = 3 over log2(43 / 14) = 1.62 octaves, N_Q = NINT(4.86) = 5 is
        // allowed. Over log2(47 / 11) = 2.1 octaves at Fs_SBR = 32000, N_Q = NINT(6.3) = 6 is not.
        let hdr = header(5, 8, 0, 2, true, 3, 2);
        assert_eq!(derive_freq_band_tables(&hdr, 44100).unwrap().f_table_noise.len(), 6);
        assert_eq!(num_noise_bands(3, 47, 11), 6);
    }

    /// Verify that every header for every sampling frequency either produces consistent tables or
    /// an error, and never panics.
    #[test]
    fn verify_exhaustive_headers() {
        let rates = [16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000];

        let mut num_ok = 0;

        for fs in rates {
            for start in 0..16 {
                for stop in 0..16 {
                    for xover in 0..8 {
                        for freq_scale in 0..4 {
                            for alter_scale in [false, true] {
                                for noise_bands in 0..4 {
                                    let hdr = header(
                                        start,
                                        stop,
                                        xover,
                                        freq_scale,
                                        alter_scale,
                                        noise_bands,
                                        (start + stop) % 4,
                                    );
                                    if let Ok(t) = derive_freq_band_tables(&hdr, fs) {
                                        check_tables(&t);
                                        assert!(t.k2 - t.k0 <= max_sbr_range(fs));
                                        assert!(t.kx <= 32 && t.kx + t.m <= 64);
                                        assert!(t.f_table_noise.len() <= MAX_NOISE_BANDS + 1);
                                        num_ok += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        assert!(num_ok > 10000);
    }
}
