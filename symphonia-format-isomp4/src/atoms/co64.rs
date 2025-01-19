// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::ReadBytes;

use crate::atoms::{Atom, AtomHeader};

/// Chunk offset atom (64-bit version).
#[allow(dead_code)]
#[derive(Debug)]
pub struct Co64Atom {
    pub chunk_offsets: Vec<u64>,
}

impl Atom for Co64Atom {
    fn read<B: ReadBytes>(reader: &mut B, mut header: AtomHeader) -> Result<Self> {
        let (_, _) = header.read_extended_header(reader)?;

        // minimum data size is 4 bytes
        let len = match header.data_len() {
            Some(len) if len >= 4 => len as u32,
            Some(_) => return decode_error("isomp4 (co64): atom size is less than 16 bytes"),
            None => return decode_error("isomp4 (co64): expected atom size to be known"),
        };

        let entry_count = reader.read_be_u32()?;
        if entry_count != (len - 4) / 8 {
            return decode_error("isomp4 (co64): invalid entry count");
        }

        // TODO: Apply a limit.
        let mut chunk_offsets = Vec::with_capacity(entry_count as usize);

        for _ in 0..entry_count {
            chunk_offsets.push(reader.read_be_u64()?);
        }

        Ok(Co64Atom { chunk_offsets })
    }
}
