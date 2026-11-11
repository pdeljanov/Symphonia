//! Port of `opus_compare.c` from the RFC 6716 Appendix A reference code
//! (`opus_compare [-s] -r <rate> reference decoded`).
//!
//! [RFC 6716 Section 6.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-6.1>): "the
//! opus_compare tool outputs an 'Opus quality metric' that indicates how well the tested decoder
//! matches the reference implementation. A quality of 0 corresponds to the passing threshold,
//! while a quality of 100 is the highest possible value and means that the output of the tested
//! decoder is identical to the reference implementation. The passing threshold (quality 0) was
//! calibrated in such a way that it corresponds to additive white noise with a 48 dB SNR".
//!
//! The reference file is always the 48 kHz decode. For `-r <rate>` below 48 kHz the tool does not
//! resample either signal: the decoded signal is analysed at its own rate with a window of
//! `TEST_WIN_SIZE / downsample` samples and a hop of `TEST_WIN_STEP / downsample`, so analysis
//! frame `i` and DFT bin `j` (100 Hz each) line up with those of the reference and the DFT is
//! scaled by `downsample` to restore the reference power. Only the `ybands` bands below the
//! decoded Nyquist frequency (13, 15, 17 and 19 bands at 8, 12, 16 and 24 kHz) are masked,
//! averaged and compared; "if working at a lower sampling rate, don't take into account the last
//! 300 Hz to allow for different transition bands. For 12 kHz, we don't skip anything, because
//! the last band already skips 400 Hz"; and the per-frame error keeps the fixed `NBANDS`
//! normalisation: "using a fixed normalization value means we're willing to accept slightly lower
//! quality for lower sampling rates".

use std::f32::consts::PI;
use std::ops::{Index, IndexMut, RangeInclusive};

/// Sampling rate of every `.dec` reference file.
const REFERENCE_RATE: u32 = 48_000;
/// Bands of the pseudo-NMR (Bark-derived CELT bands).
const NBANDS: usize = 21;
/// Bins stored per analysis frame of the 48 kHz reference.
const NFREQS: usize = 240;
/// Band edges in DFT bins of a `TEST_WIN_SIZE` window at 48 kHz (100 Hz per bin).
const BANDS: [usize; NBANDS + 1] =
    [0, 2, 4, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 68, 80, 96, 120, 156, 200];
const TEST_WIN_SIZE: usize = 480;
const TEST_WIN_STEP: usize = 120;
/// Bins (300 Hz) below the last compared band edge that are skipped at 8, 16 and 24 kHz "to
/// allow for different transition bands".
const TRANSITION_BAND_BINS: usize = 3;
/// Power floor added to every bin.
const POWER_FLOOR: f32 = 100_000.0;
/// Frequency masking from low to high bands: 10 dB/Bark slope.
const MASK_LOW_TO_HIGH: f32 = 0.1;
/// Frequency masking from high to low bands: 15 dB/Bark slope.
const MASK_HIGH_TO_LOW: f32 = 0.03;
/// Temporal masking: -3 dB/2.5 ms slope.
const MASK_TEMPORAL: f32 = 0.5;
/// Allowed stereo cross-talk.
const CROSS_TALK: f32 = 0.01;
/// Weight of the masking energy added to every bin of both spectra.
const MASK_WEIGHT: f32 = 0.1;
/// Bins around the SILK/CELT cross-over (8 kHz) whose error is attenuated to allow for mode
/// freedom in the filters; the centre bin is attenuated twice.
const CROSSOVER_BINS: RangeInclusive<usize> = 79..=81;
const CROSSOVER_CENTER_BIN: usize = 80;
const CROSSOVER_WEIGHT: f32 = 0.1;
/// Base of `Q = 100 * (1 - 0.5 * log(1 + err) / log(1.13))`, which reaches the passing
/// threshold `Q = 0` at `err = 1.13 * 1.13 - 1`.
const QUALITY_ERROR_BASE: f64 = 1.13;
/// The weighted error is the 16th root of the mean per-frame error.
const ERROR_ROOT: f64 = 16.0;

/// Opus quality metric and internal weighted error of one comparison.
#[derive(Clone, Copy, Debug)]
pub struct Quality {
    pub q: f32,
    pub err: f64,
}

impl Quality {
    /// `opus_compare` reports "Test vector FAILS" when `Q < 0`.
    pub fn passes(&self) -> bool {
        return self.q >= 0.0;
    }
}

