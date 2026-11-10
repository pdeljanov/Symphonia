//! CELT band shape decoding: PVQ unquantization, spreading, split decoding with stereo
//! unmixing, folding, time-frequency changes and anti-collapse
//! ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>) and
//! [Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)).
//!
//! Once the band energies and the bit allocation are known, every Bark band in `start..end` is
//! reconstructed as a unit-norm vector: a band wide enough for its budget is split in two around
//! a decoded angle `theta` ([Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)),
//! a coupled stereo band is split into mid and side the same way, every leaf is a pulse vector
//! read from the range coder ([Section 4.3.4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.2>))
//! and rotated against tonal artifacts ([Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)),
//! and a leaf without pulses is folded from the normalised lower bands or filled with noise.
//! Short-block frames reorder each band between time and frequency around the decoding
//! ([Section 4.3.4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.5>)) and
//! refill the blocks that collapsed ([Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)).
//!
//! ```text
//!   band i                 unquantize_bands            folding history (norm)
//!   +----------+   bits    +---------------------+      +-------------------+
//!   | pulses   |---------->| quant_band          |<-----| bands below i     |
//!   | tf_res   |           |  time-frequency     |      +-------------------+
//!   | spread   |           |  quant_partition    |----->| band i (scaled)   |
//!   +----------+           |   split angle       |      +-------------------+
//!        ^                 |   pvq_unquantize    |
//!        |                 |   fold / noise      |      collapse masks
//!   range decoder -------->| quant_band_stereo   |----->| x | y | per band  |
//!                          +---------------------+      +-------------------+
//! ```

use std::f32::consts::{FRAC_1_SQRT_2, SQRT_2};
use symphonia_core::errors::Result;

use crate::celt::constant::{
    BIT_DEINTERLEAVE_TABLE, BIT_INTERLEAVE_TABLE, BITRES, EBANDS, LOGN, MAX_BANDS, QTHETA_OFFSET,
    QTHETA_OFFSET_TWOPHASE, Spread, band_width,
};
use crate::celt::cwrs::decode_pulses;
use crate::celt::energy::BandEnergyHistory;
use crate::celt::error::Error;
use crate::celt::rate::{
    BandAllocation, CacheLevel, bits2pulses_at_level, get_pulses, max_cache_bits,
    pulses2bits_at_level,
};
use crate::celt::{BlockSize, MAX_CHANNELS, MAX_LM, lcg_next};
use crate::entropy::Decoder;
use crate::planes::Planes;

/// Maximum number of MDCT bins in any single Bark band (band 20 at `LM = 3`: `22 * 8 = 176`).
const MAX_BAND_WIDTH: usize = 176;

/// Maximum total size of the normalized lowband folding buffer (`8 * EBANDS[20] = 8 * 78 = 624`).
const MAX_NORM_BUFFER_SIZE: usize = 8 * EBANDS[MAX_BANDS - 1];

/// Small positive floor added to squared norms before the reciprocal square root used by band
/// renormalisation ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)).
const NORM_EPSILON: f32 = 1e-15_f32;

/// Natural-to-sequency Hadamard permutation table for strides `2, 4, 8, 16` used by the
/// time-frequency change ([RFC 6716 Section 4.3.4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.5>)).
const HADAMARD_ORDERY_TABLE: [usize; 30] = [
    1, 0, // stride = 2 (offset 0)
    3, 0, 2, 1, // stride = 4 (offset 2)
    7, 0, 4, 3, 6, 1, 5, 2, // stride = 8 (offset 6)
    15, 0, 8, 7, 12, 3, 11, 4, 14, 1, 9, 6, 13, 2, 10, 5, // stride = 16 (offset 14)
];

/// Base-2 exponential lookup table for fractional Q3 bit resolution in [`compute_qn`]
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
const EXP2_TABLE8: [i32; 8] = [16384, 17866, 19483, 21247, 23170, 25267, 27554, 30048];

/// PVQ spreading rotation factor indexed by `(spread as usize) - 1` ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)).
const SPREAD_FACTOR: [usize; 3] = [15, 10, 5];

/// Split angle `theta = pi / 2` in Q14 (`16384`): all energy in the side (or second) half
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
const THETA_MAX_Q14: i32 = 16384;

/// Weight of the split angles up to `qn / 2` in the step PDF of a stereo split; the angles above
/// weigh `1` ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
const STEP_PDF_WEIGHT: i32 = 3;

/// Split angle `theta = pi / 4` in Q14 (`8192`): equal energy in both halves.
const THETA_HALF_Q14: i32 = 8192;

/// Bits of `itheta` below `THETA_MAX_Q14`; a zero masked value means `theta` is `0` or `pi / 2`.
const THETA_INTERIOR_MASK_Q14: i32 = 0x3fff;

/// Unit weight in Q15 (`32767`), the cosine or sine of a split at `theta = 0` or `pi / 2`.
const Q15_ONE: i32 = 32767;

/// Scale from a Q15 weight to `f32` (`1 / 32768`).
const Q15_SCALE: f32 = 1.0 / 32768.0;

/// Eighth bits a first-decoded half may leave unused before the rest goes to the other half
/// (`3` bits) ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
const REBALANCE_THRESHOLD_Q3: i32 = 3 << BITRES;

/// Eighth bits (`1.5` bits) beyond the pulse cache capacity that make a partition split
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
const SPLIT_MARGIN_Q3: i32 = 12;

/// Largest shape budget of one band in eighth bits (`16383`)
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
const MAX_BAND_BITS_Q3: i32 = 16383;

/// Number of following bands the leftover bit balance is shared over
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
const BALANCE_SHARE_BANDS: i32 = 3;

/// Half a bit in eighth bits, below which the split angle is not coded.
const HALF_BIT_Q3: i32 = 1 << BITRES >> 1;

/// Bit of the spreading generator state that gives a pseudo-random sign
/// ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)).
const LCG_SIGN_BIT: u32 = 0x8000;

/// Right shift that turns the 32-bit generator state into a 12-bit signed noise sample.
const LCG_NOISE_SHIFT: u32 = 20;

/// Jitter added to a folded coefficient (`1 / 256`)
/// ([RFC 6716 Section 4.3.4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.2>)).
const FOLD_JITTER: f32 = 1.0 / 256.0;

/// Smallest left or right energy the stereo unmixing scales by; below it the side is dropped
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
const STEREO_MERGE_MIN_ENERGY: f32 = 6e-4_f32;

/// Anti-collapse threshold decay per pulse of depth, in base-2 log units (`1 / 8`)
/// ([RFC 6716 Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)).
const COLLAPSE_DEPTH_DECAY: f32 = 0.125;

/// Evaluates `cos(pi / 2 * x)` in `f64` and rounds once to `f32`.
#[inline]
fn cos_quarter_turn(x: f32) -> f32 {
    return (std::f64::consts::FRAC_PI_2 * f64::from(x)).cos() as f32;
}

/// Evaluates `2^x = e^(x ln 2)` in `f64` and rounds once to `f32`.
#[inline]
fn exp2(x: f32) -> f32 {
    return (std::f64::consts::LN_2 * f64::from(x)).exp() as f32;
}

/// Fixed-point Q15 fractional multiplication `((16384 + (a as i16 as i32) * (b as i16 as i32)) >> 15)`
/// used by the bit-exact split-angle trigonometry ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
#[inline]
const fn q15_mul_round(a: i32, b: i32) -> i32 {
    let a16 = (a as i16) as i32;
    let b16 = (b as i16) as i32;
    return (16384 + a16 * b16) >> 15;
}

/// Returns the 1-based position of the highest set bit of `val`, or `0` if `val == 0`: the
/// integer base-2 logarithm of the range coder ([RFC 6716 Section 4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1>)).
#[inline]
const fn bit_length(val: u32) -> i32 {
    return (32 - val.leading_zeros()) as i32;
}

/// Bit-exact Q15 cosine approximation on `x` in `0..=16384` for the split angle
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
///
/// Bit-exactness of this polynomial is required because the resulting `imid` and `iside` values
/// feed into [`bitexact_log2tan`] to determine the mid/side bit allocation split `delta`.
pub(crate) const fn bitexact_cos(x: i16) -> i16 {
    let x32 = x as i32;
    let tmp = (4096 + x32 * x32) >> 13;
    let x2 = (Q15_ONE - tmp)
        + q15_mul_round(tmp, -7651 + q15_mul_round(tmp, 8277 + q15_mul_round(-626, tmp)));
    return (1 + x2) as i16;
}

/// Bit-exact base-2 log-tangent approximation `log2(isin / icos)` in Q11
/// for the split-angle bit allocation ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
pub(crate) const fn bitexact_log2tan(isin: i32, icos: i32) -> i32 {
    let lc = bit_length(icos as u32);
    let ls = bit_length(isin as u32);
    let icos_norm = icos << (15 - lc);
    let isin_norm = isin << (15 - ls);
    return (ls - lc) * (1 << 11)
        + q15_mul_round(isin_norm, q15_mul_round(isin_norm, -2597) + 7932)
        - q15_mul_round(icos_norm, q15_mul_round(icos_norm, -2597) + 7932);
}

