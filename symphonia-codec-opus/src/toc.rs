//! The TOC Byte (Table of Contents Byte)
/// A well-formed Opus packet MUST contain at least one byte.  This
/// byte forms a table-of-contents (TOC) header that signals which of the
/// various modes and configurations a given packet uses.  It is composed
/// of a configuration number, "config", a stereo flag, "s", and a frame
/// count code, "c", arranged as illustrated in Figure 1.  A description
/// of each of these fields follows.
/// ```text
///                               0
///                               0 1 2 3 4 5 6 7
///                              +-+-+-+-+-+-+-+-+
///                              | config  |s| c |
///                              +-+-+-+-+-+-+-+-+
///
///                           Figure 1: The TOC Byte
/// ```
///
/// https://datatracker.ietf.org/doc/html/rfc6716#section-3.1
use std::convert::TryFrom;
use std::time::Duration;
use log::debug;
use thiserror::Error;
use symphonia_core::io::{BitReaderLtr, ReadBitsLtr};


#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid audio mode")]
    InvalidAudioMode,

    #[error("Invalid band width")]
    InvalidBandwidth,

    #[error("Invalid frame size")]
    InvalidFrameSize,

    #[error("Invalid frame count code")]
    InvalidFrameCountCode,
}

/// Represents the Table of Contents (TOC) byte of an Opus packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Toc {
    config: u8,
    stereo: bool,
    frame_count: FrameCount,
}


impl Toc {
    pub fn try_new(byte: u8) -> Result<Self, Error> {
        debug!("TOC byte: {:08b}", byte);

        let buf = [byte];
        let mut reader = BitReaderLtr::new(&buf);

        // 'config' field (bits 0-4).
        let config = reader.read_bits_leq32(5).map_err(Error::Io)? as u8;
        debug!("config: {config:#05b}" );

        // 's' (stereo) flag (bit 5).
        //  One additional bit, labeled "s", signals mono vs. stereo, with 0
        //  indicating mono and 1 indicating stereo.
        let stereo = reader.read_bool().map_err(Error::Io)?;
        debug!("stereo: {stereo}");

        // 'c' (frame count code) field (bits 6-7).
        let frame_count_code = reader.read_bits_leq32(2).map_err(Error::Io)? as u8;
        debug!("frame Count Code: {frame_count_code:#02b}" );

        let frame_count = FrameCount::try_from(frame_count_code)?;

        return Ok(Toc {
            config,
            stereo,
            frame_count,
        });
    }

    pub fn as_byte(&self) -> u8 {
        let mut byte = (self.config & 0x1F) << 3; // Shift 'config' into bits 7-3
        debug!("Byte after config: {:08b}", byte);

        if self.stereo {
            byte |= 1 << 2; // Set bit 2 for 'stereo'
        }
        debug!("Byte after stereo: {:08b}", byte);

        byte |= (self.frame_count as u8) & 0x03; // Set bits 1-0 for 'frame_count'
        debug!("Final reconstructed byte: {:08b}", byte);

        return byte;
    }

    pub fn params(&self) -> Result<Parameters, Error> {
        Parameters::try_new(self.config)
    }

    pub fn is_stereo(&self) -> bool {
        self.stereo
    }

    pub fn frame_count(&self) -> FrameCount {
        self.frame_count
    }
}

#[derive(Debug, PartialEq)]
pub struct Parameters {
    pub audio_mode: AudioMode,
    pub bandwidth: Bandwidth,
    pub frame_size: FrameSize,
}

impl Parameters {
    pub fn try_new(config: u8) -> Result<Self, Error> {
        let audio_mode = AudioMode::try_from(config)?;
        let bandwidth = Bandwidth::try_from(config)?;
        let frame_size = FrameSize::try_from(config)?;

        return Ok(Self {
            audio_mode,
            bandwidth,
            frame_size,
        });
    }
}

/// The `config` field specifies the operating mode, bandwidth, and frame size.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioMode {
    Silk,
    Hybrid,
    Celt,
}

impl TryFrom<u8> for AudioMode {
    type Error = Error;

    fn try_from(config: u8) -> Result<Self, Error> {
        match config {
            0..=11 => Ok(AudioMode::Silk),
            12..=15 => Ok(AudioMode::Hybrid),
            16..=31 => Ok(AudioMode::Celt),
            _ => Err(Error::InvalidAudioMode),
        }
    }
}

