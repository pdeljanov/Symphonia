// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// LSB-first bit reader, porting the getbit/getbits macros from unpack3.c.

pub struct Bits<'a> {
    data: &'a [u8],
    ptr: usize,
    bc: u32,
    sr: u64,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Bits { data, ptr: 0, bc: 0, sr: 0 }
    }

    #[inline(always)]
    pub fn getbit(&mut self) -> u32 {
        if self.bc == 0 {
            let byte = self.next_byte() as u64;
            self.sr = byte;
            self.bc = 7;
            let bit = (self.sr & 1) as u32;
            self.sr >>= 1;
            bit
        } else {
            self.bc -= 1;
            let bit = (self.sr & 1) as u32;
            self.sr >>= 1;
            bit
        }
    }

    #[inline(always)]
    pub fn getbits(&mut self, nbits: u32) -> u32 {
        if nbits == 0 {
            return 0;
        }
        while nbits > self.bc {
            let byte = self.next_byte() as u64;
            self.sr |= byte << self.bc;
            self.bc += 8;
        }
        let value = (self.sr & ((1u64 << nbits) - 1)) as u32;
        self.bc -= nbits;
        self.sr >>= nbits;
        value
    }

    #[inline(always)]
    fn next_byte(&mut self) -> u8 {
        if self.ptr < self.data.len() {
            let b = self.data[self.ptr];
            self.ptr += 1;
            b
        } else {
            0x00
        }
    }
}
