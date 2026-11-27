// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The adaptive hybrid transform of E-AC-3 (ETSI TS 102 366, clause E.2.4).

use std::f32::consts::{FRAC_1_SQRT_2, PI, SQRT_2};

use symphonia_core::errors::{Result, decode_error};

use crate::aht_tables::{
    GAQ_REMAP, VQ_HEBAP1, VQ_HEBAP2, VQ_HEBAP3, VQ_HEBAP4, VQ_HEBAP5, VQ_HEBAP6, VQ_HEBAP7,
};
use crate::bits::Bits;

/// The number of blocks the transform spans, which is all the blocks of a frame.
pub const AHT_BLOCKS: usize = 6;

/// The number of bits of a vector quantizer index for the hebaps 1 to 7.
const VQ_BITS: [u32; 8] = [0, 2, 3, 4, 5, 7, 8, 9];

/// The number of mantissa bits of the hebaps 8 to 19.
const SCALAR_BITS: [u32; 12] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 16];

fn vq_vector(hebap: u8, index: usize) -> [i16; 6] {
    match hebap {
        1 => VQ_HEBAP1[index],
        2 => VQ_HEBAP2[index],
        3 => VQ_HEBAP3[index],
        4 => VQ_HEBAP4[index],
        5 => VQ_HEBAP5[index],
        6 => VQ_HEBAP6[index],
        _ => VQ_HEBAP7[index],
    }
}

fn sign_extend(raw: u32, bits: u32) -> i32 {
    ((raw << (32 - bits)) as i32) >> (32 - bits)
}

/// Remaps a large mantissa (or any mantissa of the symmetric quantizer) to its reconstruction
/// point: `y = x + a * x + b`, where `gain_index` is 0, 1, or 2 for a gain of 1, 2, or 4.
fn remap(hebap: u8, gain_index: usize, x: f32) -> f32 {
    let [a, b_pos, b_neg] = GAQ_REMAP[usize::from(hebap) - 8][gain_index];
    let b = if x >= 0.0 { b_pos } else { b_neg };
    x + x * f32::from(a) / 32768.0 + f32::from(b) / 32768.0
}

/// Reads the 6 mantissas of a bin of the scalar quantizer, with an optional gain of 1, 2, or 4.
fn read_scalar_mantissas(
    bs: &mut Bits<'_>,
    hebap: u8,
    gain: u32,
    out: &mut [f32; 6],
) -> Result<()> {
    let m = SCALAR_BITS[usize::from(hebap) - 8];
    let full = (1u32 << (m - 1)) as f32;

    for value in out.iter_mut() {
        *value = match gain {
            2 | 4 => {
                // A tag, which is the most negative small mantissa, announces a large mantissa.
                let small_bits = if gain == 2 { m - 1 } else { m - 2 };
                let small = sign_extend(bs.u(small_bits)?, small_bits);
                if small != -(1 << (small_bits - 1)) {
                    small as f32 / full
                }
                else if gain == 2 {
                    let large = sign_extend(bs.u(m - 1)?, m - 1);
                    remap(hebap, 1, large as f32 / (1u32 << (m - 2)) as f32)
                }
                else {
                    let large = sign_extend(bs.u(m)?, m);
                    remap(hebap, 2, large as f32 / full)
                }
            }
            _ => remap(hebap, 0, sign_extend(bs.u(m)?, m) as f32 / full),
        };
    }
    Ok(())
}

/// The inverse DCT-II of the 6 transform coefficients of a bin.
///
/// The specification prints a leading factor of 2 for this transform, but the deployed decoders
/// use the square root of 2, which gives a constant signal a unit DC gain.
fn inverse_dct(x: &[f32; 6]) -> [f32; 6] {
    let mut c = [0f32; 6];
    for (m, out) in c.iter_mut().enumerate() {
        let mut acc = 0.0;
        for (j, &xj) in x.iter().enumerate() {
            let r = if j == 0 { FRAC_1_SQRT_2 } else { 1.0 };
            acc += r * xj * (j as f32 * (2 * m + 1) as f32 * PI / 12.0).cos();
        }
        *out = SQRT_2 * acc;
    }
    c
}

/// Reads the AHT mantissas of a channel for all six blocks, and returns, for each block, the
/// coefficients scaled by the exponents. `scale` converts an exponent to a coefficient scale, and
/// `dither` generates the noise that replaces bins without mantissas.
pub fn read_channel(
    bs: &mut Bits<'_>,
    hebap: &[u8],
    exps: &[u8],
    range: std::ops::Range<usize>,
    scale: impl Fn(u8) -> f32,
    mut dither: Option<&mut dyn FnMut() -> f32>,
    out: &mut [[f32; 256]; AHT_BLOCKS],
) -> Result<()> {
    let gaqmod = bs.u(2)?;
    let endbap = if gaqmod < 2 { 12 } else { 17 };

    let gaq_active = |h: u8| h > 7 && h < endbap;
    let num_active = range.clone().filter(|&bin| gaq_active(hebap[bin])).count();

    // The gains of the bins that use gain adaptive quantization, as 0 (a gain of 1) to 2.
    let mut gains = Vec::with_capacity(num_active + 2);
    match gaqmod {
        1 | 2 => {
            for _ in 0..num_active {
                gains.push(bs.u(1)? as u8);
            }
        }
        3 => {
            for _ in 0..num_active.div_ceil(3) {
                let group = bs.u(5)? as u8;
                if group >= 27 {
                    return decode_error("ac3: invalid gain group");
                }
                gains.extend([group / 9, (group % 9) / 3, group % 3]);
            }
        }
        _ => (),
    }
    let mut gains = gains.into_iter();

    for bin in range {
        let h = hebap[bin];
        let mut x = [0f32; 6];

        if h >= 8 {
            let gain = if gaq_active(h) && gaqmod != 0 {
                match (gaqmod, gains.next().unwrap_or(0)) {
                    (_, 0) => 1,
                    (1, _) => 2,
                    (2, _) => 4,
                    (_, 1) => 2,
                    _ => 4,
                }
            }
            else {
                1
            };
            read_scalar_mantissas(bs, h, gain, &mut x)?;
        }
        else if h > 0 {
            let vector = vq_vector(h, bs.u(VQ_BITS[usize::from(h)])? as usize);
            for (v, &raw) in x.iter_mut().zip(vector.iter()) {
                *v = f32::from(raw) / 32768.0;
            }
        }
        else if let Some(dither) = dither.as_mut() {
            x.fill_with(dither);
        }

        let s = scale(exps[bin]);
        let c = inverse_dct(&x);
        for (block, &v) in out.iter_mut().zip(c.iter()) {
            block[bin] = v * s;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_inverse_dct_has_unit_dc_gain() {
        let c = inverse_dct(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert!(c.iter().all(|v| (v - 1.0).abs() < 1e-6));
    }

    #[test]
    fn verify_symmetric_quantizer_remapping() {
        // The 3-bit quantizer of hebap 8 has a step of 2/7, so the code 3 reconstructs to 6/7.
        assert!((remap(8, 0, 0.75) - 6.0 / 7.0).abs() < 1e-3);
        assert!((remap(8, 0, -0.75) + 6.0 / 7.0).abs() < 1e-3);
    }

    #[test]
    fn verify_vector_tables_have_the_specified_sizes() {
        for (hebap, len) in [(1, 4), (2, 8), (3, 16), (4, 32), (5, 128), (6, 256), (7, 512)] {
            assert_eq!(1 << VQ_BITS[hebap], len);
            let _ = vq_vector(hebap as u8, len - 1);
        }
    }
}
