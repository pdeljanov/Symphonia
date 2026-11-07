//! CELT Inverse MDCT, Windowing, Overlap-Add, Comb Post-Filter, De-Emphasis, and PLC Filter Helpers
//! (RFC 6716 Section 4.3.6 and Section 4.3.7)
//!
//! This module implements the time-domain synthesis pipeline for the 48 kHz CELT mode:
//! - [`denormalise_bands`]: Scales unit-norm PVQ band vectors by their decoded linear gains
//!   $g_b = 2^{\text{bandLogE}[b] + \text{eMeans}[b]}$ (RFC 6716 Section 4.3.6.1).
//! - [`clt_mdct_backward`] and [`mdct_backward_frame`]: Mixed-radix (radix-2/3/4/5) complex IFFT
//!   pre/post-rotated Inverse Modified Discrete Cosine Transform (`IMDCT`) for block sizes
//!   $N \in \{120, 240, 480, 960\}$ with symmetric 120-sample Time-Domain Aliasing Cancellation
//!   (TDAC) overlap-add using [`CELT_WINDOW`] (RFC 6716 Section 4.3.6.2).
//! - [`comb_filter`] and [`comb_filter_in_place`]: 5-tap symmetric pitch comb post-filter with
//!   squared-window cross-fade during pitch/gain/tapset transitions (RFC 6716 Section 4.3.6.3).
//! - [`deemphasis_channel`] and [`deemphasis`]: First-order $1 / (1 - \alpha z^{-1})$ de-emphasis
//!   filter ($\alpha = 0.85000610$), integer decimation for $8, 12, 16, 24, 48\text{ kHz}$ output,
//!   and normalization from CELT's internal $\pm 32768.0$ scale to $[-1.0, 1.0]$ (RFC 6716 Section 4.3.6.4).
//! - [`celt_autocorr`], [`celt_lpc`], [`celt_fir`], [`celt_iir`], [`pitch_downsample`],
//!   [`pitch_search`], and [`plc_pitch_search`]: Linear Predictive Coding (`LPC_ORDER = 24`) and
//!   time-domain pitch estimation helpers for CELT Packet Loss Concealment (PLC).
//!
//! <https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.6>

#![allow(clippy::excessive_precision)]
#![allow(clippy::approx_constant)]
#![allow(clippy::needless_return)]
#![allow(clippy::too_many_arguments)]

use std::sync::OnceLock;

use crate::celt::constant::{
    CELT_WINDOW, COMB_FILTER_GAINS, EBANDS, E_MEANS, MAX_BANDS, OVERLAP, PREEMPH_COEF,
};
use crate::celt::{
    COMBFILTER_MAXPERIOD, COMBFILTER_MINPERIOD, DECODE_BUFFER_SIZE, MAX_FRAME_SIZE, SHORT_MDCT_SIZE,
};

/// Maximum full-window MDCT length at 48 kHz (`2 * MAX_FRAME_SIZE = 1920` samples).
pub const MDCT_MAX_N: usize = 2 * MAX_FRAME_SIZE;

/// Maximum complex quarter-length IFFT size (`MDCT_MAX_N / 4 = 480` complex points).
pub const MDCT_MAX_N4: usize = MDCT_MAX_N / 4;

/// CELT internal full-scale signal amplitude (`32768.0`, matching `CELT_SIG_SCALE` in `libopus`).
pub const CELT_SIG_SCALE: f32 = 32768.0;

/// Reciprocal of [`CELT_SIG_SCALE`] used to normalize CELT synthesis output to `[-1.0, 1.0]`.
pub const INV_CELT_SIG_SCALE: f32 = 1.0 / CELT_SIG_SCALE;

/// tiny DC/subnormal bias added inside the de-emphasis recurrence (`VERY_SMALL` in `libopus`).
const VERY_SMALL: f32 = 1.0e-30;

/// Linear Predictive Coding (LPC) order used for CELT Packet Loss Concealment (`LPC_ORDER = 24`).
pub const LPC_ORDER: usize = 24;

/// Maximum pitch period searched during CELT Packet Loss Concealment (`PLC_PITCH_LAG_MAX = 720`).
pub const PLC_PITCH_LAG_MAX: usize = 720;

/// Minimum pitch period searched during CELT Packet Loss Concealment (`PLC_PITCH_LAG_MIN = 100`).
pub const PLC_PITCH_LAG_MIN: usize = 100;

/// Single-precision complex number used in the mixed-radix quarter-length FFT.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Complex32 {
    /// Real part.
    pub r: f32,
    /// Imaginary part.
    pub i: f32,
}

impl Complex32 {
    /// Creates a new complex number `r + i * j`.
    #[inline]
    pub const fn new(r: f32, i: f32) -> Self {
        return Self { r, i };
    }

    /// Component-wise complex addition `self + rhs`.
    #[inline]
    pub fn add(self, rhs: Self) -> Self {
        return Self { r: self.r + rhs.r, i: self.i + rhs.i };
    }

    /// Component-wise complex subtraction `self - rhs`.
    #[inline]
    pub fn sub(self, rhs: Self) -> Self {
        return Self { r: self.r - rhs.r, i: self.i - rhs.i };
    }

    /// Complex multiplication `self * rhs`.
    #[inline]
    pub fn mul(self, rhs: Self) -> Self {
        return Self { r: self.r * rhs.r - self.i * rhs.i, i: self.r * rhs.i + self.i * rhs.r };
    }

    /// Scales both components by a real scalar `s`.
    #[inline]
    pub fn scale(self, s: f32) -> Self {
        return Self { r: self.r * s, i: self.i * s };
    }
}

/// Precomputed trigonometric and bit-reversal tables for the 48 kHz CELT IMDCT (`shift in 0..=3`).
struct MdctTables {
    /// Forward 480-point FFT twiddles: `fft_twiddles[k] = exp(-j * 2 * pi * k / 480)`.
    fft_twiddles: [Complex32; MDCT_MAX_N4],
    /// Mixed-radix digit-reversal permutation tables for `n4 = 480, 240, 120, 60` (`shift = 0..=3`).
    bitrev: [[u16; MDCT_MAX_N4]; 4],
    /// MDCT pre/post-rotation cosine/sine twiddles for `shift = 0..=3`:
    /// `trig[shift][i] = cos(2 * pi * (i + 0.125) / n)` and
    /// `trig[shift][n4 + i] = -sin(2 * pi * (i + 0.125) / n)` where `n = 1920 >> shift`.
    trig: [[f32; 2 * MDCT_MAX_N4]; 4],
}

/// Mixed-radix `(radix_p, stage_m)` factorizations of `n4 = 480 >> shift` matching `kiss_fft.c`.
const FFT_FACTORS_SHIFT0: [(usize, usize); 5] = [(5, 96), (3, 32), (4, 8), (2, 4), (4, 1)];
const FFT_FACTORS_SHIFT1: [(usize, usize); 4] = [(5, 48), (3, 16), (4, 4), (4, 1)];
const FFT_FACTORS_SHIFT2: [(usize, usize); 4] = [(5, 24), (3, 8), (2, 4), (4, 1)];
const FFT_FACTORS_SHIFT3: [(usize, usize); 3] = [(5, 12), (3, 4), (4, 1)];

/// Returns the mixed-radix factorization slice `(radix_p, stage_m)` for a given `shift in 0..=3`.
#[inline]
fn fft_factors(shift: usize) -> &'static [(usize, usize)] {
    return match shift {
        0 => &FFT_FACTORS_SHIFT0,
        1 => &FFT_FACTORS_SHIFT1,
        2 => &FFT_FACTORS_SHIFT2,
        _ => &FFT_FACTORS_SHIFT3,
    };
}

/// Recursively populates the KissFFT mixed-radix digit-reversal table (`compute_bitrev_table`).
fn compute_bitrev_recursive(
    fout: usize,
    out: &mut [u16],
    in_offset: usize,
    fstride: usize,
    in_stride: usize,
    factors: &[(usize, usize)],
) {
    let (p, m) = factors[0];
    let rest = &factors[1..];
    if m == 1 {
        for j in 0..p {
            out[in_offset + j * fstride * in_stride] = (fout + j) as u16;
        }
    } else {
        let mut fout_cur = fout;
        let mut offset_cur = in_offset;
        for _ in 0..p {
            compute_bitrev_recursive(fout_cur, out, offset_cur, fstride * p, in_stride, rest);
            offset_cur += fstride * in_stride;
            fout_cur += m;
        }
    }
    return;
}

