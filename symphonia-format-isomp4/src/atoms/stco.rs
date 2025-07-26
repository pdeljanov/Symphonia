// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::ReadBytes;

use crate::atoms::{Atom, AtomHeader};

/// Chunk offset atom (32-bit version).
#[allow(dead_code)]
#[derive(Debug)]
pub struct StcoAtom {
    pub chunk_offsets: Vec<u32>,
}

impl Atom for StcoAtom {
    fn read<B: ReadBytes>(reader: &mut B, mut header: AtomHeader) -> Result<Self> {
        let (_, _) = header.read_extended_header(reader)?;

        // minimum data size is 4 bytes
        let len = match header.data_len() {
            Some(len) if len >= 4 => len as u32,
            Some(_) => return decode_error("isomp4 (stco): atom size is less than 16 bytes"),
            None => return decode_error("isomp4 (stco): expected atom size to be known"),
        };

        let entry_count = reader.read_be_u32()?;
        if entry_count != (len - 4) / 4 {
            return decode_error("isomp4 (stco): invalid entry count");
        }

        // TODO: Apply a limit.
        let mut chunk_offsets = Vec::with_capacity(entry_count as usize);

        for _ in 0..entry_count {
            chunk_offsets.push(reader.read_be_u32()?);
        }

        Ok(StcoAtom { chunk_offsets })
    }
}
