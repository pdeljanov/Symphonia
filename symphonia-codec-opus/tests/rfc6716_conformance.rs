//! Decoder conformance against the RFC 6716 / RFC 8251 test vectors.
//!
//! [RFC 6716 Section 6](<https://datatracker.ietf.org/doc/html/rfc6716#section-6>) defines
//! compliance through the reference decoder of Appendix A, for every test vector and for every
//! supported output sampling rate and channel count:
//!
//! - a compliant decoder MUST have the same final range decoder state as the reference decoder:
//!   `opus_demo -d` compares the decoder final range with the encoder final range stored for every
//!   packet and stops with "Range coder state mismatch between encoder and decoder";
//! - its output MUST be within the threshold of the `opus_compare.c` tool when compared with the
//!   reference output
//!   ([RFC 6716 Section 6.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-6.1>)):
//!   quality 0 is the passing threshold, 100 means identical output, and a quality above 90 is
//!   RECOMMENDED at 48 kHz. Bit-exact output is not required.
//!
//! RFC 6716 Section 6.1 decodes every vector as `opus_demo -d <rate> <channels> testvectorX.bit
//! testX.out`, "where `<rate>` is the sampling rate and can be 8000, 12000, 16000, 24000, or
//! 48000, and `<channels>` is 1 for mono or 2 for stereo", and compares the output of every rate
//! with the same 48 kHz reference decode, `opus_compare [-s] -r <rate> testvectorX.dec testX.out`:
//! "For other sampling rates, it is normal for the quality metric to be lower (typically, as low
//! as 50 even for a good implementation) because of harmless mismatch with the delay and phase of
//! the internal sampling rate conversion." The `run_vectors.sh <exec path> <vector path> <rate>`
//! script of Appendix A runs the twelve vectors in mono and stereo for one rate; the tests below
//! do the same for each of the five rates.
//!
//! [RFC 8251 Section 11](<https://datatracker.ietf.org/doc/html/rfc8251#section-11>) updates the
//! vectors with two decoded outputs per bitstream: `testvectorNN.dec` applies the CELT 180-degree
//! phase shift allowed by RFC 8251 Section 10 and `testvectorNNm.dec` does not. An implementation
//! passes when its output passes against either set.
//!
//! The vectors are not part of the repository. The tests are ignored by default and read the
//! vector directory from `OPUS_TESTVECTORS_DIR`:
//!
//! ```text
//! OPUS_TESTVECTORS_DIR=/path/to/opus_newvectors cargo test --release \
//!     -p symphonia-codec-opus --test rfc6716_conformance -- --ignored --nocapture
//! ```

#![allow(clippy::needless_return)]

use std::fmt;
use std::path::{Path, PathBuf};

use symphonia_codec_opus::OpusDecoder;
use symphonia_core::audio::{Channels, Position};
use symphonia_core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia_core::packet::PacketRef;
use symphonia_core::units::{Duration, Timestamp};

use opus_compare::Quality;

/// Environment variable naming the directory with the RFC 8251 test vectors.
const VECTORS_DIR_ENV: &str = "OPUS_TESTVECTORS_DIR";

/// Number of test vectors (`testvector01` .. `testvector12`).
const TEST_VECTOR_COUNT: usize = 12;

/// Largest payload `opus_demo -d` accepts (`MAX_PACKET`).
const MAX_PACKET: usize = 1500;

/// Size of the big-endian payload length and encoder final range fields of a `.bit` record.
const RECORD_FIELD_BYTES: usize = 4;

/// Scale of the reference float build from `[-1, 1)` to 16-bit PCM (`CELT_SIG_SCALE`).
const CELT_SIG_SCALE: f32 = 32_768.0;

/// Worker threads per conformance run; bounded because one comparison holds about 70 MB.
const MAX_WORKERS: usize = 4;

/// Reference output sets of RFC 8251 Section 11 as `(label, file name suffix)`.
const REFERENCE_SETS: [(&str, &str); 2] = [(".dec", ""), ("m.dec", "m")];

/// Output channel layout of one conformance run (`opus_demo -d <rate> <channels>`).
#[derive(Clone, Copy, Debug)]
enum Layout {
    Mono,
    Stereo,
}

impl Layout {
    fn channels(self) -> usize {
        return match self {
            Layout::Mono => 1,
            Layout::Stereo => 2,
        };
    }

    fn positions(self) -> Channels {
        return match self {
            Layout::Mono => Channels::Positioned(Position::FRONT_CENTER),
            Layout::Stereo => Channels::Positioned(Position::FRONT_LEFT | Position::FRONT_RIGHT),
        };
    }