/// Initializes and returns the singleton [`MdctTables`] for 48 kHz CELT.
fn mdct_tables() -> &'static MdctTables {
    static TABLES: OnceLock<MdctTables> = OnceLock::new();
    return TABLES.get_or_init(|| {
        let mut fft_twiddles = [Complex32::default(); MDCT_MAX_N4];
        for (k, tw) in fft_twiddles.iter_mut().enumerate() {
            let phase = (-2.0 * std::f64::consts::PI * (k as f64)) / (MDCT_MAX_N4 as f64);
            *tw = Complex32::new(phase.cos() as f32, phase.sin() as f32);
        }

        let mut bitrev = [[0u16; MDCT_MAX_N4]; 4];
        let mut trig = [[0.0f32; 2 * MDCT_MAX_N4]; 4];

        for shift in 0..4 {
            let n = MDCT_MAX_N >> shift;
            let n4 = n >> 2;
            compute_bitrev_recursive(0, &mut bitrev[shift][..n4], 0, 1, 1, fft_factors(shift));

            for i in 0..n4 {
                let angle = (2.0 * std::f64::consts::PI * ((i as f64) + 0.125)) / (n as f64);
                trig[shift][i] = angle.cos() as f32;
                trig[shift][n4 + i] = (-angle.sin()) as f32;
            }
        }

        return MdctTables { fft_twiddles, bitrev, trig };
    });
}

/// Radix-4 butterfly stage for the innermost base case `m == 1` (`kf_bfly4` in `kiss_fft.c`).
#[inline]
fn kf_bfly4_m1(fout: &mut [Complex32], n4: usize) {
    for base in (0..n4).step_by(4) {
        let f0 = fout[base];
        let f1 = fout[base + 1];
        let f2 = fout[base + 2];
        let f3 = fout[base + 3];

        let scratch0 = f0.sub(f2);
        let f0_plus_f2 = f0.add(f2);
        let scratch1 = f1.add(f3);
        let scratch2 = f1.sub(f3);

        fout[base + 2] = f0_plus_f2.sub(scratch1);
        fout[base] = f0_plus_f2.add(scratch1);

        fout[base + 1] = Complex32::new(scratch0.r + scratch2.i, scratch0.i - scratch2.r);
        fout[base + 3] = Complex32::new(scratch0.r - scratch2.i, scratch0.i + scratch2.r);
    }
    return;
}

/// Radix-4 butterfly stage for general `m > 1` (`kf_bfly4` in `kiss_fft.c`).
#[inline]
fn kf_bfly4_general(
    fout: &mut [Complex32],
    fstride: usize,
    m: usize,
    n: usize,
    mm: usize,
    twiddles: &[Complex32; MDCT_MAX_N4],
) {
    for group in 0..n {
        let group_base = group * mm;
        for j in 0..m {
            let idx0 = group_base + j;
            let idx1 = idx0 + m;
            let idx2 = idx1 + m;
            let idx3 = idx2 + m;

            let tw1 = twiddles[j * fstride];
            let tw2 = twiddles[j * fstride * 2];
            let tw3 = twiddles[j * fstride * 3];

            let scratch0 = fout[idx1].mul(tw1);
            let scratch1 = fout[idx2].mul(tw2);
            let scratch2 = fout[idx3].mul(tw3);

            let f0 = fout[idx0];
            let scratch5 = f0.sub(scratch1);
            let f0_plus_s1 = f0.add(scratch1);
            let scratch3 = scratch0.add(scratch2);
            let scratch4 = scratch0.sub(scratch2);

            fout[idx2] = f0_plus_s1.sub(scratch3);
            fout[idx0] = f0_plus_s1.add(scratch3);

            fout[idx1] = Complex32::new(scratch5.r + scratch4.i, scratch5.i - scratch4.r);
            fout[idx3] = Complex32::new(scratch5.r - scratch4.i, scratch5.i + scratch4.r);
        }
    }
    return;
}

/// Radix-2 butterfly stage specialized for `m == 4` (`kf_bfly2` in `kiss_fft.c`).
#[inline]
fn kf_bfly2_m4(
    fout: &mut [Complex32],
    fstride: usize,
    n: usize,
    twiddles: &[Complex32; MDCT_MAX_N4],
) {
    const M: usize = 4;
    const MM: usize = 2 * M;
    for group in 0..n {
        let base = group * MM;
        for j in 0..M {
            let tw = twiddles[j * fstride];
            let t = fout[base + M + j].mul(tw);
            let f0 = fout[base + j];
            fout[base + M + j] = f0.sub(t);
            fout[base + j] = f0.add(t);
        }
    }
    return;
}

/// Radix-3 butterfly stage (`kf_bfly3` in `kiss_fft.c`).
#[inline]
fn kf_bfly3(
    fout: &mut [Complex32],
    fstride: usize,
    m: usize,
    n: usize,
    mm: usize,
    twiddles: &[Complex32; MDCT_MAX_N4],
) {
    // epi3.i = -sqrt(3)/2 = twiddles[160].i
    let epi3_i = twiddles[160].i;
    let m2 = 2 * m;

    for group in 0..n {
        let group_base = group * mm;
        for k in 0..m {
            let idx0 = group_base + k;
            let idx1 = idx0 + m;
            let idx2 = idx0 + m2;

            let tw1 = twiddles[k * fstride];
            let tw2 = twiddles[k * fstride * 2];

            let scratch1 = fout[idx1].mul(tw1);
            let scratch2 = fout[idx2].mul(tw2);
            let scratch3 = scratch1.add(scratch2);
            let scratch0 = scratch1.sub(scratch2);

            let f0 = fout[idx0];
            let f_mid = Complex32::new(f0.r - 0.5 * scratch3.r, f0.i - 0.5 * scratch3.i);
            let rot = Complex32::new(scratch0.i * epi3_i, -scratch0.r * epi3_i);

            fout[idx0] = f0.add(scratch3);
            fout[idx2] = f_mid.add(rot);
            fout[idx1] = f_mid.sub(rot);
        }
    }
    return;
}

/// Radix-5 butterfly stage (`kf_bfly5` in `kiss_fft.c`).
#[inline]
fn kf_bfly5(
    fout: &mut [Complex32],
    fstride: usize,
    m: usize,
    n: usize,
    mm: usize,
    twiddles: &[Complex32; MDCT_MAX_N4],
) {
    let ya = twiddles[96];
    let yb = twiddles[192];

    for group in 0..n {
        let group_base = group * mm;
        for u in 0..m {
            let idx0 = group_base + u;
            let idx1 = idx0 + m;
            let idx2 = idx1 + m;
            let idx3 = idx2 + m;
            let idx4 = idx3 + m;

            let scratch0 = fout[idx0];
            let scratch1 = fout[idx1].mul(twiddles[u * fstride]);
            let scratch2 = fout[idx2].mul(twiddles[2 * u * fstride]);
            let scratch3 = fout[idx3].mul(twiddles[3 * u * fstride]);
            let scratch4 = fout[idx4].mul(twiddles[4 * u * fstride]);

            let scratch7 = scratch1.add(scratch4);
            let scratch10 = scratch1.sub(scratch4);
            let scratch8 = scratch2.add(scratch3);
            let scratch9 = scratch2.sub(scratch3);

            fout[idx0] = scratch0.add(scratch7).add(scratch8);

            let scratch5 = Complex32::new(
                scratch0.r + scratch7.r * ya.r + scratch8.r * yb.r,
                scratch0.i + scratch7.i * ya.r + scratch8.i * yb.r,
            );
            let scratch6 = Complex32::new(
                scratch10.i * ya.i + scratch9.i * yb.i,
                -(scratch10.r * ya.i + scratch9.r * yb.i),
            );

            fout[idx1] = scratch5.sub(scratch6);
            fout[idx4] = scratch5.add(scratch6);

            let scratch11 = Complex32::new(
                scratch0.r + scratch7.r * yb.r + scratch8.r * ya.r,
                scratch0.i + scratch7.i * yb.r + scratch8.i * ya.r,
            );
            let scratch12 = Complex32::new(
                -scratch10.i * yb.i + scratch9.i * ya.i,
                scratch10.r * yb.i - scratch9.r * ya.i,
            );

            fout[idx2] = scratch11.add(scratch12);
            fout[idx3] = scratch11.sub(scratch12);
        }
    }
    return;
}

