//! Per-vector conformance report and the assertion over all vectors of one rate and layout.

use super::opus_compare;
use super::opus_compare::Quality;
use super::*;
use std::fmt;
use std::path::Path;

/// Outcome of one test vector for one output rate and layout.
pub(crate) struct VectorReport {
    pub(crate) vector: usize,
    pub(crate) rate: u32,
    pub(crate) layout: Layout,
    pub(crate) decoded: Decoded,
    /// `opus_compare` results against the `.dec` and `m.dec` references, in `REFERENCE_SETS`
    /// order.
    pub(crate) quality: [Result<Quality, String>; 2],
}

impl VectorReport {
    pub(crate) fn range_ok(&self) -> bool {
        return self.decoded.first_range_mismatch.is_none()
            && self.decoded.first_decode_error.is_none();
    }

    pub(crate) fn quality_ok(&self) -> bool {
        return self.quality.iter().any(|quality| quality.as_ref().is_ok_and(Quality::passes));
    }

    pub(crate) fn passes(&self) -> bool {
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

/// Decodes `testvectorNN.bit` at `rate` for `layout` and compares the output with both 48 kHz
/// reference sets (`opus_compare [-s] -r <rate>`).
pub(crate) fn check_vector(dir: &Path, vector: usize, rate: u32, layout: Layout) -> VectorReport {
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
pub(crate) fn assert_conformance(rate: u32, layout: Layout) {
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
