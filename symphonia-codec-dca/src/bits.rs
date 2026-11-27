// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{Result, decode_error};
use symphonia_core::io::{BitReaderLtr, ReadBitsLtr};

/// A reader of the bit stream that reports a truncated frame as a decode error.
pub(crate) struct Bits<'a>(pub(crate) BitReaderLtr<'a>);

impl Bits<'_> {
    pub(crate) fn u(&mut self, n: u32) -> Result<u32> {
        match self.0.read_bits_leq32(n) {
            Ok(v) => Ok(v),
            Err(_) => decode_error("dca: frame is truncated"),
        }
    }

    pub(crate) fn flag(&mut self) -> Result<bool> {
        Ok(self.u(1)? != 0)
    }

    pub(crate) fn skip(&mut self, n: u32) -> Result<()> {
        match self.0.ignore_bits(n) {
            Ok(()) => Ok(()),
            Err(_) => decode_error("dca: frame is truncated"),
        }
    }

    /// Reads an `n` bit two's complement value.
    pub(crate) fn s(&mut self, n: u32) -> Result<i32> {
        let v = self.u(n)? as i32;
        Ok(if n > 0 && v >> (n - 1) != 0 { v - (1 << n) } else { v })
    }
}
