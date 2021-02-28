// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::common::OggPacket;

use super::{Bitstream, Mapper, MapResult};

use symphonia_core::codecs::{CodecParameters, CODEC_TYPE_VORBIS};
use symphonia_core::errors::{Result, decode_error};
use symphonia_core::io::{BufReader, ReadBytes};
use symphonia_core::meta::MetadataBuilder;
use symphonia_metadata::vorbis;

use log::warn;

/// The identification header packet size.
const VORBIS_IDENTIFICATION_HEADER_SIZE: usize = 30;

/// The packet type for an identification header.
const VORBIS_PACKET_TYPE_IDENTIFICATION: u8 = 1;
/// The packet type for a comment header.
const VORBIS_PACKET_TYPE_COMMENT: u8 = 3;
/// The packet type for a setup header.
const VORBIS_PACKET_TYPE_SETUP: u8 = 5;

/// The common header packet signature.
const VORBIS_HEADER_PACKET_SIGNATURE: &[u8] = b"vorbis";

/// The Vorbis version supported by this mapper.
const VORBIS_VERSION: u32 = 0;

/// The minimum block size (64) expressed as a power-of-2 exponent.
const VORBIS_BLOCKSIZE_MIN: u8 = 6;
/// The maximum block size (8192) expressed as a power-of-2 exponent.
const VORBIS_BLOCKSIZE_MAX: u8 = 13;

pub fn detect(buf: &[u8]) -> Result<Option<Box<dyn Mapper>>> {
    // The identification header packet must be the correct size.
    if buf.len() != VORBIS_IDENTIFICATION_HEADER_SIZE {
        return Ok(None);
    }

    let mut reader = BufReader::new(&buf);

    // The packet type must be an identification header.
    let packet_type = reader.read_u8()?;

    if packet_type != VORBIS_PACKET_TYPE_IDENTIFICATION {
        return Ok(None);
    }

    // Next, the header packet signature must be correct.
    let mut packet_sig_buf = [0; 6];
    reader.read_buf_exact(&mut packet_sig_buf)?;

    if packet_sig_buf != VORBIS_HEADER_PACKET_SIGNATURE {
        return Ok(None);
    }

    // Next, the Vorbis version must be 0 for this mapper.
    let version = reader.read_u32()?;

    if version != VORBIS_VERSION {
        return Ok(None);
    }

    // Next, the number of channels and sample rate must be non-zero.
    let channels = reader.read_u8()?;
    let sample_rate = reader.read_u32()?;

    if channels == 0 || sample_rate == 0 {
        warn!("ogg: vorbis stream must not have a sample rate or channel count of 0");
        return Ok(None);
    }

    // Read the bitrate range.
    let _bitrate_max = reader.read_u32()?;
    let _bitrate_nom = reader.read_u32()?;
    let _bitrate_min = reader.read_u32()?;

    // Next, blocksize_0 and blocksize_1 are packed into a single byte.
    let block_sizes = reader.read_u8()?;

    let bs0_exp = (block_sizes & 0xf0) >> 4;
    let bs1_exp = (block_sizes & 0x0f) >> 0;

    // The block sizes must not exceed the bounds.
    if bs0_exp < VORBIS_BLOCKSIZE_MIN || bs0_exp > VORBIS_BLOCKSIZE_MAX {
        warn!("ogg: vorbis blocksize_0 out-of-bounds");
        return Ok(None);
    }

    if bs1_exp < VORBIS_BLOCKSIZE_MIN || bs1_exp > VORBIS_BLOCKSIZE_MAX {
        warn!("ogg: vorbis blocksize_1 out-of-bounds");
        return Ok(None);
    }

    // Blocksize_0 must be >= blocksize_1
    if bs1_exp > bs0_exp {
        warn!("ogg: vorbis blocksize_1 exceeds blocksize_0");
        return Ok(None);
    }

    // Populate the codec parameters with the information above.
    let mut codec_params = CodecParameters::new();

    codec_params
        .for_codec(CODEC_TYPE_VORBIS)
        .with_sample_rate(sample_rate)
        .with_extra_data(Box::from(buf));

    // Instantiate the Vorbis mapper.
    let mapper = Box::new(VorbisMapper {
        codec_params,
        has_setup_header: false,
    });

    Ok(Some(mapper))
}

struct VorbisMapper {
    codec_params: CodecParameters,
    has_setup_header: bool,
}

impl Mapper for VorbisMapper {

    fn codec(&self) -> &CodecParameters {
        &self.codec_params
    }

    fn map_packet(&mut self, packet: &OggPacket) -> Result<MapResult> {
        let mut reader = BufReader::new(&packet.data);

        // All Vorbis packets indicate the packet type in the first byte.
        let packet_type = reader.read_u8()?;

        // An even numbered packet type is an audio packet.
        if packet_type % 2 == 0 {
            // TODO: Decode the correct timestamp and duration.
            Ok(MapResult::Bitstream(Bitstream { ts: 0, dur: 0 }))
        }
        else {
            // Odd numbered packet types are header packets.
            let mut packet_sig_buf = [0; 6];
            reader.read_buf_exact(&mut packet_sig_buf)?;

            // Check if the presumed header packet has the common header packet signature.
            if packet_sig_buf != VORBIS_HEADER_PACKET_SIGNATURE {
                return decode_error("ogg: vorbis header packet signature invalid");
            }

            // Handle each header packet type specifically.
            match packet_type {
                VORBIS_PACKET_TYPE_COMMENT => {
                    let mut builder = MetadataBuilder::new();

                    vorbis::read_comment_no_framing(&mut reader, &mut builder)?;

                    Ok(MapResult::Metadata(builder.metadata()))
                }
                VORBIS_PACKET_TYPE_SETUP => {
                    // Append the setup headers to the extra data.
                    let mut extra_data = self.codec_params.extra_data.take().unwrap().to_vec();
                    extra_data.extend_from_slice(&packet.data);

                    self.codec_params.with_extra_data(extra_data.into_boxed_slice());

                    self.has_setup_header = true;

                    Ok(MapResult::Unknown)
                }
                _ => {
                    warn!("ogg: vorbis packet type {} unexpected", packet_type);
                    Ok(MapResult::Unknown)
                }
            }
        }
    }

    fn is_stream_ready(&self) -> bool {
        self.has_setup_header
    }

}