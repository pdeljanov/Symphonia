//! Tests of [`crate::toc`].

use super::*;

#[derive(Debug)]
struct TestCase {
    toc_byte: u8,
    stereo: bool,
    frame_count: FrameCountCode,
    audio_mode: AudioMode,
    bandwidth: Bandwidth,
    frame_size: FrameDuration,
}

impl TestCase {
    fn check(&self) {
        let toc = Toc::from(self.toc_byte);

        assert_eq!(
            toc.stereo, self.stereo,
            "TOC byte: {:#010b}, expected stereo: {}, got: {}",
            self.toc_byte, self.stereo, toc.stereo,
        );

        assert_eq!(
            toc.frame_count_code, self.frame_count,
            "TOC byte: {:#010b}, expected frame count: {:?}, got: {:?}",
            self.toc_byte, self.frame_count, toc.frame_count_code
        );

        assert_eq!(
            toc.audio_mode(),
            self.audio_mode,
            "TOC byte: {:#010b}, expected audio mode: {:?}, got: {:?}",
            self.toc_byte,
            self.audio_mode,
            toc.audio_mode()
        );

        assert_eq!(
            toc.bandwidth(),
            self.bandwidth,
            "TOC byte: {:#010b}, expected bandwidth: {:?}, got: {:?}",
            self.toc_byte,
            self.bandwidth,
            toc.bandwidth()
        );

        assert_eq!(
            toc.frame_duration(),
            self.frame_size,
            "TOC byte: {:#010b}, expected frame size: {:?}, got: {:?}",
            self.toc_byte,
            self.frame_size,
            toc.frame_duration()
        );
    }
}

