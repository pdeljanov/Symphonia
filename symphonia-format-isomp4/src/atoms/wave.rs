// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::Result;
use symphonia_core::io::ReadBytes;

use crate::atoms::stsd::AudioSampleEntry;
use crate::atoms::{Atom, AtomHeader, AtomIterator, AtomType, EsdsAtom};

#[allow(dead_code)]
#[derive(Debug)]
pub struct WaveAtom {
    pub esds: Option<EsdsAtom>,
}

impl Atom for WaveAtom {
    fn read<B: ReadBytes>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        let mut iter = AtomIterator::new(reader, header);

        let mut esds = None;

        while let Some(header) = iter.next()? {
            if header.atom_type == AtomType::Esds {
                esds = Some(iter.read_atom::<EsdsAtom>()?);
            }
        }

        Ok(WaveAtom { esds })
    }
}

impl WaveAtom {
    pub fn fill_audio_sample_entry(&self, entry: &mut AudioSampleEntry) -> Result<()> {
        if let Some(esds) = &self.esds {
            esds.fill_audio_sample_entry(entry)?;
        }

        Ok(())
    }
}
