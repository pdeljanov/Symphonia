// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::codecs::video::well_known::extra_data::VIDEO_EXTRA_DATA_ID_DOLBY_VISION_CONFIG;
use symphonia_core::codecs::video::VideoExtraData;
use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::ReadBytes;

use crate::atoms::stsd::VisualSampleEntry;
use crate::atoms::{Atom, AtomHeader};

#[allow(dead_code)]
#[derive(Debug)]
pub struct DoviAtom {
    extra_data: VideoExtraData,
}

impl Atom for DoviAtom {
    fn read<B: ReadBytes>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        // The Dolby Vision Configuration atom payload (dvvC and dvcC).
        // Contains DOVIDecoderConfigurationRecord, point 3.2 from
        // https://professional.dolby.com/siteassets/content-creation/dolby-vision-for-content-creators/dolby_vision_bitstreams_within_the_iso_base_media_file_format_dec2017.pdf
        // It should be 24 bytes
        let len = match header.data_len() {
            Some(len @ 24) => len as usize,
            Some(_) => return decode_error("isomp4 (dvcC/dvvC): atom size is not 24 bytes"),
            None => return decode_error("isomp4 (dvcC/dvvC): expected atom size to be known"),
        };

        let dovi_data = VideoExtraData {
            id: VIDEO_EXTRA_DATA_ID_DOLBY_VISION_CONFIG,
            data: reader.read_boxed_slice_exact(len)?,
        };

        Ok(Self { extra_data: dovi_data })
    }
}

impl DoviAtom {
    pub fn fill_video_sample_entry(&self, entry: &mut VisualSampleEntry) {
        entry.extra_data.push(self.extra_data.clone());
    }
}
