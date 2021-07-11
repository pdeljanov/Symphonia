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
    Bitstream(Bitstream),
    Metadata(MetadataRevision),
    Unknown,
}

/// A `Mapper` implements packet-handling for a specific `Codec`.
pub trait Mapper: Send {
    fn codec(&self) -> &CodecParameters;
    fn map_packet(&mut self, packet: &OggPacket) -> Result<MapResult>;
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