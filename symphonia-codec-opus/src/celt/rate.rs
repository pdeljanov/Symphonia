//! CELT Dynamic Bit Allocation, Pulse Cache Lookup, and Time-Frequency Resolution
//! (RFC 6716 Sections 4.3.1, 4.3.3, and 4.3.4.1).
//!
//! Unlike fixed-rate transform codecs that transmit explicit scale-factor and codebook indices for
//! every subband, CELT derives almost the entire per-band bit allocation implicitly from the known
//! total frame bit budget (`rate.c`). Both encoder and decoder run the exact same deterministic
//! bit-allocation algorithm on the range coder state, modified only by a small number of
//! bitstream-signaled parameters:
//!
//! 1. **Per-Band Caps ([`init_caps`])**: Maximum PVQ bit capacity per band for the frame size
//!    `lm` and channel count `channels`.
//! 2. **Dynamic Band Boosts ([`decode_band_boosts_and_trim`])**: Optional per-band bit boosts
//!    (`offsets[i]`) and global spectral tilt adjustment (`alloc_trim`).
//! 3. **Rate-Distortion Curve Interpolation ([`clt_compute_allocation`])**: Binary search across
//!    the 11 static rate-allocation vectors ([`ALLOC_VECTORS`]) followed by 6-step linear
//!    interpolation, backwards band-skipping signaling, stereo intensity/dual-stereo parameter
//!    decoding, and partitioning of each band's bit budget into fine energy bits (`fine_quant`)
//!    and PVQ shape bits (`pulses` in Q3 `1/8`-bit units).
//! 4. **Pulse Cache Lookup ([`bits2pulses`], [`pulses2bits`], [`get_pulses`])**: Maps Q3 bit
//!    allocations to exact PVQ pulse counts $K$ via precomputed CWRS codebook size tables.

use crate::celt::constant::{
    ALLOC_STEPS, ALLOC_VECTORS, BITRES, CACHE_BITS, CACHE_CAPS, CACHE_INDEX, EBANDS, FINE_OFFSET,
    LOG2_FRAC_TABLE, LOGN, LOG_MAX_PSEUDO, MAX_BANDS, MAX_FINE_BITS, NB_ALLOC_VECTORS, SPREAD_ICDF,
    SPREAD_NORMAL, TAPSET_ICDF, TF_SELECT_TABLE, TRIM_ICDF, band_width,
};
use crate::entropy::Decoder;
use symphonia_core::errors::Result;

/// Result of CELT dynamic bit allocation for a single frame (RFC 6716 Section 4.3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BandAllocation {
    /// PVQ shape bit allocation per band in Q3 (`1/8`-bit) units (`pulses` in `libopus`).
    pub pulses: [i32; MAX_BANDS],
    /// Number of fine energy quantization bits per channel in each band (`0..=8`).
    pub fine_quant: [i32; MAX_BANDS],
    /// Priority class (`0` or `1`) for the final 1-bit energy refinement pass.
    pub fine_priority: [i32; MAX_BANDS],
    /// Surplus Q3 bits exceeding the cap on the highest coded bands, carried into PVQ rebalancing.
    pub balance: i32,
    /// One past the index of the highest non-skipped band (`start < coded_bands <= end`).
    pub coded_bands: usize,
    /// First band index at which stereo intensity coding begins (`0..=coded_bands`).
    pub intensity: usize,
    /// Whether dual-stereo (independent L/R PVQ coding rather than mid/side) is enabled.
    pub dual_stereo: bool,
}

/// Pitch comb post-filter parameters decoded at the start of a CELT frame
/// (RFC 6716 Section 4.3.1, `celt_decoder.c` lines 1434-1450).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PostFilterParams {
    /// Pitch period in 48 kHz samples (`0` when the post-filter is disabled, or `15..=1022`).
    pub pitch: usize,
    /// Comb filter gain (`0.0` when disabled, or $0.09375 \times (q_g + 1)$).
    pub gain: f32,
    /// Comb filter tapset index (`0`, `1`, or `2`, indexing [`COMB_FILTER_GAINS`](crate::celt::constant::COMB_FILTER_GAINS)).
    pub tapset: usize,
}

/// Converts a pseudo-pulse index $i \in [0, 40]$ to the actual number of PVQ pulses $K$
/// (RFC 6716 Section 4.3.4, `get_pulses` in `rate.h`).
///
/// For $i < 8$, $K = i$. For $i \ge 8$, the pulse count grows semi-logarithmically:
/// $$K = (8 + (i \bmod 8)) \cdot 2^{\lfloor i / 8 \rfloor - 1}$$
#[inline]
pub const fn get_pulses(pseudo_pulse_idx: usize) -> usize {
    if pseudo_pulse_idx < 8 {
        return pseudo_pulse_idx;
    }
    return (8 + (pseudo_pulse_idx & 7)) << ((pseudo_pulse_idx >> 3) - 1);
}

/// Returns the pulse-cache slice for a given block-size exponent `lm` (`-1..=3`) and `band`
/// (`0..21`) from [`CACHE_BITS`] and [`CACHE_INDEX`].
///
/// Note that during recursive PVQ band splitting (`quant_partition` in `bands.c`), `lm` can be
/// decremented down to `-1`, which maps to row `(lm + 1) = 0` of [`CACHE_INDEX`].
#[inline]
pub fn pulse_cache_slice(lm: isize, band: usize) -> &'static [u8] {
    let row = (lm + 1) as usize;
    let start_offset = CACHE_INDEX[row * MAX_BANDS + band] as usize;
    return &CACHE_BITS[start_offset..];
}

