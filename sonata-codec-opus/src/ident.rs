// Sonata
// Copyright (c) 2020 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use sonata_core::errors::{Result, decode_error};
use sonata_core::io::{BufStream, ByteStream};

/// The signature for an Opus identification packet.
const OGG_OPUS_MAGIC_SIGNATURE: &[u8] = b"OpusHead";

/// Identification header.
pub struct IdentHeader {
    pub n_channels: u8,
    pub pre_skip: u16,
    pub in_sample_rate: u32,
    pub gain: u16,
    pub mapping_family: u8,
    pub stream_count: u8,
    pub coupled_count: u8,
    pub mapping: [u8; 8],
}

impl IdentHeader {

    pub fn parse(buf: &[u8]) -> Result<IdentHeader> {
        let mut reader = BufStream::new(buf);

        // The first 8 bytes are the magic signature ASCII bytes.
        let mut magic = [0; 8];
        reader.read_buf_exact(&mut magic)?;

        if magic != *OGG_OPUS_MAGIC_SIGNATURE {
            return decode_error("incorrect opus signature");
        }

        // The next byte is the OGG Opus encapsulation version.
        let _ = reader.read_byte()?;

        // The next byte is the number of channels and must not be 0.
        let n_channels = reader.read_byte()?;

        if n_channels == 0 {
            return decode_error("channel count is 0");
        }

        // The next 16-bit integer is the pre-skip padding.
        let pre_skip = reader.read_u16()?;

        // The next 32-bit integer is the sample rate of the original audio.
        let in_sample_rate = reader.read_u32()?;

        // Next, the 16-bit gain value.
        let gain = reader.read_u16()?;

        // The next byte indicates the channel mapping. Most of these values are reserved.
        let mapping_family = reader.read_byte()?;

        let (stream_count, coupled_count) = match mapping_family {
            // RTP mapping. Supports up-to 2 channels.
            0 => {
                if n_channels > 2 {
                    return decode_error("invalid number of channels");
                }

                (1, n_channels - 1)
            }
            // Vorbis mapping. Supports 1 to 8 channels.
            1 => {
                if n_channels > 8 {
                    return decode_error("invalid number of channels");
                }

                (reader.read_u8()?, reader.read_u8()?)
            }
            _ => return decode_error("reserved mapping family")
        };

        let mut mapping = [0; 8];
        
        // The channel mapping table is only read if not using the RTP mapping.
        if mapping_family != 0 {
            for mapping in &mut mapping[..n_channels as usize] {
                *mapping = reader.read_u8()?;
            }
        }

        Ok(IdentHeader {
            n_channels,
            pre_skip,
            in_sample_rate,
            gain,
            mapping_family,
            stream_count,
            coupled_count,
            mapping,
        })
    }
}