/// Applies a single bidirectional sweep of Givens rotations with step `stride` and rotation
/// coefficients `(c, s) = (cos phi, sin phi)` across `x`
/// ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)).
fn rotation_sweep(x: &mut [f32], stride: usize, c: f32, s: f32) {
    let len = x.len();
    if len <= stride {
        return;
    }
    let mut rotate = |i: usize| {
        let x1 = x[i];
        let x2 = x[i + stride];
        x[i + stride] = c * x2 + s * x1;
        x[i] = c * x1 - s * x2;
    };
    (0..(len - stride)).for_each(&mut rotate);
    if len > 2 * stride {
        (0..(len - 2 * stride)).rev().for_each(rotate);
    }
    return;
}

/// Angle and strides of the spreading rotation of a vector of `len` bins with `pulses` pulses
/// over `blocks` interleaved sub-blocks
/// ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)).
///
/// The rotation angle is `theta = 1/2 * (len / (len + factor * pulses))^2`; `None` means no
/// rotation applies (`2 * pulses >= len`, or no spreading).
#[derive(Debug, Clone, Copy)]
struct SpreadingRotation {
    /// `cos(pi / 2 * theta)`.
    c: f32,
    /// `sin(pi / 2 * theta)`.
    s: f32,
    /// Second, wider rotation stride (`0` when the sub-blocks are too short for it).
    stride2: usize,
    /// Bins per interleaved sub-block.
    sub_len: usize,
}

impl SpreadingRotation {
    fn new(len: usize, blocks: usize, pulses: usize, spread: Spread) -> Option<Self> {
        if 2 * pulses >= len || spread == Spread::None || blocks == 0 {
            return None;
        }
        let factor = SPREAD_FACTOR[(spread as usize) - 1];
        let gain = (len as f32) / ((len + factor * pulses) as f32);
        let theta = 0.5_f32 * gain * gain;
        let c = cos_quarter_turn(theta);
        let s = cos_quarter_turn(1.0_f32 - theta);

        let mut stride2 = 0usize;
        if len >= 8 * blocks {
            stride2 = 1;
            while (stride2 * stride2 + stride2) * blocks + (blocks >> 2) < len {
                stride2 += 1;
            }
        }
        return Some(Self { c, s, stride2, sub_len: len / blocks });
    }
}

/// Undoes the PVQ spreading rotation across `x` with `pulses` pulses over `blocks` sub-blocks
/// ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)).
fn spreading_rotation(x: &mut [f32], blocks: usize, pulses: usize, spread: Spread) {
    let Some(SpreadingRotation { c, s, stride2, sub_len }) =
        SpreadingRotation::new(x.len(), blocks, pulses, spread)
    else {
        return;
    };
    for chunk in x.chunks_exact_mut(sub_len) {
        if stride2 > 0 {
            rotation_sweep(chunk, stride2, s, c);
        }
        rotation_sweep(chunk, 1, c, s);
    }
    return;
}

/// Scales the integer pulse vector `iy` with squared L2 norm `ryy` to have Euclidean
/// norm `gain` and writes the result to `x` ([RFC 6716 Section 4.3.4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.2>)).
fn normalise_residual(iy: &[i32], x: &mut [f32], ryy: f32, gain: f32) {
    let scale = gain / ryy.sqrt();
    for (dst, &pulse) in x.iter_mut().zip(iy.iter()) {
        *dst = (pulse as f32) * scale;
    }
    return;
}

/// Extracts the `blocks`-bit sub-block collapse mask from the decoded pulse vector `iy`,
/// setting bit `i` if and only if sub-block `i` contains at least one non-zero pulse
/// for anti-collapse processing ([RFC 6716 Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)).
fn collapse_mask(iy: &[i32], blocks: usize) -> u32 {
    if blocks <= 1 {
        return 1;
    }
    let n0 = iy.len() / blocks;
    return iy
        .chunks_exact(n0)
        .enumerate()
        .filter(|(_, block_pulses)| block_pulses.iter().any(|&p| p != 0))
        .fold(0u32, |mask, (i, _)| mask | (1u32 << i));
}

/// Unquantizes the algebraic PVQ partition `x` with `pulses` pulses and `blocks` time sub-blocks
/// from `dec`, scales it to Euclidean norm `gain`, applies the inverse spreading rotation, and
/// returns the `blocks`-bit collapse mask ([RFC 6716 Section 4.3.4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.2>) and
/// [RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)).
fn pvq_unquantize(
    x: &mut [f32],
    pulses: usize,
    spread: Spread,
    blocks: usize,
    dec: &mut Decoder<'_>,
    gain: f32,
) -> Result<u32> {
    let n = x.len();
    if n == 0 || n > MAX_BAND_WIDTH || pulses == 0 {
        return Err(Error::InvalidPvqParameters.into());
    }
    let mut iy = [0i32; MAX_BAND_WIDTH];
    let ryy = decode_pulses(&mut iy[..n], n, pulses, dec)?;
    normalise_residual(&iy[..n], x, ryy, gain);
    spreading_rotation(x, blocks, pulses, spread);
    return Ok(collapse_mask(&iy[..n], blocks));
}

/// Renormalizes `x[..n]` in place so that its Euclidean norm equals `gain`
/// ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)).
pub(crate) fn renormalise_vector(x: &mut [f32], n: usize, gain: f32) {
    let energy = x[..n].iter().fold(NORM_EPSILON, |acc, &val| acc + val * val);
    let scale = gain / energy.sqrt();
    x[..n].iter_mut().for_each(|val| *val *= scale);
    return;
}

/// Applies a single-level 1D Haar wavelet butterfly between the `stride`-spaced halves of each
/// `2 * stride` group of `x[..n0 * stride]`
/// ([RFC 6716 Section 4.3.4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.5>)).
fn haar_transform(x: &mut [f32], n0: usize, stride: usize) {
    for pair in x[..(n0 * stride)].chunks_exact_mut(2 * stride) {
        let (low, high) = pair.split_at_mut(stride);
        for (a, b) in low.iter_mut().zip(high.iter_mut()) {
            let tmp1 = FRAC_1_SQRT_2 * *a;
            let tmp2 = FRAC_1_SQRT_2 * *b;
            *a = tmp1 + tmp2;
            *b = tmp1 - tmp2;
        }
    }
    return;
}

/// Order of the time blocks of a band after de-interleaving
/// ([RFC 6716 Section 4.3.4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.5>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HadamardOrder {
    /// Blocks in time order.
    Natural,
    /// Blocks in Hadamard sequency order (a long-block frame whose band was time-divided).
    Sequency,
}

impl HadamardOrder {
    /// Row of the de-interleaved buffer that holds time block `i` of `stride` blocks.
    #[inline]
    fn row(self, i: usize, stride: usize) -> usize {
        return match self {
            Self::Natural => i,
            Self::Sequency => HADAMARD_ORDERY_TABLE[stride - 2 + i],
        };
    }
}

/// De-interleaves `x[..n0 * stride]` from frequency order into time-block order in `order`
/// ([RFC 6716 Section 4.3.4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.5>)).
fn deinterleave_hadamard(x: &mut [f32], n0: usize, stride: usize, order: HadamardOrder) {
    let total = n0 * stride;
    let mut tmp = [0.0_f32; MAX_BAND_WIDTH];
    for i in 0..stride {
        let row = order.row(i, stride);
        let column = x[i..total].iter().step_by(stride);
        tmp[(row * n0)..((row + 1) * n0)].iter_mut().zip(column).for_each(|(dst, &src)| *dst = src);
    }
    x[..total].copy_from_slice(&tmp[..total]);
    return;
}

/// Interleaves `x[..n0 * stride]` from time-block order in `order` back into frequency order
/// ([RFC 6716 Section 4.3.4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.5>)).
fn interleave_hadamard(x: &mut [f32], n0: usize, stride: usize, order: HadamardOrder) {
    let total = n0 * stride;
    let mut tmp = [0.0_f32; MAX_BAND_WIDTH];
    for i in 0..stride {
        let row = order.row(i, stride);
        let column = tmp[i..total].iter_mut().step_by(stride);
        column.zip(x[(row * n0)..((row + 1) * n0)].iter()).for_each(|(dst, &src)| *dst = src);
    }
    x[..total].copy_from_slice(&tmp[..total]);
    return;
}

/// What a split divides: one partition into two halves, or a coupled band into mid and side
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplitKind {
    /// A mono partition split into two halves of equal width.
    Partition,
    /// A coupled stereo band split into mid and side.
    Stereo,
}

/// Computes the quantization resolution `qn` (an even integer in `2..=256`, or `1` if unsplit)
/// for the split angle `theta` of a vector of `n` bins with `bits` eighth bits
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
fn compute_qn(n: usize, bits: i32, offset: i32, pulse_cap: i32, kind: SplitKind) -> i32 {
    let mut n2 = 2 * (n as i32) - 1;
    if kind == SplitKind::Stereo && n == 2 {
        n2 -= 1;
    }

    let mut qb = (bits + n2 * offset) / n2;
    qb = qb.min(bits - pulse_cap - (4 << BITRES));
    qb = qb.min(8 << BITRES);

    if qb < HALF_BIT_Q3 {
        return 1;
    }
    let raw = EXP2_TABLE8[(qb & 0x7) as usize] >> (14 - (qb >> BITRES));
    return ((raw + 1) >> 1) << 1;
}

