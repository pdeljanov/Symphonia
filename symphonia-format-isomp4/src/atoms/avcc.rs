// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::codecs::video::well_known::CODEC_ID_H264;
use symphonia_core::codecs::video::VideoCodecParameters;
use symphonia_core::codecs::CodecProfile;
use symphonia_core::errors::{decode_error, Error, Result};
use symphonia_core::io::{BitReaderLtr, ReadBitsLtr, ReadBytes};

use crate::atoms::{Atom, AtomHeader};

#[allow(dead_code)]
#[derive(Debug)]
pub struct AvcCAtom {
    /// AVC extra data (AVCDecoderConfigurationRecord).
    extra_data: Box<[u8]>,
    profile: CodecProfile,
    level: u32,
}

impl Atom for AvcCAtom {
    fn read<B: ReadBytes>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        // The AVCConfiguration atom payload is a single AVCDecoderConfigurationRecord. This record
        // forms the defacto codec extra data.
        let len = header
            .data_len()
            .ok_or_else(|| Error::DecodeError("isomp4 (avcC): expected atom size to be known"))?;

        let extra_data = reader.read_boxed_slice_exact(len as usize)?;

        dbg!(extra_data.len());

        // Parse the AVCDecoderConfigurationRecord to get the profile and level. Defined in
        // ISO/IEC 14496-15 section 5.3.3.1.
        let mut br = BitReaderLtr::new(&extra_data);

        // Configuration version is always 1.
        let configuration_version = br.read_bits_leq32(8)?;

        if configuration_version != 1 {
            return decode_error(
                "isomp4 (avc): unexpected avc decoder configuration record version",
            );
        }

        // AVC profile as defined in ISO/IEC 14496-10.
        let avc_profile_indication = br.read_bits_leq32(8)?;
        let _profile_compatibility = br.read_bits_leq32(8)?;
        let avc_level_indication = br.read_bits_leq32(8)?;

        Ok(Self {
            extra_data,
            profile: CodecProfile::new(avc_profile_indication),
            level: avc_level_indication,
        })
    }
}

impl AvcCAtom {
    pub fn fill_codec_params(&self, codec_params: &mut VideoCodecParameters) {
        codec_params
            .for_codec(CODEC_ID_H264)
            .with_profile(self.profile)
            .with_level(self.level)
            .with_extra_data(self.extra_data.clone());
    }
}
