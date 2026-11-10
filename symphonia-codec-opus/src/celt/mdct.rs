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
#[allow(clippy::excessive_precision)] // recorded values keep the 9 significant digits of the dump
pub(crate) mod tests {
    // Fixtures: packets and expected values recorded from libopus 1.6.1. The behaviour they pin is
    // normative (RFC 6716 Section 4.3.7) and is verified against the RFC 8251 test vectors by
    // `tests/rfc6716_conformance.rs`.
    use super::*;

    const MDCT_LM0_HEAD16: [f32; 16] = [
        -1.50047092e+01,
        -1.44652042e+01,
        -1.41141529e+01,
        -1.33143787e+01,
        -1.33589687e+01,
        -1.20505171e+01,
        -1.27275190e+01,
        -1.06798096e+01,
        -1.22094326e+01,
        -9.20788765e+00,
        -1.17954483e+01,
        -7.63997269e+00,
        -1.14772825e+01,
        -5.98104954e+00,
        -1.12475262e+01,
        -4.23598576e+00,
    ];
    const MDCT_LM0_TAIL16: [f32; 16] = [
        -1.46367065e+02,
        4.53425476e+02,
        -1.56168579e+02,
        6.15652649e+02,
        -1.63409241e+02,
        9.71117676e+02,
        -1.50936142e+02,
        2.64755811e+03,
        -1.08150708e+03,
        1.24496307e+01,
        5.09034958e+01,
        -2.75333521e+03,
        -4.31760834e+02,
        -1.01307275e+03,
        -5.00314789e+02,
        -6.55267334e+02,
    ];
    const MDCT_LM1_HEAD16: [f32; 16] = [
        -1.50002327e+01,
        -1.45001745e+01,
        -1.40056448e+01,
        -1.35006638e+01,
        -1.30176716e+01,
        -1.25008545e+01,
        -1.20354853e+01,
        -1.15000381e+01,
        -1.10582180e+01,
        -1.04975100e+01,
        -1.00850306e+01,
        -9.49263287e+00,
        -9.11521339e+00,
        -8.48492908e+00,
        -8.14817715e+00,
        -7.47409344e+00,
    ];
    const MDCT_LM1_TAIL16: [f32; 16] = [
        2.17569385e+03,
        -4.46059229e+03,
        8.52720581e+02,
        -1.14809143e+03,
        3.91139453e+03,
        -3.23127271e+03,
        -3.02264160e+03,
        -9.07227417e+02,
        -1.26994727e+03,
        -7.13742126e+02,
        -8.75307190e+02,
        -6.32894043e+02,
        -7.11847656e+02,
        -6.00163269e+02,
        -6.33548462e+02,
        -6.00487183e+02,
    ];
    const MDCT_LM2_HEAD16: [f32; 16] = [
        -1.50000563e+01,
        -1.45005207e+01,
        -1.40013313e+01,
        -1.35026712e+01,
        -1.30040188e+01,
        -1.25061197e+01,
        -1.20076694e+01,
        -1.15103779e+01,
        -1.10117121e+01,
        -1.05148849e+01,
        -1.00158806e+01,
        -9.51929474e+00,
        -9.01932430e+00,
        -8.52315521e+00,
        -8.02253914e+00,
        -7.52627325e+00,
    ];
    const MDCT_LM2_TAIL16: [f32; 16] = [
        -7.21356079e+02,
        -1.26638721e+03,
        -6.70597534e+02,
        -1.02842932e+03,
        -6.35179688e+02,
        -8.81416992e+02,
        -6.11057312e+02,
        -7.83529358e+02,
        -5.95952454e+02,
        -7.15493530e+02,
        -5.88589050e+02,
        -6.67269104e+02,
        -5.88383667e+02,
        -6.33173462e+02,
        -5.95304321e+02,
        -6.09855835e+02,
    ];
    const MDCT_LM3_HEAD16: [f32; 16] = [
        -1.49997997e+01,
        -1.45022984e+01,
        -1.39949627e+01,
        -1.35123920e+01,
        -1.29834843e+01,
        -1.25302134e+01,
        -1.19650917e+01,
        -1.15553923e+01,
        -1.09393330e+01,
        -1.05873041e+01,
        -9.90613651e+00,
        -9.62557697e+00,
        -8.86502838e+00,
        -8.66994095e+00,
        -7.81631708e+00,
        -7.72014618e+00,
    ];
    const MDCT_LM3_TAIL16: [f32; 16] = [
        -7.30537170e+02,
        -6.16916321e+02,
        -7.02654968e+02,
        -6.08573608e+02,
        -6.79261169e+02,
        -6.02615784e+02,
        -6.59655640e+02,
        -5.98849182e+02,
        -6.43320679e+02,
        -5.97155090e+02,
        -6.29858215e+02,
        -5.97467651e+02,
        -6.18967529e+02,
        -5.99762329e+02,
        -6.10424438e+02,
        -6.04058960e+02,
    ];
    const MDCT_TRANSIENT_LM2_HEAD16: [f32; 16] = [
        -5.00217628e+00,
        -4.72963524e+00,
        -4.55283356e+00,
        -4.14251900e+00,
        -4.16643524e+00,
        -3.49234247e+00,
        -3.83798695e+00,
        -2.78429985e+00,
        -3.56311727e+00,
        -2.02296376e+00,
        -3.33797598e+00,
        -1.21241462e+00,
        -3.15926170e+00,
        -3.56351733e-01,
        -3.02402997e+00,
        5.41596413e-01,
    ];
    const MDCT_TRANSIENT_LM2_MID16: [f32; 16] = [
        -3.75002747e+01,
        3.42244339e+01,
        -3.89258652e+01,
        3.54061050e+01,
        -4.05616074e+01,
        3.67696152e+01,
        -4.24255409e+01,
        3.83231010e+01,
        -4.45367813e+01,
        4.00791016e+01,
        -4.69242897e+01,
        4.20544357e+01,
        -4.96224899e+01,
        4.42736397e+01,
        -5.26780739e+01,
        4.67659111e+01,
    ];
    const MDCT_TRANSIENT_LM2_TAIL16: [f32; 16] = [
        1.01483417e+03,
        -1.87615341e+02,
        3.89769623e+02,
        -7.71064148e+01,
        2.34954895e+02,
        -3.44625282e+01,
        1.64079422e+02,
        -1.02508965e+01,
        1.22858620e+02,
        6.70582485e+00,
        9.53674622e+01,
        2.04601460e+01,
        7.51909943e+01,
        3.29818230e+01,
        5.91826859e+01,
        4.55284805e+01,
    ];

    pub(crate) fn assert_f32_slice_close(
        actual: &[f32],
        expected: &[f32],
        rel_tol: f32,
        abs_tol: f32,
    ) {
        assert_eq!(actual.len(), expected.len());
        for (i, (&a, &e)) in actual.iter().zip(expected.iter()).enumerate() {
            let diff = (a - e).abs();
            let limit = abs_tol.max(e.abs() * rel_tol);
            assert!(
                diff <= limit,
                "mismatch at index {}: actual = {:.8e}, expected = {:.8e}, diff = {:.8e}, limit = {:.8e}",
                i,
                a,
                e,
                diff,
                limit
            );
        }
        return;
    }

    #[test]
    fn imdct_overlap_add_all_block_sizes_match_recorded_values() {
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

            mdct_backward_frame(&in_freq[..n_bins], &mut out_buf, BlockSize::new(lm), false);

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
    fn imdct_overlap_add_transient_short_blocks_match_recorded_values() {
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

        mdct_backward_frame(&in_freq, &mut out_buf, BlockSize::new(lm), true);

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
}
