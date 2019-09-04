// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::cmp;
use std::io;

use crate::util::bits::*;
use super::Bytestream;

pub mod huffman {
    //! The `huffman` module provides traits and structures for implementing Huffman decoders.
    
    /// A `HuffmanEntry` represents a Huffman code within a table. It is used to abstract the 
    /// underlying data type of a `HuffmanTable` from the Huffman decoding algorithm.
    /// 
    /// When a Huffman decoder reads a set of bits, those bits may be a partial Huffman code (a 
    /// prefix), or a complete code. If the code is a prefix, then the `HuffmanEntry` for that code 
    /// is a jump entry, pointing the Huffman decoder to where the next set of bits (the next part 
    /// of the Huffman code) should looked up within the `HuffmanTable`. If the code is not a 
    /// prefix, then `HuffmanEntry` is a value entry and the value will be returned by the Huffman 
    /// decoder.
    pub trait HuffmanEntry : Copy + Clone + Sized {
        /// The value type stored in the `HuffmanTable`.
        type ValueType : Copy;
        
        /// Returns true if the `HuffmanEntry` is a value entry.
        fn is_value(&self) -> bool;

        /// Returns true if the `HuffmanEntry` is a jump entry.
        fn is_jump(&self) -> bool;
        
        /// For jump entries only, returns the base offset in the `HuffmanTable` for the jump.
        fn jump_offset(&self) -> usize;

        /// For jump entries only, returns the number of bits the Huffman decoder should read to 
        /// obtain the next part of the Huffman code.
        fn next_len(&self) -> u32;
        
        /// For value entries only, the length of the code.
        fn code_len(&self) -> u32;

        /// For value entries only, consumes the entry and returns the value.
        fn into_value(self) -> Self::ValueType;
    }

    /// A `HuffmanTable` is the table used to map Huffman codes to decoded values.
    /// 
    /// A `HuffmanTable` is structured as a flattened table-of-tables. Wherein there is one table
    /// partitioned into many sub-tables. Each sub-table is a look-up table for a portion of a 
    /// complete Huffman code word. Upon look-up, a sub-table either contains the decoded value
    /// or indicates how many further bits should be read and the index of the sub-table to use for
    /// the the next look-up. In this way, a tree of "prefixes" is formed where the leaf nodes are 
    /// contain decoded values.
    /// 
    /// The maximum length of each sub-table is `2^n_init_bits - 1`. The initial look-up into the 
    /// table should be performed using a word of `n_init_bits`-bits long.
    pub struct HuffmanTable<H: HuffmanEntry + 'static> {
        /// The Huffman table.
        pub data: &'static [H],
        /// The number of bits to read for the initial lookup in the table.
        pub n_init_bits: u32,
        /// The maximum code length within the table in bits.
        pub n_table_bits: u32,
    }

    /// `H8` is a `HuffmanEntry` type for 8-bit data values in a `HuffmanTable`.
    pub type H8 = (u16, u16);

    impl HuffmanEntry for H8 {
        type ValueType = u8;
        
        #[inline(always)]
        fn is_value(&self) -> bool {
            self.0 & 0x8000 != 0
        }
        
        #[inline(always)]
        fn is_jump(&self) -> bool {
            self.0 & 0x8000 == 0
        }
        
        #[inline(always)]
        fn jump_offset(&self) -> usize {
            debug_assert!(self.is_jump());
            self.1 as usize
        }
        
        #[inline(always)]
        fn next_len(&self) -> u32 {
            debug_assert!(self.is_jump());
            u32::from(self.0)
        }

        #[inline(always)]
        fn code_len(&self) -> u32 {
            debug_assert!(self.is_value());
            u32::from(self.0 & 0x7fff)
        }

        #[inline(always)]
        fn into_value(self) -> Self::ValueType {
            debug_assert!(self.is_value());
            self.1 as u8
        }
    }
}

/// Convenience macro for encoding an `H8` value entry for a `HuffmanTable`. See `jmp8` for `val8`'s
/// companion entry.
#[macro_export]
macro_rules! val8 {
    ($data:expr, $len:expr) => { (0x8000 | ($len & 0x7), $data & 0xff) };
}

/// Convenience macro for encoding an `H8` jump entry for a `HuffmanTable`. See `val8` for `jmp8`'s 
/// companion entry.
#[macro_export]
macro_rules! jmp8 {
    ($offset:expr, $len:expr) => { ($len & 0x7, $offset & 0xffff) };
}