/// `-r <rate>` parameters of `opus_compare.c`.
#[derive(Clone, Copy, Debug)]
struct RateParams {
    /// Reference samples per decoded sample (`48000 / rate`).
    downsample: usize,
    /// Bands of the decoded signal that are analysed, masked and compared.
    ybands: usize,
    /// Bins stored per analysis frame of the decoded signal (`NFREQS / downsample`).
    yfreqs: usize,
    /// First bin excluded from the comparison.
    max_compare: usize,
}

impl RateParams {
    fn for_rate(rate: u32) -> Result<Self, String> {
        let ybands = match rate {
            8_000 => 13,
            12_000 => 15,
            16_000 => 17,
            24_000 => 19,
            48_000 => NBANDS,
            _ => {
                return Err(format!(
                    "sampling rate must be 8000, 12000, 16000, 24000, or 48000, not {rate}"
                ));
            }
        };
        let downsample = (REFERENCE_RATE / rate) as usize;
        let max_compare = match rate {
            48_000 => BANDS[NBANDS],
            12_000 => BANDS[ybands],
            _ => BANDS[ybands] - TRANSITION_BAND_BINS,
        };
        return Ok(Self { downsample, ybands, yfreqs: NFREQS / downsample, max_compare });
    }
}

/// Analysis geometry of one `band_energy()` call.
#[derive(Clone, Copy, Debug)]
struct Analysis {
    /// Hann window length in samples of the analysed signal.
    window_sz: usize,
    /// Hop between analysis frames in samples of the analysed signal.
    step: usize,
    /// DFT scale restoring the power of a 48 kHz analysis.
    downsample: usize,
    /// Bands whose bins are analysed.
    nbands: usize,
}

impl Analysis {
    /// The 48 kHz reference: `band_energy(xb, X, BANDS, NBANDS, x, ..., 480, 120, 1)`.
    fn reference() -> Self {
        return Self {
            window_sz: TEST_WIN_SIZE,
            step: TEST_WIN_STEP,
            downsample: 1,
            nbands: NBANDS,
        };
    }

    /// The decoded signal at its own rate: `band_energy(NULL, Y, BANDS, ybands, y, ...,
    /// 480 / downsample, 120 / downsample, downsample)`.
    fn decoded(params: RateParams) -> Self {
        return Self {
            window_sz: TEST_WIN_SIZE / params.downsample,
            step: TEST_WIN_STEP / params.downsample,
            downsample: params.downsample,
            nbands: params.ybands,
        };
    }
}

/// Per-frame, per-bin power of one signal with `bins` bins per frame and channel (`ps_sz`),
/// indexed `[(i, j, ch)]` in the C layout `[(i * bins + j) * channels + ch]`.
struct Spectrum {
    data: Vec<f32>,
    bins: usize,
    channels: usize,
}

impl Spectrum {
    fn new(frames: usize, bins: usize, channels: usize) -> Self {
        return Self { data: vec![0.0; frames * bins * channels], bins, channels };
    }
}

impl Index<(usize, usize, usize)> for Spectrum {
    type Output = f32;

    fn index(&self, (i, j, ch): (usize, usize, usize)) -> &f32 {
        return &self.data[(i * self.bins + j) * self.channels + ch];
    }
}

impl IndexMut<(usize, usize, usize)> for Spectrum {
    fn index_mut(&mut self, (i, j, ch): (usize, usize, usize)) -> &mut f32 {
        return &mut self.data[(i * self.bins + j) * self.channels + ch];
    }
}

/// Compares `decoded` (interleaved, `channels` per frame, sampled at `rate`) with the 48 kHz
/// `reference`; the reference file is always stereo and is downmixed to `(L + R) / 2` for a
/// mono comparison.
pub fn compare(
    reference: &[i16],
    decoded: &[i16],
    channels: usize,
    rate: u32,
) -> Result<Quality, String> {
    let params = RateParams::for_rate(rate)?;
    let x = reference_signal(reference, channels);
    let y: Vec<f32> = decoded.iter().map(|&sample| f32::from(sample)).collect();
    let xlength = x.len() / channels;
    let ylength = y.len() / channels * params.downsample;
    if xlength != ylength {
        return Err(format!("sample counts do not match ({xlength} != {ylength})"));
    }
    if xlength < TEST_WIN_SIZE {
        return Err(format!("insufficient sample data ({xlength} < {TEST_WIN_SIZE})"));
    }

    let frames = (xlength - TEST_WIN_SIZE + TEST_WIN_STEP) / TEST_WIN_STEP;
    let mut xb = vec![0.0f32; frames * NBANDS * channels];
    let mut spectrum_x = Spectrum::new(frames, NFREQS, channels);
    let mut spectrum_y = Spectrum::new(frames, params.yfreqs, channels);
    band_energy(Some(&mut xb), &mut spectrum_x, &x, frames, Analysis::reference());
    band_energy(None, &mut spectrum_y, &y, frames, Analysis::decoded(params));
    for i in 0..frames {
        apply_masking(&mut xb, &mut spectrum_x, &mut spectrum_y, i, params.ybands);
    }
    let compared_bins = BANDS[params.ybands];
    average_consecutive_frames(&mut spectrum_x, frames, compared_bins);
    average_consecutive_frames(&mut spectrum_y, frames, compared_bins);

    let err = weighted_error(&spectrum_x, &spectrum_y, frames, params);
    let q = (100.0 * (1.0 - 0.5 * (1.0 + err).ln() / QUALITY_ERROR_BASE.ln())) as f32;
    return Ok(Quality { q, err });
}

