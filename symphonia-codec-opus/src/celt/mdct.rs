//! CELT inverse MDCT with TDAC windowing and overlap-add
//! ([RFC 6716 Section 4.3.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7>)).

use std::sync::OnceLock;

use crate::celt::constant::{CELT_WINDOW, OVERLAP};
use crate::celt::fft::{Complex32, MixedRadixPlan};
use crate::celt::{BlockSize, MAX_FRAME_SIZE, MAX_LM, SHORT_MDCT_SIZE};

/// Maximum full-window MDCT length at 48 kHz (`2 * MAX_FRAME_SIZE = 1920` samples).
pub(crate) const MDCT_MAX_N: usize = 2 * MAX_FRAME_SIZE;

/// Maximum complex quarter-length IFFT size (`MDCT_MAX_N / 4 = 480` complex points).
pub(crate) const MDCT_MAX_N4: usize = MDCT_MAX_N / 4;

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

/// Initializes and returns the singleton [`MdctTables`] for 48 kHz CELT.
fn mdct_tables() -> &'static MdctTables {
    static TABLES: OnceLock<MdctTables> = OnceLock::new();
    return TABLES.get_or_init(|| {
        let fft_twiddles = std::array::from_fn(|k| {
            let phase = (-2.0 * std::f64::consts::PI * (k as f64)) / (MDCT_MAX_N4 as f64);
            Complex32::new(phase.cos() as f32, phase.sin() as f32)
        });

        let mut bitrev = [[0u16; MDCT_MAX_N4]; 4];
        let mut trig = [[0.0f32; 2 * MDCT_MAX_N4]; 4];

        for shift in 0..4 {
            let n = MDCT_MAX_N >> shift;
            let n4 = n >> 2;
            MixedRadixPlan::for_shift(shift).digit_reversal(&mut bitrev[shift][..n4]);

            for i in 0..n4 {
                let angle = (2.0 * std::f64::consts::PI * ((i as f64) + 0.125)) / (n as f64);
                trig[shift][i] = angle.cos() as f32;
                trig[shift][n4 + i] = (-angle.sin()) as f32;
            }
        }

        return MdctTables { fft_twiddles, bitrev, trig };
    });
}

/// Computes one CELT inverse MDCT block and applies the symmetric 120-sample TDAC window with
/// overlap-add ([RFC 6716 Section 4.3.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7>)).
///
/// # Parameters
/// - `input`: Frequency-domain MDCT coefficients. Read with step `stride` (`n2 = 960 >> shift`
///   coefficients at indices `0, stride, 2 * stride, ..., (n2 - 1) * stride`).
/// - `out`: Time-domain synthesis buffer of length at least `n2 + OVERLAP / 2` (`n2 + 60`).
///   On entry, `out[0 .. OVERLAP / 2]` (`out[0..60]`) MUST contain the unwindowed tail from the
///   previous MDCT block. On return, `out[0 .. n2]` contains the completed, overlapped time-domain
///   samples for this block, and `out[n2 .. n2 + OVERLAP / 2]` (`out[n2 .. n2 + 60]`) holds the
///   new unwindowed tail for the next block.
/// - `window`: The 120-sample symmetric power-complementary MDCT window ([`CELT_WINDOW`]).
/// - `shift`: Block-size right-shift (`0..=3`, where `shift = 3 - lm` for a single long block
///   of `n2 = 120 << lm` bins, or `shift = 3` for transient short blocks of `n2 = 120` bins).
/// - `stride`: Input stride (`1` for non-transient frames, or `B = 1 << lm` for interleaved
///   transient short blocks).
pub(crate) fn imdct_overlap_add(
    input: &[f32],
    out: &mut [f32],
    window: &[f32; OVERLAP],
    shift: usize,
    stride: usize,
) {
    debug_assert!(shift <= 3);
    let tables = mdct_tables();
    let n = MDCT_MAX_N >> shift;
    let n2 = n >> 1;
    let n4 = n >> 2;
    let half_overlap = OVERLAP >> 1;

    debug_assert!(out.len() >= n2 + half_overlap);
    debug_assert!(input.len() > (n2 - 1) * stride);

    let trig = &tables.trig[shift];

    // Step 1: Pre-rotate input frequency coefficients and scatter into the digit-reversed buffer.
    let mut fft_buf = [Complex32::default(); MDCT_MAX_N4];
    pre_rotate(input, stride, n4, trig, &tables.bitrev[shift], &mut fft_buf);

    // Step 2: Quarter-length (`N / 4`) mixed-radix complex FFT.
    MixedRadixPlan::for_shift(shift).execute(&mut fft_buf[..n4], &tables.fft_twiddles);

    // Step 3: Post-rotate and de-interleave into `out[half_overlap .. half_overlap + n2]`.
    post_rotate(&fft_buf[..n4], trig, &mut out[half_overlap..half_overlap + n2]);

    // Step 4: Symmetric TDAC windowing and overlap-add on `out[0 .. OVERLAP]`.
    window_overlap_add(&mut out[..OVERLAP], window);
    return;
}