/// Returns the maximum cached Q3 bit count for `lm` (`-1..=3`) and `band` (`0..21`), used by
/// PVQ partition splitting (`b > max_cache_bits(lm, band) + 12` in `bands.c`).
#[inline]
pub fn max_cache_bits(lm: isize, band: usize) -> i32 {
    let cache = pulse_cache_slice(lm, band);
    let max_pseudo = usize::from(cache[0]);
    return i32::from(cache[max_pseudo]);
}

/// Maps a Q3 bit budget `bits` to a pseudo-pulse index $q \in [0, 40]$ for level `lm` (`-1..=3`)
/// and `band` (`0..21`) via 6-step binary search (`bits2pulses` in `rate.h`).
///
/// Selects the pseudo-pulse index whose cached bit cost is closest to `bits` (preferring the
/// lower index on ties).
#[inline]
pub fn bits2pulses_at_level(lm: isize, band: usize, bits: i32) -> i32 {
    let cache = pulse_cache_slice(lm, band);
    let mut lo = 0usize;
    let mut hi = usize::from(cache[0]);
    let target = bits - 1;

    for _ in 0..LOG_MAX_PSEUDO {
        let mid = (lo + hi + 1) >> 1;
        if i32::from(cache[mid]) >= target {
            hi = mid;
        } else {
            lo = mid;
        }
    }

    let lo_bits = if lo == 0 { -1 } else { i32::from(cache[lo]) };
    let hi_bits = i32::from(cache[hi]);
    if target - lo_bits <= hi_bits - target {
        return lo as i32;
    }
    return hi as i32;
}

/// Maps a Q3 bit budget `bits` to a pseudo-pulse index $q \in [0, 40]$ for frame size exponent
/// `lm` (`0..=3`) and `band` (`0..21`) (`bits2pulses` in `rate.h`).
#[inline]
pub fn bits2pulses(lm: usize, band: usize, bits: i32) -> i32 {
    return bits2pulses_at_level(lm as isize, band, bits);
}

/// Maps a pseudo-pulse index `pulses` ($q \in [0, 40]$) back to its exact Q3 bit cost for level
/// `lm` (`-1..=3`) and `band` (`0..21`) (`pulses2bits` in `rate.h`).
#[inline]
pub fn pulses2bits_at_level(lm: isize, band: usize, pulses: i32) -> i32 {
    if pulses <= 0 {
        return 0;
    }
    let cache = pulse_cache_slice(lm, band);
    return i32::from(cache[pulses as usize]) + 1;
}

/// Maps a pseudo-pulse index `pulses` ($q \in [0, 40]$) back to its exact Q3 bit cost for frame
/// size exponent `lm` (`0..=3`) and `band` (`0..21`) (`pulses2bits` in `rate.h`).
#[inline]
pub fn pulses2bits(lm: usize, band: usize, pulses: i32) -> i32 {
    return pulses2bits_at_level(lm as isize, band, pulses);
}

/// Computes the maximum allowable Q3 bit allocation (`cap[band]`) for each of the 21 Bark bands
/// given frame size exponent `lm` (`0..=3`) and `channels` (`1` or `2`) (`init_caps` in `celt.c`).
///
/// $$\text{cap}[i] = \left\lfloor \frac{(\text{CACHE\_CAPS}[21 \cdot (2 \cdot \text{lm} + C - 1) + i] + 64) \cdot C \cdot (N_0[i] \ll \text{lm})}{4} \right\rfloor$$
pub fn init_caps(lm: usize, channels: usize) -> [i32; MAX_BANDS] {
    let mut cap = [0i32; MAX_BANDS];
    let table_offset = MAX_BANDS * (2 * lm + channels - 1);
    let c = channels as i32;

    for (band, cap_slot) in cap.iter_mut().enumerate() {
        let n = band_width(band, lm) as i32;
        let cached = i32::from(CACHE_CAPS[table_offset + band]);
        *cap_slot = ((cached + 64) * c * n) >> 2;
    }

    return cap;
}

/// Decodes the pitch comb post-filter parameters at the beginning of a CELT frame when
/// `start == 0` and at least 16 bits remain in the frame (`celt_decoder.c` lines 1434-1450,
/// RFC 6716 Section 4.3.1).
pub fn decode_postfilter(
    range_decoder: &mut Decoder<'_>,
    start: usize,
    total_bits: i32,
) -> Result<PostFilterParams> {
    if start == 0 && range_decoder.tell() + 16 <= total_bits {
        let enabled = range_decoder.decode_symbol_logp(1)? != 0;
        if enabled {
            let octave = range_decoder.decode_uint(6)?;
            let fine_pitch = range_decoder.decode_raw_bits(4 + octave)?;
            let pitch = ((16u32 << octave) + fine_pitch - 1) as usize;
            let qg = range_decoder.decode_raw_bits(3)?;
            let tapset = if range_decoder.tell() + 2 <= total_bits {
                range_decoder.decode_symbol_with_icdf(&TAPSET_ICDF)? as usize
            } else {
                0
            };
            let gain = 0.09375_f32 * ((qg + 1) as f32);
            return Ok(PostFilterParams {
                pitch,
                gain,
                tapset,
            });
        }
    }

    return Ok(PostFilterParams::default());
}

