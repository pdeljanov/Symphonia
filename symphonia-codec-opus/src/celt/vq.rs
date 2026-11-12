//! CELT band shape decoding of one partition: PVQ unquantization, spreading, split decoding
//! around a decoded angle, folding and time-frequency changes
//! ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>)).
//! Coupled stereo bands are unmixed in [`super::stereo`] and the sweep over the bands of a frame
//! with the anti-collapse refill lives in [`super::bands`].
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

use crate::celt::constant::{
    BIT_DEINTERLEAVE_TABLE, BIT_INTERLEAVE_TABLE, BITRES, EBANDS, EXP2_TABLE8,
    HADAMARD_SEQUENCY_ROWS, LOGN, MAX_BANDS, QTHETA_OFFSET, QTHETA_OFFSET_TWOPHASE, SPREAD_FACTOR,
    Spread,
};
use crate::celt::cwrs::decode_pulses;
use crate::celt::error::Error;
use crate::celt::lcg_next;
use crate::celt::rate::{
    CacheLevel, bits2pulses_at_level, get_pulses, max_cache_bits, pulses2bits_at_level,
};
use crate::entropy::Decoder;
use crate::planes::Planes;
use std::f32::consts::FRAC_1_SQRT_2;
use symphonia_core::errors::Result;

/// Maximum number of MDCT bins in any single Bark band (band 20 at `LM = 3`: `22 * 8 = 176`).
pub(super) const MAX_BAND_WIDTH: usize = 176;

/// Maximum total size of the normalized lowband folding buffer (`8 * EBANDS[20] = 8 * 78 = 624`).
pub(super) const MAX_NORM_BUFFER_SIZE: usize = 8 * EBANDS[MAX_BANDS - 1];

/// Small positive floor added to squared norms before the reciprocal square root used by band
/// renormalisation ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)).
const NORM_EPSILON: f32 = 1e-15_f32;

/// Split angle `theta = pi / 2` in Q14 (`16384`): all energy in the side (or second) half
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
pub(super) const THETA_MAX_Q14: i32 = 16384;

/// Weight of the split angles up to `qn / 2` in the step PDF of a stereo split; the angles above
/// weigh `1` ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
const STEP_PDF_WEIGHT: i32 = 3;

/// Split angle `theta = pi / 4` in Q14 (`8192`): equal energy in both halves.
pub(super) const THETA_HALF_Q14: i32 = 8192;

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
pub(super) const MAX_BAND_BITS_Q3: i32 = 16383;

/// Number of following bands the leftover bit balance is shared over
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
pub(super) const BALANCE_SHARE_BANDS: i32 = 3;

/// Half a bit in eighth bits, below which the split angle is not coded.
const HALF_BIT_Q3: i32 = 1 << BITRES >> 1;

/// Bit of the spreading generator state that gives a pseudo-random sign
/// ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)).
pub(super) const LCG_SIGN_BIT: u32 = 0x8000;

/// Right shift that turns the 32-bit generator state into a 12-bit signed noise sample.
const LCG_NOISE_SHIFT: u32 = 20;

/// Jitter added to a folded coefficient (`1 / 256`)
/// ([RFC 6716 Section 4.3.4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.2>)).
const FOLD_JITTER: f32 = 1.0 / 256.0;

/// Smallest left or right energy the stereo unmixing scales by; below it the side is dropped
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
pub(super) const STEREO_MERGE_MIN_ENERGY: f32 = 6e-4_f32;

/// Anti-collapse threshold decay per pulse of depth, in base-2 log units (`1 / 8`)
/// ([RFC 6716 Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)).
pub(super) const COLLAPSE_DEPTH_DECAY: f32 = 0.125;

/// Evaluates `cos(pi / 2 * x)` in `f64` and rounds once to `f32`.
#[inline]
fn cos_quarter_turn(x: f32) -> f32 {
    return (std::f64::consts::FRAC_PI_2 * f64::from(x)).cos() as f32;
}