/// Pre-rotates the `n2 = 2 * n4` strided MDCT coefficients of `input` into the quarter-length
/// complex buffer `fft_buf`, scattered through the digit-reversal table `bitrev`.
///
/// Real and imaginary components are swapped (`Complex32::new(yi, yr)`) so that the forward
/// complex DFT of [`MixedRadixPlan::execute`] computes an unscaled inverse DFT.
fn pre_rotate(
    input: &[f32],
    stride: usize,
    n4: usize,
    trig: &[f32],
    bitrev: &[u16],
    fft_buf: &mut [Complex32; MDCT_MAX_N4],
) {
    let n2 = 2 * n4;
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
    return;
}

/// Post-rotates the transformed quarter-length buffer `fft_buf[..n4]` and de-interleaves the
/// `n2 = 2 * n4` time-domain samples into `out[..n2]`.
fn post_rotate(fft_buf: &[Complex32], trig: &[f32], out: &mut [f32]) {
    let n4 = fft_buf.len();
    let n2 = 2 * n4;
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

        out[2 * i] = yr0;
        out[2 * i + 1] = yi1;
        out[n2 - 2 - 2 * i] = yr1;
        out[n2 - 1 - 2 * i] = yi0;
    }
    return;
}

/// Applies the symmetric TDAC window to the overlap region `out[..OVERLAP]`.
///
/// On entry, `out[0 .. OVERLAP / 2]` holds the previous block's unwindowed tail and
/// `out[OVERLAP / 2 .. OVERLAP]` holds the newly synthesized leading half-overlap.
fn window_overlap_add(out: &mut [f32], window: &[f32; OVERLAP]) {
    let half_overlap = OVERLAP >> 1;
    for i in 0..half_overlap {
        let x1 = out[OVERLAP - 1 - i];
        let x2 = out[i];
        let w1 = window[i];
        let w2 = window[OVERLAP - 1 - i];

        out[i] = x2 * w2 - x1 * w1;
        out[OVERLAP - 1 - i] = x2 * w1 + x1 * w2;
    }
    return;
}

/// Inverse-transforms one channel of a frame of block size `lm` into `out_syn` with overlap-add
/// ([RFC 6716 Section 4.3.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7>)):
/// one long MDCT, or in a transient frame `lm.blocks()` short MDCTs of [`SHORT_MDCT_SIZE`]
/// samples whose coefficients are interleaved in `freq`
/// ([Section 4.3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.1>)).
///
/// `freq` holds the `N = 120 << lm` denormalised coefficients. `out_syn` starts at the frame in
/// the channel's synthesis history and holds at least `N + OVERLAP / 2` samples.
pub(crate) fn mdct_backward_frame(
    freq: &[f32],
    out_syn: &mut [f32],
    lm: BlockSize,
    is_transient: bool,
) {
    let (block, count) = if is_transient { (BlockSize::new(0), lm.blocks()) } else { (lm, 1) };
    let shift = MAX_LM - block.log2();
    for i in 0..count {
        let out = &mut out_syn[(i * SHORT_MDCT_SIZE)..];
        imdct_overlap_add(&freq[i..], out, &CELT_WINDOW, shift, count);
    }
    return;
}

#[cfg(test)]
pub(crate) mod tests;
