// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::io::ByteStream;

use crate::atoms::{Atom, AtomHeader, AtomType, Mp4aAtom};

#[derive(Debug)]
pub enum SampleDescription {
    Mp4a(Mp4aAtom),
    Unsupported,
}

/// Sample description atom.
#[derive(Debug)]
pub struct StsdAtom {
    /// Atom header.
    header: AtomHeader,
    /// Codec-specific sample description.
    pub sample_desc: SampleDescription,
}

impl Atom for StsdAtom {
    fn header(&self) -> AtomHeader {
        self.header
    }

    fn read<B: ByteStream>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        let (_, _) = AtomHeader::read_extra(reader)?;

        let n_entries = reader.read_be_u32()?;

        if n_entries == 0 {
            return decode_error("missing sample description atom");
        }

        if n_entries > 1 {
            return unsupported_error("more than 1 sample description atoms");
        }

        // Get the sample description atom header.
        let sample_desc_header = AtomHeader::read(reader)?;

        let sample_desc = match sample_desc_header.atype {
            AtomType::Mp4a => {
                SampleDescription::Mp4a(Mp4aAtom::read(reader, sample_desc_header)?)
            }
            _ => SampleDescription::Unsupported,
        };

        Ok(StsdAtom {
            header,
            sample_desc,
        })
    }
}