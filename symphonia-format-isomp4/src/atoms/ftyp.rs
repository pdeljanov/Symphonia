// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::common::FourCc;
use symphonia_core::errors::{decode_error, Error, Result};
use symphonia_core::io::ReadBytes;

use crate::atoms::{Atom, AtomHeader};

/// File type atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct FtypAtom {
    pub major: FourCc,
    pub minor: [u8; 4],
    pub compatible: Vec<FourCc>,
}

impl Atom for FtypAtom {
    fn read<B: ReadBytes>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        // The Ftyp atom must be have a data length that is known, and it must be a multiple of 4
        // since it only stores FourCCs.
        let data_len = header
            .data_len()
            .ok_or(Error::DecodeError("isomp4 (ftyp): expected atom size to be known"))?;

        if data_len < 8 || data_len & 0x3 != 0 {
            return decode_error("isomp4 (ftype): invalid data length");
        }

        // Major
        let major = FourCc::new(reader.read_quad_bytes()?);

        // Minor
        let minor = reader.read_quad_bytes()?;

        // The remainder of the Ftyp atom contains the FourCCs of compatible brands.
        let n_brands = (data_len - 8) / 4;

        let mut compatible = Vec::new();

        for _ in 0..n_brands {
            let brand = reader.read_quad_bytes()?;
            compatible.push(FourCc::new(brand));
        }

        Ok(FtypAtom { major, minor, compatible })
    }
}
