//! Opus packet framing: how one packet carries one or more frames
//! ([RFC 6716 Section 3.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2>)).
//!
//! The TOC byte selects one of four framing codes. Codes 0 to 2 carry one or two frames whose
//! lengths follow from the packet length, Code 3 carries a signaled number of frames with
//! optional padding. Self-delimiting framing
//! ([Appendix B](<https://datatracker.ietf.org/doc/html/rfc6716#appendix-B>)) adds one extra
//! length field so that a packet can be found inside a larger buffer, which is how the Ogg
//! multistream layout concatenates the packets of every stream
//! ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)).
//!
//! ```text
//!   code   frames   lengths in the undelimited packet    extra length when self-delimited
//!   0      1        none (the rest of the packet)        N1
//!   1      2        none (two equal halves)              N (shared by both frames)
//!   2      2        N1                                   N2
//!   3      M        M - 1 (VBR) or none (CBR)            1 more (VBR) or 1 (CBR)
//! ```

use std::num::NonZeroU8;
use std::ops::Deref;
use std::time::Duration;

use thiserror::Error;

use crate::toc::{AudioMode, Bandwidth, FrameCountCode, FrameDuration, Toc};

/// Largest frame the length code can express, and the limit R2 of
/// [RFC 6716 Section 3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.4>).
const MAX_FRAME_LENGTH: usize = 255 * 4 + 255;
/// Longest audio one packet may carry, the limit R5 of
/// [RFC 6716 Section 3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.4>).
const MAX_PACKET_DURATION: Duration = Duration::from_millis(120);
/// Padding added by a `255` padding length byte, which also says "continue with the next byte".
const MAX_PADDING_VALUE: u8 = 254;
/// Most frames one packet can hold, `120 ms / 2.5 ms`
/// ([RFC 6716 Section 3.2.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.5>)).
pub(crate) const MAX_FRAMES: usize = 48;
/// Bit `v` of the Code 3 frame count byte.
const VBR_FLAG: u8 = 0b1000_0000;
/// Bit `p` of the Code 3 frame count byte.
const PADDING_FLAG: u8 = 0b0100_0000;
/// Field `M` of the Code 3 frame count byte.
const FRAME_COUNT_MASK: u8 = 0b0011_1111;
/// First byte value that announces a two-byte frame length.
const TWO_BYTE_LENGTH_START: u8 = 252;

/// Ways a packet can be malformed, each named after the requirement of
/// [RFC 6716 Section 3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.4>) it breaks.
#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum Error {
    /// The packet ends before its TOC byte, length fields, frames or padding do.
    #[error("packet shorter than its framing requires")]
    TooShort,

    /// A frame is longer than 1275 bytes.
    #[error("frame of {0} bytes exceeds the 1275 byte limit")]
    FrameTooLong(usize),

    /// A code 1 packet would split an odd number of payload bytes into two equal frames.
    #[error("code 1 packet payload has an odd length")]
    OddCode1Payload,

    /// A code 3 packet signals zero frames.
    #[error("code 3 packet signals zero frames")]
    ZeroFrameCount,

    /// The frames add up to more than 120 ms of audio.
    #[error("packet duration of {0:?} exceeds 120 ms")]
    DurationExceedsLimit(Duration),

    /// A code 3 CBR payload does not divide evenly into the signaled frames.
    #[error("code 3 CBR payload is not a multiple of the frame count")]
    UnevenCbrPayload,

    /// More than [`MAX_FRAMES`] frames in one packet.
    #[error("more than 48 frames in one packet")]
    TooManyFrames,
}

impl Error {
    /// Static message for `symphonia_core::errors::Error::DecodeError`, which carries a
    /// `&'static str`; per-value detail stays available through `Display` on this type.
    const fn message(&self) -> &'static str {
        return match self {
            Self::TooShort => "opus: packet shorter than its framing requires",
            Self::FrameTooLong(_) => "opus: frame exceeds the 1275 byte limit",
            Self::OddCode1Payload => "opus: code 1 packet payload has an odd length",
            Self::ZeroFrameCount => "opus: code 3 packet signals zero frames",
            Self::DurationExceedsLimit(_) => "opus: packet duration exceeds 120 ms",
            Self::UnevenCbrPayload => {
                "opus: code 3 CBR payload is not a multiple of the frame count"
            }
            Self::TooManyFrames => "opus: more than 48 frames in one packet",
        };
    }
}