/// Decodes the per-band Time-Frequency (TF) resolution changes (`tf_res[start..end]`) from the
/// bitstream (`tf_decode` in `celt_decoder.c`, RFC 6716 Section 4.3.4.1).
///
/// Differential 1-bit flags are decoded across bands `start..end` with log-probabilities
/// `(2, 4)` in transient frames or `(4, 5)` in non-transient frames, reserving 1 bit for the
/// table-column selector `tf_select` whenever `lm > 0` and budget permits. Each band's binary
/// flag is then mapped through [`TF_SELECT_TABLE`].
pub fn decode_tf_changes(
    range_decoder: &mut Decoder<'_>,
    start: usize,
    end: usize,
    is_transient: bool,
    lm: usize,
    tf_res: &mut [i32; MAX_BANDS],
) -> Result<()> {
    let mut budget = (range_decoder.storage_bytes() * 8) as i32;
    let mut tell = range_decoder.tell();
    let mut logp: i32 = if is_transient { 2 } else { 4 };
    let tf_select_rsv = lm > 0 && tell + logp < budget;
    if tf_select_rsv {
        budget -= 1;
    }

    let mut tf_changed = 0usize;
    let mut curr = 0usize;
    for tf_slot in tf_res.iter_mut().take(end).skip(start) {
        if tell + logp <= budget {
            let bit = range_decoder.decode_symbol_logp(logp as u32)? as usize;
            curr ^= bit;
            tell = range_decoder.tell();
            tf_changed |= curr;
        }
        *tf_slot = curr as i32;
        logp = if is_transient { 4 } else { 5 };
    }

    let transient_offset = 4 * usize::from(is_transient);
    let mut tf_select = 0usize;
    if tf_select_rsv
        && TF_SELECT_TABLE[lm][transient_offset + tf_changed]
            != TF_SELECT_TABLE[lm][transient_offset + 2 + tf_changed]
    {
        tf_select = range_decoder.decode_symbol_logp(1)? as usize;
    }

    let table_base = transient_offset + 2 * tf_select;
    for tf_slot in tf_res.iter_mut().take(end).skip(start) {
        let flag = *tf_slot as usize;
        *tf_slot = i32::from(TF_SELECT_TABLE[lm][table_base + flag]);
    }

    return Ok(());
}

/// Convenience wrapper around [`decode_tf_changes`] that returns the decoded `[i32; MAX_BANDS]`
/// Time-Frequency resolution array (`tf_decode` in `celt_decoder.c`).
pub fn tf_decode(
    start: usize,
    end: usize,
    is_transient: bool,
    lm: usize,
    range_decoder: &mut Decoder<'_>,
) -> Result<[i32; MAX_BANDS]> {
    let mut tf_res = [0i32; MAX_BANDS];
    decode_tf_changes(range_decoder, start, end, is_transient, lm, &mut tf_res)?;
    return Ok(tf_res);
}

/// Decodes the PVQ spreading decision (`SPREAD_NONE = 0`, `SPREAD_LIGHT = 1`,
/// `SPREAD_NORMAL = 2`, `SPREAD_AGGRESSIVE = 3`) from the bitstream (`celt_decoder.c` lines 1505-1508).
pub fn decode_spread_decision(range_decoder: &mut Decoder<'_>, total_bits: i32) -> Result<usize> {
    if range_decoder.tell() + 4 <= total_bits {
        let spread = range_decoder.decode_symbol_with_icdf(&SPREAD_ICDF)? as usize;
        return Ok(spread);
    }
    return Ok(SPREAD_NORMAL);
}

/// Decodes dynamic per-band bit boosts (`offsets[start..end]`) and the global allocation tilt
/// parameter (`alloc_trim`) (`celt_decoder.c` lines 1516-1550, RFC 6716 Section 4.3.3).
///
/// Returns `(offsets, alloc_trim)`.
pub fn decode_band_boosts_and_trim(
    range_decoder: &mut Decoder<'_>,
    start: usize,
    end: usize,
    lm: usize,
    channels: usize,
    total_bits: i32,
    cap: &[i32; MAX_BANDS],
) -> Result<([i32; MAX_BANDS], i32)> {
    let mut offsets = [0i32; MAX_BANDS];
    let mut dynalloc_logp = 6i32;
    let mut total_bits_q3 = total_bits << BITRES;
    let mut tell_q3 = range_decoder.tell_frac() as i32;
    let c = channels as i32;

    for band in start..end {
        let width = c * (band_width(band, lm) as i32);
        let quanta = (width << BITRES).min((6 << BITRES).max(width));
        let mut dynalloc_loop_logp = dynalloc_logp;
        let mut boost = 0i32;

        while tell_q3 + (dynalloc_loop_logp << BITRES) < total_bits_q3 && boost < cap[band] {
            let flag = range_decoder.decode_symbol_logp(dynalloc_loop_logp as u32)?;
            tell_q3 = range_decoder.tell_frac() as i32;
            if flag == 0 {
                break;
            }
            boost += quanta;
            total_bits_q3 -= quanta;
            dynalloc_loop_logp = 1;
        }

        offsets[band] = boost;
        if boost > 0 {
            dynalloc_logp = (dynalloc_logp - 1).max(2);
        }
    }

    let alloc_trim = if tell_q3 + (6 << BITRES) <= total_bits_q3 {
        range_decoder.decode_symbol_with_icdf(&TRIM_ICDF)? as i32
    } else {
        5
    };

    return Ok((offsets, alloc_trim));
}

