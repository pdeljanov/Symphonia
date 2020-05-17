// Sonata
// Copyright (c) 2020 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::{Mapper, MapResult};

use sonata_core::meta::MetadataBuilder;
use sonata_core::io::{BufStream, ByteStream};
use sonata_core::errors::Result;
use sonata_core::codecs::{CodecParameters, CODEC_TYPE_OPUS};
use sonata_core::audio::Channels;

use sonata_metadata::vorbis;

/// The minimum expected size of an Opus identification packet.
const OGG_OPUS_MIN_IDENTIFICATION_PACKET_SIZE: usize = 19;

/// The signature for an Opus identification packet.
const OGG_OPUS_MAGIC_SIGNATURE: &[u8] = b"OpusHead";

/// The signature for an Opus metadata packet.
const OGG_OPUS_COMMENT_SIGNATURE: &[u8] = b"OpusTags";

/// The maximum support Opus OGG mapping version.
const OGG_OPUS_MAPPING_VERSION_MAX: u8 = 0x0f;

pub fn detect(buf: &[u8]) -> Result<Option<Box<dyn Mapper>>> {
    // The identification packet for Opus must be a minimum size.
    if buf.len() < OGG_OPUS_MIN_IDENTIFICATION_PACKET_SIZE {
        return Ok(None);
    }

    let mut reader = BufStream::new(&buf);

    // The first 8 bytes are the magic signature ASCII bytes.
    let mut magic = [0; 8];
    reader.read_buf_exact(&mut magic)?;

    if magic != *OGG_OPUS_MAGIC_SIGNATURE {
        return Ok(None);
    }

    // The next byte is the OGG Opus encapsulation version. The version is split into two
    // sub-fields: major and minor. These fields are stored in the upper and lower 4-bit,
    // respectively.
    if reader.read_byte()? > OGG_OPUS_MAPPING_VERSION_MAX {
        return Ok(None);
    }

    // The next byte is the number of channels and must not be 0.
    let channel_count = reader.read_byte()?;

    if channel_count == 0 {
        return Ok(None);
    }

    // The next 16-bit integer is the pre-skip padding (# of samples at 48kHz to subtract from the
    // OGG granule position to obtain the PCM sample position).
    let pre_skip = reader.read_u16()?;

    // The next 32-bit integer is the sample rate of the original audio.
    let _ = reader.read_u32()?;

    // Next, the 16-bit gain value.
    let _ = reader.read_u16()?;

    // The next byte indicates the channel mapping. Most of these values are reserved.
    let channel_mapping = reader.read_byte()?;

    let channels = match channel_mapping {
        // RTP Mapping
        0 if channel_count == 1 => Channels::FRONT_LEFT,
        0 if channel_count == 2 => Channels::FRONT_LEFT | Channels::FRONT_RIGHT,
        // Vorbis Mapping
        1 => {
            match channel_count {
                1 => Channels::FRONT_LEFT,
                2 => Channels::FRONT_LEFT | Channels::FRONT_RIGHT,
                3 => Channels::FRONT_LEFT | Channels::FRONT_CENTRE | Channels::FRONT_RIGHT,
                4 => {
                    Channels::FRONT_LEFT
                        | Channels::FRONT_RIGHT
                        | Channels::REAR_LEFT
                        | Channels::REAR_RIGHT
                }
                5 => {
                    Channels::FRONT_LEFT
                        | Channels::FRONT_CENTRE
                        | Channels::FRONT_RIGHT
                        | Channels::REAR_LEFT
                        | Channels::REAR_RIGHT
                }
                6 => {
                    Channels::FRONT_LEFT
                        | Channels::FRONT_CENTRE
                        | Channels::FRONT_RIGHT
                        | Channels::REAR_LEFT
                        | Channels::REAR_RIGHT
                        | Channels::LFE1
                }
                7 => {
                    Channels::FRONT_LEFT
                        | Channels::FRONT_CENTRE
                        | Channels::FRONT_RIGHT
                        | Channels::SIDE_LEFT
                        | Channels::SIDE_RIGHT
                        | Channels::REAR_CENTRE
                        | Channels::LFE1
                }
                8 => {
                    Channels::FRONT_LEFT
                        | Channels::FRONT_CENTRE
                        | Channels::FRONT_RIGHT
                        | Channels::SIDE_LEFT
                        | Channels::SIDE_RIGHT
                        | Channels::REAR_LEFT
                        | Channels::REAR_RIGHT
                        | Channels::LFE1
                }
                _ => return Ok(None)
            }
        }
        // Reserved, and should NOT be supported for playback.
        _ => return Ok(None)
    };

    // Populate the codec parameters with the information read from identification header.
    let mut codec_params = CodecParameters::new();

    codec_params
        .for_codec(CODEC_TYPE_OPUS)
        .with_leading_padding(u32::from(pre_skip))
        .with_sample_rate(48_000)
        .with_channels(channels)
        .with_extra_data(Box::from(buf));

    // Instantiate the Opus mapper.
    let mapper = Box::new(OpusMapper {
        codec_params,
        need_comment: true,
    });

    Ok(Some(mapper))
}

struct OpusMapper {
    codec_params: CodecParameters,
    need_comment: bool,
}

impl Mapper for OpusMapper {

    fn codec(&self) -> &CodecParameters {
        &self.codec_params
    }

    fn map_packet(&mut self, buf: &[u8]) -> Result<MapResult> {
        // After the comment packet there should only be bitstream packets.
        if !self.need_comment {
            Ok(MapResult::Bitstream)
        }
        else {
            // If the comment packet is still required, check if the packet is the comment packet.
            if buf.len() >= 8 && buf[..8] == *OGG_OPUS_COMMENT_SIGNATURE {
                // This packet should be a metadata packet containing a Vorbis Comment.
                let mut reader = BufStream::new(&buf[8..]);
                let mut builder = MetadataBuilder::new();

                vorbis::read_comment_no_framing(&mut reader, &mut builder)?;

                self.need_comment = false;

                Ok(MapResult::Metadata(builder.metadata()))
            }
            else {
                Ok(MapResult::Unknown)
            }
        }

    }

}