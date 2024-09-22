/*use crate::toc::{FrameCount, Toc};
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
            FrameCount::TwoEqual => Self::two_equal_frames(data, toc),
            FrameCount::TwoDifferent => Self::two_different_frames(data, toc),
            FrameCount::Arbitrary => Self::signaled_number_of_frames(data, toc),
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
    use proptest::prelude::*;

    fn create_toc(config: u8, stereo: bool, frame_count: FrameCount) -> Toc {
        return Toc{ config, stereo, frame_count }
    }

    #[test]
    fn single_frame_packet() {
        let toc = create_toc(0, false, FrameCount::One);
        let data = [0; 10]; // 10 bytes of zero data
        let packet = FramePacket::new(&data).unwrap();

        assert_eq!(packet.toc(), toc);
        assert_eq!(packet.frames().len(), 1);
        assert_eq!(packet.frames()[0], &data[1..]);
        assert!(packet.padding().is_none());
    }

    #[test]
    fn two_equal_frames_packet() {
        let toc = create_toc(0, false, FrameCount::TwoEqual);
        let data = [0; 11]; // 11 bytes of zero data
        let packet = FramePacket::new(&data).unwrap();

        assert_eq!(packet.toc(), toc);
        assert_eq!(packet.frames().len(), 2);
        assert_eq!(packet.frames()[0], &data[1..6]);
        assert_eq!(packet.frames()[1], &data[6..]);
        assert!(packet.padding().is_none());
    }

    #[test]
    fn two_different_frames_packet() {
        let toc = create_toc(0, false, FrameCount::TwoDifferent);
        let data = [0, 2, 0, 0, 3, 0, 0, 0]; // TOC, length1, frame_1, length2, frame_2
        let packet = FramePacket::new(&data).unwrap();

        assert_eq!(packet.toc(), toc);
        assert_eq!(packet.frames().len(), 2);
        assert_eq!(packet.frames()[0], &[0, 0]);
        assert_eq!(packet.frames()[1], &[0, 0, 0]);
        assert!(packet.padding().is_none());
    }

    #[test]
    fn arbitrary_frames_cbr_packet() {
        let toc = create_toc(0, false, FrameCount::Arbitrary);
        let data = [0, 0b00000100, 0, 0, 0, 0, 0, 0, 0, 0, 0]; // TOC, frame_count=4, 8 bytes of data
        let packet = FramePacket::new(&data).unwrap();

        assert_eq!(packet.toc(), toc);
        assert_eq!(packet.frames().len(), 4);
        for frame in packet.frames() {
            assert_eq!(frame.len(), 2);
        }
        assert!(packet.padding().is_none());
    }

    #[test]
    fn arbitrary_frames_vbr_packet() {
        let toc = create_toc(0, false, FrameCount::Arbitrary);
        let data = [0, 0b10000011, 1, 2, 3, 0, 1, 2, 3, 4, 5]; // TOC, frame_count=3 (VBR), lengths, data
        let packet = FramePacket::new(&data).unwrap();

        assert_eq!(packet.toc(), toc);
        assert_eq!(packet.frames().len(), 3);
        assert_eq!(packet.frames()[0], &[0]);
        assert_eq!(packet.frames()[1], &[1, 2]);
        assert_eq!(packet.frames()[2], &[3, 4, 5]);
        assert!(packet.padding().is_none());
    }

    #[test]
    fn packet_with_padding() {
        let toc = create_toc(0, false, FrameCount::Arbitrary);
        let data = [0, 0b01000010, 2, 0, 0, 0, 0, 1, 0xFF, 0xFF]; // TOC, frame_count=2 (CBR), padding=1, data, padding
        let packet = FramePacket::new(&data).unwrap();

        assert_eq!(packet.toc(), toc);
        assert_eq!(packet.frames().len(), 2);
        assert_eq!(packet.frames()[0], &[0, 0]);
        assert_eq!(packet.frames()[1], &[0, 0]);
        assert_eq!(packet.padding(), Some(&[0xFF, 0xFF][..]));
    }

    #[test]
    fn handle_invalid_packet_length() {
        FramePacket::new(&[]).unwrap();
    }

    proptest! {
        #[test]
        fn prop_valid_packet_construction(
            config in 0u8..32,
            stereo in proptest::bool::ANY,
            frame_count in 1u8..5,
            data in proptest::collection::vec(0u8..255, 1..1000)
        ) {
            let toc = create_toc(config, stereo, FrameCount::Arbitrary);
            let mut packet_data = vec![toc.as_byte(), frame_count];
            packet_data.extend_from_slice(&data);

            if let Ok(packet) = FramePacket::new(&packet_data) {
                prop_assert_eq!(packet.toc(), toc);
                prop_assert!(packet.frames().len() > 0);
                prop_assert!(packet.frames().len() <= frame_count as usize);
            }
        }

        #[test]
        fn prop_invalid_packet_construction(
            config in 0u8..32,
            stereo in proptest::bool::ANY,
            frame_count in 0u8..1,
            data in proptest::collection::vec(0u8..255, 0..1000)
        ) {
            let toc = create_toc(config, stereo, FrameCount::Arbitrary);
            let mut packet_data = vec![toc.as_byte(), frame_count];
            packet_data.extend_from_slice(&data);

            prop_assert!(FramePacket::new(&packet_data).is_err());
        }
    }
}*/