/// A `BitReader` provides methods to sequentially read non-byte aligned data from a source
/// `Bytestream`.
///
/// A `BitReader` will consume whole bytes from the passed `Bytestream` as required even if only
/// one bit is to be read. If less than 8 bits are used to service a read then the remaining bits
/// will be saved for later reads. Bits saved from previous reads will be consumed before a new
/// byte is consumed from the source `Bytestream`.
pub trait BitReader {
    /// Discards any saved bits and resets the `BitReader` to prepare it for a byte-aligned read
    /// from the source `Bytestream`.
    fn realign(&mut self);

    /// Ignores one bit from the stream or returns an error.
    #[inline(always)]
    fn ignore_bit<B: Bytestream>(&mut self, src: &mut B) -> io::Result<()> {
        self.ignore_bits(src, 1)
    }

    /// Ignores the specified number of bits from the stream or returns an error.
    fn ignore_bits<B: Bytestream>(&mut self, src: &mut B, num_bits: u32) -> io::Result<()>;

    /// Read a single bit as a boolean value or returns an error.
    fn read_bit<B: Bytestream>(&mut self, src: &mut B) -> io::Result<bool>;

    /// Read up to 32-bits and return them as a u32 or returns an error.
    fn read_bits_leq32<B: Bytestream>(&mut self, src: &mut B, num_bits: u32) -> io::Result<u32>;

    /// Reads up to 32-bits and interprets them as a signed two's complement integer or returns an
    /// error.
    #[inline(always)]
    fn read_bits_leq32_signed<B: Bytestream>(&mut self, src: &mut B, num_bits: u32) -> io::Result<i32> {
        let value = self.read_bits_leq32(src, num_bits)?;
        Ok(sign_extend_leq32_to_i32(value, num_bits))
    }

    /// Read up to 64-bits and return them as a u64 or returns an error.
    fn read_bits_leq64<B: Bytestream>(&mut self, src: &mut B, num_bits: u32) -> io::Result<u64>;

    /// Reads up to 64-bits and interprets them as a signed two's complement integer or returns an
    /// error.
    #[inline(always)]
    fn read_bits_leq64_signed<B: Bytestream>(&mut self, src: &mut B, num_bits: u32) -> io::Result<i64> {
        let value = self.read_bits_leq64(src, num_bits)?;
        Ok(sign_extend_leq64_to_i64(value, num_bits))
    }

    /// Reads a unary encoded integer up to u32 or returns an error.
    fn read_unary<B: Bytestream>(&mut self, src: &mut B) -> io::Result<u32>;

    /// Reads a Huffman code from the `Bytestream` using the provided `HuffmanTable` and returns the 
    /// decoded value, or an error. 
    /// 
    /// This function efficiently operates on blocks of code bits and may read bits, and thus 
    /// potentially an extra byte, past the end of a particular code. These extra bits remain 
    /// buffered by the Bitstream for future reads, however, to prevent reading past critical byte 
    /// boundaries, `lim_bits` may be provided to limit the maximum number of bits read.
    fn read_huffman<B: Bytestream, H: huffman::HuffmanEntry>(
        &mut self, 
        src: &mut B,
        table: &huffman::HuffmanTable<H>,
        lim_bits: u32,
    ) -> io::Result<(H::ValueType, u32)>;
}

/// A `BitReaderLtr` provides an implementation of a `BitReader` that interprets sequential bits in
/// a single read as descending in significance. That is to say, if N-bits are read from a
/// `BitReaderLtr` then bit 0 (the first bit read from the source) is the most-significant bit and
/// bit N-1 is the least-significant.
pub struct BitReaderLtr {
    bits: u32,
    n_bits_left: u32, 
}

impl BitReaderLtr {
    /// Instantiates a new `BitReaderLtr`.
    pub fn new() -> BitReaderLtr {
        BitReaderLtr {
            bits: 0,
            n_bits_left: 0,
        }
    }

    #[inline(always)]
    pub fn read_bits_leq8<B: Bytestream>(&mut self, src: &mut B, num_bits: u32) -> io::Result<u32> {
        debug_assert!(num_bits <= 8);

        if self.n_bits_left < num_bits {
            self.bits = (self.bits << 8) | u32::from(src.read_u8()?);
            self.n_bits_left += 8
        }

        self.n_bits_left -= num_bits;
        Ok((self.bits >> self.n_bits_left) & ((1 << num_bits) - 1))
    }
}

