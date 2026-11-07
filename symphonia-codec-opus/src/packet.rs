//! Opus packet parsing implementation.
//!
//! This module provides functionality to parse Opus packets as specified in RFC 6716.
//! It includes structures and methods to handle various packet configurations and frame counts.
//!
//! References:
//! - RFC 6716: Definition of the Opus Audio Codec (<https://tools.ietf.org/html/rfc6716>)
use crate::toc::{AudioMode, Bandwidth, FrameCount, FrameDuration, Toc};
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
const MAX_TOTAL_DURATION_US: u128 = 120_000;
const MAX_PADDING_VALUE: u8 = 254;
const MAX_FRAMES: usize = 48;


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
/// ```
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.2>
pub struct FramePacket<'a> {
    /// Stereo flag (`s` bit of the [`Toc`] byte: `false` = mono, `true` = stereo).
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>
    pub stereo: bool,
    /// Operating mode decoded from the 5-bit `config` field of the [`Toc`] byte.
    pub audio_mode: AudioMode,
    /// Audio bandwidth decoded from the 5-bit `config` field of the [`Toc`] byte.
    pub bandwidth: Bandwidth,
    /// Duration of a single frame decoded from the 5-bit `config` field of the [`Toc`] byte.
    pub frame_size: FrameDuration,
    pub frames: Vec<&'a [u8]>,
    pub padding: Option<&'a [u8]>,
}

impl<'a> FramePacket<'a> {
    pub fn new(buf: &'a [u8]) -> Result<Self> {
        let (toc_byte, data) = buf.split_first().ok_or(Error::PacketTooShort)?;

        let Toc {
            stereo,
            frame_count,
            audio_mode,
            bandwidth,
            frame_size, ..
        } = Toc::try_new(*toc_byte).map_err(Error::Toc)?;

        let packet = Self {
            stereo,
            audio_mode,
            bandwidth,
            frame_size,
            frames: vec![],
            padding: None,
        };

        return match frame_count {
            FrameCount::One => packet.one(data),
            FrameCount::TwoEqual => packet.two_equal_frames(data),
            FrameCount::TwoDifferent => packet.two_different_frames(data),
            FrameCount::Arbitrary => packet.signaled_number_of_frames(data),
        };
    }

    /// Parses a self-delimited Opus packet from the beginning of `buf`, returning the parsed
    /// [`FramePacket`] and the exact number of bytes consumed from `buf`
    /// (RFC 6716 Appendix B / RFC 7845 Section 5.1.1).
    pub fn parse_self_delimited(buf: &'a [u8]) -> Result<(Self, usize)> {
        let (&toc_byte, data) = buf.split_first().ok_or(Error::PacketTooShort)?;

        let Toc {
            stereo,
            frame_count,
            audio_mode,
            bandwidth,
            frame_size,
            ..
        } = Toc::try_new(toc_byte).map_err(Error::Toc)?;

        let packet = Self {
            stereo,
            audio_mode,
            bandwidth,
            frame_size,
            frames: vec![],
            padding: None,
        };

        return match frame_count {
            FrameCount::One => packet.one_self_delimited(data),
            FrameCount::TwoEqual => packet.two_equal_self_delimited(data),
            FrameCount::TwoDifferent => packet.two_different_self_delimited(data),
            FrameCount::Arbitrary => packet.signaled_self_delimited(data),
        };
    }

    /// Code 0: One Frame in the Packet
    /// For code 0 packets, the TOC byte is immediately followed by N-1 bytes
    /// of compressed data for a single frame (where N is the size of the
    /// packet), as illustrated in Figure 2.
    /// ```text
    ///       0                   1                   2                   3
    ///       0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
    ///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    ///      | config  |s|0|0|                                               |
    ///      +-+-+-+-+-+-+-+-+                                               |
    ///      |                    Compressed frame 1 (N-1 bytes)...          :
    ///      :                                                               |
    ///      |                                                               |
    ///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    /// 
    ///                          Figure 2: A Code 0 Packet
    /// ``` 
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.2>
    fn one(self, data: &'a [u8]) -> Result<Self> {
        Self::check_frame_size(data.len())?;

        return Ok(Self {
            frames: vec![data],
            padding: None,
            ..self
        });
    }

