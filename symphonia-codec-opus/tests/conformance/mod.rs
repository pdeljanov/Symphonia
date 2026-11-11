//! Support code of the RFC 6716 / RFC 8251 conformance test: test vector files, decoding,
//! reporting, reference comparison and the `opus_compare` quality metric.

mod compare;
mod decode;
pub(crate) mod opus_compare;
mod records;
mod report;

pub(crate) use compare::*;
pub(crate) use decode::*;
pub(crate) use records::*;
pub(crate) use report::*;

/// Environment variable naming the directory with the RFC 8251 test vectors.
pub(crate) const VECTORS_DIR_ENV: &str = "OPUS_TESTVECTORS_DIR";

/// Environment variable naming the directory with `ref_<vector>_<rate>_<layout>.pcm` outputs
/// produced by `opus_demo -d` of the patched RFC 6716 Appendix A reference decoder.
pub(crate) const REFERENCE_OUTPUT_DIR_ENV: &str = "OPUS_REFERENCE_OUTPUT_DIR";

/// Number of test vectors (`testvector01` .. `testvector12`).
pub(crate) const TEST_VECTOR_COUNT: usize = 12;

/// Output sampling rates exercised by RFC 6716 Section 6.1.
pub(crate) const SAMPLE_RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

/// Output channel layouts exercised by RFC 6716 Section 6.1.
pub(crate) const LAYOUTS: [Layout; 2] = [Layout::Mono, Layout::Stereo];

/// Test vectors whose packets use only the fixed-point SILK layer (`testvector02`..`04`).
pub(crate) const SILK_ONLY_VECTORS: [usize; 3] = [2, 3, 4];

/// Maximum 16-bit PCM rounding difference (in LSBs) between `symphonia-codec-opus` and the
/// reference float build when both use the same intensity stereo phase inversion setting.
pub(crate) const MAX_FLOAT_ROUNDING_DIFF: u16 = 1;

/// Largest payload `opus_demo -d` accepts (`MAX_PACKET`).
pub(crate) const MAX_PACKET: usize = 1500;

/// Size of the big-endian payload length and encoder final range fields of a `.bit` record.
pub(crate) const RECORD_FIELD_BYTES: usize = 4;

/// Scale of the reference float build from `[-1, 1)` to 16-bit PCM (`CELT_SIG_SCALE`).
pub(crate) const CELT_SIG_SCALE: f32 = 32_768.0;

/// Worker threads per conformance run; bounded because one comparison holds about 70 MB.
pub(crate) const MAX_WORKERS: usize = 4;

/// Reference output sets of RFC 8251 Section 11 as `(label, file name suffix)`.
pub(crate) const REFERENCE_SETS: [(&str, &str); 2] = [(".dec", ""), ("m.dec", "m")];
