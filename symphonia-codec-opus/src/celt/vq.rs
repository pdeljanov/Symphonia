//! CELT Pyramid Vector Quantizer (PVQ) Shape Unquantization, Spreading Rotations,
//! Recursive Band Splitting, Pitch Folding, Stereo Unmixing, and Anti-Collapse
//! (RFC 6716 Sections 4.3.4, 4.3.5, and 4.3.6.1).
//!
//! After coarse and fine band energies are decoded and the dynamic bit allocation determines the
//! Q3 (`1/8`-bit) shape budget `pulses[band]` for each Bark band, CELT reconstructs the unit-norm
//! MDCT spectral shape in each band through four coordinated mechanisms:
//!
//! 1. **Time-Frequency Recombination ([`haar1`], [`deinterleave_hadamard`], [`interleave_hadamard`])**:
//!    Applies 1D Haar wavelet steps and Hadamard permutation across short MDCT sub-blocks to match
//!    the band's signaled time-frequency resolution `tf_res[band]`.
//! 2. **Recursive Partition Splitting & Stereo Coupling ([`quant_band`], [`quant_band_stereo`], [`quant_partition`])**:
//!    Splits high-rate partitions or coupled stereo channels $(X, Y)$ using a quantized split angle
//!    $\theta \in [0, \pi/2]$ (`imid = cos(theta)`, `iside = sin(theta)`), allocating bits between
//!    the two halves according to the log-tangent energy ratio ([`bitexact_log2tan`]).
//! 3. **Algebraic Shape Decoding & Spreading Rotations ([`alg_unquant`], [`exp_rotation`])**:
//!    Unpacks $K$-pulse integer codewords via [`cwrs::decode_pulses`](crate::celt::cwrs::decode_pulses),
//!    normalizes to the requested partition gain, and applies inverse Givens spreading rotations
//!    to mitigate tonal artifacts at low pulse densities. When $K = 0$, copies and phase-randomizes
//!    a lower-frequency spectral region (pitch folding) or synthesizes LCG noise.
//! 4. **Transient Anti-Collapse & Energy Denormalization ([`anti_collapse`], [`denormalise_bands`])**:
//!    Injects floor noise into collapsed transient sub-blocks and scales unit-norm bands by their
//!    decoded base-2 log energies to yield full-scale MDCT coefficients.
//!
//! <https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>

#![allow(clippy::excessive_precision)]
#![allow(clippy::approx_constant)]

use symphonia_core::errors::Result;

use crate::celt::constant::{
    BIT_DEINTERLEAVE_TABLE, BIT_INTERLEAVE_TABLE, BITRES, E_MEANS, EBANDS, LOGN, MAX_BANDS,
    QTHETA_OFFSET, QTHETA_OFFSET_TWOPHASE, Spread, band_width,
};
use crate::celt::cwrs::decode_pulses;
use crate::celt::error::Error;
use crate::celt::rate::{bits2pulses_at_level, get_pulses, max_cache_bits, pulses2bits_at_level};
use crate::celt::{SHORT_MDCT_SIZE, celt_lcg_rand};
use crate::entropy::Decoder;

/// Maximum number of MDCT bins in any single Bark band (band 20 at `LM = 3`: $22 \times 8 = 176$).
const MAX_BAND_WIDTH: usize = 176;

/// Maximum total size of the normalized lowband folding buffer (`8 * EBANDS[20] = 8 * 78 = 624`).
const MAX_NORM_BUFFER_SIZE: usize = 8 * EBANDS[MAX_BANDS - 1];

/// Small positive floor added to squared norms before reciprocal square root (`EPSILON` in `libopus`).
const NORM_EPSILON: f32 = 1e-15_f32;

/// Orthonormal $1/\sqrt{2}$ scaling constant for 2-point Haar butterflies (`0.70710678f` in `bands.c`).
const INV_SQRT_2: f32 = 0.70710678_f32;

/// √2 scaling factor applied to anti-collapse floor gain at `LM = 3` (`1.41421356f` in `bands.c`).
const SQRT_2: f32 = 1.41421356_f32;

/// Natural-to-ordery Hadamard permutation table for strides `2, 4, 8, 16` (`ordery_table` in `bands.c`,
/// RFC 6716 Section 4.3.4.1).
const HADAMARD_ORDERY_TABLE: [usize; 30] = [
    1, 0, // stride = 2 (offset 0)
    3, 0, 2, 1, // stride = 4 (offset 2)
    7, 0, 4, 3, 6, 1, 5, 2, // stride = 8 (offset 6)
    15, 0, 8, 7, 12, 3, 11, 4, 14, 1, 9, 6, 13, 2, 10, 5, // stride = 16 (offset 14)
];

/// Base-2 exponential lookup table for fractional Q3 bit resolution in [`compute_qn`]
/// (`exp2_table8` in `bands.c`, RFC 6716 Section 4.3.4.3).
const EXP2_TABLE8: [i32; 8] = [16384, 17866, 19483, 21247, 23170, 25267, 27554, 30048];

/// PVQ spreading rotation factor indexed by `(spread as usize) - 1` (`SPREAD_FACTOR` in `vq.c`,
/// RFC 6716 Section 4.3.4.2).
const SPREAD_FACTOR: [usize; 3] = [15, 10, 5];

/// Evaluates $\cos(\frac{\pi}{2} x)$ matching `celt_cos_norm` in `libopus` float mode.
#[inline]
fn celt_cos_norm(x: f32) -> f32 {
    return ((0.5_f64 * core::f64::consts::PI) * (x as f64)).cos() as f32;
}

/// Evaluates $2^x = e^{x \ln 2}$ matching `celt_exp2` in `libopus` float mode.
#[inline]
fn celt_exp2(x: f32) -> f32 {
    return (0.6931471805599453094_f64 * (x as f64)).exp() as f32;
}

/// Evaluates the 15th-order odd Remez polynomial approximation of $\frac{2}{\pi} \arctan(x)$
/// on $[0, 1]$ (`celt_atan_norm` in `mathops.h`).
#[inline]
fn celt_atan_norm(x: f32) -> f32 {
    const ATAN2_2_OVER_PI: f32 = 0.636619772367581_f32;
    const A03: f32 = -3.3331659436225891113281250000e-01_f32;
    const A05: f32 = 1.99627041816711425781250000000e-01_f32;
    const A07: f32 = -1.3976582884788513183593750000e-01_f32;
    const A09: f32 = 9.79423448443412780761718750000e-02_f32;
    const A11: f32 = -5.7773590087890625000000000000e-02_f32;
    const A13: f32 = 2.30401363223791122436523437500e-02_f32;
    const A15: f32 = -4.3554059229791164398193359375e-03_f32;

    let x_sq = x * x;
    let poly = A03
        + x_sq * (A05 + x_sq * (A07 + x_sq * (A09 + x_sq * (A11 + x_sq * (A13 + x_sq * A15)))));
    return ATAN2_2_OVER_PI * (x + x * x_sq * poly);
}

/// Evaluates $\frac{2}{\pi} \text{atan2}(y, x)$ for $x \ge 0, y \ge 0$ (`celt_atan2p_norm` in `mathops.h`).
#[inline]
fn celt_atan2p_norm(y: f32, x: f32) -> f32 {
    if x * x + y * y < 1e-18_f32 {
        return 0.0_f32;
    }
    if y < x {
        return celt_atan_norm(y / x);
    }
    return 1.0_f32 - celt_atan_norm(x / y);
}

/// Fixed-point Q15 fractional multiplication `((16384 + (a as i16 as i32) * (b as i16 as i32)) >> 15)`
/// (`FRAC_MUL16` in `arch.h`).
#[inline]
const fn frac_mul16(a: i32, b: i32) -> i32 {
    let a16 = (a as i16) as i32;
    let b16 = (b as i16) as i32;
    return (16384 + a16 * b16) >> 15;
}

/// Returns the 1-based position of the highest set bit of `val` (`EC_ILOG` in `entcode.h`),
/// or `0` if `val == 0`.
#[inline]
const fn ec_ilog(val: u32) -> i32 {
    return (32 - val.leading_zeros()) as i32;
}

/// Bit-exact integer square root $\lfloor \sqrt{\text{val}} \rfloor$ (`isqrt32` in `mathops.c`,
/// RFC 6716 Section 4.3.4.3).
pub const fn isqrt32(mut val: u32) -> u32 {
    if val == 0 {
        return 0;
    }
    let mut g = 0u32;
    let mut bshift = (ec_ilog(val) - 1) >> 1;
    let mut b = 1u32 << (bshift as u32);
    while bshift >= 0 {
        let t = ((g << 1) + b) << (bshift as u32);
        if t <= val {
            g += b;
            val -= t;
        }
        b >>= 1;
        bshift -= 1;
    }
    return g;
}

