// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::BTreeMap;

use symphonia_core::support_format;
use symphonia_core::errors::{Result, unsupported_error};
use symphonia_core::formats::prelude::*;
use symphonia_core::io::MediaSourceStream;
use symphonia_core::meta::MetadataQueue;
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};

use log::info;

use super::physical::PhysicalStream;
use super::mappings;

/// Ogg demultiplexer.
///
/// `OggReader` implements a demuxer for Xiph's OGG container format.
pub struct OggReader {
    reader: MediaSourceStream,
    tracks: Vec<Track>,
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

        let mut tracks = Vec::new();
        let mut mappers = BTreeMap::<u32, Box<dyn mappings::Mapper>>::new();

        // The first page of each logical stream, marked with the first page flag, must contain the
        // identification packet for the encapsulated codec bitstream. The first page for each
        // logical stream from the current logical stream group must appear before any other pages.
        // That is to say, if there are N logical tracks, then the first N pages must contain the
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

            info!("discovered new stream: serial={:#x}", packet.serial);

            // If a stream mapper has been detected, the stream can be read.
            if let Some(mapper) = mappings::detect(&packet.data)? {
                // Add the stream.
                tracks.push(Track::new(packet.serial, mapper.codec().clone()));
                mappers.insert(packet.serial, mapper);
                
                info!("added stream: serial={:#x}", packet.serial);
            }
        }

        let mut metadata: MetadataQueue = Default::default();

        // Each logical stream may contain additional header packets after the identification packet
        // that contains format-relevant information such as metadata. These packets should be
        // immediately after the identification packets. As much as possible, read them now.
        loop {
            let packet = physical_stream.next_packet(&mut source)?;

            // If the packet belongs to a logical stream, and it is a metadata packet, push the
            // parsed metadata onto the revision queue. If it's an unknown packet, skip it. Exit
            // from this loop for any other packet.
            if let Some(mapper) = mappers.get_mut(&packet.serial) {
                match mapper.map_packet(&packet)? {
                    mappings::MapResult::Metadata(revision) => metadata.push(revision),
                    mappings::MapResult::Unknown => (),
                    _ => break
                }
            }

            // Consume the packet.
            physical_stream.consume_packet();
        }

        Ok(OggReader {
            reader: source,
            tracks,
            cues: Default::default(),
            metadata,
            physical_stream,
            mappers,
        })
    }

    fn next_packet(&mut self) -> Result<Packet> {
        // Loop until a bitstream packet is read from the physical stream.
        loop {
            // Get the next packet, and consume it immediately.
            let ogg_packet = self.physical_stream.next_packet(&mut self.reader)?;
            self.physical_stream.consume_packet();

            // If the packet belongs to a logical stream with a mapper, process it.
            if let Some(mapper) = self.mappers.get_mut(&ogg_packet.serial) {
                // Determine what to do with the packet.
                match mapper.map_packet(&ogg_packet)? {
                    mappings::MapResult::Bitstream(bitstream) => {
                        // Create a new audio data packet to return.
                        let packet = Packet::new_from_boxed_slice(
                            ogg_packet.serial,
                            bitstream.ts,
                            bitstream.dur,
                            ogg_packet.data
                        );

                        return Ok(packet);
                    }
                    mappings::MapResult::Metadata(metadata) => {
                        // Push metadata onto metadata queue.
                        self.metadata.push(metadata);
                    }
                    _ => {
                        info!("ignoring packet for serial={:#x}", ogg_packet.serial);
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

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn seek(&mut self, _mode: SeekMode, _to: SeekTo) -> Result<SeekedTo> {
        unsupported_error("ogg seeking unsupported")
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.reader
    }

}

