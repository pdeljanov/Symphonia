// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The cyclic redundancy check of the SBR extension data, bs_sbr_crc_bits (ISO/IEC 14496-3:2009,
//! 4.5.2.8.1 and Figure 4.21).
//!
//! The CRC is defined by the generator polynomial G10(x) = x^10 + x^9 + x^5 + x^4 + x + 1 with an
//! initial value of zero. It covers all bits of sbr_extension_data() following bs_sbr_crc_bits,
//! including the bs_fill_bits.

/// The number of bits of the CRC.
const WIDTH: u32 = 10;

/// The generator polynomial without its x^10 term.
const POLY: u16 = 0x233;

/// Mask of the CRC register.
const MASK: u16 = (1 << WIDTH) - 1;

/// Compute the table for byte-wise CRC calculation: the register content after shifting the byte
/// `i` placed in the uppermost bits of the register through the register.
const fn make_table() -> [u16; 256] {
    let mut table = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = (i as u16) << (WIDTH - 8);
        let mut bit = 0;
        while bit < 8 {
            let msb = crc & (1 << (WIDTH - 1));
            crc = (crc << 1) & MASK;
            if msb != 0 {
                crc ^= POLY;
            }
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRC_TABLE: [u16; 256] = make_table();

/// Shift one message bit into the CRC register.
#[inline(always)]
fn update_bit(crc: u16, bit: u16) -> u16 {
    let feedback = ((crc >> (WIDTH - 1)) ^ bit) & 1;
    let crc = (crc << 1) & MASK;
    if feedback != 0 { crc ^ POLY } else { crc }
}

/// Shift one message byte, most-significant bit first, into the CRC register.
#[inline(always)]
fn update_byte(crc: u16, byte: u8) -> u16 {
    let index = ((crc >> (WIDTH - 8)) ^ u16::from(byte)) & 0xff;
    ((crc << 8) & MASK) ^ CRC_TABLE[usize::from(index)]
}

/// Compute the SBR CRC over the bits `start_bit..end_bit` of `buf`, where bit 0 is the
/// most-significant bit of the first byte. Bits beyond the end of `buf` are treated as zeros.
pub fn sbr_crc(buf: &[u8], start_bit: usize, end_bit: usize) -> u16 {
    let bit_at = |pos: usize| -> u16 {
        buf.get(pos / 8).map_or(0, |&byte| u16::from(byte >> (7 - pos % 8)) & 1)
    };

    let mut crc = 0;
    let mut pos = start_bit;

    // Leading bits up-to the next byte boundary.
    while pos < end_bit && pos % 8 != 0 {
        crc = update_bit(crc, bit_at(pos));
        pos += 1;
    }

    // Whole bytes.
    while pos + 8 <= end_bit {
        crc = update_byte(crc, buf.get(pos / 8).copied().unwrap_or(0));
        pos += 8;
    }

    // Trailing bits.
    while pos < end_bit {
        crc = update_bit(crc, bit_at(pos));
        pos += 1;
    }

    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Slow reference: the remainder of the polynomial division of M(x) * x^10 by G10(x), where
    /// M(x) is the message, computed by long division of the augmented message.
    fn reference_crc(bits: &[u8]) -> u16 {
        const GENERATOR: u32 = 0x633; // x^10 + x^9 + x^5 + x^4 + x + 1

        let mut rem = 0u32;
        for &bit in bits.iter().chain([0u8; 10].iter()) {
            rem = (rem << 1) | u32::from(bit);
            if rem & (1 << 10) != 0 {
                rem ^= GENERATOR;
            }
        }
        rem as u16
    }

    fn to_bits(buf: &[u8], start: usize, end: usize) -> Vec<u8> {
        (start..end).map(|i| (buf[i / 8] >> (7 - i % 8)) & 1).collect()
    }

    #[test]
    fn verify_known_answers() {
        // An empty message.
        assert_eq!(sbr_crc(&[], 0, 0), 0);
        // M(x) = 1: x^10 mod G10(x) = x^9 + x^5 + x^4 + x + 1.
        assert_eq!(sbr_crc(&[0x80], 0, 1), 0x233);
        // M(x) = x: x^11 mod G10(x) = x^9 + x^6 + x^4 + x^2 + 1.
        assert_eq!(sbr_crc(&[0x80], 0, 2), 0x255);
        // Leading zero bits do not change the CRC (zero initial value).
        assert_eq!(sbr_crc(&[0x00, 0x80], 0, 9), 0x233);
        // The CRC of a message followed by its CRC is zero.
        let msg = [0xa5u8, 0x3c, 0x0f];
        let crc = sbr_crc(&msg, 0, 24);
        let ext = [msg[0], msg[1], msg[2], (crc >> 2) as u8, (crc << 6) as u8];
        assert_eq!(sbr_crc(&ext, 0, 34), 0);
    }

    #[test]
    fn verify_against_reference() {
        let mut state = 0x1234_5678u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state >> 8
        };

        for _ in 0..2000 {
            let len = (next() % 40) as usize;
            let buf: Vec<u8> = (0..len).map(|_| next() as u8).collect();

            let total = len * 8;
            let start = if total > 0 { next() as usize % (total + 1) } else { 0 };
            let end = start + if total > start { next() as usize % (total - start + 1) } else { 0 };

            let expected = reference_crc(&to_bits(&buf, start, end));
            assert_eq!(sbr_crc(&buf, start, end), expected, "{:?} {} {}", buf, start, end);
        }
    }
}