/// The Bandwidth the Opus codec scales from 6 kbit/s narrowband mono speech to
/// 510 kbit/s fullband stereo music, with algorithmic delays ranging
/// from 5 ms to 65.2 ms.  At any given time, either the LP layer, the
/// MDCT layer, or both, may be active.  It can seamlessly switch between
/// all of its various operating modes, giving it a great deal of
/// flexibility to adapt to varying content and network conditions
/// without renegotiating the current session.  The codec allows input
/// and output of various audio bandwidths, defined as follows:
/// ```text
/// +----------------------+-----------------+-------------------------+
/// | Abbreviation         | Audio Bandwidth | Sample Rate (Effective) |
/// +----------------------+-----------------+-------------------------+
/// | NB (narrowband)      |           4 kHz |                   8 kHz |
/// |                      |                 |                         |
/// | MB (medium-band)     |           6 kHz |                  12 kHz |
/// |                      |                 |                         |
/// | WB (wideband)        |           8 kHz |                  16 kHz |
/// |                      |                 |                         |
/// | SWB (super-wideband) |          12 kHz |                  24 kHz |
/// |                      |                 |                         |
/// | FB (fullband)        |      20 kHz (*) |                  48 kHz |
/// +----------------------+-----------------+-------------------------+
/// ```
/// https://datatracker.ietf.org/doc/html/rfc6716#section-2
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Bandwidth {
    NarrowBand,
    MediumBand,
    WideBand,
    SuperWideBand,
    #[default]
    FullBand,
}

/// Effective sample rate for a given bandwidth, Hz.
impl Bandwidth {
    pub fn sample_rate(&self) -> u32 {
        return match self {
            Bandwidth::NarrowBand => 8_000,
            Bandwidth::MediumBand => 12_000,
            Bandwidth::WideBand => 16_000,
            Bandwidth::SuperWideBand => 24_000,
            Bandwidth::FullBand => 48_000,
        };
    }
}

impl TryFrom<u8> for Bandwidth {
    type Error = Error;

    fn try_from(config: u8) -> Result<Self, Error> {
        return match config {
            0..=3 => Ok(Bandwidth::NarrowBand),
            4..=7 => Ok(Bandwidth::MediumBand),
            8..=11 => Ok(Bandwidth::WideBand),
            12..=13 => Ok(Bandwidth::SuperWideBand),
            14..=15 => Ok(Bandwidth::FullBand),
            16..=19 => Ok(Bandwidth::NarrowBand),
            20..=23 => Ok(Bandwidth::WideBand),
            24..=27 => Ok(Bandwidth::SuperWideBand),
            28..=31 => Ok(Bandwidth::FullBand),
            _ => Err(Error::InvalidBandwidth),
        };
    }
}

/// Enumeration of possible frame sizes in nanoseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u64)]
pub enum FrameSize {
    Ms2_5 = 2_500_000,
    Ms5 = 5_000_000,
    Ms10 = 10_000_000,
    #[default]
    Ms20 = 20_000_000,
    Ms40 = 40_000_000,
    Ms60 = 60_000_000,
}

impl From<FrameSize> for Duration {
    fn from(frame_size: FrameSize) -> Self {
        return Duration::from_nanos(frame_size as u64);
    }
}

/// The `config` field specifies the operating mode, bandwidth, and frame size.
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
///  The 32 possible configurations each identify which one of these
///  operating modes the packet uses, as well as the audio bandwidth and
///  the frame size.  Table  lists the parameters for each configuration.
///  The configuration numbers in each range (e.g., 0...3 for NB SILK-
///  only) correspond to the various choices of frame size, in the same
///  order.  For example, configuration 0 has a 10 ms frame size and
///  configuration 3 has a 60 ms frame size.
impl FrameSize {
    pub fn duration(&self) -> Duration {
        return Duration::from(*self);
    }
}

impl TryFrom<u8> for FrameSize {
    type Error = Error;

    fn try_from(config: u8) -> Result<Self, Error> {
        return match config {
            // SILK modes (configs 0..11)
            0 | 4 | 8 => Ok(FrameSize::Ms10),
            1 | 5 | 9 => Ok(FrameSize::Ms20),
            2 | 6 | 10 => Ok(FrameSize::Ms40),
            3 | 7 | 11 => Ok(FrameSize::Ms60),
            // Hybrid modes (configs 12..15)
            12 | 14 => Ok(FrameSize::Ms10),
            13 | 15 => Ok(FrameSize::Ms20),
            // CELT modes (configs 16..31)
            16 | 20 | 24 | 28 => Ok(FrameSize::Ms2_5),
            17 | 21 | 25 | 29 => Ok(FrameSize::Ms5),
            18 | 22 | 26 | 30 => Ok(FrameSize::Ms10),
            19 | 23 | 27 | 31 => Ok(FrameSize::Ms20),
            _ => Err(Error::InvalidFrameSize),
        };
    }
}