    /// Code 1: Two Frames in the Packet, Each with Equal Compressed Size
    /// For code 1 packets, the TOC byte is immediately followed by the
    /// (N-1)/2 bytes of compressed data for the first frame, followed by
    /// (N-1)/2 bytes of compressed data for the second frame, as illustrated
    /// in Figure 3. The number of payload bytes available for compressed
    /// data, N-1, MUST be even for all code 1 packets [R3].
    /// ```text
    ///      0                   1                   2                   3
    ///      0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
    ///     +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    ///     | config  |s|0|1|                                               |
    ///     +-+-+-+-+-+-+-+-+                                               :
    ///     |             Compressed frame 1 ((N-1)/2 bytes)...             |
    ///     :                               +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    ///     |                               |                               |
    ///     +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+                               :
    ///     |             Compressed frame 2 ((N-1)/2 bytes)...             |
    ///     :                                               +-+-+-+-+-+-+-+-+
    ///     |                                               |
    ///     +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    ///
    ///                         Figure 3: A Code 1 Packet
    /// ```
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.3j>
    fn two_equal_frames(self, data: &'a [u8]) -> Result<Self> {
        if data.len() % 2 != 0 {
            return Err(Error::InvalidCode1PacketLength.into());
        }

        let frame_size = data.len() / 2;
        Self::check_frame_size(frame_size)?;

        if frame_size == 0 {
            return Ok(Self {
                frames: vec![&[][..], &[][..]],
                padding: None,
                ..self
            });
        }

        return Ok(Self {
            frames: data.chunks(frame_size).collect(),
            padding: None,
            ..self
        });
    }

    fn one_self_delimited(self, data: &'a [u8]) -> Result<(Self, usize)> {
        let (n1, len_offset) = Self::get_frame_length(data)?;
        Self::check_frame_size(n1)?;

        let frame_start = len_offset;
        let frame_end = frame_start + n1;
        let frame = data
            .get(frame_start..frame_end)
            .ok_or(Error::FrameLengthExceedsDataSize)?;

        let consumed = 1 + frame_end;
        return Ok((
            Self {
                frames: vec![frame],
                padding: None,
                ..self
            },
            consumed,
        ));
    }

    fn two_equal_self_delimited(self, data: &'a [u8]) -> Result<(Self, usize)> {
        let (n1, len_offset) = Self::get_frame_length(data)?;
        Self::check_frame_size(n1)?;

        let frame_1_start = len_offset;
        let frame_1_end = frame_1_start + n1;
        let frame_2_end = frame_1_end + n1;

        if data.len() < frame_2_end {
            return Err(Error::FrameLengthExceedsDataSize.into());
        }

        let frame_1 = &data[frame_1_start..frame_1_end];
        let frame_2 = &data[frame_1_end..frame_2_end];
        let consumed = 1 + frame_2_end;

        return Ok((
            Self {
                frames: vec![frame_1, frame_2],
                padding: None,
                ..self
            },
            consumed,
        ));
    }

    fn two_different_self_delimited(self, data: &'a [u8]) -> Result<(Self, usize)> {
        let (n1, offset1) = Self::get_frame_length(data)?;
        Self::check_frame_size(n1)?;

        if data.len() < offset1 + n1 {
            return Err(Error::FirstFrameLengthExceedsData.into());
        }

        let (n2, offset2) = Self::get_frame_length(&data[offset1..])?;
        Self::check_frame_size(n2)?;

        let header_len = offset1 + offset2;
        let frame_1_end = header_len + n1;
        let frame_2_end = frame_1_end + n2;

        if data.len() < frame_2_end {
            return Err(Error::FrameLengthExceedsDataSize.into());
        }

        let frame_1 = &data[header_len..frame_1_end];
        let frame_2 = &data[frame_1_end..frame_2_end];
        let consumed = 1 + frame_2_end;

        return Ok((
            Self {
                frames: vec![frame_1, frame_2],
                padding: None,
                ..self
            },
            consumed,
        ));
    }