/// Reads the stereo reference and downmixes it for a mono comparison.
fn reference_signal(reference: &[i16], channels: usize) -> Vec<f32> {
    let frames = reference.chunks_exact(2);
    if channels == 2 {
        return frames.flatten().map(|&sample| f32::from(sample)).collect();
    }
    return frames.map(|lr| (0.5 * (f64::from(lr[0]) + f64::from(lr[1]))) as f32).collect();
}

/// `OPUS_COSF` and `OPUS_SINF`: the float argument is evaluated in double precision.
fn cosf(x: f32) -> f32 {
    return f64::from(x).cos() as f32;
}

fn sinf(x: f32) -> f32 {
    return f64::from(x).sin() as f32;
}

/// Cosine and sine tables of the DFT of a `window_sz` window, one contiguous row of
/// `window_sz` entries per bin `j` holding the C table entries `(j * k) mod window_sz`.
struct Twiddles {
    window_sz: usize,
    cos: Vec<f32>,
    sin: Vec<f32>,
}

impl Twiddles {
    fn new(window_sz: usize, bins: usize) -> Self {
        let step = 2.0 * PI / window_sz as f32;
        let c: Vec<f32> = (0..window_sz).map(|i| cosf(step * i as f32)).collect();
        let s: Vec<f32> = (0..window_sz).map(|i| sinf(step * i as f32)).collect();
        let rows = |table: &[f32]| {
            return (0..bins)
                .flat_map(|j| (0..window_sz).map(move |k| table[(j * k) % window_sz]))
                .collect::<Vec<f32>>();
        };
        return Self { window_sz, cos: rows(&c), sin: rows(&s) };
    }

    /// DFT of bin `j` of one windowed channel, accumulated in the C order and scaled by
    /// `downsample`.
    fn bin(&self, j: usize, windowed: &[f32], downsample: f32) -> (f32, f32) {
        let row = j * self.window_sz..(j + 1) * self.window_sz;
        let (re, im) = windowed.iter().zip(&self.cos[row.clone()]).zip(&self.sin[row]).fold(
            (0.0f32, 0.0f32),
            |(re, im), ((&sample, &c), &s)| {
                return (re + c * sample, im - s * sample);
            },
        );
        return (re * downsample, im * downsample);
    }
}

/// `band_energy()`: Hann-windowed DFT power plus `POWER_FLOOR` of every bin of the first
/// `analysis.nbands` bands for every `analysis.step` hop into `ps`, and the mean power per
/// band into `bands_out` (`[(i * nbands + band) * channels + ch]`) when given.
fn band_energy(
    mut bands_out: Option<&mut [f32]>,
    ps: &mut Spectrum,
    input: &[f32],
    frames: usize,
    analysis: Analysis,
) {
    let Analysis { window_sz, step, downsample, nbands } = analysis;
    let channels = ps.channels;
    let hann_step = 2.0 * PI / (window_sz - 1) as f32;
    let window: Vec<f32> = (0..window_sz).map(|k| 0.5 - 0.5 * cosf(hann_step * k as f32)).collect();
    let twiddles = Twiddles::new(window_sz, BANDS[nbands]);
    let scale = downsample as f32;
    let mut x = vec![0.0f32; channels * window_sz];
    for i in 0..frames {
        for (ch, windowed) in x.chunks_exact_mut(window_sz).enumerate() {
            windowed.iter_mut().zip(&window).enumerate().for_each(|(k, (sample, &w))| {
                *sample = w * input[(i * step + k) * channels + ch];
            });
        }
        for band in 0..nbands {
            let mut p = [0.0f32; 2];
            for j in BANDS[band]..BANDS[band + 1] {
                for (ch, windowed) in x.chunks_exact(window_sz).enumerate() {
                    let (re, im) = twiddles.bin(j, windowed, scale);
                    let power = re * re + im * im + POWER_FLOOR;
                    ps[(i, j, ch)] = power;
                    p[ch] += power;
                }
            }
            let Some(out) = bands_out.as_deref_mut() else {
                continue;
            };
            let width = (BANDS[band + 1] - BANDS[band]) as f32;
            p.iter().take(channels).enumerate().for_each(|(ch, &power)| {
                out[(i * nbands + band) * channels + ch] = power / width;
            });
        }
    }
}

