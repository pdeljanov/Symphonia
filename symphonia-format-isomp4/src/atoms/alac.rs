// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::codecs::audio::well_known::CODEC_ID_ALAC;
use symphonia_core::errors::{decode_error, unsupported_error, Result};
use symphonia_core::io::ReadBytes;

use crate::atoms::stsd::AudioSampleEntry;
use crate::atoms::{Atom, AtomHeader};

#[allow(dead_code)]
#[derive(Debug)]
pub struct AlacAtom {
    /// ALAC extra data (magic cookie).
    extra_data: Box<[u8]>,
}

impl Atom for AlacAtom {
    fn read<B: ReadBytes>(reader: &mut B, mut header: AtomHeader) -> Result<Self> {
        let (version, flags) = header.read_extended_header(reader)?;

        if version != 0 {
            return unsupported_error("isomp4 (alac): unsupported alac version");
        }

        if flags != 0 {
            return decode_error("isomp4 (alac): flags not zero");
        }

        // The ALAC magic cookie (aka extra data) is either 24 or 48 bytes long.
        let magic_len = match header.data_len() {
            Some(len @ 24) | Some(len @ 48) => len as usize,
            Some(_) => return decode_error("isomp4 (alac): invalid magic cookie length"),
            None => return decode_error("isomp4 (alac): unknown magic cookie length"),
        };

        // Read the magic cookie.
        let extra_data = reader.read_boxed_slice_exact(magic_len)?;

        Ok(AlacAtom { extra_data })
    }
}

impl AlacAtom {
    pub fn fill_audio_sample_entry(&self, entry: &mut AudioSampleEntry) {
        entry.codec_id = CODEC_ID_ALAC;
        entry.extra_data = Some(self.extra_data.clone());
    }
}