///   The remaining two bits of the TOC byte, labeled "c", code the number
///   of frames per packet (codes 0 to 3) as follows:
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
///
/// These values correspond to the `c` field in the TOC byte.
/// 
/// https://datatracker.ietf.org/doc/html/rfc6716#section-3.1
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameCount {
    /// 1 frame in the packet (c = 0)
    One,
    
    /// 2 frames, both with equal compressed size (c = 1)
    TwoEqual,
    
    /// 2 frames, with different compressed sizes (c = 2)
    TwoDifferent,
    
    /// Arbitrary number of frames (c = 3)
    Arbitrary,
}
impl TryFrom<u8> for FrameCount {
    type Error = Error;

    fn try_from(code: u8) -> Result<Self, Error> {
        return match code {
            0 => Ok(FrameCount::One),
            1 => Ok(FrameCount::TwoEqual),
            2 => Ok(FrameCount::TwoDifferent),
            3 => Ok(FrameCount::Arbitrary),
            _ => Err(Error::InvalidFrameCountCode),
        };
    }
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;
    use super::*;

    static _LOGGER: LazyLock<(), fn()> = LazyLock::new(init_logger);
    fn init_logger() {
        env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Debug)
            .try_init()
            .unwrap();
    }

    #[derive(Debug)]
    struct TestCase {
        toc_byte: u8,
        is_stereo: bool,
        frame_count: FrameCount,
        audio_mode: AudioMode,
        bandwidth: Bandwidth,
        frame_size: FrameSize,
    }

    impl TestCase {
        fn check(&self) {
            let toc = Toc::try_new(self.toc_byte).expect("Failed to create Toc from byte");

            assert_eq!(
                toc.is_stereo(),
                self.is_stereo,
                "TOC byte: {:#010b}, expected stereo: {}, got: {}",
                self.toc_byte,
                self.is_stereo,
                toc.is_stereo()
            );

            assert_eq!(
                toc.frame_count(),
                self.frame_count,
                "TOC byte: {:#010b}, expected frame count: {:?}, got: {:?}",
                self.toc_byte,
                self.frame_count,
                toc.frame_count()
            );

            let params = toc.params().expect("Failed to get parameters from TOC");

            assert_eq!(
                params.audio_mode,
                self.audio_mode,
                "TOC byte: {:#010b}, expected audio mode: {:?}, got: {:?}",
                self.toc_byte,
                self.audio_mode,
                params.audio_mode
            );

            assert_eq!(
                params.bandwidth,
                self.bandwidth,
                "TOC byte: {:#010b}, expected bandwidth: {:?}, got: {:?}",
                self.toc_byte,
                self.bandwidth,
                params.bandwidth
            );

            assert_eq!(
                params.frame_size,
                self.frame_size,
                "TOC byte: {:#010b}, expected frame size: {:?}, got: {:?}",
                self.toc_byte,
                self.frame_size,
                params.frame_size
            );
        }
    }

    fn new_toc_byte(
        is_stereo: bool,
        frame_count: FrameCount,
        audio_mode: AudioMode,
        bandwidth: Bandwidth,
        frame_size: FrameSize,
    ) -> u8 {
        let config_number = match (audio_mode, bandwidth, frame_size) {
            (AudioMode::Silk, Bandwidth::NarrowBand, FrameSize::Ms10) => 0,
            (AudioMode::Silk, Bandwidth::NarrowBand, FrameSize::Ms20) => 1,
            (AudioMode::Silk, Bandwidth::NarrowBand, FrameSize::Ms40) => 2,
            (AudioMode::Silk, Bandwidth::NarrowBand, FrameSize::Ms60) => 3,
            (AudioMode::Silk, Bandwidth::MediumBand, FrameSize::Ms10) => 4,
            (AudioMode::Silk, Bandwidth::MediumBand, FrameSize::Ms20) => 5,
            (AudioMode::Silk, Bandwidth::MediumBand, FrameSize::Ms40) => 6,
            (AudioMode::Silk, Bandwidth::MediumBand, FrameSize::Ms60) => 7,
            (AudioMode::Silk, Bandwidth::WideBand, FrameSize::Ms10) => 8,
            (AudioMode::Silk, Bandwidth::WideBand, FrameSize::Ms20) => 9,
            (AudioMode::Silk, Bandwidth::WideBand, FrameSize::Ms40) => 10,
            (AudioMode::Silk, Bandwidth::WideBand, FrameSize::Ms60) => 11,
            (AudioMode::Hybrid, Bandwidth::SuperWideBand, FrameSize::Ms10) => 12,
            (AudioMode::Hybrid, Bandwidth::SuperWideBand, FrameSize::Ms20) => 13,
            (AudioMode::Hybrid, Bandwidth::FullBand, FrameSize::Ms10) => 14,
            (AudioMode::Hybrid, Bandwidth::FullBand, FrameSize::Ms20) => 15,
            (AudioMode::Celt, Bandwidth::NarrowBand, FrameSize::Ms2_5) => 16,
            (AudioMode::Celt, Bandwidth::NarrowBand, FrameSize::Ms5) => 17,
            (AudioMode::Celt, Bandwidth::NarrowBand, FrameSize::Ms10) => 18,
            (AudioMode::Celt, Bandwidth::NarrowBand, FrameSize::Ms20) => 19,
            (AudioMode::Celt, Bandwidth::WideBand, FrameSize::Ms2_5) => 20,
            (AudioMode::Celt, Bandwidth::WideBand, FrameSize::Ms5) => 21,
            (AudioMode::Celt, Bandwidth::WideBand, FrameSize::Ms10) => 22,
            (AudioMode::Celt, Bandwidth::WideBand, FrameSize::Ms20) => 23,
            (AudioMode::Celt, Bandwidth::SuperWideBand, FrameSize::Ms2_5) => 24,
            (AudioMode::Celt, Bandwidth::SuperWideBand, FrameSize::Ms5) => 25,
            (AudioMode::Celt, Bandwidth::SuperWideBand, FrameSize::Ms10) => 26,
            (AudioMode::Celt, Bandwidth::SuperWideBand, FrameSize::Ms20) => 27,
            (AudioMode::Celt, Bandwidth::FullBand, FrameSize::Ms2_5) => 28,
            (AudioMode::Celt, Bandwidth::FullBand, FrameSize::Ms5) => 29,
            (AudioMode::Celt, Bandwidth::FullBand, FrameSize::Ms10) => 30,
            (AudioMode::Celt, Bandwidth::FullBand, FrameSize::Ms20) => 31,
            _ => panic!("Invalid audio mode or bandwidth or frame size"),
        };

        let stereo_flag = if is_stereo { 1 << 6 } else { 0 };

        let frame_count_code = match frame_count {
            FrameCount::One => 0,
            FrameCount::TwoEqual => 1 << 6,
            FrameCount::TwoDifferent => 1 << 7,
            FrameCount::Arbitrary => 3 << 6,
        };

        return (config_number << 3) | stereo_flag | frame_count_code;
    }
    fn populate_test_table() -> Vec<TestCase> {
        vec![
            TestCase { toc_byte: 0b00000000, is_stereo: false, frame_count: FrameCount::One, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::NarrowBand, frame_size: FrameSize::Ms10 },
            TestCase { toc_byte: 0b00000001, is_stereo: false, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::NarrowBand, frame_size: FrameSize::Ms10 },
            TestCase { toc_byte: 0b00000100, is_stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::NarrowBand, frame_size: FrameSize::Ms10 },
            TestCase { toc_byte: 0b00000101, is_stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::NarrowBand, frame_size: FrameSize::Ms10 },
            TestCase { toc_byte: 0b00010000, is_stereo: false, frame_count: FrameCount::One, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::NarrowBand, frame_size: FrameSize::Ms40 },
            TestCase { toc_byte: 0b00010001, is_stereo: false, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::NarrowBand, frame_size: FrameSize::Ms40 },
            TestCase { toc_byte: 0b00010100, is_stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::NarrowBand, frame_size: FrameSize::Ms40 },
            TestCase { toc_byte: 0b00010101, is_stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::NarrowBand, frame_size: FrameSize::Ms40 },
            TestCase { toc_byte: 0b00011100, is_stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::NarrowBand, frame_size: FrameSize::Ms60 },
            TestCase { toc_byte: 0b00011101, is_stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::NarrowBand, frame_size: FrameSize::Ms60 },
            TestCase { toc_byte: 0b00100000, is_stereo: false, frame_count: FrameCount::One, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::MediumBand, frame_size: FrameSize::Ms10 },
            TestCase { toc_byte: 0b00100001, is_stereo: false, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::MediumBand, frame_size: FrameSize::Ms10 },
            TestCase { toc_byte: 0b00100100, is_stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::MediumBand, frame_size: FrameSize::Ms10 },
            TestCase { toc_byte: 0b00100101, is_stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::MediumBand, frame_size: FrameSize::Ms10 },
            TestCase { toc_byte: 0b00101100, is_stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::MediumBand, frame_size: FrameSize::Ms20 },
            TestCase { toc_byte: 0b00101101, is_stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::MediumBand, frame_size: FrameSize::Ms20 },
            TestCase { toc_byte: 0b00110000, is_stereo: false, frame_count: FrameCount::One, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::MediumBand, frame_size: FrameSize::Ms40 },
            TestCase { toc_byte: 0b00110001, is_stereo: false, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::MediumBand, frame_size: FrameSize::Ms40 },
            TestCase { toc_byte: 0b00110100, is_stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::MediumBand, frame_size: FrameSize::Ms40 },
            TestCase { toc_byte: 0b00110101, is_stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::MediumBand, frame_size: FrameSize::Ms40 },
            TestCase { toc_byte: 0b00111100, is_stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::MediumBand, frame_size: FrameSize::Ms60 },
            TestCase { toc_byte: 0b00111101, is_stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::MediumBand, frame_size: FrameSize::Ms60 },
            TestCase { toc_byte: 0b01001000, is_stereo: false, frame_count: FrameCount::One, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::WideBand, frame_size: FrameSize::Ms20 },
            TestCase { toc_byte: 0b01001001, is_stereo: false, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::WideBand, frame_size: FrameSize::Ms20 },
            TestCase { toc_byte: 0b01001100, is_stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::WideBand, frame_size: FrameSize::Ms20 },
            TestCase { toc_byte: 0b01001101, is_stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Silk, bandwidth: Bandwidth::WideBand, frame_size: FrameSize::Ms20 },
            TestCase { toc_byte: 0b01100000, is_stereo: false, frame_count: FrameCount::One, audio_mode: AudioMode::Hybrid, bandwidth: Bandwidth::SuperWideBand, frame_size: FrameSize::Ms10 },
            TestCase { toc_byte: 0b01100001, is_stereo: false, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Hybrid, bandwidth: Bandwidth::SuperWideBand, frame_size: FrameSize::Ms10 },
            TestCase { toc_byte: 0b01100100, is_stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::Hybrid, bandwidth: Bandwidth::SuperWideBand, frame_size: FrameSize::Ms10 },
            TestCase { toc_byte: 0b01100101, is_stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Hybrid, bandwidth: Bandwidth::SuperWideBand, frame_size: FrameSize::Ms10 },
            TestCase { toc_byte: 0b10000000, is_stereo: false, frame_count: FrameCount::One, audio_mode: AudioMode::Celt, bandwidth: Bandwidth::NarrowBand, frame_size: FrameSize::Ms2_5 },
            TestCase { toc_byte: 0b10000001, is_stereo: false, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Celt, bandwidth: Bandwidth::NarrowBand, frame_size: FrameSize::Ms2_5 },
            TestCase { toc_byte: 0b10000100, is_stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::Celt, bandwidth: Bandwidth::NarrowBand, frame_size: FrameSize::Ms2_5 },
            TestCase { toc_byte: 0b10000101, is_stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Celt, bandwidth: Bandwidth::NarrowBand, frame_size: FrameSize::Ms2_5 },
            TestCase { toc_byte: 0b10001000, is_stereo: false, frame_count: FrameCount::One, audio_mode: AudioMode::Celt, bandwidth: Bandwidth::NarrowBand, frame_size: FrameSize::Ms5 },
            TestCase { toc_byte: 0b10001001, is_stereo: false, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Celt, bandwidth: Bandwidth::NarrowBand, frame_size: FrameSize::Ms5 },
        ]
    }

    #[test]
    fn run_all_cases() {
        let _ = populate_test_table()
            .iter()
            .map(|t| {
                t.check();
            });
    }

    #[test]
    fn invalid_toc_byte() {
        Toc::try_new(42).unwrap();
    }

    #[test]
    fn invalid_config_value() {
        Parameters::try_new(0b1).unwrap();
    }

    #[test]
    fn frame_size_to_duration() {
        assert_eq!(Duration::from(FrameSize::Ms2_5), Duration::from_nanos(2_500_000));
        assert_eq!(Duration::from(FrameSize::Ms5), Duration::from_nanos(5_000_000));
        assert_eq!(Duration::from(FrameSize::Ms10), Duration::from_nanos(10_000_000));
        assert_eq!(Duration::from(FrameSize::Ms20), Duration::from_nanos(20_000_000));
        assert_eq!(Duration::from(FrameSize::Ms40), Duration::from_nanos(40_000_000));
        assert_eq!(Duration::from(FrameSize::Ms60), Duration::from_nanos(60_000_000));
    }

    macro_rules! test_mapping {
        ($field:ident, $object:ident,  $inputs:expr) => {
            #[test]
            fn $field() {
                for (input, expected) in $inputs.iter() {
                    let instance = $object::try_new(*input).unwrap();
                    assert_eq!(
                        instance.$field,
                        *expected,
                        "{} {:?} should be {:?} for {}",
                        stringify!($object), input, expected, stringify!($field)
                    );
                }
            }
        }
    }

    test_mapping!(frame_count, Toc,
            [
              (0b00000000, FrameCount::One),
              (0b00000001, FrameCount::TwoEqual),
              (0b00000010, FrameCount::TwoDifferent),
              (0b00000011, FrameCount::Arbitrary),
            ]);

    test_mapping!(frame_size, Parameters,
        [
            (0, FrameSize::Ms10), (1, FrameSize::Ms20), (2, FrameSize::Ms40), (3, FrameSize::Ms60),
            (4, FrameSize::Ms10), (5, FrameSize::Ms20), (6, FrameSize::Ms40), (7, FrameSize::Ms60),
            (8, FrameSize::Ms10), (9, FrameSize::Ms20), (10, FrameSize::Ms40), (11, FrameSize::Ms60),
            (12, FrameSize::Ms10), (13, FrameSize::Ms20), (14, FrameSize::Ms10), (15, FrameSize::Ms20),
            (16, FrameSize::Ms2_5), (17, FrameSize::Ms5), (18, FrameSize::Ms10), (19, FrameSize::Ms20),
            (20, FrameSize::Ms2_5), (21, FrameSize::Ms5), (22, FrameSize::Ms10), (23, FrameSize::Ms20),
            (24, FrameSize::Ms2_5), (25, FrameSize::Ms5), (26, FrameSize::Ms10), (27, FrameSize::Ms20),
            (28, FrameSize::Ms2_5), (29, FrameSize::Ms5), (30, FrameSize::Ms10), (31, FrameSize::Ms20),
        ]);


    test_mapping!(audio_mode, Parameters,
        [
            (0, AudioMode::Silk),
            (11, AudioMode::Silk),
            (12, AudioMode::Hybrid),
            (15, AudioMode::Hybrid),
            (16, AudioMode::Celt),
            (31, AudioMode::Celt),
        ]);

    test_mapping!(bandwidth, Parameters,
       [
            (0, Bandwidth::NarrowBand),
            (4, Bandwidth::MediumBand),
            (8, Bandwidth::WideBand),
            (12, Bandwidth::SuperWideBand),
            (14, Bandwidth::FullBand),
            (16, Bandwidth::NarrowBand),
            (20, Bandwidth::WideBand),
            (24, Bandwidth::SuperWideBand),
            (28, Bandwidth::FullBand),
        ]);

    #[test]
    fn as_byte() {
        for t in populate_test_table() {
            let toc = Toc::try_new(t.toc_byte).expect("Failed to create Toc from byte");
            let as_byte = toc.as_byte();

            assert_eq!(
                as_byte,
                t.toc_byte,
                "Failed to test as_byte result. Original: {:#010b}, as_byte(): {:#010b}",
                t.toc_byte,
                as_byte
            );

            let reconstructed = new_toc_byte(t.is_stereo, t.frame_count, t.audio_mode, t.bandwidth, t.frame_size);
            assert_eq!(
                as_byte,
                t.toc_byte,
                "Failed to reconstruct TOC byte. Original: {:#010b}, Reconstructed: {:#010b}",
                t.toc_byte,
                reconstructed
            )
        }
    }
}

