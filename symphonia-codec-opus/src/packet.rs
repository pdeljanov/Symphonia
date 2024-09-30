//! Opus packet parsing implementation.
//!
//! This module provides functionality to parse Opus packets as specified in RFC 6716.
//! It includes structures and methods to handle various packet configurations and frame counts.
//!
//! References:
//! - RFC 6716: Definition of the Opus Audio Codec (https://tools.ietf.org/html/rfc6716)
//! Opus packet parsing implementation.
//!
//! This module provides functionality to parse Opus packets as specified in RFC 6716.
//! It includes structures and methods to handle various packet configurations and frame counts.
//!
//! References:
//! - RFC 6716: Definition of the Opus Audio Codec (https://tools.ietf.org/html/rfc6716)
use crate::toc::{FrameCount, Toc};
use std::num::NonZeroU8;
use std::time::Duration;
use symphonia_core::errors::Result;
use symphonia_core::io::{BitReaderLtr, ReadBitsLtr};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Packet is too short")]
    PacketTooShort,

    #[error("Empty frame data")]
    EmptyFrameData,

    #[error("Invalid packet length")]
    InvalidCode1PacketLength,

    #[error("First frame length exceeds available data")]
    FirstFrameLengthExceedsData,

    #[error("Number of frames can't be zero")]
    ZeroFrameCount,

    #[error("Insufficient data after accounting for padding")]
    InsufficientDataAfterPadding,

    #[error("Frame length exceeds data size")]
    FrameLengthExceedsDataSize,

    #[error("Invalid frame length for CBR")]
    InvalidCbrFrameLength,

    #[error("Padding length overflow")]
    PaddingLengthOverflow,

    #[error("Insufficient data for frame length")]
    InsufficientDataForFrameLength,

    #[error("Insufficient data for extended frame length")]
    InsufficientDataForExtendedFrameLength,

    #[error("Total duration overflow")]
    TotalDurationOverflow,

    #[error("Total audio duration exceeds 120 ms")]
    ExcessiveTotalDuration,

    #[error("Frame length exceeds maximum allowed size")]
    FrameLengthExceedsMaximum,

    #[error("Symphonia error: {0}")]
    CoreError(#[from] symphonia_core::errors::Error),

    #[error("TOC error: {0}")]
    Toc(#[from] crate::toc::Error),
}


impl From<Error> for symphonia_core::errors::Error {
    fn from(err: Error) -> Self {
        return symphonia_core::errors::Error::DecodeError(err.to_string().leak());
    }
}

const MAX_FRAME_LENGTH: usize = 255 * 4 + 255; // 1275 
const MAX_TOTAL_DURATION_MS: u128 = 120;
const MAX_PADDING_VALUE: u8 = 254;


/// Packet Organization
/// ```text
///      0                   1                   2                   3
///      0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
///     +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///     | config  |s|    companded frame size                            |
///     +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///     |                                                               |
///     +-+         compressed frame 1 (N-1 bytes)...                 +-+
///     |                                                               |
///     +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///
///                    Figure 1: A Code 0 Packet
///```
///
/// https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.2
pub struct FramePacket<'a> {
    /// Table of Contents (TOC) byte
    /// ```text
    ///  0 1 2 3 4 5 6 7
    /// +-+-+-+-+-+-+-+-+
    /// | config  |s| c |
    /// +-+-+-+-+-+-+-+-+
    ///
    ///                Figure 2: The TOC byte
    ///```
    ///
    /// https://datatracker.ietf.org/doc/html/rfc6716#section-3.1
    pub(crate) toc: Toc,
    pub(crate) frames: Vec<&'a [u8]>,
    padding: Option<&'a [u8]>,
}

impl<'a> FramePacket<'a> {
    pub fn new(buf: &'a [u8]) -> Result<Self> {
        let (toc_byte, data) = buf.split_first().ok_or(Error::PacketTooShort)?;

        if data.is_empty() {
            return Err(Error::EmptyFrameData.into());
        }

        let toc = Toc::try_new(*toc_byte).map_err(Error::Toc)?;

        return match toc.frame_count() {
            FrameCount::One => Self::one(data, toc),
            FrameCount::TwoEqual => Self::two_equal_frames(data, toc),
            FrameCount::TwoDifferent => Self::two_different_frames(data, toc),
            FrameCount::Arbitrary => Self::signaled_number_of_frames(data, toc),
        };
    }

    /// Parse a Code 0 packet (single frame).
    ///
    /// https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.2
    fn one(data: &'a [u8], toc: Toc) -> Result<Self> {
        Self::check_frame_size(data.len())?;

        return Ok(Self {
            toc,
            frames: vec![data],
            padding: None,
        });
    }

