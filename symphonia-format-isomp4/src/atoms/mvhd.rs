// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::ReadBytes;

use crate::atoms::{Atom, AtomHeader};
use crate::fp::FpU8;

/// Movie header atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct MvhdAtom {
    /// The creation time.
    pub ctime: u64,
    /// The modification time.
    pub mtime: u64,
    /// Timescale for the movie expressed as the number of units per second.
    pub timescale: u32,
    /// The duration of the movie in timescale units.
    ///
    /// This value is equal to the sum of the durations of all the longest track's edits. If there
    /// are no edits, then this is the duration of all the longest track's samples.
    pub duration: u64,
    /// The preferred volume to play the movie.
    pub volume: FpU8,
}

impl Atom for MvhdAtom {
    fn read<B: ReadBytes>(reader: &mut B, mut header: AtomHeader) -> Result<Self> {
        let (version, _) = header.read_extended_header(reader)?;

        let expected_len = if version == 0 { 96 } else { 108 };
        if header.data_len() != Some(expected_len) {
            return decode_error("isomp4 (mvhd): atom size is not 108 or 120 bytes");
        }

        let mut mvhd =
            MvhdAtom { ctime: 0, mtime: 0, timescale: 0, duration: 0, volume: Default::default() };

        // Version 0 uses 32-bit time values, verion 1 used 64-bit values.
        match version {
            0 => {
                mvhd.ctime = u64::from(reader.read_be_u32()?);
                mvhd.mtime = u64::from(reader.read_be_u32()?);
                mvhd.timescale = reader.read_be_u32()?;
                // 0xffff_ffff is a special case.
                mvhd.duration = match reader.read_be_u32()? {
                    u32::MAX => u64::MAX,
                    duration => u64::from(duration),
                };
            }
            1 => {
                mvhd.ctime = reader.read_be_u64()?;
                mvhd.mtime = reader.read_be_u64()?;
                mvhd.timescale = reader.read_be_u32()?;
                mvhd.duration = reader.read_be_u64()?;
            }
            _ => return decode_error("isomp4 (mvhd): invalid version"),
        }

        // Ignore the preferred playback rate.
        let _ = reader.read_be_u32()?;

        // Preferred volume.
        mvhd.volume = FpU8::parse_raw(reader.read_be_u16()?);

        // Remaining fields are ignored.

        Ok(mvhd)
    }
}
