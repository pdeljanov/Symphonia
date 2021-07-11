// Symphonia
// Copyright (c) 2019 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::support_format;

use symphonia_core::codecs::{CodecParameters, CODEC_TYPE_MP3};
use symphonia_core::errors::{Result, SeekErrorKind, seek_error};
use symphonia_core::formats::prelude::*;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};

use std::io::{Seek, SeekFrom};

use log::debug;

use super::{header, common::FrameHeader, common::SAMPLES_PER_GRANULE};

/// MPEG1 and MPEG2 audio frame reader.
///
/// `Mp3Reader` implements a demuxer for the MPEG1 and MPEG2 audio frame format.
pub struct Mp3Reader {
    reader: MediaSourceStream,
    tracks: Vec<Track>,
    cues: Vec<Cue>,
    metadata: MetadataLog,
    first_frame_pos: u64,
    next_packet_ts: u64,
}

impl QueryDescriptor for Mp3Reader {
    fn query() -> &'static [Descriptor] {
        &[
            // Layer 3
            support_format!(
                "mp3",
                "MPEG Audio Layer 3 Native",
                &[ "mp3" ],
                &[ "audio/mp3" ],
                &[
                    &[ 0xff, 0xfa ], &[ 0xff, 0xfb ], // MPEG 1
                    &[ 0xff, 0xf2 ], &[ 0xff, 0xf3 ], // MPEG 2
                    &[ 0xff, 0xe2 ], &[ 0xff, 0xe3 ], // MPEG 2.5
                ]),
        ]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}

impl FormatReader for Mp3Reader {

