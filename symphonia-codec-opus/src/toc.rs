//! The TOC byte: the table of contents that opens every Opus packet
//! ([RFC 6716 Section 3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>)).
//!
//! The byte carries a 5-bit configuration number, a stereo flag and a 2-bit frame count code.
//! Every one of the 256 byte values is a valid TOC, so parsing it cannot fail.
//!
//! ```text
//!                               0
//!                               0 1 2 3 4 5 6 7
//!                              +-+-+-+-+-+-+-+-+
//!                              | config  |s| c |
//!                              +-+-+-+-+-+-+-+-+
//!
//!                           Figure 1: The TOC Byte
//! ```

use std::time::Duration;

/// Number of bits the configuration number is shifted left by inside the byte.
const CONFIG_SHIFT: u32 = 3;
/// Mask of the stereo flag `s`.
const STEREO_BIT: u8 = 1 << 2;
/// Mask of the frame count code `c`.
const FRAME_COUNT_CODE_MASK: u8 = 0b11;

/// The decoded TOC byte
/// ([RFC 6716 Section 3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Toc {
    /// The 5-bit configuration number selecting mode, bandwidth and frame duration.
    pub(crate) config: Config,
    /// The `s` flag: `false` for mono, `true` for stereo.
    pub(crate) stereo: bool,
    /// The `c` field selecting how the frames are packed.
    pub(crate) frame_count_code: FrameCountCode,
}

impl From<u8> for Toc {
    fn from(byte: u8) -> Self {
        return Self {
            config: Config(byte >> CONFIG_SHIFT),
            stereo: byte & STEREO_BIT != 0,
            frame_count_code: FrameCountCode::from(byte & FRAME_COUNT_CODE_MASK),
        };
    }
}

impl Toc {
    /// The operating mode selected by the configuration number
    /// ([RFC 6716 Section 3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>)).
    #[inline]
    pub(crate) fn audio_mode(self) -> AudioMode {
        return self.config.audio_mode();
    }

    /// The audio bandwidth selected by the configuration number
    /// ([RFC 6716 Section 3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>)).
    #[inline]
    pub(crate) fn bandwidth(self) -> Bandwidth {
        return self.config.bandwidth();
    }

    /// The duration of one frame selected by the configuration number
    /// ([RFC 6716 Section 3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>)).
    #[inline]
    pub(crate) fn frame_duration(self) -> FrameDuration {
        return self.config.frame_duration();
    }

    /// Re-encodes the byte, the inverse of [`Toc::from`].
    #[cfg(test)]
    pub(crate) fn as_byte(self) -> u8 {
        let stereo = if self.stereo { STEREO_BIT } else { 0 };
        return (self.config.number() << CONFIG_SHIFT) | stereo | self.frame_count_code as u8;
    }
}

/// The 5-bit configuration number (`0..=31`) of the TOC byte, which selects one row of Table 2
/// ([RFC 6716 Section 3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Config(u8);

impl Config {
    /// The configuration number itself, the row of Table 2
    /// ([RFC 6716 Section 3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>)).
    #[inline]
    pub(crate) const fn number(self) -> u8 {
        return self.0;
    }

    /// The operating mode of this configuration, from Table 2
    /// ([RFC 6716 Section 3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>)).
    #[inline]
    pub(crate) fn audio_mode(self) -> AudioMode {
        return CONFIG_TABLE[self.number() as usize].0;
    }

    /// The audio bandwidth of this configuration, from Table 2
    /// ([RFC 6716 Section 3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>)).
    #[inline]
    pub(crate) fn bandwidth(self) -> Bandwidth {
        return CONFIG_TABLE[self.number() as usize].1;
    }

    /// The frame duration of this configuration, from Table 2
    /// ([RFC 6716 Section 3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>)).
    #[inline]
    pub(crate) fn frame_duration(self) -> FrameDuration {
        return CONFIG_TABLE[self.number() as usize].2;
    }
}

/// The configuration table (Table 2), indexed by the configuration number
/// ([RFC 6716 Section 3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>)).
///
/// ```text
/// +-----------------------+-----------+-----------+-------------------+
/// | Configuration         | Mode      | Bandwidth | Frame Sizes       |
/// | Number(s)             |           |           |                   |
/// +-----------------------+-----------+-----------+-------------------+
/// | 0...3                 | SILK-only | NB        | 10, 20, 40, 60 ms |
/// | 4...7                 | SILK-only | MB        | 10, 20, 40, 60 ms |
/// | 8...11                | SILK-only | WB        | 10, 20, 40, 60 ms |
/// | 12...13               | Hybrid    | SWB       | 10, 20 ms         |
/// | 14...15               | Hybrid    | FB        | 10, 20 ms         |
/// | 16...19               | CELT-only | NB        | 2.5, 5, 10, 20 ms |
/// | 20...23               | CELT-only | WB        | 2.5, 5, 10, 20 ms |
/// | 24...27               | CELT-only | SWB       | 2.5, 5, 10, 20 ms |
/// | 28...31               | CELT-only | FB        | 2.5, 5, 10, 20 ms |
/// +-----------------------+-----------+-----------+-------------------+
/// ```
static CONFIG_TABLE: [(AudioMode, Bandwidth, FrameDuration); 32] = {
    use AudioMode::{Celt, Hybrid, Silk};
    use Bandwidth::{Fullband, MediumBand, Narrowband, SuperWideband, Wideband};
    use FrameDuration::{Ms2_5, Ms5, Ms10, Ms20, Ms40, Ms60};
    [
        (Silk, Narrowband, Ms10),
        (Silk, Narrowband, Ms20),
        (Silk, Narrowband, Ms40),
        (Silk, Narrowband, Ms60),
        (Silk, MediumBand, Ms10),
        (Silk, MediumBand, Ms20),
        (Silk, MediumBand, Ms40),
        (Silk, MediumBand, Ms60),
        (Silk, Wideband, Ms10),
        (Silk, Wideband, Ms20),
        (Silk, Wideband, Ms40),
        (Silk, Wideband, Ms60),
        (Hybrid, SuperWideband, Ms10),
        (Hybrid, SuperWideband, Ms20),
        (Hybrid, Fullband, Ms10),
        (Hybrid, Fullband, Ms20),
        (Celt, Narrowband, Ms2_5),
        (Celt, Narrowband, Ms5),
        (Celt, Narrowband, Ms10),
        (Celt, Narrowband, Ms20),
        (Celt, Wideband, Ms2_5),
        (Celt, Wideband, Ms5),
        (Celt, Wideband, Ms10),
        (Celt, Wideband, Ms20),
        (Celt, SuperWideband, Ms2_5),
        (Celt, SuperWideband, Ms5),
        (Celt, SuperWideband, Ms10),
        (Celt, SuperWideband, Ms20),
        (Celt, Fullband, Ms2_5),
        (Celt, Fullband, Ms5),
        (Celt, Fullband, Ms10),
        (Celt, Fullband, Ms20),
    ]
};

