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
mod tests;
