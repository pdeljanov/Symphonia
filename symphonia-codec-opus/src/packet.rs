use crate::toc::{FrameCount, Toc};
use std::num::NonZeroU8;
use std::time::Duration;
use symphonia_core::errors::Result;
use symphonia_core::io::{BitReaderRtl, BitReaderLtr, ReadBitsLtr};
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

pub struct FramePacket<'a> {
    toc: Toc,
    frames: Vec<&'a [u8]>,
    padding: Option<&'a [u8]>,
}

impl<'a> FramePacket<'a> {
    pub fn new(buf: &'a [u8]) -> Result<Self> {
        let (toc_byte, data) = buf.split_first().ok_or(Error::PacketTooShort)?;

        if data.is_empty() {
            return Err(Error::EmptyFrameData.into());
        }

        let toc = Toc::new(*toc_byte).map_err(Error::Toc)?;

        return match toc.frame_count() {
            FrameCount::One => Self::one(data, toc),
            FrameCount::TwoEqual => Self::two_equal(data, toc),
            FrameCount::TwoDifferent => Self::two_different(data, toc),
            FrameCount::Arbitrary => Self::arbitrary(data, toc),
        };
    }

    fn one(data: &'a [u8], toc: Toc) -> Result<Self> {
        Self::check_frame_size(data.len())?;

        return Ok(Self {
            toc,
            frames: vec![data],
            padding: None,
        });
    }

