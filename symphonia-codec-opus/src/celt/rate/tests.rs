//! Tests of [`crate::celt::rate`].

// Fixtures: packets and expected values recorded from libopus 1.6.1. The behaviour they pin is
// normative (RFC 6716 Section 4.3.3) and is verified against the RFC 8251 test vectors by
// `tests/rfc6716_conformance.rs`.
use super::*;

/// Verifies `get_pulses`, `bits2pulses`, and `pulses2bits` against the recorded values.
#[test]
fn pulse_cache_conversions_match_recorded_values() {
    assert_eq!(get_pulses(0), 0);
    assert_eq!(get_pulses(7), 7);
    assert_eq!(get_pulses(8), 8);
    assert_eq!(get_pulses(15), 15);
    assert_eq!(get_pulses(16), 16);
    assert_eq!(get_pulses(40), 128);

    // Tuple format: (lm, band, input_bits, expected_pulses, expected_roundtrip_bits)
    static PULSE_CACHE_CASES: [(usize, usize, i32, i32, i32); 32] = [
        (0, 0, 0, 0, 0),
        (0, 0, 13, 40, 8),
        (0, 10, 13, 1, 16),
        (0, 10, 26, 2, 24),
        (0, 10, 39, 7, 39),
        (0, 10, 52, 19, 52),
        (0, 15, 26, 1, 29),
        (0, 15, 52, 2, 50),
        (0, 20, 26, 1, 44),
        (1, 0, 13, 1, 16),
        (1, 0, 26, 2, 24),
        (1, 0, 39, 7, 39),
        (1, 0, 52, 19, 52),
        (1, 10, 13, 1, 24),
        (1, 10, 39, 2, 40),
        (1, 10, 52, 3, 52),
        (1, 15, 26, 1, 37),
        (1, 15, 52, 2, 66),
        (1, 20, 39, 1, 52),
        (2, 0, 13, 1, 24),
        (2, 0, 39, 2, 40),
        (2, 0, 52, 3, 52),
        (2, 10, 26, 1, 32),
        (2, 10, 52, 2, 56),
        (2, 15, 26, 1, 45),
        (2, 20, 39, 1, 60),
        (3, 0, 13, 0, 0),
        (3, 0, 26, 1, 32),
        (3, 0, 52, 2, 56),
        (3, 10, 26, 1, 40),
        (3, 15, 39, 1, 53),
        (3, 20, 39, 1, 68),
    ];

    for &(lm, band, bits, expected_p, expected_b) in &PULSE_CACHE_CASES {
        let p = bits2pulses_at_level(CacheLevel::from_block_size(BlockSize::new(lm)), band, bits);
        let b = pulses2bits_at_level(CacheLevel::from_block_size(BlockSize::new(lm)), band, p);
        assert_eq!(p, expected_p, "bits2pulses mismatch for lm={} band={} bits={}", lm, band, bits);
        assert_eq!(b, expected_b, "pulses2bits mismatch for lm={} band={} pulses={}", lm, band, p);
    }

    return;
}

/// Verifies `tf_decode` across non-transient, transient, and hybrid (`start = 17`) configurations
/// against the recorded values.
#[test]
fn tf_decode_matches_recorded_values() {
    let mut packet = [0u8; 16];
    for (i, byte) in packet.iter_mut().enumerate() {
        *byte = ((i * 97 + 11) & 0xFF) as u8;
    }

    // Case 1: transient = false, lm = 3, start = 0, end = 21
    let mut dec1 = Decoder::new(&packet);
    let tf1 =
        tf_decode(0, 21, false, BlockSize::new(3), &mut dec1).expect("tf_decode case 1 failed");
    assert_eq!(dec1.tell(), 3);
    assert_eq!(dec1.tell_frac(), 17);
    assert_eq!(tf1, [0; 21]);

    // Case 2: transient = true, lm = 2, start = 0, end = 21
    let mut dec2 = Decoder::new(&packet);
    let tf2 =
        tf_decode(0, 21, true, BlockSize::new(2), &mut dec2).expect("tf_decode case 2 failed");
    assert_eq!(dec2.tell(), 5);
    assert_eq!(dec2.tell_frac(), 35);
    assert_eq!(tf2, [2; 21]);

    // Case 3: hybrid start = 17, end = 21, transient = false, lm = 2
    let mut dec3 = Decoder::new(&packet);
    let tf3 =
        tf_decode(17, 21, false, BlockSize::new(2), &mut dec3).expect("tf_decode case 3 failed");
    assert_eq!(dec3.tell(), 2);
    assert_eq!(dec3.tell_frac(), 10);
    assert_eq!(tf3, [0; 21]);

    return;
}

