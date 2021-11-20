use std::collections::VecDeque;

use symphonia_core::errors::{Result, decode_error};
use symphonia_core::io::{BufReader, ReadBytes};

use crate::ebml::{read_vint, read_vint_signed};

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
            let delta = read_vint_signed(&mut reader)?;
            sizes.push((last_size as i64 + delta) as u64)
        } else {
            let size = read_vint::<_, true>(&mut reader)?;
            sizes.push(size);
        }
    }

    Ok(sizes)
}

pub(crate) fn read_xiph_sizes<R: ReadBytes>(mut reader: R, frames: usize) -> Result<Vec<u64>> {
    let mut prefixes = 0;
    let mut sizes = Vec::new();
    while sizes.len() < frames as usize {
        let byte = reader.read_byte()? as u64;
        if byte == 255 {
            prefixes += 1;
        } else {
            let size = prefixes * 255 + byte;
            prefixes = 0;
            sizes.push(size);
        }
    }

    Ok(sizes)
}

pub(crate) struct Frame {
    pub(crate) track: u32,
    /// Frame timestamp (relative to Cluster timestamp).
    pub(crate) timestamp: i16,
    pub(crate) data: Box<[u8]>,
}

pub(crate) fn extract_frames(block: &[u8], buffer: &mut VecDeque<Frame>) -> Result<()> {
    let mut reader = BufReader::new(&block);
    let track = read_vint::<_, true>(&mut reader)? as u32;
    let timestamp = reader.read_be_u16()? as i16;
    let flags = reader.read_byte()?;
    let lacing = parse_flags(flags)?;
    match lacing {
        Lacing::None => {
            let data = reader.read_boxed_slice_exact(block.len() - reader.pos() as usize)?;
            buffer.push_back(Frame { track, timestamp, data });
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

            for frame_size in sizes {
                let data = reader.read_boxed_slice_exact(frame_size as usize)?;
                buffer.push_back(Frame { track, timestamp, data });
            }

            // Size of last frame is not provided so we read to the end of the block.
            let size = block.len() - reader.pos() as usize;
            let data = reader.read_boxed_slice_exact(size)?;
            buffer.push_back(Frame { track, timestamp, data });
        }
        Lacing::FixedSize => {
            let frames = reader.read_byte()? as usize + 1;
            let total_size = block.len() - reader.pos() as usize;
            if total_size % frames != 0 {
                return decode_error("mkv: invalid block size");
            }

            let frame_size = total_size / frames;
            for _ in 0..frames {
                let data = reader.read_boxed_slice_exact(frame_size)?;
                buffer.push_back(Frame { track, timestamp, data });
            }
        }
    }

    Ok(())
}