    fn signaled_self_delimited(self, data: &'a [u8]) -> Result<(Self, usize)> {
        let (&frame_count_byte, rest) = data.split_first().ok_or(Error::PacketTooShort)?;

        let buf = [frame_count_byte];
        let mut reader = BitReaderLtr::new(&buf);

        let vbr = reader.read_bool()?;
        let padding_flag = reader.read_bool()?;
        let m = reader.read_bits_leq32(6)? as u8;

        let frame_count = NonZeroU8::new(m).ok_or(Error::ZeroFrameCount)?;
        let count = frame_count.get() as usize;

        Self::check_total_duration(frame_count.get(), self.frame_size)?;

        let mut offset = 0usize;

        let (padding_length, padding_offset) = if padding_flag {
            if rest.is_empty() {
                return Err(Error::InsufficientDataAfterPadding.into());
            }
            let (pad_len, pad_off) = Self::get_padding_length(&rest[offset..])?;
            if rest[offset + pad_off - 1] == 255 {
                return Err(Error::InsufficientDataAfterPadding.into());
            }
            (pad_len, pad_off)
        } else {
            (0, 0)
        };

        offset += padding_offset;

        if rest.len() < offset + padding_length {
            return Err(Error::InsufficientDataAfterPadding.into());
        }

        let avail_without_padding = rest.len() - padding_length;
        let mut frames = Vec::with_capacity(count);

        if vbr {
            let mut frame_sizes = Vec::with_capacity(count);
            let mut total_frames_bytes = 0usize;

            for _ in 0..count {
                let (flen, len_bytes) =
                    Self::get_frame_length(&rest[offset..avail_without_padding])?;
                Self::check_frame_size(flen)?;
                offset += len_bytes;
                total_frames_bytes = total_frames_bytes
                    .checked_add(flen)
                    .ok_or(Error::FrameLengthExceedsDataSize)?;
                if offset + total_frames_bytes > avail_without_padding {
                    return Err(Error::FrameLengthExceedsDataSize.into());
                }
                frame_sizes.push(flen);
            }

            for flen in frame_sizes {
                let frame = &rest[offset..(offset + flen)];
                offset += flen;
                frames.push(frame);
            }
        } else {
            let (cbr_frame_size, len_bytes) =
                Self::get_frame_length(&rest[offset..avail_without_padding])?;
            Self::check_frame_size(cbr_frame_size)?;
            offset += len_bytes;

            let total_frames_bytes = cbr_frame_size
                .checked_mul(count)
                .ok_or(Error::FrameLengthExceedsDataSize)?;
            if offset + total_frames_bytes > avail_without_padding {
                return Err(Error::FrameLengthExceedsDataSize.into());
            }

            for _ in 0..count {
                let frame = &rest[offset..(offset + cbr_frame_size)];
                offset += cbr_frame_size;
                frames.push(frame);
            }
        }

        let padding = if padding_length > 0 {
            let pad_slice = &rest[offset..(offset + padding_length)];
            offset += padding_length;
            Some(pad_slice)
        } else {
            None
        };

        let consumed = 2 + offset;
        return Ok((
            Self {
                frames,
                padding,
                ..self
            },
            consumed,
        ));
    }