/// Verifies `init_caps` and `BandAllocation::compute` for Mono (`C = 1`), `LM = 3` (20 ms),
/// `start = 0, end = 21`, 40-byte frame against the recorded values.
#[test]
fn band_allocation_mono_lm3_matches_recorded_values() {
    let mut packet = [0u8; 40];
    for (i, byte) in packet.iter_mut().enumerate() {
        *byte = ((i * 41 + 7) & 0xFF) as u8;
    }
    let mut dec = Decoder::new(&packet);

    let cap = init_caps(BlockSize::new(3), 1);
    const EXPECTED_CAP: [i32; 21] = [
        514, 514, 514, 514, 514, 514, 514, 514, 1028, 1028, 1028, 1028, 2064, 2064, 2064, 2976,
        2976, 3792, 4872, 4644, 4532,
    ];
    assert_eq!(cap, EXPECTED_CAP);

    let mut offsets = [0i32; MAX_BANDS];
    for (i, slot) in offsets.iter_mut().enumerate() {
        if i % 5 == 0 {
            *slot = 6;
        }
    }
    let total = ((40 * 8) << BITRES) - (dec.tell_frac() as i32) - 1;
    let alloc = BandAllocation::compute(
        &mut dec,
        AllocationLayout { start: 0, end: 21, channels: 1, lm: BlockSize::new(3) },
        AllocationBudget { offsets: &offsets, cap: &cap, alloc_trim: 5, total },
    )
    .expect("mono allocation failed");

    assert_eq!(alloc.coded_bands, 21);
    assert_eq!(alloc.balance, 0);
    assert_eq!(alloc.intensity, 0);
    assert!(!alloc.dual_stereo);
    assert_eq!(dec.tell_frac(), 8);

    const EXPECTED_PULSES: [i32; 21] = [
        164, 154, 143, 127, 115, 112, 106, 99, 183, 165, 153, 128, 227, 170, 120, 121, 0, 0, 0, 0,
        0,
    ];
    const EXPECTED_FINE_QUANT: [i32; 21] =
        [3, 2, 2, 2, 2, 2, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 0, 0, 0];
    const EXPECTED_FINE_PRIO: [i32; 21] =
        [1, 0, 0, 1, 1, 1, 0, 0, 1, 1, 1, 1, 0, 1, 1, 1, 0, 0, 0, 0, 0];

    assert_eq!(alloc.pulses, EXPECTED_PULSES);
    assert_eq!(alloc.fine_quant, EXPECTED_FINE_QUANT);
    assert_eq!(alloc.fine_priority.map(i32::from), EXPECTED_FINE_PRIO);

    return;
}

