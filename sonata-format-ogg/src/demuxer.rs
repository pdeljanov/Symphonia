// Sonata
// Copyright (c) 2020 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::BTreeMap;

use sonata_core::support_format;

use sonata_core::audio::Timestamp;
use sonata_core::errors::{Result, unsupported_error};
use sonata_core::formats::prelude::*;
use sonata_core::io::MediaSourceStream;
use sonata_core::meta::MetadataQueue;
use sonata_core::probe::{Descriptor, Instantiate, QueryDescriptor};

use super::physical::PhysicalStream;
use super::mappings;

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

    fn seek(&mut self, _ts: Timestamp) -> Result<u64> {
        unsupported_error("ogg seeking unsupported")
    }

}