    /// Parse a Code 1 packet (two frames with equal compressed size).
    ///
    /// https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.3j
    fn two_equal_frames(data: &'a [u8], toc: Toc) -> Result<Self> {
        if data.len() % 2 != 0 {
            return Err(Error::InvalidCode1PacketLength.into());
        }

        let frame_size = data.len() / 2;
        Self::check_frame_size(frame_size)?;

        return Ok(Self {
            toc,
            frames: data.chunks(frame_size).collect(),
            padding: None,
        });
    }

    /// Parse a Code 2 packet (two frames with different compressed sizes).
    ///
    /// https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.4
    fn two_different_frames(data: &'a [u8], toc: Toc) -> Result<Self> {
        let (n1, offset) = Self::get_frame_length(data)?;

        if data.len() < offset + n1 {
            return Err(Error::FirstFrameLengthExceedsData.into());
        }

        let frame_1_end = offset + n1;
        let frame_1 = &data[offset..frame_1_end];
        let frame_2 = &data[frame_1_end..];

        Self::check_frame_size(frame_1.len())?;
        Self::check_frame_size(frame_2.len())?;

        return Ok(Self {
            toc,
            frames: vec![frame_1, frame_2],
            padding: None,
        });
    }

    /// Parse a Code 3 packet (an arbitrary number of frames).
    ///
    /// This method handles both CBR and VBR modes, as well as padding.
    ///
    /// https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.5
    fn signaled_number_of_frames(data: &'a [u8], toc: Toc) -> Result<Self> {
        let (frame_count_byte, rest) = data.split_first().ok_or(Error::PacketTooShort)?;

        let buf = [*frame_count_byte];
        let mut reader = BitReaderLtr::new(&buf);

        let vbr = reader.read_bool()?;
        let padding_flag = reader.read_bool()?;
        let m = reader.read_bits_leq32(6)? as u8;

        let frame_count = NonZeroU8::new(m).ok_or(Error::ZeroFrameCount)?;

        Self::check_total_duration(frame_count.get(), &toc)?;

        let mut offset = 0;

        let (padding_length, padding_offset) = if padding_flag {
            Self::get_padding_length(&rest[offset..])?
        } else {
            (0, 0)
        };

        offset += padding_offset;

        if rest.len() < offset + padding_length {
            return Err(Error::InsufficientDataAfterPadding.into());
        }

        let padding_data = if padding_length > 0 {
            let padding_start = offset;
            let padding_end = offset + padding_length;
            let padding_data = &rest[padding_start..padding_end];
            offset = padding_end;
            Some(padding_data)
        } else {
            None
        };

        let frames_data = &rest[offset..];

        let frames = if vbr {
            Self::get_vbr_frames(frames_data, frame_count.get())?
        } else {
            Self::get_cbr_frames(frames_data, frame_count)?
        };

        return Ok(Self {
            toc,
            frames,
            padding: padding_data,
        });
    }

