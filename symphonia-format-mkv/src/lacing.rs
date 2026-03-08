// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{HashMap, VecDeque};

use symphonia_core::errors::{Result, decode_error};
use symphonia_core::io::{BufReader, ReadBytes};
use symphonia_core::units::{Duration, Timestamp};

use crate::demuxer::TrackState;
use crate::ebml::{read_signed_vint, read_unsigned_vint};

enum Lacing {
    None,
    Xiph,
    FixedSize,
    Ebml,
}

fn parse_flags(flags: u8) -> Result<Lacing> {
    match (flags >> 1) & 0b11 {
        0b00 => Ok(Lacing::None),
        0b01 => Ok(Lacing::Xiph),
        0b10 => Ok(Lacing::FixedSize),
        0b11 => Ok(Lacing::Ebml),
        _ => unreachable!(),
    }
}

fn read_ebml_sizes<R: ReadBytes>(mut reader: R, num_frames: usize) -> Result<Vec<u64>> {
    let mut sizes = Vec::with_capacity(num_frames);
    for _ in 0..num_frames {
        if let Some(last_size) = sizes.last().copied() {
            let delta = read_signed_vint(&mut reader)?;
            sizes.push((last_size as i64 + delta) as u64)
        }
        else {
            let size = read_unsigned_vint(&mut reader)?;
            sizes.push(size);
        }
    }

    Ok(sizes)
}

pub(crate) fn read_xiph_sizes<R: ReadBytes>(mut reader: R, num_frames: usize) -> Result<Vec<u64>> {
    let mut sizes = Vec::with_capacity(num_frames);
    let mut prefixes = 0;
    while sizes.len() < num_frames {
        let byte = reader.read_byte()? as u64;
        if byte == 255 {
            prefixes += 1;
        }
        else {
            let size = prefixes * 255 + byte;
            prefixes = 0;
            sizes.push(size);
        }
    }

    Ok(sizes)
}

pub(crate) struct Frame {
    pub(crate) track: u32,
    /// Absolute frame timestamp.
    pub(crate) pts: Timestamp,
    pub(crate) duration: Duration,
    pub(crate) data: Box<[u8]>,
}

pub(crate) fn calc_abs_block_timestamp(
    cluster_ts: u64,
    rel_block_ts: i16,
    codec_delay: u64,
) -> Option<Timestamp> {
    let cluster_ts: i64 = cluster_ts.try_into().ok()?;
    cluster_ts
        .checked_add(i64::from(rel_block_ts))
        .and_then(|ts| ts.checked_sub_unsigned(codec_delay))
        .map(Timestamp::from)
}

pub(crate) fn extract_frames(
    block: &[u8],
    block_duration: Option<u64>,
    tracks: &HashMap<u32, TrackState>,
    cluster_timestamp: u64,
    timestamp_scale: u64,
    frames: &mut VecDeque<Frame>,
) -> Result<bool> {
    let mut reader = BufReader::new(block);
    let track = read_unsigned_vint(&mut reader)? as u32;
    let rel_ts = reader.read_be_u16()? as i16;
    let flags = reader.read_byte()?;
    let lacing = parse_flags(flags)?;

    let (default_frame_duration, codec_delay) = match tracks.get(&track) {
        Some(t) => (t.default_frame_duration.map(|d| d / timestamp_scale), t.codec_delay),
        None => (None, 0),
    };

    let mut pts = match calc_abs_block_timestamp(cluster_timestamp, rel_ts, codec_delay) {
        Some(pts) => pts,
        _ => return Ok(false),
    };

    match lacing {
        Lacing::None => {
            let data = reader.read_boxed_slice_exact(block.len() - reader.pos() as usize)?;
            let duration = Duration::from(block_duration.or(default_frame_duration).unwrap_or(0));
            frames.push_back(Frame { track, pts, data, duration });
        }
        Lacing::Xiph | Lacing::Ebml => {
            // Read number of stored sizes which is actually `number of frames` - 1
            // since size of the last frame is deduced from block size.
            let num_frames = reader.read_byte()? as usize;
            let sizes = match lacing {
                Lacing::Xiph => read_xiph_sizes(&mut reader, num_frames)?,
                Lacing::Ebml => read_ebml_sizes(&mut reader, num_frames)?,
                _ => unreachable!(),
            };

            let frame_duration = block_duration
                .map(|it| it / (num_frames + 1) as u64)
                .or(default_frame_duration)
                .map(Duration::from)
                .unwrap_or_default();

            for frame_size in sizes {
                let data = reader.read_boxed_slice_exact(frame_size as usize)?;
                frames.push_back(Frame { track, pts, data, duration: frame_duration });

                // If PTS overflows, end the stream.
                pts = match pts.checked_add(frame_duration) {
                    Some(pts) => pts,
                    None => return Ok(false),
                };
            }

            // Size of last frame is not provided so we read to the end of the block.
            let size = block.len() - reader.pos() as usize;
            let data = reader.read_boxed_slice_exact(size)?;
            frames.push_back(Frame { track, pts, data, duration: frame_duration });
        }
        Lacing::FixedSize => {
            let num_frames = reader.read_byte()? as usize + 1;
            let total_size = block.len() - reader.pos() as usize;
            if total_size % num_frames != 0 {
                return decode_error("mkv: invalid block size");
            }

            let frame_duration = block_duration
                .map(|it| it / num_frames as u64)
                .or(default_frame_duration)
                .map(Duration::from)
                .unwrap_or_default();

            let frame_size = total_size / num_frames;
            for _ in 0..num_frames {
                let data = reader.read_boxed_slice_exact(frame_size)?;
                frames.push_back(Frame { track, pts, data, duration: frame_duration });

                // If PTS overflows, end the stream.
                pts = match pts.checked_add(frame_duration) {
                    Some(pts) => pts,
                    None => return Ok(false),
                };
            }
        }
    }

    Ok(true)
}
