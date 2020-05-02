// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]

use sonata_core::support_format;

use sonata_core::codecs::{CodecParameters, CODEC_TYPE_MP3};
use sonata_core::errors::Result;
use sonata_core::formats::prelude::*;
use sonata_core::io::*;
use sonata_core::meta::MetadataQueue;
use sonata_core::probe::{Descriptor, Instantiate, QueryDescriptor};

use super::header;

/// MPEG1 and MPEG2 audio frame reader.
///
/// `Mp3Reader` implements a demuxer for the MPEG1 and MPEG2 audio frame format.
pub struct Mp3Reader {
    reader: MediaSourceStream,
    streams: Vec<Stream>,
    cues: Vec<Cue>,
    metadata: MetadataQueue,
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

        Ok(Mp3Reader {
            reader: source,
            streams: vec![ Stream::new(0, params) ],
            cues: Vec::new(),
            metadata: Default::default(),
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

        Ok(Packet::new_from_boxed_slice(0, 0, 0, packet_buf.into_boxed_slice()))
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

    fn seek(&mut self, _to: SeekTo) -> Result<SeekedTo> {
        unimplemented!();
    }

}