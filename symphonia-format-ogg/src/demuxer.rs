// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::BTreeMap;
use std::io::{Seek, SeekFrom};

use symphonia_core::checksum::Crc32;
use symphonia_core::errors::{Result, SeekErrorKind, decode_error, seek_error, reset_error};
use symphonia_core::formats::prelude::*;
use symphonia_core::io::{MediaSource, MediaSourceStream};
use symphonia_core::io::{BufReader, Monitor, MonitorStream, ReadBytes};
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};
use symphonia_core::support_format;

use log::{debug, info, warn};

use super::common::OggPacket;
use super::logical::LogicalStream;
use super::mappings;
use super::page::*;

/// OGG demultiplexer.
///
/// `OggReader` implements a demuxer for Xiph's OGG container format.
pub struct OggReader {
    reader: MediaSourceStream,
    tracks: Vec<Track>,
    cues: Vec<Cue>,
    metadata: MetadataLog,
    /// The current page.
    page: PageHeader,
    /// `Mapper` for each serial.
    mappers: BTreeMap<u32, Box<dyn mappings::Mapper>>,
    /// `LogicalStream` for each serial.
    streams: BTreeMap<u32, LogicalStream>,
    physical_stream_lower_pos: u64,
    physical_stream_upper_pos: u64,
}

impl OggReader {

    fn read_page(&mut self) -> Result<()> {
        let mut page_header_buf = [0u8; OGG_PAGE_HEADER_SIZE];
        page_header_buf[..4].copy_from_slice(&OGG_PAGE_MARKER);

        // Synchronize to an OGG page capture pattern.
        sync_page(&mut self.reader)?;

        // Read the part of the page header after the capture pattern into a buffer.
        self.reader.read_buf_exact(&mut page_header_buf[4..])?;

        // Parse the page header buffer.
        let page = read_page_header(&mut BufReader::new(&page_header_buf))?;

        // debug!(
        //     "page {{ version={}, ts={}, serial={}, sequence={}, crc={:#x}, n_segments={}, \
        //         is_first={}, is_last={}, is_continuation={} }}",
        //     page.version,
        //     page.ts,
        //     page.serial,
        //     page.sequence,
        //     page.crc,
        //     page.n_segments,
        //     page.is_first_page,
        //     page.is_last_page,
        //     page.is_continuation,
        // );

        // The CRC of the OGG page requires the page checksum bytes to be zeroed.
        page_header_buf[22..26].copy_from_slice(&[0u8; 4]);

        // Instantiate a Crc32, initialize it with 0, and feed it the page header buffer.
        let mut crc32 = Crc32::new(0);

        crc32.process_buf_bytes(&page_header_buf);

        // The remainder of the page will be checksummed as it is read.
        let mut reader_crc32 = MonitorStream::new(&mut self.reader, crc32);

        // If the page is marked as the first page, then this *may* be the start of a new logical
        // stream. However, this could just be page corruption, so read the first page fully and
        // only after verifying the CRC to be correct add the new logical stream.
        if page.is_first_page {
            // Create a new logical stream.
            let mut stream = LogicalStream::new(page.serial);

            // Read the page contents into the new logical stream.
            stream.read(&mut reader_crc32, &page)?;

            // Get the calculated CRC for the page.
            let calculated_crc = reader_crc32.monitor().crc();

            // If the CRC is correct for the page, add the new logical stream.
            if page.crc == calculated_crc {
                debug!("create logical stream with serial={:#x}", page.serial);
                self.streams.insert(page.serial, stream);

                // Update the current page.
                self.page = page;
            }
        }
        else if let Some(stream) = self.streams.get_mut(&page.serial) {
            // For non-first pages, if there is an associated logical stream, read the page contents
            // into the logical stream.
            stream.read(&mut reader_crc32, &page)?;

            // Get the calculated CRC for the page.
            let calculated_crc = reader_crc32.monitor().crc();

            // If the CRC for the page is incorrect, then the page is corrupt.
            if page.crc != calculated_crc {
                warn!(
                    "crc mismatch: expected {:#x}, got {:#x}",
                    page.crc,
                    calculated_crc
                );

                // Reset the logical stream since its packet buffer should either be empty or
                // contain an incomplete packet. In the latter case, that packet can no longer be
                // completed.
                stream.reset();

                return decode_error("crc failure");
            }

            // Update the current page.
            self.page = page;
        }
        else {
            // If there is no associated logical stream with this page, then this is a completely
            // random page within the physical stream. Discard it.
        }

        Ok(())
    }

