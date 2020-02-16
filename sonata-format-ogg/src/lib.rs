// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

use sonata_core::support_format;

use sonata_core::audio::Timestamp;
use sonata_core::errors::{Result, unsupported_error};
use sonata_core::formats::prelude::*;
use sonata_core::io::{ByteStream, MediaSourceStream};
use sonata_core::meta::MetadataQueue;
use sonata_core::probe::{Descriptor, Instantiate, QueryDescriptor};

const OGG_STREAM_MARKER: [u8; 4] = *b"OggS";

/// Ogg demultiplexer.
///
/// `OggReader` implements a demuxer for Xiph's OGG container format.
pub struct OggReader {
    reader: MediaSourceStream,
    streams: Vec<Stream>,
    cues: Vec<Cue>,
    metadata: MetadataQueue,
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

    fn score(_context: &[u8]) -> f32 {
        1.0
    }
}

impl FormatReader for OggReader {

    fn try_new(mut source: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {
        // The OggS marker should be present.
        let marker = source.read_quad_bytes()?;

        if marker != OGG_STREAM_MARKER {
            return unsupported_error("missing ogg stream marker");
        }

        Ok(OggReader {
            reader: source,
            streams: Default::default(),
            cues: Default::default(),
            metadata: Default::default(),
        })
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

    fn seek(&mut self, ts: Timestamp) -> Result<u64> {
        unsupported_error("ogg seeking unsupported")
    }

}