/// Operating mode of a frame
/// ([RFC 6716 Section 2](<https://datatracker.ietf.org/doc/html/rfc6716#section-2>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioMode {
    /// Linear prediction layer only, for speech up to wideband.
    Silk,
    /// MDCT layer only, for music and low delay.
    Celt,
    /// Both layers: SILK below 8 kHz, CELT above.
    Hybrid,
}

/// Audio bandwidth of a frame, carried as its effective sample rate in Hz
/// ([RFC 6716 Section 2](<https://datatracker.ietf.org/doc/html/rfc6716#section-2>)).
///
/// ```text
/// +----------------------+-----------------+-------------------------+
/// | Abbreviation         | Audio Bandwidth | Sample Rate (Effective) |
/// +----------------------+-----------------+-------------------------+
/// | NB (narrowband)      |           4 kHz |                   8 kHz |
/// | MB (medium-band)     |           6 kHz |                  12 kHz |
/// | WB (wideband)        |           8 kHz |                  16 kHz |
/// | SWB (super-wideband) |          12 kHz |                  24 kHz |
/// | FB (fullband)        |      20 kHz (*) |                  48 kHz |
/// +----------------------+-----------------+-------------------------+
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum Bandwidth {
    /// NB: 4 kHz audio bandwidth.
    Narrowband = 8_000,
    /// MB: 6 kHz audio bandwidth.
    MediumBand = 12_000,
    /// WB: 8 kHz audio bandwidth.
    Wideband = 16_000,
    /// SWB: 12 kHz audio bandwidth.
    SuperWideband = 24_000,
    /// FB: 20 kHz audio bandwidth.
    Fullband = 48_000,
}

impl Bandwidth {
    /// Effective sample rate of the bandwidth in Hz
    /// ([RFC 6716 Section 2](<https://datatracker.ietf.org/doc/html/rfc6716#section-2>)).
    #[inline]
    pub(crate) const fn sample_rate(self) -> u32 {
        return self as u32;
    }
}

/// Duration of one frame, carried in nanoseconds
/// ([RFC 6716 Section 2.1.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-2.1.4>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub(crate) enum FrameDuration {
    /// 2.5 ms, CELT-only configurations.
    Ms2_5 = 2_500_000,
    /// 5 ms, CELT-only configurations.
    Ms5 = 5_000_000,
    /// 10 ms, every mode.
    Ms10 = 10_000_000,
    /// 20 ms, every mode.
    Ms20 = 20_000_000,
    /// 40 ms, SILK-only configurations.
    Ms40 = 40_000_000,
    /// 60 ms, SILK-only configurations.
    Ms60 = 60_000_000,
}

impl From<FrameDuration> for Duration {
    fn from(frame_duration: FrameDuration) -> Self {
        return Duration::from_nanos(frame_duration as u64);
    }
}

/// The frame count code `c`
/// ([RFC 6716 Section 3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>)).
///
/// ```text
/// +---+----------------------------------------------+
/// | c | Frames per packet                            |
/// +---+----------------------------------------------+
/// | 0 | 1 frame in the packet                        |
/// | 1 | 2 frames in the packet, equal compressed size|
/// | 2 | 2 frames in the packet, different sizes      |
/// | 3 | An arbitrary number of frames                |
/// +---+----------------------------------------------+
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum FrameCountCode {
    /// One frame (`c = 0`,
    /// [RFC 6716 Section 3.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.2>)).
    Single = 0,
    /// Two frames of equal compressed size (`c = 1`,
    /// [RFC 6716 Section 3.2.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.3>)).
    TwoEqual = 1,
    /// Two frames of different compressed sizes (`c = 2`,
    /// [RFC 6716 Section 3.2.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.4>)).
    TwoVariable = 2,
    /// A signaled number of frames (`c = 3`,
    /// [RFC 6716 Section 3.2.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.5>)).
    Signaled = 3,
}

impl From<u8> for FrameCountCode {
    fn from(code: u8) -> Self {
        return match code & FRAME_COUNT_CODE_MASK {
            0 => Self::Single,
            1 => Self::TwoEqual,
            2 => Self::TwoVariable,
            _ => Self::Signaled,
        };
    }
}

#[cfg(test)]
mod tests {
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
}
