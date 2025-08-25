// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::ReadBytes;

use crate::atoms::{Atom, AtomHeader};

/// Movie fragment header atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct MfhdAtom {
    /// Sequence number associated with fragment.
    pub sequence_number: u32,
}

impl Atom for MfhdAtom {
    fn read<B: ReadBytes>(reader: &mut B, mut header: AtomHeader) -> Result<Self> {
        let (_, _) = header.read_extended_header(reader)?;

        if header.data_len() != Some(4) {
            return decode_error("isomp4 (mfhd): atom size is not 16 bytes");
        }

        let sequence_number = reader.read_be_u32()?;

        Ok(MfhdAtom { sequence_number })
    }
}