impl From<Error> for symphonia_core::errors::Error {
    fn from(err: Error) -> Self {
        return symphonia_core::errors::Error::DecodeError(err.message());
    }
}

/// Whether a packet is alone in its buffer or followed by more data
/// ([RFC 6716 Appendix B](<https://datatracker.ietf.org/doc/html/rfc6716#appendix-B>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Framing {
    /// The packet ends where the buffer ends; the last frame takes the remaining bytes.
    Undelimited,
    /// Every frame length is coded, so the packet ends before the buffer does.
    SelfDelimited,
}

/// The frames of one packet, at most [`MAX_FRAMES`], without heap allocation
/// ([RFC 6716 Section 3.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2>)).
#[derive(Clone, Copy)]
pub(crate) struct Frames<'a> {
    slots: [&'a [u8]; MAX_FRAMES],
    len: usize,
}

impl<'a> Frames<'a> {
    /// An empty frame list.
    const fn new() -> Self {
        return Self { slots: [&[]; MAX_FRAMES], len: 0 };
    }

    /// Appends one frame.
    fn push(&mut self, frame: &'a [u8]) -> Result<(), Error> {
        let slot = self.slots.get_mut(self.len).ok_or(Error::TooManyFrames)?;
        *slot = frame;
        self.len += 1;
        return Ok(());
    }

    /// The frames in packet order.
    #[inline]
    pub(crate) fn as_slice(&self) -> &[&'a [u8]] {
        return &self.slots[..self.len];
    }
}

impl<'a> Deref for Frames<'a> {
    type Target = [&'a [u8]];

    fn deref(&self) -> &Self::Target {
        return self.as_slice();
    }
}

impl<'a, 'b> IntoIterator for &'b Frames<'a> {
    type Item = &'b &'a [u8];
    type IntoIter = std::slice::Iter<'b, &'a [u8]>;

    fn into_iter(self) -> Self::IntoIter {
        return self.as_slice().iter();
    }
}

impl std::fmt::Debug for Frames<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        return f.debug_list().entries(self.as_slice()).finish();
    }
}

#[cfg(test)]
impl<'a> PartialEq<Vec<&'a [u8]>> for Frames<'a> {
    fn eq(&self, other: &Vec<&'a [u8]>) -> bool {
        return self.as_slice() == other.as_slice();
    }
}

/// One parsed Opus packet: the TOC fields it applies to every frame, and the frames
/// ([RFC 6716 Section 3](<https://datatracker.ietf.org/doc/html/rfc6716#section-3>)).
#[derive(Debug)]
pub(crate) struct FramePacket<'a> {
    /// The `s` flag of the TOC byte: `false` for mono, `true` for stereo.
    pub(crate) stereo: bool,
    /// Operating mode of every frame in the packet.
    pub(crate) audio_mode: AudioMode,
    /// Audio bandwidth of every frame in the packet.
    pub(crate) bandwidth: Bandwidth,
    /// Duration of each frame in the packet.
    pub(crate) frame_duration: FrameDuration,
    /// The compressed frames in order.
    pub(crate) frames: Frames<'a>,
}

impl<'a> FramePacket<'a> {
    /// Parses a packet that fills `buf` exactly
    /// ([RFC 6716 Section 3.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2>)).
    pub(crate) fn new(buf: &'a [u8]) -> Result<Self, Error> {
        return Self::parse(buf, Framing::Undelimited).map(|(packet, _)| packet);
    }

    /// Parses a self-delimited packet at the start of `buf` and returns it with the number of
    /// bytes it occupied
    /// ([RFC 6716 Appendix B](<https://datatracker.ietf.org/doc/html/rfc6716#appendix-B>)).
    pub(crate) fn parse_self_delimited(buf: &'a [u8]) -> Result<(Self, usize), Error> {
        return Self::parse(buf, Framing::SelfDelimited);
    }

