// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::VecDeque;

use symphonia_core::errors::Result;
use symphonia_core::io::ReadBytes;

use log::{debug, error, warn};

use super::common::OggPacket;
use super::page::PageHeader;

struct OggPacketInfo {
    base_ts: u64,
    len: usize,
}

pub struct LogicalStream {
    serial: u32,
    buf: Vec<u8>,
    read_from: usize,
    write_at: usize,
    packets: VecDeque<OggPacketInfo>,
    base_ts: u64,
}

impl LogicalStream {
    const MAX_BUFFER_LEN: usize = 256 * 1024 * 1024;

    pub fn new(serial: u32) -> LogicalStream {
        LogicalStream {
            serial,
            buf: Default::default(),
            read_from: 0,
            write_at: 0,
            packets: Default::default(),
            base_ts: 0,
        }
    }

    fn compact(&mut self) {
        if self.read_from > 0 {
            self.buf.copy_within(self.read_from..self.write_at, 0);
            self.write_at -= self.read_from;
            self.read_from = 0;
        }
    }

    fn write(&mut self, len: usize) -> &mut [u8] {
        debug_assert!(len <= 64 * 1024, "ogg pages are <= 64kB");

        // Attempt to compact the buffer first.
        self.compact();

        let next_write_at = self.write_at + len;

        if next_write_at >= self.buf.len() {
            let new_buf_size = next_write_at + (8 * 1024 - 1) & !(8 * 1024 - 1);
            debug!("grow packet buffer to {} bytes", new_buf_size);

            if new_buf_size > LogicalStream::MAX_BUFFER_LEN {
                error!("packet buffer would exceed max size");
            }

            self.buf.resize(new_buf_size, Default::default());
        }

        let slice = &mut self.buf[self.write_at..next_write_at];

        self.write_at = next_write_at;

        slice
    }

    /// Clears all packets from the logical stream and resets the stream's packet queue.
    pub fn reset(&mut self) {
        self.read_from = 0;
        self.write_at = 0;
        self.base_ts = 0;
        self.packets.clear();
    }

    /// Read the body of a page from the provided `ByteStream` and enqueues complete packets onto
    /// the stream's packet queue.
    pub fn read<B: ReadBytes>(&mut self, reader: &mut B, page: &PageHeader) -> Result<()> {
        let mut payload_len = 0;
        let mut packet_len = self.write_at - self.read_from;

        if packet_len > 0 && !page.is_continuation {
            warn!("expected continuation page");

            // Expected a continuation page to complete an incomplete packet, however this page does
            // not continue a previous page, therefore the incomplete packet must be dropped.
            packet_len = 0;
            self.write_at -= packet_len;
        }

        // Read each segment length from the segment table and calculate length of each packet
        // contained within the page, and the total payload size.
        for _ in 0..page.n_segments {
            let segment_len = reader.read_byte()?;

            packet_len += usize::from(segment_len);
            payload_len += usize::from(segment_len);

            // trace!("  │  ├ segment {{ len: {} }}", segment_len);

            // A segment with a length < 255 indicates that the segment is the end of a packet.
            // Push the packet length into the packet queue for the stream.
            if segment_len < 255 {
                // trace!("  ├ packet {{ len: {} }}", packet_len);
                self.packets.push_back(OggPacketInfo { base_ts: self.base_ts, len: packet_len });
                packet_len = 0;
            }
        }

        // Load the page's payload in the packet buffer for the stream.
        let context_slice = self.write(payload_len);
        reader.read_buf_exact(context_slice)?;

        // An OGG Page timestamp is the timestamp at the *end* of all *complete* packets in the page.
        // Therefore, save this timestamp to use as the base timestamp for the next set of packets
        // in the next page.
        //
        // TODO: If no packets were completed in this page then do not update the base timestamp.
        self.base_ts = page.ts;

        Ok(())
    }

    /// Maybe gets the next complete packet that has been read and queued from the stream.
    pub fn next_packet(&self) -> Option<OggPacket> {
        match self.packets.front() {
            Some(packet_info) => {
                let data = Box::from(&self.buf[self.read_from..self.read_from + packet_info.len]);

                Some(OggPacket {
                    serial: self.serial,
                    base_ts: packet_info.base_ts,
                    data
                })
            },
            None => None
        }
    }

    /// Maybe consumes the next complete packet.
    pub fn consume_packet(&mut self) {
        match self.packets.pop_front() {
            Some(packet_info) => {
                self.read_from += packet_info.len;
            },
            None => ()
        }
    }

}