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
pub struct SttsEntry {
    pub sample_count: u32,
    pub sample_delta: u32,
}

#[derive(Debug)]
pub struct SttsAtom {
    /// Atom header.
    header: AtomHeader,
    pub entries: Vec<SttsEntry>,
}

impl Atom for SttsAtom {
    fn header(&self) -> AtomHeader {
        self.header
    }

    fn read<B: ByteStream>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        let (_, _) = AtomHeader::read_extra(reader)?;

        let entry_count = reader.read_be_u32()?;

        // TODO: Limit table length.
        let mut entries = Vec::with_capacity(entry_count as usize);

        for _ in 0..entry_count {
            entries.push(SttsEntry {
                sample_count: reader.read_be_u32()?,
                sample_delta: reader.read_be_u32()?,
            });
        }

        Ok(SttsAtom {
            header,
            entries,
        })
    }
}