/// Runs the in-place mixed-radix KissFFT butterfly cascade (`opus_fft_impl` in `kiss_fft.c`)
/// on a bit-reversed complex buffer `fout[..n4]` for `shift in 0..=3`.
fn opus_fft_impl(fout: &mut [Complex32], shift: usize, twiddles: &[Complex32; MDCT_MAX_N4]) {
    let n4 = MDCT_MAX_N4 >> shift;
    let factors = fft_factors(shift);

    // Stage 0 (innermost): p = 4, m = 1
    kf_bfly4_m1(&mut fout[..n4], n4);

    // Remaining stages from inner to outer
    let mut fstride = n4 / 4;
    for &(p, m) in factors[..factors.len() - 1].iter().rev() {
        fstride /= p;
        let mm = p * m;
        let n_groups = n4 / mm;
        let scaled_fstride = fstride << shift;
        match p {
            2 => kf_bfly2_m4(&mut fout[..n4], scaled_fstride, n_groups, twiddles),
            3 => kf_bfly3(&mut fout[..n4], scaled_fstride, m, n_groups, mm, twiddles),
            4 => kf_bfly4_general(&mut fout[..n4], scaled_fstride, m, n_groups, mm, twiddles),
            5 => kf_bfly5(&mut fout[..n4], scaled_fstride, m, n_groups, mm, twiddles),
            _ => unreachable!("unsupported FFT radix"),
        }
    }
    return;
}

/// Computes the CELT Inverse MDCT (`clt_mdct_backward_c` in `mdct.c`, RFC 6716 Section 4.3.6.2)
/// with symmetric 120-sample TDAC overlap-add.
///
/// # Parameters
/// - `input`: Frequency-domain MDCT coefficients. Read with step `stride` (`n2 = 960 >> shift`
///   coefficients at indices `0, stride, 2 * stride, ..., (n2 - 1) * stride`).
/// - `out`: Time-domain synthesis buffer of length at least `n2 + OVERLAP` (`n2 + 120`).
///   On entry, `out[0 .. OVERLAP / 2]` (`out[0..60]`) MUST contain the unwindowed tail from the
///   previous MDCT block. On return, `out[0 .. n2]` contains the completed, overlapped time-domain
///   samples for this block, and `out[n2 .. n2 + OVERLAP / 2]` (`out[n2 .. n2 + 60]`) holds the
///   new unwindowed tail for the next block.
/// - `window`: The 120-sample symmetric Vorbis-like MDCT window (typically [`CELT_WINDOW`]).
/// - `overlap`: Window overlap length (`120` in 48 kHz CELT mode).
/// - `shift`: Block-size right-shift (`0..=3`, where `shift = 3 - lm` for a single long block
///   of `n2 = 120 << lm` bins, or `shift = 3` for transient short blocks of `n2 = 120` bins).
/// - `stride`: Input stride (`1` for non-transient frames, or `B = 1 << lm` for interleaved
///   transient short blocks).
pub fn clt_mdct_backward(
    input: &[f32],
    out: &mut [f32],
    window: &[f32],
    overlap: usize,
    shift: usize,
    stride: usize,
) {
    debug_assert!(shift <= 3);
    let tables = mdct_tables();
    let n = MDCT_MAX_N >> shift;
    let n2 = n >> 1;
    let n4 = n >> 2;
    let half_overlap = overlap >> 1;

    debug_assert!(out.len() >= n2 + half_overlap);
    debug_assert!(window.len() >= overlap);
    debug_assert!(input.len() > (n2 - 1) * stride);

    let trig = &tables.trig[shift];
    let bitrev = &tables.bitrev[shift];

    // Step 1: Pre-rotate input frequency coefficients and scatter into bit-reversed FFT buffer.
    // Note: Real and imaginary components are swapped (`Complex32::new(yi, yr)`) so that
    // the forward complex DFT `opus_fft_impl` computes an unscaled inverse DFT.
    let mut fft_buf = [Complex32::default(); MDCT_MAX_N4];
    let stride2 = 2 * stride;
    for i in 0..n4 {
        let x1 = input[i * stride2];
        let x2 = input[(n2 - 1 - 2 * i) * stride];
        let t_cos = trig[i];
        let t_sin = trig[n4 + i];

        let yr = x2 * t_cos + x1 * t_sin;
        let yi = x1 * t_cos - x2 * t_sin;
        let rev = usize::from(bitrev[i]);
        fft_buf[rev] = Complex32::new(yi, yr);
    }

    // Step 2: Quarter-length (`N / 4`) mixed-radix complex FFT.
    opus_fft_impl(&mut fft_buf[..n4], shift, &tables.fft_twiddles);

    // Step 3: Post-rotate and de-interleave into `out[half_overlap .. half_overlap + n2]`.
    let half_n4 = (n4 + 1) >> 1;
    for i in 0..half_n4 {
        let j = n4 - 1 - i;

        // Swap real/imag back from the forward-FFT conjugation trick: `re = c.i`, `im = c.r`.
        let c0 = fft_buf[i];
        let re0 = c0.i;
        let im0 = c0.r;

        let c1 = fft_buf[j];
        let re1 = c1.i;
        let im1 = c1.r;

        let t0_cos = trig[i];
        let t0_sin = trig[n4 + i];
        let t1_cos = trig[j];
        let t1_sin = trig[n4 + j];

        let yr0 = re0 * t0_cos + im0 * t0_sin;
        let yi0 = re0 * t0_sin - im0 * t0_cos;
        let yr1 = re1 * t1_cos + im1 * t1_sin;
        let yi1 = re1 * t1_sin - im1 * t1_cos;

        out[half_overlap + 2 * i] = yr0;
        out[half_overlap + 2 * i + 1] = yi1;
        out[half_overlap + n2 - 2 - 2 * i] = yr1;
        out[half_overlap + n2 - 1 - 2 * i] = yi0;
    }

    // Step 4: Symmetric TDAC windowing and overlap-add on `out[0 .. overlap]`.
    // Prior to this loop, `out[0 .. half_overlap]` holds the previous block's unwindowed tail,
    // and `out[half_overlap .. overlap]` holds the newly synthesized leading half-overlap.
    for i in 0..half_overlap {
        let x1 = out[overlap - 1 - i];
        let x2 = out[i];
        let w1 = window[i];
        let w2 = window[overlap - 1 - i];

        out[i] = x2 * w2 - x1 * w1;
        out[overlap - 1 - i] = x2 * w1 + x1 * w2;
    }
    return;
}

/// Synthesizes a complete CELT frame for a single channel using either a single long IMDCT
/// (`is_transient == false`, `b = 1`) or `B = 1 << lm` interleaved short 120-sample IMDCTs
/// (`is_transient == true`), matching `celt_synthesis` in `celt_decoder.c` (RFC 6716 Section 4.3.6.2).
///
/// - `freq`: Denormalized MDCT spectrum of length `N = 120 << lm`.
/// - `out_syn`: Output buffer slice of length at least `N + OVERLAP / 2` (`(120 << lm) + 60`),
///   starting at the current frame offset in the channel's synthesis buffer (`decode_mem + DECODE_BUFFER_SIZE - N`).
/// - `lm`: Frame size exponent (`0..=3`).
/// - `is_transient`: True if the frame uses `1 << lm` short blocks of 120 samples.
pub fn mdct_backward_frame(freq: &[f32], out_syn: &mut [f32], lm: usize, is_transient: bool) {
    debug_assert!(lm <= 3);
    if is_transient {
        let num_short_blocks = 1usize << lm;
        let shift = 3;
        for b in 0..num_short_blocks {
            let block_offset = b * SHORT_MDCT_SIZE;
            clt_mdct_backward(
                &freq[b..],
                &mut out_syn[block_offset..],
                &CELT_WINDOW,
                OVERLAP,
                shift,
                num_short_blocks,
            );
        }
    } else {
        let shift = 3 - lm;
        clt_mdct_backward(freq, out_syn, &CELT_WINDOW, OVERLAP, shift, 1);
    }
    return;
}

