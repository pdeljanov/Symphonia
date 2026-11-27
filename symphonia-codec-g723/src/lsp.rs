// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The line spectral pair decoding and conversion of ITU-T Recommendation G.723.1.

use crate::tables::{LSP_BAND0_Q13, LSP_BAND1_Q13, LSP_BAND2_Q13, LSP_COSINE_Q14, LSP_DC_Q15};

pub const ORDER: usize = 10;

/// The minimum distance of two lines, 31.25 Hz.
pub const MIN_GAP: i32 = 256;
/// The minimum distance of two lines of a concealed frame, 62.5 Hz.
pub const MIN_GAP_CONCEALED: i32 = 512;

const PREDICTOR: i32 = 12288;
const PREDICTOR_CONCEALED: i32 = 23552;
/// The first line and the number of lines of the three bands of the split quantizer.
const BANDS: [(usize, usize); 3] = [(0, 3), (3, 3), (6, 4)];

pub type Lsp = [i32; ORDER];
pub type Lpc = [i32; ORDER];

pub fn saturate(v: i32) -> i32 {
    v.clamp(-32768, 32767)
}

/// A Q15 multiplication.
pub fn mult(a: i32, b: i32) -> i32 {
    saturate((a * b) >> 15)
}

pub fn initial() -> Lsp {
    LSP_DC_Q15.map(i32::from)
}

fn predict(prev: &Lsp, line: usize, predictor: i32) -> i32 {
    let dc = i32::from(LSP_DC_Q15[line]);
    saturate(dc + mult(saturate(prev[line] - dc), predictor))
}

/// Decodes the lines from the index of the split quantizer and the previous lines.
pub fn decode(index: u32, prev: &Lsp) -> Lsp {
    let indices = [(index >> 16) & 255, (index >> 8) & 255, index & 255];
    let books: [&[i16]; 3] = [&LSP_BAND0_Q13, &LSP_BAND1_Q13, &LSP_BAND2_Q13];

    let mut out = [0; ORDER];
    for (band, &(first, len)) in BANDS.iter().enumerate() {
        for j in 0..len {
            let residual = i32::from(books[band][indices[band] as usize * len + j]);
            out[first + j] = saturate(predict(prev, first + j, PREDICTOR) + residual);
        }
    }
    out
}

/// Extrapolates the lines of a lost frame.
pub fn conceal(prev: &Lsp) -> Lsp {
    std::array::from_fn(|i| predict(prev, i, PREDICTOR_CONCEALED))
}

/// Spreads the lines that are closer than `min_gap`, and returns whether they are ordered after
/// at most ten passes.
pub fn stabilize(lines: &mut Lsp, min_gap: i32) -> bool {
    let half = min_gap / 2;
    for _ in 0..10 {
        let mut changed = false;
        for j in 0..ORDER - 1 {
            if lines[j + 1] - lines[j] < min_gap {
                let avg = (lines[j] + lines[j + 1]) >> 1;
                lines[j] = saturate(avg - half);
                lines[j + 1] = saturate(avg + half);
                changed = true;
            }
        }
        if !changed {
            return true;
        }
    }
    lines.windows(2).all(|w| w[1] - w[0] >= min_gap)
}

/// Interpolates the lines of a subframe between those of the previous and current frame.
pub fn interpolate(subframe: usize, prev: &Lsp, cur: &Lsp) -> Lsp {
    let (wp, wc) = match subframe {
        0 => (24576, 8192),
        1 => (16384, 16384),
        2 => (8192, 24576),
        _ => return *cur,
    };
    std::array::from_fn(|i| saturate((prev[i] * wp + cur[i] * wc + 0x4000) >> 15))
}

fn cosine(freq: i32) -> i64 {
    let freq = freq.max(0) as usize;
    let (i, frac) = (freq >> 7, (freq & 127) as i32);
    let base = i32::from(LSP_COSINE_Q14[i]);
    let next = i32::from(LSP_COSINE_Q14[i + 1]);
    i64::from(saturate(base + mult(saturate(next - base), frac << 8)))
}

/// Converts the lines to the Q13 coefficients of the prediction filter.
pub fn to_lpc(lines: &Lsp) -> Lpc {
    const ONE: i64 = 1 << 24;

    let mut p = [0i64; ORDER + 1];
    let mut q = [0i64; ORDER + 1];
    p[0] = ONE;
    q[0] = ONE;

    let mut degree = 0;
    for k in 0..ORDER / 2 {
        let c_even = cosine(lines[2 * k]);
        let c_odd = cosine(lines[2 * k + 1]);
        degree += 2;
        for i in (2..=degree).rev() {
            p[i] += -((p[i - 1] * c_even) >> 13) + p[i - 2];
            q[i] += -((q[i - 1] * c_odd) >> 13) + q[i - 2];
        }
        p[1] -= (p[0] * c_even) >> 13;
        q[1] -= (q[0] * c_odd) >> 13;
    }

    std::array::from_fn(|j| {
        let sum = p[j + 1] + p[j] + q[j + 1] - q[j];
        saturate(((-(sum >> 1) + (1 << 10)) >> 11).clamp(-32768, 32767) as i32)
    })
}