    /// Parses one packet from the start of `buf` under `framing` and returns it together with
    /// the number of bytes consumed
    /// ([RFC 6716 Section 3.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2>)).
    /// An undelimited packet always consumes the whole buffer.
    pub(crate) fn parse(buf: &'a [u8], framing: Framing) -> Result<(Self, usize), Error> {
        let (&toc_byte, mut cursor) = buf.split_first().ok_or(Error::TooShort)?;
        let toc = Toc::from(toc_byte);

        let frames = match toc.frame_count_code {
            FrameCountCode::Single => single_frame(&mut cursor, framing)?,
            FrameCountCode::TwoEqual => two_equal_frames(&mut cursor, framing)?,
            FrameCountCode::TwoVariable => two_variable_frames(&mut cursor, framing)?,
            FrameCountCode::Signaled => {
                signaled_frames(&mut cursor, framing, toc.frame_duration())?
            }
        };

        let packet = Self {
            stereo: toc.stereo,
            audio_mode: toc.audio_mode(),
            bandwidth: toc.bandwidth(),
            frame_duration: toc.frame_duration(),
            frames,
        };
        return Ok((packet, buf.len() - cursor.len()));
    }
}

/// Code 0: one frame
/// ([RFC 6716 Section 3.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.2>)).
///
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
fn single_frame<'a>(cursor: &mut &'a [u8], framing: Framing) -> Result<Frames<'a>, Error> {
    let frame = match framing {
        Framing::Undelimited => take_rest(cursor)?,
        Framing::SelfDelimited => {
            let len = take_length(cursor)?;
            take_frame(cursor, len)?
        }
    };
    let mut frames = Frames::new();
    frames.push(frame)?;
    return Ok(frames);
}

/// Code 1: two frames of equal size, so the payload length must be even (rule R3)
/// ([RFC 6716 Section 3.2.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.3>)).
///
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
fn two_equal_frames<'a>(cursor: &mut &'a [u8], framing: Framing) -> Result<Frames<'a>, Error> {
    let len = match framing {
        Framing::Undelimited => {
            if cursor.len() % 2 != 0 {
                return Err(Error::OddCode1Payload);
            }
            check_frame_length(cursor.len() / 2)?
        }
        Framing::SelfDelimited => take_length(cursor)?,
    };
    let mut frames = Frames::new();
    frames.push(take_frame(cursor, len)?)?;
    frames.push(take_frame(cursor, len)?)?;
    return Ok(frames);
}

/// Code 2: two frames, the first length coded, the second taking the rest (rule R4)
/// ([RFC 6716 Section 3.2.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.4>)).
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
///                         Figure 4: A Code 2 Packet
/// ```
fn two_variable_frames<'a>(cursor: &mut &'a [u8], framing: Framing) -> Result<Frames<'a>, Error> {
    let first_len = take_length(cursor)?;
    let mut frames = Frames::new();
    match framing {
        Framing::Undelimited => {
            frames.push(take_frame(cursor, first_len)?)?;
            frames.push(take_rest(cursor)?)?;
        }
        Framing::SelfDelimited => {
            let second_len = take_length(cursor)?;
            frames.push(take_frame(cursor, first_len)?)?;
            frames.push(take_frame(cursor, second_len)?)?;
        }
    }
    return Ok(frames);
}