/// Scales the unit-norm PVQ band vectors `norm_x` by their decoded log-energies `band_log_e`
/// into the full-scale MDCT coefficient buffer `freq` (`denormalise_bands` in `bands.c`,
/// RFC 6716 Section 4.3.6.1).
///
/// For each band $b \in [\text{start}, \text{end})$, the linear band amplitude is:
/// $$g_b = 2^{\min(\text{bandLogE}[b] + \text{eMeans}[b],\, 32.0)} = \exp\bigl(\ln(2)\cdot \min(\text{bandLogE}[b] + \text{eMeans}[b],\, 32.0)\bigr)$$
///
/// Bins below `start`, above `end`, or above the downsampling Nyquist limit `N / downsample`
/// are zeroed (and the entire spectrum is zeroed if `silence` is true).
pub fn denormalise_bands(
    norm_x: &[f32],
    freq: &mut [f32],
    band_log_e: &[f32; MAX_BANDS],
    start: usize,
    end: usize,
    lm: usize,
    downsample: usize,
    silence: bool,
) {
    let m = 1usize << lm;
    let n = m * SHORT_MDCT_SIZE;
    debug_assert!(norm_x.len() >= n);
    debug_assert!(freq.len() >= n);

    if silence {
        freq[..n].fill(0.0);
        return;
    }

    let start_bin = m * EBANDS[start];
    freq[..start_bin].fill(0.0);

    let mut bound = m * EBANDS[end];
    if downsample > 1 {
        bound = bound.min(n / downsample);
    }

    for band in start..end {
        let j_start = m * EBANDS[band];
        let band_end = (m * EBANDS[band + 1]).min(bound);
        if j_start >= band_end {
            break;
        }
        let lg = (band_log_e[band] + E_MEANS[band]).min(32.0);
        // Match libopus `celt_exp2(lg) = exp(0.6931471805599453094 * lg)`
        let gain = (0.6931471805599453094f32 * lg).exp();
        for (dst, &src) in freq[j_start..band_end].iter_mut().zip(&norm_x[j_start..band_end]) {
            *dst = src * gain;
        }
    }

    freq[bound..n].fill(0.0);
    return;
}

/// Applies the 5-tap symmetric pitch comb post-filter (`comb_filter` in `celt.c`,
/// RFC 6716 Section 4.3.6.3), reading from `x_buf` at `history_offset` and writing `n` filtered
/// samples into `y_out[..n]`.
///
/// The filter impulse response for period $T$, gain $G$, and tapset $k \in \{0, 1, 2\}$ uses
/// tap weights $[g_0, g_1, g_2] = \text{COMB\_FILTER\_GAINS}[k]$:
/// $$y[i] = x[i] + G\Bigl(g_0 x[i - T] + g_1\bigl(x[i - T - 1] + x[i - T + 1]\bigr) + g_2\bigl(x[i - T - 2] + x[i - T + 2]\bigr)\Bigr)$$
///
/// When $(T_0, G_0, \text{tapset}_0) \ne (T_1, G_1, \text{tapset}_1)$, the first `overlap`
/// samples smoothly cross-fade from the old filter output to the new filter output using the
/// squared MDCT window weight $f(i) = w[i]^2$.
pub fn comb_filter(
    y_out: &mut [f32],
    x_buf: &[f32],
    history_offset: usize,
    t0: i32,
    t1: i32,
    g0: f32,
    g1: f32,
    tapset0: usize,
    tapset1: usize,
    window: &[f32],
    overlap: usize,
    n: usize,
) {
    debug_assert!(y_out.len() >= n);
    debug_assert!(x_buf.len() >= history_offset + n);

    if g0 == 0.0 && g1 == 0.0 {
        y_out[..n].copy_from_slice(&x_buf[history_offset..history_offset + n]);
        return;
    }

    let period0 = (t0.max(COMBFILTER_MINPERIOD as i32) as usize).min(COMBFILTER_MAXPERIOD);
    let period1 = (t1.max(COMBFILTER_MINPERIOD as i32) as usize).min(COMBFILTER_MAXPERIOD);
    debug_assert!(history_offset >= period0 + 2);
    debug_assert!(history_offset >= period1 + 2);

    let gains0 = COMB_FILTER_GAINS[tapset0.min(2)];
    let g00 = g0 * gains0[0];
    let g01 = g0 * gains0[1];
    let g02 = g0 * gains0[2];

    let gains1 = COMB_FILTER_GAINS[tapset1.min(2)];
    let g10 = g1 * gains1[0];
    let g11 = g1 * gains1[1];
    let g12 = g1 * gains1[2];

    let same_filter = g0 == g1 && period0 == period1 && tapset0 == tapset1;
    let xfade_len = if same_filter { 0 } else { overlap.min(n) };

    for i in 0..xfade_len {
        let idx = history_offset + i;
        let x0 = x_buf[idx];
        let f = window[i] * window[i];

        let p0 = idx - period0;
        let p1 = idx - period1;

        let old_comb = g00 * x_buf[p0]
            + g01 * (x_buf[p0 + 1] + x_buf[p0 - 1])
            + g02 * (x_buf[p0 + 2] + x_buf[p0 - 2]);
        let new_comb = g10 * x_buf[p1]
            + g11 * (x_buf[p1 + 1] + x_buf[p1 - 1])
            + g12 * (x_buf[p1 + 2] + x_buf[p1 - 2]);

        y_out[i] = x0 + (1.0 - f) * old_comb + f * new_comb;
    }

    if g1 == 0.0 {
        y_out[xfade_len..n]
            .copy_from_slice(&x_buf[history_offset + xfade_len..history_offset + n]);
        return;
    }

    for (i, y_slot) in y_out.iter_mut().enumerate().take(n).skip(xfade_len) {
        let idx = history_offset + i;
        let p1 = idx - period1;
        *y_slot = x_buf[idx]
            + g10 * x_buf[p1]
            + g11 * (x_buf[p1 + 1] + x_buf[p1 - 1])
            + g12 * (x_buf[p1 + 2] + x_buf[p1 - 2]);
    }
    return;
}

/// Applies [`comb_filter`] in-place on `signal_buf[history_offset .. history_offset + n]`,
/// using `signal_buf[..history_offset]` as the unmodified pitch history (`COMBFILTER_MAXPERIOD = 1024`).
pub fn comb_filter_in_place(
    signal_buf: &mut [f32],
    history_offset: usize,
    t0: i32,
    t1: i32,
    g0: f32,
    g1: f32,
    tapset0: usize,
    tapset1: usize,
    overlap: usize,
    n: usize,
) {
    debug_assert!( n <= MAX_FRAME_SIZE);
    if g0 == 0.0 && g1 == 0.0 {
        return;
    }
    let mut filtered = [0.0f32; MAX_FRAME_SIZE];
    comb_filter(
        &mut filtered[..n],
        signal_buf,
        history_offset,
        t0,
        t1,
        g0,
        g1,
        tapset0,
        tapset1,
        &CELT_WINDOW,
        overlap,
        n,
    );
    signal_buf[history_offset..history_offset + n].copy_from_slice(&filtered[..n]);
    return;
}

/// Applies the CELT 1-pole de-emphasis filter $y[j] = x[j] + \alpha\,y[j - 1]$
/// ($\alpha = \text{PREEMPH\_COEF} = 0.85000610$), integer decimation (`downsample`), and
/// $1 / 32768$ amplitude normalization for a single channel (`deemphasis` in `celt_decoder.c`,
/// RFC 6716 Section 4.3.6.4).
///
/// Returns the number of output samples written to `pcm_out` (`n / downsample`).
pub fn deemphasis_channel(
    input: &[f32],
    pcm_out: &mut [f32],
    n: usize,
    downsample: usize,
    preemph_mem: &mut f32,
) -> usize {
    debug_assert!(input.len() >= n);
    debug_assert!(downsample >= 1);
    let out_len = n / downsample;
    debug_assert!(pcm_out.len() >= out_len);

    let mut m = *preemph_mem;
    if downsample == 1 {
        for (dst, &x) in pcm_out[..n].iter_mut().zip(&input[..n]) {
            let tmp = x + VERY_SMALL + m;
            m = PREEMPH_COEF * tmp;
            *dst = tmp * INV_CELT_SIG_SCALE;
        }
    } else {
        let mut out_idx = 0usize;
        for (j, &x) in input[..n].iter().enumerate() {
            let tmp = x + VERY_SMALL + m;
            m = PREEMPH_COEF * tmp;
            if j % downsample == 0 {
                pcm_out[out_idx] = tmp * INV_CELT_SIG_SCALE;
                out_idx += 1;
            }
        }
    }
    *preemph_mem = m;
    return out_len;
}

