//! Tests of [`crate::celt::decoder`].

#![expect(
    clippy::excessive_precision,
    reason = "recorded values keep the 9 significant digits of the dump"
)]

// Fixtures: packets and expected values recorded from libopus 1.6.1. Decoding of received
// frames is normative (RFC 6716 Section 4.3) and is verified against the RFC 8251 test
// vectors by `tests/rfc6716_conformance.rs`; the concealment steps are regression pins for
// behaviour that RFC 6716 Section 4.4 leaves to the implementation.
use super::*;

/// Frame layout for a test decode: `frame_size_48k` samples at 48 kHz, `start_band`,
/// `bandwidth` and `stream_channels` as the TOC would give them.
fn layout(
    frame_size_48k: usize,
    start_band: usize,
    bandwidth: Bandwidth,
    stream_channels: usize,
) -> FrameLayout {
    return FrameLayout {
        block: BlockSize::from_frame_size_48k(frame_size_48k).expect("valid frame size"),
        start_band,
        bandwidth,
        stream_channels,
    };
}

const MONO_PKT_120: [u8; 20] = [
    0x7c, 0x3c, 0x4a, 0x25, 0xae, 0x69, 0x4c, 0x84, 0xe3, 0x2c, 0x1d, 0x8a, 0x9e, 0x7a, 0x30, 0x0b,
    0xa3, 0xbb, 0xe1, 0x5c,
];
const MONO_HEAD_120: [f32; 8] = [
    -3.11499889e-06_f32,
    -1.29576329e-05_f32,
    -2.13003987e-05_f32,
    7.35655995e-05_f32,
    -5.78037725e-05_f32,
    -9.47556837e-05_f32,
    2.44734198e-04_f32,
    3.67265166e-05_f32,
];
const MONO_TAIL_120: [f32; 8] = [
    -8.53236765e-04_f32,
    -2.93973424e-02_f32,
    -1.93749368e-02_f32,
    5.63502871e-03_f32,
    -2.30330266e-02_f32,
    -1.34612210e-02_f32,
    5.58452401e-03_f32,
    5.10411188e-02_f32,
];
const MONO_RNG_120: u32 = 0x12041c00;

const MONO_PKT_240: [u8; 40] = [
    0x7d, 0x06, 0x44, 0xc4, 0x03, 0xbc, 0xe4, 0x65, 0xe6, 0xd2, 0x49, 0x4e, 0xed, 0xb7, 0xc3, 0x63,
    0x18, 0xe2, 0xd0, 0xa2, 0x91, 0x0e, 0x23, 0x4a, 0x6f, 0x66, 0x31, 0x86, 0xbd, 0x33, 0x4b, 0x44,
    0x56, 0x59, 0xd6, 0xde, 0x8a, 0x80, 0x5f, 0x18,
];
const MONO_HEAD_240: [f32; 8] = [
    1.49026409e-01_f32,
    2.15950310e-01_f32,
    2.79721141e-01_f32,
    2.82028317e-01_f32,
    2.61658549e-01_f32,
    3.21524531e-01_f32,
    3.37180316e-01_f32,
    3.17863524e-01_f32,
];
const MONO_TAIL_240: [f32; 8] = [
    3.37832958e-01_f32,
    3.25293601e-01_f32,
    3.11400712e-01_f32,
    2.96576291e-01_f32,
    2.80858099e-01_f32,
    2.64457643e-01_f32,
    2.47565538e-01_f32,
    2.30448693e-01_f32,
];
const MONO_RNG_240: u32 = 0x013a4a00;