/// Interpolates between adjacent allocation curve bounds `bits1` and `bits1 + bits2`, decodes
/// backwards band-skipping flags and stereo coding parameters (`intensity`, `dual_stereo`),
/// redistributes leftover bits, and partitions each band's budget into fine energy bits and PVQ
/// pulse bits (`interp_bits2pulses` in `rate.c`, RFC 6716 Section 4.3.3).
#[allow(clippy::too_many_arguments)]
fn interpolate_and_partition_bits(
    range_decoder: &mut Decoder<'_>,
    start: usize,
    end: usize,
    skip_start: usize,
    bits1: &[i32; MAX_BANDS],
    bits2: &[i32; MAX_BANDS],
    thresh: &[i32; MAX_BANDS],
    cap: &[i32; MAX_BANDS],
    mut total: i32,
    skip_rsv: i32,
    mut intensity_rsv: i32,
    mut dual_stereo_rsv: i32,
    channels: usize,
    lm: usize,
) -> Result<BandAllocation> {
    let c = channels as i32;
    let alloc_floor = c << BITRES;
    let stereo_shift = usize::from(channels > 1);
    let log_m = (lm as i32) << BITRES;

    // 1. 6-step binary search for the fine interpolation weight `lo` in [0, 64].
    let mut lo = 0i32;
    let mut hi = 1i32 << ALLOC_STEPS;
    for _ in 0..ALLOC_STEPS {
        let mid = (lo + hi) >> 1;
        let mut psum = 0i32;
        let mut done = false;
        for j in (start..end).rev() {
            let tmp = bits1[j] + ((mid * bits2[j]) >> ALLOC_STEPS);
            match (tmp >= thresh[j] || done, tmp >= alloc_floor) {
                (true, _) => {
                    done = true;
                    psum += tmp.min(cap[j]);
                }
                (false, true) => psum += alloc_floor,
                (false, false) => {}
            }
        }
        if psum > total {
            hi = mid;
        } else {
            lo = mid;
        }
    }

    // 2. Evaluate the interpolated per-band bit curve at `lo`.
    let mut psum = 0i32;
    let mut done = false;
    let mut bits = [0i32; MAX_BANDS];
    for j in (start..end).rev() {
        let mut tmp = bits1[j] + ((lo * bits2[j]) >> ALLOC_STEPS);
        if tmp < thresh[j] && !done {
            if tmp >= alloc_floor {
                tmp = alloc_floor;
            } else {
                tmp = 0;
            }
        } else {
            done = true;
        }
        tmp = tmp.min(cap[j]);
        bits[j] = tmp;
        psum += tmp;
    }

    // 3. Work backwards from `end` to decide which high-frequency bands are skipped.
    let mut coded_bands = end;
    loop {
        let j = coded_bands - 1;
        if j <= skip_start {
            total += skip_rsv;
            break;
        }

        let left = total - psum;
        let coeff_span = (EBANDS[coded_bands] - EBANDS[start]) as i32;
        let percoeff = left / coeff_span;
        let rem_total = left - coeff_span * percoeff;
        let rem = (rem_total - ((EBANDS[j] - EBANDS[start]) as i32)).max(0);
        let band_w = (EBANDS[coded_bands] - EBANDS[j]) as i32;
        let mut band_bits = bits[j] + percoeff * band_w + rem;

        if band_bits >= thresh[j].max(alloc_floor + (1 << BITRES)) {
            if range_decoder.decode_symbol_logp(1)? != 0 {
                break;
            }
            psum += 1 << BITRES;
            band_bits -= 1 << BITRES;
        }

        psum -= bits[j] + intensity_rsv;
        if intensity_rsv > 0 {
            intensity_rsv = i32::from(LOG2_FRAC_TABLE[j - start]);
        }
        psum += intensity_rsv;

        if band_bits >= alloc_floor {
            psum += alloc_floor;
            bits[j] = alloc_floor;
        } else {
            bits[j] = 0;
        }

        coded_bands -= 1;
    }

    // 4. Decode stereo intensity start band and dual-stereo flag.
    let intensity = if intensity_rsv > 0 {
        let count = (coded_bands + 1 - start) as u32;
        start + (range_decoder.decode_uint(count)? as usize)
    } else {
        0
    };

    if intensity <= start {
        total += dual_stereo_rsv;
        dual_stereo_rsv = 0;
    }

    let dual_stereo = if dual_stereo_rsv > 0 {
        range_decoder.decode_symbol_logp(1)? != 0
    } else {
        false
    };

    // 5. Distribute remaining bits uniformly per MDCT coefficient across `start..coded_bands`.
    let mut left = total - psum;
    let coded_span = (EBANDS[coded_bands] - EBANDS[start]) as i32;
    let percoeff = left / coded_span;
    left -= coded_span * percoeff;

    for (j, bit_slot) in bits.iter_mut().enumerate().take(coded_bands).skip(start) {
        *bit_slot += percoeff * (band_width(j, 0) as i32);
    }
    for (j, bit_slot) in bits.iter_mut().enumerate().take(coded_bands).skip(start) {
        let add = left.min(band_width(j, 0) as i32);
        *bit_slot += add;
        left -= add;
    }

    // 6. Partition each band's total bits into fine energy (`fine_quant`) and PVQ shape (`bits`).
    let mut balance = 0i32;
    let mut fine_quant = [0i32; MAX_BANDS];
    let mut fine_priority = [0i32; MAX_BANDS];

    for j in start..coded_bands {
        let n = band_width(j, lm) as i32;
        let bit = bits[j] + balance;
        let mut excess;

        if n > 1 {
            excess = (bit - cap[j]).max(0);
            bits[j] = bit - excess;

            let extra_dof = i32::from(channels == 2 && n > 2 && !dual_stereo && j < intensity);
            let den = c * n + extra_dof;
            let nc_log_n = den * (i32::from(LOGN[j]) + log_m);

            let mut offset = (nc_log_n >> 1) - den * FINE_OFFSET;
            if n == 2 {
                offset += (den << BITRES) >> 2;
            }
            offset += match (
                bits[j] + offset < (den * 2) << BITRES,
                bits[j] + offset < (den * 3) << BITRES,
            ) {
                (true, _) => nc_log_n >> 2,
                (false, true) => nc_log_n >> 3,
                (false, false) => 0,
            };

            let num = (bits[j] + offset + (den << (BITRES - 1))).max(0);
            let mut ebits_j = (num / den) >> BITRES;

            if c * ebits_j > (bits[j] >> BITRES) {
                ebits_j = (bits[j] >> stereo_shift) >> BITRES;
            }
            ebits_j = ebits_j.min(MAX_FINE_BITS);

            fine_priority[j] = i32::from(ebits_j * (den << BITRES) >= bits[j] + offset);
            bits[j] -= (c * ebits_j) << BITRES;
            fine_quant[j] = ebits_j;
        } else {
            excess = (bit - (c << BITRES)).max(0);
            bits[j] = bit - excess;
            fine_quant[j] = 0;
            fine_priority[j] = 1;
        }

        if excess > 0 {
            let max_add = MAX_FINE_BITS - fine_quant[j];
            let extra_fine = (excess >> (stereo_shift + BITRES as usize)).min(max_add);
            fine_quant[j] += extra_fine;
            let extra_bits = (extra_fine * c) << BITRES;
            fine_priority[j] = i32::from(extra_bits >= excess - balance);
            excess -= extra_bits;
        }
        balance = excess;
    }

    // Skipped high-frequency bands (`coded_bands..end`) assign all their floor bits to fine energy.
    for j in coded_bands..end {
        fine_quant[j] = (bits[j] >> stereo_shift) >> BITRES;
        bits[j] = 0;
        fine_priority[j] = i32::from(fine_quant[j] < 1);
    }

    return Ok(BandAllocation {
        pulses: bits,
        fine_quant,
        fine_priority,
        balance,
        coded_bands,
        intensity,
        dual_stereo,
    });
}