/// Bit-exact Q15 cosine approximation on $x \in [0, 16384]$ (`bitexact_cos` in `bands.c`,
/// RFC 6716 Section 4.3.4.3).
///
/// Bit-exactness of this polynomial is required because the resulting `imid` and `iside` values
/// feed into [`bitexact_log2tan`] to determine the mid/side bit allocation split `delta`.
pub const fn bitexact_cos(x: i16) -> i16 {
    let x32 = x as i32;
    let tmp = (4096 + x32 * x32) >> 13;
    let x2 = (32767 - tmp)
        + frac_mul16(
            tmp,
            -7651 + frac_mul16(tmp, 8277 + frac_mul16(-626, tmp)),
        );
    return (1 + x2) as i16;
}

/// Bit-exact base-2 log-tangent approximation $\log_2(\text{isin} / \text{icos})$ in Q11
/// (`bitexact_log2tan` in `bands.c`, RFC 6716 Section 4.3.4.3).
pub const fn bitexact_log2tan(isin: i32, icos: i32) -> i32 {
    let lc = ec_ilog(icos as u32);
    let ls = ec_ilog(isin as u32);
    let icos_norm = icos << (15 - lc);
    let isin_norm = isin << (15 - ls);
    return (ls - lc) * (1 << 11)
        + frac_mul16(isin_norm, frac_mul16(isin_norm, -2597) + 7932)
        - frac_mul16(icos_norm, frac_mul16(icos_norm, -2597) + 7932);
}

/// Applies a single bidirectional sweep of Givens rotations with step `stride` and rotation
/// coefficients $(c, s) = (\cos \phi, \sin \phi)$ across `x[..len]` (`exp_rotation1` in `vq.c`,
/// RFC 6716 Section 4.3.4.2).
pub fn exp_rotation1(x: &mut [f32], len: usize, stride: usize, c: f32, s: f32) {
    if len <= stride {
        return;
    }

    for i in 0..(len - stride) {
        let x1 = x[i];
        let x2 = x[i + stride];
        x[i + stride] = c * x2 + s * x1;
        x[i] = c * x1 - s * x2;
    }

    if len > 2 * stride {
        for i in (0..(len - 2 * stride)).rev() {
            let x1 = x[i];
            let x2 = x[i + stride];
            x[i + stride] = c * x2 + s * x1;
            x[i] = c * x1 - s * x2;
        }
    }

    return;
}

/// Applies the PVQ spreading rotation (`dir = 1` forward, `dir = -1` inverse for decoding)
/// across `x[..len]` (`exp_rotation` in `vq.c`, RFC 6716 Section 4.3.4.2).
///
/// When $2K < \text{len}$ and `spread != Spread::None`, rotates adjacent and `stride2`-separated
/// bins within each of the `stride` interleaved sub-blocks by angle:
/// $$\theta = \frac{1}{2} \left( \frac{\text{len}}{\text{len} + \text{factor} \cdot K} \right)^2$$
pub fn exp_rotation(
    x: &mut [f32],
    len: usize,
    dir: i32,
    stride: usize,
    k: usize,
    spread: Spread,
) {
    if 2 * k >= len || spread == Spread::None || stride == 0 {
        return;
    }

    let factor = SPREAD_FACTOR[(spread as usize) - 1];
    let gain = (len as f32) / ((len + factor * k) as f32);
    let theta = 0.5_f32 * gain * gain;

    let c = celt_cos_norm(theta);
    let s = celt_cos_norm(1.0_f32 - theta);

    let mut stride2 = 0usize;
    if len >= 8 * stride {
        stride2 = 1;
        while (stride2 * stride2 + stride2) * stride + (stride >> 2) < len {
            stride2 += 1;
        }
    }

    let sub_len = len / stride;
    for chunk in x[..len].chunks_exact_mut(sub_len) {
        if dir < 0 {
            if stride2 > 0 {
                exp_rotation1(chunk, sub_len, stride2, s, c);
            }
            exp_rotation1(chunk, sub_len, 1, c, s);
        } else {
            exp_rotation1(chunk, sub_len, 1, c, -s);
            if stride2 > 0 {
                exp_rotation1(chunk, sub_len, stride2, s, -c);
            }
        }
    }

    return;
}

/// Scales the integer pulse vector `iy[..n]` with squared $L_2$ norm `ryy` to have Euclidean
/// norm `gain` and writes the result to `x[..n]` (`normalise_residual` in `vq.c`,
/// RFC 6716 Section 4.3.4.1).
pub fn normalise_residual(iy: &[i32], x: &mut [f32], n: usize, ryy: f32, gain: f32) {
    let scale = gain / ryy.sqrt();
    for (dst, &pulse) in x[..n].iter_mut().zip(iy[..n].iter()) {
        *dst = (pulse as f32) * scale;
    }
    return;
}

/// Extracts the `b`-bit sub-block collapse mask from the decoded pulse vector `iy[..n]`,
/// setting bit $i \in [0, b)$ if and only if sub-block $i$ contains at least one non-zero pulse
/// (`extract_collapse_mask` in `vq.c`, RFC 6716 Section 4.3.4.1).
pub fn extract_collapse_mask(iy: &[i32], n: usize, b: usize) -> u32 {
    if b <= 1 {
        return 1;
    }
    let n0 = n / b;
    let mut collapse_mask = 0u32;
    for (block_idx, block_pulses) in iy[..n].chunks_exact(n0).enumerate() {
        if block_pulses.iter().any(|&p| p != 0) {
            collapse_mask |= 1u32 << block_idx;
        }
    }
    return collapse_mask;
}

/// Unquantizes an $n$-dimensional algebraic PVQ partition with $k$ pulses and $b$ time sub-blocks
/// from `dec`, scales it to Euclidean norm `gain`, applies the inverse spreading rotation, and
/// returns the $b$-bit collapse mask (`alg_unquant` in `vq.c`, RFC 6716 Sections 4.3.4.1-4.3.4.2).
pub fn alg_unquant(
    x: &mut [f32],
    n: usize,
    k: usize,
    spread: Spread,
    b: usize,
    dec: &mut Decoder<'_>,
    gain: f32,
) -> Result<u32> {
    if n == 0 || n > MAX_BAND_WIDTH || k == 0 || x.len() < n {
        return Err(Error::InvalidPvqParameters.into());
    }
    let mut iy = [0i32; MAX_BAND_WIDTH];
    let ryy = decode_pulses(&mut iy[..n], n, k, dec)?;
    normalise_residual(&iy[..n], &mut x[..n], n, ryy, gain);
    exp_rotation(&mut x[..n], n, -1, b, k, spread);
    return Ok(extract_collapse_mask(&iy[..n], n, b));
}

/// Renormalizes `x[..n]` in place so that its Euclidean norm equals `gain`
/// (`renormalise_vector` in `vq.c`, RFC 6716 Section 4.3.4.3).
pub fn renormalise_vector(x: &mut [f32], n: usize, gain: f32) {
    let mut energy = NORM_EPSILON;
    for &val in &x[..n] {
        energy += val * val;
    }
    let scale = gain / energy.sqrt();
    for val in &mut x[..n] {
        *val *= scale;
    }
    return;
}