const MONO_PKT_480: [u8; 80] = [
    0x79, 0xed, 0xeb, 0x78, 0xc8, 0x60, 0xe3, 0xc4, 0x2d, 0x2a, 0x8e, 0x4d, 0x4b, 0x8b, 0x77, 0x02,
    0xe7, 0xc0, 0x51, 0x37, 0xfa, 0x36, 0xf4, 0x27, 0xee, 0xe1, 0xbe, 0xd3, 0xb5, 0x6f, 0xe7, 0xf8,
    0xd1, 0x1f, 0x9d, 0x46, 0x92, 0xbb, 0xa7, 0x74, 0x8f, 0x67, 0xe6, 0x87, 0xe8, 0x49, 0x7a, 0x30,
    0x6e, 0x23, 0x00, 0x19, 0x4f, 0xc8, 0xa1, 0xdd, 0xd7, 0xc3, 0x83, 0xbe, 0x66, 0xe9, 0x82, 0x25,
    0x4f, 0xdc, 0x17, 0x01, 0x9d, 0x48, 0x5c, 0x63, 0x9e, 0x71, 0xdf, 0xdc, 0x3e, 0xd3, 0x2e, 0xda,
];
const MONO_HEAD_480: [f32; 8] = [
    2.13207811e-01_f32,
    1.96301848e-01_f32,
    1.79373786e-01_f32,
    1.63763851e-01_f32,
    1.48507282e-01_f32,
    1.33852482e-01_f32,
    1.22132048e-01_f32,
    1.09447777e-01_f32,
];
const MONO_TAIL_480: [f32; 8] = [
    -4.72173393e-01_f32,
    -4.60278928e-01_f32,
    -4.47942793e-01_f32,
    -4.36154902e-01_f32,
    -4.24710900e-01_f32,
    -4.13523257e-01_f32,
    -4.02829885e-01_f32,
    -3.92409801e-01_f32,
];
const MONO_RNG_480: u32 = 0x471a6800;

const MONO_PKT_960: [u8; 160] = [
    0x7c, 0x85, 0x51, 0xaf, 0x45, 0x13, 0xb4, 0x3b, 0x2f, 0xee, 0x14, 0x9b, 0x28, 0x00, 0xcc, 0xc4,
    0xc4, 0x02, 0x1b, 0xd8, 0x64, 0x08, 0xca, 0x41, 0x00, 0xb1, 0xbb, 0x39, 0xe5, 0x35, 0x20, 0x94,
    0xe5, 0x92, 0xa6, 0x00, 0x54, 0xb6, 0x46, 0xaa, 0x1a, 0x29, 0xb8, 0xc3, 0x3d, 0xc7, 0x1b, 0x8e,
    0xee, 0xeb, 0x7e, 0x3f, 0xb9, 0x3d, 0xa4, 0xd7, 0xd7, 0xd6, 0x62, 0x61, 0x74, 0x27, 0x44, 0x0e,
    0x13, 0x7c, 0x60, 0x4a, 0xf4, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x1d,
    0x69, 0x2f, 0xa3, 0x70, 0x00, 0xe6, 0x49, 0x08, 0x18, 0x17, 0x3b, 0xa4, 0x64, 0x8a, 0x08, 0xfa,
    0x5d, 0xd1, 0xe9, 0x98, 0xdb, 0xd6, 0x00, 0x00, 0xa0, 0x19, 0xe9, 0x8d, 0x51, 0x1e, 0x3e, 0x2a,
    0xd0, 0x88, 0x01, 0xbd, 0x88, 0xc7, 0x80, 0x05, 0x27, 0xdb, 0x6d, 0x8d, 0xb6, 0x9b, 0x8f, 0x03,
];
const MONO_HEAD_960: [f32; 8] = [
    -3.82692218e-01_f32,
    -3.73273969e-01_f32,
    -3.64426851e-01_f32,
    -3.55661809e-01_f32,
    -3.47891659e-01_f32,
    -3.40573609e-01_f32,
    -3.32307875e-01_f32,
    -3.26006442e-01_f32,
];
const MONO_TAIL_960: [f32; 8] = [
    5.53317666e-02_f32,
    4.05748375e-02_f32,
    2.44297516e-02_f32,
    6.78314455e-03_f32,
    -1.19780730e-02_f32,
    -3.21609192e-02_f32,
    -5.35312481e-02_f32,
    -7.58339986e-02_f32,
];
const MONO_RNG_960: u32 = 0x3c141500;