/// Decoded split-angle parameters for recursive mono partition or stereo mid/side splitting
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SplitAngle {
    /// Whether the side channel sign is inverted (intensity stereo with `invert_side == true`).
    pub(crate) invert_side: bool,
    /// Fixed-point Q15 cosine weight `cos(theta)` for the mid / first half (`0..=32767`).
    pub(crate) imid: i32,
    /// Fixed-point Q15 sine weight `sin(theta)` for the side / second half (`0..=32767`).
    pub(crate) iside: i32,
    /// Bit allocation differential between mid and side in Q3 (`1/8`-bit) units.
    pub(crate) delta: i32,
    /// Quantized split angle in Q14 (`0..=16384`, corresponding to `[0, pi / 2]`).
    pub(crate) itheta: i32,
    /// Fractional Q3 bits consumed from the range coder to decode `itheta`.
    pub(crate) qalloc: i32,
}

impl SplitAngle {
    /// Builds the weights and the bit differential of a vector of `n` bins split at `itheta`
    /// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
    fn from_theta(itheta: i32, n: usize, invert_side: bool, qalloc: i32) -> Self {
        let (imid, iside, delta) = match itheta {
            0 => (Q15_ONE, 0, -THETA_MAX_Q14),
            THETA_MAX_Q14 => (0, Q15_ONE, THETA_MAX_Q14),
            _ => {
                let im = i32::from(bitexact_cos(itheta as i16));
                let is = i32::from(bitexact_cos((THETA_MAX_Q14 - itheta) as i16));
                // `(n - 1)` in Q7 times the Q11 log-tangent gives the Q3 differential.
                let dl = q15_mul_round(((n as i32) - 1) << 7, bitexact_log2tan(is, im));
                (im, is, dl)
            }
        };
        return Self { invert_side, imid, iside, delta, itheta, qalloc };
    }

    /// Linear mid and side gains `(cos theta, sin theta)` as `f32`.
    #[inline]
    fn gains(&self) -> (f32, f32) {
        return ((self.imid as f32) * Q15_SCALE, (self.iside as f32) * Q15_SCALE);
    }

    /// Restricts the folding mask `fill` over `2 * blocks` sub-blocks to the half that carries
    /// energy: the first half at `theta = 0`, the second at `theta = pi / 2`.
    #[inline]
    fn restrict_fill(&self, fill: u32, blocks: usize) -> u32 {
        let first_half = (1u32 << blocks) - 1;
        return match self.itheta {
            0 => fill & first_half,
            THETA_MAX_Q14 => fill & (first_half << blocks),
            _ => fill,
        };
    }
}

/// Per-band state tracked during recursive PVQ shape decoding ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>)).
#[derive(Debug, Clone)]
pub(crate) struct BandDecoderCtx {
    /// Current Bark band index (`0..21`).
    pub(crate) band: usize,
    /// First band index at which stereo intensity coding begins (`0..=21`).
    pub(crate) intensity_band: usize,
    /// Spectral spreading rotation mode for the current frame.
    pub(crate) spread: Spread,
    /// Time-frequency resolution change for the current band.
    pub(crate) tf_change: i32,
    /// Remaining frame bit budget in Q3 (`1/8`-bit) units.
    pub(crate) remaining_bits: i32,
    /// Linear congruential generator state for spectral folding and noise fill.
    pub(crate) seed: u32,
}

/// What one recursive shape-decoding step receives besides its coefficients: the vector
/// geometry (`n` bins over `blocks` short MDCT blocks at pulse-cache level `lm`), its
/// eighth-bit budget, the folding fill mask inherited from the lower bands and the target gain
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
#[derive(Debug, Clone, Copy)]
struct Partition {
    /// Number of MDCT bins.
    n: usize,
    /// Number of short MDCT blocks the bins are interleaved over.
    blocks: usize,
    /// Pulse-cache level (`LM`, decremented by one per split, down to `-1`).
    lm: CacheLevel,
    /// Shape budget in eighth bits.
    bits: i32,
    /// Per-block mask of the folding source blocks that carried energy.
    fill: u32,
    /// Euclidean norm the decoded vector is scaled to.
    gain: f32,
}

/// Folding history of one band: the normalised lower-band vector it folds from and the slot of
/// the history it writes for the bands above it (absent on the last band and the side channel)
/// ([RFC 6716 Section 4.3.4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.2>)).
#[derive(Default)]
struct Lowband<'a> {
    /// Normalised lower-band coefficients to fold from.
    source: Option<&'a [f32]>,
    /// Slot of the folding history that receives this band's normalised shape.
    out: Option<&'a mut [f32]>,
}

/// Decodes the step PDF of the split angle: weight [`STEP_PDF_WEIGHT`] up to `qn / 2`, weight
/// `1` above, inverted in closed form
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
fn decode_step_pdf(dec: &mut Decoder<'_>, qn: i32) -> i32 {
    let x0 = qn / 2;
    let knee = STEP_PDF_WEIGHT * (x0 + 1);
    return dec.decode_with((knee + x0) as u32, |frequency| {
        let fs = frequency as i32;
        let x = if fs < knee { fs / STEP_PDF_WEIGHT } else { x0 + 1 + (fs - knee) };
        let (fl, fh) = if x <= x0 {
            (STEP_PDF_WEIGHT * x, STEP_PDF_WEIGHT * (x + 1))
        } else {
            (x - 1 - x0 + knee, x - x0 + knee)
        };
        return (x, fl as u32, fh as u32);
    });
}

/// Decodes the symmetric triangular PDF of the split angle peaking at `qn / 2`, inverted in
/// closed form with an integer square root
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
fn decode_triangular_pdf(dec: &mut Decoder<'_>, qn: i32) -> i32 {
    let half_qn = qn >> 1;
    let ft = (half_qn + 1) * (half_qn + 1);
    let rising = (half_qn * (half_qn + 1)) >> 1;
    return dec.decode_with(ft as u32, |frequency| {
        let fm = frequency as i32;
        let (x, fl, fs) = if fm < rising {
            let x = ((((8 * fm + 1) as u32).isqrt() as i32) - 1) >> 1;
            (x, (x * (x + 1)) >> 1, x + 1)
        } else {
            let x = (2 * (qn + 1) - (((8 * (ft - fm - 1) + 1) as u32).isqrt() as i32)) >> 1;
            (x, ft - (((qn + 1 - x) * (qn + 2 - x)) >> 1), qn + 1 - x)
        };
        return (x, fl as u32, (fl + fs) as u32);
    });
}

/// Decodes the quantized split angle symbol in `0..=qn` with the step PDF (stereo, `n > 2`),
/// the uniform PDF (`b0 > 1`, or stereo `n = 2`) or the triangular PDF (`b0 = 1`, partition)
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
fn decode_theta_symbol(
    dec: &mut Decoder<'_>,
    qn: i32,
    n: usize,
    b0: usize,
    kind: SplitKind,
) -> Result<i32> {
    return match (kind, n > 2, b0 > 1) {
        (SplitKind::Stereo, true, _) => Ok(decode_step_pdf(dec, qn)),
        (SplitKind::Stereo, false, _) | (SplitKind::Partition, _, true) => {
            Ok(dec.decode_uint((qn + 1) as u32)? as i32)
        }
        (SplitKind::Partition, _, false) => Ok(decode_triangular_pdf(dec, qn)),
    };
}

/// Decodes the split angle `theta` of a vector of `n` bins with `bits` eighth bits at
/// pulse-cache level `lm` in a band of `b0` short blocks
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
fn decode_split_angle(
    ctx: &BandDecoderCtx,
    dec: &mut Decoder<'_>,
    n: usize,
    bits: i32,
    lm: CacheLevel,
    b0: usize,
    kind: SplitKind,
) -> Result<SplitAngle> {
    let pulse_cap = i32::from(LOGN[ctx.band]) + lm.exponent() * (1 << BITRES);
    let offset = (pulse_cap >> 1)
        - match (kind, n) {
            (SplitKind::Stereo, 2) => QTHETA_OFFSET_TWOPHASE,
            _ => QTHETA_OFFSET,
        };
    let intensity_coded = kind == SplitKind::Stereo && ctx.band >= ctx.intensity_band;
    let qn = if intensity_coded { 1 } else { compute_qn(n, bits, offset, pulse_cap, kind) };

    let tell = dec.tell_frac() as i32;
    let mut invert_side = false;
    let itheta = match (qn != 1, kind) {
        (true, _) => (decode_theta_symbol(dec, qn, n, b0, kind)? * THETA_MAX_Q14) / qn,
        (false, SplitKind::Stereo) => {
            if bits > (2 << BITRES) && ctx.remaining_bits > (2 << BITRES) {
                invert_side = dec.decode_symbol_logp(2)? != 0;
            }
            0
        }
        (false, SplitKind::Partition) => 0,
    };
    let qalloc = (dec.tell_frac() as i32) - tell;
    return Ok(SplitAngle::from_theta(itheta, n, invert_side, qalloc));
}

