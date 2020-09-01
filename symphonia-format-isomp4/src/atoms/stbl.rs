// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{Result, decode_error};
use symphonia_core::io::ByteStream;

use crate::atoms::{Atom, AtomHeader, AtomIterator, AtomType};
use crate::atoms::{StsdAtom, SttsAtom, CttsAtom, StssAtom, StscAtom, StszAtom, StcoAtom, Co64Atom};

use log::warn;

/// Sample table atom.
#[derive(Debug)]
pub struct StblAtom {
    /// Atom header.
    header: AtomHeader,
    pub stsd: StsdAtom,
    pub stts: SttsAtom,
    // pub ctts: Option<CttsAtom>,
    // pub stss: Option<StssAtom>,
    pub stsc: StscAtom,
    pub stsz: StszAtom,
    pub stco: Option<StcoAtom>,
    pub co64: Option<Co64Atom>,
}

impl Atom for StblAtom {
    fn header(&self) -> AtomHeader {
        self.header
    }

    fn read<B: ByteStream>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        let mut iter = AtomIterator::new(reader, header);

        let mut stsd = None;
        let mut stts = None;
        // let mut ctts = None;
        // let mut stss = None;
        let mut stsc = None;
        let mut stsz = None;
        let mut stco = None;
        let mut co64 = None;

        while let Some(header) = iter.next()? {
            match header.atype {
                AtomType::Stsd => {
                    stsd = Some(iter.read_atom::<StsdAtom>()?);
                }
                AtomType::Stts => {
                    stts = Some(iter.read_atom::<SttsAtom>()?);
                }
                // AtomType::Ctts => {
                //     ctts = Some(iter.read_atom::<CttsAtom>()?);
                // }
                // Sync sample box is only required for video.
                // AtomType::Stss => {
                //     stss = Some(iter.read_atom::<StssAtom>()?);
                // }
                AtomType::Stsc => {
                    stsc = Some(iter.read_atom::<StscAtom>()?);
                }
                AtomType::Stsz => {
                    stsz = Some(iter.read_atom::<StszAtom>()?);
                }
                AtomType::Stco => {
                    stco = Some(iter.read_atom::<StcoAtom>()?);
                }
                AtomType::Co64 => {
                    co64 = Some(iter.read_atom::<Co64Atom>()?);
                }
                _ => ()
            }
        }

        if stsd.is_none() {
            return decode_error("missing stsd atom");
        }

        if stts.is_none() {
            return decode_error("missing stts atom");
        }

        if stsc.is_none() {
            return decode_error("missing stsc atom");
        }

        if stsz.is_none() {
            return decode_error("missing stsz atom");
        }

        if stco.is_none() && co64.is_none() {
            warn!("missing stco or co64 atom");
        }

        Ok(StblAtom {
            header,
            stsd: stsd.unwrap(),
            stts: stts.unwrap(),
            // ctts,
            // stss,
            stsc: stsc.unwrap(),
            stsz: stsz.unwrap(),
            stco,
            co64,
        })
    }
}