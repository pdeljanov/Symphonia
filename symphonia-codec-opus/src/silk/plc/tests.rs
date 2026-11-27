//! Tests of [`crate::silk::plc`].

// Regression pins: expected values recorded from libopus 1.6.1 for concealment, which
// RFC 6716 Section 4.4 leaves to the implementation; a change here is a design decision, not
// a conformance failure.
use super::test_support::*;
use super::*;
use crate::silk::cng::CngState;
use crate::silk::{FrameType, QuantizationOffsetType};

/// Executes one lost-frame concealment + CNG + glue step and asserts the resulting
/// PLC state and PCM head/tail slices against the recorded values.
macro_rules! assert_plc_loss_step {
    (
        $plc:expr,
        $ch:expr,
        $cng:expr,
        $pcm:expr,
        frame_len: $frame_len:expr,
        nb_subfr: $nb_subfr:expr,
        lpc_order: $lpc_order:expr,
        loss_count: $loss_count:expr,
        rand_seed: $rand_seed:expr,
        rand_scale_q14: $rand_scale:expr,
        conc_energy: $conc_energy:expr,
        head: $expected_head:expr,
        tail: $expected_tail:expr
        $(, pitch_l_q8: $pitch_l_q8:expr)?
        $(, conc_energy_shift: $conc_shift:expr)?
        $(, lag_prev: $lag_prev:expr)?
        $(, s_lpc: $expected_slpc:expr)?
    ) => {{
        let layout = concealment_layout($frame_len, $nb_subfr, $lpc_order);
        $plc.conceal_lost_frame(&mut $ch, &layout, &mut $pcm[..$frame_len]);
        $cng.apply(&$ch, &$plc, None, $lpc_order, &mut $pcm[..$frame_len]).unwrap();
        $plc.glue_frames(&mut $pcm[..$frame_len]);

        assert_eq!($plc.loss_count, $loss_count);
        assert_eq!($plc.rand_seed, $rand_seed);
        assert_eq!($plc.rand_scale_q14, $rand_scale);
        assert_eq!($plc.conc_energy, $conc_energy);
        assert_eq!(&$pcm[..16], &$expected_head);
        assert_eq!(&$pcm[$frame_len - 16..$frame_len], &$expected_tail);
        $(assert_eq!($plc.pitch_l_q8, $pitch_l_q8);)?
        $(assert_eq!($plc.conc_energy_shift, $conc_shift);)?
        $(assert_eq!($ch.prev_pitch_lag, $lag_prev);)?
        $(assert_eq!(&$ch.lpc_state_q14, &$expected_slpc);)?
    }};
}

