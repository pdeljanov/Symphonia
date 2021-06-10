// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::support_format;

use symphonia_core::audio::Channels;
use symphonia_core::codecs::{CodecParameters, CODEC_TYPE_AAC};
use symphonia_core::errors::{Result, decode_error};
use symphonia_core::formats::prelude::*;
use symphonia_core::io::*;
use symphonia_core::meta::MetadataQueue;
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};

use super::common::{AAC_SAMPLE_RATES, map_channels, M4AType, M4A_TYPES};

/// Audio Data Transport Stream (ADTS) format reader.
///
/// `AdtsReader` implements a demuxer for ADTS (AAC native frames).
pub struct AdtsReader {
    reader: MediaSourceStream,
    streams: Vec<Stream>,
    cues: Vec<Cue>,
    metadata: MetadataQueue,
}

impl QueryDescriptor for AdtsReader {
    fn query() -> &'static [Descriptor] {
        &[
            support_format!(
                "aac",
                "Audio Data Transport Stream (native AAC)",
                &[ "aac" ],
                &[ "audio/aac" ],
                &[
                    &[ 0xff, 0xf1 ]
                ]),
        ]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}

#[derive(Debug)]
struct AdtsHeader {
    profile: M4AType,
    channels: Option<Channels>,
    sample_rate: u32,
    frame_len: usize,
}

impl AdtsHeader {

    fn sync<B: ByteStream>(reader: &mut B) -> Result<()> {
        let mut sync = 0u16;

        while sync != 0xfff1 {
            sync = (sync << 8) | u16::from(reader.read_u8()?);
        }

        Ok(())
    }

    fn read<B: ByteStream>(reader: &mut B) -> Result<Self> {
        AdtsHeader::sync(reader)?;

        let mut bs = BitStreamLtr::new(reader);

        // Profile
        let profile = M4A_TYPES[bs.read_bits_leq32(2)? as usize + 1];

        // Sample rate index.
        let sample_rate = match bs.read_bits_leq32(4)? as usize {
            15 => return decode_error("forbidden sample rate"),
            idx => AAC_SAMPLE_RATES[idx],
        };

        // Private bit.
        bs.ignore_bit()?;

        // Channel configuration
        let channels = match bs.read_bits_leq32(3)? {
            0   => None,
            idx => map_channels(idx),
        };

        // Originality, Home, Copyrighted ID bit, Copyright ID start bits. Only used for encoding.
        bs.ignore_bits(4)?;

        // Frame length = Header size (7) + AAC frame size
        let frame_len = bs.read_bits_leq32(13)? as usize;

        if frame_len < 7 {
            return decode_error("invalid ADTS frame length");
        }

        let _fullness = bs.read_bits_leq32(11)?;
        let num_aac_frames = bs.read_bits_leq32(2)? + 1;

        assert!(num_aac_frames == 1);

        Ok(AdtsHeader {
            profile,
            channels,
            sample_rate,
            frame_len: frame_len - 7,
        })
    }
}

impl FormatReader for AdtsReader {

    fn try_new(mut source: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {
        let header = AdtsHeader::read(&mut source)?;

        // Use the header to populate the codec parameters.
        let mut params = CodecParameters::new();

        params.for_codec(CODEC_TYPE_AAC)
              .with_sample_rate(header.sample_rate);

        if let Some(channels) = header.channels {
            params.with_channels(channels);
        }

        // Rewind back to the start of the frame.
        source.rewind(7);

        Ok(AdtsReader {
            reader: source,
            streams: vec![ Stream::new(0, params) ],
            cues: Vec::new(),
            metadata: Default::default(),
        })
    }

    fn next_packet(&mut self) -> Result<Packet> {
        // Parse the header to get the calculated frame size.
        let header = AdtsHeader::read(&mut self.reader)?;

        Ok(Packet::new_from_boxed_slice(
            0,
            0,
            0,
            self.reader.read_boxed_slice_exact(header.frame_len)?
        ))
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

    fn seek(&mut self, _mode: SeekMode, _to: SeekTo) -> Result<SeekedTo> {
        unimplemented!();
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.reader
    }

}