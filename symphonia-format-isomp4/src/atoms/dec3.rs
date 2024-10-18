// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::codecs::audio::well_known::CODEC_ID_EAC3;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::errors::{Error, Result};
use symphonia_core::io::ReadBytes;

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
            .ok_or_else(|| Error::DecodeError("isomp4 (dec3): expected atom size to be known"))?;

        let extra_data = reader.read_boxed_slice_exact(len as usize)?;

        Ok(Dec3Atom { extra_data })
    }
}

impl Dec3Atom {
    pub fn fill_codec_params(&self, codec_params: &mut AudioCodecParameters) {
        codec_params.for_codec(CODEC_ID_EAC3).with_extra_data(self.extra_data.clone());
    }
}