/// Decodes a single-bin (`n = 1`) mono or stereo band: one sign per channel
/// ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>)).
fn quant_band_n1(
    ctx: &mut BandDecoderCtx,
    dec: &mut Decoder<'_>,
    band: Planes<'_>,
    lowband: Lowband<'_>,
) -> Result<u32> {
    let Planes { left: x, right: y } = band;
    let mut decode_sign = |chan: &mut [f32]| -> Result<()> {
        let mut sign = 0u32;
        if ctx.remaining_bits >= (1 << BITRES) {
            sign = dec.decode_raw_bits(1)?;
            ctx.remaining_bits -= 1 << BITRES;
        }
        chan[0] = if sign != 0 { -1.0_f32 } else { 1.0_f32 };
        return Ok(());
    };

    decode_sign(x)?;
    if let Some(y_chan) = y {
        decode_sign(y_chan)?;
    }
    if let Some(out) = lowband.out {
        out[0] = x[0];
    }
    return Ok(1);
}

/// One half of a split vector handed to [`decode_two_halves`].
struct Half<'a> {
    /// Coefficients of the half.
    x: &'a mut [f32],
    /// Folding source and output of the half.
    lowband: Lowband<'a>,
    /// Geometry, budget, fill and gain of the half.
    part: Partition,
    /// Left shift of the half's collapse mask into the band's mask.
    mask_shift: usize,
}

/// Decodes the `mid` and `side` halves of a split, the larger budget first, handing the bits the
/// first half left unused beyond [`REBALANCE_THRESHOLD_Q3`] to the second unless `itheta` put
/// all energy in the first half; returns the combined collapse mask
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
fn decode_two_halves<F>(
    ctx: &mut BandDecoderCtx,
    dec: &mut Decoder<'_>,
    mid: Half<'_>,
    side: Half<'_>,
    itheta: i32,
    mut decode: F,
) -> Result<u32>
where
    F: FnMut(
        &mut BandDecoderCtx,
        &mut Decoder<'_>,
        &mut [f32],
        Lowband<'_>,
        Partition,
    ) -> Result<u32>,
{
    let (first, mut second, theta_guard) =
        if mid.part.bits >= side.part.bits { (mid, side, 0) } else { (side, mid, THETA_MAX_Q14) };

    let before = ctx.remaining_bits;
    let cm_first = decode(ctx, dec, first.x, first.lowband, first.part)? << first.mask_shift;
    let rebalance = first.part.bits - (before - ctx.remaining_bits);
    if rebalance > REBALANCE_THRESHOLD_Q3 && itheta != theta_guard {
        second.part.bits += rebalance - REBALANCE_THRESHOLD_Q3;
    }
    let cm_second = decode(ctx, dec, second.x, second.lowband, second.part)? << second.mask_shift;
    return Ok(cm_first | cm_second);
}

/// Bit differential of a split in a short-block band (`b0 > 1`): extra bits go to the
/// low-energy half to mitigate pre-echo and forward masking
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
fn short_block_delta(split: &SplitAngle, b0: usize, next_lm: CacheLevel, half_n: usize) -> i32 {
    if b0 <= 1 || (split.itheta & THETA_INTERIOR_MASK_Q14) == 0 {
        return split.delta;
    }
    if split.itheta > THETA_HALF_Q14 {
        return split.delta - (split.delta >> (4 - next_lm.exponent()));
    }
    return 0.min(split.delta + (((half_n as i32) << BITRES) >> (5 - next_lm.exponent())));
}

/// Splits the partition `x` in two around a decoded angle and decodes both halves recursively
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
fn decode_split_partition(
    ctx: &mut BandDecoderCtx,
    dec: &mut Decoder<'_>,
    x: &mut [f32],
    lowband: Option<&[f32]>,
    part: Partition,
    next_lm: CacheLevel,
) -> Result<u32> {
    let Partition { n, blocks: b0, mut bits, mut fill, gain, .. } = part;
    let half_n = n >> 1;
    if b0 == 1 {
        fill = (fill & 1) | (fill << 1);
    }
    let half_blocks = (b0 + 1) >> 1;

    let split = decode_split_angle(ctx, dec, half_n, bits, next_lm, b0, SplitKind::Partition)?;
    fill = split.restrict_fill(fill, half_blocks);
    bits -= split.qalloc;
    let (mid, side) = split.gains();
    let delta = short_block_delta(&split, b0, next_lm, half_n);
    let mbits = 0.max(bits.min((bits - delta) / 2));
    let sbits = bits - mbits;
    ctx.remaining_bits -= split.qalloc;

    let (x_first, x_second) = x[..n].split_at_mut(half_n);
    let (lb_first, lb_second) = lowband.map(|lb| lb[..n].split_at(half_n)).unzip();
    let half = Partition { n: half_n, blocks: half_blocks, lm: next_lm, ..part };
    let first = Half {
        x: x_first,
        lowband: Lowband { source: lb_first, out: None },
        part: Partition { bits: mbits, fill, gain: gain * mid, ..half },
        mask_shift: 0,
    };
    let second = Half {
        x: x_second,
        lowband: Lowband { source: lb_second, out: None },
        part: Partition { bits: sbits, fill: fill >> half_blocks, gain: gain * side, ..half },
        mask_shift: b0 >> 1,
    };
    return decode_two_halves(
        ctx,
        dec,
        first,
        second,
        split.itheta,
        |ctx, dec, x, lowband, part| {
            return quant_partition(ctx, dec, x, lowband.source, part);
        },
    );
}

/// Maps the budget `bits` of a leaf partition at level `lm` to a pseudo-pulse index through the
/// pulse cache, lowering it while the frame budget is overrun
/// ([RFC 6716 Section 4.3.4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.1>)).
fn select_pulse_index(ctx: &mut BandDecoderCtx, lm: CacheLevel, bits: i32) -> i32 {
    let mut q = bits2pulses_at_level(lm, ctx.band, bits);
    let mut curr_bits = pulses2bits_at_level(lm, ctx.band, q);
    ctx.remaining_bits -= curr_bits;
    while ctx.remaining_bits < 0 && q > 0 {
        ctx.remaining_bits += curr_bits;
        q -= 1;
        curr_bits = pulses2bits_at_level(lm, ctx.band, q);
        ctx.remaining_bits -= curr_bits;
    }
    return q;
}

/// Fills a zero-pulse partition `x` by folding `lowband` with a pseudo-random jitter, or with
/// generator noise when there is no folding source; returns the collapse mask
/// ([RFC 6716 Section 4.3.4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.2>)).
fn fold_partition(
    ctx: &mut BandDecoderCtx,
    x: &mut [f32],
    lowband: Option<&[f32]>,
    part: Partition,
) -> u32 {
    let Partition { n, blocks, fill, gain, .. } = part;
    let cm_mask = (1u32 << blocks) - 1;
    let fill = fill & cm_mask;
    if fill == 0 {
        x[..n].fill(0.0_f32);
        return 0;
    }

    let Some(lb) = lowband else {
        for dst in &mut x[..n] {
            ctx.seed = lcg_next(ctx.seed);
            *dst = ((ctx.seed as i32) >> LCG_NOISE_SHIFT) as f32;
        }
        renormalise_vector(&mut x[..n], n, gain);
        return cm_mask;
    };
    for (dst, &src) in x[..n].iter_mut().zip(lb[..n].iter()) {
        ctx.seed = lcg_next(ctx.seed);
        let jitter = if (ctx.seed & LCG_SIGN_BIT) != 0 { FOLD_JITTER } else { -FOLD_JITTER };
        *dst = src + jitter;
    }
    renormalise_vector(&mut x[..n], n, gain);
    return fill;
}

/// Recursively decodes a mono partition `x`: split while the budget exceeds the pulse cache,
/// otherwise a pulse vector, or a folded or noise-filled vector without pulses
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
fn quant_partition(
    ctx: &mut BandDecoderCtx,
    dec: &mut Decoder<'_>,
    x: &mut [f32],
    lowband: Option<&[f32]>,
    part: Partition,
) -> Result<u32> {
    let Partition { n, blocks, lm, bits, gain, .. } = part;
    // The lowest level is never split; checking it first also keeps the cache lookup in range.
    let split_level =
        lm.split().filter(|_| bits > max_cache_bits(lm, ctx.band) + SPLIT_MARGIN_Q3 && n > 2);
    if let Some(next_lm) = split_level {
        return decode_split_partition(ctx, dec, x, lowband, part, next_lm);
    }

    let q = select_pulse_index(ctx, lm, bits);
    if q != 0 {
        return pvq_unquantize(&mut x[..n], get_pulses(q as usize), ctx.spread, blocks, dec, gain);
    }
    return Ok(fold_partition(ctx, x, lowband, part));
}