const PLC_HEAD_0: [f32; 8] = [
    -9.92090106e-02_f32,
    -1.23355642e-01_f32,
    -1.48679987e-01_f32,
    -1.74979076e-01_f32,
    -2.01964051e-01_f32,
    -2.29726657e-01_f32,
    -2.57801831e-01_f32,
    -2.86338896e-01_f32,
];
const PLC_TAIL_0: [f32; 8] = [
    -2.79531717e-01_f32,
    -2.73246348e-01_f32,
    -2.66399741e-01_f32,
    -2.59047687e-01_f32,
    -2.51049578e-01_f32,
    -2.42268860e-01_f32,
    -2.32731909e-01_f32,
    -2.22371534e-01_f32,
];
const PLC_RNG_0: u32 = 0x3c141500;

const PLC_HEAD_1: [f32; 8] = [
    -2.11372018e-01_f32,
    -1.99950516e-01_f32,
    -1.87685072e-01_f32,
    -1.74513876e-01_f32,
    -1.60712898e-01_f32,
    -1.46051675e-01_f32,
    -1.30629361e-01_f32,
    -1.14582062e-01_f32,
];
const PLC_TAIL_1: [f32; 8] = [
    6.04914837e-02_f32,
    6.15975000e-02_f32,
    6.36616424e-02_f32,
    6.65952265e-02_f32,
    7.03878999e-02_f32,
    7.49458671e-02_f32,
    8.01428184e-02_f32,
    8.60325098e-02_f32,
];
const PLC_RNG_1: u32 = 0x3c141500;

const PLC_HEAD_5: [f32; 8] = [
    2.98432112e-02_f32,
    3.11845299e-02_f32,
    3.29137966e-02_f32,
    3.49432863e-02_f32,
    3.68133858e-02_f32,
    3.84001546e-02_f32,
    4.14124504e-02_f32,
    4.40755300e-02_f32,
];
const PLC_TAIL_5: [f32; 8] = [
    1.68310285e-01_f32,
    1.37671083e-01_f32,
    1.33715436e-01_f32,
    1.84914246e-01_f32,
    1.79313451e-01_f32,
    2.61573136e-01_f32,
    3.10502410e-01_f32,
    2.62677759e-01_f32,
];
const PLC_RNG_5: u32 = 0xd911c0a0;

const RECOVERY_PKT: [u8; 80] = [
    0x78, 0x38, 0xdc, 0x47, 0xf1, 0xef, 0x72, 0xad, 0x17, 0x39, 0x88, 0x60, 0x4d, 0xd5, 0x72, 0x9d,
    0x0a, 0xea, 0x6e, 0x34, 0xd6, 0x79, 0xf4, 0xbb, 0xd2, 0x34, 0xee, 0x39, 0x6e, 0xa3, 0x03, 0xce,
    0x7a, 0xf4, 0x47, 0x40, 0x09, 0xa3, 0x9e, 0x3f, 0x5c, 0x7e, 0xa5, 0x5c, 0xcc, 0x32, 0x0d, 0xa3,
    0xd8, 0x48, 0x00, 0x5e, 0x73, 0x40, 0x00, 0x8a, 0x42, 0xc0, 0x00, 0x80, 0x02, 0x6c, 0x00, 0x0f,
    0xf8, 0x86, 0x08, 0x07, 0xf4, 0xc7, 0x5f, 0x08, 0xdc, 0xe0, 0xa2, 0x57, 0x58, 0xf0, 0x34, 0x93,
];
const RECOVERY_HEAD: [f32; 8] = [
    2.60167867e-01_f32,
    1.50642604e-01_f32,
    8.24194700e-02_f32,
    1.21272191e-01_f32,
    1.05687872e-01_f32,
    1.06321760e-01_f32,
    9.18942466e-02_f32,
    9.43008214e-02_f32,
];
const RECOVERY_TAIL: [f32; 8] = [
    9.41674225e-04_f32,
    9.99980140e-04_f32,
    1.05673436e-03_f32,
    1.11144036e-03_f32,
    1.16428710e-03_f32,
    1.21546513e-03_f32,
    1.26444083e-03_f32,
    1.31133106e-03_f32,
];
const RECOVERY_RNG: u32 = 0x0083c940;

