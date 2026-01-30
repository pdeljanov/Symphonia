// Symphonia DSD Format Demuxer
// Copyright (c) 2026 M0Rf30
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

use symphonia_core::codecs::{decl_codec_type, CodecType};

mod dff;
mod dsf;

pub use dff::DffReader;
pub use dsf::DsfReader;

// Codec type for DSD "DSD\0"
pub const CODEC_TYPE_DSD: CodecType = decl_codec_type(b"DSD\0");

// DSD sample rates (in Hz)
pub const DSD64_RATE: u32 = 2822400; // 64 * 44100
pub const DSD128_RATE: u32 = 5644800; // 128 * 44100
pub const DSD256_RATE: u32 = 11289600; // 256 * 44100
pub const DSD512_RATE: u32 = 22579200; // 512 * 44100