    pub fn next_logical_packet(&mut self) -> Result<OggPacket> {
        loop {
            // Read the next packet. Packets can only ever be buffered in the logical stream of the
            // current page.
            if let Some(logical_stream) = self.streams.get_mut(&self.page.serial) {
                if let Some(packet) = logical_stream.next_packet() {
                    return Ok(packet);
                }
            }

            // If there are no packets, or there are no logical streams, then read a new page.
            self.read_page()?;
        }
    }

    pub fn consume_logical_packet(&mut self) {
        // Consume a packet from the logical stream belonging to the current page and get the
        // number of packets buffered.
        if let Some(logical_stream) = self.streams.get_mut(&self.page.serial) {
            logical_stream.consume_packet();
        }
    }

    pub fn do_seek(&mut self, serial: u32, required_ts: u64) -> Result<SeekedTo> {
        // If the reader is seekable, then use the bisection method to coarsely seek to the nearest
        // page that ends before the required timestamp.
        let seek_ts = if self.reader.is_seekable() {
            let original_pos = self.reader.pos();

            // TODO: This should start searching AFTER the header packets for the selected stream.
            let mut start_byte_offset = self.physical_stream_lower_pos;
            let mut end_byte_offset = self.reader.len().unwrap();

            // Bisection method.
            let bisected_loc = loop {
                // Find the middle of the upper and lower byte search range.
                let mid_byte_offset = (start_byte_offset + end_byte_offset) / 2;

                // Seek to the middle of the byte range.
                self.reader.seek(SeekFrom::Start(mid_byte_offset))?;

                // Resync the first page of the stream identified by serial. If it cannot be found
                // then the seek is out-of-range.
                let page0 = match resync_page_serial(&mut self.reader, serial) {
                    Ok(page0) => page0,
                    _ => break seek_error(SeekErrorKind::OutOfRange),
                };

                // Read the next page after the first of the stream identified by serial so that
                // a duration can be established for the first page.
                let page1 = match resync_page_serial(&mut self.reader, serial) {
                    Ok(page1) => page1,
                    _ => {
                        // If page0 has a timestamp <= the required timestamp, and there are no more
                        // pages for that stream (hence this error), then the seek is out-of-range.
                        if page0.header.ts < required_ts {
                            break seek_error(SeekErrorKind::OutOfRange);
                        }
                        else {
                            break Ok((page0.pos, page0.header.ts));
                        }
                    }
                };

                // TODO: Handle the case where we enter a chained physical stream (a new serial
                // is observed for page0 or page1).

                debug!("bisect step: ts0={} ts1={}", page0.header.ts, page1.header.ts);

                if required_ts < page0.header.ts {
                    // The required timestamp is less-than the timestamp of the final sample in the
                    // last complete packet of page0. Update the upper bound and bisect again.
                    end_byte_offset = mid_byte_offset;
                }
                else if required_ts > page1.header.ts {
                    // The required timestamp is greater-than the timestamp of the final sample in
                    // the last complete packet of page1. Update the lower bound and bisect again.
                    start_byte_offset = mid_byte_offset;
                }
                else {
                    // The required timestamp is greater-than the timestamp of the final sample in
                    // the last packet of page0, but less than the final sample of the last packet
                    // in page1. Therefore, the actual packet to seek to is contained in either
                    // page0 or page1.
                    break Ok((page0.pos, page0.header.ts));
                }

                // Protect against infinite iteration.
                if end_byte_offset == start_byte_offset {
                    break Ok((page0.pos, 0));
                }
            };

            let bisected_ts = match bisected_loc {
                Ok((pos, ts)) => {
                    // The bisection succeeded, seek to the start of the returned page.
                    self.reader.seek(SeekFrom::Start(pos))?;
                    ts
                }
                Err(err) => {
                    // The bisection failed, seek back to where we started and return an error.
                    self.reader.seek(SeekFrom::Start(original_pos))?;
                    return Err(err);
                }
            };

            // Reset all logical bitstreams since the physical stream will be reading from a new
            // location now.
            for stream in self.streams.values_mut() {
                stream.reset();
            }

            bisected_ts
        }
        else {
            // The reader is not seekable so it is only possible to emulate forward seeks by
            // consuming packets. Check if the required timestamp has been passed, and if so,
            // return an error.
            if let Some(stream) = self.streams.get(&serial) {
                // Note that the stream's base timestamp is the timestamp of the first packet in the
                // current page belonging to the stream. Therefore, the next /packet/ may actually
                // have a timestamp greater-than the stream's base timestamp. Therefore, the
                // required timestamp must be strictly less-than the base timestamp to ensure
                // sample-accurate seeking is possible.
                if stream.base_ts() >= required_ts {
                    return seek_error(SeekErrorKind::ForwardOnly);
                }
            }

            required_ts
        };

        // Consume packets until reaching the desired timestamp for both bisection and
        // forward-seeking methods.
        let actual_ts = loop {
            let packet = self.next_logical_packet()?;

            // The next packet has a base timestamp greater-than or equal-to the timestamp we're
            // seeking to. Don't consume the packet, and break out of the loop with the actual
            // timestamp.
            if packet.serial == serial && packet.base_ts >= seek_ts {
                break packet.base_ts;
            }

            self.consume_logical_packet();
        };

        debug!(
            "seeked track={:#x} to packet_ts={} (delta={})",
            serial, actual_ts, actual_ts as i64 - required_ts as i64);

        Ok(SeekedTo { track_id: serial, actual_ts, required_ts })
    }