    fn try_new(mut source: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {
        // Try to parse the header of the first MPEG frame.
        let header = header::parse_frame_header(source.read_be_u32()?)?;

        // Use the header to populate the codec parameters.
        let mut params = CodecParameters::new();

        params.for_codec(CODEC_TYPE_MP3)
              .with_sample_rate(header.sample_rate)
              .with_channels(header.channel_mode.channels());

        // Rewind back to the start of the frame.
        source.rewind(std::mem::size_of::<u32>());

        let first_frame_offset = source.pos();

        Ok(Mp3Reader {
            reader: source,
            tracks: vec![ Track::new(0, params) ],
            cues: Vec::new(),
            metadata: Default::default(),
            first_frame_pos: first_frame_offset,
            next_packet_ts: 0,
        })
    }

    fn next_packet(&mut self) -> Result<Packet> {
        // Sync to the next frame header.
        let header_buf = header::sync_frame(&mut self.reader)?;

        // Parse the header to get the calculated frame size.
        let header = header::parse_frame_header(header_buf)?;

        // Allocate a buffer for the entire MPEG frame. Prefix the buffer with the frame header.
        let mut packet_buf = vec![0u8; header.frame_size + 4];
        packet_buf[0..4].copy_from_slice(&header_buf.to_be_bytes());
        self.reader.read_buf_exact(&mut packet_buf[4..])?;

        // Each frame contains 1 or 2 granules with each granule being exactly 576 samples long.
        let duration = SAMPLES_PER_GRANULE * header.n_granules() as u64;

        let ts = self.next_packet_ts;

        self.next_packet_ts += duration;

        Ok(Packet::new_from_boxed_slice(0, ts, duration, packet_buf.into_boxed_slice()))
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
        const MAX_REF_FRAMES: usize = 4;
        const REF_FRAMES_MASK: usize = MAX_REF_FRAMES - 1;

        // Get the timestamp of the desired audio frame.
        let required_ts = match to {
            // Frame timestamp given.
            SeekTo::TimeStamp { ts, .. } => ts,
            // Time value given, calculate frame timestamp from sample rate.
            SeekTo::Time { time, .. } => {
                // Use the sample rate to calculate the frame timestamp. If sample rate is not
                // known, the seek cannot be completed.
                if let Some(sample_rate) = self.tracks[0].codec_params.sample_rate {
                    TimeBase::new(1, sample_rate).calc_timestamp(time)
                }
                else {
                    return seek_error(SeekErrorKind::Unseekable);
                }
            }
        };

        debug!("seeking to ts={}", required_ts);

        // If the desired timestamp is less-than the next packet timestamp, attempt to seek
        // to the start of the stream.
        if required_ts < self.next_packet_ts {
            // If the reader is not seekable then only forward seeks are possible.
            if self.reader.is_seekable() {
                let seeked_pos = self.reader.seek(SeekFrom::Start(self.first_frame_pos))?;

                // Since the elementary stream has no timestamp information, the position seeked
                // to must be exactly as requested.
                if seeked_pos != self.first_frame_pos {
                    return seek_error(SeekErrorKind::Unseekable);
                }
            }
            else {
                return seek_error(SeekErrorKind::ForwardOnly)
            }

            // Successfuly seeked to the start of the stream, reset the next packet timestamp.
            self.next_packet_ts = 0;
        }

        let mut frames : [FramePos; MAX_REF_FRAMES] = Default::default();
        let mut n_frames = 0;

        // Parse frames from the stream until the frame containing the desired timestamp is
        // reached.
        loop {
            // Parse the next frame header.
            let header = header::parse_frame_header(header::sync_frame(&mut self.reader)?)?;

            // Position of the frame header.
            let frame_pos = self.reader.pos() - std::mem::size_of::<u32>() as u64;

            // Calculate the duration of the frame.
            let duration = SAMPLES_PER_GRANULE * header.n_granules() as u64;

            // Add the frame to the frame ring.
            frames[n_frames & REF_FRAMES_MASK] = FramePos { pos: frame_pos, ts: self.next_packet_ts };
            n_frames += 1;

            // If the next frame's timestamp would exceed the desired timestamp, rewind back to the
            // start of this frame and end the search.
            if self.next_packet_ts + duration > required_ts {
                // The main_data_begin offset is a negative offset from the frame's header to where
                // its main data begins. Therefore, for a decoder to properly decode this frame, the
                // reader must provide previous (reference) frames up-to and including the frame
                // that contains the first byte this frame's main_data.
                let main_data_begin = read_main_data_begin(&mut self.reader, &header)? as u64;

                debug!(
                    "found frame with ts={} @ pos={} with main_data_begin={}",
                    self.next_packet_ts,
                    frame_pos,
                    main_data_begin
                );

                // The number of reference frames is 0 if main_data_begin is also 0. Otherwise,
                // attempt to find the first (oldest) reference frame, then select 1 frame before
                // that one to actually seek to.
                let mut n_ref_frames = 0;
                let mut ref_frame = &frames[(n_frames - 1) & REF_FRAMES_MASK];

                if main_data_begin > 0 {
                    // The maximum number of reference frames is limited to the number of frames
                    // read and the number of previous frames recorded.
                    let max_ref_frames = std::cmp::min(n_frames, frames.len());

                    while n_ref_frames < max_ref_frames {
                        ref_frame = &frames[(n_frames - n_ref_frames - 1) & REF_FRAMES_MASK];

                        if frame_pos - ref_frame.pos >= main_data_begin {
                            break;
                        }

                        n_ref_frames += 1;
                    }

                    debug!(
                        "will seek to ts={} (-{} frames) @ pos={} (-{} bytes)",
                        ref_frame.ts,
                        n_ref_frames,
                        ref_frame.pos,
                        frame_pos - ref_frame.pos
                    );
                }

                // Do the actual seek to the reference frame.
                self.next_packet_ts = ref_frame.ts;
                self.reader.seek_buffered(ref_frame.pos);

                break;
            }

            // Otherwise, ignore the frame body.
            self.reader.ignore_bytes(header.frame_size as u64)?;

            // Increment the timestamp for the next packet.
            self.next_packet_ts += duration;
        }

        debug!("seeked to ts={} (delta={})",
            self.next_packet_ts,
            required_ts as i64 - self.next_packet_ts as i64);

        Ok(SeekedTo {
            track_id: 0,
            required_ts,
            actual_ts: self.next_packet_ts,
        })
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.reader
    }
}

#[derive(Default)]
struct FramePos {
    ts: u64,
    pos: u64,
}

/// Reads the main_data_begin field from the side information of a MP3 frame.
fn read_main_data_begin<B: ByteStream>(reader: &mut B, header: &FrameHeader) -> Result<u16> {
    // After the head the optional CRC is present.
    if header.has_crc {
        let _crc = reader.read_be_u16()?;
    }

    // Then the side-info.
    let mut bs = BitStreamLtr::new(reader);

    // For MPEG version 1 the first 9 bits is main_data_begin.
    if header.is_mpeg1() {
        Ok(bs.read_bits_leq32(9)? as u16)
    }
    // For MPEG version 2 the first 8 bits is main_data_begin.
    else {
        Ok(bs.read_bits_leq32(8)? as u16)
    }
}