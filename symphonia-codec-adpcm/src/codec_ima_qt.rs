// Symphonia
// Copyright (c) 2019-2025 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::Result;
use symphonia_core::io::ReadBytes;

use crate::common::{Nibble, u16_to_i32};
use crate::common_ima::AdpcmImaBlockStatus;

fn read_preamble<B: ReadBytes>(stream: &mut B) -> Result<AdpcmImaBlockStatus> {
    let header = stream.read_be_u16()?;
    let predictor = u16_to_i32!(header & 0xFF80);
    let step_index = ((header & 0x7F) as usize).min(88) as i32;

    let status = AdpcmImaBlockStatus { predictor, step_index };
    Ok(status)
}

pub(crate) fn decode_mono<B: ReadBytes>(
    stream: &mut B,
    buffer: &mut [i32],
    _: usize,
) -> Result<()> {
    // IMA4 apparently always uses 34 bytes packets
    // https://wiki.multimedia.cx/index.php/Apple_QuickTime_IMA_ADPCM
    let mut status = read_preamble(stream)?;
    for byte in 0..32 {
        let nibbles = stream.read_u8()?;
        buffer[byte * 2] = status.expand_nibble(nibbles, Nibble::Lower);
        buffer[byte * 2 + 1] = status.expand_nibble(nibbles, Nibble::Upper);
    }
    Ok(())
}

pub(crate) fn decode_stereo<B: ReadBytes>(
    stream: &mut B,
    buffers: [&mut [i32]; 2],
    _: usize,
) -> Result<()> {
    decode_mono(stream, buffers[0], 0)?;
    decode_mono(stream, buffers[1], 0)?;
    Ok(())
}
