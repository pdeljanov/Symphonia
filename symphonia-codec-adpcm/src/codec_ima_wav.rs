// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{Result, decode_error};
use symphonia_core::io::ReadBytes;

use crate::common::{Nibble, from_i16_shift, u16_to_i32};
use crate::common_ima::AdpcmImaBlockStatus;

fn read_preamble<B: ReadBytes>(stream: &mut B) -> Result<AdpcmImaBlockStatus> {
    let predictor = u16_to_i32!(stream.read_u16()?);
    let step_index = stream.read_byte()? as i32;
    if step_index > 88 {
        return decode_error("adpcm (ima): invalid step index");
    }
    //reserved byte
    let _ = stream.read_byte()?;
    let status = AdpcmImaBlockStatus { predictor, step_index };
    Ok(status)
}

pub(crate) fn decode_mono<B: ReadBytes>(
    stream: &mut B,
    buffer: &mut [i32],
    frames_per_block: usize,
) -> Result<()> {
    let data_bytes_per_channel = (frames_per_block - 1) / 2;
    let mut status = read_preamble(stream)?;
    buffer[0] = from_i16_shift!(status.predictor);
    for byte in 0..data_bytes_per_channel {
        let nibbles = stream.read_u8()?;
        buffer[1 + byte * 2] = status.expand_nibble(nibbles, Nibble::Lower);
        buffer[1 + byte * 2 + 1] = status.expand_nibble(nibbles, Nibble::Upper);
    }
    Ok(())
}

pub(crate) fn decode_stereo<B: ReadBytes>(
    stream: &mut B,
    buffers: [&mut [i32]; 2],
    frames_per_block: usize,
) -> Result<()> {
    let data_bytes_per_channel = frames_per_block - 1;
    let mut status = [read_preamble(stream)?, read_preamble(stream)?];
    buffers[0][0] = from_i16_shift!(status[0].predictor);
    buffers[1][0] = from_i16_shift!(status[1].predictor);
    for index in 0..data_bytes_per_channel {
        let channel = (index / 4) & 1;
        let offset = (index / 8) * 8;
        let byte = index % 4;
        let nibbles = stream.read_u8()?;
        buffers[channel][1 + offset + byte * 2] =
            status[channel].expand_nibble(nibbles, Nibble::Lower);
        buffers[channel][1 + offset + byte * 2 + 1] =
            status[channel].expand_nibble(nibbles, Nibble::Upper);
    }
    Ok(())
}