    /// Parse frames for VBR (Variable Bit Rate) packets.
    ///
    /// https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.5
    fn get_vbr_frames(data: &'a [u8], frame_count: u8) -> Result<Vec<&'a [u8]>> {
        let mut frames = Vec::with_capacity(frame_count as usize);
        let mut offset = 0;

        for i in 0..frame_count {
            let (frame_len, len_offset) = if i == frame_count - 1 {
                (data.len() - offset, 0)
            } else {
                let (frame_len, len_offset) = Self::get_frame_length(&data[offset..])?;
                (frame_len, len_offset)
            };

            let frame_start = offset + len_offset;
            let frame_end = frame_start + frame_len;

            let frame = data.get(frame_start..frame_end).ok_or(Error::FrameLengthExceedsDataSize)?;

            Self::check_frame_size(frame.len())?;
            frames.push(frame);
            offset = frame_end;
        }

        return Ok(frames);
    }


    /// Parse frames for CBR (Constant Bit Rate) packets.
    ///
    /// https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.5
    fn get_cbr_frames(data: &'a [u8], frame_count: NonZeroU8) -> Result<Vec<&'a [u8]>> {
        let frame_count = frame_count.get() as usize;
        let frame_size = data.len() / frame_count;

        if frame_size * frame_count != data.len() {
            return Err(Error::InvalidCbrFrameLength.into());
        }

        Self::check_frame_size(frame_size)?;

        return Ok(data.chunks_exact(frame_size).collect());
    }

    /// Padding
    /// ```text
    /// Values from 0...254 indicate that 0...254 bytes of padding are included,
    /// in addition to the bytes used to indicate the size of the padding.
    /// If the value is 255, then the size of the additional padding is 254 bytes,
    /// plus the padding value encoded in the next byte.
    ///```
    ///
    /// https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.5
    fn get_padding_length(data: &[u8]) -> Result<(usize, usize)> {
        let mut total_padding: usize = 0;
        let mut offset: usize = 0;

        for &byte in data {
            offset += 1;

            total_padding = total_padding.checked_add(byte as usize).ok_or(Error::PaddingLengthOverflow)?;

            if byte != 255 {
                break;
            }
        }

        return Ok((total_padding, offset));
    }

    /// Frame length coding
    /// ```text
    /// 0: No frame (DTX or lost packet)
    /// 1...251: Length of the frame in bytes
    /// 252...255: A second byte is needed. The total length is (second_byte*4)+first_byte
    ///```
    ///
    /// https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.1
    fn get_frame_length(data: &[u8]) -> Result<(usize, usize)> {
        let (&first_byte, rest) = data.split_first()
            .ok_or(Error::InsufficientDataForFrameLength)?;

        return match first_byte {
            0 => Ok((0, 1)),
            1..=251 => Ok((first_byte as usize, 1)),
            252..=255 => {
                let &second_byte = rest.first()
                    .ok_or(Error::InsufficientDataForExtendedFrameLength)?;
                let length = (second_byte as usize * 4) + (first_byte as usize);
                Ok((length, 2))
            }
            _ => unreachable!()
        };
    }

    fn check_total_duration(frame_count: u8, toc: &Toc) -> Result<()> {
        let params = toc.params().map_err(Error::Toc)?;
        let total_duration_ms = Duration::from(params.frame_size).as_millis()
            .checked_mul(frame_count as u128)
            .ok_or(Error::TotalDurationOverflow)?;

        if total_duration_ms > MAX_TOTAL_DURATION_MS {
            return Err(Error::ExcessiveTotalDuration.into());
        }

        return Ok(());
    }

    fn check_frame_size(size: usize) -> Result<()> {
        if size > MAX_FRAME_LENGTH {
            return Err(Error::FrameLengthExceedsMaximum.into());
        }
        return Ok(());
    }

    pub fn toc(&self) -> Toc {
        return self.toc;
    }

    pub fn frames(&self) -> &[&'a [u8]] {
        return &self.frames;
    }

    pub fn padding(&self) -> Option<&'a [u8]> {
        return self.padding;
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    struct Packet {
        toc_byte: u8,
        vbr: bool,
        padding_flag: bool,
        frame_count: Option<u8>,
        padding_length: usize,
        frames: Vec<Vec<u8>>,
    }

    impl Packet {
        fn new(toc_byte: u8) -> Self {
            return Self {
                toc_byte,
                vbr: false,
                padding_flag: false,
                frame_count: None,
                padding_length: 0,
                frames: Vec::new(),
            };
        }

        fn vbr(mut self, vbr: bool) -> Self {
            self.vbr = vbr;
            return self;
        }

        fn padding_flag(mut self, padding_flag: bool) -> Self {
            self.padding_flag = padding_flag;
            return self;
        }

        fn frame_count(mut self, frame_count: u8) -> Self {
            self.frame_count = Some(frame_count);
            return self;
        }

        fn padding(mut self, padding_length: usize) -> Self {
            self.padding_flag = true;
            self.padding_length = padding_length;
            return self;
        }

        fn add_frame(mut self, frame: &[u8]) -> Self {
            self.frames.push(frame.to_vec());
            return self;
        }

        fn build(self) -> Vec<u8> {
            let mut data = Vec::new();
            data.push(self.toc_byte);

            if let Some(frame_count) = self.frame_count {
                let mut frame_count_byte = 0u8;
                if self.vbr {
                    frame_count_byte |= 0b1000_0000;
                }
                if self.padding_flag {
                    frame_count_byte |= 0b0100_0000;
                }
                frame_count_byte |= frame_count & 0b00111111;
                data.push(frame_count_byte);
            }

            if self.padding_flag {
                let mut remaining_padding = self.padding_length;
                while remaining_padding >= 255 {
                    data.push(255u8);
                    remaining_padding -= 255;
                }
                data.push(remaining_padding as u8);
                data.extend(vec![0u8; self.padding_length]);
            }

            if self.vbr {
                for (i, frame) in self.frames.iter().enumerate() {
                    if i != self.frames.len() - 1 {
                        let frame_len = frame.len();
                        assert!(
                            frame_len < 252,
                            "Frame length too long for test builder (max 251)"
                        );
                        data.push(frame_len as u8);
                    }
                    data.extend(frame);
                }
            } else {
                for frame in self.frames {
                    data.extend(frame);
                }
            }

            return data;
        }
    }

    #[test]
    fn single_frame_packet() {
        let toc_byte = 0b0000_0000;
        let frame_data = [0xAA, 0xBB, 0xCC];

        let packet_data = Packet::new(toc_byte)
            .add_frame(&frame_data)
            .build();

        let packet = FramePacket::new(&packet_data).expect("Failed to parse single-frame packet");

        assert_eq!(packet.frames.len(), 1);
        assert_eq!(packet.frames[0], &frame_data[..]);
        assert!(packet.padding.is_none());
    }

    #[test]
    fn two_equal_frames_packet() {
        let toc_byte = 0b0000_0001;
        let frame_data = [0xAA, 0xBB, 0xCC, 0xDD];

        let packet_data = Packet::new(toc_byte)
            .add_frame(&frame_data)
            .build();

        let packet = FramePacket::new(&packet_data).expect("Failed to parse two equal frames packet");

        assert_eq!(packet.frames.len(), 2);
        assert_eq!(packet.frames[0], &frame_data[0..2]);
        assert_eq!(packet.frames[1], &frame_data[2..4]);
        assert!(packet.padding.is_none());
    }

    #[test]
    fn two_different_frames_packet() {
        let toc_byte = 0b0000_0010;
        let frame_1 = [0xAA, 0xBB];
        let frame_2 = [0xCC, 0xDD, 0xEE];

        let frame_1_len = frame_1.len();
        assert!(
            frame_1_len < 252,
            "Frame length too long for test builder (max 251)"
        );
        let frame_1_len_byte = [frame_1_len as u8];

        let data = Packet::new(toc_byte)
            .add_frame(&frame_1_len_byte)
            .add_frame(&frame_1)
            .add_frame(&frame_2)
            .build();

        let packet = FramePacket::new(&data).expect("Failed to parse two different frames packet");

        assert_eq!(packet.frames.len(), 2);
        assert_eq!(packet.frames[0], &frame_1[..]);
        assert_eq!(packet.frames[1], &frame_2[..]);
        assert!(packet.padding.is_none());
    }

    #[test]
    fn arbitrary_frames_cbr_packet() {
        let toc_byte = 0b0000_0011;
        let frame_count = 3u8;

        let frame_1 = [0xAA, 0xBB];
        let frame_2 = [0xCC, 0xDD];
        let frame_3 = [0xEE, 0xFF];

        let data = Packet::new(toc_byte)
            .frame_count(frame_count)
            .add_frame(&frame_1)
            .add_frame(&frame_2)
            .add_frame(&frame_3)
            .build();

        let packet = FramePacket::new(&data).expect("Failed to parse arbitrary frames CBR packet");

        assert_eq!(packet.frames.len(), 3);
        assert_eq!(packet.frames[0], &frame_1[..]);
        assert_eq!(packet.frames[1], &frame_2[..]);
        assert_eq!(packet.frames[2], &frame_3[..]);
        assert!(packet.padding.is_none());
    }

    #[test]
    fn arbitrary_frames_vbr_packet() {
        let toc_byte = 0b0000_0011;
        let frame_count = 2u8;

        let frame_1 = [0xAA, 0xBB];
        let frame_2 = [0xCC, 0xDD, 0xEE];

        let data = Packet::new(toc_byte)
            .vbr(true)
            .frame_count(frame_count)
            .add_frame(&frame_1)
            .add_frame(&frame_2)
            .build();

        let packet =
            FramePacket::new(&data).expect("Failed to parse arbitrary frames VBR packet");

        assert_eq!(packet.frames.len(), 2);
        assert_eq!(packet.frames[0], &frame_1[..]);
        assert_eq!(packet.frames[1], &frame_2[..]);
        assert!(packet.padding.is_none());
    }

    #[test]
    fn packet_with_padding() {
        let toc_byte = 0b0000_0011;
        let frame_count = 1u8;
        let frame = [0xAA, 0xBB, 0xCC];

        let data = Packet::new(toc_byte)
            .frame_count(frame_count)
            .padding(2)
            .add_frame(&frame)
            .build();

        let packet = FramePacket::new(&data).expect("Failed to parse packet with padding");

        assert_eq!(packet.frames.len(), 1);
        assert_eq!(packet.frames[0], &frame[..]);

        let expected = [0u8; 2];
        assert_eq!(packet.padding.unwrap(), &expected[..]);
    }

    #[test]
    fn handle_invalid_packet_length() {
        let toc_byte = 0b0000_0001;
        let frame_data = [0xAA, 0xBB, 0xCC];

        let data = Packet::new(toc_byte)
            .add_frame(&frame_data)
            .build();

        let result = FramePacket::new(&data);

        assert!(result.is_err());
        match result {
            Err(err) => match err {
                symphonia_core::errors::Error::DecodeError(msg) => {
                    assert_eq!(msg, "Invalid packet length");
                }
                _ => panic!("Unexpected error type"),
            },
            _ => panic!("Expected an error"),
        }
    }
}
