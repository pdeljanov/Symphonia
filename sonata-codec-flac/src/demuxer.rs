// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]

use std::io::{Seek, SeekFrom};

use sonata_core::support_format;

use sonata_core::audio::Timestamp;
use sonata_core::codecs::{CODEC_TYPE_FLAC, CodecParameters};
use sonata_core::errors::{Result, decode_error, seek_error, SeekErrorKind};
use sonata_core::formats::{Cue, ProbeDepth, ProbeResult, SeekIndex, SeekSearchResult, Stream};
use sonata_core::formats::{FormatDescriptor, FormatOptions, FormatReader, Packet};
use sonata_core::io::*;
use sonata_core::meta::{MetadataQueue, MetadataBuilder};

use super::decoder::PacketParser;
use super::metadata::*;

/// The FLAC start of stream marker: "fLaC" in ASCII.
const FLAC_STREAM_MARKER: [u8; 4] = *b"fLaC";

/// The recommended maximum number of bytes advance a stream to find the stream marker before giving
/// up.
const FLAC_PROBE_SEARCH_LIMIT: usize = 512 * 1024;

/// `Free Lossless Audio Codec (FLAC) native frame reader.
pub struct FlacReader {
    reader: MediaSourceStream,
    metadata: MetadataQueue,
    streams: Vec<Stream>,
    cues: Vec<Cue>,
    index: Option<SeekIndex>,
    first_frame_offset: u64,
}

impl FlacReader {

    /// Reads all the metadata blocks.
    fn read_all_metadata_blocks(&mut self) -> Result<()> {
        let mut metadata_builder = MetadataBuilder::new();

        loop {
            let header = MetadataBlockHeader::read(&mut self.reader)?;

            // Create a scoped bytestream to error if the metadata block read functions exceed the
            // stated length of the block.
            let mut block_stream = ScopedStream::new(&mut self.reader, u64::from(header.block_len));

            match header.block_type {
                MetadataBlockType::Application => {
                    // TODO: Store vendor data.
                    read_application_block(&mut block_stream, header.block_len)?;
                },
                // SeekTable blocks are parsed into a SeekIndex.
                MetadataBlockType::SeekTable => {
                    // Check if a SeekTable has already be parsed. If one has, then the file is
                    // invalid, atleast for seeking. Either way, it's a violation of the
                    // specification.
                    if self.index.is_none() {
                        let mut index = SeekIndex::new();
                        read_seek_table_block(&mut block_stream, header.block_len, &mut index)?;
                        self.index = Some(index);
                    }
                    else {
                        return decode_error("Found more than one SeekTable block.");
                    }
                },
                // VorbisComment blocks are parsed into Tags.
                MetadataBlockType::VorbisComment => {
                    read_comment_block(&mut block_stream, &mut metadata_builder)?;
                },
                // Cuesheet blocks are parsed into Cues.
                MetadataBlockType::Cuesheet => {
                    read_cuesheet_block(&mut block_stream, &mut self.cues)?;
                },
                // Picture blocks are read as Visuals.
                MetadataBlockType::Picture => {
                    read_picture_block(&mut block_stream, &mut metadata_builder)?;
                },
                // StreamInfo blocks are parsed into Streams.
                MetadataBlockType::StreamInfo => {
                    read_stream_info_block(&mut block_stream, &mut self.streams)?;
                },
                // Padding blocks are skipped.
                MetadataBlockType::Padding => {
                    block_stream.ignore_bytes(u64::from(header.block_len))?;
                },
                // Unknown block encountered. Skip these blocks as they may be part of a future
                // version of FLAC, but  print a message.
                MetadataBlockType::Unknown(id) => {
                    block_stream.ignore_bytes(u64::from(header.block_len))?;
                    eprintln!("Ignoring {} bytes of block width id={}.", header.block_len, id);
                }
            }

            // If the stated block length is longer than the number of bytes from the block read,
            // ignore the remaining unread data.
            let block_unread_len = block_stream.bytes_available();

            if block_unread_len > 0 {
                eprintln!("Under read block by {} bytes.", block_unread_len);
                block_stream.ignore_bytes(block_unread_len)?;
            }

            // Exit when the last header is read.
            if header.is_last {
                break;
            }
        }

        // Commit any read metadata to the metadata queue.
        self.metadata.push(metadata_builder.metadata());

        Ok(())
    }

}

impl FormatReader for FlacReader {

    fn open(source: MediaSourceStream, _options: &FormatOptions) -> Self {
        FlacReader {
            reader: source,
            streams: Vec::new(),
            cues: Vec::new(),
            metadata: Default::default(),
            index: None,
            first_frame_offset: 0,
        }
    }