/// Verifies `init_caps` and `BandAllocation::compute` for Stereo (`C = 2`), `LM = 2` (10 ms),
/// `start = 0, end = 21`, 60-byte frame, `alloc_trim = 4` (including skipped band 20,
/// `intensity = 3`, `dual_stereo = true`) against the recorded values.
#[test]
fn band_allocation_stereo_lm2_matches_recorded_values() {
    let mut packet = [0u8; 60];
    for (i, byte) in packet.iter_mut().enumerate() {
        *byte = ((i * 83 + 150) & 0xFF) as u8;
    }
    let mut dec = Decoder::new(&packet);

    let cap = init_caps(BlockSize::new(2), 2);
    const EXPECTED_CAP: [i32; 21] = [
        542, 542, 542, 542, 542, 542, 542, 542, 1072, 1072, 1072, 1072, 2120, 2120, 2120, 3024,
        3024, 3840, 4920, 4680, 4576,
    ];
    assert_eq!(cap, EXPECTED_CAP);

    let mut offsets = [0i32; MAX_BANDS];
    offsets[2] = 12;
    offsets[8] = 6;
    let total = ((60 * 8) << BITRES) - (dec.tell_frac() as i32) - 1;
    let alloc = BandAllocation::compute(
        &mut dec,
        AllocationLayout { start: 0, end: 21, channels: 2, lm: BlockSize::new(2) },
        AllocationBudget { offsets: &offsets, cap: &cap, alloc_trim: 4, total },
    )
    .expect("stereo allocation failed");

    assert_eq!(alloc.coded_bands, 20);
    assert_eq!(alloc.balance, 0);
    assert_eq!(alloc.intensity, 3);
    assert!(alloc.dual_stereo);
    assert_eq!(dec.tell_frac(), 60);

    const EXPECTED_PULSES: [i32; 21] = [
        167, 156, 157, 150, 133, 124, 114, 106, 216, 192, 174, 156, 271, 239, 198, 228, 176, 162,
        99, 97, 0,
    ];
    const EXPECTED_FINE_QUANT: [i32; 21] =
        [2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 1, 1, 2, 1, 2, 2, 2, 1];
    const EXPECTED_FINE_PRIO: [i32; 21] =
        [1, 1, 1, 0, 0, 0, 1, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 1, 1, 0, 0];

    assert_eq!(alloc.pulses, EXPECTED_PULSES);
    assert_eq!(alloc.fine_quant, EXPECTED_FINE_QUANT);
    assert_eq!(alloc.fine_priority.map(i32::from), EXPECTED_FINE_PRIO);

    return;
}

/// Verifies Hybrid mode (`start = 17, end = 21`), Stereo (`C = 2`), `LM = 3` (20 ms)
/// `BandAllocation::compute` against the recorded values.
#[test]
fn band_allocation_hybrid_stereo_lm3_matches_recorded_values() {
    let mut packet = [0u8; 32];
    for (i, byte) in packet.iter_mut().enumerate() {
        *byte = ((i * 29 + 99) & 0xFF) as u8;
    }
    let mut dec = Decoder::new(&packet);
    for _ in 0..15 {
        dec.decode_raw_bits(8).expect("advance bits");
    }

    let cap = init_caps(BlockSize::new(3), 2);
    let offsets = [0i32; MAX_BANDS];
    let total = ((32 * 8) << BITRES) - (dec.tell_frac() as i32) - 1;
    let alloc = BandAllocation::compute(
        &mut dec,
        AllocationLayout { start: 17, end: 21, channels: 2, lm: BlockSize::new(3) },
        AllocationBudget { offsets: &offsets, cap: &cap, alloc_trim: 6, total },
    )
    .expect("hybrid allocation failed");

    assert_eq!(alloc.coded_bands, 19);
    assert_eq!(alloc.balance, 0);
    assert_eq!(alloc.intensity, 18);
    assert!(alloc.dual_stereo);
    assert_eq!(dec.tell_frac(), 1005);

    const EXPECTED_PULSES: [i32; 21] =
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 522, 408, 0, 0];
    const EXPECTED_FINE_QUANT: [i32; 21] =
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 1, 1];
    const EXPECTED_FINE_PRIO: [i32; 21] =
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0];

    assert_eq!(alloc.pulses, EXPECTED_PULSES);
    assert_eq!(alloc.fine_quant, EXPECTED_FINE_QUANT);
    assert_eq!(alloc.fine_priority.map(i32::from), EXPECTED_FINE_PRIO);

    return;
}