    fn start_new_physical_stream(&mut self) -> Result<()> {
        // The new mapper set.
        let mut mappers = BTreeMap::<u32, Box<dyn mappings::Mapper>>::new();

        // The start of page position.
        let mut physical_stream_lower_pos;

        // The first page of each logical stream, marked with the first page flag, must contain the
        // identification packet for the encapsulated codec bitstream. The first page for each
        // logical stream from the current logical stream group must appear before any other pages.
        // That is to say, if there are N logical tracks, then the first N pages must contain the
        // identification packets for each respective stream.
        loop {
            physical_stream_lower_pos = self.reader.pos();

            let packet = self.next_logical_packet()?;

            // If the page containing packet is not the first-page of a logical stream, then the
            // packet is not an identification packet. This terminates the identification packet
            // group.
            if !self.page.is_first_page {
                break;
            }

            self.consume_logical_packet();

            // If a stream mapper has been detected, register the mapper for the stream's serial
            // number.
            if let Some(mapper) = mappings::detect(&packet.data)? {
                info!("selected mapper for stream with serial={:#x}", packet.serial);
                mappers.insert(packet.serial, mapper);
            }
        }

        // Each logical stream may contain additional header packets after the identification packet
        // that contains format-relevant information such as extra data and metadata. These packets,
        // for all logical streams, should be grouped together after the identification packets.
        loop {
            let packet = self.next_logical_packet()?;

            // If the packet belongs to a logical stream, and it is a metadata packet, push the
            // parsed metadata onto the revision log. If the packet was consumed by the mapper
            // or is unknown, continute iterating. Exit from this loop for any other packet.
            if let Some(mapper) = mappers.get_mut(&packet.serial) {
                match mapper.map_packet(&packet)? {
                    mappings::MapResult::Metadata(rev) => self.metadata.push(rev),
                    mappings::MapResult::Unknown => (),
                    _ => break,
                }
            }

            // Consume the packet.
            self.consume_logical_packet();

            physical_stream_lower_pos = self.reader.pos();
        }

        // At this point it can safely be assumed that a new physical stream is starting.
        info!("starting new physical stream");

        // First, clear the existing track listing.
        self.tracks.clear();

        // Second, add a track for all mappers.
        for (&serial, mapper) in mappers.iter() {
            self.tracks.push(Track::new(serial, mapper.codec().clone()));

            // Warn if the track is not ready. This should not happen if the physical stream was
            // muxed properly.
            if !mapper.is_stream_ready() {
                warn!("track for serial={:#x} may not be ready", serial);
            }
        }

        // Third, remove all logical streams that are not associated with the new mapper set. This
        // effectively removes all the logical streams from the previous physical stream.
        self.streams.retain(|serial, _| mappers.contains_key(serial));

        // Fourth, replace the previous set of mappers with the new set.
        self.mappers = mappers;

        // Last, store the lower and upper byte boundaries of the physical stream.
        self.physical_stream_lower_pos = physical_stream_lower_pos;
        self.physical_stream_upper_pos = 0;

        Ok(())
    }
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

