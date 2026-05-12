// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// WavPack v2–v3 lossless sample unpacker, ported from unpack3.c by Conifer Software (BSD licence).

use super::bits::Bits;
use super::words::{WordState, WORD_EOF, get_word1, get_old_word1, get_word2, get_word3};

// ---------------------------------------------------------------------------
// Flag constants (match WavpackHeader3 flags field)
// ---------------------------------------------------------------------------

pub const MONO_FLAG:       i16 = 0x0001;
pub const FAST_FLAG:       i16 = 0x0002;
pub const HIGH_FLAG:       i16 = 0x0010;
pub const BYTES_3:         i16 = 0x0020;
pub const OVER_20:         i16 = 0x0040;
pub const NEW_HIGH_FLAG:   i16 = 0x0400;
pub const CROSS_DECORR:    i16 = 0x1000;
pub const NEW_DECORR_FLAG: i16 = 0x2000;
pub const JOINT_STEREO:    i16 = 0x4000;
pub const EXTREME_DECORR:  i16 = 0x8000u16 as i16;

// ---------------------------------------------------------------------------
// Decorrelation tables (reversed for decoder order)
// ---------------------------------------------------------------------------

static EXTREME_TERMS: &[i8] = &[1,1,1,2,4,-1,1,2,3,6,-2,8,5,7,4,1,2,3];
static DEFAULT_TERMS: &[i8] = &[1,1,1,-1,2,1,-2];
static SIMPLE_TERMS:  &[i8] = &[1,1,1,1];

// ---------------------------------------------------------------------------
// Decorr pass state
// ---------------------------------------------------------------------------

pub const MAX_NTERMS3: usize = 18;
pub const MAX_TERM:    usize = 8;

#[derive(Default, Clone)]
pub struct DecorrPass {
    pub term:      i32,
    pub weight_a:  i32,
    pub weight_b:  i32,
    pub samples_a: [i32; MAX_TERM],
    pub samples_b: [i32; MAX_TERM],
}

// ---------------------------------------------------------------------------
// Per-block running state (persists across packets in multi-block files)
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
pub struct DcState {
    pub sample:           [[i32; 2]; 2], // [2 channels][0=pos, 1=delta]
    pub weight:           [[i32; 1]; 2], // [2 channels][0=weight]
    pub m:                usize,         // ring-buffer modulo index
    pub crc:              i32,
    pub sum_level:        i32,
    pub left_level:       i32,
    pub right_level:      i32,
    pub diff_level:       i32,
    pub last_extra_bits:  i32,
    pub extra_bits_count: i32,
}

// ---------------------------------------------------------------------------
// Helpers mirroring the C macros
// ---------------------------------------------------------------------------

#[inline(always)]
fn apply_weight_n8(weight: i32, sample: i32) -> i32 {
    (weight as i64 * sample as i64 + 128).wrapping_shr(8) as i32
}

#[inline(always)]
fn apply_weight_n9(weight: i32, sample: i32) -> i32 {
    (weight as i64 * sample as i64 + 256).wrapping_shr(9) as i32
}

#[inline(always)]
fn update_weight_n8(weight: &mut i32, source: i32, result: i32, min_weight: i32) {
    if source != 0 && result != 0 {
        if (source ^ result) >= 0 {
            if *weight < 256 { *weight += 1; }
        } else {
            if *weight > min_weight { *weight -= 1; }
        }
    }
}

#[inline(always)]
fn update_weight_n9(weight: &mut i32, source: i32, result: i32, min_weight: i32) {
    if source != 0 && result != 0 {
        if (source ^ result) >= 0 {
            if *weight < 512 { *weight += 1; }
        } else {
            if *weight > min_weight { *weight -= 1; }
        }
    }
}

// ---------------------------------------------------------------------------
// unpack_init3: set up decorr passes (called once per stream)
// ---------------------------------------------------------------------------

