// Symphonia
// Copyright (c) 2019 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

// Justification: edentity operations are allowed to vertically align, and better illustrate 
// complex alogrithms and vectorizations.
#![allow(clippy::identity_op)]

// Justification: excessive floating point precision is allowed in-case f32 constants should be 
// switched to f64.
#![allow(clippy::excessive_precision)]

mod common;
mod decoder;
mod demuxer;
mod header;
mod huffman_tables;
mod layer3;
mod synthesis;

pub use decoder::Mp3Decoder;
pub use demuxer::Mp3Reader;