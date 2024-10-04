//! The TOC Byte (Table of Contents Byte)
use log::debug;
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
use symphonia_core::io::{BitReaderLtr, ReadBitsLtr};
use thiserror::Error;


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
    pub stereo: bool,
    pub frame_count: FrameCount,
    pub audio_mode: AudioMode,
    pub bandwidth: Bandwidth,
    pub frame_size: FrameDuration,
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

        let audio_mode = AudioMode::try_from(config)?;
        
        let bandwidth = Bandwidth::try_from(config)?;
        
        let frame_size = FrameDuration::try_from(config)?;
        
        return Ok(Toc {
            config,
            stereo,
            frame_count,
            audio_mode,
            bandwidth,
            frame_size,
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
    SILK,
    CELT,
    Hybrid,
}

impl TryFrom<u8> for AudioMode {
    type Error = Error;

    fn try_from(config: u8) -> Result<Self, Error> {
        match config {
            0..=11 => Ok(AudioMode::SILK),
            12..=15 => Ok(AudioMode::Hybrid),
            16..=31 => Ok(AudioMode::CELT),
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
///
///  Just like for the number of channels, any decoder can decode audio that is
///  encoded at any bandwidth.  For example, any Opus decoder operating at
///  8 kHz can decode an FB Opus frame, and any Opus decoder operating at
///  48 kHz can decode an NB frame.  Similarly, the reference encoder can
///  take a 48 kHz input signal and encode it as NB.  The higher the audio
///  bandwidth, the higher the required bitrate to achieve acceptable
///  quality.  The audio bandwidth can be explicitly specified in
///  real-time, but, by default, the reference encoder attempts to make the
///  best bandwidth decision possible given the current bitrate.
/// https://datatracker.ietf.org/doc/html/rfc6716#section-2.1.3
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Bandwidth {
    NarrowBand = 8_000,
    MediumBand = 12_000,
    WideBand = 16_000,
    SuperWideBand = 24_000,
    #[default]
    FullBand = 48_000,
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

/// Opus can encode frames of 2.5, 5, 10, 20, 40, or 60 ms.
/// It can also combine multiple frames into packets of up to 120 ms.
/// For real-time applications, sending fewer packets per second reduces
/// the bitrate, since it reduces the overhead from IP, UDP, and RTP headers.
/// However, it increases latency and sensitivity to packet losses,
/// as losing one packet constitutes a loss of a bigger chunk of audio.
/// Increasing the frame duration also slightly improves coding
/// efficiency, but the gain becomes small for frame sizes above 20 ms.
/// For this reason, 20 ms frames are a good choice for most applications.
/// https://datatracker.ietf.org/doc/html/rfc6716#section-2.1.4
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(u64)]
pub enum FrameDuration {
    Ms2_5 = 2_500_000,
    Ms5 = 5_000_000,
    Ms10 = 10_000_000,
    #[default]
    Ms20 = 20_000_000,
    Ms40 = 40_000_000,
    Ms60 = 60_000_000,
}

impl From<FrameDuration> for Duration {
    fn from(frame_size: FrameDuration) -> Self {
        return Duration::from_nanos(frame_size as u64);
    }
}

impl FrameDuration { // at 48kHz
    pub fn sample_count(&self) -> usize {
        return match self {
            FrameDuration::Ms2_5 => 120,
            FrameDuration::Ms5 => 240,
            FrameDuration::Ms10 => 480,
            FrameDuration::Ms20 => 960,
            FrameDuration::Ms40 => 1_920,
            FrameDuration::Ms60 => 2_880,
        };
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
impl FrameDuration {
    pub fn duration(&self) -> Duration {
        return Duration::from(*self);
    }
}

impl TryFrom<u8> for FrameDuration {
    type Error = Error;

    fn try_from(config: u8) -> Result<Self, Error> {
        return match config {
            // SILK modes (configs 0..11)
            0 | 4 | 8 => Ok(FrameDuration::Ms10),
            1 | 5 | 9 => Ok(FrameDuration::Ms20),
            2 | 6 | 10 => Ok(FrameDuration::Ms40),
            3 | 7 | 11 => Ok(FrameDuration::Ms60),
            // Hybrid modes (configs 12..15)
            12 | 14 => Ok(FrameDuration::Ms10),
            13 | 15 => Ok(FrameDuration::Ms20),
            // CELT modes (configs 16..31)
            16 | 20 | 24 | 28 => Ok(FrameDuration::Ms2_5),
            17 | 21 | 25 | 29 => Ok(FrameDuration::Ms5),
            18 | 22 | 26 | 30 => Ok(FrameDuration::Ms10),
            19 | 23 | 27 | 31 => Ok(FrameDuration::Ms20),
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
    use super::*;
    use std::sync::LazyLock;

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
        stereo: bool,
        frame_count: FrameCount,
        audio_mode: AudioMode,
        bandwidth: Bandwidth,
        frame_size: FrameDuration,
    }

    impl TestCase {
        fn check(&self) {
            let toc = Toc::try_new(self.toc_byte).expect("Failed to create Toc from byte");

            assert_eq!(
                toc.stereo,
                self.stereo,
                "TOC byte: {:#010b}, expected stereo: {}, got: {}",
                self.toc_byte,
                self.stereo,
                toc.stereo,
            );

            assert_eq!(
                toc.frame_count,
                self.frame_count,
                "TOC byte: {:#010b}, expected frame count: {:?}, got: {:?}",
                self.toc_byte,
                self.frame_count,
                toc.frame_count
            );

            assert_eq!(
                toc.audio_mode,
                self.audio_mode,
                "TOC byte: {:#010b}, expected audio mode: {:?}, got: {:?}",
                self.toc_byte,
                self.audio_mode,
                toc.audio_mode
            );

            assert_eq!(
                toc.bandwidth,
                self.bandwidth,
                "TOC byte: {:#010b}, expected bandwidth: {:?}, got: {:?}",
                self.toc_byte,
                self.bandwidth,
                toc.bandwidth
            );

            assert_eq!(
                toc.frame_size,
                self.frame_size,
                "TOC byte: {:#010b}, expected frame size: {:?}, got: {:?}",
                self.toc_byte,
                self.frame_size,
                toc.frame_size
            );
        }
    }

    fn new_toc_byte(
        is_stereo: bool,
        frame_count: FrameCount,
        audio_mode: AudioMode,
        bandwidth: Bandwidth,
        frame_size: FrameDuration,
    ) -> u8 {
        let config_number = match (audio_mode, bandwidth, frame_size) {
            (AudioMode::SILK, Bandwidth::NarrowBand, FrameDuration::Ms10) => 0,
            (AudioMode::SILK, Bandwidth::NarrowBand, FrameDuration::Ms20) => 1,
            (AudioMode::SILK, Bandwidth::NarrowBand, FrameDuration::Ms40) => 2,
            (AudioMode::SILK, Bandwidth::NarrowBand, FrameDuration::Ms60) => 3,
            (AudioMode::SILK, Bandwidth::MediumBand, FrameDuration::Ms10) => 4,
            (AudioMode::SILK, Bandwidth::MediumBand, FrameDuration::Ms20) => 5,
            (AudioMode::SILK, Bandwidth::MediumBand, FrameDuration::Ms40) => 6,
            (AudioMode::SILK, Bandwidth::MediumBand, FrameDuration::Ms60) => 7,
            (AudioMode::SILK, Bandwidth::WideBand, FrameDuration::Ms10) => 8,
            (AudioMode::SILK, Bandwidth::WideBand, FrameDuration::Ms20) => 9,
            (AudioMode::SILK, Bandwidth::WideBand, FrameDuration::Ms40) => 10,
            (AudioMode::SILK, Bandwidth::WideBand, FrameDuration::Ms60) => 11,
            (AudioMode::Hybrid, Bandwidth::SuperWideBand, FrameDuration::Ms10) => 12,
            (AudioMode::Hybrid, Bandwidth::SuperWideBand, FrameDuration::Ms20) => 13,
            (AudioMode::Hybrid, Bandwidth::FullBand, FrameDuration::Ms10) => 14,
            (AudioMode::Hybrid, Bandwidth::FullBand, FrameDuration::Ms20) => 15,
            (AudioMode::CELT, Bandwidth::NarrowBand, FrameDuration::Ms2_5) => 16,
            (AudioMode::CELT, Bandwidth::NarrowBand, FrameDuration::Ms5) => 17,
            (AudioMode::CELT, Bandwidth::NarrowBand, FrameDuration::Ms10) => 18,
            (AudioMode::CELT, Bandwidth::NarrowBand, FrameDuration::Ms20) => 19,
            (AudioMode::CELT, Bandwidth::WideBand, FrameDuration::Ms2_5) => 20,
            (AudioMode::CELT, Bandwidth::WideBand, FrameDuration::Ms5) => 21,
            (AudioMode::CELT, Bandwidth::WideBand, FrameDuration::Ms10) => 22,
            (AudioMode::CELT, Bandwidth::WideBand, FrameDuration::Ms20) => 23,
            (AudioMode::CELT, Bandwidth::SuperWideBand, FrameDuration::Ms2_5) => 24,
            (AudioMode::CELT, Bandwidth::SuperWideBand, FrameDuration::Ms5) => 25,
            (AudioMode::CELT, Bandwidth::SuperWideBand, FrameDuration::Ms10) => 26,
            (AudioMode::CELT, Bandwidth::SuperWideBand, FrameDuration::Ms20) => 27,
            (AudioMode::CELT, Bandwidth::FullBand, FrameDuration::Ms2_5) => 28,
            (AudioMode::CELT, Bandwidth::FullBand, FrameDuration::Ms5) => 29,
            (AudioMode::CELT, Bandwidth::FullBand, FrameDuration::Ms10) => 30,
            (AudioMode::CELT, Bandwidth::FullBand, FrameDuration::Ms20) => 31,
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
            TestCase { toc_byte: 0b00000000, stereo: false, frame_count: FrameCount::One, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::NarrowBand, frame_size: FrameDuration::Ms10 },
            TestCase { toc_byte: 0b00000001, stereo: false, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::NarrowBand, frame_size: FrameDuration::Ms10 },
            TestCase { toc_byte: 0b00000100, stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::NarrowBand, frame_size: FrameDuration::Ms10 },
            TestCase { toc_byte: 0b00000101, stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::NarrowBand, frame_size: FrameDuration::Ms10 },
            TestCase { toc_byte: 0b00010000, stereo: false, frame_count: FrameCount::One, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::NarrowBand, frame_size: FrameDuration::Ms40 },
            TestCase { toc_byte: 0b00010001, stereo: false, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::NarrowBand, frame_size: FrameDuration::Ms40 },
            TestCase { toc_byte: 0b00010100, stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::NarrowBand, frame_size: FrameDuration::Ms40 },
            TestCase { toc_byte: 0b00010101, stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::NarrowBand, frame_size: FrameDuration::Ms40 },
            TestCase { toc_byte: 0b00011100, stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::NarrowBand, frame_size: FrameDuration::Ms60 },
            TestCase { toc_byte: 0b00011101, stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::NarrowBand, frame_size: FrameDuration::Ms60 },
            TestCase { toc_byte: 0b00100000, stereo: false, frame_count: FrameCount::One, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::MediumBand, frame_size: FrameDuration::Ms10 },
            TestCase { toc_byte: 0b00100001, stereo: false, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::MediumBand, frame_size: FrameDuration::Ms10 },
            TestCase { toc_byte: 0b00100100, stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::MediumBand, frame_size: FrameDuration::Ms10 },
            TestCase { toc_byte: 0b00100101, stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::MediumBand, frame_size: FrameDuration::Ms10 },
            TestCase { toc_byte: 0b00101100, stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::MediumBand, frame_size: FrameDuration::Ms20 },
            TestCase { toc_byte: 0b00101101, stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::MediumBand, frame_size: FrameDuration::Ms20 },
            TestCase { toc_byte: 0b00110000, stereo: false, frame_count: FrameCount::One, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::MediumBand, frame_size: FrameDuration::Ms40 },
            TestCase { toc_byte: 0b00110001, stereo: false, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::MediumBand, frame_size: FrameDuration::Ms40 },
            TestCase { toc_byte: 0b00110100, stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::MediumBand, frame_size: FrameDuration::Ms40 },
            TestCase { toc_byte: 0b00110101, stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::MediumBand, frame_size: FrameDuration::Ms40 },
            TestCase { toc_byte: 0b00111100, stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::MediumBand, frame_size: FrameDuration::Ms60 },
            TestCase { toc_byte: 0b00111101, stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::MediumBand, frame_size: FrameDuration::Ms60 },
            TestCase { toc_byte: 0b01001000, stereo: false, frame_count: FrameCount::One, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::WideBand, frame_size: FrameDuration::Ms20 },
            TestCase { toc_byte: 0b01001001, stereo: false, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::WideBand, frame_size: FrameDuration::Ms20 },
            TestCase { toc_byte: 0b01001100, stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::WideBand, frame_size: FrameDuration::Ms20 },
            TestCase { toc_byte: 0b01001101, stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::SILK, bandwidth: Bandwidth::WideBand, frame_size: FrameDuration::Ms20 },
            TestCase { toc_byte: 0b01100000, stereo: false, frame_count: FrameCount::One, audio_mode: AudioMode::Hybrid, bandwidth: Bandwidth::SuperWideBand, frame_size: FrameDuration::Ms10 },
            TestCase { toc_byte: 0b01100001, stereo: false, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Hybrid, bandwidth: Bandwidth::SuperWideBand, frame_size: FrameDuration::Ms10 },
            TestCase { toc_byte: 0b01100100, stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::Hybrid, bandwidth: Bandwidth::SuperWideBand, frame_size: FrameDuration::Ms10 },
            TestCase { toc_byte: 0b01100101, stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::Hybrid, bandwidth: Bandwidth::SuperWideBand, frame_size: FrameDuration::Ms10 },
            TestCase { toc_byte: 0b10000000, stereo: false, frame_count: FrameCount::One, audio_mode: AudioMode::CELT, bandwidth: Bandwidth::NarrowBand, frame_size: FrameDuration::Ms2_5 },
            TestCase { toc_byte: 0b10000001, stereo: false, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::CELT, bandwidth: Bandwidth::NarrowBand, frame_size: FrameDuration::Ms2_5 },
            TestCase { toc_byte: 0b10000100, stereo: true, frame_count: FrameCount::One, audio_mode: AudioMode::CELT, bandwidth: Bandwidth::NarrowBand, frame_size: FrameDuration::Ms2_5 },
            TestCase { toc_byte: 0b10000101, stereo: true, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::CELT, bandwidth: Bandwidth::NarrowBand, frame_size: FrameDuration::Ms2_5 },
            TestCase { toc_byte: 0b10001000, stereo: false, frame_count: FrameCount::One, audio_mode: AudioMode::CELT, bandwidth: Bandwidth::NarrowBand, frame_size: FrameDuration::Ms5 },
            TestCase { toc_byte: 0b10001001, stereo: false, frame_count: FrameCount::TwoEqual, audio_mode: AudioMode::CELT, bandwidth: Bandwidth::NarrowBand, frame_size: FrameDuration::Ms5 },
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

            let reconstructed = new_toc_byte(t.stereo, t.frame_count, t.audio_mode, t.bandwidth, t.frame_size);
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