/// Multi-channel wrapper around [`deemphasis_channel`] that updates per-channel de-emphasis
/// state and optionally interleaves stereo samples when `interleave` is true.
pub fn deemphasis(
    inputs: &[&[f32]],
    pcm_out: &mut [f32],
    n: usize,
    channels: usize,
    downsample: usize,
    preemph_mem: &mut [f32],
    interleave: bool,
) -> usize {
    debug_assert!(channels >= 1 && channels <= inputs.len());
    debug_assert!(preemph_mem.len() >= channels);
    let out_per_ch = n / downsample;

    if channels == 1 || !interleave {
        for c in 0..channels {
            let ch_out = &mut pcm_out[c * out_per_ch..(c + 1) * out_per_ch];
            deemphasis_channel(inputs[c], ch_out, n, downsample, &mut preemph_mem[c]);
        }
        return out_per_ch;
    }

    // Interleaved multi-channel output (`pcm_out[k * channels + c]`)
    debug_assert!(pcm_out.len() >= out_per_ch * channels);
    for c in 0..channels {
        let mut m = preemph_mem[c];
        let x_ch = &inputs[c][..n];
        for (j, &x) in x_ch.iter().enumerate() {
            let tmp = x + VERY_SMALL + m;
            m = PREEMPH_COEF * tmp;
            if j % downsample == 0 {
                let sample_idx = j / downsample;
                pcm_out[sample_idx * channels + c] = tmp * INV_CELT_SIG_SCALE;
            }
        }
        preemph_mem[c] = m;
    }
    return out_per_ch;
}

/// Computes `lag + 1` autocorrelation coefficients of `x[..n]`, optionally tapering the first
/// and last `overlap` samples with `window[..overlap]` (`_celt_autocorr` in `celt_lpc.c`).
pub fn celt_autocorr(
    x: &[f32],
    ac: &mut [f32],
    window: Option<&[f32]>,
    overlap: usize,
    lag: usize,
    n: usize,
) {
    debug_assert!(x.len() >= n);
    debug_assert!(ac.len() > lag);
    debug_assert!(n > lag);

    let mut xx = [0.0f32; DECODE_BUFFER_SIZE];
    let buf: &[f32] = if overlap > 0 {
        if let Some(win) = window {
            debug_assert!(n <= DECODE_BUFFER_SIZE);
            debug_assert!(win.len() >= overlap && overlap <= n);
            xx[..n].copy_from_slice(&x[..n]);
            for i in 0..overlap {
                xx[i] *= win[i];
                xx[n - overlap + i] *= win[overlap - 1 - i];
            }
            &xx[..n]
        } else {
            &x[..n]
        }
    } else {
        &x[..n]
    };

    let fast_n = n - lag;
    for k in 0..=lag {
        let mut d = 0.0f32;
        for i in k..fast_n + k {
            d += buf[i] * buf[i - k];
        }
        for i in fast_n + k..n {
            d += buf[i] * buf[i - k];
        }
        ac[k] = d;
    }
    return;
}

/// Solves the Yule-Walker equations via Levinson-Durbin recursion to obtain `p` Linear Predictive
/// Coding (LPC) filter coefficients `lpc[..p]` from autocorrelation values `ac[..=p]`
/// (`_celt_lpc` in `celt_lpc.c`).
pub fn celt_lpc(lpc: &mut [f32], ac: &[f32], p: usize) {
    debug_assert!(lpc.len() >= p);
    debug_assert!(ac.len() > p);

    lpc[..p].fill(0.0);
    if ac[0] <= 1.0e-10 {
        return;
    }

    let mut error = ac[0];
    for i in 0..p {
        let mut rr = 0.0f32;
        for j in 0..i {
            rr += lpc[j] * ac[i - j];
        }
        rr += ac[i + 1];
        let r = -rr / error;
        lpc[i] = r;

        let half = (i + 1) >> 1;
        for j in 0..half {
            let tmp1 = lpc[j];
            let tmp2 = lpc[i - 1 - j];
            lpc[j] = tmp1 + r * tmp2;
            lpc[i - 1 - j] = tmp2 + r * tmp1;
        }

        error -= r * r * error;
        // Stop early if prediction residual energy drops below 0.1% of `ac[0]`
        if error <= 0.001 * ac[0] {
            break;
        }
    }
    return;
}

/// Applies the `ord`-tap causal FIR analysis filter (`celt_fir_c` in `celt_lpc.c`):
/// $$y[i] = x[i] + \sum_{j=0}^{\text{ord}-1} \text{num}[j]\,x[i - 1 - j]$$
///
/// Note: `x_with_history` has length `ord + n`, where index `ord` corresponds to `x[0]` and
/// indices `0..ord` hold the `ord` past input samples `x[-ord .. -1]`.
pub fn celt_fir(x_with_history: &[f32], num: &[f32], y: &mut [f32], n: usize, ord: usize) {
    debug_assert!(x_with_history.len() >= ord + n);
    debug_assert!(num.len() >= ord);
    debug_assert!(y.len() >= n);

    for i in 0..n {
        let mut sum = x_with_history[ord + i];
        for j in 0..ord {
            sum += num[j] * x_with_history[ord + i - 1 - j];
        }
        y[i] = sum;
    }
    return;
}

/// Applies the `ord`-pole all-pole IIR synthesis filter (`celt_iir` in `celt_lpc.c`):
/// $$y[i] = x[i] - \sum_{j=0}^{\text{ord}-1} \text{den}[j]\,y[i - 1 - j]$$
///
/// `mem[0..ord]` stores the `ord` most recent outputs in reverse chronological order
/// (`mem[0] = y[-1]`, `mem[1] = y[-2]`, ..., `mem[ord - 1] = y[-ord]`) and is updated in-place.
pub fn celt_iir(
    x: &[f32],
    den: &[f32],
    y: &mut [f32],
    n: usize,
    ord: usize,
    mem: &mut [f32],
) {
    debug_assert!(x.len() >= n);
    debug_assert!(den.len() >= ord);
    debug_assert!(y.len() >= n);
    debug_assert!(mem.len() >= ord);

    for i in 0..n {
        let mut sum = x[i];
        for j in 0..ord {
            sum -= den[j] * mem[j];
        }
        for j in (1..ord).rev() {
            mem[j] = mem[j - 1];
        }
        if ord > 0 {
            mem[0] = sum;
        }
        y[i] = sum;
    }
    return;
}

/// Downsamples 1 or 2 channels of 48 kHz CELT history by 2x and whitens the spectrum via a
/// 4th-order LPC analysis filter (`pitch_downsample` in `pitch.c`).
pub fn pitch_downsample(x: &[&[f32]], x_lp: &mut [f32], len: usize, channels: usize) {
    let half_len = len >> 1;
    debug_assert!(!x.is_empty() && x[0].len() >= len);
    debug_assert!(x_lp.len() >= half_len);

    x_lp[0] = 0.25 * x[0][1] + 0.5 * x[0][0];
    for i in 1..half_len {
        x_lp[i] = 0.25 * (x[0][2 * i - 1] + x[0][2 * i + 1]) + 0.5 * x[0][2 * i];
    }

    if channels == 2 && x.len() >= 2 {
        debug_assert!(x[1].len() >= len);
        x_lp[0] += 0.25 * x[1][1] + 0.5 * x[1][0];
        for i in 1..half_len {
            x_lp[i] += 0.25 * (x[1][2 * i - 1] + x[1][2 * i + 1]) + 0.5 * x[1][2 * i];
        }
    }

    let mut ac = [0.0f32; 5];
    celt_autocorr(x_lp, &mut ac, None, 0, 4, half_len);

    // Lag windowing and bandwidth expansion (`lpc_bw = 0.9^k` and `0.8` first-difference blend)
    ac[0] *= 1.0001;
    for (i, ac_slot) in ac.iter_mut().enumerate().skip(1) {
        let frac = 0.008 * (i as f32);
        *ac_slot -= *ac_slot * frac * frac;
    }

    let mut lpc = [0.0f32; 4];
    celt_lpc(&mut lpc, &ac, 4);

    let mut tmp = 1.0f32;
    for coeff in &mut lpc {
        tmp *= 0.9;
        *coeff *= tmp;
    }

    let mut lpc2 = [0.0f32; 5];
    let c1 = 0.8f32;
    lpc2[0] = lpc[0] + c1;
    lpc2[1] = lpc[1] + c1 * lpc[0];
    lpc2[2] = lpc[2] + c1 * lpc[1];
    lpc2[3] = lpc[3] + c1 * lpc[2];
    lpc2[4] = c1 * lpc[3];

    // Apply 5-tap in-place FIR whitening filter using a 5-sample history shift register
    let mut mem = [0.0f32; 5];
    for sample in &mut x_lp[..half_len] {
        let x0 = *sample;
        let out = x0
            + lpc2[0] * mem[0]
            + lpc2[1] * mem[1]
            + lpc2[2] * mem[2]
            + lpc2[3] * mem[3]
            + lpc2[4] * mem[4];
        mem[4] = mem[3];
        mem[3] = mem[2];
        mem[2] = mem[1];
        mem[1] = mem[0];
        mem[0] = x0;
        *sample = out;
    }
    return;
}