/// Time-frequency layout of a band of `n` bins over `initial_blocks` short blocks after the
/// `tf_change` of the band: `recombine` Haar steps merge blocks (`tf_change > 0`), `time_divide`
/// steps split them (`tf_change < 0`)
/// ([RFC 6716 Section 4.3.4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.5>)).
#[derive(Debug, Clone, Copy)]
struct TimeFrequency {
    /// Bins in the band.
    n: usize,
    /// Short blocks of the frame.
    initial_blocks: usize,
    /// Haar recombination steps.
    recombine: usize,
    /// Haar time-division steps.
    time_divide: usize,
    /// Blocks the partition is decoded over.
    blocks: usize,
    /// Bins per decoded block.
    n_b: usize,
}

impl TimeFrequency {
    fn new(n: usize, initial_blocks: usize, tf_change: i32) -> Self {
        let recombine = tf_change.max(0) as usize;
        let divisions = (-tf_change).max(0) as usize;
        let n_b0 = (n / initial_blocks) << recombine;
        let time_divide = (0..divisions).take_while(|&k| ((n_b0 >> k) & 1) == 0).count();
        return Self {
            n,
            initial_blocks,
            recombine,
            time_divide,
            blocks: (initial_blocks >> recombine) << time_divide,
            n_b: n_b0 >> time_divide,
        };
    }

    /// Block order of the de-interleaved band: sequency order for a long-block frame.
    #[inline]
    fn order(&self) -> HadamardOrder {
        if self.initial_blocks == 1 {
            return HadamardOrder::Sequency;
        }
        return HadamardOrder::Natural;
    }

    /// Blocks the band's collapse mask covers after the inverse pass.
    #[inline]
    fn final_blocks(&self) -> usize {
        return (self.blocks >> self.time_divide) << self.recombine;
    }

    /// Brings the folding source `lowband` to the decoded layout: Haar recombination and
    /// time division, then de-interleaving into block order.
    fn fold_source_forward(&self, lowband: &mut [f32]) {
        for k in 0..self.recombine {
            haar_transform(lowband, self.n >> k, 1 << k);
        }
        let blocks = self.initial_blocks >> self.recombine;
        let n_b = (self.n / self.initial_blocks) << self.recombine;
        for k in 0..self.time_divide {
            haar_transform(lowband, n_b >> k, blocks << k);
        }
        if self.blocks > 1 {
            let stride = self.blocks << self.recombine;
            deinterleave_hadamard(lowband, self.n_b >> self.recombine, stride, self.order());
        }
        return;
    }

    /// Brings the folding fill mask `fill` to the decoded layout.
    fn fill_forward(&self, mut fill: u32) -> u32 {
        for _ in 0..self.recombine {
            fill = u32::from(BIT_INTERLEAVE_TABLE[(fill & 0xF) as usize])
                | (u32::from(BIT_INTERLEAVE_TABLE[(fill >> 4) as usize]) << 2);
        }
        let blocks = self.initial_blocks >> self.recombine;
        for k in 0..self.time_divide {
            fill |= fill << (blocks << k);
        }
        return fill;
    }

    /// Returns the decoded band `x` and its collapse mask `cm` to frequency order and the
    /// frame's block layout.
    fn inverse(&self, x: &mut [f32], mut cm: u32) -> u32 {
        if self.blocks > 1 {
            let stride = self.blocks << self.recombine;
            interleave_hadamard(x, self.n_b >> self.recombine, stride, self.order());
        }
        for k in 0..self.time_divide {
            let blocks = self.blocks >> (k + 1);
            cm |= cm >> blocks;
            haar_transform(x, self.n_b << (k + 1), blocks);
        }
        for k in 0..self.recombine {
            cm = u32::from(BIT_DEINTERLEAVE_TABLE[(cm & 0xF) as usize]);
            haar_transform(x, self.n >> k, 1 << k);
        }
        return cm & ((1u32 << self.final_blocks()) - 1);
    }
}

/// Decodes a single mono Bark band `x`, applying time-frequency pre/post-recombination and
/// writing its scaled shape to the folding history
/// ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>)).
fn quant_band(
    ctx: &mut BandDecoderCtx,
    dec: &mut Decoder<'_>,
    x: &mut [f32],
    lowband: Lowband<'_>,
    part: Partition,
) -> Result<u32> {
    let Partition { n, blocks, fill, .. } = part;
    if n == 1 {
        return quant_band_n1(ctx, dec, Planes::new(x, None), lowband);
    }
    let Lowband { source, out } = lowband;
    let tf = TimeFrequency::new(n, blocks, ctx.tf_change);

    let mut lowband_buf = [0.0_f32; MAX_BAND_WIDTH];
    if let Some(src) = source {
        lowband_buf[..n].copy_from_slice(&src[..n]);
        tf.fold_source_forward(&mut lowband_buf[..n]);
    }
    let folded = source.map(|_| &lowband_buf[..n]);
    let decoded = Partition { blocks: tf.blocks, fill: tf.fill_forward(fill), ..part };
    let cm = quant_partition(ctx, dec, &mut x[..n], folded, decoded)?;
    let cm = tf.inverse(&mut x[..n], cm);

    if let Some(out) = out {
        let scale = (n as f32).sqrt();
        for (dst, &src) in out[..n].iter_mut().zip(x[..n].iter()) {
            *dst = scale * src;
        }
    }
    return Ok(cm);
}

