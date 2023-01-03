// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{HashMap, VecDeque};

use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::{BufReader, ReadBytes};

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

fn read_ebml_sizes<R: ReadBytes>(mut reader: R, frames: usize) -> Result<Vec<u64>> {
    let mut sizes = Vec::new();
    for _ in 0..frames {
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

pub(crate) fn read_xiph_sizes<R: ReadBytes>(mut reader: R, frames: usize) -> Result<Vec<u64>> {
    let mut prefixes = 0;
    let mut sizes = Vec::new();
    while sizes.len() < frames {
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
    pub(crate) timestamp: u64,
    pub(crate) duration: u64,
    pub(crate) data: Box<[u8]>,
}

pub(crate) fn calc_abs_block_timestamp(cluster_ts: u64, rel_block_ts: i16) -> u64 {
    if rel_block_ts < 0 {
        cluster_ts - (-rel_block_ts) as u64
    }
    else {
        cluster_ts + rel_block_ts as u64
    }
}

pub(crate) fn extract_frames(
    block: &[u8],
    block_duration: Option<u64>,
    tracks: &HashMap<u32, TrackState>,
    cluster_timestamp: u64,
    timestamp_scale: u64,
    buffer: &mut VecDeque<Frame>,
) -> Result<()> {
    let mut reader = BufReader::new(block);
    let track = read_unsigned_vint(&mut reader)? as u32;
    let rel_ts = reader.read_be_u16()? as i16;
    let flags = reader.read_byte()?;
    let lacing = parse_flags(flags)?;

    let default_frame_duration =
        tracks.get(&track).and_then(|it| it.default_frame_duration).map(|it| it / timestamp_scale);

    let mut timestamp = calc_abs_block_timestamp(cluster_timestamp, rel_ts);

    match lacing {
        Lacing::None => {
            let data = reader.read_boxed_slice_exact(block.len() - reader.pos() as usize)?;
            let duration = block_duration.or(default_frame_duration).unwrap_or(0);
            buffer.push_back(Frame { track, timestamp, data, duration });
        }
        Lacing::Xiph | Lacing::Ebml => {
            // Read number of stored sizes which is actually `number of frames` - 1
            // since size of the last frame is deduced from block size.
            let frames = reader.read_byte()? as usize;
            let sizes = match lacing {
                Lacing::Xiph => read_xiph_sizes(&mut reader, frames)?,
                Lacing::Ebml => read_ebml_sizes(&mut reader, frames)?,
                _ => unreachable!(),
            };

            let frame_duration = block_duration
                .map(|it| it / (frames + 1) as u64)
                .or(default_frame_duration)
                .unwrap_or(0);

            for frame_size in sizes {
                let data = reader.read_boxed_slice_exact(frame_size as usize)?;
                buffer.push_back(Frame { track, timestamp, data, duration: frame_duration });
                timestamp += frame_duration;
            }

            // Size of last frame is not provided so we read to the end of the block.
            let size = block.len() - reader.pos() as usize;
            let data = reader.read_boxed_slice_exact(size)?;
            buffer.push_back(Frame { track, timestamp, data, duration: frame_duration });
        }
        Lacing::FixedSize => {
            let frames = reader.read_byte()? as usize + 1;
            let total_size = block.len() - reader.pos() as usize;
            if total_size % frames != 0 {
                return decode_error("mkv: invalid block size");
            }

            let frame_duration =
                block_duration.map(|it| it / frames as u64).or(default_frame_duration).unwrap_or(0);

            let frame_size = total_size / frames;
            for _ in 0..frames {
                let data = reader.read_boxed_slice_exact(frame_size)?;
                buffer.push_back(Frame { track, timestamp, data, duration: frame_duration });
                timestamp += frame_duration;
            }
        }
    }

    Ok(())
}
