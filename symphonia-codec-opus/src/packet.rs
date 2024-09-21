use crate::toc::{FrameCount, Toc};
use std::num::NonZeroU8;
use std::time::Duration;
use symphonia_core::errors::{Error, Result};
use symphonia_core::io::{BitReaderLtr, ReadBitsLtr};

const MAX_FRAME_LENGTH: usize = 255 * 4 + 255;
const MAX_TOTAL_DURATION_MS: u128 = 120;
const MAX_PADDING_VALUE: usize = 254;

pub struct FramePacket<'a> {
    toc: Toc,
    frames: Vec<&'a [u8]>,
    padding: Option<&'a [u8]>,
}

impl<'a> FramePacket<'a> {
    /// ## 3.2 Frame Packing
    ///
    /// This section describes how frames are packed according to each
    /// possible value of "c" in the TOC byte.
    ///
    pub fn new(buf: &'a [u8]) -> Result<Self> {
        let (toc_byte, frames) = buf.split_first()
            .ok_or_else(|| Error::DecodeError("Packet is too short"))?;

        if frames.is_empty() {
            return Err(Error::DecodeError("Empty frame"));
        }

        let toc = Toc::new(*toc_byte)?;

        match toc.frame_count() {
            FrameCount::One => Self::one(frames, toc),
            FrameCount::TwoEqual => Self::two_equal(frames, toc),
            FrameCount::TwoDifferent => Self::two_different(frames, toc),
            FrameCount::Arbitrary => Self::arbitrary(frames, toc),
        }
    }
    /// ### 3.2.1 Frame Length Coding
    ///
    /// When a packet contains multiple VBR frames (i.e., code 2 or 3), the
    /// compressed length of one or more of these frames is indicated with a
    /// one- or two-byte sequence, with the meaning of the first byte as
    /// follows:
    ///
    /// - `0`: No frame (Discontinuous Transmission (DTX) or lost packet)
    ///
    /// - `1...251`: Length of the frame in bytes
    ///
    /// - `252...255`: A second byte is needed. The total length is
    ///   `(second_byte * 4) + first_byte`
    ///
    /// The special length `0` indicates that no frame is available, either
    /// because it was dropped during transmission by some intermediary or
    /// because the encoder chose not to transmit it. Any Opus frame in any
    /// mode MAY have a length of `0`.
    ///
    /// The maximum representable length is `255 * 4 + 255 = 1275` bytes. For
    /// 20 ms frames, this represents a bitrate of 510 kbit/s, which is
    /// approximately the highest useful rate for lossily compressed fullband
    /// stereo music. Beyond this point, lossless codecs are more
    /// appropriate. It is also roughly the maximum useful rate of the MDCT
    /// layer as, shortly thereafter, quality no longer improves with
    /// additional bits due to limitations on the codebook sizes.
    ///
    /// No length is transmitted for the last frame in a VBR packet, or for
    /// any of the frames in a CBR packet, as it can be inferred from the
    /// total size of the packet and the size of all other data in the
    /// packet. However, the length of any individual frame MUST NOT exceed
    /// 1275 bytes to allow for repacketization by gateways, conference
    /// bridges, or other software.
    pub fn frame_length_coding() {}

    /// ### 3.2.2 Code 0: One Frame in the Packet
    ///
    /// For code `0` packets, the TOC byte is immediately followed by `N-1` bytes
    /// of compressed data for a single frame (where `N` is the size of the
    /// packet), as illustrated in Figure 2.
    ///
    /// ```text
    ///    0                   1                   2                   3
    ///    0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
    ///   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    ///   | config  |s|0|0|                                               |
    ///   +-+-+-+-+-+-+-+-+                                               |
    ///   |                    Compressed frame 1 (N-1 bytes)...          :
    ///   :                                                               |
    ///   |                                                               |
    ///   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    /// ```
    pub fn one(data: &'a [u8], toc: Toc) -> Result<Self> {
        Self::check_frame_size(data.len())?;

        return Ok(Self {
            toc,
            frames: vec![data],
            padding: None,
        });
    }

