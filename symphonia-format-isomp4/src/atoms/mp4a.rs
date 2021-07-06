// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{Result, unsupported_error};
use symphonia_core::io::ReadBytes;

use crate::atoms::{Atom, AtomHeader, EsdsAtom};
use crate::fp::FpU16;

#[derive(Debug)]
pub struct SoundSampleDescription {
    pub n_channels: u32,
    pub sample_size: u16,
    pub sample_rate: f64,
}

impl SoundSampleDescription {
    pub fn read<B: ReadBytes>(reader: &mut B) -> Result<SoundSampleDescription> {
        let version = reader.read_be_u16()?;

        // Skip revision and vendor.
        reader.ignore_bytes(6)?;

        let mut n_channels = u32::from(reader.read_be_u16()?);
        let sample_size = reader.read_be_u16()?;

        // Skip compression ID and packet size.
        reader.ignore_bytes(4)?;

        let mut sample_rate = f64::from(FpU16::parse_raw(reader.read_be_u32()?));

        match version {
            0 => (),
            1 => {
                // Version 1 appends an additional 4 32-bit fields (samples/packet, bytes/packet,
                // bytes/frame, and bytes/sample) to the version 0 description.
                reader.ignore_bytes(16)?;
            }
            2 => {
                // Version 2 appends new fields onto the version 0 description. The version 0 fields
                // are mostly overriden by the new fields.
                reader.ignore_bytes(4)?;

                sample_rate = reader.read_be_f64()?;
                n_channels = reader.read_be_u32()?;

                // Skip 5 32-bit fields: 
                reader.ignore_bytes(20)?;
            }
            _ => {
                return unsupported_error("unknown sound sample description version");
            }
        }

        Ok(SoundSampleDescription {
            n_channels,
            sample_size,
            sample_rate,
        })
    }
}

#[derive(Debug)]
pub struct Mp4aAtom {
    /// Atom header.
    header: AtomHeader,
    /// General sound sample description.
    pub sound_desc: SoundSampleDescription,
    /// MPEG4 elementary stream descriptor atom.
    pub esds: EsdsAtom,
}

impl Atom for Mp4aAtom {
    fn header(&self) -> AtomHeader {
        self.header
    }

    fn read<B: ReadBytes>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        // First 6 bytes should be all 0.
        reader.ignore_bytes(6)?;
        
        // Data reference.
        let _ = reader.read_be_u16()?;

        // Common sound description for all codec-specific atoms.
        let sound_desc = SoundSampleDescription::read(reader)?;

        // An ESDS atom follows.
        let esds_atom_header = AtomHeader::read(reader)?;
        let esds = EsdsAtom::read(reader, esds_atom_header)?;

        Ok(Mp4aAtom {
            header,
            sound_desc,
            esds,
        })
    }
}