fn assert_f32_slice_close(actual: &[f32], expected: &[f32], tol: f32) {
    assert_eq!(actual.len(), expected.len());
    for (i, (&a, &e)) in actual.iter().zip(expected.iter()).enumerate() {
        let diff = (a - e).abs();
        assert!(
            diff <= tol,
            "sample {i} mismatch: actual={a:.8e}, expected={e:.8e}, diff={diff:.8e} > tol={tol:.8e}"
        );
    }
    return;
}

#[test]
fn celt_mono_multiframe_decodes_and_conceals_as_recorded() {
    let mut dec = Decoder::new(SampleRate::Hz48000, 1).expect("valid mono decoder");
    let mut out = [0.0_f32; 960];

    // 1. Decode 2.5 ms (120), 5 ms (240), 10 ms (480, transient), and 20 ms (960) frames.
    for (pkt, n, exp_head, exp_tail, exp_rng) in [
        (&MONO_PKT_120[..], 120usize, &MONO_HEAD_120, &MONO_TAIL_120, MONO_RNG_120),
        (&MONO_PKT_240[..], 240usize, &MONO_HEAD_240, &MONO_TAIL_240, MONO_RNG_240),
        (&MONO_PKT_480[..], 480usize, &MONO_HEAD_480, &MONO_TAIL_480, MONO_RNG_480),
        (&MONO_PKT_960[..], 960usize, &MONO_HEAD_960, &MONO_TAIL_960, MONO_RNG_960),
    ] {
        let written = dec
            .decode_frame(
                FrameInput::Packet(pkt),
                layout(n, 0, Bandwidth::Fullband, 1),
                Planes::mono(&mut out[..n]),
                Mix::Overwrite,
            )
            .expect("decode mono frame");
        assert_eq!(written, n);
        assert_eq!(dec.final_range(), exp_rng);
        assert_f32_slice_close(&out[..8], exp_head, 2e-5);
        assert_f32_slice_close(&out[(n - 8)..n], exp_tail, 2e-5);
    }

    // 2. Conceal 6 consecutive 20 ms lost frames:
    //    - i = 0..4 use pitch-based LPC extrapolation (loss_duration = 0, 8, 16, 24, 32 < 40).
    //    - i = 5 transitions to spectral LCG noise concealment (loss_duration = 40 >= 40).
    for i in 0..6 {
        let written = dec
            .decode_frame(
                FrameInput::Lost,
                layout(960, 0, Bandwidth::Fullband, 1),
                Planes::mono(&mut out),
                Mix::Overwrite,
            )
            .expect("conceal lost frame");
        assert_eq!(written, 960);
        match i {
            0 => {
                assert_eq!(dec.final_range(), PLC_RNG_0);
                assert_f32_slice_close(&out[..8], &PLC_HEAD_0, 2e-5);
                assert_f32_slice_close(&out[952..960], &PLC_TAIL_0, 2e-5);
            }
            1 => {
                assert_eq!(dec.final_range(), PLC_RNG_1);
                assert_f32_slice_close(&out[..8], &PLC_HEAD_1, 2e-5);
                assert_f32_slice_close(&out[952..960], &PLC_TAIL_1, 2e-5);
            }
            5 => {
                assert_eq!(dec.final_range(), PLC_RNG_5);
                assert_f32_slice_close(&out[..8], &PLC_HEAD_5, 2e-5);
                assert_f32_slice_close(&out[952..960], &PLC_TAIL_5, 2e-5);
            }
            _ => {}
        }
    }

    // 3. Decode recovery frame after packet loss (exercises apply_loss_recovery_energy_safety).
    let written = dec
        .decode_frame(
            FrameInput::Packet(&RECOVERY_PKT),
            layout(960, 0, Bandwidth::Fullband, 1),
            Planes::mono(&mut out),
            Mix::Overwrite,
        )
        .expect("decode recovery frame");
    assert_eq!(written, 960);
    assert_eq!(dec.final_range(), RECOVERY_RNG);
    assert_f32_slice_close(&out[..8], &RECOVERY_HEAD, 2e-5);
    assert_f32_slice_close(&out[952..960], &RECOVERY_TAIL, 2e-5);
    return;
}

