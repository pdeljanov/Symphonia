// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{Result, decode_error};

/// The length of the Speex header packet.
pub const HEADER_LEN: usize = 80;

/// The signature of the Speex header packet.
pub const SIGNATURE: &[u8; 8] = b"Speex   ";

/// The header of a Speex stream.
#[derive(Clone, Debug)]
pub struct SpeexHeader {
    pub sample_rate: u32,
    /// The mode of the stream: 0 for narrowband, 1 for wideband, and 2 for ultra wideband.
    pub mode: u32,
    pub channels: u32,
    pub frame_size: u32,
    pub frames_per_packet: u32,
}

impl SpeexHeader {
    pub fn parse(buf: &[u8]) -> Result<SpeexHeader> {
        if buf.len() < HEADER_LEN || &buf[..8] != SIGNATURE {
            return decode_error("speex: invalid header");
        }

        let word = |at: usize| u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);

        let header = SpeexHeader {
            sample_rate: word(36),
            mode: word(40),
            channels: word(48),
            frame_size: word(56),
            frames_per_packet: word(64).max(1),
        };

        if header.mode > 2 || header.sample_rate == 0 || !(1..=2).contains(&header.channels) {
            return decode_error("speex: unsupported stream");
        }

        Ok(header)
    }
}
