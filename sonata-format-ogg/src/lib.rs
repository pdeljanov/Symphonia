// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, VecDeque};

use sonata_core::support_format;

use sonata_core::audio::Timestamp;
use sonata_core::checksum::Crc32;
use sonata_core::errors::{Result, decode_error, unsupported_error};
use sonata_core::formats::prelude::*;
use sonata_core::io::{ByteStream, BufStream, MediaSourceStream, Monitor, MonitorStream};
use sonata_core::meta::MetadataQueue;
use sonata_core::probe::{Descriptor, Instantiate, QueryDescriptor};

mod mappings;

const OGG_STREAM_MARKER: [u8; 4] = *b"OggS";

/// Ogg demultiplexer.
///
/// `OggReader` implements a demuxer for Xiph's OGG container format.
pub struct OggReader {
    reader: MediaSourceStream,
    streams: Vec<Stream>,
    cues: Vec<Cue>,
    metadata: MetadataQueue,
    physical_stream: PhysicalStream,
    mappers: BTreeMap<u32, Box<dyn mappings::Mapper>>,
}

impl QueryDescriptor for OggReader {
    fn query() -> &'static [Descriptor] {
        &[
            support_format!(
                "ogg",
                "OGG",
                &[ "ogg", "ogv", "oga", "ogx", "ogm", "spx", "opus" ],
                &[ "video/ogg", "audio/ogg", "application/ogg" ],
                &[ b"OggS" ]
            ),
        ]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}

impl FormatReader for OggReader {

    fn try_new(mut source: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {
        let mut physical_stream: PhysicalStream = Default::default();

        let mut streams = Vec::new();
        let mut mappers = BTreeMap::<u32, Box<dyn mappings::Mapper>>::new();

        // The first page of each logical stream, marked with the first page flag, must contain the
        // identification packet for the encapsulated codec bitstream. The first page for each
        // logical stream from the current logical stream group must appear before any other pages.
        // That is to say, if there are N logical streams, then the first N pages must contain the
        // identification packets for each respective stream.
        loop {
            let packet = physical_stream.next_packet(&mut source)?;

            // If the page containing packet is not the first-page of the logical stream, then the
            // packet is not an identification packet. Don't consume the packet and exit stream
            // discovery.
            if !physical_stream.current_page().is_first_page {
                break;
            }

            physical_stream.consume_packet();

            eprintln!("ogg: discovered new stream: serial={:#x}", packet.serial);

            // If a stream mapper has been detected, the stream can be read.
            if let Some(mapper) = mappings::detect(&packet.data)? {
                // Add the stream.
                streams.push(Stream::new(mapper.codec().clone()));
                mappers.insert(packet.serial, mapper);
                
                eprintln!("ogg: added stream: serial={:#x}", packet.serial);
            }
        }

        // Each logical stream may contain additional header packets after the identification packet
        // that contains format-relevant information such as metadata. Parse those packets now.
        // loop {
        //     let packet = parser.next_packet(&mut source)?;
        // }

        Ok(OggReader {
            reader: source,
            streams,
            cues: Default::default(),
            metadata: Default::default(),
            physical_stream,
            mappers,
        })
    }

    fn next_packet(&mut self) -> Result<Packet> {
        // Loop until a bitstream packet is read from the physical stream.
        loop {
            // Get the next packet, and consume it immediately.
            let packet = self.physical_stream.next_packet(&mut self.reader)?;
            self.physical_stream.consume_packet();

            // If the packet belongs to a logical stream with a mapper, process it.
            if let Some(mapper) = self.mappers.get_mut(&packet.serial) {
                // Determine what to do with the packet.
                match mapper.map_packet(&packet.data)? {
                    mappings::MapResult::Bitstream => {
                        return Ok(Packet::new_from_boxed_slice(0, 0, packet.data));
                    },
                    _ => {
                        eprintln!("ogg: ignoring packet for serial={:#x}", packet.serial);
                    }
                }
            }
        }
    }

    fn metadata(&self) -> &MetadataQueue {
        &self.metadata
    }

    fn cues(&self) -> &[Cue] {
        &self.cues
    }

    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn seek(&mut self, ts: Timestamp) -> Result<u64> {
        unsupported_error("ogg seeking unsupported")
    }

}

#[derive(Default)]
struct PageHeader {
    version: u8,
    ts: u64,
    serial: u32,
    sequence: u32,
    crc: u32,
    n_segments: u8,
    is_continuation: bool,
    is_first_page: bool,
    is_last_page: bool,
}

#[derive(Default)]
struct LogicalStream {
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
                eprintln!("ogg:   ├ packet {{ len: {} }}", packet_len);
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

struct OggPacket {
    serial: u32,
    data: Box<[u8]>,
}

#[derive(Default)]
struct PhysicalStream {
    page: PageHeader,
    stream_map: BTreeMap<u32, LogicalStream>,
}

impl PhysicalStream {

    fn current_page(&self) -> &PageHeader {
        &self.page
    }

    fn read_page<B: ByteStream>(&mut self, reader: &mut B) -> Result<()> {
        // Read the page header into a buffer.
        let mut page_header_buf = [0u8; 27];

        reader.read_buf_bytes(&mut page_header_buf)?;

        // Parse the page header buffer.
        self.page = read_page_header(&mut BufStream::new(&page_header_buf))?;

        eprintln!(
            "ogg: page {{ version={}, ts={}, serial={}, sequence={}, crc={:#x}, n_segments={}, \
                is_first={}, is_last={}, is_continuation={} }}",
            self.page.version,
            self.page.ts,
            self.page.serial,
            self.page.sequence,
            self.page.crc,
            self.page.n_segments,
            self.page.is_first_page,
            self.page.is_last_page,
            self.page.is_continuation,
        );

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
            eprintln!("ogg: create packet buffer for stream with serial {}", self.page.serial);
            self.stream_map.insert(self.page.serial, Default::default());
        }

        if let Some(logical_stream) = self.stream_map.get_mut(&self.page.serial) {

            logical_stream.read(&mut reader_crc32, &self.page)?;

            // Get the calculated CRC for the page.
            let calculated_crc = reader_crc32.monitor().crc();

            if self.page.crc != calculated_crc {
                eprintln!(
                    "ogg: crc mismatch: expected {:#x}, got {:#x}",
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

    fn next_packet<B: ByteStream>(&mut self, reader: &mut B) -> Result<OggPacket> {
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

    fn consume_packet(&mut self) {
        // Consume a packet from the logical stream belonging to the current page.
        if let Some(logical_stream) = self.stream_map.get_mut(&self.page.serial) {
            logical_stream.consume_packet();
        }
    }

}

/// Reads a `PageHeader` from the the provided `Bytestream`.
fn read_page_header<B: ByteStream>(reader: &mut B) -> Result<PageHeader> {
    // The OggS marker should be present.
    let marker = reader.read_quad_bytes()?;

    if marker != OGG_STREAM_MARKER {
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