/// Code 3: a signaled number of frames with optional padding (rules R5, R6, R7)
/// ([RFC 6716 Section 3.2.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.5>)).
///
/// ```text
///                             0
///                             0 1 2 3 4 5 6 7
///                            +-+-+-+-+-+-+-+-+
///                            |v|p|     M     |
///                            +-+-+-+-+-+-+-+-+
///
///                     Figure 5: The frame count byte
/// ```
///
/// With `v` set the packet is VBR and the frame lengths precede the frame data (all but the
/// last in an undelimited packet, all of them when self-delimited). With `v` clear the packet
/// is CBR: undelimited frames share the payload equally, a self-delimited packet codes the
/// common length once. With `p` set the padding length bytes follow the frame count byte and
/// that many padding bytes follow the frames.
fn signaled_frames<'a>(
    cursor: &mut &'a [u8],
    framing: Framing,
    frame_duration: FrameDuration,
) -> Result<Frames<'a>, Error> {
    let (&count_byte, rest) = cursor.split_first().ok_or(Error::TooShort)?;
    *cursor = rest;
    let vbr = count_byte & VBR_FLAG != 0;
    let padded = count_byte & PADDING_FLAG != 0;
    let count = NonZeroU8::new(count_byte & FRAME_COUNT_MASK).ok_or(Error::ZeroFrameCount)?;
    check_total_duration(count, frame_duration)?;
    let count = usize::from(count.get());

    let padding = if padded { take_padding_length(cursor)? } else { 0 };
    let payload_len = cursor.len().checked_sub(padding).ok_or(Error::TooShort)?;

    let frames = match (framing, vbr) {
        (Framing::Undelimited, true) => {
            let mut payload = &cursor[..payload_len];
            let frames = take_vbr_frames(&mut payload, count, framing)?;
            *cursor = &cursor[payload_len..];
            frames
        }
        (Framing::Undelimited, false) => {
            let payload = &cursor[..payload_len];
            *cursor = &cursor[payload_len..];
            split_cbr_payload(payload, count)?
        }
        (Framing::SelfDelimited, true) => take_vbr_frames(cursor, count, framing)?,
        (Framing::SelfDelimited, false) => {
            let len = take_length(cursor)?;
            let mut frames = Frames::new();
            for _ in 0..count {
                frames.push(take_frame(cursor, len)?)?;
            }
            frames
        }
    };

    take_frame(cursor, padding)?;
    return Ok(frames);
}

/// Reads the VBR frame lengths that precede the frame data, then the frames themselves. An
/// undelimited packet codes `count - 1` lengths and the last frame takes the remaining bytes.
fn take_vbr_frames<'a>(
    cursor: &mut &'a [u8],
    count: usize,
    framing: Framing,
) -> Result<Frames<'a>, Error> {
    let coded_lengths = match framing {
        Framing::Undelimited => count - 1,
        Framing::SelfDelimited => count,
    };
    let mut lengths = [0usize; MAX_FRAMES];
    for len in lengths.iter_mut().take(coded_lengths) {
        *len = take_length(cursor)?;
    }

    let mut frames = Frames::new();
    for &len in lengths.iter().take(coded_lengths) {
        frames.push(take_frame(cursor, len)?)?;
    }
    if framing == Framing::Undelimited {
        frames.push(take_rest(cursor)?)?;
    }
    return Ok(frames);
}

/// Splits an undelimited CBR payload into `count` equal frames (rule R6).
fn split_cbr_payload(payload: &[u8], count: usize) -> Result<Frames<'_>, Error> {
    if payload.len() % count != 0 {
        return Err(Error::UnevenCbrPayload);
    }
    let len = check_frame_length(payload.len() / count)?;
    let mut frames = Frames::new();
    let mut cursor = payload;
    for _ in 0..count {
        frames.push(take_frame(&mut cursor, len)?)?;
    }
    return Ok(frames);
}

/// Reads a one or two byte frame length
/// ([RFC 6716 Section 3.2.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.1>)).
/// The largest codable value is `MAX_FRAME_LENGTH`, so no further bound check is needed.
///
/// ```text
///   first byte   meaning
///   0            no frame (DTX or lost)
///   1...251      the length in bytes
///   252...255    the length is 4 * second_byte + first_byte  (252...1275 bytes)
/// ```
fn take_length(cursor: &mut &[u8]) -> Result<usize, Error> {
    let (&first, rest) = cursor.split_first().ok_or(Error::TooShort)?;
    *cursor = rest;
    if first < TWO_BYTE_LENGTH_START {
        return Ok(usize::from(first));
    }
    let (&second, rest) = cursor.split_first().ok_or(Error::TooShort)?;
    *cursor = rest;
    return Ok(usize::from(second) * 4 + usize::from(first));
}

/// Reads the padding length: every `255` adds 254 bytes and continues, the first other byte
/// terminates
/// ([RFC 6716 Section 3.2.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.5>)).
fn take_padding_length(cursor: &mut &[u8]) -> Result<usize, Error> {
    let terminator = cursor.iter().position(|&b| b != u8::MAX).ok_or(Error::TooShort)?;
    let total = terminator * usize::from(MAX_PADDING_VALUE) + usize::from(cursor[terminator]);
    *cursor = &cursor[terminator + 1..];
    return Ok(total);
}