#[test]
fn celt_stereo_silence_and_mono_in_stereo_match_recorded_values() {
    const STEREO_PKT_480: [u8; 60] = [
        0x7f, 0xaa, 0x5a, 0x6f, 0x47, 0xc9, 0x27, 0x2d, 0xa6, 0x0c, 0xb9, 0xa0, 0x34, 0x27, 0x45,
        0x2e, 0x43, 0x03, 0x2c, 0x61, 0x24, 0x02, 0x4b, 0x1b, 0xc6, 0x55, 0x1c, 0x43, 0xdc, 0xd5,
        0xc9, 0x9e, 0xb8, 0xa0, 0x0c, 0x8e, 0x63, 0xf5, 0xc6, 0x38, 0xda, 0x1b, 0x12, 0xc0, 0x6a,
        0x2f, 0x31, 0x6c, 0xac, 0x29, 0xf2, 0x7f, 0x0e, 0x43, 0x3c, 0xbe, 0x3a, 0xf3, 0xca, 0x28,
    ];
    const STEREO_L_TAIL_480: [f32; 8] = [
        -4.17376965e-01_f32,
        -4.00183052e-01_f32,
        -3.92758131e-01_f32,
        -4.02621746e-01_f32,
        -3.88109535e-01_f32,
        -3.74064475e-01_f32,
        -3.80719841e-01_f32,
        -3.67973894e-01_f32,
    ];
    const STEREO_R_TAIL_480: [f32; 8] = [
        -1.45076156e-01_f32,
        -1.27458289e-01_f32,
        -1.11662380e-01_f32,
        -9.88233164e-02_f32,
        -8.15347508e-02_f32,
        -6.38602301e-02_f32,
        -4.98592928e-02_f32,
        -3.31351310e-02_f32,
    ];
    const STEREO_RNG_480: u32 = 0x37ba0a00;

    const STEREO_PKT_960: [u8; 120] = [
        0xca, 0x38, 0xaf, 0xe2, 0xfb, 0x71, 0xca, 0x2c, 0xda, 0xf6, 0xb3, 0xdf, 0xe6, 0x36, 0xef,
        0xcc, 0x9d, 0x6b, 0x0b, 0xcd, 0x68, 0x31, 0x20, 0x4f, 0xaf, 0x9a, 0x36, 0x51, 0xf8, 0xf2,
        0x76, 0x55, 0x45, 0xd6, 0xf6, 0x6c, 0xfb, 0x75, 0x9c, 0xb4, 0xe1, 0xce, 0xbf, 0x48, 0x8f,
        0xa7, 0xad, 0x32, 0x6b, 0x48, 0x6d, 0x53, 0x03, 0x10, 0xda, 0xa9, 0x3b, 0x9d, 0xa9, 0xaf,
        0xf1, 0x9d, 0xef, 0x41, 0x15, 0xad, 0x9b, 0x78, 0x9d, 0x23, 0xda, 0x26, 0x77, 0x83, 0xab,
        0x36, 0x91, 0x28, 0x17, 0xdb, 0x66, 0x86, 0x27, 0x73, 0x56, 0x9d, 0x5a, 0x08, 0x88, 0x00,
        0x77, 0x29, 0xae, 0xca, 0x44, 0x00, 0x94, 0x50, 0xc0, 0x01, 0x00, 0x03, 0xcd, 0xc4, 0x0a,
        0x10, 0xfd, 0xdb, 0x54, 0x6f, 0x67, 0x8c, 0xdc, 0x99, 0x9f, 0x7d, 0x72, 0x33, 0x8d, 0xc1,
    ];
    const STEREO_L_TAIL_960: [f32; 8] = [
        -9.60404426e-02_f32,
        -1.08324490e-01_f32,
        -1.20585375e-01_f32,
        -1.32816896e-01_f32,
        -1.45001888e-01_f32,
        -1.57111838e-01_f32,
        -1.69115692e-01_f32,
        -1.80991650e-01_f32,
    ];
    const STEREO_R_TAIL_960: [f32; 8] = [
        1.83009833e-01_f32,
        1.66810691e-01_f32,
        1.50145844e-01_f32,
        1.33115575e-01_f32,
        1.15805209e-01_f32,
        9.82826427e-02_f32,
        8.06091353e-02_f32,
        6.28555566e-02_f32,
    ];
    const STEREO_RNG_960: u32 = 0x029cef00;

    const SILENCE_L_HEAD: [f32; 8] = [
        -1.92732185e-01_f32,
        -2.04334676e-01_f32,
        -2.15793774e-01_f32,
        -2.27094308e-01_f32,
        -2.38211840e-01_f32,
        -2.49119058e-01_f32,
        -2.59790957e-01_f32,
        -2.70205140e-01_f32,
    ];
    const SILENCE_R_HEAD: [f32; 8] = [
        4.51083407e-02_f32,
        2.74521299e-02_f32,
        9.96772107e-03_f32,
        -7.27671199e-03_f32,
        -2.42255889e-02_f32,
        -4.08281609e-02_f32,
        -5.70321754e-02_f32,
        -7.27841631e-02_f32,
    ];
    const SILENCE_RNG: u32 = 0x01000000;

    const MONO_IN_STEREO_PKT: [u8; 40] = [
        0x7f, 0x52, 0xdb, 0x7b, 0x1f, 0x34, 0x78, 0x79, 0xba, 0xe8, 0x4c, 0x33, 0xdf, 0x7b, 0xce,
        0x11, 0x2f, 0xb0, 0xc7, 0xd7, 0x6b, 0x3a, 0x40, 0xf8, 0x45, 0x4e, 0xbe, 0x3e, 0x32, 0x4f,
        0x2d, 0x3c, 0x5e, 0x79, 0xf5, 0xce, 0x80, 0x2c, 0xd1, 0xe9,
    ];
    const MONO_IN_STEREO_L_TAIL: [f32; 8] = [
        -1.26281887e-01_f32,
        -1.11190245e-01_f32,
        -9.57806483e-02_f32,
        -8.00936297e-02_f32,
        -6.41752258e-02_f32,
        -4.80800085e-02_f32,
        -3.18579338e-02_f32,
        -1.55676492e-02_f32,
    ];
    const MONO_IN_STEREO_R_TAIL: [f32; 8] = [
        -1.26281887e-01_f32,
        -1.11190245e-01_f32,
        -9.57806483e-02_f32,
        -8.00936297e-02_f32,
        -6.41752258e-02_f32,
        -4.80800085e-02_f32,
        -3.18579338e-02_f32,
        -1.55676492e-02_f32,
    ];
    const MONO_IN_STEREO_RNG: u32 = 0x5a536100;

    let mut dec = Decoder::new(SampleRate::Hz48000, 2).expect("valid stereo decoder");
    let mut out_l = [0.0_f32; 960];
    let mut out_r = [0.0_f32; 960];

    // 1. 10 ms stereo transient frame (N = 480)
    let n1 = dec
        .decode_frame(
            FrameInput::Packet(&STEREO_PKT_480),
            layout(480, 0, Bandwidth::Fullband, 2),
            Planes::new(&mut out_l[..480], Some(&mut out_r[..480])),
            Mix::Overwrite,
        )
        .expect("decode stereo 480");
    assert_eq!(n1, 480);
    assert_eq!(dec.final_range(), STEREO_RNG_480);
    assert_f32_slice_close(&out_l[472..480], &STEREO_L_TAIL_480, 2e-5);
    assert_f32_slice_close(&out_r[472..480], &STEREO_R_TAIL_480, 2e-5);

    // 2. 20 ms stereo steady-state frame (N = 960)
    let n2 = dec
        .decode_frame(
            FrameInput::Packet(&STEREO_PKT_960),
            layout(960, 0, Bandwidth::Fullband, 2),
            Planes::new(&mut out_l, Some(&mut out_r[..])),
            Mix::Overwrite,
        )
        .expect("decode stereo 960");
    assert_eq!(n2, 960);
    assert_eq!(dec.final_range(), STEREO_RNG_960);
    assert_f32_slice_close(&out_l[952..960], &STEREO_L_TAIL_960, 2e-5);
    assert_f32_slice_close(&out_r[952..960], &STEREO_R_TAIL_960, 2e-5);

    // 3. Silence frame ([0xFF]) on active stereo decoder smoothly fades out the MDCT overlap tail.
    let n3 = dec
        .decode_frame(
            FrameInput::new(Some(&[0xFF]), None),
            layout(120, 0, Bandwidth::Fullband, 2),
            Planes::new(&mut out_l[..120], Some(&mut out_r[..120])),
            Mix::Overwrite,
        )
        .expect("decode silence frame");
    assert_eq!(n3, 120);
    assert_eq!(dec.final_range(), SILENCE_RNG);
    assert_f32_slice_close(&out_l[..8], &SILENCE_L_HEAD, 2e-5);
    assert_f32_slice_close(&out_r[..8], &SILENCE_R_HEAD, 2e-5);

    // 4. Mono-in-stereo (stream_channels = 1, output_channels = 2)
    let n4 = dec
        .decode_frame(
            FrameInput::Packet(&MONO_IN_STEREO_PKT),
            layout(240, 0, Bandwidth::Fullband, 1),
            Planes::new(&mut out_l[..240], Some(&mut out_r[..240])),
            Mix::Overwrite,
        )
        .expect("decode mono-in-stereo frame");
    assert_eq!(n4, 240);
    assert_eq!(dec.final_range(), MONO_IN_STEREO_RNG);
    assert_f32_slice_close(&out_l[232..240], &MONO_IN_STEREO_L_TAIL, 2e-5);
    assert_f32_slice_close(&out_r[232..240], &MONO_IN_STEREO_R_TAIL, 2e-5);
    return;
}

