//! Tests of [`crate::celt::vq`].

#![expect(
    clippy::excessive_precision,
    reason = "recorded values keep the 9 significant digits of the dump"
)]

// Fixtures: packets and expected values recorded from libopus 1.6.1. The behaviour they pin is
// normative (RFC 6716 Section 4.3.4) and is verified against the RFC 8251 test vectors by
// `tests/rfc6716_conformance.rs`.
use super::*;

/// Applies the forward spreading rotation (the encoder direction), used only to check that
/// the decoder's inverse rotation undoes it.
fn spreading_rotation_forward(x: &mut [f32], blocks: usize, pulses: usize, spread: Spread) {
    let Some(SpreadingRotation { c, s, stride2, sub_len }) =
        SpreadingRotation::new(x.len(), blocks, pulses, spread)
    else {
        return;
    };
    for chunk in x.chunks_exact_mut(sub_len) {
        rotation_sweep(chunk, 1, c, -s);
        if stride2 > 0 {
            rotation_sweep(chunk, stride2, s, -c);
        }
    }
    return;
}

#[test]
#[expect(
    clippy::approx_constant,
    reason = "the literal is the value being proven equal to the constant"
)]
fn std_consts_are_bit_identical_to_reference_literals() {
    assert_eq!(std::f64::consts::LN_2.to_bits(), 0.6931471805599453094_f64.to_bits());
    assert_eq!(std::f64::consts::FRAC_PI_2.to_bits(), (0.5_f64 * std::f64::consts::PI).to_bits());
    assert_eq!(Q15_SCALE.to_bits(), (1.0_f32 / 32768.0_f32).to_bits());
    assert_eq!(FOLD_JITTER.to_bits(), (1.0_f32 / 256.0_f32).to_bits());
    return;
}

#[test]
fn spreading_rotation_matches_recorded_values() {
    let mut x = [0.0_f32; 16];
    for (i, slot) in x.iter_mut().enumerate() {
        *slot = ((i + 1) as f32) * 0.1_f32;
    }
    renormalise_vector(&mut x, 16, 1.0_f32);
    spreading_rotation(&mut x, 2, 5, Spread::Normal);

    let expected: [f32; 16] = [
        1.54906273e-01,
        2.25728750e-01,
        -1.21630229e-01,
        -1.09531626e-01,
        -1.53222278e-01,
        6.74848109e-02,
        4.28060964e-02,
        6.89890534e-02,
        3.23417872e-01,
        4.45986003e-01,
        -2.81007141e-01,
        -3.11197400e-01,
        -3.96054655e-01,
        2.41699502e-01,
        2.75663614e-01,
        3.05051595e-01,
    ];

    for (i, (&got, &exp)) in x.iter().zip(expected.iter()).enumerate() {
        assert!(
            (got - exp).abs() <= 1e-6,
            "exp_rotation mismatch at {i}: got {got}, expected {exp}"
        );
    }

    // Verify forward rotation followed by inverse rotation is an exact identity up to float precision.
    let mut roundtrip = expected;
    spreading_rotation_forward(&mut roundtrip, 2, 5, Spread::Normal);
    spreading_rotation(&mut roundtrip, 2, 5, Spread::Normal);
    for (i, (&got, &exp)) in roundtrip.iter().zip(expected.iter()).enumerate() {
        assert!(
            (got - exp).abs() <= 1e-6,
            "exp_rotation roundtrip mismatch at {i}: got {got}, expected {exp}"
        );
    }
    return;
}

#[test]
fn pvq_unquantize_matches_recorded_values() {
    let packet: [u8; 16] = [
        0x43, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
        0x58,
    ];
    let mut dec = Decoder::new(&packet);
    let mut x = [0.0_f32; 8];
    let cm = pvq_unquantize(&mut x, 7, Spread::Normal, 2, &mut dec, 1.0_f32)
        .expect("pvq_unquantize succeeds");

    assert_eq!(cm, 3);
    let expected: [f32; 8] = [
        3.33333343e-01,
        -3.33333343e-01,
        0.0,
        6.66666687e-01,
        -3.33333343e-01,
        0.0,
        -3.33333343e-01,
        3.33333343e-01,
    ];
    for (i, (&got, &exp)) in x.iter().zip(expected.iter()).enumerate() {
        assert!(
            (got - exp).abs() <= 1e-7,
            "alg_unquant mismatch at {i}: got {got}, expected {exp}"
        );
    }
    return;
}
