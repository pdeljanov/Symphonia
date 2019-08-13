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
use sonata_core::formats::{FormatDescriptor, FormatOptions, FormatReader, Packet};
use sonata_core::formats::{Cue, ProbeDepth, ProbeResult, Stream, Visual};
use sonata_core::tags::Tag;
use sonata_core::io::*;

use crate::id3v2;

/// MPEG1 and MPEG2 audio frame reader.
/// 
/// `Mp3Reader` implements a demuxer for the MPEG1 and MPEG2 audio frame format.
pub struct Mp3Reader {
    reader: MediaSourceStream,
    streams: Vec<Stream>,
    tags: Vec<Tag>,
    visuals: Vec<Visual>,
    cues: Vec<Cue>,
}

impl FormatReader for Mp3Reader {
    fn open(source: MediaSourceStream, _options: &FormatOptions) -> Self {
        Mp3Reader {
            reader: source,
            streams: Vec::new(),
            tags: Vec::new(),
            visuals: Vec::new(),
            cues: Vec::new(),
        }
    }

    fn supported_formats() -> &'static [FormatDescriptor] {
        &[ support_format!(&["mp3"], &["audio/mp3"], b"MPEG    ", 4, 0) ]
    }

    fn next_packet(&mut self) -> Result<Packet<'_>> {
        Ok(Packet::new_direct(0, &mut self.reader))
    }

    fn tags(&self) -> &[Tag] {
        &self.tags
    }

    fn visuals(&self) -> &[Visual] {
        &self.visuals
    }

    fn cues(&self) -> &[Cue] {
        &self.cues
    }

    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn seek(&mut self, ts: Timestamp) -> Result<u64> {
        unimplemented!();
    }

    fn probe(&mut self, depth: ProbeDepth) -> Result<ProbeResult> {
        self.tags.append(&mut id3v2::read_id3v2(&mut self.reader)?);

        let mut params = CodecParameters::new();
        params.for_codec(CODEC_TYPE_MP3);
        self.streams.push(Stream::new(params));

        Ok(ProbeResult::Supported)
    }
}