fn new_toc_byte(
    is_stereo: bool,
    frame_count: FrameCountCode,
    audio_mode: AudioMode,
    bandwidth: Bandwidth,
    frame_size: FrameDuration,
) -> u8 {
    let config_number = match (audio_mode, bandwidth, frame_size) {
        (AudioMode::Silk, Bandwidth::Narrowband, FrameDuration::Ms10) => 0,
        (AudioMode::Silk, Bandwidth::Narrowband, FrameDuration::Ms20) => 1,
        (AudioMode::Silk, Bandwidth::Narrowband, FrameDuration::Ms40) => 2,
        (AudioMode::Silk, Bandwidth::Narrowband, FrameDuration::Ms60) => 3,
        (AudioMode::Silk, Bandwidth::MediumBand, FrameDuration::Ms10) => 4,
        (AudioMode::Silk, Bandwidth::MediumBand, FrameDuration::Ms20) => 5,
        (AudioMode::Silk, Bandwidth::MediumBand, FrameDuration::Ms40) => 6,
        (AudioMode::Silk, Bandwidth::MediumBand, FrameDuration::Ms60) => 7,
        (AudioMode::Silk, Bandwidth::Wideband, FrameDuration::Ms10) => 8,
        (AudioMode::Silk, Bandwidth::Wideband, FrameDuration::Ms20) => 9,
        (AudioMode::Silk, Bandwidth::Wideband, FrameDuration::Ms40) => 10,
        (AudioMode::Silk, Bandwidth::Wideband, FrameDuration::Ms60) => 11,
        (AudioMode::Hybrid, Bandwidth::SuperWideband, FrameDuration::Ms10) => 12,
        (AudioMode::Hybrid, Bandwidth::SuperWideband, FrameDuration::Ms20) => 13,
        (AudioMode::Hybrid, Bandwidth::Fullband, FrameDuration::Ms10) => 14,
        (AudioMode::Hybrid, Bandwidth::Fullband, FrameDuration::Ms20) => 15,
        (AudioMode::Celt, Bandwidth::Narrowband, FrameDuration::Ms2_5) => 16,
        (AudioMode::Celt, Bandwidth::Narrowband, FrameDuration::Ms5) => 17,
        (AudioMode::Celt, Bandwidth::Narrowband, FrameDuration::Ms10) => 18,
        (AudioMode::Celt, Bandwidth::Narrowband, FrameDuration::Ms20) => 19,
        (AudioMode::Celt, Bandwidth::Wideband, FrameDuration::Ms2_5) => 20,
        (AudioMode::Celt, Bandwidth::Wideband, FrameDuration::Ms5) => 21,
        (AudioMode::Celt, Bandwidth::Wideband, FrameDuration::Ms10) => 22,
        (AudioMode::Celt, Bandwidth::Wideband, FrameDuration::Ms20) => 23,
        (AudioMode::Celt, Bandwidth::SuperWideband, FrameDuration::Ms2_5) => 24,
        (AudioMode::Celt, Bandwidth::SuperWideband, FrameDuration::Ms5) => 25,
        (AudioMode::Celt, Bandwidth::SuperWideband, FrameDuration::Ms10) => 26,
        (AudioMode::Celt, Bandwidth::SuperWideband, FrameDuration::Ms20) => 27,
        (AudioMode::Celt, Bandwidth::Fullband, FrameDuration::Ms2_5) => 28,
        (AudioMode::Celt, Bandwidth::Fullband, FrameDuration::Ms5) => 29,
        (AudioMode::Celt, Bandwidth::Fullband, FrameDuration::Ms10) => 30,
        (AudioMode::Celt, Bandwidth::Fullband, FrameDuration::Ms20) => 31,
        _ => panic!("Invalid audio mode or bandwidth or frame size"),
    };

    let stereo_flag = if is_stereo { 1 << 6 } else { 0 };

    let frame_count_code = match frame_count {
        FrameCountCode::Single => 0,
        FrameCountCode::TwoEqual => 1 << 6,
        FrameCountCode::TwoVariable => 1 << 7,
        FrameCountCode::Signaled => 3 << 6,
    };

    return (config_number << 3) | stereo_flag | frame_count_code;
}
fn populate_test_table() -> Vec<TestCase> {
    vec![
        TestCase {
            toc_byte: 0b00000000,
            stereo: false,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::Narrowband,
            frame_size: FrameDuration::Ms10,
        },
        TestCase {
            toc_byte: 0b00000001,
            stereo: false,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::Narrowband,
            frame_size: FrameDuration::Ms10,
        },
        TestCase {
            toc_byte: 0b00000100,
            stereo: true,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::Narrowband,
            frame_size: FrameDuration::Ms10,
        },
        TestCase {
            toc_byte: 0b00000101,
            stereo: true,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::Narrowband,
            frame_size: FrameDuration::Ms10,
        },
        TestCase {
            toc_byte: 0b00010000,
            stereo: false,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::Narrowband,
            frame_size: FrameDuration::Ms40,
        },
        TestCase {
            toc_byte: 0b00010001,
            stereo: false,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::Narrowband,
            frame_size: FrameDuration::Ms40,
        },
        TestCase {
            toc_byte: 0b00010100,
            stereo: true,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::Narrowband,
            frame_size: FrameDuration::Ms40,
        },
        TestCase {
            toc_byte: 0b00010101,
            stereo: true,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::Narrowband,
            frame_size: FrameDuration::Ms40,
        },
        TestCase {
            toc_byte: 0b00011100,
            stereo: true,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::Narrowband,
            frame_size: FrameDuration::Ms60,
        },
        TestCase {
            toc_byte: 0b00011101,
            stereo: true,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::Narrowband,
            frame_size: FrameDuration::Ms60,
        },
        TestCase {
            toc_byte: 0b00100000,
            stereo: false,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::MediumBand,
            frame_size: FrameDuration::Ms10,
        },
        TestCase {
            toc_byte: 0b00100001,
            stereo: false,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::MediumBand,
            frame_size: FrameDuration::Ms10,
        },
        TestCase {
            toc_byte: 0b00100100,
            stereo: true,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::MediumBand,
            frame_size: FrameDuration::Ms10,
        },
        TestCase {
            toc_byte: 0b00100101,
            stereo: true,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::MediumBand,
            frame_size: FrameDuration::Ms10,
        },
        TestCase {
            toc_byte: 0b00101100,
            stereo: true,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::MediumBand,
            frame_size: FrameDuration::Ms20,
        },
        TestCase {
            toc_byte: 0b00101101,
            stereo: true,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::MediumBand,
            frame_size: FrameDuration::Ms20,
        },
        TestCase {
            toc_byte: 0b00110000,
            stereo: false,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::MediumBand,
            frame_size: FrameDuration::Ms40,
        },
        TestCase {
            toc_byte: 0b00110001,
            stereo: false,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::MediumBand,
            frame_size: FrameDuration::Ms40,
        },
        TestCase {
            toc_byte: 0b00110100,
            stereo: true,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::MediumBand,
            frame_size: FrameDuration::Ms40,
        },
        TestCase {
            toc_byte: 0b00110101,
            stereo: true,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::MediumBand,
            frame_size: FrameDuration::Ms40,
        },
        TestCase {
            toc_byte: 0b00111100,
            stereo: true,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::MediumBand,
            frame_size: FrameDuration::Ms60,
        },
        TestCase {
            toc_byte: 0b00111101,
            stereo: true,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::MediumBand,
            frame_size: FrameDuration::Ms60,
        },
        TestCase {
            toc_byte: 0b01001000,
            stereo: false,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::Wideband,
            frame_size: FrameDuration::Ms20,
        },
        TestCase {
            toc_byte: 0b01001001,
            stereo: false,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::Wideband,
            frame_size: FrameDuration::Ms20,
        },
        TestCase {
            toc_byte: 0b01001100,
            stereo: true,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::Wideband,
            frame_size: FrameDuration::Ms20,
        },
        TestCase {
            toc_byte: 0b01001101,
            stereo: true,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Silk,
            bandwidth: Bandwidth::Wideband,
            frame_size: FrameDuration::Ms20,
        },
        TestCase {
            toc_byte: 0b01100000,
            stereo: false,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Hybrid,
            bandwidth: Bandwidth::SuperWideband,
            frame_size: FrameDuration::Ms10,
        },
        TestCase {
            toc_byte: 0b01100001,
            stereo: false,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Hybrid,
            bandwidth: Bandwidth::SuperWideband,
            frame_size: FrameDuration::Ms10,
        },
        TestCase {
            toc_byte: 0b01100100,
            stereo: true,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Hybrid,
            bandwidth: Bandwidth::SuperWideband,
            frame_size: FrameDuration::Ms10,
        },
        TestCase {
            toc_byte: 0b01100101,
            stereo: true,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Hybrid,
            bandwidth: Bandwidth::SuperWideband,
            frame_size: FrameDuration::Ms10,
        },
        TestCase {
            toc_byte: 0b10000000,
            stereo: false,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Celt,
            bandwidth: Bandwidth::Narrowband,
            frame_size: FrameDuration::Ms2_5,
        },
        TestCase {
            toc_byte: 0b10000001,
            stereo: false,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Celt,
            bandwidth: Bandwidth::Narrowband,
            frame_size: FrameDuration::Ms2_5,
        },
        TestCase {
            toc_byte: 0b10000100,
            stereo: true,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Celt,
            bandwidth: Bandwidth::Narrowband,
            frame_size: FrameDuration::Ms2_5,
        },
        TestCase {
            toc_byte: 0b10000101,
            stereo: true,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Celt,
            bandwidth: Bandwidth::Narrowband,
            frame_size: FrameDuration::Ms2_5,
        },
        TestCase {
            toc_byte: 0b10001000,
            stereo: false,
            frame_count: FrameCountCode::Single,
            audio_mode: AudioMode::Celt,
            bandwidth: Bandwidth::Narrowband,
            frame_size: FrameDuration::Ms5,
        },
        TestCase {
            toc_byte: 0b10001001,
            stereo: false,
            frame_count: FrameCountCode::TwoEqual,
            audio_mode: AudioMode::Celt,
            bandwidth: Bandwidth::Narrowband,
            frame_size: FrameDuration::Ms5,
        },
    ]
}

