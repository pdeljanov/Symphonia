// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{Result, decode_error};

/// A reader of the packed bits of a Speex packet, most significant bit first.
pub(crate) struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0 }
    }

    pub(crate) fn remaining(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }

    pub(crate) fn peek(&self) -> u32 {
        self.data.get(self.pos / 8).map_or(0, |b| u32::from(b >> (7 - self.pos % 8)) & 1)
    }

    pub(crate) fn advance(&mut self, n: usize) {
        self.pos += n;
    }

    pub(crate) fn read(&mut self, n: u32) -> Result<u32> {
        if self.remaining() < n as usize {
            return decode_error("speex: packet is truncated");
        }
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.peek();
            self.pos += 1;
        }
        Ok(v)
    }
}