    /// Code 2: Two Frames in the Packet, with Different CompressedSizes
    /// For code 2 packets, the TOC byte is followed by a one- or two-byte
    /// sequence indicating the length of the first frame (marked N1 in
    /// Figure 4), followed by N1 bytes of compressed data for the first
    /// frame.  The remaining N-N1-2 or N-N1-3 bytes are the compressed data
    /// for the second frame.  This is illustrated in Figure 4.  A code 2
    /// packet MUST contain enough bytes to represent a valid length.  For
    /// example, a 1-byte code 2 packet is always invalid, and a 2-byte code
    /// 2 packet whose second byte is in the range 252...255 is also invalid.
    ///  The length of the first frame, N1, MUST also be no larger than the
    /// size of the payload remaining after decoding that length for all code
    /// 2 packets [R4].  This makes, for example, a 2-byte code 2 packet with
    /// a second byte in the range 1...251 invalid as well (the only valid
    /// 2-byte code 2 packet is one where the length of both frames is zero).
    /// ```text
    ///    0                   1                   2                   3
    ///    0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
    ///   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    ///   | config  |s|1|0| N1 (1-2 bytes):                               |
    ///   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+                               :
    ///   |               Compressed frame 1 (N1 bytes)...                |
    ///   :                               +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    ///   |                               |                               |
    ///   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+                               |
    ///   |                     Compressed frame 2...                     :
    ///   :                                                               |
    ///   |                                                               |
    ///   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    ///                         Figure 4: A Code 2 Packet
    /// ```
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.4>
    fn two_different_frames(self, data: &'a [u8]) -> Result<Self> {
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
            frames: vec![frame_1, frame_2],
            padding: None,
            ..self
        });
    }

    /// Code 3: A Signaled Number of Frames in the Packet
    /// Code 3 packets signal the number of frames, as well as additional
    /// padding, called "Opus padding" to indicate that this padding is added
    /// at the Opus layer rather than at the transport layer.Code 3 packets
    /// MUST have at least 2 bytes. The TOC byte is followed by a
    /// byte encoding the number of frames in the packet in bits 2 to 7
    /// (marked "M" in Figure 5), with bit 1 indicating whether or not Opus
    /// padding is inserted (marked "p" in Figure 5), and bit 0 indicating
    /// VBR (marked "v" in Figure 5).  M MUST NOT be zero, and the audio
    /// duration contained within a packet MUST NOT exceed 120 ms [R5].  This
    /// limits the maximum frame count for any frame size to 48 (for 2.5 ms
    /// frames), with lower limits for longer frame sizes.  Figure 5
    /// illustrates the layout of the frame count byte.
    /// ```text
    ///                             0
    ///                             0 1 2 3 4 5 6 7
    ///                            +-+-+-+-+-+-+-+-+
    ///                            |v|p|     M     |
    ///                            +-+-+-+-+-+-+-+-+
    ///
    ///                     Figure 5: The frame count byte
    /// ```
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.5>
    fn signaled_number_of_frames(self, data: &'a [u8]) -> Result<Self> {
        let (frame_count_byte, rest) = data.split_first().ok_or(Error::PacketTooShort)?;

        let buf = [*frame_count_byte];
        let mut reader = BitReaderLtr::new(&buf);

        let vbr = reader.read_bool()?;
        let padding_flag = reader.read_bool()?;
        let m = reader.read_bits_leq32(6)? as u8;

        let frame_count = NonZeroU8::new(m).ok_or(Error::ZeroFrameCount)?;

        Self::check_total_duration(frame_count.get(), self.frame_size)?;

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

        let avail_without_padding = rest.len() - padding_length;
        let frames_data = &rest[offset..avail_without_padding];
        let padding_data = if padding_length > 0 {
            Some(&rest[avail_without_padding..])
        } else {
            None
        };

        let frames = if vbr {
            Self::get_vbr_frames(frames_data, frame_count.get())?
        } else {
            Self::get_cbr_frames(frames_data, frame_count)?
        };

        return Ok(Self {
            frames,
            padding: padding_data,
            ..self
        });
    }

    /// Parse frames for VBR (Variable Bit Rate) packets.
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.5>
    fn get_vbr_frames(data: &'a [u8], frame_count: u8) -> Result<Vec<&'a [u8]>> {
        let count = frame_count as usize;
        let mut frame_sizes = Vec::with_capacity(count);
        let mut offset = 0usize;
        let mut total_prefix_frames = 0usize;

        for _ in 0..count.saturating_sub(1) {
            let (frame_len, len_offset) = Self::get_frame_length(&data[offset..])?;
            Self::check_frame_size(frame_len)?;
            offset += len_offset;
            total_prefix_frames = total_prefix_frames
                .checked_add(frame_len)
                .ok_or(Error::FrameLengthExceedsDataSize)?;
            if offset + total_prefix_frames > data.len() {
                return Err(Error::FrameLengthExceedsDataSize.into());
            }
            frame_sizes.push(frame_len);
        }

        let last_frame_len = data.len() - offset - total_prefix_frames;
        Self::check_frame_size(last_frame_len)?;
        frame_sizes.push(last_frame_len);

        let mut frames = Vec::with_capacity(count);
        for frame_len in frame_sizes {
            let frame = &data[offset..(offset + frame_len)];
            offset += frame_len;
            frames.push(frame);
        }

        return Ok(frames);
    }


    /// Parse frames for CBR (Constant Bit Rate) packets.
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.5>
    fn get_cbr_frames(data: &'a [u8], frame_count: NonZeroU8) -> Result<Vec<&'a [u8]>> {
        let frame_count = frame_count.get() as usize;
        let frame_size = data.len() / frame_count;

        if frame_size * frame_count != data.len() {
            return Err(Error::InvalidCbrFrameLength.into());
        }

        Self::check_frame_size(frame_size)?;

        if frame_size == 0 {
            return Ok(vec![&[][..]; frame_count]);
        }

        return Ok(data.chunks_exact(frame_size).collect());
    }

    /// Padding
    /// ```text
    /// Values from 0...254 indicate that 0...254 bytes of padding are included,
    /// in addition to the bytes used to indicate the size of the padding.
    /// If the value is 255, then the size of the additional padding is 254 bytes,
    /// plus the padding value encoded in the next byte.
    /// ```
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.5>
    fn get_padding_length(data: &[u8]) -> Result<(usize, usize)> {
        let mut total_padding: usize = 0;
        let mut offset: usize = 0;

        for &byte in data {
            offset += 1;

            let pad_increment = if byte == 255 {
                MAX_PADDING_VALUE as usize
            } else {
                byte as usize
            };

            total_padding = total_padding
                .checked_add(pad_increment)
                .ok_or(Error::PaddingLengthOverflow)?;

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
    /// ```
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.1>
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

    fn check_total_duration(frame_count: u8, frame_duration: FrameDuration) -> Result<()> {
        if frame_count as usize > MAX_FRAMES {
            return Err(Error::ExcessiveTotalDuration.into());
        }

        let total_duration_us = Duration::from(frame_duration)
            .as_micros()
            .checked_mul(frame_count as u128)
            .ok_or(Error::TotalDurationOverflow)?;

        if total_duration_us > MAX_TOTAL_DURATION_US {
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
                    remaining_padding -= MAX_PADDING_VALUE as usize;
                }
                data.push(remaining_padding as u8);
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
                }
                for frame in self.frames {
                    data.extend(frame);
                }
            } else {
                for frame in self.frames {
                    data.extend(frame);
                }
            }

            if self.padding_flag {
                data.extend(vec![0u8; self.padding_length]);
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

    #[test]
    fn total_duration_limits_all_frame_sizes() {
        // (config_index, max_valid_frames_for_120ms)
        // Config 16 = 2.5 ms (CELT NB) -> max 48 frames
        // Config 17 = 5 ms   (CELT NB) -> max 24 frames
        // Config 0  = 10 ms  (SILK NB) -> max 12 frames
        // Config 1  = 20 ms  (SILK NB) -> max 6 frames
        // Config 2  = 40 ms  (SILK NB) -> max 3 frames
        // Config 3  = 60 ms  (SILK NB) -> max 2 frames
        let cases: [(u8, u8); 6] = [
            (16, 48),
            (17, 24),
            (0, 12),
            (1, 6),
            (2, 3),
            (3, 2),
        ];

        for (config, max_frames) in cases {
            let toc_byte = (config << 3) | 0b011; // Code 3

            // Exactly 120 ms -> valid
            let mut valid_builder = Packet::new(toc_byte).frame_count(max_frames);
            for _ in 0..max_frames {
                valid_builder = valid_builder.add_frame(&[0xAA]);
            }
            let valid_data = valid_builder.build();
            let valid_packet = FramePacket::new(&valid_data)
                .expect("Packet at 120 ms boundary should be valid");
            assert_eq!(valid_packet.frames.len(), max_frames as usize);

            // Exceeding 120 ms (max_frames + 1) -> ExcessiveTotalDuration error
            let invalid_count = max_frames + 1;
            let mut invalid_builder = Packet::new(toc_byte).frame_count(invalid_count);
            for _ in 0..invalid_count {
                invalid_builder = invalid_builder.add_frame(&[0xAA]);
            }
            let invalid_data = invalid_builder.build();
            let invalid_res = FramePacket::new(&invalid_data);
            assert!(
                invalid_res.is_err(),
                "Config {config} with {invalid_count} frames should exceed 120 ms"
            );
        }
    }

    #[test]
    fn packet_with_extended_padding() {
        let toc_byte = 0b0000_0011;
        let frame_count = 1u8;
        let frame = [0xAA, 0xBB];

        let data = Packet::new(toc_byte)
            .frame_count(frame_count)
            .padding(300)
            .add_frame(&frame)
            .build();

        let packet = FramePacket::new(&data).expect("Failed to parse packet with 300-byte padding");
        assert_eq!(packet.frames.len(), 1);
        assert_eq!(packet.frames[0], &frame[..]);
        assert_eq!(packet.padding.unwrap().len(), 300);
        return;
    }

    #[test]
    fn code0_and_code1_zero_byte_dtx_frames() {
        let code0_dtx = [0x00u8];
        let pkt0 = FramePacket::new(&code0_dtx).expect("1-byte Code 0 DTX packet should be valid");
        assert_eq!(pkt0.frames.len(), 1);
        assert!(pkt0.frames[0].is_empty());
        assert!(pkt0.padding.is_none());

        let code1_dtx = [0x01u8];
        let pkt1 = FramePacket::new(&code1_dtx).expect("1-byte Code 1 DTX packet should be valid");
        assert_eq!(pkt1.frames.len(), 2);
        assert!(pkt1.frames[0].is_empty());
        assert!(pkt1.frames[1].is_empty());
        assert!(pkt1.padding.is_none());

        assert!(FramePacket::new(&[0x02u8]).is_err());
        assert!(FramePacket::new(&[0x03u8]).is_err());
        return;
    }

    #[test]
    fn self_delimited_multistream_packets() {
        let mut stream_buf = Vec::new();

        // Stream 0: Code 0 self-delimited (TOC=0x00, len=3, payload=[0x11, 0x22, 0x33])
        let s0_start = stream_buf.len();
        stream_buf.extend_from_slice(&[0x00, 3, 0x11, 0x22, 0x33]);
        let s0_expected_len = stream_buf.len() - s0_start;

        // Stream 1: Code 1 self-delimited (TOC=0x01, len=2 per frame, payloads=[0x44, 0x55], [0x66, 0x77])
        let s1_start = stream_buf.len();
        stream_buf.extend_from_slice(&[0x01, 2, 0x44, 0x55, 0x66, 0x77]);
        let s1_expected_len = stream_buf.len() - s1_start;

        // Stream 2: Code 2 self-delimited (TOC=0x02, n1=2, n2=3, payloads=[0x88, 0x99], [0xAA, 0xBB, 0xCC])
        let s2_start = stream_buf.len();
        stream_buf.extend_from_slice(&[0x02, 2, 3, 0x88, 0x99, 0xAA, 0xBB, 0xCC]);
        let s2_expected_len = stream_buf.len() - s2_start;

        // Stream 3: Code 3 CBR self-delimited with 2 bytes trailing padding
        // TOC=0x03, ch=(padding=1, m=3) = 0x43, pad_len=2, cbr_size=2, 3 frames of 2 bytes, 2 pad bytes
        let s3_start = stream_buf.len();
        stream_buf.extend_from_slice(&[
            0x03, 0x43, 2, 2, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0xF0, 0xF1,
        ]);
        let s3_expected_len = stream_buf.len() - s3_start;

        // Stream 4: Code 3 VBR self-delimited with 3 bytes trailing padding
        // TOC=0x03, ch=(vbr=1, padding=1, m=2) = 0xC2, pad_len=3, n1=1, n2=3, frames=[0x10], [0x20, 0x30, 0x40], pad=[0, 0, 0]
        let s4_start = stream_buf.len();
        stream_buf.extend_from_slice(&[
            0x03, 0xC2, 3, 1, 3, 0x10, 0x20, 0x30, 0x40, 0x00, 0x00, 0x00,
        ]);
        let s4_expected_len = stream_buf.len() - s4_start;

        // Final undelimited stream at end: Code 0 [0x00, 0x9A, 0xBC]
        stream_buf.extend_from_slice(&[0x00, 0x9A, 0xBC]);

        let mut cursor = &stream_buf[..];

        let (p0, c0) = FramePacket::parse_self_delimited(cursor).expect("Stream 0 failed");
        assert_eq!(c0, s0_expected_len);
        assert_eq!(p0.frames, vec![&[0x11, 0x22, 0x33][..]]);
        cursor = &cursor[c0..];

        let (p1, c1) = FramePacket::parse_self_delimited(cursor).expect("Stream 1 failed");
        assert_eq!(c1, s1_expected_len);
        assert_eq!(p1.frames, vec![&[0x44, 0x55][..], &[0x66, 0x77][..]]);
        cursor = &cursor[c1..];

        let (p2, c2) = FramePacket::parse_self_delimited(cursor).expect("Stream 2 failed");
        assert_eq!(c2, s2_expected_len);
        assert_eq!(
            p2.frames,
            vec![&[0x88, 0x99][..], &[0xAA, 0xBB, 0xCC][..]]
        );
        cursor = &cursor[c2..];

        let (p3, c3) = FramePacket::parse_self_delimited(cursor).expect("Stream 3 failed");
        assert_eq!(c3, s3_expected_len);
        assert_eq!(
            p3.frames,
            vec![&[0x01, 0x02][..], &[0x03, 0x04][..], &[0x05, 0x06][..]]
        );
        assert_eq!(p3.padding, Some(&[0xF0, 0xF1][..]));
        cursor = &cursor[c3..];

        let (p4, c4) = FramePacket::parse_self_delimited(cursor).expect("Stream 4 failed");
        assert_eq!(c4, s4_expected_len);
        assert_eq!(p4.frames, vec![&[0x10][..], &[0x20, 0x30, 0x40][..]]);
        assert_eq!(p4.padding, Some(&[0x00, 0x00, 0x00][..]));
        cursor = &cursor[c4..];

        let p_final = FramePacket::new(cursor).expect("Final stream failed");
        assert_eq!(p_final.frames, vec![&[0x9A, 0xBC][..]]);
        return;
    }
}