pub fn unpack_init3(flags: i16, passes: &mut Vec<DecorrPass>, num_terms: &mut usize) {
    passes.clear();

    let table: &[i8] = if (flags & EXTREME_DECORR) != 0 {
        EXTREME_TERMS
    } else if (flags & NEW_DECORR_FLAG) != 0 {
        DEFAULT_TERMS
    } else {
        SIMPLE_TERMS
    };

    // Terms are applied in reversed table order.
    for &t in table.iter().rev() {
        if t > 0 || (flags & CROSS_DECORR) != 0 {
            let mut p = DecorrPass::default();
            p.term = t as i32;
            passes.push(p);
        }
    }

    *num_terms = passes.len();
}

// ---------------------------------------------------------------------------
// Main decode function
// Returns a flat interleaved Vec<i32>: [L0, R0, L1, R1, ...] for stereo,
// or [S0, S1, ...] for mono.  Caller separates into channels.
// ---------------------------------------------------------------------------

pub fn unpack_samples_v3(
    version:      i16,
    wphdr_bits:   i16,
    flags:        i16,
    shift:        i16,
    sample_count: u32,
    num_channels: u32,
    audio_bytes:  &[u8],
    dc:           &mut DcState,
    passes:       &mut Vec<DecorrPass>,
    num_terms:    usize,
    ws:           &mut WordState,
) -> Vec<i32> {
    let shift = shift as i32;
    let is_mono   = (flags & MONO_FLAG)   != 0;
    let is_fast   = (flags & FAST_FLAG)   != 0;
    let is_high   = (flags & HIGH_FLAG)   != 0;
    let is_bytes3 = (flags & BYTES_3)     != 0;
    let is_over20 = (flags & OVER_20)     != 0;
    let is_newhigh = (flags & NEW_HIGH_FLAG) != 0;
    let is_extreme = (flags & EXTREME_DECORR) != 0;

    if is_over20 {
        // Not needed for GrandOrgue's standard 16/24-bit files; guard against it.
        return Vec::new();
    }

    let min_weight: i32 = if wphdr_bits != 0 {
        if (flags & (NEW_DECORR_FLAG | EXTREME_DECORR)) != 0 { -256 } else { 0 }
    } else {
        if (flags & NEW_DECORR_FLAG) != 0 {
            if is_extreme { -512 } else { -256 }
        } else {
            0
        }
    };

    let (min_value, max_value): (i32, i32) = if is_bytes3 {
        (-8_388_608 >> shift, 8_388_607 >> shift)
    } else {
        (-32768 >> shift, 32767 >> shift)
    };

    let mut bits = Bits::new(audio_bytes);

    // Local copies of dc state (mirroring the C local variables)
    let mut sample = dc.sample;
    let mut weight = dc.weight;
    let mut m      = dc.m;
    let mut crc    = dc.crc;
    let mut sum_level   = dc.sum_level;
    let mut left_level  = dc.left_level;
    let mut right_level = dc.right_level;
    let mut diff_level  = dc.diff_level;

    let per_frame = if is_mono { 1 } else { 2 };
    let mut out = Vec::with_capacity(sample_count as usize * per_frame);
    let mut decoded = 0u32;

    // -----------------------------------------------------------------------
    // Version 3 lossless mono
    // -----------------------------------------------------------------------
    if version == 3 && wphdr_bits == 0 && is_mono {
        if is_fast {
            while decoded < sample_count {
                let rw = get_word3(ws, &mut bits, 0, 0);
                if rw == WORD_EOF { break; }
                sample[0][1] = sample[0][1].wrapping_add(rw);
                sample[0][0] = sample[0][0].wrapping_add(sample[0][1]);
                crc = crc.wrapping_mul(3).wrapping_add(sample[0][0]);
                out.push(sample[0][0] << shift);
                decoded += 1;
            }
        } else if is_high {
            while decoded < sample_count {
                let rw = if is_newhigh {
                    get_word1(ws, &mut bits, 0, flags)
                } else {
                    get_old_word1(ws, &mut bits, 0)
                };
                if rw == WORD_EOF { break; }

                let mut val = rw;
                let weight_bits = if is_extreme { 9 } else { 8 };
                if weight_bits == 9 {
                    for i in 0..num_terms {
                        let p = &mut passes[i];
                        let sam = p.samples_a[m];
                        let temp = apply_weight_n9(p.weight_a, sam) + val;
                        update_weight_n9(&mut p.weight_a, sam, val, min_weight);
                        p.samples_a[(m + p.term as usize) & (MAX_TERM - 1)] = temp;
                        val = temp;
                    }
                } else {
                    for i in 0..num_terms {
                        let p = &mut passes[i];
                        let sam = p.samples_a[m];
                        let temp = apply_weight_n8(p.weight_a, sam) + val;
                        update_weight_n8(&mut p.weight_a, sam, val, min_weight);
                        p.samples_a[(m + p.term as usize) & (MAX_TERM - 1)] = temp;
                        val = temp;
                    }
                }
                m = (m + 1) & (MAX_TERM - 1);
                crc = crc.wrapping_mul(3).wrapping_add(val);
                out.push(val << shift);
                decoded += 1;
            }
        } else {
            // Default: get_word3 + one-pass adaptive predictor from dc.weight
            while decoded < sample_count {
                let rw = get_word3(ws, &mut bits, 0, 0);
                if rw == WORD_EOF { break; }

                let temp = sample[0][0]
                    + apply_weight_n8(weight[0][0], sample[0][1])
                    + rw;

                if (sample[0][1] >= 0) == (rw > 0) {
                    if weight[0][0] < 256 { weight[0][0] += 1; }
                } else {
                    if weight[0][0] > 0 { weight[0][0] -= 1; }
                }

                sample[0][0] = sample[0][0].wrapping_add({
                    sample[0][1] = temp.wrapping_sub(sample[0][0]);
                    sample[0][1]
                });

                crc = crc.wrapping_mul(3).wrapping_add(sample[0][0]);
                out.push(sample[0][0] << shift);
                decoded += 1;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Version 3 lossless stereo
    // -----------------------------------------------------------------------
    else if version == 3 && wphdr_bits == 0 && !is_mono {
        if is_fast {
            while decoded < sample_count {
                let rw0 = get_word3(ws, &mut bits, 0, 0);
                if rw0 == WORD_EOF { break; }
                let rw1 = get_word3(ws, &mut bits, 1, 0);
                if rw1 == WORD_EOF { break; }

                // Interleaved sum+diff encoding
                let sum  = (rw0 << 1) | (rw1 & 1);
                let diff = rw1;
                let dl = (sum + diff) >> 1;
                let dr = (sum - diff) >> 1;

                sample[0][1] = sample[0][1].wrapping_add(dl);
                sample[1][1] = sample[1][1].wrapping_add(dr);
                sample[0][0] = sample[0][0].wrapping_add(sample[0][1]);
                sample[1][0] = sample[1][0].wrapping_add(sample[1][1]);

                crc = crc.wrapping_mul(3).wrapping_add(sample[0][0]);
                crc = crc.wrapping_mul(3).wrapping_add(sample[1][0]);
                out.push(sample[0][0] << shift);
                out.push(sample[1][0] << shift);
                decoded += 1;
            }
        } else if is_high {
            while decoded < sample_count {
                let (left, right) = if (flags & CROSS_DECORR) != 0 {
                    let l = get_word1(ws, &mut bits, 0, flags);
                    if l == WORD_EOF { break; }
                    let r = get_word1(ws, &mut bits, 1, flags);
                    if r == WORD_EOF { break; }
                    (l, r)
                } else if is_newhigh {
                    let rw0 = get_word1(ws, &mut bits, 0, flags);
                    if rw0 == WORD_EOF { break; }
                    let rw1 = get_word1(ws, &mut bits, 1, flags);
                    if rw1 == WORD_EOF { break; }
                    // Adaptive sum/diff channel selection
                    let (left, right) = stereo_adapt_newhigh(
                        rw0, rw1,
                        &mut sum_level, &mut left_level, &mut right_level, &mut diff_level,
                        flags,
                    );
                    (left, right)
                } else {
                    let rw0 = get_old_word1(ws, &mut bits, 0);
                    if rw0 == WORD_EOF { break; }
                    let rw1 = get_old_word1(ws, &mut bits, 1);
                    if rw1 == WORD_EOF { break; }
                    let (left, right, sum, diff) = stereo_adapt_oldhigh(rw0, rw1, sum_level, left_level, right_level);
                    sum_level   = sum_level  .wrapping_sub(sum_level   >> 8).wrapping_add((sum >> 1).abs());
                    left_level  = left_level .wrapping_sub(left_level  >> 8).wrapping_add(left.abs());
                    right_level = right_level.wrapping_sub(right_level >> 8).wrapping_add(right.abs());
                    diff_level  = diff_level .wrapping_sub(diff_level  >> 8).wrapping_add(diff.abs());
                    (if (flags & JOINT_STEREO) != 0 { diff } else { left },
                     if (flags & JOINT_STEREO) != 0 { sum >> 1 } else { right })
                };

                // Apply decorr passes
                let (mut lv, mut rv) = (left, right);
                if is_extreme {
                    for i in 0..num_terms {
                        let p = &mut passes[i];
                        if p.term > 0 {
                            let sam_a = p.samples_a[m];
                            let sam_b = p.samples_b[m];
                            let k = (m + p.term as usize) & (MAX_TERM - 1);
                            let l2 = apply_weight_n9(p.weight_a, sam_a) + lv;
                            let r2 = apply_weight_n9(p.weight_b, sam_b) + rv;
                            update_weight_n9(&mut p.weight_a, sam_a, lv, min_weight);
                            update_weight_n9(&mut p.weight_b, sam_b, rv, min_weight);
                            p.samples_a[k] = l2; lv = l2;
                            p.samples_b[k] = r2; rv = r2;
                        } else if p.term == -1 {
                            let l2 = lv + apply_weight_n9(p.weight_a, p.samples_a[0]);
                            update_weight_n9(&mut p.weight_a, p.samples_a[0], lv, min_weight);
                            lv = l2;
                            let r2 = rv + apply_weight_n9(p.weight_b, lv);
                            update_weight_n9(&mut p.weight_b, lv, rv, min_weight);
                            p.samples_a[0] = rv; rv = r2;
                        } else {
                            let r2 = rv + apply_weight_n9(p.weight_a, p.samples_a[0]);
                            update_weight_n9(&mut p.weight_a, p.samples_a[0], rv, min_weight);
                            rv = r2;
                            let l2 = lv + apply_weight_n9(p.weight_b, rv);
                            update_weight_n9(&mut p.weight_b, rv, lv, min_weight);
                            p.samples_a[0] = lv; lv = l2;
                        }
                    }
                } else {
                    for i in 0..num_terms {
                        let p = &mut passes[i];
                        if p.term > 0 {
                            let sam_a = p.samples_a[m];
                            let sam_b = p.samples_b[m];
                            let k = (m + p.term as usize) & (MAX_TERM - 1);
                            let l2 = apply_weight_n8(p.weight_a, sam_a) + lv;
                            let r2 = apply_weight_n8(p.weight_b, sam_b) + rv;
                            update_weight_n8(&mut p.weight_a, sam_a, lv, min_weight);
                            update_weight_n8(&mut p.weight_b, sam_b, rv, min_weight);
                            p.samples_a[k] = l2; lv = l2;
                            p.samples_b[k] = r2; rv = r2;
                        } else if p.term == -1 {
                            let l2 = lv + apply_weight_n8(p.weight_a, p.samples_a[0]);
                            update_weight_n8(&mut p.weight_a, p.samples_a[0], lv, min_weight);
                            lv = l2;
                            let r2 = rv + apply_weight_n8(p.weight_b, lv);
                            update_weight_n8(&mut p.weight_b, lv, rv, min_weight);
                            p.samples_a[0] = rv; rv = r2;
                        } else {
                            let r2 = rv + apply_weight_n8(p.weight_a, p.samples_a[0]);
                            update_weight_n8(&mut p.weight_a, p.samples_a[0], rv, min_weight);
                            rv = r2;
                            let l2 = lv + apply_weight_n8(p.weight_b, rv);
                            update_weight_n8(&mut p.weight_b, rv, lv, min_weight);
                            p.samples_a[0] = lv; lv = l2;
                        }
                    }
                }

                m = (m + 1) & (MAX_TERM - 1);

                // JOINT_STEREO undo (for non-CROSS_DECORR HIGH_FLAG path)
                if (flags & CROSS_DECORR) == 0 && (flags & JOINT_STEREO) != 0 {
                    let sum = (rv << 1) | (lv & 1);
                    rv = (sum - lv) >> 1;
                    lv = (sum + lv) >> 1;
                }

                crc = crc.wrapping_mul(9)
                    .wrapping_add(lv.wrapping_mul(3))
                    .wrapping_add(rv);
                out.push(lv << shift);
                out.push(rv << shift);
                decoded += 1;
            }

            dc.sum_level   = sum_level;
            dc.left_level  = left_level;
            dc.right_level = right_level;
            dc.diff_level  = diff_level;
        } else {
            // Default stereo: get_word3 + adaptive sum/diff + predictor
            while decoded < sample_count {
                let rw0 = get_word3(ws, &mut bits, 0, 0);
                if rw0 == WORD_EOF { break; }
                let rw1 = get_word3(ws, &mut bits, 1, 0);
                if rw1 == WORD_EOF { break; }

                let (left, right) = stereo_default_decode(
                    rw0, rw1,
                    &mut sum_level, &mut left_level, &mut right_level,
                );

                let left2  = sample[0][0] + apply_weight_n8(weight[0][0], sample[0][1]) + left;
                let right2 = sample[1][0] + apply_weight_n8(weight[1][0], sample[1][1]) + right;

                if (sample[0][1] >= 0) == (left  > 0) {
                    if weight[0][0] < 256 { weight[0][0] += 1; }
                } else {
                    if weight[0][0] > 0 { weight[0][0] -= 1; }
                }
                if (sample[1][1] >= 0) == (right > 0) {
                    if weight[1][0] < 256 { weight[1][0] += 1; }
                } else {
                    if weight[1][0] > 0 { weight[1][0] -= 1; }
                }

                sample[0][0] = sample[0][0].wrapping_add({
                    sample[0][1] = left2.wrapping_sub(sample[0][0]);
                    sample[0][1]
                });
                sample[1][0] = sample[1][0].wrapping_add({
                    sample[1][1] = right2.wrapping_sub(sample[1][0]);
                    sample[1][1]
                });

                crc = crc.wrapping_mul(9)
                    .wrapping_add(sample[0][0].wrapping_mul(3))
                    .wrapping_add(sample[1][0]);
                out.push(sample[0][0] << shift);
                out.push(sample[1][0] << shift);
                decoded += 1;
            }

            dc.sum_level   = sum_level;
            dc.left_level  = left_level;
            dc.right_level = right_level;
        }
    }

    // -----------------------------------------------------------------------
    // Version 2 mono
    // -----------------------------------------------------------------------
    else if version == 2 && is_mono {
        while decoded < sample_count {
            let rw = get_word2(ws, &mut bits, 0, wphdr_bits);
            if rw == WORD_EOF { break; }
            sample[0][1] = sample[0][1].wrapping_add(rw);
            sample[0][0] = sample[0][0].wrapping_add(sample[0][1]);
            if wphdr_bits != 0 {
                let clamped = sample[0][0].clamp(min_value, max_value);
                out.push(clamped << shift);
            } else {
                out.push(sample[0][0] << shift);
            }
            decoded += 1;
        }
    }

    // -----------------------------------------------------------------------
    // Version 2 stereo  (also version 1)
    // -----------------------------------------------------------------------
    else if version < 3 && !is_mono {
        while decoded < sample_count {
            let rw0 = get_word2(ws, &mut bits, 0, wphdr_bits);
            if rw0 == WORD_EOF { break; }
            let rw1 = get_word2(ws, &mut bits, 1, wphdr_bits);
            if rw1 == WORD_EOF { break; }

            let sum  = (rw0 << 1) | (rw1 & 1);
            let diff = rw1;
            sample[0][1] = sample[0][1].wrapping_add((sum + diff) >> 1);
            sample[1][1] = sample[1][1].wrapping_add((sum - diff) >> 1);
            sample[0][0] = sample[0][0].wrapping_add(sample[0][1]);
            sample[1][0] = sample[1][0].wrapping_add(sample[1][1]);

            if wphdr_bits != 0 {
                out.push(sample[0][0].clamp(min_value, max_value) << shift);
                out.push(sample[1][0].clamp(min_value, max_value) << shift);
            } else {
                out.push(sample[0][0] << shift);
                out.push(sample[1][0] << shift);
            }
            decoded += 1;
        }
    }

    // Commit updated state back
    dc.sample = sample;
    dc.weight = weight;
    dc.m      = m;
    dc.crc    = crc;

    let _ = (min_value, max_value, num_channels);
    out
}

// ---------------------------------------------------------------------------
// Helpers for stereo channel mixing
// ---------------------------------------------------------------------------

fn stereo_default_decode(
    rw0: i32, rw1: i32,
    sum_level:   &mut i32,
    left_level:  &mut i32,
    right_level: &mut i32,
) -> (i32, i32) {
    let (left, right, sum) = if *sum_level <= *right_level && *sum_level <= *left_level {
        let sum = (rw1 << 1) | (rw0 & 1);
        let l = (sum + rw0) >> 1;
        let r = (sum - rw0) >> 1;
        (l, r, sum)
    } else if *left_level <= *right_level {
        let r = rw1 - rw0;
        let s = rw1 + r;
        (rw1, r, s)
    } else {
        let l = rw0 + rw1;
        let s = rw1 + l;
        (l, rw1, s)
    };

    *sum_level   = sum_level  .wrapping_sub(*sum_level   >> 8).wrapping_add((sum >> 1).abs());
    *left_level  = left_level .wrapping_sub(*left_level  >> 8).wrapping_add(left.abs());
    *right_level = right_level.wrapping_sub(*right_level >> 8).wrapping_add(right.abs());

    (left, right)
}

fn stereo_adapt_newhigh(
    rw0: i32, rw1: i32,
    sum_level:   &mut i32,
    left_level:  &mut i32,
    right_level: &mut i32,
    diff_level:  &mut i32,
    flags: i16,
) -> (i32, i32) {
    let (left, right, sum, diff) = if *right_level > *left_level {
        if *left_level + *right_level < *sum_level + *diff_level && *right_level < *diff_level {
            let s = rw0 + rw1; let d = rw0 - rw1;
            (rw1, rw0, s, d)
        } else if *sum_level < *left_level {
            let sum = (rw1 << 1) | (rw0 & 1);
            let l = (sum + rw0) >> 1;
            let r = (sum - rw0) >> 1;
            (l, r, sum, rw0)
        } else {
            let r = rw1 - rw0;
            let l = rw1;
            let s = l + r;
            (l, r, s, rw0)
        }
    } else {
        if *left_level + *right_level < *sum_level + *diff_level && *left_level < *diff_level {
            let s = rw0 + rw1; let d = rw0 - rw1;
            (rw0, rw1, s, d)
        } else if *sum_level < *right_level {
            let sum = (rw1 << 1) | (rw0 & 1);
            let l = (sum + rw0) >> 1;
            let r = (sum - rw0) >> 1;
            (l, r, sum, rw0)
        } else {
            let l = rw0 + rw1;
            let r = rw1;
            let s = l + r;
            (l, r, s, rw0)
        }
    };

    *sum_level   = sum_level  .wrapping_sub(*sum_level   >> 8).wrapping_add((sum >> 1).abs());
    *left_level  = left_level .wrapping_sub(*left_level  >> 8).wrapping_add(left.abs());
    *right_level = right_level.wrapping_sub(*right_level >> 8).wrapping_add(right.abs());
    *diff_level  = diff_level .wrapping_sub(*diff_level  >> 8).wrapping_add(diff.abs());

    if (flags & JOINT_STEREO) != 0 {
        (diff, sum >> 1)
    } else {
        (left, right)
    }
}

fn stereo_adapt_oldhigh(
    rw0: i32, rw1: i32,
    sum_level: i32, left_level: i32, right_level: i32,
) -> (i32, i32, i32, i32) {
    if sum_level <= right_level && sum_level <= left_level {
        let sum = (rw1 << 1) | (rw0 & 1);
        let l = (sum + rw0) >> 1;
        let r = (sum - rw0) >> 1;
        (l, r, sum, rw0)
    } else if left_level <= right_level {
        let l = rw1;
        let r = rw1 - rw0;
        let s = l + r;
        (l, r, s, rw0)
    } else {
        let r = rw1;
        let l = rw0 + r;
        let s = l + r;
        (l, r, s, rw0)
    }
}
