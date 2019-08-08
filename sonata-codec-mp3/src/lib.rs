// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

mod bitstream;
mod decoder;
mod demuxer;
mod huffman_tables;
mod synthesis;

pub mod id3v1;
pub mod id3v2;

pub use decoder::Mp3Decoder;
pub use demuxer::Mp3Reader;