/// Verifies `update_from_decoded_frame`, `conceal_lost_frame`, `CngState::apply`, and
/// `glue_frames` on a Wideband (16 kHz, 20 ms, 16-tap LPC) Voiced frame across 3 consecutive lost frames
/// and a higher-energy recovery frame against the recorded values.
#[test]
fn voiced_wb_concealment_of_3_losses_and_glue_regression_pin() {
    const LTP_Q7: [[i8; LTP_ORDER]; 4] =
        [[4, 12, 62, 18, -3], [6, 15, 68, 20, -2], [5, 14, 72, 22, -4], [3, 10, 64, 16, -2]];
    const PITCH_LAGS: [u16; 4] = [74, 75, 76, 77];
    const GAINS_Q16: [i32; 4] = [3_276_800, 3_604_480, 3_932_160, 4_194_304];
    const A_Q12_WB: [i16; 16] =
        [7772, -3675, 43, -45, 264, -676, 460, -283, 541, -219, 61, -369, 301, -381, 139, 146];

    const EXPECTED_LOSS_1_HEAD: [i16; 16] = [
        326, 346, 298, 223, 170, -1007, -1043, -1066, -1078, -1075, -1068, -1051, -1023, -1000,
        -959, -904,
    ];
    const EXPECTED_LOSS_1_TAIL: [i16; 16] =
        [-122, -83, -45, -379, -334, -276, -220, -152, -75, -1, 57, 104, 112, 100, 64, 41];
    const EXPECTED_LOSS_1_SLPC: [i32; 16] = [
        -31324, -21188, -11404, -97084, -85392, -70780, -56228, -38832, -19180, -152, 14616, 26680,
        28648, 25724, 16348, 10552,
    ];

    const EXPECTED_LOSS_2_HEAD: [i16; 16] =
        [20, -233, -219, -186, -153, -125, -101, -86, -61, -33, -3, 27, 53, 94, 129, 168];
    const EXPECTED_LOSS_2_TAIL: [i16; 16] =
        [50, 67, 83, 103, 129, 144, 159, 173, 182, 130, 132, 134, 128, 126, 120, 112];
    const EXPECTED_LOSS_2_SLPC: [i32; 16] = [
        12752, 17180, 21364, 26340, 32904, 36772, 40792, 44212, 46680, 33392, 33796, 34388, 32832,
        32160, 30776, 28552,
    ];

    const EXPECTED_LOSS_3_HEAD: [i16; 16] =
        [103, 94, 73, 50, 32, 15, -8, -30, -43, -54, -88, -82, -81, -83, -78, -72];
    const EXPECTED_LOSS_3_TAIL: [i16; 16] =
        [-45, -42, -38, -35, -29, -23, -17, -10, -7, -5, -8, -5, -2, 0, -1, -1];
    const EXPECTED_LOSS_3_SLPC: [i32; 16] = [
        -11424, -10732, -9800, -8936, -7340, -5912, -4284, -2656, -1824, -1244, -1936, -1236, -404,
        28, -336, -188,
    ];

    const EXPECTED_GLUED_HEAD: [i16; 16] =
        [-89, -127, -156, -179, -193, -200, -200, -192, -176, -153, -122, -83, -37, 16, 78, 146];
    const EXPECTED_GLUED_TAIL: [i16; 16] = [
        2583, 2894, 3205, 3516, 3827, -3863, -3552, -3241, -2930, -2619, -2308, -1997, -1686,
        -1375, -1064, -753,
    ];

    let mut ch_state = ChannelState::new();
    ch_state.set_sample_rate(16, 48_000).unwrap();
    let mut plc = PlcState::new();
    let mut cng = CngState::new();

    let mut frame = Frame::new(320, 4, 16);
    frame.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);
    frame.ltp_scale_q14 = 12_288;
    for i in 0..4 {
        frame.subframes[i].pitch_lag = PITCH_LAGS[i];
        frame.subframes[i].ltp_coeffs = LTP_Q7[i];
        frame.subframes[i].gain_q16 = GAINS_Q16[i];
        frame.subframes[i].lpc_coeffs_q12 = A_Q12_WB;
    }
    for i in 0..320 {
        frame.exc_q14[i] = (((i as i32) * 173 + 29) % 2001 - 1000) * 16;
        frame.output_pcm[i] = ((((i as i32) * 97 + 13) % 1601) - 800) as i16;
        ch_state.output_history[i] = frame.output_pcm[i];
    }
    for i in 0..16 {
        ch_state.lpc_state_q14[i] = ((i as i32) - 8) * 12_345;
    }

    let mut pcm_buf = frame.output_pcm;
    record_decoded(&mut plc, &mut ch_state, &frame);
    cng.apply(&ch_state, &plc, Some(cng_input(&frame)), 16, &mut pcm_buf[..320]).unwrap();
    plc.glue_frames(&mut pcm_buf[..320]);

    assert_eq!(plc.pitch_l_q8, 19_712);
    assert_eq!(plc.ltp_scale_q14, 12_288);
    assert_eq!(plc.prev_gain_q16, [3_932_160, 4_194_304]);
    assert_eq!(plc.ltp_coef_q14, [0, 0, 11_648, 0, 0]);
    assert_eq!(plc.prev_lpc_q12, A_Q12_WB);

    assert_plc_loss_step!(
        plc, ch_state, cng, pcm_buf,
        frame_len: 320, nb_subfr: 4, lpc_order: 16,
        loss_count: 1, rand_seed: -1_783_179_200, rand_scale_q14: 2_891, conc_energy: 74_254_257,
        head: EXPECTED_LOSS_1_HEAD, tail: EXPECTED_LOSS_1_TAIL,
        pitch_l_q8: 20_509, conc_energy_shift: 0, lag_prev: 80, s_lpc: EXPECTED_LOSS_1_SLPC
    );
    assert_plc_loss_step!(
        plc, ch_state, cng, pcm_buf,
        frame_len: 320, nb_subfr: 4, lpc_order: 16,
        loss_count: 2, rand_seed: -451_448_704, rand_scale_q14: 1_183, conc_energy: 5_530_189,
        head: EXPECTED_LOSS_2_HEAD, tail: EXPECTED_LOSS_2_TAIL,
        pitch_l_q8: 21_340, conc_energy_shift: 0, lag_prev: 83, s_lpc: EXPECTED_LOSS_2_SLPC
    );
    assert_plc_loss_step!(
        plc, ch_state, cng, pcm_buf,
        frame_len: 320, nb_subfr: 4, lpc_order: 16,
        loss_count: 3, rand_seed: -765_343_552, rand_scale_q14: 483, conc_energy: 890_239,
        head: EXPECTED_LOSS_3_HEAD, tail: EXPECTED_LOSS_3_TAIL,
        pitch_l_q8: 22_204, conc_energy_shift: 0, lag_prev: 87, s_lpc: EXPECTED_LOSS_3_SLPC
    );

    // Recovery frame with higher energy triggers glue_frames ramp
    for (i, sample) in pcm_buf[..320].iter_mut().enumerate() {
        *sample = ((((i as i32) * 311 + 50) % 8001) - 4000) as i16;
    }
    record_decoded(&mut plc, &mut ch_state, &frame);
    cng.apply(&ch_state, &plc, Some(cng_input(&frame)), 16, &mut pcm_buf[..320]).unwrap();
    plc.glue_frames(&mut pcm_buf[..320]);

    assert!(!plc.last_frame_lost);
    assert_eq!(plc.conc_energy, 1_823_203_328);
    assert_eq!(&pcm_buf[..16], &EXPECTED_GLUED_HEAD);
    assert_eq!(&pcm_buf[320 - 16..320], &EXPECTED_GLUED_TAIL);
}