/// Computes the normalized open-loop pitch lag in 48 kHz samples via two-stage decimated
/// cross-correlation (`pitch_search` in `pitch.c`).
pub fn pitch_search(x_lp: &[f32], y: &[f32], len: usize, max_pitch: usize) -> usize {
    let lag = len + max_pitch;
    let len_quarter = len >> 2;
    let lag_quarter = lag >> 2;
    let max_pitch_quarter = max_pitch >> 2;

    let mut x_lp4 = [0.0f32; DECODE_BUFFER_SIZE >> 2];
    let mut y_lp4 = [0.0f32; DECODE_BUFFER_SIZE >> 2];
    for j in 0..len_quarter {
        x_lp4[j] = x_lp[2 * j];
    }
    for j in 0..lag_quarter {
        y_lp4[j] = y[2 * j];
    }

    let mut xcorr = [0.0f32; PLC_PITCH_LAG_MAX >> 1];
    for i in 0..max_pitch_quarter {
        let mut sum = 0.0f32;
        for j in 0..len_quarter {
            sum += x_lp4[j] * y_lp4[i + j];
        }
        xcorr[i] = sum.max(-1.0);
    }

    // Coarse 2-candidate search on 4x-decimated correlation
    let mut best_num = [-1.0f32, -1.0f32];
    let mut best_ener = [0.0f32, 1.0f32];
    let mut best_pitch = [0usize, 1usize];

    let mut syy = 1.0f32;
    for &yv in &y_lp4[..len_quarter] {
        syy += yv * yv;
    }

    for i in 0..max_pitch_quarter {
        if xcorr[i] > 0.0 {
            let xcorr16 = xcorr[i] * 1.0e-12;
            let num = xcorr16 * xcorr16;
            if num * best_ener[1] > best_num[1] * syy {
                if num * best_ener[0] > best_num[0] * syy {
                    best_num[1] = best_num[0];
                    best_ener[1] = best_ener[0];
                    best_pitch[1] = best_pitch[0];
                    best_num[0] = num;
                    best_ener[0] = syy;
                    best_pitch[0] = i;
                } else {
                    best_num[1] = num;
                    best_ener[1] = syy;
                    best_pitch[1] = i;
                }
            }
        }
        syy += y_lp4[i + len_quarter] * y_lp4[i + len_quarter] - y_lp4[i] * y_lp4[i];
        syy = syy.max(1.0);
    }

    best_pitch[0] *= 2;
    best_pitch[1] *= 2;

    // Fine refinement on 2x-decimated signal around the top 2 coarse candidates
    let half_max_pitch = max_pitch >> 1;
    let half_len = len >> 1;
    xcorr[..half_max_pitch].fill(0.0);

    for &candidate in &best_pitch {
        let lower = candidate.saturating_sub(2);
        let upper = (candidate + 2).min(half_max_pitch.saturating_sub(1));
        for i in lower..=upper {
            if xcorr[i] == 0.0 {
                let mut sum = 0.0f32;
                for j in 0..half_len {
                    sum += x_lp[j] * y[i + j];
                }
                xcorr[i] = sum.max(-1.0);
            }
        }
    }

    best_num = [-1.0, -1.0];
    best_ener = [0.0, 1.0];
    best_pitch = [0, 1];

    syy = 1.0;
    for &yv in &y[..half_len] {
        syy += yv * yv;
    }

    for i in 0..half_max_pitch {
        if xcorr[i] > 0.0 {
            let xcorr16 = xcorr[i] * 1.0e-12;
            let num = xcorr16 * xcorr16;
            if num * best_ener[1] > best_num[1] * syy {
                if num * best_ener[0] > best_num[0] * syy {
                    best_num[1] = best_num[0];
                    best_ener[1] = best_ener[0];
                    best_pitch[1] = best_pitch[0];
                    best_num[0] = num;
                    best_ener[0] = syy;
                    best_pitch[0] = i;
                } else {
                    best_num[1] = num;
                    best_ener[1] = syy;
                    best_pitch[1] = i;
                }
            }
        }
        syy += y[i + half_len] * y[i + half_len] - y[i] * y[i];
        syy = syy.max(1.0);
    }

    // Sub-sample parabolic interpolation offset in 48 kHz samples
    let p0 = best_pitch[0];
    let offset: isize = if p0 > 0 && p0 + 1 < half_max_pitch {
        let a = xcorr[p0 - 1];
        let b = xcorr[p0];
        let c = xcorr[p0 + 1];
        if (c - a) > 0.7 * (b - a) {
            1
        } else if (a - c) > 0.7 * (b - c) {
            -1
        } else {
            0
        }
    } else {
        0
    };

    return ((2 * p0) as isize - offset) as usize;
}

/// Estimates the 48 kHz pitch period (`100..=720` samples) over the `DECODE_BUFFER_SIZE = 2048`
/// synthesis history buffer for CELT Packet Loss Concealment (`celt_plc_pitch_search` in
/// `celt_decoder.c`).
pub fn plc_pitch_search(decode_mem: &[&[f32]], channels: usize) -> usize {
    let mut lp_pitch_buf = [0.0f32; DECODE_BUFFER_SIZE >> 1];
    pitch_downsample(decode_mem, &mut lp_pitch_buf, DECODE_BUFFER_SIZE, channels);
    let raw_index = pitch_search(
        &lp_pitch_buf[(PLC_PITCH_LAG_MAX >> 1)..],
        &lp_pitch_buf,
        DECODE_BUFFER_SIZE - PLC_PITCH_LAG_MAX,
        PLC_PITCH_LAG_MAX - PLC_PITCH_LAG_MIN,
    );
    return PLC_PITCH_LAG_MAX - raw_index;
}

#[cfg(test)]
mod tests {
    use super::*;

