// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Framing for the `data` chunk of a file that wraps a compressed elementary stream: AC-3
//! (`WAVE_FORMAT_DOLBY_AC3_SPDIF`, tag `0x2000`) and DTS, which the DTS CD format writes as 16-bit
//! stereo PCM.
//!
//! Frames are self-describing and variable-length, so each frame's length is computed from its own
//! header. Only the little part of the headers that is needed to find the frame boundaries and the
//! audio parameters is parsed; the tables are duplicated from the codec crates because a format
//! crate must not depend on a codec.

use std::io::{ErrorKind, Seek, SeekFrom};

use symphonia_common::frames::{
    AC3_SYNC, AudioFrame, DTS_SYNC_14_BE, DTS_SYNC_14_LE, DTS_SYNC_16_BE, DTS_SYNC_16_LE,
    parse_ac3, parse_dts,
};
use symphonia_core::errors::{Result, SeekErrorKind, seek_error};
use symphonia_core::io::{MediaSource, MediaSourceStream, ReadBytes, SeekBuffered};
use symphonia_core::packet::Packet;
use symphonia_core::units::{Duration, Timestamp};

/// The number of bytes of a frame that its header is parsed from.
const HEADER_LEN: usize = 16;

/// The kind of elementary stream that a `data` chunk holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Ac3,
    Dts,
}

fn parse(buf: &[u8], kind: Kind) -> Option<AudioFrame> {
    if buf.len() < HEADER_LEN {
        return None;
    }
    match kind {
        Kind::Ac3 => parse_ac3(buf),
        Kind::Dts => parse_dts(buf),
    }
}

/// Returns the kind of stream that starts with the given bytes, if any.
pub fn detect_dts(buf: &[u8]) -> bool {
    buf.len() >= HEADER_LEN
        && matches!(
            u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]),
            DTS_SYNC_16_BE | DTS_SYNC_16_LE | DTS_SYNC_14_BE | DTS_SYNC_14_LE
        )
        && parse_dts(buf).is_some()
}

fn is_sync(word: u32, kind: Kind) -> bool {
    match kind {
        Kind::Ac3 => word >> 16 == u32::from(AC3_SYNC),
        Kind::Dts => {
            matches!(word, DTS_SYNC_16_BE | DTS_SYNC_16_LE | DTS_SYNC_14_BE | DTS_SYNC_14_LE)
        }
    }
}

fn rewind(reader: &mut MediaSourceStream<'_>, pos: u64) -> Result<()> {
    if reader.seek_buffered(pos) != pos {
        return seek_error(SeekErrorKind::Unseekable);
    }
    Ok(())
}

/// Synchronises to and reads the next whole frame from the `data` chunk, stopping at `end_pos`.
/// Returns the frame's audio parameters and its bytes, or `None` at the end of the data chunk.
pub fn read_frame(
    reader: &mut MediaSourceStream<'_>,
    end_pos: u64,
    kind: Kind,
) -> Result<Option<(AudioFrame, Vec<u8>)>> {
    let mut sync = 0u32;
    let mut have = 0u32;

    loop {
        if reader.pos() >= end_pos {
            return Ok(None);
        }

        let byte = match reader.read_u8() {
            Ok(byte) => byte,
            Err(err) if err.kind() == ErrorKind::UnexpectedEof => return Ok(None),
            Err(err) => return Err(err.into()),
        };

        sync = (sync << 8) | u32::from(byte);
        have = (have + 1).min(4);
        if have < 4 || !is_sync(sync, kind) {
            continue;
        }

        let start_pos = reader.pos() - 4;

        let mut header = [0u8; HEADER_LEN];
        header[..4].copy_from_slice(&sync.to_be_bytes());
        let format = match reader.read_buf_exact(&mut header[4..]) {
            Ok(()) => parse(&header, kind),
            Err(err) if err.kind() == ErrorKind::UnexpectedEof => None,
            Err(err) => return Err(err.into()),
        };

        if let Some(format) = format {
            rewind(reader, start_pos)?;

            let mut frame = vec![0u8; format.len];
            let complete = if (format.len as u64) > end_pos - start_pos {
                false
            }
            else {
                match reader.read_buf_exact(&mut frame) {
                    Ok(()) => true,
                    Err(err) if err.kind() == ErrorKind::UnexpectedEof => false,
                    Err(err) => return Err(err.into()),
                }
            };

            if complete {
                let frame_end_pos = reader.pos();
                // Confirm another frame at the boundary, unless it is the last one.
                let confirmed = if end_pos - frame_end_pos < 4 {
                    true
                }
                else {
                    match reader.read_be_u32() {
                        Ok(word) => is_sync(word, kind),
                        Err(err) if err.kind() == ErrorKind::UnexpectedEof => true,
                        Err(err) => return Err(err.into()),
                    }
                };
                if confirmed {
                    rewind(reader, frame_end_pos)?;
                    return Ok(Some((format, frame)));
                }
            }
        }

        // The candidate may contain the actual sync word, so resume one byte after its start.
        rewind(reader, start_pos + 1)?;
        sync = 0;
        have = 0;
    }
}

/// Seeks by counting the samples of the frames. Returns the timestamp of the frame that holds
/// `required_ts`, with the reader positioned at the start of that frame.
pub fn seek(
    reader: &mut MediaSourceStream<'_>,
    end_pos: u64,
    first_frame_pos: u64,
    next_ts: &mut u64,
    required_ts: u64,
    kind: Kind,
) -> Result<u64> {
    if reader.is_seekable() {
        if reader.seek(SeekFrom::Start(first_frame_pos))? != first_frame_pos {
            return seek_error(SeekErrorKind::Unseekable);
        }
        *next_ts = 0;
    }
    else if required_ts < *next_ts {
        return seek_error(SeekErrorKind::ForwardOnly);
    }

    loop {
        let Some((format, frame)) = read_frame(reader, end_pos, kind)?
        else {
            return seek_error(SeekErrorKind::OutOfRange);
        };
        let pos = reader.pos() - frame.len() as u64;
        let frame_end_ts = next_ts.saturating_add(format.samples_per_frame);
        if required_ts < frame_end_ts {
            rewind(reader, pos)?;
            return Ok(*next_ts);
        }
        *next_ts = frame_end_ts;
    }
}

/// Reads the next frame from the `data` chunk and wraps it in a [`Packet`], advancing `next_ts` by
/// the frame's sample count. Returns `None` at the end of the data chunk.
pub fn next_packet(
    reader: &mut MediaSourceStream<'_>,
    end_pos: u64,
    next_ts: &mut u64,
    kind: Kind,
) -> Result<Option<Packet>> {
    let Some((format, frame)) = read_frame(reader, end_pos, kind)?
    else {
        return Ok(None);
    };

    let pts = match Timestamp::try_from(*next_ts) {
        Ok(pts) => pts,
        Err(_) => return Ok(None),
    };
    let dur = Duration::from(format.samples_per_frame);
    *next_ts = next_ts.saturating_add(format.samples_per_frame);

    Ok(Some(Packet::new(0, pts, dur, frame)))
}