/// Projects `x[..n]` onto the $K$-pulse $L_1$-pyramid $\sum_{i=0}^{n-1} |iy_i| = k$ by greedy
/// correlation maximization and returns the squared norm $\sum_{i=0}^{n-1} iy_i^2$
/// (`op_pvq_search_c` in `vq.c`).
pub fn op_pvq_search(x: &mut [f32], iy: &mut [i32], k: usize, n: usize) -> f32 {
    let mut y = [0.0_f32; MAX_BAND_WIDTH];
    let mut signx = [false; MAX_BAND_WIDTH];

    for j in 0..n {
        signx[j] = x[j] < 0.0;
        x[j] = x[j].abs();
        iy[j] = 0;
    }

    let mut xy = 0.0_f32;
    let mut yy = 0.0_f32;
    let mut pulses_left = k as i32;

    if k > (n >> 1) {
        let mut sum = 0.0_f32;
        for &val in &x[..n] {
            sum += val;
        }

        if !(sum > NORM_EPSILON && sum < 64.0_f32) {
            x[0] = 1.0_f32;
            x[1..n].fill(0.0_f32);
            sum = 1.0_f32;
        }

        let rcp = ((k as f32) + 0.8_f32) / sum;
        for j in 0..n {
            iy[j] = (rcp * x[j]).floor() as i32;
            y[j] = iy[j] as f32;
            yy += y[j] * y[j];
            xy += x[j] * y[j];
            y[j] *= 2.0_f32;
            pulses_left -= iy[j];
        }
    }

    if pulses_left > (n as i32) + 3 {
        let tmp = pulses_left as f32;
        yy += tmp * tmp + tmp * y[0];
        iy[0] += pulses_left;
        pulses_left = 0;
    }

    for _ in 0..pulses_left {
        yy += 1.0_f32;
        let mut best_id = 0usize;
        let rxy0 = xy + x[0];
        let mut best_num = rxy0 * rxy0;
        let mut best_den = yy + y[0];

        for j in 1..n {
            let rxy = xy + x[j];
            let ryy = yy + y[j];
            let num = rxy * rxy;
            if best_den * num > ryy * best_num {
                best_den = ryy;
                best_num = num;
                best_id = j;
            }
        }

        xy += x[best_id];
        yy += y[best_id];
        y[best_id] += 2.0_f32;
        iy[best_id] += 1;
    }

    for j in 0..n {
        if signx[j] {
            iy[j] = -iy[j];
        }
    }

    return yy;
}

/// Computes the split angle $\theta = \frac{2}{\pi} \arctan(\|\text{side}\| / \|\text{mid}\|)$
/// in Q30 fixed-point units (`stereo_itheta` in `vq.c`).
pub fn stereo_itheta(x: &[f32], y: &[f32], stereo: bool, n: usize) -> i32 {
    let mut emid = NORM_EPSILON;
    let mut eside = NORM_EPSILON;

    if stereo {
        for (&vx, &vy) in x[..n].iter().zip(y[..n].iter()) {
            let m = vx + vy;
            let s = vy - vx;
            emid += m * m;
            eside += s * s;
        }
    } else {
        for (&vx, &vy) in x[..n].iter().zip(y[..n].iter()) {
            emid += vx * vx;
            eside += vy * vy;
        }
    }

    let mid = emid.sqrt();
    let side = eside.sqrt();
    return (0.5_f32 + 65536.0_f32 * 16384.0_f32 * celt_atan2p_norm(side, mid)).floor() as i32;
}

/// Applies a single-level 1D Haar wavelet butterfly with step `stride` across `x[..n0]`
/// (`haar1` in `bands.c`, RFC 6716 Section 4.3.4.1).
pub fn haar1(x: &mut [f32], n0: usize, stride: usize) {
    let half_n0 = n0 >> 1;
    for i in 0..stride {
        for j in 0..half_n0 {
            let idx0 = stride * 2 * j + i;
            let idx1 = stride * (2 * j + 1) + i;
            let tmp1 = INV_SQRT_2 * x[idx0];
            let tmp2 = INV_SQRT_2 * x[idx1];
            x[idx0] = tmp1 + tmp2;
            x[idx1] = tmp1 - tmp2;
        }
    }
    return;
}

/// Returns the Hadamard sequency-order slice for a power-of-two `stride` $\in \{2, 4, 8, 16\}$.
#[inline]
fn hadamard_ordery(stride: usize) -> &'static [usize] {
    let start = stride - 2;
    return &HADAMARD_ORDERY_TABLE[start..(start + stride)];
}

/// De-interleaves `x[..n0 * stride]` from frequency order into time-block order, applying
/// the sequency-ordered Hadamard permutation when `hadamard` is `true`
/// (`deinterleave_hadamard` in `bands.c`, RFC 6716 Section 4.3.4.1).
pub fn deinterleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let total = n0 * stride;
    let mut tmp = [0.0_f32; MAX_BAND_WIDTH];

    if hadamard {
        let ordery = hadamard_ordery(stride);
        for (i, &ord) in ordery.iter().enumerate() {
            for j in 0..n0 {
                tmp[ord * n0 + j] = x[j * stride + i];
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                tmp[i * n0 + j] = x[j * stride + i];
            }
        }
    }

    x[..total].copy_from_slice(&tmp[..total]);
    return;
}

/// Interleaves `x[..n0 * stride]` from time-block order back into frequency order, undoing
/// the sequency-ordered Hadamard permutation when `hadamard` is `true`
/// (`interleave_hadamard` in `bands.c`, RFC 6716 Section 4.3.4.1).
pub fn interleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let total = n0 * stride;
    let mut tmp = [0.0_f32; MAX_BAND_WIDTH];

    if hadamard {
        let ordery = hadamard_ordery(stride);
        for (i, &ord) in ordery.iter().enumerate() {
            for j in 0..n0 {
                tmp[j * stride + i] = x[ord * n0 + j];
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                tmp[j * stride + i] = x[i * n0 + j];
            }
        }
    }

    x[..total].copy_from_slice(&tmp[..total]);
    return;
}

/// Computes the quantization resolution `qn` (an even integer in `2..=256`, or `1` if unsplit)
/// for the split angle $\theta$ (`compute_qn` in `bands.c`, RFC 6716 Section 4.3.4.3).
pub fn compute_qn(n: usize, b: i32, offset: i32, pulse_cap: i32, stereo: bool) -> i32 {
    let mut n2 = 2 * (n as i32) - 1;
    if stereo && n == 2 {
        n2 -= 1;
    }

    let mut qb = (b + n2 * offset) / n2;
    qb = qb.min(b - pulse_cap - (4 << BITRES));
    qb = qb.min(8 << BITRES);

    let qn = if qb < (1 << BITRES >> 1) {
        1
    } else {
        let raw = EXP2_TABLE8[(qb & 0x7) as usize] >> (14 - (qb >> BITRES));
        ((raw + 1) >> 1) << 1
    };

    return qn;
}

/// Decoded split-angle parameters for recursive mono partition or stereo mid/side splitting
/// (RFC 6716 Section 4.3.4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitAngle {
    /// Whether the side channel sign is inverted (intensity stereo with `invert_side == true`).
    pub invert_side: bool,
    /// Fixed-point Q15 cosine weight $\cos(\theta)$ for the mid / first half (`0..=32767`).
    pub imid: i32,
    /// Fixed-point Q15 sine weight $\sin(\theta)$ for the side / second half (`0..=32767`).
    pub iside: i32,
    /// Bit allocation differential between mid and side in Q3 (`1/8`-bit) units.
    pub delta: i32,
    /// Quantized split angle in Q14 (`0..=16384`, corresponding to $[0, \pi/2]$).
    pub itheta: i32,
    /// Fractional Q3 bits consumed from the range coder to decode `itheta`.
    pub qalloc: i32,
}

/// Per-band state tracked during recursive PVQ shape decoding (`bands.c`, RFC 6716 Section 4.3.4).
#[derive(Debug, Clone)]
pub struct BandDecoderCtx {
    /// Current Bark band index (`0..21`).
    pub band: usize,
    /// First band index at which stereo intensity coding begins (`0..=21`).
    pub intensity_band: usize,
    /// Spectral spreading rotation mode for the current frame.
    pub spread: Spread,
    /// Time-frequency resolution change for the current band.
    pub tf_change: i32,
    /// Remaining frame bit budget in Q3 (`1/8`-bit) units.
    pub remaining_bits: i32,
    /// Linear congruential generator state for spectral folding and noise fill.
    pub seed: u32,
}