impl BitReader for BitReaderLtr {
    #[inline(always)]
    fn realign(&mut self) {
        self.n_bits_left = 0;
    }

    #[inline(always)]
    fn ignore_bits<B: Bytestream>(&mut self, src: &mut B, mut num_bits: u32) -> io::Result<()> {
        // If the number of bits to ignore is less than the amount left, simply reduce the amount 
        // left.
        if num_bits <= self.n_bits_left {
            self.n_bits_left -= num_bits;
        }
        // Otherwise, there are more bits to ignore than are left.
        else {
            // Consume all bits left.
            num_bits -= self.n_bits_left;

            // Consume 8 bit blocks at a time.
            while num_bits >= 8 {
                src.read_u8()?;
                num_bits -= 8;
            }

            // Less than 8 bits remain to be ignored.
            if num_bits > 0 {
                self.bits = u32::from(src.read_u8()?);
                self.n_bits_left = 8 - num_bits;
            }
            else {
                self.n_bits_left = 0;
            }
        }

        Ok(())
    }

    #[inline(always)]
    fn read_bit<B: Bytestream>(&mut self, src: &mut B) -> io::Result<bool> {
        if self.n_bits_left == 0 {
            self.bits = u32::from(src.read_u8()?);
            self.n_bits_left = 8;
        }
        self.n_bits_left -= 1;
        let mask = 1u32 << self.n_bits_left;
        Ok((self.bits & mask) == mask)
    }

    #[inline(always)]
    fn read_bits_leq32<B: Bytestream>(&mut self, src: &mut B, mut num_bits: u32) -> io::Result<u32> {
        debug_assert!(num_bits <= 32);

        let mask = !(0xffff_ffff_ffff_ffffu64 << num_bits) as u32;

        let mut res: u32 = self.bits;

        if num_bits <= self.n_bits_left {
            self.n_bits_left -= num_bits;
            res >>= self.n_bits_left;
        }
        else {
            num_bits -= self.n_bits_left;

            while num_bits >= 8 {
                res <<= 8;
                res |= u32::from(src.read_u8()?);
                num_bits -= 8;
            }

            if num_bits > 0 {
                res <<= num_bits;
                self.bits = u32::from(src.read_u8()?);
                self.n_bits_left = 8 - num_bits;
                res |= self.bits >> self.n_bits_left;
            }
            else {
                self.n_bits_left = 0;
            }
        }

        Ok(res & mask)
    }

    #[inline(always)]
    fn read_bits_leq64<B: Bytestream>(&mut self, src: &mut B, num_bits: u32) -> io::Result<u64> {
        debug_assert!(num_bits <= 64);

        if num_bits > 32 {
            let shift = num_bits - 32;
            let upper = u64::from(self.read_bits_leq32(src, 32)?) << shift;
            let lower = u64::from(self.read_bits_leq32(src, shift)?);
            Ok(upper | lower)
        }
        else {
            Ok(u64::from(self.read_bits_leq32(src, num_bits)?))
        }
    }

    #[inline(always)]
    fn read_unary<B: Bytestream>(&mut self, src: &mut B) -> io::Result<u32> {
        let mut num = 0;

        loop {

            let zeros = 
                if self.n_bits_left == 0 {
                    self.bits = u32::from(src.read_u8()?);
                    self.n_bits_left = 8;

                    (self.bits as u8).leading_zeros()
                }
                else {
                    // Count the number of valid leading zeros in bits by filling the upper unused 
                    // 24 bits with 1s and rotating right by the number of bits left. The leading 
                    // bits will then contain the number of unread bits.
                    let byte = (self.bits | 0xffff_ff00).rotate_right(self.n_bits_left);
                    byte.leading_zeros()
                };

            // Increment the decoded number.
            num += zeros;

            // A unary encoded number is suffixed with a 1. If the number of bits remaining in the
            // currently readable byte is greater than the number of 0s counted this iteration,
            // then a 1 was encounted. The unary number is decoded at this point. Subtract an extra
            // bit from the bits_left value to account for the suffixed 1.
            if zeros < self.n_bits_left {
                self.n_bits_left -= zeros + 1;
                break;
            }

            self.n_bits_left -= zeros;
        }

        Ok(num)
    }

