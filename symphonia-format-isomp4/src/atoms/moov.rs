// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{Result, decode_error};
use symphonia_core::io::ByteStream;

use crate::atoms::{Atom, AtomHeader, AtomIterator, AtomType, MvhdAtom, TrakAtom};

/// Movie atom.
#[derive(Debug)]
pub struct MoovAtom {
    /// Atom header.
    header: AtomHeader,
    /// Movie header atom.
    pub mvhd: MvhdAtom,
    /// Trak atoms.
    pub traks: Vec<TrakAtom>,
}

impl Atom for MoovAtom {
    fn read<B: ByteStream>(reader: &mut B, header: AtomHeader) -> Result<Self> {

        let mut iter = AtomIterator::new(reader, header);

        let mut mvhd = None;
        let mut traks = Vec::new();

        while let Some(header) = iter.next()? {
            match header.atype {
                AtomType::Mvhd => {
                    mvhd = Some(iter.read_atom::<MvhdAtom>()?);
                }
                AtomType::Trak => {
                    let trak = iter.read_atom::<TrakAtom>()?;
                    traks.push(trak);
                }
                _ => ()
            }
        }

        if mvhd.is_none() {
            return decode_error("missing mvhd atom");
        }

        Ok(MoovAtom {
            header,
            mvhd: mvhd.unwrap(),
            traks,
        })

    }

    fn header(&self) -> AtomHeader {
        self.header
    }
    
}
