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
use symphonia_core::meta::MetadataQueue;
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};

use std::io::{Seek, SeekFrom};

use log::debug;

use super::{header, common::SAMPLES_PER_GRANULE};

/// MPEG1 and MPEG2 audio frame reader.
///
/// `Mp3Reader` implements a demuxer for the MPEG1 and MPEG2 audio frame format.
pub struct Mp3Reader {
    reader: MediaSourceStream,
    streams: Vec<Stream>,
    cues: Vec<Cue>,
    metadata: MetadataQueue,
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
            streams: vec![ Stream::new(0, params) ],
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

        let ts = self.next_packet_ts;

        // Each frame contains 1 or 2 granules with each granule being exactly 576 samples long.
        let duration = SAMPLES_PER_GRANULE * header.n_granules() as u64;

        self.next_packet_ts += duration;

        Ok(Packet::new_from_boxed_slice(0, ts, duration, packet_buf.into_boxed_slice()))
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

    fn seek(&mut self, to: SeekTo) -> Result<SeekedTo> {
        // Get the timestamp of the desired audio frame.
        let desired_ts = match to {
            // Frame timestamp given.
            SeekTo::TimeStamp { ts, .. } => ts,
            // Time value given, calculate frame timestamp from sample rate.
            SeekTo::Time { time } => {
                // Use the sample rate to calculate the frame timestamp. If sample rate is not
                // known, the seek cannot be completed.
                if let Some(sample_rate) = self.streams[0].codec_params.sample_rate {
                    TimeBase::new(1, sample_rate).calc_timestamp(time)
                }
                else {
                    return seek_error(SeekErrorKind::Unseekable);
                }
            }
        };

        debug!("seeking to ts={}", desired_ts);

        // If the desired timestamp is less-than the next packet timestamp, attempt to seek
        // to the start of the stream.
        if desired_ts < self.next_packet_ts {
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

        // Parse frames from the stream until the frame containing the desired timestamp is
        // reached.
        loop {
            // Parse the next frame header.
            let header = header::parse_frame_header(header::sync_frame(&mut self.reader)?)?;

            // Calculate the duration of the frame.
            let duration = SAMPLES_PER_GRANULE * header.n_granules() as u64;

            // If the next frame's timestamp would exceed the desired timestamp, rewind back to the
            // start of this frame and end the search.
            if self.next_packet_ts + duration > desired_ts {
                self.reader.rewind(std::mem::size_of::<u32>());
                break;
            }

            // TODO: The above check will find the frame containing the sample, but that frame may
            // use data from the bit resevoir and thus data from previous frames. Improve this by
            // further parsing the frame to determine how many previous frames are required and
            // seek the stream to the oldest required frame.

            // Otherwise, ignore the frame body.
            self.reader.ignore_bytes(header.frame_size as u64)?;

            // Increment the timestamp for the next packet.
            self.next_packet_ts += duration;
        }

        debug!("seeked to ts={} (delta={})",
            self.next_packet_ts,
            desired_ts as i64 - self.next_packet_ts as i64);

        Ok(SeekTo::TimeStamp { ts: self.next_packet_ts, stream: 0 })
    }

}