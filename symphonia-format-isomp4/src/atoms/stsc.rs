// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::Result;
use symphonia_core::io::ByteStream;

use crate::atoms::{Atom, AtomHeader};

#[derive(Debug)]
pub struct StscEntry {
    pub first_chunk: u32,
    pub first_sample: u32,
    pub samples_per_chunk: u32,
    pub sample_desc_index: u32,
}

/// Sample to Chunk Atom
#[derive(Debug)]
pub struct StscAtom {
    /// Atom header.
    header: AtomHeader,
    /// Entries.
    pub entries: Vec<StscEntry>,
}

impl Atom for StscAtom {
    fn header(&self) -> AtomHeader {
        self.header
    }

    fn read<B: ByteStream>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        let (_, _) = AtomHeader::read_extra(reader)?;

        let entry_count = reader.read_be_u32()?;

        // TODO: Apply a limit.
        let mut entries = Vec::with_capacity(entry_count as usize);

        for _ in 0..entry_count {
            entries.push(StscEntry {
                first_chunk: reader.read_be_u32()? - 1,
                first_sample: 0,
                samples_per_chunk: reader.read_be_u32()?,
                sample_desc_index: reader.read_be_u32()?,
            });
        }

        for i in 0..entry_count as usize - 1 {
            let n = entries[i + 1].first_chunk - entries[i].first_chunk;

            entries[i + 1].first_sample = entries[i].first_sample + (n * entries[i].samples_per_chunk);
        }

        Ok(StscAtom {
            header,
            entries
        })
    }
}