    #[inline(always)]
    fn read_huffman<B: Bytestream, H: huffman::HuffmanEntry>(
        &mut self, 
        src: &mut B,
        table: &huffman::HuffmanTable<H>,
        lim_bits: u32,
    ) -> io::Result<(H::ValueType, u32)> {

        debug_assert!(lim_bits > 0);
        debug_assert!(!table.data.is_empty());

        // The most recent number of bits read from the bitstream.
        let mut n_bits;
        let mut code_len;
       
        // The table's longest possible code word is smaller than lim_bits. Since the limit cannot 
        // be reached with this table, do not check the limit. This should be the case for most 
        // reads in a Huffman bitstream.
        let entry = if table.n_table_bits <= lim_bits {
            n_bits = table.n_init_bits;
            code_len = n_bits;

            let mut entry = table.data[self.read_bits_leq8(src, n_bits)? as usize];

            while entry.is_jump() {
                n_bits = entry.next_len();

                let prefix = self.read_bits_leq8(src, n_bits)? as usize;
                entry = table.data[entry.jump_offset() + prefix];

                code_len += n_bits;
            }
            
            entry
        }
        // The table's longest possible code word is longer than lim_bits. It is possible that that 
        // the limit could be exceeded, therefore, check the limit as the code is decoded.
        else {
            n_bits = cmp::min(table.n_init_bits, lim_bits);
            code_len = n_bits;

            // The limit is not constraining the initial look-up in the table, however it may 
            // constrain some further look-up. Check the limit before each look-up after the first.
            if table.n_init_bits < lim_bits {
                let mut entry = table.data[self.read_bits_leq8(src, n_bits)? as usize];

                while entry.is_jump() && code_len < lim_bits {
                    n_bits = cmp::min(entry.next_len(), lim_bits - code_len);
                    
                    let prefix = self.read_bits_leq8(src, n_bits)? << (entry.next_len() - n_bits);
                    
                    entry = table.data[entry.jump_offset() + prefix as usize];

                    code_len += n_bits;
                }
                
                entry
            }
            // The table's initial lookup length is longer than the limit. Read the remaining bits 
            // up-to the limit and try to decode them.
            else {
                let prefix = (self.read_bits_leq8(src, n_bits)? as usize) 
                                << (table.n_init_bits - lim_bits);

                table.data[prefix]
            }
        };

        // If the entry is a data entry then a valid code was decoded.
        if entry.is_value() && n_bits >= entry.code_len() {
            // Extra bits may have been consumed by the decoder. Return any extra bits back to the
            // bitstream.
            let extra_bits = n_bits - entry.code_len();
            self.n_bits_left += extra_bits;

            // Return the value with the code length.
            Ok((entry.into_value(), code_len - extra_bits))
        }
        // If the entry is a jump entry then decoding exited early because lim_bits was reached, 
        // since no matter how the table is followed, it will always reach a data entry. Any read
        // bits remain consumed. Return an error as this would generally indicate either an error 
        // in how the bitstream is being used, or the stream is malformed.
        else {
            Err(io::Error::new(io::ErrorKind::Other, "reached bit limit for huffman decode"))
        }
    }
}

/// A `BitStream` provides methods to sequentially read non-byte aligned data from an inner
/// `Bytestream`.
///
/// A `BitStream` will consume whole bytes from the inner `Bytestream` as required even if only
/// one bit is to be read. If less than 8 bits are used to service a read then the remaining bits
/// will be saved for later reads. Bits saved from previous reads will be consumed before a new
/// byte is consumed from the source `Bytestream`.
pub trait BitStream {
    /// Discards any saved bits and resets the `BitStream` to prepare it for a byte-aligned read
    /// from the source `Bytestream`.
    fn realign(&mut self);

    /// Ignores one bit from the stream or returns an error.
    #[inline(always)]
    fn ignore_bit(&mut self) -> io::Result<()> {
        self.ignore_bits(1)
    }

    /// Ignores the specified number of bits from the stream or returns an error.
    fn ignore_bits(&mut self, bit_width: u32) -> io::Result<()>;

    /// Read a single bit as a boolean value or returns an error.
    fn read_bit(&mut self) -> io::Result<bool>;

    /// Read up to 32-bits and return them as a u32 or returns an error.
    fn read_bits_leq32(&mut self, bit_width: u32) -> io::Result<u32>;

    /// Reads up to 32-bits and interprets them as a signed two's complement integer or returns an
    /// error.
    #[inline(always)]
    fn read_bits_leq32_signed(&mut self, bit_width: u32) -> io::Result<i32> {
        let value = self.read_bits_leq32(bit_width)?;
        Ok(sign_extend_leq32_to_i32(value, bit_width))
    }