/// Verifies high-bitrate Stereo (`C = 2`), `LM = 3` (20 ms), 160-byte frame allocation
/// against the recorded values.
#[test]
fn band_allocation_high_bitrate_matches_recorded_values() {
    let mut packet = [0u8; 160];
    for (i, byte) in packet.iter_mut().enumerate() {
        *byte = ((i * 67 + 13) & 0xFF) as u8;
    }
    let mut dec = Decoder::new(&packet);

    let cap = init_caps(BlockSize::new(3), 2);
    let mut offsets = [0i32; MAX_BANDS];
    for slot in offsets.iter_mut().take(4) {
        *slot = 24;
    }

    let total = ((160 * 8) << BITRES) - (dec.tell_frac() as i32) - 1;
    let alloc = BandAllocation::compute(
        &mut dec,
        AllocationLayout { start: 0, end: 21, channels: 2, lm: BlockSize::new(3) },
        AllocationBudget { offsets: &offsets, cap: &cap, alloc_trim: 5, total },
    )
    .expect("high-bitrate allocation failed");

    assert_eq!(alloc.coded_bands, 16);
    assert_eq!(alloc.balance, 0);
    assert_eq!(alloc.intensity, 11);
    assert!(!alloc.dual_stereo);
    assert_eq!(dec.tell_frac(), 89);

    const EXPECTED_PULSES: [i32; 21] = [
        485, 463, 441, 423, 361, 343, 341, 323, 618, 581, 545, 509, 994, 906, 834, 1167, 0, 0, 0,
        0, 0,
    ];
    const EXPECTED_FINE_QUANT: [i32; 21] =
        [3, 3, 3, 3, 3, 3, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 1, 1, 1, 1, 1];
    const EXPECTED_FINE_PRIO: [i32; 21] =
        [0, 0, 1, 1, 1, 1, 0, 0, 1, 1, 1, 1, 0, 1, 1, 0, 0, 0, 0, 0, 0];

    assert_eq!(alloc.pulses, EXPECTED_PULSES);
    assert_eq!(alloc.fine_quant, EXPECTED_FINE_QUANT);
    assert_eq!(alloc.fine_priority.map(i32::from), EXPECTED_FINE_PRIO);

    return;
}

/// Verifies `decode_bit_allocation` (`decode_band_boosts_and_trim` + `anti_collapse_rsv` +
/// `BandAllocation::compute`) on a transient (`is_transient = true, lm = 2`) frame with
/// active dynamic boosts (`offsets[0] = 32, offsets[3] = 64`) and `alloc_trim = 7`
/// against the recorded values.
#[test]
fn frame_allocation_with_transient_and_band_boosts_matches_recorded_values() {
    const PACKET5: [u8; 48] = [
        0xfd, 0xda, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x0a, 0xaa, 0xaa,
    ];
    let mut dec = Decoder::new(&PACKET5);
    let (alloc, anti_collapse_rsv) = decode_bit_allocation(
        &mut dec,
        AllocationLayout { start: 0, end: 21, channels: 1, lm: BlockSize::new(2) },
        true,
        48 * 8,
    )
    .expect("transient dynalloc frame allocation failed");

    assert_eq!(anti_collapse_rsv, 8);
    assert_eq!(alloc.coded_bands, 16);
    assert_eq!(alloc.balance, 0);
    assert_eq!(dec.tell_frac(), 203);

    const EXPECTED_PULSES: [i32; 21] = [
        150, 119, 111, 161, 102, 96, 90, 84, 163, 159, 147, 135, 255, 234, 209, 285, 0, 0, 0, 0, 0,
    ];
    const EXPECTED_FINE_QUANT: [i32; 21] =
        [4, 3, 3, 4, 2, 2, 2, 2, 3, 2, 2, 2, 3, 2, 2, 2, 1, 1, 1, 1, 1];
    const EXPECTED_FINE_PRIO: [i32; 21] =
        [0, 0, 1, 0, 0, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0];

    assert_eq!(alloc.pulses, EXPECTED_PULSES);
    assert_eq!(alloc.fine_quant, EXPECTED_FINE_QUANT);
    assert_eq!(alloc.fine_priority.map(i32::from), EXPECTED_FINE_PRIO);

    return;
}
