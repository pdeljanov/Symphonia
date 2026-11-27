// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

pub(crate) type BitResult<T> = Result<T, &'static str>;

/// A most-significant-bit-first reader over a byte slice.
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) fn seek(&mut self, pos: usize) -> BitResult<()> {
        if pos > self.data.len() * 8 {
            return Err("truehd: bitstream overrun");
        }
        self.pos = pos;
        Ok(())
    }

    pub(crate) fn skip(&mut self, n: usize) -> BitResult<()> {
        if self.pos + n > self.data.len() * 8 {
            return Err("truehd: bitstream overrun");
        }
        self.pos += n;
        Ok(())
    }

    pub(crate) fn align16(&mut self) {
        self.pos = (self.pos + 15) & !15;
    }

    /// Reads up to 32 bits.
    pub(crate) fn read(&mut self, n: u32) -> BitResult<u32> {
        debug_assert!(n <= 32);
        if self.pos + n as usize > self.data.len() * 8 {
            return Err("truehd: bitstream overrun");
        }

        let mut value = 0u64;
        let mut need = n;
        while need > 0 {
            let byte = u32::from(self.data[self.pos >> 3]);
            let avail = 8 - (self.pos & 7) as u32;
            let take = avail.min(need);
            value = (value << take) | u64::from((byte >> (avail - take)) & ((1 << take) - 1));
            self.pos += take as usize;
            need -= take;
        }

        Ok(value as u32)
    }

    pub(crate) fn bit(&mut self) -> BitResult<bool> {
        Ok(self.read(1)? != 0)
    }

    /// Reads an `n`-bit two's complement value, `n` being at most 31.
    pub(crate) fn read_signed(&mut self, n: u32) -> BitResult<i32> {
        if n == 0 {
            return Ok(0);
        }
        let value = self.read(n)? as i32;
        Ok((value << (32 - n)) >> (32 - n))
    }
}