    fn name(self) -> &'static str {
        return match self {
            Layout::Mono => "mono",
            Layout::Stereo => "stereo",
        };
    }
}

/// One `opus_demo` record: the encoder final range and the packet payload.
struct BitRecord<'a> {
    enc_final_range: u32,
    payload: &'a [u8],
}

/// Splits a `.bit` file into records the way `opus_demo -d` reads it: per packet a 32-bit
/// big-endian payload length, a 32-bit big-endian encoder final range and the payload. A partial
/// length field at the end of the file is end of input (`feof`); an invalid length or a truncated
/// record is an error.
fn parse_bit_records(bytes: &[u8]) -> Result<Vec<BitRecord<'_>>, String> {
    let mut records = Vec::new();
    let mut rest = bytes;
    while let Some((len_field, tail)) = rest.split_first_chunk::<RECORD_FIELD_BYTES>() {
        let i = records.len();
        let len = u32::from_be_bytes(*len_field) as usize;
        if len > MAX_PACKET {
            return Err(format!("record {i}: invalid payload length {len}"));
        }
        let Some((range_field, tail)) = tail.split_first_chunk::<RECORD_FIELD_BYTES>() else {
            return Err(format!("record {i}: truncated encoder final range"));
        };
        let Some((payload, tail)) = tail.split_at_checked(len) else {
            return Err(format!("record {i}: ran out of input, expecting {len} bytes"));
        };
        records.push(BitRecord { enc_final_range: u32::from_be_bytes(*range_field), payload });
        rest = tail;
    }
    return Ok(records);
}

/// `FLOAT2INT16` of the reference float build (`celt/float_cast.h`): scale by `CELT_SIG_SCALE`,
/// saturate to 16 bits and round to nearest with ties to even (`lrintf`).
fn float_to_pcm16(sample: f32) -> i16 {
    let scaled = (sample * CELT_SIG_SCALE).clamp(f32::from(i16::MIN), f32::from(i16::MAX));
    return scaled.round_ties_even() as i16;
}

/// First packet whose decoder final range differs from the encoder final range.
#[derive(Clone, Copy, Debug)]
struct RangeMismatch {
    packet: usize,
    expected: u32,
    actual: u32,
}

/// Output and final range outcome of decoding one `.bit` file.
struct Decoded {
    packets: usize,
    lost: usize,
    /// Packets whose final range `opus_demo` checks.
    checked: usize,
    range_mismatches: usize,
    first_range_mismatch: Option<RangeMismatch>,
    first_decode_error: Option<(usize, String)>,
    pcm: Vec<i16>,
}

/// Creates the decoder of `opus_demo -d <rate> <channels>`.
fn new_decoder(rate: u32, layout: Layout) -> OpusDecoder {
    let mut params = AudioCodecParameters::new();
    params.for_codec(CODEC_ID_OPUS).with_sample_rate(rate).with_channels(layout.positions());
    return OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap_or_else(|err| {
        panic!("cannot create a {rate} Hz {} decoder: {err}", layout.name())
    });
}

