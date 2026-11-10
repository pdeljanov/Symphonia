//! CELT band denormalisation and de-emphasis
//! ([RFC 6716 Section 4.3.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.6>) and
//! [Section 4.3.7.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.2>)).
//!
//! The unit-norm band shapes are scaled back to full-scale MDCT coefficients by the decoded
//! band energies, and after the inverse MDCT the time signal passes through the first-order
//! de-emphasis filter that undoes the encoder's pre-emphasis, decimated to the output rate.
//!
//! ```text
//!   shapes (unit norm)   band energies      freq (MDCT bins)          pcm
//!   +--------------+     +-----------+      +------------------+      +-----------+
//!   | band 0 ..... |  x  | 2^(e + m) | ---> | 0 .. bound zeroed| ...  | y = x + a |
//!   | band i ..... |     | per band  |      | above Nyquist    |      |   * y[-1] |
//!   +--------------+     +-----------+      +------------------+      +-----------+
//! ```

use crate::celt::BlockSize;
use crate::celt::constant::{E_MEANS, EBANDS, MAX_BANDS, PREEMPH_COEF};

/// Full-scale amplitude of the CELT synthesis domain (`32768.0`): the decoder works on 16-bit
/// integer scale internally and divides by it when writing `f32` output.
pub(crate) const CELT_SIG_SCALE: f32 = 32768.0;

/// Reciprocal of [`CELT_SIG_SCALE`] used to normalize CELT synthesis output to `[-1.0, 1.0]`.
pub(crate) const INV_CELT_SIG_SCALE: f32 = 1.0 / CELT_SIG_SCALE;

/// Tiny bias added inside the de-emphasis recurrence so that the filter state never decays into
/// subnormal numbers (RFC 6716 Section 4.3.7.2).
const VERY_SMALL: f32 = 1.0e-30;

/// Band range, block size, output decimation and silence flag of one frame's denormalisation
/// ([RFC 6716 Section 4.3.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.6>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Denormalisation {
    /// First coded Bark band.
    pub(crate) start: usize,
    /// One past the last coded Bark band.
    pub(crate) end: usize,
    /// MDCT block size of the frame.
    pub(crate) lm: BlockSize,
    /// Integer decimation factor `48_000 / sample_rate`; bins above `N / downsample` are zeroed.
    pub(crate) downsample: usize,
    /// The frame is silent: every bin is zeroed.
    pub(crate) silence: bool,
}

/// Scales the unit-norm PVQ band vectors `norm_x` by their decoded log-energies `band_log_e`
/// into the full-scale MDCT coefficient buffer `freq`
/// ([RFC 6716 Section 4.3.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.6>)).
///
/// For each band `b` in `start..end`, the linear band amplitude is
/// `g_b = 2^min(bandLogE[b] + eMeans[b], 32)`. Bins below `start`, above `end`, or above the
/// downsampling Nyquist limit `N / downsample` are zeroed, and the entire spectrum is zeroed for
/// a silent frame.
pub(crate) fn denormalise_bands(
    norm_x: &[f32],
    freq: &mut [f32],
    band_log_e: &[f32; MAX_BANDS],
    layout: &Denormalisation,
) {
    let Denormalisation { start, end, lm, downsample, silence } = *layout;
    let m = lm.blocks();
    let n = lm.frame_samples();
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
        // Base-2 exponential `2^lg = exp(ln(2) * lg)` of the log-domain band energy.
        let gain = (std::f32::consts::LN_2 * lg).exp();
        for (dst, &src) in freq[j_start..band_end].iter_mut().zip(&norm_x[j_start..band_end]) {
            *dst = src * gain;
        }
    }

    freq[bound..n].fill(0.0);
    return;
}

/// Applies the one-pole de-emphasis filter `y[j] = x[j] + PREEMPH_COEF * y[j - 1]` to one
/// channel of `input`, keeps every `downsample`-th output sample, and scales the result by
/// `1 / CELT_SIG_SCALE` into `pcm_out`
/// ([RFC 6716 Section 4.3.7.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.2>)).
///
/// Returns the number of output samples written to `pcm_out` (`input.len() / downsample`).
pub(crate) fn deemphasis_channel(
    input: &[f32],
    pcm_out: &mut [f32],
    downsample: usize,
    preemph_mem: &mut f32,
) -> usize {
    let n = input.len();
    debug_assert!(downsample >= 1);
    let out_len = n / downsample;
    debug_assert!(pcm_out.len() >= out_len);

    let mut m = *preemph_mem;
    for (block, dst) in input.chunks_exact(downsample).zip(pcm_out[..out_len].iter_mut()) {
        *dst = deemphasize(block[0], &mut m) * INV_CELT_SIG_SCALE;
        block[1..].iter().for_each(|&x| _ = deemphasize(x, &mut m));
    }
    *preemph_mem = m;
    return out_len;
}

/// One step of the first-order de-emphasis filter `y = x + m`, `m = PREEMPH_COEF * y`
/// ([RFC 6716 Section 4.3.7.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.2>)).
#[inline]
fn deemphasize(x: f32, memory: &mut f32) -> f32 {
    let y = x + VERY_SMALL + *memory;
    *memory = PREEMPH_COEF * y;
    return y;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deemphasis_48khz_and_decimated_rates() {
        let mut input = [0.0f32; 48];
        for (i, slot) in input.iter_mut().enumerate() {
            *slot = ((i as f32) * 0.25).sin() * 16384.0;
        }

        // Full 48 kHz (`downsample = 1`)
        let mut pcm_48k = [0.0f32; 48];
        let mut mem_48k = 0.0f32;
        let written_48k = deemphasis_channel(&input, &mut pcm_48k, 1, &mut mem_48k);
        assert_eq!(written_48k, 48);

        // Decimated 16 kHz (`downsample = 3`) should keep every 3rd sample of the 48 kHz filter
        let mut pcm_16k = [0.0f32; 16];
        let mut mem_16k = 0.0f32;
        let written_16k = deemphasis_channel(&input, &mut pcm_16k, 3, &mut mem_16k);
        assert_eq!(written_16k, 16);
        assert!((mem_48k - mem_16k).abs() < 1.0e-5);
        for k in 0..16 {
            assert!((pcm_16k[k] - pcm_48k[3 * k]).abs() < 1.0e-7);
        }
        return;
    }

    #[test]
    fn test_denormalise_bands_silence_and_nyquist_bound() {
        let mut norm_x = [1.0f32; 240];
        let mut freq = [99.0f32; 240];
        let mut band_log_e = [0.0f32; MAX_BANDS];
        band_log_e[0] = 1.0 - E_MEANS[0];

        let silent = Denormalisation {
            start: 0,
            end: 21,
            lm: BlockSize::new(1),
            downsample: 1,
            silence: true,
        };
        denormalise_bands(&norm_x, &mut freq, &band_log_e, &silent);
        assert!(freq.iter().all(|&v| v == 0.0));

        norm_x.fill(1.0);
        let decimated = Denormalisation { downsample: 2, silence: false, ..silent };
        denormalise_bands(&norm_x, &mut freq, &band_log_e, &decimated);
        // Band 0 (`0..2` at `lm = 1`) has `lg = 1.0 -> gain = 2^1 = 2.0`
        assert!((freq[0] - 2.0).abs() < 1.0e-5);
        assert!((freq[1] - 2.0).abs() < 1.0e-5);
        // Above `n / downsample = 120` must be zeroed
        assert!(freq[120..].iter().all(|&v| v == 0.0));
        return;
    }
}