    /// Read up to 64-bits and return them as a u64 or returns an error.
    fn read_bits_leq64(&mut self, bit_width: u32) -> io::Result<u64>;

    /// Reads up to 64-bits and interprets them as a signed two's complement integer or returns an
    /// error.
    #[inline(always)]
    fn read_bits_leq64_signed(&mut self, bit_width: u32) -> io::Result<i64> {
        let value = self.read_bits_leq64(bit_width)?;
        Ok(sign_extend_leq64_to_i64(value, bit_width))
    }

    /// Reads a unary encoded integer up to u32 or returns an error.
    fn read_unary(&mut self) -> io::Result<u32>;

    /// Reads a Huffman code from the `BitStream` using the provided `HuffmanTable` and returns the 
    /// decoded value, or an error. 
    /// 
    /// This function efficiently operates on blocks of code bits and may read bits, and thus 
    /// potentially an extra byte, past the end of a particular code. These extra bits remain
    /// buffered by the `BitStream` for future reads, however, to prevent reading past critical byte 
    /// boundaries, `lim_bits` may be provided to limit the maximum number of bits read.
    fn read_huffman<H: huffman::HuffmanEntry>(
        &mut self, 
        table: &huffman::HuffmanTable<H>,
        lim_bits: u32,
    ) -> io::Result<(H::ValueType, u32)>;
}

/// `BitStreamLtr` wraps `BitReaderLtr` into a `BitStream`.
pub struct BitStreamLtr<B: Bytestream> {
    inner: B,
    reader: BitReaderLtr,
}

impl<B: Bytestream> BitStreamLtr<B> {
    pub fn new(inner: B) -> BitStreamLtr<B> {
        BitStreamLtr {
            inner,
            reader: BitReaderLtr::new(),
        }
    }
}

impl<B: Bytestream> BitStream for BitStreamLtr<B> {
    #[inline(always)]
    fn realign(&mut self) {
        self.reader.realign();
    }

    #[inline(always)]
    fn ignore_bits(&mut self, bit_width: u32) -> io::Result<()> {
        self.reader.ignore_bits(&mut self.inner, bit_width)
    }

    #[inline(always)]
    fn read_bit(&mut self) -> io::Result<bool> {
        self.reader.read_bit(&mut self.inner)
    }

    #[inline(always)]
    fn read_bits_leq32(&mut self, bit_width: u32) -> io::Result<u32> {
        self.reader.read_bits_leq32(&mut self.inner, bit_width)
    }

    #[inline(always)]
    fn read_bits_leq64(&mut self, bit_width: u32) -> io::Result<u64> {
        self.reader.read_bits_leq64(&mut self.inner, bit_width)
    }

    #[inline(always)]
    fn read_unary(&mut self) -> io::Result<u32> {
        self.reader.read_unary(&mut self.inner)
    }

    #[inline(always)]
    fn read_huffman<H: huffman::HuffmanEntry>(
        &mut self, 
        table: &huffman::HuffmanTable<H>,
        lim_bits: u32,
    ) -> io::Result<(H::ValueType, u32)> {
        self.reader.read_huffman(&mut self.inner, table, lim_bits)
    }
}

#[cfg(test)]
mod tests {
    use crate::io::BufStream;
    use super::{BitReader, BitReaderLtr, huffman::{HuffmanTable, H8}};

    #[test]
    fn verify_read_bit() {
        let mut stream = BufStream::new(&[0b1010_1010]);

        let mut br = BitReaderLtr::new();

        assert_eq!(br.read_bit(&mut stream).unwrap(), true);
        assert_eq!(br.read_bit(&mut stream).unwrap(), false);
        assert_eq!(br.read_bit(&mut stream).unwrap(), true);
        assert_eq!(br.read_bit(&mut stream).unwrap(), false);
        assert_eq!(br.read_bit(&mut stream).unwrap(), true);
        assert_eq!(br.read_bit(&mut stream).unwrap(), false);
        assert_eq!(br.read_bit(&mut stream).unwrap(), true);
        assert_eq!(br.read_bit(&mut stream).unwrap(), false);
    }

