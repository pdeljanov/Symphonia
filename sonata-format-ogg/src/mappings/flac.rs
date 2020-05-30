// Sonata
// Copyright (c) 2020 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::{Mapper, MapResult};

use sonata_core::codecs::{CodecParameters, CODEC_TYPE_FLAC};
use sonata_core::errors::{Result, decode_error};
use sonata_core::meta::MetadataBuilder;
use sonata_core::io::{BufStream, ByteStream};

use sonata_utils_xiph::flac::metadata::{MetadataBlockHeader, MetadataBlockType, StreamInfo};
use sonata_utils_xiph::flac::metadata::{read_comment_block, read_picture_block};

/// The expected size of the first FLAC header packet.
const OGG_FLAC_HEADER_PACKET_SIZE: usize = 51;

/// The major version number of the supported OGG-FLAC mapping.
const OGG_FLAC_MAPPING_MAJOR_VERSION: u8 = 1;

/// The OGG-FLAC header packet signature.
const OGG_FLAC_HEADER_SIGNATURE: &[u8] = b"FLAC";

/// The OGG-FLAC header packet type value.
const OGG_FLAC_PACKET_TYPE: u8 = 0x7f;

/// The native FLAC signature.
const FLAC_SIGNATURE: &[u8] = b"fLaC";

pub fn detect(buf: &[u8]) -> Result<Option<Box<dyn Mapper>>> {
    // The packet shall be exactly the expected length.
    if buf.len() != OGG_FLAC_HEADER_PACKET_SIZE {
        return Ok(None);
    }

    let mut reader = BufStream::new(&buf);
    
    // The first byte indicates the packet type and must be 0x7f.
    if reader.read_u8()? != OGG_FLAC_PACKET_TYPE {
        return Ok(None);
    }
    
    // Next, the OGG FLAC signature, in ASCII, must be "FLAC".
    if reader.read_quad_bytes()? != OGG_FLAC_HEADER_SIGNATURE {
        return Ok(None);
    }
    
    // Next, a one-byte binary major version number for the mapping, only version 1 is supported.
    if reader.read_u8()? != OGG_FLAC_MAPPING_MAJOR_VERSION {
        return Ok(None);
    }
    
    // Next, a one-byte minor version number for the mapping. This is ignored because we support all
    // version 1 features.
    let _minor = reader.read_u8()?;
    
    // Next, a two-byte, big-endian number signifying the number of header (non-audio) packets, not
    // including the identification packet. This number may be 0 to signify it is unknown.
    let _ = reader.read_be_u16()?;
    
    // Last, the four-byte ASCII native FLAC signature "fLaC".
    if reader.read_quad_bytes()? != FLAC_SIGNATURE {
        return Ok(None);
    }

    // Following the previous OGG FLAC identification data is the stream information block as a
    // native FLAC metadata block.
    let header = MetadataBlockHeader::read(&mut reader)?;

    if header.block_type != MetadataBlockType::StreamInfo {
        return Ok(None);
    }

    let stream_info = StreamInfo::read(&mut reader)?;

    // Populate the codec parameters with the information read from the stream information block.
    let mut codec_params = CodecParameters::new();

    codec_params
        .for_codec(CODEC_TYPE_FLAC)
        .with_sample_rate(stream_info.sample_rate)
        .with_bits_per_sample(stream_info.bits_per_sample)
        .with_max_frames_per_packet(u64::from(stream_info.block_len_max))
        .with_channels(stream_info.channels)
        .with_packet_data_integrity(true);

    if let Some(n_frames) = stream_info.n_samples {
        codec_params.with_n_frames(n_frames);
    }

    // Instantiate the FLAC mapper.
    let mapper = Box::new(FlacMapper {
        codec_params,
    });

    Ok(Some(mapper))
}

struct FlacMapper {
    codec_params: CodecParameters,
}

impl Mapper for FlacMapper {

    fn codec(&self) -> &CodecParameters {
        &self.codec_params
    }

    fn map_packet(&mut self, buf: &[u8]) -> Result<MapResult> {
        // The first byte of a packet is the packet type.
        if buf[0] == 0xff {
            // A packet-type of 0xff is a bitstream packet.
            Ok(MapResult::Bitstream)
        }
        else if buf[0] == 0x00 || buf[0] == 0x80 {
            // Packet types 0x00 and 0x80 are invalid in the OGG mapping.
            decode_error("invalid packet type")
        }
        else {
            // Packet types in the range 0x01 to 0x7f, and 0x81 to 0xfe are metadata blocks.
            let mut reader = BufStream::new(&buf);
            let header = MetadataBlockHeader::read(&mut reader)?;

            match header.block_type {
                MetadataBlockType::VorbisComment => {
                    let mut builder = MetadataBuilder::new();

                    read_comment_block(&mut reader, &mut builder)?;

                    Ok(MapResult::Metadata(builder.metadata()))
                }
                MetadataBlockType::Picture => {
                    let mut builder = MetadataBuilder::new();

                    read_picture_block(&mut reader, &mut builder)?;

                    Ok(MapResult::Metadata(builder.metadata()))
                }
                _ => Ok(MapResult::Unknown)
            }
        }
    }

}