/// Computes the complete CELT per-band bit allocation for bands `start..end`
/// (RFC 6716 Section 4.3.3, `clt_compute_allocation` in `rate.c`).
///
/// Given per-band dynamic boosts `offsets`, per-band caps `cap`, spectral tilt `alloc_trim`,
/// and remaining Q3 bit budget `total`, performs binary search and interpolation over
/// [`ALLOC_VECTORS`] and decodes band-skipping, intensity stereo, and dual-stereo flags from
/// `range_decoder`.
#[allow(clippy::too_many_arguments)]
pub fn clt_compute_allocation(
    range_decoder: &mut Decoder<'_>,
    start: usize,
    end: usize,
    offsets: &[i32; MAX_BANDS],
    cap: &[i32; MAX_BANDS],
    alloc_trim: i32,
    total: i32,
    channels: usize,
    lm: usize,
) -> Result<BandAllocation> {
    let mut total = total.max(0);
    let mut skip_start = start;

    // Reserve 1 bit (8 Q3 units) to signal the end of manually skipped bands.
    let skip_rsv = if total >= (1 << BITRES) {
        1 << BITRES
    } else {
        0
    };
    total -= skip_rsv;

    // In stereo, reserve fractional bits for intensity stereo and 1 bit for dual stereo.
    let mut intensity_rsv = 0i32;
    let mut dual_stereo_rsv = 0i32;
    if channels == 2 {
        intensity_rsv = i32::from(LOG2_FRAC_TABLE[end - start]);
        if intensity_rsv > total {
            intensity_rsv = 0;
        } else {
            total -= intensity_rsv;
            dual_stereo_rsv = if total >= (1 << BITRES) {
                1 << BITRES
            } else {
                0
            };
            total -= dual_stereo_rsv;
        }
    }

    let c = channels as i32;
    let mut thresh = [0i32; MAX_BANDS];
    let mut trim_offset = [0i32; MAX_BANDS];

    for j in start..end {
        let n0 = band_width(j, 0) as i32;
        // Minimum threshold below which no PVQ shape bits are allocated.
        thresh[j] = (c << BITRES).max((3 * (n0 << lm << BITRES)) >> 4);
        // Spectral tilt offset controlled by `alloc_trim` (neutral at 5).
        trim_offset[j] = (c
            * n0
            * (alloc_trim - 5 - (lm as i32))
            * ((end - j - 1) as i32)
            * (1 << (lm + BITRES as usize)))
            >> 6;
        // Single-coefficient bands get slightly lower threshold tilt because coarse energy
        // already provides one value per coefficient.
        if (n0 << lm) == 1 {
            trim_offset[j] -= c << BITRES;
        }
    }

    // Coarse binary search across the 11 static allocation vectors.
    let mut lo: isize = 1;
    let mut hi: isize = (NB_ALLOC_VECTORS as isize) - 1;
    while lo <= hi {
        let mut done = false;
        let mut psum = 0i32;
        let mid = ((lo + hi) >> 1) as usize;

        for j in (start..end).rev() {
            let n0 = band_width(j, 0) as i32;
            let mut bits_j = ((c * n0 * i32::from(ALLOC_VECTORS[mid][j])) << lm) >> 2;
            if bits_j > 0 {
                bits_j = (bits_j + trim_offset[j]).max(0);
            }
            bits_j += offsets[j];

            match (bits_j >= thresh[j] || done, bits_j >= (c << BITRES)) {
                (true, _) => {
                    done = true;
                    psum += bits_j.min(cap[j]);
                }
                (false, true) => psum += c << BITRES,
                (false, false) => {}
            }
        }

        if psum > total {
            hi = (mid as isize) - 1;
        } else {
            lo = (mid as isize) + 1;
        }
    }

    let hi_idx = lo as usize;
    let lo_idx = (lo - 1) as usize;

    let mut bits1 = [0i32; MAX_BANDS];
    let mut bits2 = [0i32; MAX_BANDS];

    for j in start..end {
        let n0 = band_width(j, 0) as i32;
        let mut bits1_j = ((c * n0 * i32::from(ALLOC_VECTORS[lo_idx][j])) << lm) >> 2;
        let mut bits2_j = if hi_idx >= NB_ALLOC_VECTORS {
            cap[j]
        } else {
            ((c * n0 * i32::from(ALLOC_VECTORS[hi_idx][j])) << lm) >> 2
        };

        if bits1_j > 0 {
            bits1_j = (bits1_j + trim_offset[j]).max(0);
        }
        if bits2_j > 0 {
            bits2_j = (bits2_j + trim_offset[j]).max(0);
        }
        if lo_idx > 0 {
            bits1_j += offsets[j];
        }
        bits2_j += offsets[j];

        if offsets[j] > 0 {
            skip_start = j;
        }

        bits2_j = (bits2_j - bits1_j).max(0);
        bits1[j] = bits1_j;
        bits2[j] = bits2_j;
    }

    return interpolate_and_partition_bits(
        range_decoder,
        start,
        end,
        skip_start,
        &bits1,
        &bits2,
        &thresh,
        cap,
        total,
        skip_rsv,
        intensity_rsv,
        dual_stereo_rsv,
        channels,
        lm,
    );
}