/// Takes the next `len` bytes as one frame.
fn take_frame<'a>(cursor: &mut &'a [u8], len: usize) -> Result<&'a [u8], Error> {
    let (frame, rest) = cursor.split_at_checked(len).ok_or(Error::TooShort)?;
    *cursor = rest;
    return Ok(frame);
}

/// Takes every remaining byte as the last frame of an undelimited packet.
fn take_rest<'a>(cursor: &mut &'a [u8]) -> Result<&'a [u8], Error> {
    let len = check_frame_length(cursor.len())?;
    return take_frame(cursor, len);
}

/// Rejects frames above the 1275 byte limit (rule R2).
fn check_frame_length(len: usize) -> Result<usize, Error> {
    if len > MAX_FRAME_LENGTH {
        return Err(Error::FrameTooLong(len));
    }
    return Ok(len);
}

/// Rejects packets carrying more than 120 ms of audio (rule R5), which also bounds the frame count
/// by [`MAX_FRAMES`].
fn check_total_duration(count: NonZeroU8, frame_duration: FrameDuration) -> Result<(), Error> {
    let total = Duration::from(frame_duration) * u32::from(count.get());
    if total > MAX_PACKET_DURATION {
        return Err(Error::DurationExceedsLimit(total));
    }
    return Ok(());
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

        let packet_data = Packet::new(toc_byte).add_frame(&frame_data).build();

        let packet = FramePacket::new(&packet_data).expect("Failed to parse single-frame packet");

        assert_eq!(packet.frames.len(), 1);
        assert_eq!(packet.frames[0], &frame_data[..]);
    }

    #[test]
    fn two_equal_frames_packet() {
        let toc_byte = 0b0000_0001;
        let frame_data = [0xAA, 0xBB, 0xCC, 0xDD];

        let packet_data = Packet::new(toc_byte).add_frame(&frame_data).build();

        let packet =
            FramePacket::new(&packet_data).expect("Failed to parse two equal frames packet");

        assert_eq!(packet.frames.len(), 2);
        assert_eq!(packet.frames[0], &frame_data[0..2]);
        assert_eq!(packet.frames[1], &frame_data[2..4]);
    }

    #[test]
    fn two_different_frames_packet() {
        let toc_byte = 0b0000_0010;
        let frame_1 = [0xAA, 0xBB];
        let frame_2 = [0xCC, 0xDD, 0xEE];

        let frame_1_len = frame_1.len();
        assert!(frame_1_len < 252, "Frame length too long for test builder (max 251)");
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

        let packet = FramePacket::new(&data).expect("Failed to parse arbitrary frames VBR packet");

        assert_eq!(packet.frames.len(), 2);
        assert_eq!(packet.frames[0], &frame_1[..]);
        assert_eq!(packet.frames[1], &frame_2[..]);
    }

    #[test]
    fn packet_with_padding() {
        let toc_byte = 0b0000_0011;
        let frame_count = 1u8;
        let frame = [0xAA, 0xBB, 0xCC];

        let data =
            Packet::new(toc_byte).frame_count(frame_count).padding(2).add_frame(&frame).build();

        let packet = FramePacket::new(&data).expect("Failed to parse packet with padding");

        assert_eq!(packet.frames.len(), 1);
        assert_eq!(packet.frames[0], &frame[..]);
    }

    #[test]
    fn handle_invalid_packet_length() {
        let toc_byte = 0b0000_0001;
        let frame_data = [0xAA, 0xBB, 0xCC];

        let data = Packet::new(toc_byte).add_frame(&frame_data).build();

        let result = FramePacket::new(&data);

        assert_eq!(result.err(), Some(Error::OddCode1Payload));
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
        let cases: [(u8, u8); 6] = [(16, 48), (17, 24), (0, 12), (1, 6), (2, 3), (3, 2)];

        for (config, max_frames) in cases {
            let toc_byte = (config << 3) | 0b011; // Code 3

            // Exactly 120 ms -> valid
            let mut valid_builder = Packet::new(toc_byte).frame_count(max_frames);
            for _ in 0..max_frames {
                valid_builder = valid_builder.add_frame(&[0xAA]);
            }
            let valid_data = valid_builder.build();
            let valid_packet =
                FramePacket::new(&valid_data).expect("Packet at 120 ms boundary should be valid");
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

        let data =
            Packet::new(toc_byte).frame_count(frame_count).padding(300).add_frame(&frame).build();

        let packet = FramePacket::new(&data).expect("Failed to parse packet with 300-byte padding");
        assert_eq!(packet.frames.len(), 1);
        assert_eq!(packet.frames[0], &frame[..]);
        return;
    }

    #[test]
    fn code0_and_code1_zero_byte_dtx_frames() {
        let code0_dtx = [0x00u8];
        let pkt0 = FramePacket::new(&code0_dtx).expect("1-byte Code 0 DTX packet should be valid");
        assert_eq!(pkt0.frames.len(), 1);
        assert!(pkt0.frames[0].is_empty());

        let code1_dtx = [0x01u8];
        let pkt1 = FramePacket::new(&code1_dtx).expect("1-byte Code 1 DTX packet should be valid");
        assert_eq!(pkt1.frames.len(), 2);
        assert!(pkt1.frames[0].is_empty());
        assert!(pkt1.frames[1].is_empty());

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
        stream_buf
            .extend_from_slice(&[0x03, 0x43, 2, 2, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0xF0, 0xF1]);
        let s3_expected_len = stream_buf.len() - s3_start;

        // Stream 4: Code 3 VBR self-delimited with 3 bytes trailing padding
        // TOC=0x03, ch=(vbr=1, padding=1, m=2) = 0xC2, pad_len=3, n1=1, n2=3, frames=[0x10], [0x20, 0x30, 0x40], pad=[0, 0, 0]
        let s4_start = stream_buf.len();
        stream_buf
            .extend_from_slice(&[0x03, 0xC2, 3, 1, 3, 0x10, 0x20, 0x30, 0x40, 0x00, 0x00, 0x00]);
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
        assert_eq!(p2.frames, vec![&[0x88, 0x99][..], &[0xAA, 0xBB, 0xCC][..]]);
        cursor = &cursor[c2..];

        let (p3, c3) = FramePacket::parse_self_delimited(cursor).expect("Stream 3 failed");
        assert_eq!(c3, s3_expected_len);
        assert_eq!(p3.frames, vec![&[0x01, 0x02][..], &[0x03, 0x04][..], &[0x05, 0x06][..]]);
        cursor = &cursor[c3..];

        let (p4, c4) = FramePacket::parse_self_delimited(cursor).expect("Stream 4 failed");
        assert_eq!(c4, s4_expected_len);
        assert_eq!(p4.frames, vec![&[0x10][..], &[0x20, 0x30, 0x40][..]]);
        cursor = &cursor[c4..];

        let p_final = FramePacket::new(cursor).expect("Final stream failed");
        assert_eq!(p_final.frames, vec![&[0x9A, 0xBC][..]]);
        return;
    }

    /// RFC 6716 Section 3.2.5: a set padding flag requires at least one padding length byte, and a
    /// padding length byte of 255 requires a following byte, in both framings.
    #[test]
    fn code3_padding_flag_without_a_complete_padding_length_is_rejected() {
        assert!(FramePacket::new(&[0b0000_0011, 0b0100_0001]).is_err());
        assert!(FramePacket::parse_self_delimited(&[0b0000_0011, 0b0100_0001]).is_err());
        assert!(FramePacket::new(&[0b0000_0011, 0b0100_0001, 0xFF]).is_err());
        assert!(FramePacket::parse_self_delimited(&[0b0000_0011, 0b0100_0001, 0xFF]).is_err());
        return;
    }

    /// Converting a framing error into a `symphonia_core` error must not allocate or leak.
    #[test]
    fn conversion_to_core_error_uses_a_static_message() {
        let err: symphonia_core::errors::Error = Error::TooShort.into();
        assert!(matches!(
            err,
            symphonia_core::errors::Error::DecodeError(
                "opus: packet shorter than its framing requires"
            )
        ));
        return;
    }
    /// The frame list is bounded by RFC 6716 Section 3.4 [R5]; a 49th push is refused.
    #[test]
    fn frame_list_refuses_a_49th_frame() {
        let mut frames = Frames::new();
        for _ in 0..MAX_FRAMES {
            frames.push(&[]).unwrap();
        }
        assert_eq!(frames.push(&[]), Err(Error::TooManyFrames));
        assert_eq!(frames.len(), MAX_FRAMES);
    }
}