#[test]
fn run_all_cases() {
    for case in populate_test_table() {
        case.check();
    }
}

#[test]
fn every_byte_round_trips_through_the_toc_fields() {
    for byte in u8::MIN..=u8::MAX {
        let toc = Toc::from(byte);
        assert!(toc.config.number() < 32);
        assert_eq!(toc.as_byte(), byte);
    }
}

#[test]
fn frame_size_to_duration() {
    assert_eq!(Duration::from(FrameDuration::Ms2_5), Duration::from_nanos(2_500_000));
    assert_eq!(Duration::from(FrameDuration::Ms5), Duration::from_nanos(5_000_000));
    assert_eq!(Duration::from(FrameDuration::Ms10), Duration::from_nanos(10_000_000));
    assert_eq!(Duration::from(FrameDuration::Ms20), Duration::from_nanos(20_000_000));
    assert_eq!(Duration::from(FrameDuration::Ms40), Duration::from_nanos(40_000_000));
    assert_eq!(Duration::from(FrameDuration::Ms60), Duration::from_nanos(60_000_000));
}

macro_rules! test_mapping {
    ($field:ident, $object:ident,  $inputs:expr) => {
        #[test]
        fn $field() {
            for (input, expected) in $inputs.iter() {
                let instance = $object::from(*input);
                assert_eq!(
                    instance.$field,
                    *expected,
                    "{} {:?} should be {:?} for {}",
                    stringify!($object),
                    input,
                    expected,
                    stringify!($field)
                );
            }
        }
    };
}

test_mapping!(
    frame_count_code,
    Toc,
    [
        (0b00000000, FrameCountCode::Single),
        (0b00000001, FrameCountCode::TwoEqual),
        (0b00000010, FrameCountCode::TwoVariable),
        (0b00000011, FrameCountCode::Signaled),
    ]
);

#[test]
fn as_byte() {
    for t in populate_test_table() {
        let toc = Toc::from(t.toc_byte);
        let as_byte = toc.as_byte();

        assert_eq!(
            as_byte, t.toc_byte,
            "Failed to test as_byte result. Original: {:#010b}, as_byte(): {:#010b}",
            t.toc_byte, as_byte
        );

        let reconstructed =
            new_toc_byte(t.stereo, t.frame_count, t.audio_mode, t.bandwidth, t.frame_size);
        assert_eq!(
            as_byte, t.toc_byte,
            "Failed to reconstruct TOC byte. Original: {:#010b}, Reconstructed: {:#010b}",
            t.toc_byte, reconstructed
        )
    }
}