/// Decodes a quantized split angle $\theta$ from the range coder using the step PDF (stereo $N > 2$),
/// uniform PDF ($B_0 > 1$ or stereo $N = 2$), or triangular PDF ($B_0 = 1$ mono)
/// (`compute_theta` in `bands.c`, RFC 6716 Section 4.3.4.3).
#[allow(clippy::too_many_arguments)]
fn decode_split_angle(
    ctx: &BandDecoderCtx,
    dec: &mut Decoder<'_>,
    n: usize,
    b: &mut i32,
    b_blocks: usize,
    b0: usize,
    lm: isize,
    stereo: bool,
    fill: &mut u32,
) -> Result<SplitAngle> {
    let pulse_cap = i32::from(LOGN[ctx.band]) + (lm as i32) * (1 << BITRES);
    let offset = (pulse_cap >> 1)
        - if stereo && n == 2 {
            QTHETA_OFFSET_TWOPHASE
        } else {
            QTHETA_OFFSET
        };
    let mut qn = compute_qn(n, *b, offset, pulse_cap, stereo);
    if stereo && ctx.band >= ctx.intensity_band {
        qn = 1;
    }

    let tell = dec.tell_frac() as i32;
    let mut itheta = 0i32;
    let mut invert_side = false;

    if qn != 1 {
        let qn_usize = qn as usize;
        if stereo && n > 2 {
            // Step PDF: weight p0 = 3 up to x0 = qn / 2, weight 1 above x0.
            let p0 = 3i32;
            let x0 = qn / 2;
            let ft = p0 * (x0 + 1) + x0;
            let mut icdf = [0u32; 258];
            icdf[0] = ft as u32;
            for x in 0..=qn {
                let fh = if x <= x0 {
                    p0 * (x + 1)
                } else {
                    (x - x0) + (x0 + 1) * p0
                };
                icdf[(x as usize) + 1] = fh as u32;
            }
            itheta = dec.decode_symbol_with_icdf(&icdf[..=(qn_usize + 1)])? as i32;
        } else if b0 > 1 || stereo {
            // Uniform PDF on 0..=qn.
            itheta = dec.decode_uint((qn + 1) as u32)? as i32;
        } else {
            // Symmetric triangular PDF peaking at qn / 2.
            let half_qn = qn >> 1;
            let ft = (half_qn + 1) * (half_qn + 1);
            let mut icdf = [0u32; 258];
            icdf[0] = ft as u32;
            for x in 0..=qn {
                let (fl, fs) = if x <= half_qn {
                    ((x * (x + 1)) >> 1, x + 1)
                } else {
                    let rem = qn + 1 - x;
                    (ft - ((rem * (rem + 1)) >> 1), rem)
                };
                icdf[(x as usize) + 1] = (fl + fs) as u32;
            }
            itheta = dec.decode_symbol_with_icdf(&icdf[..=(qn_usize + 1)])? as i32;
        }
        itheta = (itheta * 16384) / qn;
    } else if stereo {
        if *b > (2 << BITRES) && ctx.remaining_bits > (2 << BITRES) {
            invert_side = dec.decode_symbol_logp(2)? != 0;
        }
        itheta = 0;
    }

    let qalloc = (dec.tell_frac() as i32) - tell;
    *b -= qalloc;

    let (imid, iside, delta) = if itheta == 0 {
        *fill &= (1u32 << b_blocks) - 1;
        (32767, 0, -16384)
    } else if itheta == 16384 {
        *fill &= ((1u32 << b_blocks) - 1) << b_blocks;
        (0, 32767, 16384)
    } else {
        let im = i32::from(bitexact_cos(itheta as i16));
        let is = i32::from(bitexact_cos((16384 - itheta) as i16));
        let dl = frac_mul16(((n as i32) - 1) << 7, bitexact_log2tan(is, im));
        (im, is, dl)
    };

    return Ok(SplitAngle {
        invert_side,
        imid,
        iside,
        delta,
        itheta,
        qalloc,
    });
}

/// Decodes a single-bin ($N = 1$) mono or stereo band (`quant_band_n1` in `bands.c`,
/// RFC 6716 Section 4.3.4).
pub fn quant_band_n1(
    ctx: &mut BandDecoderCtx,
    dec: &mut Decoder<'_>,
    x: &mut [f32],
    y: Option<&mut [f32]>,
    lowband_out: Option<&mut [f32]>,
) -> Result<u32> {
    let mut decode_one = |chan: &mut [f32]| -> Result<()> {
        let mut sign = 0u32;
        if ctx.remaining_bits >= (1 << BITRES) {
            sign = dec.decode_raw_bits(1)?;
            ctx.remaining_bits -= 1 << BITRES;
        }
        chan[0] = if sign != 0 { -1.0_f32 } else { 1.0_f32 };
        return Ok(());
    };

    decode_one(x)?;
    if let Some(y_chan) = y {
        decode_one(y_chan)?;
    }

    if let Some(lb_out) = lowband_out {
        lb_out[0] = x[0];
    }

    return Ok(1);
}

/// Recursively decodes a mono partition `x[..n]` with bit allocation `b` (in Q3 units) and
/// `b_blocks` time sub-blocks (`quant_partition` in `bands.c`, RFC 6716 Section 4.3.4).
#[allow(clippy::too_many_arguments)]
pub fn quant_partition(
    ctx: &mut BandDecoderCtx,
    dec: &mut Decoder<'_>,
    x: &mut [f32],
    n: usize,
    mut b: i32,
    b_blocks: usize,
    lowband: Option<&[f32]>,
    lm: isize,
    gain: f32,
    mut fill: u32,
) -> Result<u32> {
    let b0 = b_blocks;

    // Split the partition in two if it requires > 1.5 bits beyond the pulse cache table capacity.
    if lm != -1 && b > max_cache_bits(lm, ctx.band) + 12 && n > 2 {
        let half_n = n >> 1;
        let next_lm = lm - 1;
        if b_blocks == 1 {
            fill = (fill & 1) | (fill << 1);
        }
        let half_blocks = (b_blocks + 1) >> 1;

        let split = decode_split_angle(
            ctx,
            dec,
            half_n,
            &mut b,
            half_blocks,
            b0,
            next_lm,
            false,
            &mut fill,
        )?;

        let mid = (split.imid as f32) * (1.0_f32 / 32768.0_f32);
        let side = (split.iside as f32) * (1.0_f32 / 32768.0_f32);
        let mut delta = split.delta;

        // Give extra bits to low-energy short MDCTs to mitigate pre-echo and forward masking.
        if b0 > 1 && (split.itheta & 0x3fff) != 0 {
            if split.itheta > 8192 {
                delta -= delta >> (4 - next_lm);
            } else {
                delta = 0.min(delta + (((half_n as i32) << BITRES) >> (5 - next_lm)));
            }
        }

        let mut mbits = 0.max(b.min((b - delta) / 2));
        let mut sbits = b - mbits;
        ctx.remaining_bits -= split.qalloc;

        let (x_first, x_second) = x[..n].split_at_mut(half_n);
        let (lb_first, lb_second) = match lowband {
            Some(lb) => (Some(&lb[..half_n]), Some(&lb[half_n..n])),
            None => (None, None),
        };

        let rebalance_start = ctx.remaining_bits;
        if mbits >= sbits {
            let mut cm = quant_partition(
                ctx,
                dec,
                x_first,
                half_n,
                mbits,
                half_blocks,
                lb_first,
                next_lm,
                gain * mid,
                fill,
            )?;
            let rebalance = mbits - (rebalance_start - ctx.remaining_bits);
            if rebalance > (3 << BITRES) && split.itheta != 0 {
                sbits += rebalance - (3 << BITRES);
            }
            cm |= quant_partition(
                ctx,
                dec,
                x_second,
                half_n,
                sbits,
                half_blocks,
                lb_second,
                next_lm,
                gain * side,
                fill >> half_blocks,
            )? << (b0 >> 1);
            return Ok(cm);
        } else {
            let mut cm = quant_partition(
                ctx,
                dec,
                x_second,
                half_n,
                sbits,
                half_blocks,
                lb_second,
                next_lm,
                gain * side,
                fill >> half_blocks,
            )? << (b0 >> 1);
            let rebalance = sbits - (rebalance_start - ctx.remaining_bits);
            if rebalance > (3 << BITRES) && split.itheta != 16384 {
                mbits += rebalance - (3 << BITRES);
            }
            cm |= quant_partition(
                ctx,
                dec,
                x_first,
                half_n,
                mbits,
                half_blocks,
                lb_first,
                next_lm,
                gain * mid,
                fill,
            )?;
            return Ok(cm);
        }
    }

    // Leaf partition: map Q3 bit allocation to a pulse count via the pulse cache.
    let mut q = bits2pulses_at_level(lm, ctx.band, b);
    let mut curr_bits = pulses2bits_at_level(lm, ctx.band, q);
    ctx.remaining_bits -= curr_bits;

    while ctx.remaining_bits < 0 && q > 0 {
        ctx.remaining_bits += curr_bits;
        q -= 1;
        curr_bits = pulses2bits_at_level(lm, ctx.band, q);
        ctx.remaining_bits -= curr_bits;
    }

    if q != 0 {
        let k = get_pulses(q as usize);
        return alg_unquant(&mut x[..n], n, k, ctx.spread, b_blocks, dec, gain);
    }

    // Zero-pulse partition: synthesize via spectral folding from `lowband` or LCG noise.
    let cm_mask = (1u32 << b_blocks) - 1;
    fill &= cm_mask;
    if fill == 0 {
        x[..n].fill(0.0_f32);
        return Ok(0);
    }

    if let Some(lb) = lowband {
        for (dst, &src) in x[..n].iter_mut().zip(lb[..n].iter()) {
            ctx.seed = celt_lcg_rand(ctx.seed);
            let jitter = if (ctx.seed & 0x8000) != 0 {
                1.0_f32 / 256.0_f32
            } else {
                -1.0_f32 / 256.0_f32
            };
            *dst = src + jitter;
        }
        renormalise_vector(&mut x[..n], n, gain);
        return Ok(fill);
    }

    for dst in &mut x[..n] {
        ctx.seed = celt_lcg_rand(ctx.seed);
        *dst = ((ctx.seed as i32) >> 20) as f32;
    }
    renormalise_vector(&mut x[..n], n, gain);
    return Ok(cm_mask);
}

