// Sonata
// Copyright (c) 2020 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::BTreeMap;

use sonata_core::checksum::Crc32;
use sonata_core::errors::{Result, decode_error};
use sonata_core::io::{ByteStream, BufStream, Monitor, MonitorStream};

use log::{debug, warn};

use super::page::{PageHeader, read_page_header};
use super::logical::LogicalStream;

pub struct OggPacket {
    pub serial: u32,
    pub data: Box<[u8]>,
}

#[derive(Default)]
pub struct PhysicalStream {
    page: PageHeader,
    stream_map: BTreeMap<u32, LogicalStream>,
}

impl PhysicalStream {

    pub fn current_page(&self) -> &PageHeader {
        &self.page
    }

    fn read_page<B: ByteStream>(&mut self, reader: &mut B) -> Result<()> {
        // Read the page header into a buffer.
        let mut page_header_buf = [0u8; 27];

        reader.read_buf_exact(&mut page_header_buf)?;

        // Parse the page header buffer.
        self.page = read_page_header(&mut BufStream::new(&page_header_buf))?;

        // trace!(
        //     "page {{ version={}, ts={}, serial={}, sequence={}, crc={:#x}, n_segments={}, \
        //         is_first={}, is_last={}, is_continuation={} }}",
        //     self.page.version,
        //     self.page.ts,
        //     self.page.serial,
        //     self.page.sequence,
        //     self.page.crc,
        //     self.page.n_segments,
        //     self.page.is_first_page,
        //     self.page.is_last_page,
        //     self.page.is_continuation,
        // );

        // The CRC of the OGG page requires the page checksum bytes to be zeroed.
        page_header_buf[22..26].copy_from_slice(&[0u8; 4]);

        // Instantiate a Crc32, initialize it with 0, and feed it the page header buffer.
        let mut crc32 = Crc32::new(0);

        crc32.process_buf_bytes(&page_header_buf);

        // The remainder of the page will be checksummed as it is read.
        let mut reader_crc32 = MonitorStream::new(reader, crc32);

        // If the page belongs to a new logical stream, instantiate it.
        if !self.stream_map.contains_key(&self.page.serial) {
            // TODO: Limit maximum number of streams.
            // TODO: Streams can only be created in groups.
            debug!("create packet buffer for stream with serial {:#x}", self.page.serial);
            self.stream_map.insert(self.page.serial, Default::default());
        }

        if let Some(logical_stream) = self.stream_map.get_mut(&self.page.serial) {

            logical_stream.read(&mut reader_crc32, &self.page)?;

            // Get the calculated CRC for the page.
            let calculated_crc = reader_crc32.monitor().crc();

            if self.page.crc != calculated_crc {
                warn!(
                    "crc mismatch: expected {:#x}, got {:#x}",
                    self.page.crc,
                    calculated_crc
                );

                // If the page was corrupt then reset the logical stream since its packet buffer
                // should either be empty or contain an incomplete packet. In the latter case, that
                // packet can no longer be completed so there is no harm in resetting the stream.
                logical_stream.reset();

                return decode_error("crc failure");
            }
        }

        Ok(())
    }

    pub fn next_packet<B: ByteStream>(&mut self, reader: &mut B) -> Result<OggPacket> {
        loop {
            // Read the next complete buffered packet. Complete packets can only be buffered in the
            // logical stream of the current page (if there any).
            let serial = self.page.serial;

            if let Some(logical_stream) = self.stream_map.get_mut(&serial) {
                if let Some(data) = logical_stream.next_packet() {
                    return Ok(OggPacket{ serial, data });
                }
            }

            // If there are no more complete buffered packets, or there are no logical streams, then
            // read in new page.
            self.read_page(reader)?;
        }
    }

    pub fn consume_packet(&mut self) {
        // Consume a packet from the logical stream belonging to the current page.
        if let Some(logical_stream) = self.stream_map.get_mut(&self.page.serial) {
            logical_stream.consume_packet();
        }
    }

}