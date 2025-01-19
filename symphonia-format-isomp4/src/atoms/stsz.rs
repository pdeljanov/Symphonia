// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::ReadBytes;

use crate::atoms::{Atom, AtomHeader};

#[derive(Debug)]
pub enum SampleSize {
    Constant(u32),
    Variable(Vec<u32>),
}

/// Sample Size Atom
#[allow(dead_code)]
#[derive(Debug)]
pub struct StszAtom {
    /// The total number of samples.
    pub sample_count: u32,
    /// A vector of `sample_count` sample sizes, or a constant size for all samples.
    pub sample_sizes: SampleSize,
}

impl Atom for StszAtom {
    fn read<B: ReadBytes>(reader: &mut B, mut header: AtomHeader) -> Result<Self> {
        let (_, _) = header.read_extended_header(reader)?;

        // minimum data size is 8 bytes
        let len = match header.data_len() {
            Some(len) if len >= 8 => len as u32,
            Some(_) => return decode_error("isomp4 (stsz): atom size is less than 16 bytes"),
            None => return decode_error("isomp4 (stsz): expected atom size to be known"),
        };

        let sample_size = reader.read_be_u32()?;
        let sample_count = reader.read_be_u32()?;

        let sample_sizes = if sample_size == 0 {
            if sample_count != (len - 8) / 4 {
                return decode_error("isomp4 (stsz): invalid sample count");
            }

            // TODO: Apply a limit.
            let mut entries = Vec::with_capacity(sample_count as usize);

            for _ in 0..sample_count {
                entries.push(reader.read_be_u32()?);
            }

            SampleSize::Variable(entries)
        }
        else {
            SampleSize::Constant(sample_size)
        };

        Ok(StszAtom { sample_count, sample_sizes })
    }
}
