//! Sample comparison of the decoded output with the RFC 8251 reference decoder output.

use super::*;
use std::fmt;
use std::path::Path;

/// Sample-by-sample comparison of one decoded `(vector, rate, layout)` run against the output of
/// the patched RFC 6716 Appendix A reference decoder (`opus_demo -d`).
pub(crate) struct SampleComparison {
    pub(crate) vector: usize,
    pub(crate) rate: u32,
    pub(crate) layout: Layout,
    pub(crate) samples: usize,
    pub(crate) identical: usize,
    pub(crate) max_abs_diff: u16,
    pub(crate) range_ok: bool,
    pub(crate) length_matches: bool,
}

impl SampleComparison {
    pub(crate) fn identical_percent(&self) -> f64 {
        if self.samples == 0 {
            return 100.0;
        }
        return 100.0 * self.identical as f64 / self.samples as f64;
    }

    pub(crate) fn is_silk_only(&self) -> bool {
        return SILK_ONLY_VECTORS.contains(&self.vector);
    }

    pub(crate) fn passes(&self) -> bool {
        if !self.range_ok || !self.length_matches {
            return false;
        }
        if self.is_silk_only() {
            return self.max_abs_diff == 0 && self.identical == self.samples;
        }
        if matches!(self.layout, Layout::Stereo) {
            return self.max_abs_diff <= MAX_FLOAT_ROUNDING_DIFF;
        }
        return true;
    }
}

impl fmt::Display for SampleComparison {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        return write!(
            f,
            "testvector{:02} {:>5} Hz {:<6} | {:>7} samples | identical {:>7} ({:6.2}%) | max |diff| {:>5}",
            self.vector,
            self.rate,
            self.layout.name(),
            self.samples,
            self.identical,
            self.identical_percent(),
            self.max_abs_diff,
        );
    }
}

/// Decodes `testvectorNN.bit` at `rate` and `layout` and compares the 16-bit PCM output sample by
/// sample with `ref_NN_<rate>_<layout>.pcm` produced by the patched RFC 6716 Appendix A reference
/// decoder.
pub(crate) fn compare_with_reference_output(
    vectors_dir: &Path,
    ref_dir: &Path,
    vector: usize,
    rate: u32,
    layout: Layout,
) -> SampleComparison {
    let bit_path = vectors_dir.join(format!("testvector{vector:02}.bit"));
    let bit = read_file(&bit_path);
    let records = parse_bit_records(&bit)
        .unwrap_or_else(|err| panic!("malformed {}: {err}", bit_path.display()));
    let decoded = decode_records(&records, rate, layout);

    let ref_path = ref_dir.join(format!("ref_{vector:02}_{rate}_{}.pcm", layout.name()));
    let reference = read_pcm16(&read_file(&ref_path));
    let length_matches = decoded.pcm.len() == reference.len();
    let (identical, max_abs_diff) = decoded.pcm.iter().zip(&reference).fold(
        (0usize, 0u16),
        |(identical, max_diff), (&actual, &expected)| {
            let diff = actual.abs_diff(expected);
            return (identical + usize::from(diff == 0), max_diff.max(diff));
        },
    );
    return SampleComparison {
        vector,
        rate,
        layout,
        samples: decoded.pcm.len(),
        identical,
        max_abs_diff,
        range_ok: decoded.first_range_mismatch.is_none() && decoded.first_decode_error.is_none(),
        length_matches,
    };
}