/// Decodes a single mono Bark band `x[..n]`, applying time-frequency pre/post-recombination
/// (`quant_band` in `bands.c`, RFC 6716 Section 4.3.4).
#[allow(clippy::too_many_arguments)]
pub fn quant_band(
    ctx: &mut BandDecoderCtx,
    dec: &mut Decoder<'_>,
    x: &mut [f32],
    n: usize,
    b: i32,
    mut b_blocks: usize,
    lowband: Option<&[f32]>,
    lm: isize,
    lowband_out: Option<&mut [f32]>,
    gain: f32,
    mut fill: u32,
) -> Result<u32> {
    if n == 1 {
        return quant_band_n1(ctx, dec, x, None, lowband_out);
    }

    let n0 = n;
    let mut n_b = n / b_blocks;
    let long_blocks = b_blocks == 1;
    let recombine = ctx.tf_change.max(0) as usize;

    let mut lowband_buf = [0.0_f32; MAX_BAND_WIDTH];
    let has_lowband = if let Some(lb) = lowband {
        lowband_buf[..n].copy_from_slice(&lb[..n]);
        true
    } else {
        false
    };

    for k in 0..recombine {
        if has_lowband {
            haar1(&mut lowband_buf[..n], n >> k, 1 << k);
        }
        fill = u32::from(BIT_INTERLEAVE_TABLE[(fill & 0xF) as usize])
            | (u32::from(BIT_INTERLEAVE_TABLE[(fill >> 4) as usize]) << 2);
    }
    b_blocks >>= recombine;
    n_b <<= recombine;

    let mut time_divide = 0usize;
    let mut tf_change = ctx.tf_change;
    while (n_b & 1) == 0 && tf_change < 0 {
        if has_lowband {
            haar1(&mut lowband_buf[..n], n_b, b_blocks);
        }
        fill |= fill << b_blocks;
        b_blocks <<= 1;
        n_b >>= 1;
        time_divide += 1;
        tf_change += 1;
    }

    let b0 = b_blocks;
    let n_b0 = n_b;

    if b0 > 1 && has_lowband {
        deinterleave_hadamard(
            &mut lowband_buf[..n],
            n_b >> recombine,
            b0 << recombine,
            long_blocks,
        );
    }

    let mut cm = quant_partition(
        ctx,
        dec,
        &mut x[..n],
        n,
        b,
        b_blocks,
        if has_lowband {
            Some(&lowband_buf[..n])
        } else {
            None
        },
        lm,
        gain,
        fill,
    )?;

    // Undo time-frequency reorganization from time-block order back to frequency order.
    if b0 > 1 {
        interleave_hadamard(
            &mut x[..n],
            n_b >> recombine,
            b0 << recombine,
            long_blocks,
        );
    }

    n_b = n_b0;
    b_blocks = b0;
    for _ in 0..time_divide {
        b_blocks >>= 1;
        n_b <<= 1;
        cm |= cm >> b_blocks;
        haar1(&mut x[..n], n_b, b_blocks);
    }

    for k in 0..recombine {
        cm = u32::from(BIT_DEINTERLEAVE_TABLE[(cm & 0xF) as usize]);
        haar1(&mut x[..n], n0 >> k, 1 << k);
    }
    b_blocks <<= recombine;

    if let Some(lb_out) = lowband_out {
        let scale = (n0 as f32).sqrt();
        for (dst, &src) in lb_out[..n0].iter_mut().zip(x[..n0].iter()) {
            *dst = scale * src;
        }
    }

    cm &= (1u32 << b_blocks) - 1;
    return Ok(cm);
}

/// Unmixes normalized mid (`x[..n]`) and scaled side (`y[..n]`) vectors back into unit-norm
/// left (`x[..n]`) and right (`y[..n]`) channel vectors (`stereo_merge` in `bands.c`,
/// RFC 6716 Section 4.3.4.4).
pub fn stereo_merge(x: &mut [f32], y: &mut [f32], mid: f32, n: usize) {
    let mut xp = 0.0_f32;
    let mut side = 0.0_f32;
    for (&vy, &vx) in y[..n].iter().zip(x[..n].iter()) {
        xp += vy * vx;
        side += vy * vy;
    }

    xp *= mid;
    let el = mid * mid + side - 2.0_f32 * xp;
    let er = mid * mid + side + 2.0_f32 * xp;
    if er < 6e-4_f32 || el < 6e-4_f32 {
        y[..n].copy_from_slice(&x[..n]);
        return;
    }

    let lgain = 1.0_f32 / el.sqrt();
    let rgain = 1.0_f32 / er.sqrt();

    for (vx, vy) in x[..n].iter_mut().zip(y[..n].iter_mut()) {
        let l = mid * (*vx);
        let r = *vy;
        *vx = lgain * (l - r);
        *vy = rgain * (l + r);
    }

    return;
}

/// Decodes a coupled stereo Bark band (`x[..n]` and `y[..n]`) using mid-side or intensity
/// stereo splitting (`quant_band_stereo` in `bands.c`, RFC 6716 Section 4.3.4.4).
#[allow(clippy::too_many_arguments)]
pub fn quant_band_stereo(
    ctx: &mut BandDecoderCtx,
    dec: &mut Decoder<'_>,
    x: &mut [f32],
    y: &mut [f32],
    n: usize,
    mut b: i32,
    b_blocks: usize,
    lowband: Option<&[f32]>,
    lm: isize,
    lowband_out: Option<&mut [f32]>,
    mut fill: u32,
) -> Result<u32> {
    if n == 1 {
        return quant_band_n1(ctx, dec, x, Some(y), lowband_out);
    }

    let orig_fill = fill;
    let split = decode_split_angle(
        ctx,
        dec,
        n,
        &mut b,
        b_blocks,
        b_blocks,
        lm,
        true,
        &mut fill,
    )?;

    let mid = (split.imid as f32) * (1.0_f32 / 32768.0_f32);
    let side = (split.iside as f32) * (1.0_f32 / 32768.0_f32);

    let cm = if n == 2 {
        // Two-phase orthogonal stereo specialization for N = 2.
        let mut mbits = b;
        let sbits = if split.itheta != 0 && split.itheta != 16384 {
            1 << BITRES
        } else {
            0
        };
        mbits -= sbits;
        let side_is_primary = split.itheta > 8192;
        ctx.remaining_bits -= split.qalloc + sbits;

        let mut sign_bit = 0u32;
        if sbits != 0 {
            sign_bit = dec.decode_raw_bits(1)?;
        }
        let sign = 1.0_f32 - 2.0_f32 * (sign_bit as f32);

        let cm_res = if side_is_primary {
            let c = quant_band(
                ctx,
                dec,
                y,
                n,
                mbits,
                b_blocks,
                lowband,
                lm,
                lowband_out,
                1.0_f32,
                orig_fill,
            )?;
            x[0] = -sign * y[1];
            x[1] = sign * y[0];
            c
        } else {
            let c = quant_band(
                ctx,
                dec,
                x,
                n,
                mbits,
                b_blocks,
                lowband,
                lm,
                lowband_out,
                1.0_f32,
                orig_fill,
            )?;
            y[0] = -sign * x[1];
            y[1] = sign * x[0];
            c
        };

        x[0] *= mid;
        x[1] *= mid;
        y[0] *= side;
        y[1] *= side;
        let tmp0 = x[0];
        x[0] = tmp0 - y[0];
        y[0] += tmp0;
        let tmp1 = x[1];
        x[1] = tmp1 - y[1];
        y[1] += tmp1;
        cm_res
    } else {
        let mut mbits = 0.max(b.min((b - split.delta) / 2));
        let mut sbits = b - mbits;
        ctx.remaining_bits -= split.qalloc;

        let rebalance_start = ctx.remaining_bits;
        let cm_res = if mbits >= sbits {
            let mut c = quant_band(
                ctx,
                dec,
                x,
                n,
                mbits,
                b_blocks,
                lowband,
                lm,
                lowband_out,
                1.0_f32,
                fill,
            )?;
            let rebalance = mbits - (rebalance_start - ctx.remaining_bits);
            if rebalance > (3 << BITRES) && split.itheta != 0 {
                sbits += rebalance - (3 << BITRES);
            }
            c |= quant_band(
                ctx,
                dec,
                y,
                n,
                sbits,
                b_blocks,
                None,
                lm,
                None,
                side,
                fill >> b_blocks,
            )?;
            c
        } else {
            let mut c = quant_band(
                ctx,
                dec,
                y,
                n,
                sbits,
                b_blocks,
                None,
                lm,
                None,
                side,
                fill >> b_blocks,
            )?;
            let rebalance = sbits - (rebalance_start - ctx.remaining_bits);
            if rebalance > (3 << BITRES) && split.itheta != 16384 {
                mbits += rebalance - (3 << BITRES);
            }
            c |= quant_band(
                ctx,
                dec,
                x,
                n,
                mbits,
                b_blocks,
                lowband,
                lm,
                lowband_out,
                1.0_f32,
                fill,
            )?;
            c
        };

        stereo_merge(x, y, mid, n);
        cm_res
    };

    if split.invert_side {
        for vy in &mut y[..n] {
            *vy = -*vy;
        }
    }

    return Ok(cm);
}

