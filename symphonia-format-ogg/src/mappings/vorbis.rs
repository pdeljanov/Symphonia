// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use super::Mapper;

// use symphonia_core::codecs::{CodecParameters, CODEC_TYPE_VORBIS};
use symphonia_core::errors::Result;

pub fn detect(_buf: &[u8]) -> Result<Option<Box<dyn Mapper>>> {
    Ok(None)
}