/// High-level helper that decodes dynamic band boosts, `alloc_trim`, anti-collapse reservation,
/// and the full [`BandAllocation`] for a CELT frame (`celt_decoder.c` lines 1510-1560).
///
/// Returns `(allocation, anti_collapse_rsv)` where `anti_collapse_rsv` is `1 << BITRES` (8 Q3
/// bits) if `is_transient && lm >= 2 && bits >= (lm + 2) << BITRES`, or `0` otherwise.
pub fn decode_frame_allocation(
    range_decoder: &mut Decoder<'_>,
    start: usize,
    end: usize,
    lm: usize,
    channels: usize,
    is_transient: bool,
    total_bits: i32,
) -> Result<(BandAllocation, i32)> {
    let cap = init_caps(lm, channels);
    let (offsets, alloc_trim) =
        decode_band_boosts_and_trim(range_decoder, start, end, lm, channels, total_bits, &cap)?;

    let mut bits = (total_bits << BITRES) - (range_decoder.tell_frac() as i32) - 1;
    let anti_collapse_rsv = if is_transient && lm >= 2 && bits >= (((lm as i32) + 2) << BITRES) {
        1 << BITRES
    } else {
        0
    };
    bits -= anti_collapse_rsv;

    let allocation = clt_compute_allocation(
        range_decoder,
        start,
        end,
        &offsets,
        &cap,
        alloc_trim,
        bits,
        channels,
        lm,
    )?;

    return Ok((allocation, anti_collapse_rsv));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies `get_pulses`, `bits2pulses`, and `pulses2bits` against the `libopus 1.6.1` C oracle.
    #[test]
    fn test_bits2pulses_and_pulses2bits_oracle() {
        assert_eq!(get_pulses(0), 0);
        assert_eq!(get_pulses(7), 7);
        assert_eq!(get_pulses(8), 8);
        assert_eq!(get_pulses(15), 15);
        assert_eq!(get_pulses(16), 16);
        assert_eq!(get_pulses(40), 128);

        // Tuple format: (lm, band, input_bits, expected_pulses, expected_roundtrip_bits)
        static ORACLE_CASES: [(usize, usize, i32, i32, i32); 32] = [
            (0, 0, 0, 0, 0),
            (0, 0, 13, 40, 8),
            (0, 10, 13, 1, 16),
            (0, 10, 26, 2, 24),
            (0, 10, 39, 7, 39),
            (0, 10, 52, 19, 52),
            (0, 15, 26, 1, 29),
            (0, 15, 52, 2, 50),
            (0, 20, 26, 1, 44),
            (1, 0, 13, 1, 16),
            (1, 0, 26, 2, 24),
            (1, 0, 39, 7, 39),
            (1, 0, 52, 19, 52),
            (1, 10, 13, 1, 24),
            (1, 10, 39, 2, 40),
            (1, 10, 52, 3, 52),
            (1, 15, 26, 1, 37),
            (1, 15, 52, 2, 66),
            (1, 20, 39, 1, 52),
            (2, 0, 13, 1, 24),
            (2, 0, 39, 2, 40),
            (2, 0, 52, 3, 52),
            (2, 10, 26, 1, 32),
            (2, 10, 52, 2, 56),
            (2, 15, 26, 1, 45),
            (2, 20, 39, 1, 60),
            (3, 0, 13, 0, 0),
            (3, 0, 26, 1, 32),
            (3, 0, 52, 2, 56),
            (3, 10, 26, 1, 40),
            (3, 15, 39, 1, 53),
            (3, 20, 39, 1, 68),
        ];

        for &(lm, band, bits, expected_p, expected_b) in &ORACLE_CASES {
            let p = bits2pulses(lm, band, bits);
            let b = pulses2bits(lm, band, p);
            assert_eq!(
                p, expected_p,
                "bits2pulses mismatch for lm={} band={} bits={}",
                lm, band, bits
            );
            assert_eq!(
                b, expected_b,
                "pulses2bits mismatch for lm={} band={} pulses={}",
                lm, band, p
            );
        }

        return;
    }

    /// Verifies `tf_decode` across non-transient, transient, and hybrid (`start = 17`) configurations
    /// against the `libopus 1.6.1` C oracle.
    #[test]
    fn test_tf_decode_oracle() {
        let mut packet = [0u8; 16];
        for (i, byte) in packet.iter_mut().enumerate() {
            *byte = ((i * 97 + 11) & 0xFF) as u8;
        }

        // Case 1: transient = false, lm = 3, start = 0, end = 21
        let mut dec1 = Decoder::new(&packet).expect("valid range decoder");
        let tf1 = tf_decode(0, 21, false, 3, &mut dec1).expect("tf_decode case 1 failed");
        assert_eq!(dec1.tell(), 3);
        assert_eq!(dec1.tell_frac(), 17);
        assert_eq!(tf1, [0; 21]);

        // Case 2: transient = true, lm = 2, start = 0, end = 21
        let mut dec2 = Decoder::new(&packet).expect("valid range decoder");
        let tf2 = tf_decode(0, 21, true, 2, &mut dec2).expect("tf_decode case 2 failed");
        assert_eq!(dec2.tell(), 5);
        assert_eq!(dec2.tell_frac(), 35);
        assert_eq!(tf2, [2; 21]);

        // Case 3: hybrid start = 17, end = 21, transient = false, lm = 2
        let mut dec3 = Decoder::new(&packet).expect("valid range decoder");
        let tf3 = tf_decode(17, 21, false, 2, &mut dec3).expect("tf_decode case 3 failed");
        assert_eq!(dec3.tell(), 2);
        assert_eq!(dec3.tell_frac(), 10);
        assert_eq!(tf3, [0; 21]);

        return;
    }

    /// Verifies `init_caps` and `clt_compute_allocation` for Mono (`C = 1`), `LM = 3` (20 ms),
    /// `start = 0, end = 21`, 40-byte frame against the `libopus 1.6.1` C oracle.
    #[test]
    fn test_clt_compute_allocation_mono_lm3_oracle() {
        let mut packet = [0u8; 40];
        for (i, byte) in packet.iter_mut().enumerate() {
            *byte = ((i * 41 + 7) & 0xFF) as u8;
        }
        let mut dec = Decoder::new(&packet).expect("valid range decoder");

        let cap = init_caps(3, 1);
        const EXPECTED_CAP: [i32; 21] = [
            514, 514, 514, 514, 514, 514, 514, 514, 1028, 1028, 1028, 1028, 2064, 2064, 2064,
            2976, 2976, 3792, 4872, 4644, 4532,
        ];
        assert_eq!(cap, EXPECTED_CAP);

        let mut offsets = [0i32; MAX_BANDS];
        for (i, slot) in offsets.iter_mut().enumerate() {
            if i % 5 == 0 {
                *slot = 6;
            }
        }
        let total = ((40 * 8) << BITRES) - (dec.tell_frac() as i32) - 1;
        let alloc = clt_compute_allocation(&mut dec, 0, 21, &offsets, &cap, 5, total, 1, 3)
            .expect("mono allocation failed");

        assert_eq!(alloc.coded_bands, 21);
        assert_eq!(alloc.balance, 0);
        assert_eq!(alloc.intensity, 0);
        assert!(!alloc.dual_stereo);
        assert_eq!(dec.tell_frac(), 8);

        const EXPECTED_PULSES: [i32; 21] = [
            164, 154, 143, 127, 115, 112, 106, 99, 183, 165, 153, 128, 227, 170, 120, 121, 0, 0,
            0, 0, 0,
        ];
        const EXPECTED_FINE_QUANT: [i32; 21] = [
            3, 2, 2, 2, 2, 2, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 0, 0, 0,
        ];
        const EXPECTED_FINE_PRIO: [i32; 21] = [
            1, 0, 0, 1, 1, 1, 0, 0, 1, 1, 1, 1, 0, 1, 1, 1, 0, 0, 0, 0, 0,
        ];

        assert_eq!(alloc.pulses, EXPECTED_PULSES);
        assert_eq!(alloc.fine_quant, EXPECTED_FINE_QUANT);
        assert_eq!(alloc.fine_priority, EXPECTED_FINE_PRIO);

        return;
    }

    /// Verifies `init_caps` and `clt_compute_allocation` for Stereo (`C = 2`), `LM = 2` (10 ms),
    /// `start = 0, end = 21`, 60-byte frame, `alloc_trim = 4` (including skipped band 20,
    /// `intensity = 3`, `dual_stereo = true`) against the `libopus 1.6.1` C oracle.
    #[test]
    fn test_clt_compute_allocation_stereo_lm2_oracle() {
        let mut packet = [0u8; 60];
        for (i, byte) in packet.iter_mut().enumerate() {
            *byte = ((i * 83 + 150) & 0xFF) as u8;
        }
        let mut dec = Decoder::new(&packet).expect("valid range decoder");

        let cap = init_caps(2, 2);
        const EXPECTED_CAP: [i32; 21] = [
            542, 542, 542, 542, 542, 542, 542, 542, 1072, 1072, 1072, 1072, 2120, 2120, 2120,
            3024, 3024, 3840, 4920, 4680, 4576,
        ];
        assert_eq!(cap, EXPECTED_CAP);

        let mut offsets = [0i32; MAX_BANDS];
        offsets[2] = 12;
        offsets[8] = 6;
        let total = ((60 * 8) << BITRES) - (dec.tell_frac() as i32) - 1;
        let alloc = clt_compute_allocation(&mut dec, 0, 21, &offsets, &cap, 4, total, 2, 2)
            .expect("stereo allocation failed");

        assert_eq!(alloc.coded_bands, 20);
        assert_eq!(alloc.balance, 0);
        assert_eq!(alloc.intensity, 3);
        assert!(alloc.dual_stereo);
        assert_eq!(dec.tell_frac(), 60);

        const EXPECTED_PULSES: [i32; 21] = [
            167, 156, 157, 150, 133, 124, 114, 106, 216, 192, 174, 156, 271, 239, 198, 228, 176,
            162, 99, 97, 0,
        ];
        const EXPECTED_FINE_QUANT: [i32; 21] = [
            2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 1, 1, 2, 1, 2, 2, 2, 1,
        ];
        const EXPECTED_FINE_PRIO: [i32; 21] = [
            1, 1, 1, 0, 0, 0, 1, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 0,
        ];

        assert_eq!(alloc.pulses, EXPECTED_PULSES);
        assert_eq!(alloc.fine_quant, EXPECTED_FINE_QUANT);
        assert_eq!(alloc.fine_priority, EXPECTED_FINE_PRIO);

        return;
    }

    /// Verifies Hybrid mode (`start = 17, end = 21`), Stereo (`C = 2`), `LM = 3` (20 ms)
    /// `clt_compute_allocation` against the `libopus 1.6.1` C oracle.
    #[test]
    fn test_clt_compute_allocation_hybrid_stereo_lm3_oracle() {
        let mut packet = [0u8; 32];
        for (i, byte) in packet.iter_mut().enumerate() {
            *byte = ((i * 29 + 99) & 0xFF) as u8;
        }
        let mut dec = Decoder::new(&packet).expect("valid range decoder");
        for _ in 0..15 {
            dec.decode_raw_bits(8).expect("advance bits");
        }

        let cap = init_caps(3, 2);
        let offsets = [0i32; MAX_BANDS];
        let total = ((32 * 8) << BITRES) - (dec.tell_frac() as i32) - 1;
        let alloc = clt_compute_allocation(&mut dec, 17, 21, &offsets, &cap, 6, total, 2, 3)
            .expect("hybrid allocation failed");

        assert_eq!(alloc.coded_bands, 19);
        assert_eq!(alloc.balance, 0);
        assert_eq!(alloc.intensity, 18);
        assert!(alloc.dual_stereo);
        assert_eq!(dec.tell_frac(), 1005);

        const EXPECTED_PULSES: [i32; 21] = [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 522, 408, 0, 0,
        ];
        const EXPECTED_FINE_QUANT: [i32; 21] = [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 1, 1,
        ];
        const EXPECTED_FINE_PRIO: [i32; 21] = [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0,
        ];

        assert_eq!(alloc.pulses, EXPECTED_PULSES);
        assert_eq!(alloc.fine_quant, EXPECTED_FINE_QUANT);
        assert_eq!(alloc.fine_priority, EXPECTED_FINE_PRIO);

        return;
    }

    /// Verifies high-bitrate Stereo (`C = 2`), `LM = 3` (20 ms), 160-byte frame allocation
    /// against the `libopus 1.6.1` C oracle.
    #[test]
    fn test_clt_compute_allocation_high_bitrate_oracle() {
        let mut packet = [0u8; 160];
        for (i, byte) in packet.iter_mut().enumerate() {
            *byte = ((i * 67 + 13) & 0xFF) as u8;
        }
        let mut dec = Decoder::new(&packet).expect("valid range decoder");

        let cap = init_caps(3, 2);
        let mut offsets = [0i32; MAX_BANDS];
        for slot in offsets.iter_mut().take(4) {
            *slot = 24;
        }

        let total = ((160 * 8) << BITRES) - (dec.tell_frac() as i32) - 1;
        let alloc = clt_compute_allocation(&mut dec, 0, 21, &offsets, &cap, 5, total, 2, 3)
            .expect("high-bitrate allocation failed");

        assert_eq!(alloc.coded_bands, 16);
        assert_eq!(alloc.balance, 0);
        assert_eq!(alloc.intensity, 11);
        assert!(!alloc.dual_stereo);
        assert_eq!(dec.tell_frac(), 89);

        const EXPECTED_PULSES: [i32; 21] = [
            485, 463, 441, 423, 361, 343, 341, 323, 618, 581, 545, 509, 994, 906, 834, 1167, 0, 0,
            0, 0, 0,
        ];
        const EXPECTED_FINE_QUANT: [i32; 21] = [
            3, 3, 3, 3, 3, 3, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 1, 1, 1, 1, 1,
        ];
        const EXPECTED_FINE_PRIO: [i32; 21] = [
            0, 0, 1, 1, 1, 1, 0, 0, 1, 1, 1, 1, 0, 1, 1, 0, 0, 0, 0, 0, 0,
        ];

        assert_eq!(alloc.pulses, EXPECTED_PULSES);
        assert_eq!(alloc.fine_quant, EXPECTED_FINE_QUANT);
        assert_eq!(alloc.fine_priority, EXPECTED_FINE_PRIO);

        return;
    }

    /// Verifies `decode_frame_allocation` (`decode_band_boosts_and_trim` + `anti_collapse_rsv` +
    /// `clt_compute_allocation`) on a transient (`is_transient = true, lm = 2`) frame with
    /// active dynamic boosts (`offsets[0] = 32, offsets[3] = 64`) and `alloc_trim = 7`
    /// against the `libopus 1.6.1` C oracle.
    #[test]
    fn test_decode_frame_allocation_transient_dynalloc_oracle() {
        const PACKET5: [u8; 48] = [
            0xfd, 0xda, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x0a, 0xaa, 0xaa,
        ];
        let mut dec = Decoder::new(&PACKET5).expect("valid range decoder");
        let (alloc, anti_collapse_rsv) =
            decode_frame_allocation(&mut dec, 0, 21, 2, 1, true, 48 * 8)
                .expect("transient dynalloc frame allocation failed");

        assert_eq!(anti_collapse_rsv, 8);
        assert_eq!(alloc.coded_bands, 16);
        assert_eq!(alloc.balance, 0);
        assert_eq!(dec.tell_frac(), 203);

        const EXPECTED_PULSES: [i32; 21] = [
            150, 119, 111, 161, 102, 96, 90, 84, 163, 159, 147, 135, 255, 234, 209, 285, 0, 0, 0,
            0, 0,
        ];
        const EXPECTED_FINE_QUANT: [i32; 21] = [
            4, 3, 3, 4, 2, 2, 2, 2, 3, 2, 2, 2, 3, 2, 2, 2, 1, 1, 1, 1, 1,
        ];
        const EXPECTED_FINE_PRIO: [i32; 21] = [
            0, 0, 1, 0, 0, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0,
        ];

        assert_eq!(alloc.pulses, EXPECTED_PULSES);
        assert_eq!(alloc.fine_quant, EXPECTED_FINE_QUANT);
        assert_eq!(alloc.fine_priority, EXPECTED_FINE_PRIO);

        return;
    }
}
