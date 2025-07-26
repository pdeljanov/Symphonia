// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::ReadBytes;
use symphonia_core::util::bits;

use crate::atoms::{Atom, AtomHeader};

/// Edit list entry.
#[allow(dead_code)]
#[derive(Debug)]
pub struct ElstEntry {
    segment_duration: u64,
    media_time: i64,
    media_rate_int: i16,
    media_rate_frac: i16,
}

/// Edit list atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct ElstAtom {
    entries: Vec<ElstEntry>,
}

impl Atom for ElstAtom {
    fn read<B: ReadBytes>(reader: &mut B, mut header: AtomHeader) -> Result<Self> {
        let (version, _) = header.read_extended_header(reader)?;

        // minimum data size is 4 bytes
        let len = match header.data_len() {
            Some(len) if len >= 4 => len as u32,
            Some(_) => return decode_error("isomp4 (elst): atom size is less than 16 bytes"),
            None => return decode_error("isomp4 (elst): expected atom size to be known"),
        };

        let entry_count = reader.read_be_u32()?;
        let entry_len = if version == 0 { 12 } else { 20 };
        if entry_count != (len - 4) / entry_len {
            return decode_error("isomp4 (elst): invalid entry count");
        }

        // TODO: Apply a limit.
        let mut entries = Vec::new();

        for _ in 0..entry_count {
            let (segment_duration, media_time) = match version {
                0 => (
                    u64::from(reader.read_be_u32()?),
                    i64::from(bits::sign_extend_leq32_to_i32(reader.read_be_u32()?, 32)),
                ),
                1 => (
                    reader.read_be_u64()?,
                    bits::sign_extend_leq64_to_i64(reader.read_be_u64()?, 64),
                ),
                _ => return decode_error("isomp4 (elst): invalid version"),
            };

            let media_rate_int = bits::sign_extend_leq16_to_i16(reader.read_be_u16()?, 16);
            let media_rate_frac = bits::sign_extend_leq16_to_i16(reader.read_be_u16()?, 16);

            entries.push(ElstEntry {
                segment_duration,
                media_time,
                media_rate_int,
                media_rate_frac,
            });
        }

        Ok(ElstAtom { entries })
    }
}