/// Evaluates `2^x = e^(x ln 2)` in `f64` and rounds once to `f32`.
#[inline]
pub(super) fn exp2(x: f32) -> f32 {
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
            Self::Sequency => {
                let level = (stride.trailing_zeros() as usize) - 1;
                usize::from(HADAMARD_SEQUENCY_ROWS[level][i])
            }
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
pub(super) enum SplitKind {
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
    pub(super) fn gains(&self) -> (f32, f32) {
        return ((self.imid as f32) * Q15_SCALE, (self.iside as f32) * Q15_SCALE);
    }

    /// Restricts the folding mask `fill` over `2 * blocks` sub-blocks to the half that carries
    /// energy: the first half at `theta = 0`, the second at `theta = pi / 2`.
    #[inline]
    pub(super) fn restrict_fill(&self, fill: u32, blocks: usize) -> u32 {
        let first_half = (1u32 << blocks) - 1;
        return match self.itheta {
            0 => fill & first_half,
            THETA_MAX_Q14 => fill & (first_half << blocks),
            _ => fill,
        };
    }
}

/// Per-band state tracked during recursive PVQ shape decoding ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>)).
pub(crate) struct BandDecoderCtx<'a, 'd> {
    /// The range decoder the band shapes are read from.
    pub(crate) dec: &'a mut Decoder<'d>,
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

impl<'a, 'd> BandDecoderCtx<'a, 'd> {
    /// Decodes a single-bin (`n = 1`) mono or stereo band: one sign per channel
    /// ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>)).
    pub(super) fn quant_band_n1(&mut self, band: Planes<'_>, lowband: Lowband<'_>) -> Result<u32> {
        let Planes { left: x, right: y } = band;
        let mut decode_sign = |chan: &mut [f32]| -> Result<()> {
            let mut sign = 0u32;
            if self.remaining_bits >= (1 << BITRES) {
                sign = self.dec.decode_raw_bits(1)?;
                self.remaining_bits -= 1 << BITRES;
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

    /// Decodes the `mid` and `side` halves of a split, the larger budget first, handing the bits the
    /// first half left unused beyond [`REBALANCE_THRESHOLD_Q3`] to the second unless `itheta` put
    /// all energy in the first half; returns the combined collapse mask
    /// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
    pub(super) fn decode_two_halves<F>(
        &mut self,
        mid: Half<'_>,
        side: Half<'_>,
        itheta: i32,
        mut decode: F,
    ) -> Result<u32>
    where
        F: FnMut(&mut Self, &mut [f32], Lowband<'_>, Partition) -> Result<u32>,
    {
        let (first, mut second, theta_guard) = match mid.part.bits >= side.part.bits {
            true => (mid, side, 0),
            false => (side, mid, THETA_MAX_Q14),
        };

        let before = self.remaining_bits;
        let cm_first = decode(self, first.x, first.lowband, first.part)? << first.mask_shift;
        let rebalance = first.part.bits - (before - self.remaining_bits);
        if rebalance > REBALANCE_THRESHOLD_Q3 && itheta != theta_guard {
            second.part.bits += rebalance - REBALANCE_THRESHOLD_Q3;
        }
        let cm_second = decode(self, second.x, second.lowband, second.part)? << second.mask_shift;
        return Ok(cm_first | cm_second);
    }

    /// Decodes the split angle `theta` of a vector of `band_width` bins with `bits` eighth bits at
    /// pulse-cache level `level` in a band of `block_count` short blocks
    /// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
    pub(super) fn decode_split_angle(
        &mut self,
        band_width: usize,
        bits: i32,
        level: CacheLevel,
        block_count: usize,
        kind: SplitKind,
    ) -> Result<SplitAngle> {
        let pulse_cap = i32::from(LOGN[self.band]) + level.exponent() * (1 << BITRES);
        let offset = (pulse_cap >> 1)
            - match (kind, band_width) {
                (SplitKind::Stereo, 2) => QTHETA_OFFSET_TWOPHASE,
                _ => QTHETA_OFFSET,
            };
        let intensity_coded = kind == SplitKind::Stereo && self.band >= self.intensity_band;
        let qn =
            if intensity_coded { 1 } else { compute_qn(band_width, bits, offset, pulse_cap, kind) };

        let tell = self.dec.tell_frac() as i32;
        let mut invert_side = false;
        let itheta = match (qn != 1, kind) {
            (true, _) => {
                (decode_theta_symbol(self.dec, qn, band_width, block_count, kind)? * THETA_MAX_Q14)
                    / qn
            }
            (false, SplitKind::Stereo) => {
                if bits > (2 << BITRES) && self.remaining_bits > (2 << BITRES) {
                    invert_side = self.dec.decode_symbol_logp(2)? != 0;
                }
                0
            }
            (false, SplitKind::Partition) => 0,
        };
        let qalloc = (self.dec.tell_frac() as i32) - tell;
        return Ok(SplitAngle::from_theta(itheta, band_width, invert_side, qalloc));
    }

    /// Splits the partition `x` in two around a decoded angle and decodes both halves recursively
    /// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
    fn decode_split_partition(
        &mut self,
        x: &mut [f32],
        lowband: Option<&[f32]>,
        part: Partition,
        next_level: CacheLevel,
    ) -> Result<u32> {
        let Partition { n, blocks: b0, mut bits, mut fill, gain, .. } = part;
        let half_n = n >> 1;
        if b0 == 1 {
            fill = (fill & 1) | (fill << 1);
        }
        let half_blocks = (b0 + 1) >> 1;

        let split = self.decode_split_angle(half_n, bits, next_level, b0, SplitKind::Partition)?;
        fill = split.restrict_fill(fill, half_blocks);
        bits -= split.qalloc;
        let (mid, side) = split.gains();
        let delta = short_block_delta(&split, b0, next_level, half_n);
        let mbits = 0.max(bits.min((bits - delta) / 2));
        let sbits = bits - mbits;
        self.remaining_bits -= split.qalloc;

        let (x_first, x_second) = x[..n].split_at_mut(half_n);
        let (lb_first, lb_second) = lowband.map(|lb| lb[..n].split_at(half_n)).unzip();
        let half = Partition { n: half_n, blocks: half_blocks, level: next_level, ..part };
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
        return self.decode_two_halves(first, second, split.itheta, |ctx, x, lowband, part| {
            return ctx.quant_partition(x, lowband.source, part);
        });
    }

    /// Maps the budget `bits` of a leaf partition at level `lm` to a pseudo-pulse index through the
    /// pulse cache, lowering it while the frame budget is overrun
    /// ([RFC 6716 Section 4.3.4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.1>)).
    fn select_pulse_index(&mut self, level: CacheLevel, bits: i32) -> i32 {
        let mut q = bits2pulses_at_level(level, self.band, bits);
        let mut curr_bits = pulses2bits_at_level(level, self.band, q);
        self.remaining_bits -= curr_bits;
        while self.remaining_bits < 0 && q > 0 {
            self.remaining_bits += curr_bits;
            q -= 1;
            curr_bits = pulses2bits_at_level(level, self.band, q);
            self.remaining_bits -= curr_bits;
        }
        return q;
    }

    /// Fills a zero-pulse partition `x` by folding `lowband` with a pseudo-random jitter, or with
    /// generator noise when there is no folding source; returns the collapse mask
    /// ([RFC 6716 Section 4.3.4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.2>)).
    fn fold_partition(&mut self, x: &mut [f32], lowband: Option<&[f32]>, part: Partition) -> u32 {
        let Partition { n, blocks, fill, gain, .. } = part;
        let cm_mask = (1u32 << blocks) - 1;
        let fill = fill & cm_mask;
        if fill == 0 {
            x[..n].fill(0.0_f32);
            return 0;
        }

        let Some(lb) = lowband
        else {
            for dst in &mut x[..n] {
                self.seed = lcg_next(self.seed);
                *dst = ((self.seed as i32) >> LCG_NOISE_SHIFT) as f32;
            }
            renormalise_vector(&mut x[..n], n, gain);
            return cm_mask;
        };
        for (dst, &src) in x[..n].iter_mut().zip(lb[..n].iter()) {
            self.seed = lcg_next(self.seed);
            let jitter = if (self.seed & LCG_SIGN_BIT) != 0 { FOLD_JITTER } else { -FOLD_JITTER };
            *dst = src + jitter;
        }
        renormalise_vector(&mut x[..n], n, gain);
        return fill;
    }

    /// Recursively decodes a mono partition `x`: split while the budget exceeds the pulse cache,
    /// otherwise a pulse vector, or a folded or noise-filled vector without pulses
    /// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
    fn quant_partition(
        &mut self,
        x: &mut [f32],
        lowband: Option<&[f32]>,
        part: Partition,
    ) -> Result<u32> {
        let Partition { n, blocks, level, bits, gain, .. } = part;
        // The lowest level is never split; checking it first also keeps the cache lookup in range.
        let split_level = level
            .split()
            .filter(|_| bits > max_cache_bits(level, self.band) + SPLIT_MARGIN_Q3 && n > 2);
        if let Some(next_level) = split_level {
            return self.decode_split_partition(x, lowband, part, next_level);
        }

        let q = self.select_pulse_index(level, bits);
        if q != 0 {
            return pvq_unquantize(
                &mut x[..n],
                get_pulses(q as usize),
                self.spread,
                blocks,
                self.dec,
                gain,
            );
        }
        return Ok(self.fold_partition(x, lowband, part));
    }

    /// Decodes a single mono Bark band `x`, applying time-frequency pre/post-recombination and
    /// writing its scaled shape to the folding history
    /// ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>)).
    pub(super) fn quant_band(
        &mut self,
        x: &mut [f32],
        lowband: Lowband<'_>,
        part: Partition,
    ) -> Result<u32> {
        let Partition { n, blocks, fill, .. } = part;
        if n == 1 {
            return self.quant_band_n1(Planes::new(x, None), lowband);
        }
        let Lowband { source, out } = lowband;
        let tf = TimeFrequency::new(n, blocks, self.tf_change);

        let mut lowband_buf = [0.0_f32; MAX_BAND_WIDTH];
        if let Some(src) = source {
            lowband_buf[..n].copy_from_slice(&src[..n]);
            tf.fold_source_forward(&mut lowband_buf[..n]);
        }
        let folded = source.map(|_| &lowband_buf[..n]);
        let decoded = Partition { blocks: tf.blocks, fill: tf.fill_forward(fill), ..part };
        let cm = self.quant_partition(&mut x[..n], folded, decoded)?;
        let cm = tf.inverse(&mut x[..n], cm);

        if let Some(out) = out {
            let scale = (n as f32).sqrt();
            for (dst, &src) in out[..n].iter_mut().zip(x[..n].iter()) {
                *dst = scale * src;
            }
        }
        return Ok(cm);
    }
}

/// What one recursive shape-decoding step receives besides its coefficients: the vector
/// geometry (`n` bins over `blocks` short MDCT blocks at pulse-cache level `lm`), its
/// eighth-bit budget, the folding fill mask inherited from the lower bands and the target gain
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
#[derive(Debug, Clone, Copy)]
pub(super) struct Partition {
    /// Number of MDCT bins.
    pub(super) n: usize,
    /// Number of short MDCT blocks the bins are interleaved over.
    pub(super) blocks: usize,
    /// Pulse-cache level (`LM`, decremented by one per split, down to `-1`).
    pub(super) level: CacheLevel,
    /// Shape budget in eighth bits.
    pub(super) bits: i32,
    /// Per-block mask of the folding source blocks that carried energy.
    pub(super) fill: u32,
    /// Euclidean norm the decoded vector is scaled to.
    pub(super) gain: f32,
}

/// Folding history of one band: the normalised lower-band vector it folds from and the slot of
/// the history it writes for the bands above it (absent on the last band and the side channel)
/// ([RFC 6716 Section 4.3.4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.2>)).
#[derive(Default)]
pub(super) struct Lowband<'a> {
    /// Normalised lower-band coefficients to fold from.
    pub(super) source: Option<&'a [f32]>,
    /// Slot of the folding history that receives this band's normalised shape.
    pub(super) out: Option<&'a mut [f32]>,
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
        let (fl, fh) = match x <= x0 {
            true => (STEP_PDF_WEIGHT * x, STEP_PDF_WEIGHT * (x + 1)),
            false => (x - 1 - x0 + knee, x - x0 + knee),
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
        let (x, fl, fs) = match fm < rising {
            true => {
                let x = ((((8 * fm + 1) as u32).isqrt() as i32) - 1) >> 1;
                (x, (x * (x + 1)) >> 1, x + 1)
            }
            false => {
                let x = (2 * (qn + 1) - (((8 * (ft - fm - 1) + 1) as u32).isqrt() as i32)) >> 1;
                (x, ft - (((qn + 1 - x) * (qn + 2 - x)) >> 1), qn + 1 - x)
            }
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
    band_width: usize,
    block_count: usize,
    kind: SplitKind,
) -> Result<i32> {
    return match (kind, band_width > 2, block_count > 1) {
        (SplitKind::Stereo, true, _) => Ok(decode_step_pdf(dec, qn)),
        (SplitKind::Stereo, false, _) | (SplitKind::Partition, _, true) => {
            Ok(dec.decode_uint((qn + 1) as u32)? as i32)
        }
        (SplitKind::Partition, _, false) => Ok(decode_triangular_pdf(dec, qn)),
    };
}

/// One half of a split vector handed to [`BandDecoderCtx::decode_two_halves`].
pub(super) struct Half<'a> {
    /// Coefficients of the half.
    pub(super) x: &'a mut [f32],
    /// Folding source and output of the half.
    pub(super) lowband: Lowband<'a>,
    /// Geometry, budget, fill and gain of the half.
    pub(super) part: Partition,
    /// Left shift of the half's collapse mask into the band's mask.
    pub(super) mask_shift: usize,
}

/// Bit differential of a split in a short-block band (`b0 > 1`): extra bits go to the
/// low-energy half to mitigate pre-echo and forward masking
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
fn short_block_delta(
    split: &SplitAngle,
    block_count: usize,
    next_level: CacheLevel,
    half_n: usize,
) -> i32 {
    if block_count <= 1 || (split.itheta & THETA_INTERIOR_MASK_Q14) == 0 {
        return split.delta;
    }
    if split.itheta > THETA_HALF_Q14 {
        return split.delta - (split.delta >> (4 - next_level.exponent()));
    }
    return 0.min(split.delta + (((half_n as i32) << BITRES) >> (5 - next_level.exponent())));
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

#[cfg(test)]
mod tests;
