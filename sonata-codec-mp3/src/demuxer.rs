// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]

use sonata_core::support_format;

use sonata_core::audio::Timestamp;
use sonata_core::codecs::{CodecParameters, CODEC_TYPE_MP3};
use sonata_core::errors::Result;
use sonata_core::formats::{Cue, FormatOptions, FormatReader, Packet, ProbeResult, Stream};
use sonata_core::io::*;
use sonata_core::meta::MetadataQueue;
use sonata_core::probe::{Descriptor, Instantiate, QueryDescriptor};

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

    fn score(_context: &[u8]) -> f32 {
        1.0
    }
}

impl FormatReader for Mp3Reader {
    fn open(source: MediaSourceStream, _options: &FormatOptions) -> Self {
        Mp3Reader {
            reader: source,
            streams: Vec::new(),
            cues: Vec::new(),
            metadata: Default::default(),
        }
    }

    fn next_packet(&mut self) -> Result<Packet<'_>> {
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

    fn seek(&mut self, _ts: Timestamp) -> Result<u64> {
        unimplemented!();
    }

    fn probe(&mut self) -> Result<ProbeResult> {
        // Read ID3v2 tags.
        let mut params = CodecParameters::new();
        params.for_codec(CODEC_TYPE_MP3);
        self.streams.push(Stream::new(params));

        Ok(ProbeResult::Supported)
    }
}