#[test]
fn celt_hybrid_highband_on_a_shared_range_decoder_matches_recorded_values() {
    const HYBRID_PKT: [u8; 48] = [
        0x29, 0x72, 0xbb, 0x04, 0x4d, 0x96, 0xdf, 0x28, 0x71, 0xba, 0x03, 0x4c, 0x95, 0xde, 0x27,
        0x70, 0xb9, 0x02, 0x4b, 0x94, 0xdd, 0x26, 0x6f, 0xb8, 0x01, 0x4a, 0x93, 0xdc, 0x25, 0x6e,
        0xb7, 0x00, 0x49, 0x92, 0xdb, 0x24, 0x6d, 0xb6, 0xff, 0x48, 0x91, 0xda, 0x23, 0x6c, 0xb5,
        0xfe, 0x47, 0x90,
    ];
    const HYBRID_HEAD: [f32; 8] = [
        -1.00000000e+00_f32,
        -8.75000417e-01_f32,
        -7.50000894e-01_f32,
        -6.25000179e-01_f32,
        -4.99999672e-01_f32,
        -3.74994516e-01_f32,
        -2.49995813e-01_f32,
        -1.25009388e-01_f32,
    ];
    const HYBRID_TAIL: [f32; 8] = [
        6.25069678e-01_f32,
        7.49322832e-01_f32,
        8.74420822e-01_f32,
        1.00020945e+00_f32,
        -9.99211788e-01_f32,
        -8.74615490e-01_f32,
        -7.50568449e-01_f32,
        -6.25610173e-01_f32,
    ];
    const HYBRID_RNG: u32 = 0x3a35db00;

    let mut dec = Decoder::new(SampleRate::Hz48000, 1).expect("valid mono decoder");

    let mut range_dec = entropy::Decoder::new(&HYBRID_PKT);
    let _ = range_dec.decode_symbol_logp(2).expect("silk bit");
    let _ = range_dec.decode_uint(16).expect("silk uint");

    let mut out_pcm = [0.0_f32; 480];
    for (i, sample) in out_pcm.iter_mut().enumerate() {
        *sample = 0.125_f32 * (((i % 17) as i32 - 8) as f32);
    }

    let written = dec
        .decode_frame(
            FrameInput::Shared(&mut range_dec),
            layout(480, 17, Bandwidth::Fullband, 1),
            Planes::mono(&mut out_pcm),
            Mix::Accumulate,
        )
        .expect("decode hybrid highband");
    assert_eq!(written, 480);
    assert_eq!(dec.final_range(), HYBRID_RNG);
    assert_f32_slice_close(&out_pcm[..8], &HYBRID_HEAD, 2e-5);
    assert_f32_slice_close(&out_pcm[472..480], &HYBRID_TAIL, 2e-5);
    return;
}