/// Duplicates the tail of the first Hybrid band's normalized history (`start == 17`, width $8M$)
/// so the second Hybrid band (`band 18`, width $12M$) has a complete $12M$-sample lowband source
/// (`special_hybrid_folding` in `bands.c`).
pub fn special_hybrid_folding(
    norm: &mut [f32],
    norm2: Option<&mut [f32]>,
    start: usize,
    m: usize,
    dual_stereo: bool,
) {
    if start + 2 > MAX_BANDS {
        return;
    }
    let n1 = m * (EBANDS[start + 1] - EBANDS[start]);
    let n2 = m * (EBANDS[start + 2] - EBANDS[start + 1]);
    if n2 > n1 && 2 * n1 >= n2 {
        let src_start = 2 * n1 - n2;
        let copy_len = n2 - n1;
        norm.copy_within(src_start..(src_start + copy_len), n1);
        if dual_stereo {
            if let Some(n2_buf) = norm2 {
                n2_buf.copy_within(src_start..(src_start + copy_len), n1);
            }
        }
    }
    return;
}

/// Unquantizes all active Bark bands `start..end` for a mono (`y == None`) or stereo (`y == Some(...)`)
/// CELT frame (`quant_all_bands` in `bands.c`, RFC 6716 Section 4.3.4).
#[allow(clippy::too_many_arguments)]
pub fn quant_all_bands(
    start: usize,
    end: usize,
    x_full: &mut [f32],
    mut y_full: Option<&mut [f32]>,
    collapse_masks: &mut [u8],
    pulses: &[i32],
    short_blocks: bool,
    spread: Spread,
    mut dual_stereo: bool,
    intensity: usize,
    tf_res: &[i32],
    total_bits: i32,
    mut balance: i32,
    dec: &mut Decoder<'_>,
    lm: usize,
    coded_bands: usize,
    seed: &mut u32,
) -> Result<()> {
    let m = 1usize << lm;
    let b_blocks = if short_blocks { m } else { 1 };
    let norm_offset = m * EBANDS[start];
    let channels = if y_full.is_some() { 2usize } else { 1usize };

    let mut norm = [0.0_f32; MAX_NORM_BUFFER_SIZE];
    let mut norm2 = [0.0_f32; MAX_NORM_BUFFER_SIZE];

    let mut lowband_offset = 0usize;
    let mut update_lowband = true;

    let mut ctx = BandDecoderCtx {
        band: start,
        intensity_band: intensity,
        spread,
        tf_change: 0,
        remaining_bits: 0,
        seed: *seed,
    };

    for i in start..end {
        ctx.band = i;
        let last = i == end - 1;
        let band_start = m * EBANDS[i];
        let n = band_width(i, lm);
        let tell = dec.tell_frac() as i32;

        if i != start {
            balance -= tell;
        }
        let remaining_bits = total_bits - tell - 1;
        ctx.remaining_bits = remaining_bits;

        let b = if i < coded_bands {
            let divisor = 3.min((coded_bands - i) as i32);
            let curr_balance = balance / divisor;
            0.max(16383.min((remaining_bits + 1).min(pulses[i] + curr_balance)))
        } else {
            0
        };

        if (band_start >= norm_offset + n || i == start + 1)
            && (update_lowband || lowband_offset == 0)
        {
            lowband_offset = i;
        }
        if i == start + 1 {
            special_hybrid_folding(&mut norm, Some(&mut norm2), start, m, dual_stereo);
        }

        let tf_change = tf_res[i];
        ctx.tf_change = tf_change;

        let mut effective_lowband: Option<usize> = None;
        let (mut x_cm, mut y_cm) = if lowband_offset != 0
            && (spread != Spread::Aggressive || b_blocks > 1 || tf_change < 0)
        {
            let eff_lb = (m * EBANDS[lowband_offset])
                .saturating_sub(norm_offset + n);
            effective_lowband = Some(eff_lb);

            let mut fold_start = lowband_offset;
            while fold_start > 0 && m * EBANDS[fold_start - 1] > eff_lb + norm_offset {
                fold_start -= 1;
            }
            fold_start = fold_start.saturating_sub(1);

            let mut fold_end = lowband_offset - 1;
            while fold_end + 1 < i && m * EBANDS[fold_end + 1] < eff_lb + norm_offset + n {
                fold_end += 1;
            }
            fold_end += 1;

            let mut x_mask = 0u32;
            let mut y_mask = 0u32;
            for fold_i in fold_start..fold_end {
                x_mask |= u32::from(collapse_masks[fold_i * channels]);
                y_mask |= u32::from(collapse_masks[fold_i * channels + channels - 1]);
            }
            (x_mask, y_mask)
        } else {
            let full_mask = (1u32 << b_blocks) - 1;
            (full_mask, full_mask)
        };

        if dual_stereo && i == intensity {
            dual_stereo = false;
            let merge_len = band_start - norm_offset;
            for (n1, &n2) in norm[..merge_len].iter_mut().zip(norm2[..merge_len].iter()) {
                *n1 = 0.5_f32 * (*n1 + n2);
            }
        }

        let norm_write_pos = band_start - norm_offset;
        let mut lb_src1 = [0.0_f32; MAX_BAND_WIDTH];
        let mut lb_src2 = [0.0_f32; MAX_BAND_WIDTH];
        if let Some(eff_lb) = effective_lowband {
            lb_src1[..n].copy_from_slice(&norm[eff_lb..(eff_lb + n)]);
            if dual_stereo {
                lb_src2[..n].copy_from_slice(&norm2[eff_lb..(eff_lb + n)]);
            }
        }

        let x_band = &mut x_full[band_start..(band_start + n)];
        if dual_stereo {
            let y_band = y_full
                .as_deref_mut()
                .map(|yf| &mut yf[band_start..(band_start + n)])
                .ok_or(Error::InvalidChannelCount(1))?;

            x_cm = quant_band(
                &mut ctx,
                dec,
                x_band,
                n,
                b / 2,
                b_blocks,
                effective_lowband.map(|_| &lb_src1[..n]),
                lm as isize,
                if last {
                    None
                } else {
                    Some(&mut norm[norm_write_pos..(norm_write_pos + n)])
                },
                1.0_f32,
                x_cm,
            )?;

            y_cm = quant_band(
                &mut ctx,
                dec,
                y_band,
                n,
                b / 2,
                b_blocks,
                effective_lowband.map(|_| &lb_src2[..n]),
                lm as isize,
                if last {
                    None
                } else {
                    Some(&mut norm2[norm_write_pos..(norm_write_pos + n)])
                },
                1.0_f32,
                y_cm,
            )?;
        } else if let Some(yf) = y_full.as_deref_mut() {
            let y_band = &mut yf[band_start..(band_start + n)];
            x_cm = quant_band_stereo(
                &mut ctx,
                dec,
                x_band,
                y_band,
                n,
                b,
                b_blocks,
                effective_lowband.map(|_| &lb_src1[..n]),
                lm as isize,
                if last {
                    None
                } else {
                    Some(&mut norm[norm_write_pos..(norm_write_pos + n)])
                },
                x_cm | y_cm,
            )?;
            y_cm = x_cm;
        } else {
            x_cm = quant_band(
                &mut ctx,
                dec,
                x_band,
                n,
                b,
                b_blocks,
                effective_lowband.map(|_| &lb_src1[..n]),
                lm as isize,
                if last {
                    None
                } else {
                    Some(&mut norm[norm_write_pos..(norm_write_pos + n)])
                },
                1.0_f32,
                x_cm | y_cm,
            )?;
            y_cm = x_cm;
        }

        collapse_masks[i * channels] = x_cm as u8;
        collapse_masks[i * channels + channels - 1] = y_cm as u8;
        balance += pulses[i] + tell;

        update_lowband = b > ((n as i32) << BITRES);
    }

    *seed = ctx.seed;
    return Ok(());
}