/// Unmixes normalized mid (`x[..n]`) and scaled side (`y[..n]`) vectors back into unit-norm
/// left (`x[..n]`) and right (`y[..n]`) channel vectors ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
fn stereo_merge(x: &mut [f32], y: &mut [f32], mid: f32, n: usize) {
    let (xp, side) = y[..n]
        .iter()
        .zip(x[..n].iter())
        .fold((0.0_f32, 0.0_f32), |(xp, side), (&vy, &vx)| (xp + vy * vx, side + vy * vy));

    let xp = xp * mid;
    let el = mid * mid + side - 2.0_f32 * xp;
    let er = mid * mid + side + 2.0_f32 * xp;
    if er < STEREO_MERGE_MIN_ENERGY || el < STEREO_MERGE_MIN_ENERGY {
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

/// Decodes a two-bin stereo band (`n = 2`): one channel carries the shape and a sign, the other
/// is its orthogonal rotation, then both are scaled by the split weights and unmixed
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
fn decode_two_phase_stereo(
    ctx: &mut BandDecoderCtx,
    dec: &mut Decoder<'_>,
    x: &mut [f32],
    y: &mut [f32],
    lowband: Lowband<'_>,
    part: Partition,
    split: &SplitAngle,
) -> Result<u32> {
    let (mid, side) = split.gains();
    let sbits = if split.itheta != 0 && split.itheta != THETA_MAX_Q14 { 1 << BITRES } else { 0 };
    let mbits = part.bits - sbits;
    ctx.remaining_bits -= split.qalloc + sbits;

    let sign_bit = if sbits != 0 { dec.decode_raw_bits(1)? } else { 0 };
    let sign = 1.0_f32 - 2.0_f32 * (sign_bit as f32);
    let primary = Partition { bits: mbits, gain: 1.0_f32, ..part };

    let cm = if split.itheta > THETA_HALF_Q14 {
        let cm = quant_band(ctx, dec, y, lowband, primary)?;
        x[0] = -sign * y[1];
        x[1] = sign * y[0];
        cm
    } else {
        let cm = quant_band(ctx, dec, x, lowband, primary)?;
        y[0] = -sign * x[1];
        y[1] = sign * x[0];
        cm
    };

    for (vx, vy) in x[..2].iter_mut().zip(y[..2].iter_mut()) {
        *vx *= mid;
        *vy *= side;
        let tmp = *vx;
        *vx = tmp - *vy;
        *vy += tmp;
    }
    return Ok(cm);
}

/// Decodes a mid/side stereo band (`n > 2`): mid and side as two mono bands with the budget
/// split by the angle, then unmixed into left and right
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
fn decode_mid_side_stereo(
    ctx: &mut BandDecoderCtx,
    dec: &mut Decoder<'_>,
    x: &mut [f32],
    y: &mut [f32],
    lowband: Lowband<'_>,
    part: Partition,
    split: &SplitAngle,
) -> Result<u32> {
    let Partition { n, blocks, bits, fill, .. } = part;
    let (mid, side) = split.gains();
    let mbits = 0.max(bits.min((bits - split.delta) / 2));
    let sbits = bits - mbits;
    ctx.remaining_bits -= split.qalloc;

    let mid_half = Half {
        x: &mut x[..n],
        lowband,
        part: Partition { bits: mbits, gain: 1.0_f32, ..part },
        mask_shift: 0,
    };
    let side_half = Half {
        x: &mut y[..n],
        lowband: Lowband::default(),
        part: Partition { bits: sbits, fill: fill >> blocks, gain: side, ..part },
        mask_shift: 0,
    };
    let cm = decode_two_halves(ctx, dec, mid_half, side_half, split.itheta, quant_band)?;
    stereo_merge(x, y, mid, n);
    return Ok(cm);
}

/// Decodes a coupled stereo Bark band (`x` and `y`) using mid/side or intensity
/// stereo splitting ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>),
/// [RFC 8251 Section 10](<https://datatracker.ietf.org/doc/html/rfc8251#section-10>)).
fn quant_band_stereo(
    ctx: &mut BandDecoderCtx,
    dec: &mut Decoder<'_>,
    x: &mut [f32],
    y: &mut [f32],
    lowband: Lowband<'_>,
    part: Partition,
    phase_inversion: bool,
) -> Result<u32> {
    let Partition { n, blocks, lm, bits, fill, .. } = part;
    if n == 1 {
        return quant_band_n1(ctx, dec, Planes::new(x, Some(y)), lowband);
    }

    let split = decode_split_angle(ctx, dec, n, bits, lm, blocks, SplitKind::Stereo)?;
    let bits = bits - split.qalloc;
    let cm = match n {
        2 => {
            let two_phase = Partition { bits, ..part };
            decode_two_phase_stereo(ctx, dec, x, y, lowband, two_phase, &split)?
        }
        _ => {
            let mid_side = Partition { bits, fill: split.restrict_fill(fill, blocks), ..part };
            decode_mid_side_stereo(ctx, dec, x, y, lowband, mid_side, &split)?
        }
    };

    if split.invert_side && phase_inversion {
        y[..n].iter_mut().for_each(|vy| *vy = -*vy);
    }
    return Ok(cm);
}

/// Frame-wide inputs of the band shape decoding shared by every band
/// ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>),
/// [RFC 8251 Section 10](<https://datatracker.ietf.org/doc/html/rfc8251#section-10>)).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ShapeFrame {
    /// First coded Bark band.
    pub(crate) start: usize,
    /// One past the last coded Bark band.
    pub(crate) end: usize,
    /// MDCT block size `LM` of the frame.
    pub(crate) lm: BlockSize,
    /// Short MDCT blocks `B` the shapes are interleaved over: `1`, or `2^LM` in a transient frame.
    pub(crate) blocks: usize,
    /// Spreading rotation decision of the frame.
    pub(crate) spread: Spread,
    /// Time-frequency change per band.
    pub(crate) tf_res: [i32; MAX_BANDS],
    /// Frame budget in eighth bits available to the shapes.
    pub(crate) total_bits: i32,
    /// Whether the 180-degree phase shift of intensity stereo is applied to the side channel
    /// ([RFC 8251 Section 10](<https://datatracker.ietf.org/doc/html/rfc8251#section-10>)).
    pub(crate) phase_inversion: bool,
}

/// Normalised shapes of the bands decoded so far per channel: the folding source of the bands
/// above them ([RFC 6716 Section 4.3.4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.2>)).
struct FoldingHistory {
    norm: [[f32; MAX_NORM_BUFFER_SIZE]; MAX_CHANNELS],
}

impl FoldingHistory {
    /// Duplicates the tail of the first Hybrid band's history (`start = 17`, width `8M`) so the
    /// second Hybrid band (width `12M`) has a complete lowband source, for both channels when
    /// `dual_stereo` ([RFC 8251 Section 9](<https://datatracker.ietf.org/doc/html/rfc8251#section-9>)).
    fn fold_first_hybrid_band(&mut self, start: usize, m: usize, dual_stereo: bool) {
        if start + 2 > MAX_BANDS {
            return;
        }
        let n1 = m * (EBANDS[start + 1] - EBANDS[start]);
        let n2 = m * (EBANDS[start + 2] - EBANDS[start + 1]);
        if n2 <= n1 || 2 * n1 < n2 {
            return;
        }
        let src_start = 2 * n1 - n2;
        let copy_len = n2 - n1;
        Planes::from_rows(&mut self.norm, dual_stereo)
            .each_mut()
            .for_each(|plane| plane.copy_within(src_start..(src_start + copy_len), n1));
        return;
    }

    /// Averages the two channel histories over `len` bins when dual stereo ends at the intensity
    /// band ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
    fn merge_channels(&mut self, len: usize) {
        let [norm_x, norm_y] = &mut self.norm;
        norm_x[..len]
            .iter_mut()
            .zip(norm_y[..len].iter())
            .for_each(|(n1, &n2)| *n1 = 0.5_f32 * (*n1 + n2));
        return;
    }

    /// Copies the folding source of a band of `n` bins into `scratch` (so the band can write its
    /// own shape at `write` while folding) and returns per channel its source and output slot.
    fn band_lowbands<'b>(
        &'b mut self,
        scratch: &'b mut [[f32; MAX_BAND_WIDTH]; MAX_CHANNELS],
        fold: &FoldSource,
        n: usize,
        write: Option<usize>,
        dual_stereo: bool,
    ) -> [Lowband<'b>; 2] {
        if let Some(offset) = fold.lowband {
            scratch[0][..n].copy_from_slice(&self.norm[0][offset..(offset + n)]);
            if dual_stereo {
                scratch[1][..n].copy_from_slice(&self.norm[1][offset..(offset + n)]);
            }
        }
        let [norm_x, norm_y] = &mut self.norm;
        let [src_x, src_y] = &*scratch;
        return [
            Lowband {
                source: fold.lowband.map(|_| &src_x[..n]),
                out: write.map(|w| &mut norm_x[w..(w + n)]),
            },
            Lowband {
                source: fold.lowband.map(|_| &src_y[..n]),
                out: write.map(|w| &mut norm_y[w..(w + n)]),
            },
        ];
    }
}

/// Where a band folds its zero-pulse partitions from: the offset of the source in the folding
/// history, and per channel the collapse masks of the bands the source spans (the full mask
/// when the band is noise-filled instead)
/// ([RFC 6716 Section 4.3.4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.2>)).
struct FoldSource {
    /// Offset of the folding source in the folding history, `None` for noise filling.
    lowband: Option<usize>,
    /// Fill mask of the first channel.
    x_mask: u32,
    /// Fill mask of the second channel.
    y_mask: u32,
}

/// Bookkeeping carried from one band to the next while the shapes of a frame are decoded:
/// the recursive-decoding context, the bit balance, the stereo coding in force and which lower
/// band the folding reads from
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>),
/// [Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>)).
struct BandSweep<'a> {
    frame: &'a ShapeFrame,
    alloc: &'a BandAllocation,
    ctx: BandDecoderCtx,
    /// Eighth bits left over by the bands decoded so far, shared out to the bands that follow.
    balance: i32,
    /// Whether the two channels are still coded independently (cleared at the intensity band).
    dual_stereo: bool,
    /// Band whose normalised shape the folding reads from (`0` before any band qualifies).
    lowband_offset: usize,
    /// Whether the last band spent enough bits for its shape to serve as a folding source.
    update_lowband: bool,
}

impl<'a> BandSweep<'a> {
    fn new(frame: &'a ShapeFrame, alloc: &'a BandAllocation, seed: u32) -> Self {
        return Self {
            frame,
            alloc,
            ctx: BandDecoderCtx {
                band: frame.start,
                intensity_band: alloc.intensity,
                spread: frame.spread,
                tf_change: 0,
                remaining_bits: 0,
                seed,
            },
            balance: alloc.balance,
            dual_stereo: alloc.dual_stereo,
            lowband_offset: 0,
            update_lowband: true,
        };
    }

    /// Positions the context on band `i` and returns `(tell, bits)`: the range coder position in
    /// eighth bits and the band's shape budget
    /// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
    fn begin_band(&mut self, dec: &Decoder<'_>, i: usize) -> (i32, i32) {
        let tell = dec.tell_frac() as i32;
        if i != self.frame.start {
            self.balance -= tell;
        }
        let remaining_bits = self.frame.total_bits - tell - 1;
        self.ctx.band = i;
        self.ctx.remaining_bits = remaining_bits;
        self.ctx.tf_change = self.frame.tf_res[i];
        if i >= self.alloc.coded_bands {
            return (tell, 0);
        }
        let divisor = BALANCE_SHARE_BANDS.min((self.alloc.coded_bands - i) as i32);
        let curr_balance = self.balance / divisor;
        let bits = 0.max(
            MAX_BAND_BITS_Q3.min((remaining_bits + 1).min(self.alloc.pulses[i] + curr_balance)),
        );
        return (tell, bits);
    }

    /// Makes band `i` (`n` bins from `band_start`) the folding source of the bands above it when
    /// it lies a full band width above the history start (or is the second Hybrid band) and the
    /// previous source may be replaced.
    fn select_lowband(&mut self, i: usize, band_start: usize, norm_offset: usize, n: usize) {
        let reaches_down = band_start >= norm_offset + n || i == self.frame.start + 1;
        if reaches_down && (self.update_lowband || self.lowband_offset == 0) {
            self.lowband_offset = i;
        }
        return;
    }

