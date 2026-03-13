// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::audio::{Channels, Position};
use symphonia_core::errors::{Result, decode_error};

/// Get the mapping 0 channel listing for the given number of channels.
pub fn vorbis_channels_to_channels(num_channels: u8) -> Option<Channels> {
    let positions = match num_channels {
        1 => Position::FRONT_LEFT,
        2 => Position::FRONT_LEFT | Position::FRONT_RIGHT,
        3 => Position::FRONT_LEFT | Position::FRONT_CENTER | Position::FRONT_RIGHT,
        4 => {
            Position::FRONT_LEFT
                | Position::FRONT_RIGHT
                | Position::REAR_LEFT
                | Position::REAR_RIGHT
        }
        5 => {
            Position::FRONT_LEFT
                | Position::FRONT_CENTER
                | Position::FRONT_RIGHT
                | Position::REAR_LEFT
                | Position::REAR_RIGHT
        }
        6 => {
            Position::FRONT_LEFT
                | Position::FRONT_CENTER
                | Position::FRONT_RIGHT
                | Position::REAR_LEFT
                | Position::REAR_RIGHT
                | Position::LFE1
        }
        7 => {
            Position::FRONT_LEFT
                | Position::FRONT_CENTER
                | Position::FRONT_RIGHT
                | Position::SIDE_LEFT
                | Position::SIDE_RIGHT
                | Position::REAR_CENTER
                | Position::LFE1
        }
        8 => {
            Position::FRONT_LEFT
                | Position::FRONT_CENTER
                | Position::FRONT_RIGHT
                | Position::SIDE_LEFT
                | Position::SIDE_RIGHT
                | Position::REAR_LEFT
                | Position::REAR_RIGHT
                | Position::LFE1
        }
        _ => return None,
    };

    Some(Channels::Positioned(positions))
}

// Xiph lacing for three packets starts with `2`, if it's not 2 assume the extradata is
// parseable as a raw Vorbis identification header (which must start with a `1`).
pub const XIPH_LACED_LEADING_HEADER: u8 = 2;

/// Unpack Vorbis extradata packed in the Xiph lacing format (used by WebM/Matroska).
///
/// If the data is not Xiph laced (does not start with 2), it is returned as is.
/// If it is Xiph laced, it extracts the Identification and Setup packets and returns
/// them concatenated.
pub fn unpack_xiph_laced_extradata(extradata: &[u8]) -> Result<Vec<u8>> {
    if extradata.is_empty() {
        return decode_error("vorbis: extradata is empty");
    }

    if extradata[0] != XIPH_LACED_LEADING_HEADER {
        return decode_error("vorbis: invalid Xiph lacing count");
    }

    let mut offset = 1;
    let mut lengths = Vec::new();

    for _ in 0..XIPH_LACED_LEADING_HEADER {
        let mut length = 0;
        let mut reached_end = false;
        while offset < extradata.len() {
            let val = extradata[offset] as usize;
            offset += 1;
            length += val;

            if val < 255 {
                reached_end = true;
                break;
            }
        }
        if !reached_end {
            return decode_error("vorbis: truncated length lacing");
        }
        lengths.push(length);
    }

    let ident_len = lengths[0];
    let comment_len = lengths[1];

    if offset >= extradata.len() {
        return decode_error("vorbis: no data remains after reading lacing");
    }

    if offset + ident_len + comment_len > extradata.len() {
        return decode_error("vorbis: header lengths exceed buffer size");
    }

    let remaining_data = &extradata[offset..];

    let id_packet = &remaining_data[..ident_len];
    let setup_packet = &remaining_data[ident_len + comment_len..];

    let mut unpacked = Vec::with_capacity(ident_len + setup_packet.len());
    unpacked.extend_from_slice(id_packet);
    unpacked.extend_from_slice(setup_packet);

    Ok(unpacked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unpack_xiph_laced_extradata_valid() {
        let id_packet = b"id_packet";
        let comment_packet = b"comment_packet";
        let setup_packet = b"setup_packet";

        let mut extradata = Vec::new();
        extradata.push(XIPH_LACED_LEADING_HEADER);
        extradata.push(id_packet.len() as u8);
        extradata.push(comment_packet.len() as u8);
        extradata.extend_from_slice(id_packet);
        extradata.extend_from_slice(comment_packet);
        extradata.extend_from_slice(setup_packet);

        let result = unpack_xiph_laced_extradata(&extradata).unwrap();

        let mut expected = Vec::new();
        expected.extend_from_slice(id_packet);
        expected.extend_from_slice(setup_packet);

        assert_eq!(result, expected);
    }

    #[test]
    fn test_unpack_xiph_laced_extradata_truncated() {
        let id_packet = b"id";
        let comment_packet = b"comment";

        let mut extradata = Vec::new();
        extradata.push(XIPH_LACED_LEADING_HEADER);
        extradata.push(id_packet.len() as u8);
        extradata.push(comment_packet.len() as u8);
        extradata.extend_from_slice(id_packet);

        let result = unpack_xiph_laced_extradata(&extradata);
        assert!(result.is_err());
    }

    #[test]
    fn test_unpack_xiph_laced_extradata_large_sizes() {
        let setup_packet = b"setup";

        let mut extradata = Vec::new();
        extradata.push(XIPH_LACED_LEADING_HEADER);
        extradata.push(255);
        extradata.push(0);
        extradata.push(0);
        extradata.extend_from_slice(&vec![0; 255]);
        extradata.extend_from_slice(setup_packet);

        let result = unpack_xiph_laced_extradata(&extradata).unwrap();

        let mut expected = Vec::new();
        expected.extend_from_slice(&vec![0; 255]);
        expected.extend_from_slice(setup_packet);

        assert_eq!(result, expected);
    }
}
