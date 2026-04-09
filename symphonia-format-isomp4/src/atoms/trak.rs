// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::atoms::{
    Atom, AtomHeader, AtomIterator, AtomType, EdtsAtom, MdiaAtom, ReadAtom, Result, TkhdAtom,
    decode_error,
};

/// Track atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct TrakAtom {
    /// Track header atom.
    pub tkhd: TkhdAtom,
    /// Optional, edit list atom.
    pub edts: Option<EdtsAtom>,
    /// Media atom.
    pub mdia: MdiaAtom,
    /// Duration, equal to mdia.mdhd.duration unless it is zero, in which case it is equal to tkhd.duration.
    pub duration: u64,
}

impl Atom for TrakAtom {
    fn read<R: ReadAtom>(it: &mut AtomIterator<R>, _header: &AtomHeader) -> Result<Self> {
        let mut tkhd = None;
        let mut edts = None;
        let mut mdia = None;

        while let Some(header) = it.next_header()? {
            match header.atom_type {
                AtomType::TrackHeader => {
                    tkhd = Some(it.read_atom::<TkhdAtom>()?);
                }
                AtomType::Edit => {
                    edts = Some(it.read_atom::<EdtsAtom>()?);
                }
                AtomType::Media => {
                    mdia = Some(it.read_atom::<MdiaAtom>()?);
                }
                _ => (),
            }
        }

        let Some(tkhd_atom) = tkhd
        else {
            return decode_error("isomp4 (trak): missing tkhd atom");
        };

        let Some(mdia_atom) = mdia
        else {
            return decode_error("isomp4 (trak): missing mdia atom");
        };

        let duration =
            if mdia_atom.mdhd.duration != 0 { mdia_atom.mdhd.duration } else { tkhd_atom.duration };

        Ok(TrakAtom { tkhd: tkhd_atom, edts, mdia: mdia_atom, duration })
    }
}
