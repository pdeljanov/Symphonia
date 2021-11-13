// Symphonia
// Copyright (c) 2019-2021 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::codecs::{CODEC_TYPE_FLAC, CodecParameters, VerificationCheck};
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::io::ReadBytes;

use symphonia_utils_xiph::flac::metadata::{MetadataBlockHeader, MetadataBlockType, StreamInfo};

use crate::atoms::{Atom, AtomHeader};

#[derive(Debug)]
pub struct FlacAtom {
    /// Atom header.
    header: AtomHeader,
    /// FLAC stream info block.
    stream_info: StreamInfo,
}

impl Atom for FlacAtom {
    fn header(&self) -> AtomHeader {
        self.header
    }

    fn read<B: ReadBytes>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        let (version, flags) = AtomHeader::read_extra(reader)?;

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

        let stream_info = StreamInfo::read(reader)?;

        Ok(FlacAtom {
            header,
            stream_info,
        })
    }
}

impl FlacAtom {
    pub fn fill_codec_params(&self, codec_params: &mut CodecParameters) {
        codec_params.for_codec(CODEC_TYPE_FLAC)
                    .with_sample_rate(self.stream_info.sample_rate)
                    .with_bits_per_sample(self.stream_info.bits_per_sample)
                    .with_max_frames_per_packet(u64::from(self.stream_info.block_len_max))
                    .with_channels(self.stream_info.channels)
                    .with_packet_data_integrity(true)
                    .with_verification_code(VerificationCheck::Md5(self.stream_info.md5));
    }
}