#[test]
fn frame_input_classifies_decode_call_arguments() {
    let bytes = [0x12, 0x34];
    let mut shared = entropy::Decoder::new(&bytes);
    let shared_input = FrameInput::new(None, Some(&mut shared));
    assert!(matches!(shared_input, FrameInput::Shared(_)));
    assert!(matches!(FrameInput::new(Some(&[]), Some(&mut shared)), FrameInput::Lost));
    assert!(matches!(FrameInput::new(None, None), FrameInput::Lost));
    assert!(matches!(FrameInput::new(Some(&[]), None), FrameInput::Lost));
    assert!(matches!(FrameInput::new(Some(&[0x7F]), None), FrameInput::Lost));
    assert!(matches!(FrameInput::new(Some(&[0xFF]), None), FrameInput::Silence));
    assert!(matches!(FrameInput::new(Some(&bytes), None), FrameInput::Packet(_)));
    return;
}

#[test]
fn synthesis_memory_reset_and_shift() {
    let mut memory = SynthesisMemory::default();
    memory.decode_mem[0][120] = 1.0;
    memory.decode_mem[1][120] = 2.0;
    memory.shift_history(1, 120);
    assert_eq!((memory.decode_mem[0][0], memory.decode_mem[1][0]), (1.0, 0.0));

    memory.preemph_mem[1] = 0.5;
    memory.prefilter_and_fold_next = true;
    memory.reset();
    assert!(memory.decode_mem.iter().flatten().all(|&s| s == 0.0));
    assert_eq!((memory.preemph_mem, memory.prefilter_and_fold_next), ([0.0; 2], false));
    return;
}

