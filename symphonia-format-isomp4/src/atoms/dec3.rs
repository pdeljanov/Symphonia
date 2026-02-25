// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use alloc::boxed::Box;
use symphonia_core::codecs::audio::well_known::CODEC_ID_EAC3;
use symphonia_core::errors::{Error, Result};
use symphonia_core::io::ReadBytes;

use crate::atoms::stsd::AudioSampleEntry;
use crate::atoms::{Atom, AtomHeader};

#[allow(dead_code)]
#[derive(Debug)]
pub struct Dec3Atom {
    /// EAC3SpecificBox
    extra_data: Box<[u8]>,
}

impl Atom for Dec3Atom {
    fn read<B: ReadBytes>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        // EAC3SpecificBox should have length
        let len = header
            .data_len()
            .ok_or(Error::DecodeError("isomp4 (dec3): expected atom size to be known"))?;

        let extra_data = reader.read_boxed_slice_exact(len as usize)?;

        Ok(Dec3Atom { extra_data })
    }
}

impl Dec3Atom {
    pub fn fill_audio_sample_entry(&self, entry: &mut AudioSampleEntry) {
        entry.codec_id = CODEC_ID_EAC3;
        entry.extra_data = Some(self.extra_data.clone());
    }
}