/// Decodes `records` like `opus_demo -d <rate> <channels>`: a zero-length payload is a lost
/// packet (`decode_ref` with an empty packet runs the loss path), a failed packet writes no
/// output, and the final range is checked unless the encoder range is zero or this or the
/// previous packet was lost (the first packet counts as preceded by a loss).
fn decode_records(records: &[BitRecord<'_>], rate: u32, layout: Layout) -> Decoded {
    let mut decoder = new_decoder(rate, layout);
    let mut decoded = Decoded {
        packets: records.len(),
        lost: 0,
        checked: 0,
        range_mismatches: 0,
        first_range_mismatch: None,
        first_decode_error: None,
        pcm: Vec::new(),
    };
    let mut samples: Vec<f32> = Vec::new();
    let mut lost_prev = true;
    for (i, record) in records.iter().enumerate() {
        let lost = record.payload.is_empty();
        let packet = PacketRef::new(0, Timestamp::from(0u32), Duration::from(0u64), record.payload);
        match decoder.decode_ref(&packet) {
            Ok(buf) => {
                buf.copy_to_vec_interleaved(&mut samples);
                decoded.pcm.extend(samples.iter().map(|&sample| float_to_pcm16(sample)));
            }
            Err(err) => {
                decoded.first_decode_error.get_or_insert_with(|| (i, err.to_string()));
            }
        }

        let actual = decoder.final_range();
        let checked = record.enc_final_range != 0 && !lost && !lost_prev;
        decoded.checked += usize::from(checked);
        if checked && actual != record.enc_final_range {
            decoded.range_mismatches += 1;
            decoded.first_range_mismatch.get_or_insert(RangeMismatch {
                packet: i,
                expected: record.enc_final_range,
                actual,
            });
        }
        decoded.lost += usize::from(lost);
        lost_prev = lost;
    }
    return decoded;
}

/// Outcome of one test vector for one output rate and layout.
struct VectorReport {
    vector: usize,
    rate: u32,
    layout: Layout,
    decoded: Decoded,
    /// `opus_compare` results against the `.dec` and `m.dec` references, in `REFERENCE_SETS`
    /// order.
    quality: [Result<Quality, String>; 2],
}

impl VectorReport {
    fn range_ok(&self) -> bool {
        return self.decoded.first_range_mismatch.is_none()
            && self.decoded.first_decode_error.is_none();
    }

    fn quality_ok(&self) -> bool {
        return self.quality.iter().any(|quality| quality.as_ref().is_ok_and(Quality::passes));
    }

    fn passes(&self) -> bool {
        return self.range_ok() && self.quality_ok();
    }
}

impl fmt::Display for VectorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let decoded = &self.decoded;
        write!(
            f,
            "testvector{:02} {:>5} Hz {:<6} {:>5} packets ({} lost) | range ({:>5} checked) ",
            self.vector,
            self.rate,
            self.layout.name(),
            decoded.packets,
            decoded.lost,
            decoded.checked
        )?;
        match decoded.first_range_mismatch {
            Some(RangeMismatch { packet, expected, actual }) => write!(
                f,
                "MISMATCH first at packet {packet} (expected 0x{expected:08x}, got \
                 0x{actual:08x}), {} mismatching packets",
                decoded.range_mismatches
            )?,
            None => write!(f, "OK")?,
        }
        if let Some((packet, err)) = &decoded.first_decode_error {
            write!(f, " | decode ERROR first at packet {packet}: {err}")?;
        }
        for ((label, _), quality) in REFERENCE_SETS.iter().zip(&self.quality) {
            match quality {
                Ok(quality) => write!(
                    f,
                    " | {label} Q {:6.1} (err {:.4}) {}",
                    quality.q,
                    quality.err,
                    if quality.passes() { "pass" } else { "FAIL" }
                )?,
                Err(err) => write!(f, " | {label} FAIL ({err})")?,
            }
        }
        return write!(f, " => {}", if self.passes() { "PASS" } else { "FAIL" });
    }
}

/// Reads the vector directory from `OPUS_TESTVECTORS_DIR`; a conformance run without vectors
/// must fail loudly rather than pass.
fn vectors_dir() -> PathBuf {
    let Some(dir) = std::env::var_os(VECTORS_DIR_ENV) else {
        panic!(
            "{VECTORS_DIR_ENV} is not set: point it at the unpacked RFC 8251 test vectors \
             (testvector01.bit .. testvector12.bit, testvectorNN.dec, testvectorNNm.dec), see \
             <https://datatracker.ietf.org/doc/html/rfc8251#section-11>"
        );
    };
    return PathBuf::from(dir);
}

fn read_file(path: &Path) -> Vec<u8> {
    return std::fs::read(path)
        .unwrap_or_else(|err| panic!("cannot read test vector file {}: {err}", path.display()));
}

/// Reads a 16-bit little-endian PCM file as written by `opus_demo`.
fn read_pcm16(bytes: &[u8]) -> Vec<i16> {
    return bytes.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]])).collect();
}

/// Decodes `testvectorNN.bit` at `rate` for `layout` and compares the output with both 48 kHz
/// reference sets (`opus_compare [-s] -r <rate>`).
fn check_vector(dir: &Path, vector: usize, rate: u32, layout: Layout) -> VectorReport {
    let bit_path = dir.join(format!("testvector{vector:02}.bit"));
    let bit = read_file(&bit_path);
    let records = parse_bit_records(&bit)
        .unwrap_or_else(|err| panic!("malformed {}: {err}", bit_path.display()));
    let decoded = decode_records(&records, rate, layout);

    let quality = REFERENCE_SETS.map(|(_, suffix)| {
        let reference =
            read_pcm16(&read_file(&dir.join(format!("testvector{vector:02}{suffix}.dec"))));
        return opus_compare::compare(&reference, &decoded.pcm, layout.channels(), rate);
    });
    return VectorReport { vector, rate, layout, decoded, quality };
}