#[test]
fn celt_decoder_configuration_and_boundary_validation() {
    assert!(SampleRate::try_from(44_100).is_err());
    assert!(Error::InvalidChannelCount(0).matches(&Decoder::new(SampleRate::Hz48000, 0)));
    assert!(Error::InvalidChannelCount(3).matches(&Decoder::new(SampleRate::Hz48000, 3)));

    let mut dec = Decoder::new(SampleRate::Hz24000, 1).expect("valid 24 kHz decoder");
    let mut full_buf = [0.0_f32; 480];
    let band_range = Error::InvalidBandRange { start: 0, end: 0 };
    for (bad, expected) in [
        (layout(120, 21, Bandwidth::Wideband, 1), band_range),
        (layout(120, 17, Bandwidth::Narrowband, 1), band_range),
        (layout(120, 0, Bandwidth::Wideband, 0), Error::InvalidChannelCount(0)),
        (layout(120, 0, Bandwidth::Wideband, 3), Error::InvalidChannelCount(3)),
    ] {
        let silence = FrameInput::new(Some(&[0xFF]), None);
        let result = dec.decode_frame(silence, bad, Planes::mono(&mut full_buf), Mix::Overwrite);
        assert!(expected.matches(&result));
    }

    dec.reset();
    assert_eq!(dec.final_range(), 0);

    let mut small_buf = [0.0_f32; 10];
    // At 24 kHz (downsample = 2), frame_size_48k = 120 requires 60 output samples.
    let silence = FrameInput::new(Some(&[0xFF]), None);
    let wideband = layout(120, 0, Bandwidth::Wideband, 1);
    let result = dec.decode_frame(silence, wideband, Planes::mono(&mut small_buf), Mix::Overwrite);
    assert!(Error::BufferTooSmall.matches(&result));
    // Invalid 48 kHz MDCT size (e.g. 360) must be rejected.
    assert!(Error::InvalidMdctSize(360).matches(&BlockSize::from_frame_size_48k(360)));
    return;
}
