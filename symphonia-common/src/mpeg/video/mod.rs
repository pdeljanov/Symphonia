// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::codecs::CodecProfile;
use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::{BitReaderLtr, ReadBitsLtr};

pub struct AVCDecoderConfigurationRecord {
    pub profile: CodecProfile,
    pub level: u32,
}

impl AVCDecoderConfigurationRecord {
    pub fn read(buf: &[u8]) -> Result<Self> {
        let mut br = BitReaderLtr::new(buf);

        // Parse the AVCDecoderConfigurationRecord to get the profile and level. Defined in
        // ISO/IEC 14496-15 section 5.3.3.1.

        // Configuration version is always 1.
        let configuration_version = br.read_bits_leq32(8)?;

        if configuration_version != 1 {
            return decode_error(
                "utils (avc): unexpected avc decoder configuration record version",
            );
        }

        // AVC profile as defined in ISO/IEC 14496-10.
        let avc_profile_indication = br.read_bits_leq32(8)?;
        let _profile_compatibility = br.read_bits_leq32(8)?;
        let avc_level_indication = br.read_bits_leq32(8)?;

        Ok(AVCDecoderConfigurationRecord {
            profile: CodecProfile::new(avc_profile_indication),
            level: avc_level_indication,
        })
    }
}

pub struct HEVCDecoderConfigurationRecord {
    pub profile: CodecProfile,
    pub level: u32,
}

impl HEVCDecoderConfigurationRecord {
    pub fn read(buf: &[u8]) -> Result<Self> {
        let mut br = BitReaderLtr::new(buf);

        // Parse the HEVCDecoderConfigurationRecord to get the profile and level. Defined in
        // ISO/IEC 14496-15 section 8.3.3.1.

        // Configuration version is always 1.
        let configuration_version = br.read_bits_leq32(8)?;

        if configuration_version != 1 {
            return decode_error(
                "utils (hevc): unexpected hevc decoder configuration record version",
            );
        }

        let _general_profile_space = br.read_bits_leq32(2)?;
        let _general_tier_flag = br.read_bit()?;
        let general_profile_idc = br.read_bits_leq32(5)?;
        let _general_profile_compatibility_flags = br.read_bits_leq32(32)?;
        let _general_constraint_indicator_flags = br.read_bits_leq64(48)?;
        let general_level_idc = br.read_bits_leq32(8)?;

        Ok(HEVCDecoderConfigurationRecord {
            profile: CodecProfile::new(general_profile_idc),
            level: general_level_idc,
        })
    }
}
