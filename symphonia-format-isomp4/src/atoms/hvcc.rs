// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_common::mpeg::video::HEVCDecoderConfigurationRecord;
use symphonia_core::codecs::video::well_known::CODEC_ID_HEVC;
use symphonia_core::codecs::video::VideoCodecParameters;
use symphonia_core::codecs::CodecProfile;
use symphonia_core::errors::{Error, Result};
use symphonia_core::io::ReadBytes;

use crate::atoms::{Atom, AtomHeader};

#[allow(dead_code)]
#[derive(Debug)]
pub struct HvcCAtom {
    /// HEVC extra data (HEVCDecoderConfigurationRecord).
    extra_data: Box<[u8]>,
    profile: CodecProfile,
    level: u32,
}

impl Atom for HvcCAtom {
    fn read<B: ReadBytes>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        // The HEVCConfiguration atom payload is a single HEVCDecoderConfigurationRecord. This record
        // forms the defacto codec extra data.
        let len = header
            .data_len()
            .ok_or_else(|| Error::DecodeError("isomp4 (hvcC): expected atom size to be known"))?;

        let extra_data = reader.read_boxed_slice_exact(len as usize)?;

        let hevc_config = HEVCDecoderConfigurationRecord::read(&extra_data)?;

        Ok(Self { extra_data, profile: hevc_config.profile, level: hevc_config.level })
    }
}

impl HvcCAtom {
    pub fn fill_codec_params(&self, codec_params: &mut VideoCodecParameters) {
        codec_params
            .for_codec(CODEC_ID_HEVC)
            .with_profile(self.profile)
            .with_level(self.level)
            .with_extra_data(self.extra_data.clone());
    }
}