    const MDCT_LM0_HEAD16: [f32; 16] = [
        -1.50047092e+01, -1.44652042e+01, -1.41141529e+01, -1.33143787e+01, -1.33589687e+01,
        -1.20505171e+01, -1.27275190e+01, -1.06798096e+01, -1.22094326e+01, -9.20788765e+00,
        -1.17954483e+01, -7.63997269e+00, -1.14772825e+01, -5.98104954e+00, -1.12475262e+01,
        -4.23598576e+00,
    ];
    const MDCT_LM0_TAIL16: [f32; 16] = [
        -1.46367065e+02, 4.53425476e+02, -1.56168579e+02, 6.15652649e+02, -1.63409241e+02,
        9.71117676e+02, -1.50936142e+02, 2.64755811e+03, -1.08150708e+03, 1.24496307e+01,
        5.09034958e+01, -2.75333521e+03, -4.31760834e+02, -1.01307275e+03, -5.00314789e+02,
        -6.55267334e+02,
    ];
    const MDCT_LM1_HEAD16: [f32; 16] = [
        -1.50002327e+01, -1.45001745e+01, -1.40056448e+01, -1.35006638e+01, -1.30176716e+01,
        -1.25008545e+01, -1.20354853e+01, -1.15000381e+01, -1.10582180e+01, -1.04975100e+01,
        -1.00850306e+01, -9.49263287e+00, -9.11521339e+00, -8.48492908e+00, -8.14817715e+00,
        -7.47409344e+00,
    ];
    const MDCT_LM1_TAIL16: [f32; 16] = [
        2.17569385e+03, -4.46059229e+03, 8.52720581e+02, -1.14809143e+03, 3.91139453e+03,
        -3.23127271e+03, -3.02264160e+03, -9.07227417e+02, -1.26994727e+03, -7.13742126e+02,
        -8.75307190e+02, -6.32894043e+02, -7.11847656e+02, -6.00163269e+02, -6.33548462e+02,
        -6.00487183e+02,
    ];
    const MDCT_LM2_HEAD16: [f32; 16] = [
        -1.50000563e+01, -1.45005207e+01, -1.40013313e+01, -1.35026712e+01, -1.30040188e+01,
        -1.25061197e+01, -1.20076694e+01, -1.15103779e+01, -1.10117121e+01, -1.05148849e+01,
        -1.00158806e+01, -9.51929474e+00, -9.01932430e+00, -8.52315521e+00, -8.02253914e+00,
        -7.52627325e+00,
    ];
    const MDCT_LM2_TAIL16: [f32; 16] = [
        -7.21356079e+02, -1.26638721e+03, -6.70597534e+02, -1.02842932e+03, -6.35179688e+02,
        -8.81416992e+02, -6.11057312e+02, -7.83529358e+02, -5.95952454e+02, -7.15493530e+02,
        -5.88589050e+02, -6.67269104e+02, -5.88383667e+02, -6.33173462e+02, -5.95304321e+02,
        -6.09855835e+02,
    ];
    const MDCT_LM3_HEAD16: [f32; 16] = [
        -1.49997997e+01, -1.45022984e+01, -1.39949627e+01, -1.35123920e+01, -1.29834843e+01,
        -1.25302134e+01, -1.19650917e+01, -1.15553923e+01, -1.09393330e+01, -1.05873041e+01,
        -9.90613651e+00, -9.62557697e+00, -8.86502838e+00, -8.66994095e+00, -7.81631708e+00,
        -7.72014618e+00,
    ];
    const MDCT_LM3_TAIL16: [f32; 16] = [
        -7.30537170e+02, -6.16916321e+02, -7.02654968e+02, -6.08573608e+02, -6.79261169e+02,
        -6.02615784e+02, -6.59655640e+02, -5.98849182e+02, -6.43320679e+02, -5.97155090e+02,
        -6.29858215e+02, -5.97467651e+02, -6.18967529e+02, -5.99762329e+02, -6.10424438e+02,
        -6.04058960e+02,
    ];
    const MDCT_TRANSIENT_LM2_HEAD16: [f32; 16] = [
        -5.00217628e+00, -4.72963524e+00, -4.55283356e+00, -4.14251900e+00, -4.16643524e+00,
        -3.49234247e+00, -3.83798695e+00, -2.78429985e+00, -3.56311727e+00, -2.02296376e+00,
        -3.33797598e+00, -1.21241462e+00, -3.15926170e+00, -3.56351733e-01, -3.02402997e+00,
        5.41596413e-01,
    ];
    const MDCT_TRANSIENT_LM2_MID16: [f32; 16] = [
        -3.75002747e+01, 3.42244339e+01, -3.89258652e+01, 3.54061050e+01, -4.05616074e+01,
        3.67696152e+01, -4.24255409e+01, 3.83231010e+01, -4.45367813e+01, 4.00791016e+01,
        -4.69242897e+01, 4.20544357e+01, -4.96224899e+01, 4.42736397e+01, -5.26780739e+01,
        4.67659111e+01,
    ];
    const MDCT_TRANSIENT_LM2_TAIL16: [f32; 16] = [
        1.01483417e+03, -1.87615341e+02, 3.89769623e+02, -7.71064148e+01, 2.34954895e+02,
        -3.44625282e+01, 1.64079422e+02, -1.02508965e+01, 1.22858620e+02, 6.70582485e+00,
        9.53674622e+01, 2.04601460e+01, 7.51909943e+01, 3.29818230e+01, 5.91826859e+01,
        4.55284805e+01,
    ];
    const COMB_FILTER_TRANSITION_HEAD16: [f32; 16] = [
        -2.52808472e+02, -2.58464203e+02, -2.59383698e+02, -2.55522751e+02, -2.47017944e+02,
        -2.34177368e+02, -2.17467178e+02, -1.97491516e+02, -1.74957397e+02, -1.50643997e+02,
        -1.25367424e+02, -9.99369125e+01, -7.51180267e+01, -5.15998192e+01, -2.99595184e+01,
        -1.06409883e+01,
    ];
    const COMB_FILTER_TRANSITION_STEADY16: [f32; 16] = [
        7.24783325e+01, 6.39892960e+01, 5.28281555e+01, 3.91309052e+01, 2.31589203e+01,
        5.30031013e+00, -1.39564381e+01, -3.40370178e+01, -5.43131485e+01, -7.41294785e+01,
        -9.28349457e+01, -1.09814491e+02, -1.24519028e+02, -1.36492706e+02, -1.45384567e+02,
        -1.50985382e+02,
    ];
    const LPC24_ORACLE: [f32; 24] = [
        -1.96576309e+00, 9.91539001e-01, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    ];
    const FIR_OUT16_ORACLE: [f32; 16] = [
        -1.59914970e+00, -1.15696335e+00, -6.23615265e-01, -5.57785034e-02, 4.86587524e-01,
        9.47113037e-01, 1.27740479e+00, 1.44265747e+00, 1.42449951e+00, 1.22282410e+00,
        8.56124878e-01, 3.59603882e-01, -2.18490601e-01, -8.20785522e-01, -1.38717651e+00,
        -1.85980988e+00,
    ];
    const IIR_OUT16_ORACLE: [f32; 16] = [
        -9.59978676e+00, -3.19644890e+01, -5.39396667e+01, -7.43943481e+01, -9.22717972e+01,
        -1.06672478e+02, -1.16924332e+02, -1.22632957e+02, -1.23707802e+02, -1.20362053e+02,
        -1.13086052e+02, -1.02597115e+02, -8.97708817e+01, -7.55600357e+01, -6.09089737e+01,
        -4.66716995e+01,
    ];
    const IIR_MEM24_ORACLE: [f32; 24] = [
        -4.55824547e+01, -4.89341164e+01, -5.14271545e+01, -5.37007256e+01, -5.63234367e+01,
        -5.97234650e+01, -6.41346588e+01, -6.95632324e+01, -7.57788315e+01, -8.23309860e+01,
        -8.85891266e+01, -9.38030014e+01, -9.71768417e+01, -9.79515305e+01, -9.54852905e+01,
        -8.93258896e+01, -7.92673798e+01, -6.53853073e+01, -4.80463791e+01, -2.78929291e+01,
        -5.80063629e+00, 1.71837959e+01, 3.99220276e+01, 6.12720642e+01,
    ];

    fn assert_f32_slice_close(actual: &[f32], expected: &[f32], rel_tol: f32, abs_tol: f32) {
        assert_eq!(actual.len(), expected.len());
        for (idx, (&a, &e)) in actual.iter().zip(expected.iter()).enumerate() {
            let diff = (a - e).abs();
            let limit = abs_tol.max(e.abs() * rel_tol);
            assert!(
                diff <= limit,
                "mismatch at index {}: actual = {:.8e}, expected = {:.8e}, diff = {:.8e}, limit = {:.8e}",
                idx,
                a,
                e,
                diff,
                limit
            );
        }
        return;
    }

    #[test]
    fn test_clt_mdct_backward_all_block_sizes_against_libopus_oracle() {
        let heads = [&MDCT_LM0_HEAD16, &MDCT_LM1_HEAD16, &MDCT_LM2_HEAD16, &MDCT_LM3_HEAD16];
        let tails = [&MDCT_LM0_TAIL16, &MDCT_LM1_TAIL16, &MDCT_LM2_TAIL16, &MDCT_LM3_TAIL16];

        for lm in 0..=3 {
            let n_bins = 120usize << lm;
            let mut in_freq = [0.0f32; 960];
            let mut out_buf = [0.0f32; 960 + 120];

            for (i, slot) in in_freq[..n_bins].iter_mut().enumerate() {
                *slot = ((i + 1) as f32 * 0.17).sin() * ((i as f32) * 0.03).cos() * 100.0;
            }
            for (i, slot) in out_buf[..60].iter_mut().enumerate() {
                *slot = ((i as isize - 30) as f32) * 0.5;
            }

            mdct_backward_frame(&in_freq[..n_bins], &mut out_buf, lm, false);

            assert_f32_slice_close(&out_buf[..16], heads[lm], 1.0e-5, 1.0e-3);
            assert_f32_slice_close(
                &out_buf[n_bins + 60 - 16..n_bins + 60],
                tails[lm],
                1.0e-5,
                1.0e-2,
            );
        }
        return;
    }

