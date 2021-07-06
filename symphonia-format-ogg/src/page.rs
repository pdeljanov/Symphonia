// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::io::ReadBytes;

const OGG_PAGE_MARKER: [u8; 4] = *b"OggS";

#[derive(Default)]
pub struct PageHeader {
    pub version: u8,
    pub ts: u64,
    pub serial: u32,
    pub sequence: u32,
    pub crc: u32,
    pub n_segments: u8,
    pub is_continuation: bool,
    pub is_first_page: bool,
    pub is_last_page: bool,
}

/// Reads a `PageHeader` from the the provided `Bytestream`.
pub fn read_page_header<B: ReadBytes>(reader: &mut B) -> Result<PageHeader> {
    // The OggS marker should be present.
    let marker = reader.read_quad_bytes()?;

    if marker != OGG_PAGE_MARKER {
        return unsupported_error("missing ogg stream marker");
    }

    let version = reader.read_byte()?;

    // There is only one OGG version, and that is version 0.
    if version != 0 {
        return unsupported_error("invalid ogg version");
    }

    let flags = reader.read_byte()?;

    // Only the first 3 least-significant bits are used for flags.
    if flags & 0xf8 != 0 {
        return decode_error("invalid flag bits set");
    }

    let ts = reader.read_u64()?;
    let serial = reader.read_u32()?;
    let sequence = reader.read_u32()?;
    let crc = reader.read_u32()?;
    let n_segments = reader.read_byte()?;

    Ok(PageHeader {
        version,
        ts,
        serial,
        sequence,
        crc,
        n_segments,
        is_continuation: (flags & 0x01) != 0,
        is_first_page: (flags & 0x02) != 0,
        is_last_page: (flags & 0x04) != 0,
    })
}