/// Verifies `update_from_decoded_frame`, `conceal_lost_frame`, and `glue_frames` on a
/// Narrowband (8 kHz, 20 ms, 10-tap LPC) Unvoiced frame across 3 consecutive lost frames
/// against the recorded values.
#[test]
fn unvoiced_nb_concealment_of_3_losses_regression_pin() {
    const GAINS_Q16: [i32; 4] = [2_097_152, 1_835_008, 1_572_864, 1_310_720];
    const A_Q12_NB: [i16; 10] = [1545, 716, 1663, -2049, 2207, -1542, 1286, -805, 595, -332];

    const EXPECTED_LOSS_1_HEAD: [i16; 16] =
        [1, 8, 4, 18, 12, 8, 9, 10, 15, 9, 20, 16, 12, 18, 0, -3];
    const EXPECTED_LOSS_1_TAIL: [i16; 16] = [-6, 7, 6, 3, 2, 0, -3, 7, -7, 7, 16, -4, 21, 1, 5, 13];

    const EXPECTED_LOSS_2_HEAD: [i16; 16] =
        [-19, -2, -21, -15, 0, -23, 11, -13, -11, 0, -14, 10, -8, -1, 4, -9];
    const EXPECTED_LOSS_2_TAIL: [i16; 16] =
        [-13, 1, -17, 1, -1, -5, 8, -18, 2, -7, -9, -1, -14, 11, -14, -3];

    const EXPECTED_LOSS_3_HEAD: [i16; 16] =
        [-3, -17, 9, -17, -1, -8, -15, 0, -18, 1, -15, -20, -2, -27, -3, -18];
    const EXPECTED_LOSS_3_TAIL: [i16; 16] =
        [1, -8, -6, -2, -3, 3, -3, -2, 0, -3, -5, -6, 1, -2, -2, -2];

    let mut ch_state = ChannelState::new();
    ch_state.set_sample_rate(8, 48_000).unwrap();
    let mut plc = PlcState::new();
    let mut cng = CngState::new();

    let mut frame = Frame::new(160, 4, 10);
    frame.frame_type = FrameType::new(SignalType::Unvoiced, QuantizationOffsetType::High);
    for (i, &gain) in GAINS_Q16.iter().enumerate() {
        frame.subframes[i].gain_q16 = gain;
        frame.subframes[i].lpc_coeffs_q12[..10].copy_from_slice(&A_Q12_NB);
    }
    for i in 0..160 {
        let scale = if (80..120).contains(&i) { 4 } else { 24 };
        frame.exc_q14[i] = (((i as i32) * 251 + 43) % 1001 - 500) * scale;
        frame.output_pcm[i] = ((((i as i32) * 67 + 19) % 801) - 400) as i16;
        ch_state.output_history[i] = frame.output_pcm[i];
    }
    for i in 0..10 {
        ch_state.lpc_state_q14[i] = (5 - (i as i32)) * 8192;
    }

    let mut pcm_buf = [0i16; 160];
    pcm_buf.copy_from_slice(&frame.output_pcm[..160]);
    record_decoded(&mut plc, &mut ch_state, &frame);
    cng.apply(&ch_state, &plc, Some(cng_input(&frame)), 10, &mut pcm_buf).unwrap();
    plc.glue_frames(&mut pcm_buf);

    assert_eq!(plc.pitch_l_q8, 36_864);
    assert_eq!(plc.ltp_scale_q14, 0);
    assert_eq!(plc.prev_gain_q16, [1_572_864, 1_310_720]);
    assert_eq!(ch_state.prev_pitch_lag, 0);

    assert_plc_loss_step!(
        plc, ch_state, cng, pcm_buf,
        frame_len: 160, nb_subfr: 4, lpc_order: 10,
        loss_count: 1, rand_seed: -932_563_936, rand_scale_q14: 15_736, conc_energy: 16_240,
        head: EXPECTED_LOSS_1_HEAD, tail: EXPECTED_LOSS_1_TAIL,
        pitch_l_q8: 36_864, lag_prev: 144
    );
    assert_plc_loss_step!(
        plc, ch_state, cng, pcm_buf,
        frame_len: 160, nb_subfr: 4, lpc_order: 10,
        loss_count: 2, rand_seed: -1_783_179_200, rand_scale_q14: 10_322, conc_energy: 20_320,
        head: EXPECTED_LOSS_2_HEAD, tail: EXPECTED_LOSS_2_TAIL
    );
    assert_plc_loss_step!(
        plc, ch_state, cng, pcm_buf,
        frame_len: 160, nb_subfr: 4, lpc_order: 10,
        loss_count: 3, rand_seed: 443_411_552, rand_scale_q14: 6_770, conc_energy: 9_662,
        head: EXPECTED_LOSS_3_HEAD, tail: EXPECTED_LOSS_3_TAIL
    );
}
