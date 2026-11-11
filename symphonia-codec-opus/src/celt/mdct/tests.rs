//! Tests of [`crate::celt::mdct`].

#![allow(clippy::excessive_precision)] // recorded values keep the 9 significant digits of the dump

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

pub(crate) fn assert_f32_slice_close(actual: &[f32], expected: &[f32], rel_tol: f32, abs_tol: f32) {
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
        assert_f32_slice_close(&out_buf[n_bins + 60 - 16..n_bins + 60], tails[lm], 1.0e-5, 1.0e-2);
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
