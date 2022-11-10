// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{unsupported_error, Result};
use symphonia_core::io::ReadBytes;
use symphonia_core::util::clamp::clamp_i16;

use crate::common::{from_i16_shift, u16_to_i32, Nibble};

#[rustfmt::skip]
const MS_ADAPTATION_TABLE: [i32; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614,
    768, 614, 512, 409, 307, 230, 230, 230,
];

const MS_ADAPT_COEFFS1: [i32; 7] = [256, 512, 0, 192, 240, 460, 392];
const MS_ADAPT_COEFFS2: [i32; 7] = [0, -256, 0, 64, 0, -208, -232];

const DELTA_MIN: i32 = 16;

macro_rules! check_block_predictor {
    ($block_predictor:ident, $max:expr) => {
        if $block_predictor > $max {
            return unsupported_error("adpcm: block predictor exceeds range");
        }
    };
}

pub fn signed_nibble(nibble: u8) -> i8 {
    if (nibble & 0x08) != 0 {
        nibble as i8 - 0x10
    }
    else {
        nibble as i8
    }
}

/// `AdpcmMsBlockStatus` contains values to decode a block
struct AdpcmMsBlockStatus {
    coeff1: i32,
    coeff2: i32,
    delta: i32,
    sample1: i32,
    sample2: i32,
}

impl AdpcmMsBlockStatus {
    fn read_mono_preamble<B: ReadBytes>(stream: &mut B) -> Result<Self> {
        let block_predictor = stream.read_byte()? as usize;
        check_block_predictor!(block_predictor, 6);
        let status = Self {
            coeff1: MS_ADAPT_COEFFS1[block_predictor],
            coeff2: MS_ADAPT_COEFFS2[block_predictor],
            delta: u16_to_i32!(stream.read_u16()?),
            sample1: u16_to_i32!(stream.read_u16()?),
            sample2: u16_to_i32!(stream.read_u16()?),
        };
        Ok(status)
    }

    fn read_stereo_preamble<B: ReadBytes>(stream: &mut B) -> Result<(Self, Self)> {
        let left_block_predictor = stream.read_byte()? as usize;
        check_block_predictor!(left_block_predictor, 6);
        let right_block_predictor = stream.read_byte()? as usize;
        check_block_predictor!(right_block_predictor, 6);
        let left_delta = u16_to_i32!(stream.read_u16()?);
        let right_delta = u16_to_i32!(stream.read_u16()?);
        let left_sample1 = u16_to_i32!(stream.read_u16()?);
        let right_sample1 = u16_to_i32!(stream.read_u16()?);
        let left_sample2 = u16_to_i32!(stream.read_u16()?);
        let right_sample2 = u16_to_i32!(stream.read_u16()?);
        Ok((
            Self {
                coeff1: MS_ADAPT_COEFFS1[left_block_predictor],
                coeff2: MS_ADAPT_COEFFS2[left_block_predictor],
                delta: left_delta,
                sample1: left_sample1,
                sample2: left_sample2,
            },
            Self {
                coeff1: MS_ADAPT_COEFFS1[right_block_predictor],
                coeff2: MS_ADAPT_COEFFS2[right_block_predictor],
                delta: right_delta,
                sample1: right_sample1,
                sample2: right_sample2,
            },
        ))
    }

    fn expand_nibble(&mut self, byte: u8, nibble: Nibble) -> i32 {
        let nibble = nibble.get_nibble(byte);
        let signed_nibble = signed_nibble(nibble) as i32;
        let predictor = ((self.sample1 * self.coeff1) + (self.sample2 * self.coeff2)) / 256
            + signed_nibble * self.delta;
        self.sample2 = self.sample1;
        self.sample1 = clamp_i16(predictor) as i32;
        self.delta = (MS_ADAPTATION_TABLE[nibble as usize] * self.delta) / 256;
        self.delta = self.delta.max(DELTA_MIN);
        from_i16_shift!(self.sample1)
    }
}

pub(crate) fn decode_mono<B: ReadBytes>(
    stream: &mut B,
    buffer: &mut [i32],
    frames_per_block: usize,
) -> Result<()> {
    let mut status = AdpcmMsBlockStatus::read_mono_preamble(stream)?;
    buffer[0] = from_i16_shift!(status.sample2);
    buffer[1] = from_i16_shift!(status.sample1);
    for byte in 1..(frames_per_block / 2) {
        let nibbles = stream.read_u8()?;
        buffer[byte * 2] = status.expand_nibble(nibbles, Nibble::Upper);
        buffer[byte * 2 + 1] = status.expand_nibble(nibbles, Nibble::Lower);
    }
    Ok(())
}

pub(crate) fn decode_stereo<B: ReadBytes>(
    stream: &mut B,
    buffers: [&mut [i32]; 2],
    frames_per_block: usize,
) -> Result<()> {
    let (mut left_status, mut right_status) = AdpcmMsBlockStatus::read_stereo_preamble(stream)?;
    buffers[0][0] = from_i16_shift!(left_status.sample2);
    buffers[0][1] = from_i16_shift!(left_status.sample1);
    buffers[1][0] = from_i16_shift!(right_status.sample2);
    buffers[1][1] = from_i16_shift!(right_status.sample1);
    for frame in 2..frames_per_block {
        let nibbles = stream.read_u8()?;
        buffers[0][frame] = left_status.expand_nibble(nibbles, Nibble::Upper);
        buffers[1][frame] = right_status.expand_nibble(nibbles, Nibble::Lower);
    }
    Ok(())
}
