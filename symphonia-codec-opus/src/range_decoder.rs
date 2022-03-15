// Symphonia
// Copyright (c) 2019-2021 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]
// The following lints are allowed in all Symphonia crates. Please see clippy.toml for their
// justification.
#![allow(clippy::comparison_chain)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_range_contains)]
// Disable to better express the specification.
#![allow(clippy::collapsible_else_if)]

use symphonia_core::errors::Result;
use symphonia_core::io::{BufReader, ReadBytes};

/// The range decoder for the Opus Decoder.
/// See RFC 6716 Section 4.1, https://tools.ietf.org/pdf/rfc7845.pdf.
pub struct RangeDecoder<'a> {
    range: u32,
    value: u32,
    number_of_bits: u32,
    reader: &'a mut BufReader<'a>,
    previous_byte: u8,
}

impl RangeDecoder<'_> {
    /// Create a new range decoder that reads from a `BufReader`.
    /// See RFC 6716 Section 4.1.1.    
    pub fn try_new<'a>(reader: &'a mut BufReader<'a>) -> Result<RangeDecoder<'a>> {
        let previous_byte = reader.read_byte()?;
        let range = 128;
        let value: u32 = 127u32 - u32::from(previous_byte >> 1);

        // number_of_bits can also be initialised to 33 after the first renormalization.
        // See RFC 6716 Section 4.1.6.
        let mut range_decoder =
            RangeDecoder { range, value, number_of_bits: 9, reader, previous_byte };
        range_decoder.normalize();
        Ok(range_decoder)
    }

    /// Decodes the current symbol given a frequency table and the sum of that frequency table.
    /// See RFC 6716 Section 4.1.2.       
    fn decode_symbol(&mut self, symbol_frequencies: &[u32], total: u32) -> u32 {
        use std::cmp;

        // current_symbol_frequency == fs.
        // lower_symbol_frequency_threshold = fl.
        // higher_symbol_frequency_threshold = fh.
        // index_of_current_symbol = k.
        // See RFC 6716 Section 4.1.2 for more details on fs, fl, fh and k.
        let current_symbol_frequency =
            total - cmp::min(self.value / (self.range / total) + 1, total);
        let mut lower_symbol_frequency_threshold = 0;
        let mut higher_symbol_frequency_threshold = 0;
        let mut index_of_current_symbol = 0;
        for frequency in symbol_frequencies {
            higher_symbol_frequency_threshold = lower_symbol_frequency_threshold + frequency;
            if lower_symbol_frequency_threshold <= current_symbol_frequency
                && current_symbol_frequency < lower_symbol_frequency_threshold + frequency
            {
                break;
            }
            lower_symbol_frequency_threshold += frequency;
            index_of_current_symbol += 1;
        }

        self.value = self.value - self.range / total * (total - higher_symbol_frequency_threshold);
        if lower_symbol_frequency_threshold > 0 {
            self.range = self.range / total
                * (higher_symbol_frequency_threshold - lower_symbol_frequency_threshold);
        }
        else {
            self.range =
                self.range - self.range / total * (total - higher_symbol_frequency_threshold);
        }

        self.normalize();

        index_of_current_symbol
    }

    // Normalizes the range decoder's range.
    // See RFC 6716 Section 4.1.2.1.
    fn normalize(&mut self) -> Result<()> {
        while self.range <= (1 << 23) {
            self.number_of_bits += 8;
            self.range <<= 8;
            let carry_bit = self.previous_byte & 1;
            let current_byte = self.reader.read_byte()?;
            self.value = ((self.value << 8)
                + u32::from(255 - (carry_bit << 7) | current_byte >> 1))
                & 0x7FFFFFF;
            self.previous_byte = current_byte;
        }
        Ok(())
    }
}