/// Runs every test vector at `rate` for `layout` like `run_vectors.sh`, prints one line per
/// vector and fails with the lines of every failing vector.
fn assert_conformance(rate: u32, layout: Layout) {
    let dir = vectors_dir();
    let workers = std::thread::available_parallelism().map_or(1, usize::from).min(MAX_WORKERS);
    let mut reports: Vec<VectorReport> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|i| {
                let dir = dir.as_path();
                return scope.spawn(move || {
                    return (1..=TEST_VECTOR_COUNT)
                        .skip(i)
                        .step_by(workers)
                        .map(|vector| check_vector(dir, vector, rate, layout))
                        .collect::<Vec<_>>();
                });
            })
            .collect();
        return handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("test vector worker panicked"))
            .collect();
    });
    reports.sort_by_key(|report| report.vector);

    reports.iter().for_each(|report| println!("{report}"));
    let failures: Vec<String> =
        reports.iter().filter(|report| !report.passes()).map(ToString::to_string).collect();
    assert!(
        failures.is_empty(),
        "{} of {TEST_VECTOR_COUNT} RFC 6716 test vectors fail at {rate} Hz {} (RFC 6716 Section 6 \
         needs a matching final range for every packet and opus_compare quality >= 0 against \
         either RFC 8251 reference set):\n{}",
        failures.len(),
        layout.name(),
        failures.join("\n")
    );
}

/// One ignored conformance test per `(rate, layout)` pair of RFC 6716 Section 6.1.
macro_rules! conformance_tests {
    ($($name:ident => ($rate:expr, $layout:expr)),* $(,)?) => {
        $(
            #[test]
            #[ignore = "needs OPUS_TESTVECTORS_DIR with the RFC 8251 test vectors"]
            fn $name() {
                assert_conformance($rate, $layout);
            }
        )*
    };
}

conformance_tests! {
    rfc6716_test_vectors_8k_mono => (8_000, Layout::Mono),
    rfc6716_test_vectors_8k_stereo => (8_000, Layout::Stereo),
    rfc6716_test_vectors_12k_mono => (12_000, Layout::Mono),
    rfc6716_test_vectors_12k_stereo => (12_000, Layout::Stereo),
    rfc6716_test_vectors_16k_mono => (16_000, Layout::Mono),
    rfc6716_test_vectors_16k_stereo => (16_000, Layout::Stereo),
    rfc6716_test_vectors_24k_mono => (24_000, Layout::Mono),
    rfc6716_test_vectors_24k_stereo => (24_000, Layout::Stereo),
    rfc6716_test_vectors_48k_mono => (48_000, Layout::Mono),
    rfc6716_test_vectors_48k_stereo => (48_000, Layout::Stereo),
}

/// Port of `opus_compare.c` from the RFC 6716 Appendix A reference code
/// (`opus_compare [-s] -r <rate> reference decoded`).
///
/// [RFC 6716 Section 6.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-6.1>): "the
/// opus_compare tool outputs an 'Opus quality metric' that indicates how well the tested decoder
/// matches the reference implementation. A quality of 0 corresponds to the passing threshold,
/// while a quality of 100 is the highest possible value and means that the output of the tested
/// decoder is identical to the reference implementation. The passing threshold (quality 0) was
/// calibrated in such a way that it corresponds to additive white noise with a 48 dB SNR".
///
/// The reference file is always the 48 kHz decode. For `-r <rate>` below 48 kHz the tool does not
/// resample either signal: the decoded signal is analysed at its own rate with a window of
/// `TEST_WIN_SIZE / downsample` samples and a hop of `TEST_WIN_STEP / downsample`, so analysis
/// frame `i` and DFT bin `j` (100 Hz each) line up with those of the reference and the DFT is
/// scaled by `downsample` to restore the reference power. Only the `ybands` bands below the
/// decoded Nyquist frequency (13, 15, 17 and 19 bands at 8, 12, 16 and 24 kHz) are masked,
/// averaged and compared; "if working at a lower sampling rate, don't take into account the last
/// 300 Hz to allow for different transition bands. For 12 kHz, we don't skip anything, because
/// the last band already skips 400 Hz"; and the per-frame error keeps the fixed `NBANDS`
/// normalisation: "using a fixed normalization value means we're willing to accept slightly lower
/// quality for lower sampling rates".
mod opus_compare {
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
        let window: Vec<f32> =
            (0..window_sz).map(|k| 0.5 - 0.5 * cosf(hann_step * k as f32)).collect();
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
}