    /// Locates the folding source of band `i` (`n` bins) and the collapse masks of the bands it
    /// spans, or the full fill mask when the band is noise-filled
    /// ([RFC 6716 Section 4.3.4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.2>)).
    fn folding_source(
        &self,
        i: usize,
        n: usize,
        collapse_masks: &[u8],
        channels: usize,
    ) -> FoldSource {
        let ShapeFrame { start, lm, blocks, spread, .. } = *self.frame;
        let folds = self.lowband_offset != 0
            && (spread != Spread::Aggressive || blocks > 1 || self.ctx.tf_change < 0);
        if !folds {
            let full_mask = (1u32 << blocks) - 1;
            return FoldSource { lowband: None, x_mask: full_mask, y_mask: full_mask };
        }

        let m = lm.blocks();
        let norm_offset = m * EBANDS[start];
        let offset = (m * EBANDS[self.lowband_offset]).saturating_sub(norm_offset + n);
        let fold_bin = offset + norm_offset;
        // First band whose bins the folding source reaches down into.
        let fold_start = (0..=self.lowband_offset)
            .rev()
            .find(|&j| j == 0 || m * EBANDS[j - 1] <= fold_bin)
            .unwrap_or(0)
            .saturating_sub(1);
        // One past the last band whose bins the folding source reaches up into.
        let fold_end = ((self.lowband_offset - 1)..i)
            .find(|&j| j + 1 >= i || m * EBANDS[j + 1] >= fold_bin + n)
            .unwrap_or(i - 1)
            + 1;
        let (x_mask, y_mask) = (fold_start..fold_end).fold((0u32, 0u32), |(x_mask, y_mask), j| {
            let x = u32::from(collapse_masks[j * channels]);
            let y = u32::from(collapse_masks[j * channels + channels - 1]);
            return (x_mask | x, y_mask | y);
        });
        return FoldSource { lowband: Some(offset), x_mask, y_mask };
    }

    /// Decodes the shapes of one band: both channels independently in dual stereo, mid/side for
    /// a coupled band, or the single mono channel; returns the collapse masks `(x, y)`
    /// ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>)).
    fn decode_band_shapes(
        &mut self,
        dec: &mut Decoder<'_>,
        band: Planes<'_>,
        lowbands: [Lowband<'_>; 2],
        (n, bits): (usize, i32),
        fold: &FoldSource,
    ) -> Result<(u32, u32)> {
        let Planes { left: x, right: y } = band;
        let [x_lowband, y_lowband] = lowbands;
        let part = Partition {
            n,
            blocks: self.frame.blocks,
            lm: CacheLevel::from_block_size(self.frame.lm),
            bits,
            fill: fold.x_mask | fold.y_mask,
            gain: 1.0_f32,
        };
        let ctx = &mut self.ctx;
        // Dual stereo is only coded for two channels, so a mono band ignores the flag
        // ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
        return match (y, self.dual_stereo) {
            (Some(y), true) => {
                let x_part = Partition { bits: bits / 2, fill: fold.x_mask, ..part };
                let y_part = Partition { bits: bits / 2, fill: fold.y_mask, ..part };
                let x_cm = quant_band(ctx, dec, x, x_lowband, x_part)?;
                let y_cm = quant_band(ctx, dec, y, y_lowband, y_part)?;
                Ok((x_cm, y_cm))
            }
            (Some(y), false) => {
                let cm =
                    quant_band_stereo(ctx, dec, x, y, x_lowband, part, self.frame.phase_inversion)?;
                Ok((cm, cm))
            }
            (None, _) => {
                let cm = quant_band(ctx, dec, x, x_lowband, part)?;
                Ok((cm, cm))
            }
        };
    }

    /// Returns the band's unused budget to the balance and records whether its shape may serve
    /// as a folding source ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
    fn end_band(&mut self, i: usize, tell: i32, bits: i32, n: usize) {
        self.balance += self.alloc.pulses[i] + tell;
        self.update_lowband = bits > ((n as i32) << BITRES);
        return;
    }
}

/// Unquantizes the shapes of all bands `frame.start..frame.end` of a mono or stereo CELT frame
/// into `spectra`, recording the per-band, per-channel collapse masks and advancing the folding
/// noise `seed` ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>)).
pub(crate) fn unquantize_bands(
    frame: &ShapeFrame,
    alloc: &BandAllocation,
    dec: &mut Decoder<'_>,
    mut spectra: Planes<'_>,
    collapse_masks: &mut [u8],
    seed: &mut u32,
) -> Result<()> {
    let ShapeFrame { start, end, lm, .. } = *frame;
    let m = lm.blocks();
    let norm_offset = m * EBANDS[start];
    let channels = if spectra.is_stereo() { 2usize } else { 1usize };
    let mut history = FoldingHistory { norm: [[0.0_f32; MAX_NORM_BUFFER_SIZE]; MAX_CHANNELS] };
    let mut sweep = BandSweep::new(frame, alloc, *seed);

    for i in start..end {
        let band_start = m * EBANDS[i];
        let n = band_width(i, lm);
        let (tell, bits) = sweep.begin_band(dec, i);
        sweep.select_lowband(i, band_start, norm_offset, n);
        if i == start + 1 {
            history.fold_first_hybrid_band(start, m, sweep.dual_stereo);
        }
        let fold = sweep.folding_source(i, n, collapse_masks, channels);
        if sweep.dual_stereo && i == alloc.intensity {
            sweep.dual_stereo = false;
            history.merge_channels(band_start - norm_offset);
        }

        let mut scratch = [[0.0_f32; MAX_BAND_WIDTH]; MAX_CHANNELS];
        let write = (i != end - 1).then_some(band_start - norm_offset);
        let lowbands = history.band_lowbands(&mut scratch, &fold, n, write, sweep.dual_stereo);
        let band = spectra.range(band_start..(band_start + n));
        let (x_cm, y_cm) = sweep.decode_band_shapes(dec, band, lowbands, (n, bits), &fold)?;

        collapse_masks[i * channels] = x_cm as u8;
        collapse_masks[i * channels + channels - 1] = y_cm as u8;
        sweep.end_band(i, tell, bits, n);
    }

    *seed = sweep.ctx.seed;
    return Ok(());
}

/// Noise amplitude for the collapsed blocks of band `i` of channel `c`: the band's energy rise
/// over the two previous frames, capped by `thresh`, scaled to one block
/// ([RFC 6716 Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)).
fn collapse_noise_level(
    energy: &BandEnergyHistory,
    channels: usize,
    c: usize,
    i: usize,
    lm: BlockSize,
    thresh: f32,
    sqrt_1: f32,
) -> f32 {
    let mut prev1 = energy.previous[c][i];
    let mut prev2 = energy.two_back[c][i];
    if channels == 1 {
        prev1 = prev1.max(energy.previous[1][i]);
        prev2 = prev2.max(energy.two_back[1][i]);
    }
    let ediff = (energy.current[c][i] - prev1.min(prev2)).max(0.0_f32);

    let mut r = 2.0_f32 * exp2(-ediff);
    if lm.log2() == MAX_LM {
        r *= SQRT_2;
    }
    return thresh.min(r) * sqrt_1;
}

/// Prevents energy collapse across short MDCT sub-blocks during transient frames by injecting
/// pseudorandom noise into sub-blocks that received zero pulses and renormalizing the band
/// ([RFC 6716 Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)).
pub(crate) fn anti_collapse(
    x: &mut [f32],
    collapse_masks: &[u8],
    frame: &ShapeFrame,
    channels: usize,
    energy: &BandEnergyHistory,
    pulses: &[i32; MAX_BANDS],
    mut seed: u32,
) {
    let ShapeFrame { start, end, lm, .. } = *frame;
    let blocks = lm.blocks();
    let size = lm.frame_samples();

    for i in start..end {
        let n0 = EBANDS[i + 1] - EBANDS[i];
        let depth = ((1 + pulses[i]) / (n0 as i32)) >> lm.log2();
        let thresh = 0.5_f32 * exp2(-COLLAPSE_DEPTH_DECAY * (depth as f32));
        let sqrt_1 = 1.0_f32 / (lm.scale(n0) as f32).sqrt();

        for c in 0..channels {
            let r = collapse_noise_level(energy, channels, c, i, lm, thresh, sqrt_1);
            let band_offset = c * size + lm.scale(EBANDS[i]);
            let band_slice = &mut x[band_offset..(band_offset + lm.scale(n0))];
            let mask = collapse_masks[i * channels + c];

            let mut any_collapsed = false;
            for k in (0..blocks).filter(|&k| (mask & (1u8 << k)) == 0) {
                any_collapsed = true;
                for coeff in band_slice.iter_mut().skip(k).step_by(blocks) {
                    seed = lcg_next(seed);
                    *coeff = if (seed & LCG_SIGN_BIT) != 0 { r } else { -r };
                }
            }
            if any_collapsed {
                renormalise_vector(band_slice, lm.scale(n0), 1.0_f32);
            }
        }
    }
    return;
}

#[cfg(test)]
#[allow(clippy::excessive_precision)] // recorded values keep the 9 significant digits of the dump
mod tests {
    // Fixtures: packets and expected values recorded from libopus 1.6.1. The behaviour they pin is
    // normative (RFC 6716 Section 4.3.4) and is verified against the RFC 8251 test vectors by
    // `tests/rfc6716_conformance.rs`.
    use super::*;

