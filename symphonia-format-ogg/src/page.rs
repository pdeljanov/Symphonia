// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::checksum::Crc32;
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::io::{BufReader, Monitor, MonitorStream, ReadBytes};

pub const OGG_PAGE_MARKER: [u8; 4] = *b"OggS";

pub const OGG_PAGE_HEADER_SIZE: usize = 27;
pub const OGG_PAGE_MAX_SIZE: usize = OGG_PAGE_HEADER_SIZE + 255 + 255 * 255;

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

/// Reads a `PageHeader` from the the provided reader.
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

/// Quickly synchronizes the provided reader to the next OGG page capture pattern, but does not
/// perform any further verification.
pub fn sync_page<B: ReadBytes>(reader: &mut B) -> Result<()> {
    let mut marker = u32::from_be_bytes(reader.read_quad_bytes()?);

    while marker.to_be_bytes() != OGG_PAGE_MARKER {
        marker <<= 8;
        marker |= u32::from(reader.read_u8()?);
    }

    Ok(())
}

/// Result of a resync operation.
pub struct ResyncResult {
    /// The page header.
    pub header: PageHeader,
    /// The position of the OGG page.
    pub pos: u64,
}

/// Synchronizes the provided reader to the next OGG page, verifies the synchronization by reading
/// the page and computing the CRC, and returning the `PageHeader` and the position of the start of
/// the page.
pub fn resync_page<B: ReadBytes>(reader: &mut B) -> Result<ResyncResult> {
    let mut header_buf = [0u8; OGG_PAGE_HEADER_SIZE];
    header_buf[..4].copy_from_slice(&OGG_PAGE_MARKER);

    loop {
        // Sync to the next OGG page capture pattern.
        sync_page(reader)?;

        // Read the possible page header into a buffer.
        reader.read_buf_exact(&mut header_buf[4..])?;

        // Read the page header from the buffer. If the header is invalid then it is likely the
        // reader synchronized to part of a codec bitstream and not an actual header. Try again.
        let header = match read_page_header(&mut BufReader::new(&header_buf)) {
            Ok(header) => header,
            _ => continue,
        };

        let pos = reader.pos() - OGG_PAGE_HEADER_SIZE as u64;

        // The remainder of the page will be checksummed as it is read. Start calculating the CRC
        // from the page capture pattern.
        let mut crc = Crc32::new(0);

        // The page header CRC is zeroed when calculating the actual CRC.
        header_buf[22..26].copy_from_slice(&[0u8; 4]);
        crc.process_buf_bytes(&header_buf);

        let mut reader = MonitorStream::new(&mut *reader, crc);

        // Read the segment table to determine the data length of this page.
        let mut data_len = 0;

        for _ in 0..header.n_segments {
            data_len += reader.read_byte()? as usize;
        }

        // Read and discard the data.
        // TODO: This allocates, and ignore_bytes doesn't compute the CRC. Extend MonitorStream to
        // support this use-case (skipping bytes whilst computing the CRC) efficiently.
        let _ = reader.read_boxed_slice_exact(data_len);

        // If the page's calculated CRC matches the header, then return the page header.
        if reader.monitor().crc() == header.crc {
            return Ok(ResyncResult { header, pos, });
        }
    }
}

/// Performs the same operation as `resync_page` but synchronizes to the next OGG page with a
/// specific serial.
pub fn resync_page_serial<B: ReadBytes>(
    reader: &mut B,
    serial: u32,
    physical_stream_end: u64
) -> Option<ResyncResult> {
    loop {
        // Do not exceed the end of the physical stream.
        if reader.pos() >= physical_stream_end {
            break
        }

        let resync = match resync_page(reader) {
            Ok(resync) => resync,
            _ => break,
        };

        // Return if the synchronized page belongs to the logical bitstream specified by serial,
        // and it was not a continuation page (i.e., it was a fresh page).
        if resync.header.serial == serial && !resync.header.is_continuation {
            return Some(resync);
        }
    }

    None
}