    #[test]
    fn test_clt_mdct_backward_transient_short_blocks_against_libopus_oracle() {
        let lm = 2usize;
        let n_bins = 120usize << lm;
        let mut in_freq = [0.0f32; 480];
        let mut out_buf = [0.0f32; 480 + 120];

        for (i, slot) in in_freq.iter_mut().enumerate() {
            *slot = ((i + 3) as f32 * 0.11).cos() * 50.0;
        }
        for (i, slot) in out_buf[..60].iter_mut().enumerate() {
            *slot = (i as f32) * 0.25 - 5.0;
        }

        mdct_backward_frame(&in_freq, &mut out_buf, lm, true);

        assert_f32_slice_close(&out_buf[..16], &MDCT_TRANSIENT_LM2_HEAD16, 1.0e-5, 1.0e-3);
        assert_f32_slice_close(&out_buf[240..256], &MDCT_TRANSIENT_LM2_MID16, 1.0e-5, 1.0e-3);
        assert_f32_slice_close(
            &out_buf[n_bins + 60 - 16..n_bins + 60],
            &MDCT_TRANSIENT_LM2_TAIL16,
            1.0e-5,
            1.0e-2,
        );
        return;
    }

    #[test]
    fn test_comb_filter_transition_and_steady_against_libopus_oracle() {
        let mut buf = [0.0f32; COMBFILTER_MAXPERIOD + 240];
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = (i as f32 * 0.09).sin() * 200.0 + (i as f32 * 0.23).cos() * 80.0;
        }

        let mut out = [0.0f32; 240];
        comb_filter(
            &mut out,
            &buf,
            COMBFILTER_MAXPERIOD,
            120,
            180,
            0.375,
            0.5625,
            0,
            2,
            &CELT_WINDOW,
            OVERLAP,
            240,
        );

        assert_f32_slice_close(&out[..16], &COMB_FILTER_TRANSITION_HEAD16, 1.0e-5, 1.0e-4);
        assert_f32_slice_close(&out[120..136], &COMB_FILTER_TRANSITION_STEADY16, 1.0e-5, 1.0e-4);

        // Verify in-place variant produces identical results
        let mut buf_in_place = buf;
        comb_filter_in_place(
            &mut buf_in_place,
            COMBFILTER_MAXPERIOD,
            120,
            180,
            0.375,
            0.5625,
            0,
            2,
            OVERLAP,
            240,
        );
        assert_f32_slice_close(
            &buf_in_place[COMBFILTER_MAXPERIOD..COMBFILTER_MAXPERIOD + 240],
            &out,
            1.0e-6,
            1.0e-5,
        );
        return;
    }

    #[test]
    fn test_deemphasis_48khz_and_decimated_rates() {
        let n = 48;
        let mut input = [0.0f32; 48];
        for (i, slot) in input.iter_mut().enumerate() {
            *slot = ((i as f32) * 0.25).sin() * 16384.0;
        }

        // Full 48 kHz (`downsample = 1`)
        let mut pcm_48k = [0.0f32; 48];
        let mut mem_48k = 0.0f32;
        let written_48k = deemphasis_channel(&input, &mut pcm_48k, n, 1, &mut mem_48k);
        assert_eq!(written_48k, 48);

        // Decimated 16 kHz (`downsample = 3`) should keep every 3rd sample of the 48 kHz filter
        let mut pcm_16k = [0.0f32; 16];
        let mut mem_16k = 0.0f32;
        let written_16k = deemphasis_channel(&input, &mut pcm_16k, n, 3, &mut mem_16k);
        assert_eq!(written_16k, 16);
        assert!((mem_48k - mem_16k).abs() < 1.0e-5);
        for k in 0..16 {
            assert!((pcm_16k[k] - pcm_48k[3 * k]).abs() < 1.0e-7);
        }
        return;
    }

    #[test]
    fn test_celt_lpc_fir_iir_and_plc_pitch_against_libopus_oracle() {
        let mut sig = [0.0f32; 512];
        for (i, slot) in sig.iter_mut().enumerate() {
            *slot = (i as f32 * 0.14).sin() * 100.0 + 0.5 * (i as f32 * 0.31).cos() * 60.0;
        }

        let mut ac = [0.0f32; LPC_ORDER + 1];
        let mut lpc = [0.0f32; LPC_ORDER];
        celt_autocorr(&sig, &mut ac, Some(&CELT_WINDOW), OVERLAP, LPC_ORDER, 512);
        celt_lpc(&mut lpc, &ac, LPC_ORDER);
        assert_f32_slice_close(&lpc, &LPC24_ORACLE, 1.0e-5, 1.0e-4);

        let mut fir_out = [0.0f32; 64];
        celt_fir(&sig[..LPC_ORDER + 64], &lpc, &mut fir_out, 64, LPC_ORDER);
        assert_f32_slice_close(&fir_out[..16], &FIR_OUT16_ORACLE, 1.0e-5, 5.0e-4);

        let mut iir_mem = [0.0f32; LPC_ORDER];
        for i in 0..LPC_ORDER {
            iir_mem[i] = sig[LPC_ORDER - 1 - i];
        }
        let mut iir_out = [0.0f32; 64];
        celt_iir(&fir_out, &lpc, &mut iir_out, 64, LPC_ORDER, &mut iir_mem);
        assert_f32_slice_close(&iir_out[..16], &IIR_OUT16_ORACLE, 1.0e-4, 5.0e-3);
        assert_f32_slice_close(&iir_mem, &IIR_MEM24_ORACLE, 1.0e-4, 5.0e-3);

        // Verify PLC pitch search on a 200-sample periodic stereo history (harmonics at 200, 400, 600)
        let mut decode_mem0 = [0.0f32; DECODE_BUFFER_SIZE];
        let mut decode_mem1 = [0.0f32; DECODE_BUFFER_SIZE];
        for i in 0..DECODE_BUFFER_SIZE {
            let phase = 2.0 * 3.14159265f32 * (i as f32) / 200.0;
            decode_mem0[i] = phase.sin() * 500.0 + 0.3 * (2.0 * phase).sin() * 500.0;
            decode_mem1[i] = decode_mem0[i] * 0.8;
        }
        let history_refs: [&[f32]; 2] = [&decode_mem0, &decode_mem1];
        let pitch_period = plc_pitch_search(&history_refs, 2);
        assert!(
            pitch_period == 200 || pitch_period == 400 || pitch_period == 600,
            "expected harmonic of 200, got {}",
            pitch_period
        );

        // Verify unique fundamental pitch period (400 samples, since 2 * 400 = 800 > 720)
        for i in 0..DECODE_BUFFER_SIZE {
            let phase = 2.0 * std::f32::consts::PI * (i as f32) / 400.0;
            decode_mem0[i] = (phase.sin()
                + 0.7 * (2.0 * phase).cos()
                + 0.5 * (3.0 * phase).sin()
                + 0.4 * (4.0 * phase).cos()
                + 0.3 * (5.0 * phase).sin())
                * 300.0;
            decode_mem1[i] = decode_mem0[i];
        }
        let history_refs_400: [&[f32]; 2] = [&decode_mem0, &decode_mem1];
        let pitch_period_400 = plc_pitch_search(&history_refs_400, 2);
        assert_eq!(pitch_period_400, 400);
        return;
    }

    #[test]
    fn test_denormalise_bands_silence_and_nyquist_bound() {
        let mut norm_x = [1.0f32; 240];
        let mut freq = [99.0f32; 240];
        let mut band_log_e = [0.0f32; MAX_BANDS];
        band_log_e[0] = 1.0 - E_MEANS[0];

        denormalise_bands(&norm_x, &mut freq, &band_log_e, 0, 21, 1, 1, true);
        assert!(freq.iter().all(|&v| v == 0.0));

        norm_x.fill(1.0);
        denormalise_bands(&norm_x, &mut freq, &band_log_e, 0, 21, 1, 2, false);
        // Band 0 (`0..2` at `lm = 1`) has `lg = 1.0 -> gain = 2^1 = 2.0`
        assert!((freq[0] - 2.0).abs() < 1.0e-5);
        assert!((freq[1] - 2.0).abs() < 1.0e-5);
        // Above `n / downsample = 120` must be zeroed
        assert!(freq[120..].iter().all(|&v| v == 0.0));
        return;
    }
}