/// Prevents energy collapse across short MDCT sub-blocks during transient frames by injecting
/// pseudorandom noise into sub-blocks that received zero pulses and renormalizing the band
/// (`anti_collapse` in `bands.c`, RFC 6716 Section 4.3.5).
#[allow(clippy::too_many_arguments)]
pub fn anti_collapse(
    x: &mut [f32],
    collapse_masks: &[u8],
    lm: usize,
    channels: usize,
    size: usize,
    start: usize,
    end: usize,
    log_e: &[[f32; MAX_BANDS]],
    prev1_log_e: &[[f32; MAX_BANDS]],
    prev2_log_e: &[[f32; MAX_BANDS]],
    pulses: &[i32],
    mut seed: u32,
) {
    let blocks = 1usize << lm;

    for i in start..end {
        let n0 = EBANDS[i + 1] - EBANDS[i];
        let depth = ((1 + pulses[i]) / (n0 as i32)) >> lm;
        let thresh = 0.5_f32 * celt_exp2(-0.125_f32 * (depth as f32));
        let sqrt_1 = 1.0_f32 / ((n0 << lm) as f32).sqrt();

        for c in 0..channels {
            let mut prev1 = prev1_log_e[c][i];
            let mut prev2 = prev2_log_e[c][i];
            if channels == 1 && prev1_log_e.len() >= 2 && prev2_log_e.len() >= 2 {
                prev1 = prev1.max(prev1_log_e[1][i]);
                prev2 = prev2.max(prev2_log_e[1][i]);
            }
            let ediff = (log_e[c][i] - prev1.min(prev2)).max(0.0_f32);

            let mut r = 2.0_f32 * celt_exp2(-ediff);
            if lm == 3 {
                r *= SQRT_2;
            }
            r = thresh.min(r) * sqrt_1;

            let band_offset = c * size + (EBANDS[i] << lm);
            let band_slice = &mut x[band_offset..(band_offset + (n0 << lm))];
            let mask = collapse_masks[i * channels + c];
            let mut needs_renorm = false;

            for k in 0..blocks {
                if (mask & (1u8 << k)) == 0 {
                    for j in 0..n0 {
                        seed = celt_lcg_rand(seed);
                        band_slice[(j << lm) + k] = if (seed & 0x8000) != 0 { r } else { -r };
                    }
                    needs_renorm = true;
                }
            }

            if needs_renorm {
                renormalise_vector(band_slice, n0 << lm, 1.0_f32);
            }
        }
    }

    return;
}

