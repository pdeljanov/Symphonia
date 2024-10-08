// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::codecs::audio::well_known::CODEC_ID_FLAC;
use symphonia_core::codecs::audio::{AudioCodecParameters, VerificationCheck};
use symphonia_core::errors::{decode_error, unsupported_error, Result};
use symphonia_core::io::{BufReader, ReadBytes};

use symphonia_common::xiph::audio::flac::metadata::{
    MetadataBlockHeader, MetadataBlockType, StreamInfo,
};

use crate::atoms::{Atom, AtomHeader};

/// FLAC atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct FlacAtom {
    /// FLAC stream info block.
    stream_info: StreamInfo,
    /// FLAC extra data.
    extra_data: Box<[u8]>,
}

impl Atom for FlacAtom {
    fn read<B: ReadBytes>(reader: &mut B, mut header: AtomHeader) -> Result<Self> {
        let (version, flags) = header.read_extended_header(reader)?;

        if version != 0 {
            return unsupported_error("isomp4 (flac): unsupported flac version");
        }

        if flags != 0 {
            return decode_error("isomp4 (flac): flags not zero");
        }

        // The first block must be the stream information block.
        let block_header = MetadataBlockHeader::read(reader)?;

        if block_header.block_type != MetadataBlockType::StreamInfo {
            return decode_error("isomp4 (flac): first block is not stream info");
        }

        // Ensure the block length is correct for a stream information block before allocating a
        // buffer for it.
        if !StreamInfo::is_valid_size(u64::from(block_header.block_len)) {
            return decode_error("isomp4 (flac): invalid stream info block length");
        }

        let extra_data = reader.read_boxed_slice_exact(block_header.block_len as usize)?;
        let stream_info = StreamInfo::read(&mut BufReader::new(&extra_data))?;

        Ok(FlacAtom { stream_info, extra_data })
    }
}

impl FlacAtom {
    pub fn fill_codec_params(&self, codec_params: &mut AudioCodecParameters) {
        codec_params
            .for_codec(CODEC_ID_FLAC)
            .with_sample_rate(self.stream_info.sample_rate)
            .with_bits_per_sample(self.stream_info.bits_per_sample)
            .with_channels(self.stream_info.channels.clone())
            .with_extra_data(self.extra_data.clone());

        if let Some(md5) = self.stream_info.md5 {
            codec_params.with_verification_code(VerificationCheck::Md5(md5));
        }
    }
}