    /// ### 3.2.3 Code 1: Two Frames in the Packet, Each with Equal Compressed Size
    ///
    /// For code `1` packets, the TOC byte is immediately followed by the
    /// `(N-1)/2` bytes of compressed data for the first frame, followed by
    /// `(N-1)/2` bytes of compressed data for the second frame, as illustrated
    /// in Figure 3. The number of payload bytes available for compressed
    /// data, `N-1`, MUST be even for all code `1` packets.
    ///
    /// ```text
    ///    0                   1                   2                   3
    ///    0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
    ///   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    ///   | config  |s|0|1|                                               |
    ///   +-+-+-+-+-+-+-+-+                                               :
    ///   |             Compressed frame 1 ((N-1)/2 bytes)...             |
    ///   :                               +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    ///   |                               |                               |
    ///   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+                               :
    ///   |             Compressed frame 2 ((N-1)/2 bytes)...             |
    ///   :                                               +-+-+-+-+-+-+-+-+
    ///   |                                               |
    ///   +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
    /// ```
    pub fn two_equal(data: &'a [u8], toc: Toc) -> Result<Self> {
        if data.len() % 2 != 0 {
            return Err(Error::DecodeError("Invalid packet length"));
        }

        let frame_size = data.len() / 2;
        Self::check_frame_size(frame_size)?;

        let frames = data
            .chunks(frame_size)
            .collect::<Vec<&'a [u8]>>();

        return Ok(
            Self {
                toc,
                frames,
                padding: None,
            }
        );
    }


    /// ### 3.2.4 Code 2: Two Frames in the Packet, with Different Compressed Sizes
    ///
    /// For code `2` packets, the TOC byte is followed by a one- or two-byte
    /// sequence indicating the length of the first frame (marked `N1` in
    /// Figure 4), followed by `N1` bytes of compressed data for the first
    /// frame. The remaining `N-N1-2` or `N-N1-3` bytes are the compressed data
    /// for the second frame. This is illustrated in Figure 4. A code `2`
    /// packet MUST contain enough bytes to represent a valid length.
    ///
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
    /// ```
    pub fn two_different(data: &'a [u8], toc: Toc) -> Result<Self> {
        let (n1, offset) = Self::get_frame_length(data)?;

        if data.len() < offset + n1 {
            return Err(Error::DecodeError("First frame length exceeds available data"));
        }

        let frame_1_end = offset + n1;
        let frame_1 = &data[offset..frame_1_end];
        let frame_2 = &data[frame_1_end..];

        let frames = vec![frame_1, frame_2];

        Self::check_frame_size(frame_1.len())?;
        Self::check_frame_size(frame_2.len())?;

        return Ok(Self {
            toc,
            frames,
            padding: None,
        });
    }

    /// ### 3.2.5 Code 3: A Signaled Number of Frames in the Packet
    ///
    /// Code `3` packets signal the number of frames, as well as additional
    /// padding, called "Opus padding" to indicate that this padding is added
    /// at the Opus layer rather than at the transport layer. Code `3` packets
    /// MUST have at least 2 bytes. The TOC byte is followed by a
    /// byte encoding the number of frames in the packet in bits 2 to 7
    /// (marked `M` in Figure 5), with bit 1 indicating whether or not Opus
    /// padding is inserted (marked `p` in Figure 5), and bit 0 indicating
    /// VBR (marked `v` in Figure 5). `M` MUST NOT be zero, and the audio
    /// duration contained within a packet MUST NOT exceed 120 ms. This
    /// limits the maximum frame count for any frame size to 48 (for 2.5 ms
    /// frames), with lower limits for longer frame sizes. Figure 5
    /// illustrates the layout of the frame count byte.
    ///
    /// ```text
    ///                         0
    ///                         0 1 2 3 4 5 6 7
    ///                        +-+-+-+-+-+-+-+-+
    ///                        |v|p|     M     |
    ///                        +-+-+-+-+-+-+-+-+
    /// ```
    pub fn arbitrary(data: &'a [u8], toc: Toc) -> Result<Self> {
        let frame_count_byte = data.first()
            .ok_or_else(|| Error::DecodeError("Failed to read frame count byte"))?;

        let buf = [*frame_count_byte];
        let mut reader = BitReaderLtr::new(&buf);

        let vbr = reader.read_bit()? != 0;
        let padding_flag = reader.read_bit()? != 0;
        let m = reader.read_bits_leq32(6)? as u8;

        Self::check_total_duration(m, &toc)?;

        let mut offset = 1;

        let (padding_length, padding_offset) = if padding_flag {
            Self::get_padding_length(&data[offset..])?
        } else {
            (0usize, 0usize)
        };

        offset += padding_offset;

        let data_end = data.len().checked_sub(padding_length)
            .ok_or_else(|| Error::DecodeError("Insufficient data after accounting for padding"))?;

        let frames_data = &data[offset..data_end];


        let frame_count = NonZeroU8::new(m)
            .ok_or_else(|| Error::DecodeError("Number of frames can't be zero"))?;

        let frames = if vbr {
            Self::get_vbr_frames(frames_data, frame_count.get())?
        } else {
            Self::get_cbr_frames(frames_data, frame_count)?
        };

        let padding = if padding_length > 0 {
            Some(&data[data_end..])
        } else {
            None
        };

        Ok(Self {
            toc,
            frames,
            padding,
        })
    }

