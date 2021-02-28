// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::common::OggPacket;

use symphonia_core::codecs::CodecParameters;
use symphonia_core::errors::Result;
use symphonia_core::meta::MetadataRevision;

mod flac;
mod opus;
mod vorbis;

/// Detect `CodecParameters` for a stream that is coded using a supported codec.
pub fn detect(buf: &[u8]) -> Result<Option<Box<dyn Mapper>>> {
    let mapper = flac::detect(buf)?
                    .or(vorbis::detect(buf)?)
                    .or(opus::detect(buf)?)
                    .or_else(make_null_mapper);

    Ok(mapper)
}

pub struct Bitstream {
    pub ts: u64,
    pub dur: u64,
}

pub enum MapResult {
/// The packet belongs to the codec bitstream.
    Bitstream(Bitstream),
    /// The packet contains metadata.
    Metadata(MetadataRevision),
    /// The packet should be discarded by the demuxer. The packet may be of unknown type,
    /// unecessary, or consumed by the mapper internally.
    Unknown,
}

/// A `Mapper` implements packet-handling for a specific `Codec`.
pub trait Mapper: Send {
    /// Gets the `CodecParameters` for the stream belonging to this `Mapper`. If the stream is not
    /// ready then the set of parameters may be incomplete.
    fn codec(&self) -> &CodecParameters;

    /// Maps a packet to a specific use-case.
    fn map_packet(&mut self, packet: &OggPacket) -> Result<MapResult>;

    /// Returns `true` if the stream can is ready for usage. If the stream is not ready then the
    /// mapper needs to consume more packets.
    fn is_stream_ready(&self) -> bool {
        true
    }
}

fn make_null_mapper() -> Option<Box<dyn Mapper>> {
    Some(Box::new(NullMapper::new()))
}

struct NullMapper {
    params: CodecParameters,
}

impl NullMapper {
    fn new() -> Self {
        NullMapper {
            params: CodecParameters::new(),
        }
    }
}

impl Mapper for NullMapper {
    fn codec(&self) -> &CodecParameters {
        &self.params
    }

    fn map_packet(&mut self, _: &OggPacket) -> Result<MapResult> {
        Ok(MapResult::Unknown)
    }
}