    fn two_equal(data: &'a [u8], toc: Toc) -> Result<Self> {
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

    fn two_different(data: &'a [u8], toc: Toc) -> Result<Self> {
        let (n1, offset) = Self::get_frame_length(data)?;

        if data.len() < offset + n1 {
            return Err(Error::FirstFrameLengthExceedsData.into());
        }

        let frame1_end = offset + n1;
        let frame1 = &data[offset..frame1_end];
        let frame2 = &data[frame1_end..];

        Self::check_frame_size(frame1.len())?;
        Self::check_frame_size(frame2.len())?;

        return Ok(Self {
            toc,
            frames: vec![frame1, frame2],
            padding: None,
        });
    }

    fn arbitrary(data: &'a [u8], toc: Toc) -> Result<Self> {
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

        return  Ok(Self {
            toc,
            frames,
            padding: padding_data,
        })
    }

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


    fn get_cbr_frames(data: &'a [u8], frame_count: NonZeroU8) -> Result<Vec<&'a [u8]>> {
        let frame_count = frame_count.get() as usize;
        let frame_size = data.len() / frame_count;

        if frame_size * frame_count != data.len() {
            return Err(Error::InvalidCbrFrameLength.into());
        }

        Self::check_frame_size(frame_size)?;

        return Ok(data.chunks_exact(frame_size).collect());
    }

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
    #[test]
    fn parse_single_frame_packet() {
        let toc_byte = 0b0000_0000;
        let frame_data = [0xAA, 0xBB, 0xCC];

        let packet_data = [toc_byte].iter().chain(&frame_data).cloned().collect::<Vec<u8>>();
        let packet = FramePacket::new(&packet_data).expect("Failed to parse single frame packet");

        assert_eq!(packet.frames.len(), 1);
        assert_eq!(packet.frames[0], &frame_data[..]);
        assert!(packet.padding.is_none());
    }

    #[test]
    fn parse_two_equal_frames_packet() {
        let toc_byte = 0b0000_0001;
        let frame_data = [0xAA, 0xBB, 0xCC, 0xDD];

        let packet_data = [toc_byte].iter().chain(&frame_data).cloned().collect::<Vec<u8>>();
        let packet = FramePacket::new(&packet_data).expect("Failed to parse two equal frames packet");

        assert_eq!(packet.frames.len(), 2);
        assert_eq!(packet.frames[0], &frame_data[0..2]);
        assert_eq!(packet.frames[1], &frame_data[2..4]);
        assert!(packet.padding.is_none());
    }

    #[test]
    fn parse_two_different_frames_packet() {
        let toc_byte = 0b0000_0010;
        let frame1 = [0xAA, 0xBB];
        let frame2 = [0xCC, 0xDD, 0xEE];

        let frame1_length = [0x02];
        let frame_data = frame1_length.iter().chain(&frame1).chain(&frame2).cloned().collect::<Vec<u8>>();

        let packet_data = [toc_byte].iter().chain(&frame_data).cloned().collect::<Vec<u8>>();
        let packet = FramePacket::new(&packet_data).expect("Failed to parse two different frames packet");

        assert_eq!(packet.frames.len(), 2);
        assert_eq!(packet.frames[0], &frame1[..]);
        assert_eq!(packet.frames[1], &frame2[..]);
        assert!(packet.padding.is_none());
    }

    #[test]
    fn parse_arbitrary_frames_cbr_packet() {
        let toc_byte = 0b0000_0011;
        let frame_count_byte = 0b0000_0011;
        let frame1 = [0xAA, 0xBB];
        let frame2 = [0xCC, 0xDD];
        let frame3 = [0xEE, 0xFF];
        let frame_data = [frame1, frame2, frame3].concat();

        let packet_data = [toc_byte, frame_count_byte].iter().chain(&frame_data).cloned().collect::<Vec<u8>>();
        let packet = FramePacket::new(&packet_data).expect("Failed to parse arbitrary frames CBR packet");

        assert_eq!(packet.frames.len(), 3);
        assert_eq!(packet.frames[0], &frame1[..]);
        assert_eq!(packet.frames[1], &frame2[..]);
        assert_eq!(packet.frames[2], &frame3[..]);
        assert!(packet.padding.is_none());
    }

    #[test]
    fn parse_arbitrary_frames_vbr_packet() {
        let toc_byte = 0b0000_0011;
        let frame_count_byte = 0b1000_0010;
        let frame1_length = [0x02];
        let frame1 = [0xAA, 0xBB];
        let frame2 = [0xCC, 0xDD, 0xEE];
        let frame_data = frame1_length.iter()
            .chain(&frame1)
            .chain(&frame2)
            .cloned()
            .collect::<Vec<u8>>();

        let packet_data = [toc_byte, frame_count_byte]
            .iter()
            .chain(&frame_data)
            .cloned()
            .collect::<Vec<u8>>();

        let packet = FramePacket::new(&packet_data).expect("Failed to parse arbitrary frames VBR packet");

        assert_eq!(packet.frames.len(), 2);
        assert_eq!(packet.frames[0], &frame1[..]);
        assert_eq!(packet.frames[1], &frame2[..]);
        assert!(packet.padding.is_none());
    }

    #[test]
    fn parse_packet_with_padding() {
        let toc_byte = 0b0000_0011;
        let frame_count_byte = 0b0100_0001;
        let padding_length = [0x02];
        let padding_data = [0x00, 0x00];
        let frame = [0xAA, 0xBB, 0xCC];

        let packet_data = [toc_byte, frame_count_byte]
            .iter()
            .chain(&padding_length)
            .chain(&padding_data)
            .chain(&frame)
            .cloned()
            .collect::<Vec<u8>>();

        let packet = FramePacket::new(&packet_data).expect("Failed to parse packet with padding");

        assert_eq!(packet.frames.len(), 1); // FAILED: assertion `left == right` failed left: [0, 0, 170] right: [170, 187, 204] 
        assert_eq!(packet.frames[0], &frame[..]);

        let padding_start = 2; 
        let padding_end = padding_start + padding_length[0] as usize;

        assert_eq!(packet.padding.unwrap(), &packet_data[padding_start..padding_end]);
    }

    #[test]
    fn handle_invalid_packet_length() {
        let toc_byte = 0b0000_0001;
        let frame_data = [0xAA, 0xBB, 0xCC];

        let packet_data = [toc_byte].iter().chain(&frame_data).cloned().collect::<Vec<u8>>();
        let result = FramePacket::new(&packet_data);

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