    fn supported_formats() -> &'static [FormatDescriptor] {
        &[ support_format!(&["flac"], &["audio/flac"], b"fLaC    ", 4, 0) ]
    }

    fn next_packet(&mut self) -> Result<Packet<'_>> {
        // FLAC is not a "real" container format. FLAC frames are more-so part of the codec
        // bitstream than the actual format. In fact, it is not possible to know how long a FLAC
        // frame is without decoding its header and practically decoding it. This is all to say that
        // the what follows the metadata blocks is a codec bitstream. Therefore, next_packet will
        // simply always return the reader and let the codec advance the stream.
        Ok(Packet::new_direct(0, &mut self.reader))
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
        if self.streams.is_empty() {
            return seek_error(SeekErrorKind::Unseekable);
        }

        let params = &self.streams[0].codec_params;

        // Get the timestamp of the desired audio frame.
        let frame_ts = match ts {
            // Frame timestamp given.
            Timestamp::Frame(frame) => frame,
            // Time value given, calculate frame timestamp from sample rate.
            Timestamp::Time(time) => {
                // Ensure time value is positive.
                if time < 0.0 {
                    return seek_error(SeekErrorKind::OutOfRange);
                }

                // Use the sample rate to calculate the frame timestamp. If sample rate is not
                // known, the seek cannot be completed.
                if let Some(sample_rate) = params.sample_rate {
                    (time * f64::from(sample_rate)) as u64
                }
                else {
                    return seek_error(SeekErrorKind::Unseekable);
                }
            }
        };

        eprintln!("Seeking to frame_ts={}", frame_ts);

        // If the total number of frames in the stream is known, verify the desired frame timestamp
        // does not exceed it.
        if let Some(n_frames) = params.n_frames {
            if frame_ts > n_frames {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        }

        // If the reader supports seeking, coarsely seek to the nearest packet with a timestamp
        // lower than the desired timestamp using a binary search.
        if self.reader.is_seekable() {
            // The range formed by start_byte_offset..end_byte_offset defines an area where the
            // binary search for the packet containing the desired timestamp will be performed. The
            // lower bound is set to the byte offset of the first frame, while the upper bound is
            // set to the length of the stream.
            let mut start_byte_offset = self.first_frame_offset;
            let mut end_byte_offset = self.reader.seek(SeekFrom::End(0))?;

            // If there is an index, use it to refine the binary search range.
            if let Some(ref index) = self.index {
                // Search the index for the timestamp. Adjust the search based on the result.
                match index.search(frame_ts) {
                    // Search from the start of stream up-to an ending point.
                    SeekSearchResult::Upper(upper) => {
                        end_byte_offset = self.first_frame_offset + upper.byte_offset;
                    },
                    // Search from a starting point up-to the end of the stream.
                    SeekSearchResult::Lower(lower) => {
                        start_byte_offset = self.first_frame_offset + lower.byte_offset;
                    },
                    // Search between two points of the stream.
                    SeekSearchResult::Range(lower, upper) => {
                        start_byte_offset = self.first_frame_offset + lower.byte_offset;
                        end_byte_offset = self.first_frame_offset + upper.byte_offset;
                    },
                    // Search the entire stream (default behaviour, so do nothing).
                    SeekSearchResult::Stream => (),
                }
            }

            // Binary search the range of bytes formed by start_by_offset..end_byte_offset for the
            // desired frame timestamp. When the difference of the range reaches 2x the maximum
            // frame size, exit the loop and search from the start_byte_offset linearly. The binary
            // search becomes inefficient when the range is small.
            while end_byte_offset - start_byte_offset > 2 * 8096 {
                let mid_byte_offset = (start_byte_offset + end_byte_offset) / 2;
                self.reader.seek(SeekFrom::Start(mid_byte_offset))?;

                let packet = PacketParser::parse_packet(&mut self.reader)?;

                if frame_ts < packet.packet_ts {
                    end_byte_offset = mid_byte_offset;
                }
                else if frame_ts > packet.packet_ts 
                    && frame_ts < (packet.packet_ts + u64::from(packet.n_frames)) 
                {
                    // Rewind the stream back to the beginning of the frame.
                    self.reader.rewind(packet.parsed_len);

                    eprintln!("Seeked to packet_ts={} (delta={})", 
                        packet.packet_ts, packet.packet_ts as i64 - frame_ts as i64);

                    return Ok(packet.packet_ts);
                }
                else {
                    start_byte_offset = mid_byte_offset;
                }
            }

            // The binary search did not find an exact frame, but the range has been narrowed. Seek
            // to the start of the range, and continue with a linear search.
            self.reader.seek(SeekFrom::Start(start_byte_offset))?;
        }

        // Linearly search the stream packet-by-packet for the packet that contains the desired
        // timestamp. This search is used to find the exact packet containing the desired timestamp
        // after the search range was narrowed by the binary search. It is also the ONLY way for a
        // unseekable stream to be "seeked" forward.
        loop {
            let packet = PacketParser::parse_packet(&mut self.reader)?;

            // The desired timestamp preceeds the current packet's timestamp.
            if frame_ts < packet.packet_ts {
                // Rewind the stream back to the beginning of the frame.
                self.reader.rewind(packet.parsed_len);

                // Attempted to seek backwards on an unseekable stream.
                if !self.reader.is_seekable() {
                    return seek_error(SeekErrorKind::ForwardOnly);
                }
                // Overshot a regular seek, or the stream is corrupted, not necessarily an error
                // per-say.
                else {
                    eprintln!("Seeked to packet_ts={} (delta={})", 
                        packet.packet_ts, packet.packet_ts as i64 - frame_ts as i64);

                    return Ok(packet.packet_ts);
                }
            }
            // The desired timestamp is contained within the current packet.
            else if frame_ts >= packet.packet_ts 
                && frame_ts < (packet.packet_ts + u64::from(packet.n_frames))
            {
                // Rewind the stream back to the beginning of the frame.
                self.reader.rewind(packet.parsed_len);

                eprintln!("Seeked to packet_ts={} (delta={})", 
                    packet.packet_ts, packet.packet_ts as i64 - frame_ts as i64);

                return Ok(packet.packet_ts);
            }
        }
    }

    fn probe(&mut self, depth: ProbeDepth) -> Result<ProbeResult> {
        // Read the first 4 bytes of the stream. Ideally this will be the FLAC stream marker. If
        // not, use this as a window to scroll byte-after-byte searching for the stream marker if
        // the ProbeDepth is Deep.
        let mut marker = [
            self.reader.read_u8()?,
            self.reader.read_u8()?,
            self.reader.read_u8()?,
            self.reader.read_u8()?,
        ];

        // Count the number of bytes read in the probe so that a limit may (optionally) be applied.
        let mut probed_bytes = 4usize;

        loop {
            if marker == FLAC_STREAM_MARKER {
                // Found the header. This is enough for a Superficial probe, but not enough for a
                // Default probe.
                eprintln!("Probe: Found FLAC header @ +{} bytes.", probed_bytes - 4);

                // Strictly speaking, the first metadata block must be a StreamInfo block. There is
                // no technical need for this from the reader's point of view. Additionally, if the
                // reader is fed a stream mid-way there is no StreamInfo block. Therefore, just read
                // all metadata blocks and handle the StreamInfo block as it comes.
                self.read_all_metadata_blocks()?;

                self.first_frame_offset = self.reader.pos();
                
                // Make sure that there is atleast one StreamInfo block.
                if self.streams.is_empty() {
                    return decode_error("No StreamInfo block.");
                }

                // Read the rest of the metadata blocks.
                return Ok(ProbeResult::Supported);
            }
            // If the ProbeDepth is deep, continue searching for the stream marker.
            else if depth == ProbeDepth::Deep {
                // Do not search more than the designated search limit.
                // TODO: Replace with programmable limit.
                if probed_bytes <= FLAC_PROBE_SEARCH_LIMIT {

                    if probed_bytes % 4096 == 0 {
                        eprintln!("Probe: Searching for stream marker... ({} / {}) bytes.", 
                            probed_bytes, FLAC_PROBE_SEARCH_LIMIT);
                    }

                    marker[0] = marker[1];
                    marker[1] = marker[2];
                    marker[2] = marker[3];
                    marker[3] = self.reader.read_u8()?;

                    probed_bytes += 1;
                }
                else {
                    eprintln!("Probe: Stream marker search limit exceeded.");
                    break;
                }
            }
            else {
                break;
            }
        }

        // Loop exited, therefore stream is unsupported.
        Ok(ProbeResult::Unsupported)
    }

}

/// Reads a StreamInfo block and populates the reader with stream information.
fn read_stream_info_block<B : Bytestream>(
    block_stream: &mut B,
    streams: &mut Vec<Stream>
) -> Result<()> {
    // Only one StreamInfo block, and therefore ony one Stream, is allowed per media source stream.
    if streams.is_empty() {
        let info = StreamInfo::read(block_stream)?;

        // Populate the codec parameters with the information read from StreamInfo.
        let mut codec_params = CodecParameters::new();

        codec_params
            .for_codec(CODEC_TYPE_FLAC)
            .with_sample_rate(info.sample_rate)
            .with_bits_per_sample(info.bits_per_sample)
            .with_max_frames_per_packet(u64::from(info.block_sample_len.1))
            .with_channels(info.channels);
        
        // Total samples (per channel) aka frames may or may not be stated in StreamInfo.
        if let Some(n_frames) = info.n_samples {
            codec_params.with_n_frames(n_frames);
        }

        // Add the stream.
        streams.push(Stream::new(codec_params));
    }
    else {
        return decode_error("Found more than one StreamInfo block.");
    }

    Ok(())
}