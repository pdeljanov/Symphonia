// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::Result;
use symphonia_core::io::ReadBytes;
use symphonia_core::util::clamp::clamp_i16;

use crate::common::{from_i16_shift, u16_to_i32, Nibble};

#[rustfmt::skip]
const IMA_INDEX_TABLE: [i32; 16] = [
    -1, -1, -1, -1, 2, 4, 6, 8,
    -1, -1, -1, -1, 2, 4, 6, 8,
];

#[rustfmt::skip]
const IMA_STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17,
    19, 21, 23, 25, 28, 31, 34, 37, 41, 45,
    50, 55, 60, 66, 73, 80, 88, 97, 107, 118,
    130, 143, 157, 173, 190, 209, 230, 253, 279, 307,
    337, 371, 408, 449, 494, 544, 598, 658, 724, 796,
    876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066,
    2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358,
    5894, 6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899,
    15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

/// `AdpcmImaBlockStatus` contains values to decode a block
struct AdpcmImaBlockStatus {
    predictor: i32,
    step_index: i32,
}

impl AdpcmImaBlockStatus {
    fn read_preamble<B: ReadBytes>(stream: &mut B) -> Result<Self> {
        let predictor = u16_to_i32!(stream.read_u16()?);
        let step_index = stream.read_byte()? as i32;
        //reserved byte
        let _ = stream.read_byte()?;
        let status = Self { predictor, step_index };
        Ok(status)
    }

    fn expand_nibble(&mut self, byte: u8, nibble: Nibble) -> i32 {
        let nibble = nibble.get_nibble(byte);
        let step = IMA_STEP_TABLE[self.step_index as usize];
        let sign = (nibble & 0x08) != 0;
        let delta = (nibble & 0x07) as i32;
        let diff = ((2 * delta + 1) * step) >> 3;
        let predictor = if sign { self.predictor - diff } else { self.predictor + diff };
        self.predictor = clamp_i16(predictor) as i32;
        self.step_index = (self.step_index + IMA_INDEX_TABLE[nibble as usize]).min(88).max(0);
        from_i16_shift!(self.predictor)
    }
}

pub(crate) fn decode_mono<B: ReadBytes>(
    stream: &mut B,
    buffer: &mut [i32],
    frames_per_block: usize,
) -> Result<()> {
    let data_bytes_per_channel = (frames_per_block - 1) / 2;
    let mut status = AdpcmImaBlockStatus::read_preamble(stream)?;
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
    let mut status =
        [AdpcmImaBlockStatus::read_preamble(stream)?, AdpcmImaBlockStatus::read_preamble(stream)?];
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