    fn try_new(source: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {
        let mut ogg = OggReader {
            reader: source,
            tracks: Default::default(),
            cues: Default::default(),
            metadata: Default::default(),
            mappers: Default::default(),
            streams: Default::default(),
            page: Default::default(),
            physical_stream_lower_pos: 0,
            physical_stream_upper_pos: 0,
        };

        ogg.start_new_physical_stream()?;

        Ok(ogg)
    }

    fn next_packet(&mut self) -> Result<Packet> {
        // Loop until a bitstream packet is read from the physical stream.
        loop {
            // Get the next packet, and consume it immediately.
            let ogg_packet = self.next_logical_packet()?;

            // If a new logical stream started with this packet, then assume a new physical stream
            // has started.
            if self.page.is_first_page {
                self.start_new_physical_stream()?;
                return reset_error();
            }

            self.consume_logical_packet();

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
                        // Push metadata onto the log.
                        self.metadata.push(metadata);
                    }
                    _ => (),
                }
            }
        }
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn cues(&self) -> &[Cue] {
        &self.cues
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        // Get the timestamp of the desired audio frame.
        let (required_ts, serial) = match to {
            // Frame timestamp given.
            SeekTo::TimeStamp { ts, track_id } => {
                // Check if the user provided an invalid track ID.
                if !self.mappers.contains_key(&track_id) {
                    return seek_error(SeekErrorKind::InvalidTrack);
                }

                (ts, track_id)
            }
            // Time value given, calculate frame timestamp from sample rate.
            SeekTo::Time { time, track_id } => {
                // Get the track serial.
                let serial = if let Some(serial) = track_id {
                    serial
                }
                else if let Some(default_track) = self.default_track() {
                    default_track.id
                }
                else {
                    // No tracks.
                    return seek_error(SeekErrorKind::Unseekable);
                };

                // Convert the time to a timestamp.
                let ts = if let Some(mapper) = self.mappers.get_mut(&serial) {
                    if let Some(sample_rate) = mapper.codec().sample_rate {
                        TimeBase::new(1, sample_rate).calc_timestamp(time)
                    }
                    else {
                        // No sample rate. This should never happen.
                        return seek_error(SeekErrorKind::Unseekable);
                    }
                }
                else {
                    // No mapper for track. The user provided a bad track ID.
                    return seek_error(SeekErrorKind::InvalidTrack);
                };

                (ts, serial)
            }
        };

        debug!("seeking track={:#x} to frame_ts={}", serial, required_ts);

        // Ask the physical stream to seek.
        self.do_seek(serial, required_ts)
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.reader
    }
}