    /// Applies the forward spreading rotation (the encoder direction), used only to check that
    /// the decoder's inverse rotation undoes it.
    fn spreading_rotation_forward(x: &mut [f32], blocks: usize, pulses: usize, spread: Spread) {
        let Some(SpreadingRotation { c, s, stride2, sub_len }) =
            SpreadingRotation::new(x.len(), blocks, pulses, spread)
        else {
            return;
        };
        for chunk in x.chunks_exact_mut(sub_len) {
            rotation_sweep(chunk, 1, c, -s);
            if stride2 > 0 {
                rotation_sweep(chunk, stride2, s, -c);
            }
        }
        return;
    }

    #[test]
    #[allow(clippy::approx_constant)] // the literal is the value being proven equal to the constant
    fn std_consts_are_bit_identical_to_reference_literals() {
        assert_eq!(std::f64::consts::LN_2.to_bits(), 0.6931471805599453094_f64.to_bits());
        assert_eq!(
            std::f64::consts::FRAC_PI_2.to_bits(),
            (0.5_f64 * std::f64::consts::PI).to_bits()
        );
        assert_eq!(Q15_SCALE.to_bits(), (1.0_f32 / 32768.0_f32).to_bits());
        assert_eq!(FOLD_JITTER.to_bits(), (1.0_f32 / 256.0_f32).to_bits());
        return;
    }

    #[test]
    fn spreading_rotation_matches_recorded_values() {
        let mut x = [0.0_f32; 16];
        for (i, slot) in x.iter_mut().enumerate() {
            *slot = ((i + 1) as f32) * 0.1_f32;
        }
        renormalise_vector(&mut x, 16, 1.0_f32);
        spreading_rotation(&mut x, 2, 5, Spread::Normal);

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
        spreading_rotation_forward(&mut roundtrip, 2, 5, Spread::Normal);
        spreading_rotation(&mut roundtrip, 2, 5, Spread::Normal);
        for (i, (&got, &exp)) in roundtrip.iter().zip(expected.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "exp_rotation roundtrip mismatch at {i}: got {got}, expected {exp}"
            );
        }
        return;
    }

    #[test]
    fn pvq_unquantize_matches_recorded_values() {
        let packet: [u8; 16] = [
            0x43, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x01, 0x58,
        ];
        let mut dec = Decoder::new(&packet);
        let mut x = [0.0_f32; 8];
        let cm = pvq_unquantize(&mut x, 7, Spread::Normal, 2, &mut dec, 1.0_f32)
            .expect("pvq_unquantize succeeds");

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

    /// A [`BandAllocation`] with only the fields the shape decoding reads.
    fn allocation(
        pulses: [i32; MAX_BANDS],
        coded_bands: usize,
        intensity: usize,
        dual_stereo: bool,
    ) -> BandAllocation {
        return BandAllocation {
            pulses,
            fine_quant: [0; MAX_BANDS],
            fine_priority: [false; MAX_BANDS],
            balance: 0,
            coded_bands,
            intensity,
            dual_stereo,
        };
    }

    #[test]
    fn band_shapes_anti_collapse_and_denormalisation_match_recorded_values() {
        let qbuf: [u8; 32] = [
            0x31, 0x66, 0xd7, 0x3e, 0x26, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];
        let pulses: [i32; MAX_BANDS] =
            [40, 36, 32, 28, 24, 24, 20, 16, 32, 28, 20, 12, 8, 0, 0, 0, 0, 0, 0, 0, 0];
        let tf_res: [i32; MAX_BANDS] =
            [0, 0, 0, 0, 0, 0, 0, 0, 1, 0, -1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let frame = ShapeFrame {
            start: 0,
            end: 13,
            lm: BlockSize::new(1),
            blocks: 2,
            spread: Spread::Normal,
            tf_res,
            total_bits: 32 * 8 * 8,
            phase_inversion: true,
        };
        let alloc = allocation(pulses, 12, 10, false);

        let mut dec = Decoder::new(&qbuf);
        let mut x_dec = [0.0_f32; 240];
        let mut y_dec = [0.0_f32; 240];
        let mut cm_masks = [0u8; 42];
        let mut seed = 12345u32;

        unquantize_bands(
            &frame,
            &alloc,
            &mut dec,
            Planes::new(&mut x_dec, Some(&mut y_dec)),
            &mut cm_masks,
            &mut seed,
        )
        .expect("unquantize_bands succeeds");

        assert_eq!(seed, 3_194_601_925);
        let expected_cm: [u8; 26] =
            [3, 3, 3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 3, 3, 2, 2, 1, 1, 3, 3, 3, 3];
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

        // Anti-collapse against the recorded values
        let mut xy_ac = [0.0_f32; 480];
        xy_ac[..240].copy_from_slice(&x_dec);
        xy_ac[240..].copy_from_slice(&y_dec);

        let energy = BandEnergyHistory {
            current: [[2.5_f32; MAX_BANDS]; 2],
            previous: [[0.5_f32; MAX_BANDS]; 2],
            two_back: [[1.0_f32; MAX_BANDS]; 2],
            background: [[0.0_f32; MAX_BANDS]; 2],
        };
        cm_masks[8 * 2] = 1;
        cm_masks[9 * 2 + 1] = 2;

        anti_collapse(&mut xy_ac, &cm_masks, &frame, 2, &energy, &pulses, seed);

        let expected_ac_b8_x: [f32; 4] =
            [-1.09499902e-01, 1.62257046e-01, 9.67137575e-01, -1.62257046e-01];
        let expected_ac_b9_y: [f32; 4] =
            [1.33849055e-01, 9.79903519e-01, 1.33849055e-01, 6.29118606e-02];
        for (i, (&got, &exp)) in xy_ac[16..20].iter().zip(expected_ac_b8_x.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "anti_collapse B8 X mismatch at {i}: got {got}, expected {exp}"
            );
        }
        for (i, (&got, &exp)) in xy_ac[260..264].iter().zip(expected_ac_b9_y.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "anti_collapse B9 Y mismatch at {i}: got {got}, expected {exp}"
            );
        }

        // Denormalisation against the recorded values
        let mut freq = [0.0_f32; 240];
        let layout = crate::celt::synthesis::Denormalisation {
            start: 0,
            end: 13,
            lm: BlockSize::new(1),
            downsample: 1,
            silence: false,
        };
        crate::celt::synthesis::denormalise_bands(
            &xy_ac[..240],
            &mut freq,
            &energy.current[0],
            &layout,
        );
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
    fn hybrid_folding_and_recursive_split_match_recorded_values() {
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
        let frame = ShapeFrame {
            start: 17,
            end: 21,
            lm: BlockSize::new(3),
            blocks: 1,
            spread: Spread::Aggressive,
            tf_res,
            total_bits: 64 * 8 * 8,
            phase_inversion: true,
        };
        let alloc = allocation(pulses, 21, 19, true);

        let mut dec = Decoder::new(&hbuf);
        let mut x_dec = [0.0_f32; 960];
        let mut y_dec = [0.0_f32; 960];
        let mut cm_dec = [0u8; 42];
        let mut seed = 54321u32;

        unquantize_bands(
            &frame,
            &alloc,
            &mut dec,
            Planes::new(&mut x_dec, Some(&mut y_dec)),
            &mut cm_dec,
            &mut seed,
        )
        .expect("hybrid unquantize_bands succeeds");

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

    // RFC 8251 Section 10: a decoder downmixing to mono may skip the 180-degree phase shift of
    // intensity stereo while still consuming the inversion bit from the range coder.
    #[test]
    fn intensity_stereo_phase_inversion_is_skipped_when_disabled() {
        let buf = [0xFFu8; 16];
        let mut pulses = [0i32; MAX_BANDS];
        pulses[0] = 64;
        let alloc = allocation(pulses, 1, 0, false);

        let decode = |phase_inversion: bool| {
            let frame = ShapeFrame {
                start: 0,
                end: 1,
                lm: BlockSize::new(2),
                blocks: 1,
                spread: Spread::None,
                tf_res: [0; MAX_BANDS],
                total_bits: 16 * 8 * 8,
                phase_inversion,
            };
            let mut dec = Decoder::new(&buf);
            let mut x = [0.0_f32; 4];
            let mut y = [0.0_f32; 4];
            let mut cm = [0u8; 2 * MAX_BANDS];
            let mut seed = 0u32;
            unquantize_bands(
                &frame,
                &alloc,
                &mut dec,
                Planes::new(&mut x, Some(&mut y)),
                &mut cm,
                &mut seed,
            )
            .expect("intensity band decodes");
            return (x, y, dec.tell_frac(), dec.final_range());
        };

        let (x_inv, y_inv, tell_inv, rng_inv) = decode(true);
        let (x_no_inv, y_no_inv, tell_no_inv, rng_no_inv) = decode(false);

        assert_eq!(tell_inv, tell_no_inv);
        assert_eq!(rng_inv, rng_no_inv);
        assert_eq!(x_inv, x_no_inv);
        assert!(x_inv.iter().any(|&v| v != 0.0));
        assert_eq!(y_inv, x_inv.map(|v| -v));
        assert_eq!(y_no_inv, x_no_inv);
        return;
    }
}