    fn get_vbr_frames(data: &'a [u8], frame_count: u8) -> Result<Vec<&'a [u8]>> {
        let mut frames = Vec::with_capacity(frame_count as usize);
        let mut offset = 0usize;

        let mut frame_lengths = Vec::with_capacity((frame_count - 1) as usize);
        for _ in 0..(frame_count - 1) {
            let (frame_len, len_offset) = Self::get_frame_length(&data[offset..])?;
            offset += len_offset;
            frame_lengths.push(frame_len);
        }

        for frame_len in frame_lengths {
            let frame_end = offset + frame_len;
            let frame = data.get(offset..frame_end)
                .ok_or_else(|| Error::DecodeError("Frame length exceeds data size [R7]"))?;
            Self::check_frame_size(frame.len())?;
            frames.push(frame);
            offset = frame_end;
        }

        let last_frame = data.get(offset..)
            .ok_or_else(|| Error::DecodeError("No data for last frame"))?;
        Self::check_frame_size(last_frame.len())?;
        frames.push(last_frame);

        return Ok(frames);
    }
    fn get_cbr_frames(data: &'a [u8], frame_count: NonZeroU8) -> Result<Vec<&'a [u8]>> {
        let frame_count_usize = frame_count.get() as usize;
        let total_length = data.len();

        let frame_size = Self::calculate_frame_size(total_length, frame_count_usize)?;
        Self::check_frame_size(frame_size)?;

        return data.chunks_exact(frame_size).map(Ok).collect();
    }

    fn get_padding_length(data: &'a [u8]) -> Result<(usize, usize)> {
        let mut total_padding = 0usize;
        let mut offset = 0usize;

        for &byte in data {
            offset += 1;
            if byte == u8::MAX {
                total_padding = total_padding.checked_add(MAX_PADDING_VALUE)
                    .ok_or_else(|| Error::DecodeError("Padding length overflow"))?;
            } else {
                total_padding = total_padding.checked_add(byte as usize)
                    .ok_or_else(|| Error::DecodeError("Padding length overflow"))?;
                break;
            }
        }

        return Ok((total_padding, offset));
    }

    fn get_frame_length(data: &'a [u8]) -> Result<(usize, usize)> {
        let (first_byte, mut offset) = Self::consume_byte(data)?;

        return match first_byte {
            0 => Ok((0, offset)),
            1..=251 => Ok((first_byte as usize, offset)),
            252..=255 => {
                let (second_byte, second_offset) = Self::consume_byte(&data[offset..])?;
                offset += second_offset;
                let length = ((second_byte as usize) * 4) + (first_byte as usize);
                Ok((length, offset))
            }
            _ => unreachable!()
        };
    }

    fn consume_byte(data: &'a [u8]) -> Result<(u8, usize)> {
        return data.split_first()
            .map(|(&byte, _)| (byte, 1))
            .ok_or_else(|| Error::DecodeError("Insufficient data"));
    }

    fn calculate_frame_size(total_length: usize, frame_count: usize) -> Result<usize> {
        return total_length
            .checked_div(frame_count)
            .filter(|&size| size * frame_count == total_length)
            .ok_or_else(|| Error::DecodeError("Invalid frame length"));
    }

    fn check_total_duration(frame_count: u8, toc: &Toc) -> Result<()> {
        let params = toc.params()?;

        let total_duration_ms = Duration::from(params.frame_size).as_millis()
            .checked_mul(frame_count as u128)
            .ok_or_else(|| Error::DecodeError("Total duration overflow"))?;

        if total_duration_ms > MAX_TOTAL_DURATION_MS {
            return Err(Error::DecodeError("Total audio duration exceeds 120 ms [R5]"));
        }

        return Ok(());
    }

    fn check_frame_size(size: usize) -> Result<()> {
        if size > MAX_FRAME_LENGTH {
            return Err(Error::DecodeError("Frame length exceeds maximum allowed size [R2]"));
        }

        return Ok(());
    }
    /// Returns the parsed TOC.
    pub fn toc(&self) -> Toc {
        return self.toc;
    }

    /// Returns the vector of frames.
    pub fn frames(&self) -> &Vec<&'a [u8]> {
        return &self.frames;
    }

    /// Returns the padding data, if any.
    pub fn padding(&self) -> Option<&'a [u8]> {
        return self.padding;
    }
}


