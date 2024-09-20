//! The TOC Byte (Table of Contents Byte)
/// A well-formed Opus packet MUST contain at least one byte [R1]().  This
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
///  The top five bits of the TOC byte, labeled "config", encode one of 32
///  possible configurations of operating mode, audio bandwidth, and frame
///  size.  As described, the LP (SILK) layer and MDCT (CELT) layer can be
///  combined in three possible operating modes:
///
///  1.  A SILK-only mode for use in low bitrate connections with an audio
///      bandwidth of WB or less,
///
///  2.  A Hybrid (SILK+CELT) mode for SWB or FB speech at medium
///      bitrates, and
///
///  3.  A CELT-only mode for very low delay speech transmission as well
///      as music transmission (NB to FB).
///
///  The 32 possible configurations each identify which one of these
///  operating modes the packet uses, as well as the audio bandwidth and
///  the frame size.  Table 2 lists the parameters for each configuration.
///
///```text
///   +-----------------------+-----------+-----------+-------------------+
///   | Configuration         | Mode      | Bandwidth | Frame Sizes       |
///   | Number(s)             |           |           |                   |
///   +-----------------------+-----------+-----------+-------------------+
///   | 0...3                 | SILK-only | NB        | 10, 20, 40, 60 ms |
///   |                       |           |           |                   |
///   | 4...7                 | SILK-only | MB        | 10, 20, 40, 60 ms |
///   |                       |           |           |                   |
///   | 8...11                | SILK-only | WB        | 10, 20, 40, 60 ms |
///   |                       |           |           |                   |
///   | 12...13               | Hybrid    | SWB       | 10, 20 ms         |
///   |                       |           |           |                   |
///   | 14...15               | Hybrid    | FB        | 10, 20 ms         |
///   |                       |           |           |                   |
///   | 16...19               | CELT-only | NB        | 2.5, 5, 10, 20 ms |
///   |                       |           |           |                   |
///   | 20...23               | CELT-only | WB        | 2.5, 5, 10, 20 ms |
///   |                       |           |           |                   |
///   | 24...27               | CELT-only | SWB       | 2.5, 5, 10, 20 ms |
///   |                       |           |           |                   |
///   | 28...31               | CELT-only | FB        | 2.5, 5, 10, 20 ms |
///   +-----------------------+-----------+-----------+-------------------+
///
///                Table 2: TOC Byte Configuration Parameters
///```
///
///  The configuration numbers in each range (e.g., 0...3 for NB SILK-
///   only) correspond to the various choices of frame size, in the same
///   order.  For example, configuration 0 has a 10 ms frame size and
///   configuration 3 has a 60 ms frame size.
///
///   One additional bit, labeled "s", signals mono vs. stereo, with 0
///   indicating mono and 1 indicating stereo.
///
///   The remaining two bits of the TOC byte, labeled "c", code the number
///   of frames per packet (codes 0 to 3) as follows:
///
///   *  0: 1 frame in the packet
///
///   *  1: 2 frames in the packet, each with equal compressed size
///
///   *  2: 2 frames in the packet, with different compressed sizes
///
///   *  3: an arbitrary number of frames in the packet
///
///   This document refers to a packet as a code 0 packet, code 1 packet,
///   etc., based on the value of "c".
///
/// https://datatracker.ietf.org/doc/html/rfc6716#section-3.1

use std::convert::TryFrom;
use std::time::Duration;
use symphonia_core::errors::{Error, Result};
use symphonia_core::io::{BitReaderLtr, ReadBitsLtr};

/// Represents the Table of Contents (TOC) byte of an Opus packet.
#[derive(Debug, Clone, Copy)]
pub struct Toc {
    config: u8,
    stereo: bool,
    frame_count: FrameCount,
}

impl Toc {
    pub fn new(data: &[u8]) -> Result<Self> {
        let toc_byte = *data.first().ok_or(Error::DecodeError("Empty Opus packet"))?;
        let buf = [toc_byte];
        let mut reader = BitReaderLtr::new(&buf);

        // 'config' field (bits 0-4).
        let config = reader.read_bits_leq32(5)? as u8;

        // 's' (stereo) flag (bit 5).
        let stereo = reader.read_bool()?;

        // 'c' (frame count code) field (bits 6-7).
        let frame_count_code = reader.read_bits_leq32(2)? as u8;
        let frame_count = FrameCount::try_from(frame_count_code)?;

        return Ok(Toc {
            config,
            stereo,
            frame_count,
        });
    }

    pub fn params(&self) -> Result<Parameters> {
        Parameters::new(self.config)
    }

    pub fn is_stereo(&self) -> bool {
        self.stereo
    }

    pub fn frame_count(&self) -> FrameCount {
        self.frame_count
    }
}

#[derive(Debug)]
pub struct Parameters {
    pub audio_mode: AudioMode,
    pub bandwidth: Bandwidth,
    pub frame_size: FrameSize,
}

impl Parameters {
    pub fn new(config: u8) -> Result<Self> {
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

#[derive(Debug, Clone, Copy)]
pub enum AudioMode {
    Silk,
    Hybrid,
    Celt,
}

impl TryFrom<u8> for AudioMode {
    type Error = Error;

    fn try_from(config: u8) -> Result<Self> {
        match config {
            0..=11 => Ok(AudioMode::Silk),
            12..=15 => Ok(AudioMode::Hybrid),
            16..=31 => Ok(AudioMode::Celt),
            _ => Err(Error::DecodeError("Invalid audio mode")),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Bandwidth {
    NarrowBand,
    MediumBand,
    WideBand,
    SuperWideBand,
    FullBand,
}

impl TryFrom<u8> for Bandwidth {
    type Error = Error;

    fn try_from(config: u8) -> Result<Self> {
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
            _ => Err(Error::DecodeError("Invalid bandwidth")),
        };
    }
}

/// Enumeration of possible frame sizes in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum FrameSize {
    Ms2_5 = 2_500_000,
    Ms5 = 5_000_000,
    Ms10 = 10_000_000,
    Ms20 = 20_000_000,
    Ms40 = 40_000_000,
    Ms60 = 60_000_000,
}

impl From<FrameSize> for Duration {
    fn from(frame_size: FrameSize) -> Self {
        return Duration::from_nanos(frame_size as u64);
    }
}

impl TryFrom<u8> for FrameSize {
    type Error = Error;

    fn try_from(config: u8) -> Result<Self> {
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
            _ => Err(Error::DecodeError("Invalid frame size")),
        };
    }
}

#[derive(Debug, Clone, Copy)]
pub enum FrameCount {
    One,
    TwoEqual,
    TwoUnequal,
    Arbitrary,
}

impl TryFrom<u8> for FrameCount {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self> {
        return match value {
            0 => Ok(FrameCount::One),
            1 => Ok(FrameCount::TwoEqual),
            2 => Ok(FrameCount::TwoUnequal),
            3 => Ok(FrameCount::Arbitrary),
            _ => Err(Error::DecodeError("Invalid frame count code")),
        };
    }
}

