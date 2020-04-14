// Sonata
// Copyright (c) 2020 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::VecDeque;

use sonata_core::errors::Result;
use sonata_core::io::ByteStream;

use super::page::PageHeader;

#[derive(Default)]
pub struct LogicalStream {
    buf: Vec<u8>,
    read_from: usize,
    write_at: usize,
    packets: VecDeque<usize>,
}

impl LogicalStream {
    const MAX_BUFFER_LEN: usize = 256 * 1024 * 1024;

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
            eprintln!("ogg: grow packet buffer to {} bytes", new_buf_size);

            if new_buf_size > LogicalStream::MAX_BUFFER_LEN {
                eprintln!("ogg: packet buffer would exceed max size");
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
        self.packets.clear();
    }

    /// Read the body of a page from the provided `ByteStream` and enqueues complete packets onto
    /// the stream's packet queue.
    pub fn read<B: ByteStream>(&mut self, reader: &mut B, page: &PageHeader) -> Result<()> {
        let mut payload_len = 0;
        let mut packet_len = self.write_at - self.read_from;

        if packet_len > 0 && !page.is_continuation {
            eprintln!("ogg: expected continuation page");

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

            // eprintln!("ogg:   │  ├ segment {{ len: {} }}", segment_len);

            // A segment with a length < 255 indicates that the segment is the end of a packet.
            // Push the packet length into the packet queue for the stream.
            if segment_len < 255 {
                // eprintln!("ogg:   ├ packet {{ len: {} }}", packet_len);
                self.packets.push_back(packet_len);
                packet_len = 0;
            }
        }

        // Load the page's payload in the packet buffer for the stream.
        let context_slice = self.write(payload_len);
        reader.read_buf_bytes(context_slice)?;

        Ok(())
    }

    /// Maybe gets the next complete packet that has been read and queued from the stream.
    pub fn next_packet(&self) -> Option<Box<[u8]>> {
        match self.packets.front() {
            Some(packet_len) => {
                let slice = &self.buf[self.read_from..self.read_from + packet_len];
                Some(Box::from(slice))
            },
            None => None
        }
    }

    /// Maybe consumes the next complete packet.
    pub fn consume_packet(&mut self) {
        match self.packets.pop_front() {
            Some(packet_len) => {
                self.read_from += packet_len;
            },
            None => ()
        }
    }

}