/// Masking of analysis frame `i`: spreads the reference band energies `xb` across all
/// `NBANDS` bands, time and channels, then adds them to every bin of the first `ybands`
/// bands of both spectra.
fn apply_masking(
    xb: &mut [f32],
    spectrum_x: &mut Spectrum,
    spectrum_y: &mut Spectrum,
    i: usize,
    ybands: usize,
) {
    let channels = spectrum_x.channels;
    let at = move |i: usize, band: usize, ch: usize| (i * NBANDS + band) * channels + ch;
    for band in 1..NBANDS {
        for ch in 0..channels {
            xb[at(i, band, ch)] += MASK_LOW_TO_HIGH * xb[at(i, band - 1, ch)];
        }
    }
    for band in (0..NBANDS - 1).rev() {
        for ch in 0..channels {
            xb[at(i, band, ch)] += MASK_HIGH_TO_LOW * xb[at(i, band + 1, ch)];
        }
    }
    if i > 0 {
        for band in 0..NBANDS {
            for ch in 0..channels {
                xb[at(i, band, ch)] += MASK_TEMPORAL * xb[at(i - 1, band, ch)];
            }
        }
    }
    if channels == 2 {
        for band in 0..NBANDS {
            let (l, r) = (xb[at(i, band, 0)], xb[at(i, band, 1)]);
            xb[at(i, band, 0)] += CROSS_TALK * r;
            xb[at(i, band, 1)] += CROSS_TALK * l;
        }
    }
    for band in 0..ybands {
        for j in BANDS[band]..BANDS[band + 1] {
            for ch in 0..channels {
                let mask = MASK_WEIGHT * xb[at(i, band, ch)];
                spectrum_x[(i, j, ch)] += mask;
                spectrum_y[(i, j, ch)] += mask;
            }
        }
    }
}

/// "Average of consecutive frames to make comparison slightly less sensitive": every frame
/// adds the unaveraged value of the frame before it, for the first `bins` bins.
fn average_consecutive_frames(spectrum: &mut Spectrum, frames: usize, bins: usize) {
    for j in 0..bins {
        for ch in 0..spectrum.channels {
            let mut previous = spectrum[(0, j, ch)];
            for i in 1..frames {
                let current = spectrum[(i, j, ch)];
                spectrum[(i, j, ch)] += previous;
                previous = current;
            }
        }
    }
}

/// Per-bin error `re - log(re) - 1` of the power ratio `re = Y / X` (evaluated in double,
/// stored as float), attenuated around the SILK/CELT cross-over.
fn bin_error(ratio: f32, j: usize) -> f32 {
    let ratio = f64::from(ratio);
    let mut im = (ratio - ratio.ln() - 1.0) as f32;
    if CROSSOVER_BINS.contains(&j) {
        im *= CROSSOVER_WEIGHT;
    }
    if j == CROSSOVER_CENTER_BIN {
        im *= CROSSOVER_WEIGHT;
    }
    return im;
}

/// Internal weighted error: per frame the mean over `NBANDS` bands of the squared mean band
/// error of the first `ybands` bands (bins below `max_compare` only, divided by the full band
/// width), raised to the fourth power, averaged over frames and taken to the 16th root.
fn weighted_error(
    spectrum_x: &Spectrum,
    spectrum_y: &Spectrum,
    frames: usize,
    params: RateParams,
) -> f64 {
    let channels = spectrum_x.channels;
    let total = (0..frames).fold(0.0f64, |err, i| {
        let ef = (0..params.ybands).fold(0.0f64, |ef, band| {
            let eb = (BANDS[band]..BANDS[band + 1].min(params.max_compare))
                .flat_map(|j| (0..channels).map(move |ch| (j, ch)))
                .fold(0.0f64, |eb, (j, ch)| {
                    let ratio = spectrum_y[(i, j, ch)] / spectrum_x[(i, j, ch)];
                    return eb + f64::from(bin_error(ratio, j));
                });
            let eb = eb / ((BANDS[band + 1] - BANDS[band]) * channels) as f64;
            return ef + eb * eb;
        });
        let ef = ef / NBANDS as f64;
        let ef = ef * ef;
        return err + ef * ef;
    });
    return (total / frames as f64).powf(1.0 / ERROR_ROOT);
}