/// Denormalizes unit-energy PVQ bands `x` by their decoded log2 band energies `band_log_e`
/// plus the static mean log2 energies [`E_MEANS`] to produce full-scale MDCT coefficients `freq`
/// (`denormalise_bands` in `bands.c`, RFC 6716 Section 4.3.6.1).
#[allow(clippy::too_many_arguments)]
pub fn denormalise_bands(
    x: &[f32],
    freq: &mut [f32],
    band_log_e: &[f32],
    mut start: usize,
    mut end: usize,
    m: usize,
    downsample: usize,
    silence: bool,
) {
    let n = m * SHORT_MDCT_SIZE;
    let mut bound = m * EBANDS[end];
    if downsample != 1 {
        bound = bound.min(n / downsample);
    }
    if silence {
        bound = 0;
        start = 0;
        end = 0;
    }

    let start_bin = m * EBANDS[start];
    freq[..start_bin].fill(0.0_f32);

    for i in start..end {
        let bin_start = m * EBANDS[i];
        let bin_end = m * EBANDS[i + 1];
        let lg = (band_log_e[i] + E_MEANS[i]).min(32.0_f32);
        let gain = celt_exp2(lg);

        for (dst, &src) in freq[bin_start..bin_end]
            .iter_mut()
            .zip(x[bin_start..bin_end].iter())
        {
            *dst = src * gain;
        }
    }

    if bound < n {
        freq[bound..n].fill(0.0_f32);
    }

    return;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exp_rotation_matches_libopus_oracle() {
        let mut x = [0.0_f32; 16];
        for (i, slot) in x.iter_mut().enumerate() {
            *slot = ((i + 1) as f32) * 0.1_f32;
        }
        renormalise_vector(&mut x, 16, 1.0_f32);
        exp_rotation(&mut x, 16, -1, 2, 5, Spread::Normal);

        let expected: [f32; 16] = [
            1.54906273e-01,
            2.25728750e-01,
            -1.21630229e-01,
            -1.09531626e-01,
            -1.53222278e-01,
            6.74848109e-02,
            4.28060964e-02,
            6.89890534e-02,
            3.23417872e-01,
            4.45986003e-01,
            -2.81007141e-01,
            -3.11197400e-01,
            -3.96054655e-01,
            2.41699502e-01,
            2.75663614e-01,
            3.05051595e-01,
        ];

        for (i, (&got, &exp)) in x.iter().zip(expected.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "exp_rotation mismatch at {i}: got {got}, expected {exp}"
            );
        }

        // Verify forward rotation followed by inverse rotation is an exact identity up to float precision.
        let mut roundtrip = expected;
        exp_rotation(&mut roundtrip, 16, 1, 2, 5, Spread::Normal);
        exp_rotation(&mut roundtrip, 16, -1, 2, 5, Spread::Normal);
        for (i, (&got, &exp)) in roundtrip.iter().zip(expected.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "exp_rotation roundtrip mismatch at {i}: got {got}, expected {exp}"
            );
        }
        return;
    }

    #[test]
    fn test_alg_unquant_matches_libopus_oracle() {
        let packet: [u8; 16] = [
            0x43, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x01, 0x58,
        ];
        let mut dec = Decoder::new(&packet).expect("valid decoder");
        let mut x = [0.0_f32; 8];
        let cm = alg_unquant(&mut x, 8, 7, Spread::Normal, 2, &mut dec, 1.0_f32)
            .expect("alg_unquant succeeds");

        assert_eq!(cm, 3);
        let expected: [f32; 8] = [
            3.33333343e-01,
            -3.33333343e-01,
            0.0,
            6.66666687e-01,
            -3.33333343e-01,
            0.0,
            -3.33333343e-01,
            3.33333343e-01,
        ];
        for (i, (&got, &exp)) in x.iter().zip(expected.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-7,
                "alg_unquant mismatch at {i}: got {got}, expected {exp}"
            );
        }
        return;
    }

    #[test]
    fn test_quant_all_bands_anti_collapse_and_denormalise_match_libopus_oracle() {
        let qbuf: [u8; 32] = [
            0x31, 0x66, 0xd7, 0x3e, 0x26, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];
        let pulses: [i32; MAX_BANDS] = [
            40, 36, 32, 28, 24, 24, 20, 16, 32, 28, 20, 12, 8, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let tf_res: [i32; MAX_BANDS] = [
            0, 0, 0, 0, 0, 0, 0, 0, 1, 0, -1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];

        let mut dec = Decoder::new(&qbuf).expect("valid decoder");
        let mut x_dec = [0.0_f32; 240];
        let mut y_dec = [0.0_f32; 240];
        let mut cm_masks = [0u8; 42];
        let mut seed = 12345u32;

        quant_all_bands(
            0,
            13,
            &mut x_dec,
            Some(&mut y_dec),
            &mut cm_masks,
            &pulses,
            true,
            Spread::Normal,
            false,
            10,
            &tf_res,
            32 * 8 * 8,
            0,
            &mut dec,
            1,
            12,
            &mut seed,
        )
        .expect("quant_all_bands succeeds");

        assert_eq!(seed, 3_194_601_925);
        let expected_cm: [u8; 26] = [
            3, 3, 3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 3, 3, 2, 2, 1, 1, 3, 3, 3, 3,
        ];
        assert_eq!(&cm_masks[..26], &expected_cm);

        let expected_x_sample: [f32; 32] = [
            5.54683268e-01,
            8.32024932e-01,
            7.07085192e-01,
            7.07085192e-01,
            8.94399881e-01,
            4.47199941e-01,
            0.00000000e+00,
            9.99969482e-01,
            0.00000000e+00,
            -9.99969482e-01,
            0.00000000e+00,
            -9.99969482e-01,
            -9.99969482e-01,
            0.00000000e+00,
            -9.99969482e-01,
            0.00000000e+00,
            -8.43568146e-02,
            9.59317237e-02,
            7.45065928e-01,
            6.54643178e-01,
            0.00000000e+00,
            9.97945368e-01,
            0.00000000e+00,
            6.40701875e-02,
            7.07106769e-01,
            0.00000000e+00,
            7.07106769e-01,
            0.00000000e+00,
            -1.94773776e-03,
            -7.07104146e-01,
            -7.07104146e-01,
            -1.94773776e-03,
        ];
        for (i, (&got, &exp)) in x_dec[..32].iter().zip(expected_x_sample.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "quant_all_bands X mismatch at {i}: got {got}, expected {exp}"
            );
        }

        // Verify anti_collapse against libopus oracle
        let mut xy_ac = [0.0_f32; 480];
        xy_ac[..240].copy_from_slice(&x_dec);
        xy_ac[240..].copy_from_slice(&y_dec);

        let log_e = [[2.5_f32; MAX_BANDS]; 2];
        let prev1 = [[0.5_f32; MAX_BANDS]; 2];
        let prev2 = [[1.0_f32; MAX_BANDS]; 2];
        cm_masks[8 * 2] = 1;
        cm_masks[9 * 2 + 1] = 2;

        anti_collapse(
            &mut xy_ac,
            &cm_masks,
            1,
            2,
            240,
            0,
            13,
            &log_e,
            &prev1,
            &prev2,
            &pulses,
            seed,
        );

        let expected_ac_b8_x: [f32; 4] = [
            -1.09499902e-01,
            1.62257046e-01,
            9.67137575e-01,
            -1.62257046e-01,
        ];
        let expected_ac_b9_y: [f32; 4] = [
            1.33849055e-01,
            9.79903519e-01,
            1.33849055e-01,
            6.29118606e-02,
        ];
        for (i, (&got, &exp)) in xy_ac[16..20].iter().zip(expected_ac_b8_x.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "anti_collapse B8 X mismatch at {i}: got {got}, expected {exp}"
            );
        }
        for (i, (&got, &exp)) in xy_ac[260..264]
            .iter()
            .zip(expected_ac_b9_y.iter())
            .enumerate()
        {
            assert!(
                (got - exp).abs() <= 1e-6,
                "anti_collapse B9 Y mismatch at {i}: got {got}, expected {exp}"
            );
        }

        // Verify denormalise_bands against libopus oracle
        let mut freq = [0.0_f32; 240];
        denormalise_bands(&xy_ac[..240], &mut freq, &log_e[0], 0, 13, 2, 1, false);
        let expected_denorm: [f32; 16] = [
            2.71957245e+02,
            4.07935883e+02,
            3.04427734e+02,
            3.04427734e+02,
            2.72288422e+02,
            1.36144211e+02,
            -2.35004654e+01,
            2.23568542e+02,
            -2.34474545e+01,
            -1.87573914e+02,
            1.97168789e+01,
            -1.57730225e+02,
            -1.26608727e+02,
            1.88210735e+01,
            -1.15584290e+02,
            2.04332314e+01,
        ];
        for (i, (&got, &exp)) in freq[..16].iter().zip(expected_denorm.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-4,
                "denormalise_bands mismatch at {i}: got {got}, expected {exp}"
            );
        }
        return;
    }

    #[test]
    fn test_hybrid_folding_and_recursive_split_matches_libopus_oracle() {
        let hbuf: [u8; 64] = [
            0xee, 0x02, 0x68, 0x74, 0xc2, 0x8c, 0x4e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x01, 0x18, 0x05, 0xd9, 0xb7, 0x77,
        ];
        let mut pulses = [0i32; MAX_BANDS];
        pulses[17] = 560;
        pulses[18] = 0;
        pulses[19] = 160;
        pulses[20] = 80;

        let mut tf_res = [0i32; MAX_BANDS];
        tf_res[17] = -1;
        tf_res[19] = -1;

        let mut dec = Decoder::new(&hbuf).expect("valid decoder");
        let mut x_dec = [0.0_f32; 960];
        let mut y_dec = [0.0_f32; 960];
        let mut cm_dec = [0u8; 42];
        let mut seed = 54321u32;

        quant_all_bands(
            17,
            21,
            &mut x_dec,
            Some(&mut y_dec),
            &mut cm_dec,
            &pulses,
            false,
            Spread::Aggressive,
            true,
            19,
            &tf_res,
            64 * 8 * 8,
            0,
            &mut dec,
            3,
            21,
            &mut seed,
        )
        .expect("hybrid quant_all_bands succeeds");

        assert_eq!(seed, 3_429_552_881);
        assert_eq!(&cm_dec[34..42], &[1, 1, 1, 1, 1, 1, 1, 1]);

        let expected_b17_x: [f32; 8] = [
            -2.58565307e-01,
            -2.58565307e-01,
            -1.23134904e-01,
            -1.23134904e-01,
            -3.38164717e-02,
            -3.38164717e-02,
            1.36362864e-02,
            1.36362864e-02,
        ];
        let expected_b18_x: [f32; 8] = [
            9.48805958e-02,
            1.30983472e-01,
            -4.69578542e-02,
            1.42883748e-01,
            1.63870052e-01,
            7.79950665e-03,
            -9.67299640e-02,
            -4.98525146e-03,
        ];
        let expected_b18_y: [f32; 8] = [
            7.00975358e-02,
            1.43721988e-02,
            4.78779413e-02,
            -7.78567567e-02,
            1.24147587e-01,
            -1.47601604e-01,
            3.63272764e-02,
            -1.36050940e-01,
        ];
        let expected_b19_y: [f32; 8] = [
            3.84046286e-02,
            3.84046286e-02,
            5.76669723e-02,
            5.76669723e-02,
            4.15627807e-02,
            4.15627807e-02,
            1.84608120e-02,
            1.84608120e-02,
        ];

        for (i, (&got, &exp)) in x_dec[320..328].iter().zip(expected_b17_x.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "Hybrid B17 X mismatch at {i}: got {got}, expected {exp}"
            );
        }
        for (i, (&got, &exp)) in x_dec[384..392].iter().zip(expected_b18_x.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "Hybrid B18 folded X mismatch at {i}: got {got}, expected {exp}"
            );
        }
        for (i, (&got, &exp)) in y_dec[384..392].iter().zip(expected_b18_y.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "Hybrid B18 folded Y mismatch at {i}: got {got}, expected {exp}"
            );
        }
        for (i, (&got, &exp)) in y_dec[480..488].iter().zip(expected_b19_y.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "Hybrid B19 intensity Y mismatch at {i}: got {got}, expected {exp}"
            );
        }
        return;
    }

    #[test]
    fn test_pvq_search_stereo_itheta_and_isqrt32() {
        assert_eq!(isqrt32(0), 0);
        assert_eq!(isqrt32(1), 1);
        assert_eq!(isqrt32(15), 3);
        assert_eq!(isqrt32(16), 4);
        assert_eq!(isqrt32(1_000_000), 1000);

        let mut x = [0.5_f32, -0.2_f32, 0.1_f32, 0.7_f32, -0.3_f32, 0.05_f32, -0.4_f32, 0.25_f32];
        renormalise_vector(&mut x, 8, 1.0_f32);
        let mut iy = [0i32; 8];
        let yy = op_pvq_search(&mut x, &mut iy, 7, 8);
        let l1: i32 = iy.iter().map(|&p| p.abs()).sum();
        assert_eq!(l1, 7);
        assert_eq!(yy, 9.0_f32);
        assert_eq!(iy, [1, -1, 0, 2, -1, 0, -1, 1]);

        let left = [1.0_f32, 0.0_f32, 0.0_f32, 0.0_f32];
        let right = [1.0_f32, 0.0_f32, 0.0_f32, 0.0_f32];
        assert_eq!(stereo_itheta(&left, &right, true, 4), 11);

        let opp_right = [-1.0_f32, 0.0_f32, 0.0_f32, 0.0_f32];
        assert_eq!(stereo_itheta(&left, &opp_right, true, 4), 1 << 30);
        return;
    }
}
