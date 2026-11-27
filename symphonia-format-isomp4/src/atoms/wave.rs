// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::atoms::stsd::AudioSampleEntry;
use crate::atoms::{
    AlacAtom, Atom, AtomHeader, AtomIterator, AtomType, EsdsAtom, ReadAtom, Result,
};

#[allow(dead_code)]
#[derive(Debug)]
pub struct WaveAtom {
    pub esds: Option<EsdsAtom>,
    pub alac: Option<AlacAtom>,
}

impl Atom for WaveAtom {
    fn read<R: ReadAtom>(it: &mut AtomIterator<R>, _header: &AtomHeader) -> Result<Self> {
        let mut esds = None;
        let mut alac = None;

        while let Some(header) = it.next_header()? {
            match header.atom_type {
                AtomType::Esds => esds = Some(it.read_atom::<EsdsAtom>()?),
                AtomType::AudioSampleEntryAlac => alac = Some(it.read_atom::<AlacAtom>()?),
                _ => (),
            }
        }

        Ok(WaveAtom { esds, alac })
    }
}

impl WaveAtom {
    pub fn fill_audio_sample_entry(self, entry: &mut AudioSampleEntry) -> Result<()> {
        if let Some(esds) = self.esds {
            esds.fill_audio_sample_entry(entry)?;
        }

        if let Some(alac) = self.alac {
            alac.fill_audio_sample_entry(entry);
        }

        Ok(())
    }
}
