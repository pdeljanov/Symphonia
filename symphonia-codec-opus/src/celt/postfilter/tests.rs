//! Tests of [`crate::celt::postfilter`].

#![expect(
    clippy::excessive_precision,
    reason = "recorded values keep the 9 significant digits of the dump"
)]

// Fixtures: packets and expected values recorded from libopus 1.6.1. The behaviour they pin is
// normative (RFC 6716 Section 4.3.7.1) and is verified against the RFC 8251 test vectors by
// `tests/rfc6716_conformance.rs`.
use super::*;
use crate::celt::MAX_FRAME_SIZE;
use crate::celt::PITCH_HISTORY_LEN;
use crate::celt::constant::OVERLAP;
use crate::celt::mdct::tests::assert_f32_slice_close;

const COMB_FILTER_TRANSITION_HEAD16: [f32; 16] = [
    -2.52808472e+02,
    -2.58464203e+02,
    -2.59383698e+02,
    -2.55522751e+02,
    -2.47017944e+02,
    -2.34177368e+02,
    -2.17467178e+02,
    -1.97491516e+02,
    -1.74957397e+02,
    -1.50643997e+02,
    -1.25367424e+02,
    -9.99369125e+01,
    -7.51180267e+01,
    -5.15998192e+01,
    -2.99595184e+01,
    -1.06409883e+01,
];
const COMB_FILTER_TRANSITION_STEADY16: [f32; 16] = [
    7.24783325e+01,
    6.39892960e+01,
    5.28281555e+01,
    3.91309052e+01,
    2.31589203e+01,
    5.30031013e+00,
    -1.39564381e+01,
    -3.40370178e+01,
    -5.43131485e+01,
    -7.41294785e+01,
    -9.28349457e+01,
    -1.09814491e+02,
    -1.24519028e+02,
    -1.36492706e+02,
    -1.45384567e+02,
    -1.50985382e+02,
];

#[test]
fn comb_filter_reset_and_state_advance() {
    let next = PostFilterParams { period: 200, gain: 0.5, tapset: 1 };
    let mut filter = CombFilter::default();
    let mut history = [[0.0_f32; DECODE_BUFFER_SIZE + OVERLAP]; 1];

    // A 2.5 ms frame keeps the cross-fade source: `previous` becomes the old `current`.
    filter.apply(&mut history, 120, BlockSize::new(0), next);
    assert_eq!(filter.current, next);
    assert_eq!(filter.previous.period, MIN_PITCH_PERIOD);
    assert_eq!(filter.previous.gain, 0.0);

    // A longer frame filters its tail with `next`, so both halves end equal.
    filter.apply(&mut history, 240, BlockSize::new(1), next);
    assert_eq!(filter.previous, next);
    assert_eq!(filter.held(), next);

    filter.reset();
    assert_eq!(filter, CombFilter::default());
    return;
}

#[test]
fn comb_filter_transition_and_steady_state_match_recorded_values() {
    let mut buf = [0.0f32; PITCH_HISTORY_LEN + 240];
    for (i, slot) in buf.iter_mut().enumerate() {
        *slot = (i as f32 * 0.09).sin() * 200.0 + (i as f32 * 0.23).cos() * 80.0;
    }

    let from = PostFilterParams { period: 120, gain: 0.375, tapset: 0 };
    let to = PostFilterParams { period: 180, gain: 0.5625, tapset: 2 };
    let mut out = [0.0f32; 240];
    comb_filter(&mut out, &buf, PITCH_HISTORY_LEN, from, to);

    assert_f32_slice_close(&out[..16], &COMB_FILTER_TRANSITION_HEAD16, 1.0e-5, 1.0e-4);
    assert_f32_slice_close(&out[120..136], &COMB_FILTER_TRANSITION_STEADY16, 1.0e-5, 1.0e-4);

    // Verify in-place variant produces identical results
    let mut buf_in_place = buf;
    comb_filter_in_place(&mut buf_in_place, PITCH_HISTORY_LEN, from, to, 240);
    assert_f32_slice_close(
        &buf_in_place[PITCH_HISTORY_LEN..PITCH_HISTORY_LEN + 240],
        &out,
        1.0e-6,
        1.0e-5,
    );
    return;
}

/// Applies [`comb_filter`] in-place on `signal_buf[history_offset .. history_offset + n]`,
/// using `signal_buf[..history_offset]` as the unmodified pitch history.
pub(crate) fn comb_filter_in_place(
    signal_buf: &mut [f32],
    history_offset: usize,
    from: PostFilterParams,
    to: PostFilterParams,
    n: usize,
) {
    debug_assert!(n <= MAX_FRAME_SIZE);
    if from.gain == 0.0 && to.gain == 0.0 {
        return;
    }
    let mut filtered = [0.0f32; MAX_FRAME_SIZE];
    comb_filter(&mut filtered[..n], signal_buf, history_offset, from, to);
    signal_buf[history_offset..history_offset + n].copy_from_slice(&filtered[..n]);
    return;
}