    #[test]
    fn verify_read_bits_leq32() {
        let mut stream = BufStream::new(&[0b1010_0101, 0b0111_1110, 0b1101_0011]);

        let mut br = BitReaderLtr::new();

        assert_eq!(br.read_bits_leq32(&mut stream,  4).unwrap(), 0b0000_0000_0000_1010);
        assert_eq!(br.read_bits_leq32(&mut stream,  4).unwrap(), 0b0000_0000_0000_0101);
        assert_eq!(br.read_bits_leq32(&mut stream, 13).unwrap(), 0b0000_1111_1101_1010);
        assert_eq!(br.read_bits_leq32(&mut stream,  3).unwrap(), 0b0000_0000_0000_0011);
    }

    #[test]
    fn verify_read_bits_leq64() {
        let mut stream = BufStream::new(
            &[0x99, 0xaa, 0x55, 0xff, 0xff, 0x55, 0xaa, 0x99, 
              0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);

        let mut br = BitReaderLtr::new();

        assert_eq!(br.read_bits_leq64(&mut stream, 40).unwrap(), 0x99aa55ffff);
        assert_eq!(br.read_bits_leq64(&mut stream,  4).unwrap(), 0x05);
        assert_eq!(br.read_bits_leq64(&mut stream,  4).unwrap(), 0x05);
        assert_eq!(br.read_bits_leq64(&mut stream, 16).unwrap(), 0xaa99);
        assert_eq!(br.read_bits_leq64(&mut stream, 64).unwrap(), 0x1122334455667788);
    }

    #[test]
    fn verify_read_unary() {
        let mut stream = BufStream::new(
            &[0b0000_0001, 0b0001_0000, 0b0000_0000, 0b1000_0000, 0b1111_1011]);

        let mut br = BitReaderLtr::new();

        assert_eq!(br.read_unary(&mut stream).unwrap(),  7);
        assert_eq!(br.read_unary(&mut stream).unwrap(),  3);
        assert_eq!(br.read_unary(&mut stream).unwrap(), 12);
        assert_eq!(br.read_unary(&mut stream).unwrap(),  7);
        assert_eq!(br.read_unary(&mut stream).unwrap(),  0);
        assert_eq!(br.read_unary(&mut stream).unwrap(),  0);
        assert_eq!(br.read_unary(&mut stream).unwrap(),  0);
        assert_eq!(br.read_unary(&mut stream).unwrap(),  0);
        assert_eq!(br.read_unary(&mut stream).unwrap(),  1);
        assert_eq!(br.read_unary(&mut stream).unwrap(),  0);
    }

        #[test]
    fn verify_read_huffman() {
        // A simple Huffman table.
        const TABLE: HuffmanTable<H8> = HuffmanTable {
            data: &[
                // 0b ... (0)
                jmp8!(16, 2),    // 0b0000
                jmp8!(20, 1),    // 0b0001
                val8!(0x11, 3),    // 0b0010
                val8!(0x11, 3),    // 0b0011
                val8!(0x1, 3),    // 0b0100
                val8!(0x1, 3),    // 0b0101
                val8!(0x10, 3),    // 0b0110
                val8!(0x10, 3),    // 0b0111
                val8!(0x0, 1),    // 0b1000
                val8!(0x0, 1),    // 0b1001
                val8!(0x0, 1),    // 0b1010
                val8!(0x0, 1),    // 0b1011
                val8!(0x0, 1),    // 0b1100
                val8!(0x0, 1),    // 0b1101
                val8!(0x0, 1),    // 0b1110
                val8!(0x0, 1),    // 0b1111

                // 0b0000 ... (16)
                val8!(0x22, 2),    // 0b00
                val8!(0x2, 2),    // 0b01
                val8!(0x12, 1),    // 0b10
                val8!(0x12, 1),    // 0b11

                // 0b0001 ... (20)
                val8!(0x21, 1),    // 0b0
                val8!(0x20, 1),    // 0b1
            ],
            n_init_bits: 4,
            n_table_bits: 8,
        };

        let mut stream = BufStream::new(&[0b010_00000, 0b0_00001_00, 0b0001_001_0]);

        let mut br = BitReaderLtr::new();

        assert_eq!(br.read_huffman(&mut stream, &TABLE, 24).unwrap().0, 0x1 );
        assert_eq!(br.read_huffman(&mut stream, &TABLE, 21).unwrap().0, 0x22);
        assert_eq!(br.read_huffman(&mut stream, &TABLE, 15).unwrap().0, 0x12);
        assert_eq!(br.read_huffman(&mut stream, &TABLE, 10).unwrap().0, 0x2 );
        assert_eq!(br.read_huffman(&mut stream, &TABLE,  3).unwrap().0, 0x11);
    }
}