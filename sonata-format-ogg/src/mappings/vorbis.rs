// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::Mapper;

use sonata_core::codecs::{CodecParameters, CODEC_TYPE_VORBIS};
use sonata_core::errors::Result;

pub fn detect(buf: &[u8]) -> Result<Option<Box<dyn Mapper>>> {
    Ok(None)
}