// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The bit stream of ITU-T Recommendation G.723.1, clause 4.

use symphonia_core::errors::{Result, decode_error};

use crate::tables::COMBINATORIAL;

pub const SUBFRAME_LEN: usize = 60;
pub const SUBFRAMES: usize = 4;
pub const FRAME_LEN: usize = SUBFRAME_LEN * SUBFRAMES;

const HIGH_RATE_BYTES: usize = 24;
const LOW_RATE_BYTES: usize = 20;
const SID_BYTES: usize = 4;
const UNTRANSMITTED_BYTES: usize = 1;

/// The exclusive upper bounds of the position codes of the 6.3 kbit/s subframes.
const MAX_POSITION: [u32; SUBFRAMES] = [593775, 142506, 593775, 142506];
/// The number of low bits of the position codes that are not part of the combined MSB word.
const POSITION_LOW_BITS: [usize; SUBFRAMES] = [16, 14, 16, 14];
const MSB_RADIX: [u32; SUBFRAMES] = [10, 9, 10, 9];

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FrameKind {
    /// 6.3 kbit/s multipulse excitation.
    High,
    /// 5.3 kbit/s algebraic excitation.
    Low,
    /// Silence insertion descriptor.
    Sid,
    Untransmitted,
}

impl FrameKind {
    /// Reads the kind of the frame that starts with the byte.
    pub fn from_first_byte(byte: u8) -> FrameKind {
        match byte & 3 {
            0 => FrameKind::High,
            1 => FrameKind::Low,
            2 => FrameKind::Sid,
            _ => FrameKind::Untransmitted,
        }
    }

    pub fn bytes(self) -> usize {
        match self {
            FrameKind::High => HIGH_RATE_BYTES,
            FrameKind::Low => LOW_RATE_BYTES,
            FrameKind::Sid => SID_BYTES,
            FrameKind::Untransmitted => UNTRANSMITTED_BYTES,
        }
    }
}

/// The parameters of a speech frame.
#[derive(Clone, Debug, Default)]
pub struct FrameParams {
    pub high_rate: bool,
    pub lsp_index: u32,
    pub lag_index: [u32; SUBFRAMES],
    pub gain_index: [u32; SUBFRAMES],
    pub grid: [usize; SUBFRAMES],
    pub position: [u32; SUBFRAMES],
    pub signs: [u32; SUBFRAMES],
}

/// A reader of fields whose first bit is the least significant bit of the first byte.
struct BitsLe<'a> {
    data: &'a [u8],
    pos: usize,
}

impl BitsLe<'_> {
    fn read(&mut self, n: usize) -> u32 {
        let mut value = 0;
        for i in 0..n {
            let bit = (self.data[self.pos >> 3] >> (self.pos & 7)) & 1;
            value |= u32::from(bit) << i;
            self.pos += 1;
        }
        value
    }
}

/// Unpacks a 6.3 or 5.3 kbit/s frame, whose size has been checked by the caller.
pub fn unpack(data: &[u8], kind: FrameKind) -> Result<FrameParams> {
    let mut bits = BitsLe { data, pos: 2 };
    let mut p = FrameParams { high_rate: kind == FrameKind::High, ..Default::default() };

    p.lsp_index = bits.read(24);
    for (s, width) in [7, 2, 7, 2].into_iter().enumerate() {
        p.lag_index[s] = bits.read(width);
    }
    for s in 0..SUBFRAMES {
        p.gain_index[s] = bits.read(12);
    }
    for s in 0..SUBFRAMES {
        p.grid[s] = bits.read(1) as usize;
    }

    if p.high_rate {
        bits.read(1);
        let mut msb = bits.read(13);
        if msb >= MSB_RADIX.iter().product::<u32>() {
            return decode_error("g723: invalid pulse position word");
        }

        let mut digits = [0u32; SUBFRAMES];
        for s in (0..SUBFRAMES).rev() {
            digits[s] = msb % MSB_RADIX[s];
            msb /= MSB_RADIX[s];
        }

        for s in 0..SUBFRAMES {
            let low = bits.read(POSITION_LOW_BITS[s]);
            p.position[s] = (digits[s] << POSITION_LOW_BITS[s]) | low;
            if p.position[s] >= MAX_POSITION[s] {
                return decode_error("g723: invalid pulse position");
            }
        }
        for (s, width) in [6, 5, 6, 5].into_iter().enumerate() {
            p.signs[s] = bits.read(width);
        }
    }
    else {
        for s in 0..SUBFRAMES {
            p.position[s] = bits.read(12);
        }
        for s in 0..SUBFRAMES {
            p.signs[s] = bits.read(4);
        }
    }

    Ok(p)
}

/// Decodes the combinatorial code of the positions of `count` pulses on the 30 slot grid.
pub fn unpack_positions(code: u32, count: usize) -> [usize; 6] {
    let mut slots = [0usize; 6];
    let mut index = code;
    let mut start = 0;
    for (r, slot) in slots.iter_mut().enumerate().take(count) {
        let mut c = start;
        loop {
            let weight = COMBINATORIAL[(r + 6 - count) * 30 + c.min(29)];
            if c >= 29 || weight == 0 || index < weight {
                break;
            }
            index -= weight;
            c += 1;
        }
        *slot = c;
        start = c + 1;
    }
    slots
}
