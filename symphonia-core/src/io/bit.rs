// Symphonia
// Copyright (c) 2019 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::cmp::min;
use std::io;

use crate::util::bits::*;
use crate::io::ReadBytes;

fn end_of_bitstream_error<T>() -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::Other, "unexpected end of bitstream"))
}

pub mod huffman {
    //! The `huffman` module provides traits and structures for implementing Huffman decoders.

    use std::marker::PhantomData;

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

        fn root(init_len: usize) -> Self;

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
    pub type H8 = (u16, u16, PhantomData<u8>);
    pub type H16 = (u16, u16, PhantomData<u16>);

    impl HuffmanEntry for H8 {
        type ValueType = u8;

        #[inline(always)]
        fn root(init_len: usize) -> Self {
            (init_len as u16 & 0x7, 0, std::marker::PhantomData)
        }

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
            self.1 as Self::ValueType
        }
    }

    impl HuffmanEntry for H16 {
        type ValueType = u16;

        #[inline(always)]
        fn root(init_len: usize) -> Self {
            (init_len as u16 & 0x7, 0, std::marker::PhantomData)
        }

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
            self.1 as Self::ValueType
        }
    }

}

/// Convenience macro for encoding an `H8` value entry for a `HuffmanTable`. See `jmp8` for
/// `val8`'s companion entry.
#[macro_export]
macro_rules! val8 {
    ($data:expr, $len:expr) => {
        (0x8000 | ($len & 0x7), $data & 0xff, std::marker::PhantomData)
    };
}

/// Convenience macro for encoding an `H8` jump entry for a `HuffmanTable`. See `val8` for `jmp8`'s
/// companion entry.
#[macro_export]
macro_rules! jmp8 {
    ($offset:expr, $len:expr) => {
        ($len & 0x7, $offset & 0xffff, std::marker::PhantomData)
    };
}

/// Convenience macro for encoding an `H6` value entry for a `HuffmanTable`. See `jmp16` for
/// `val16`'s companion entry.
#[macro_export]
macro_rules! val16 {
    ($data:expr, $len:expr) => {
        (0x8000 | ($len & 0x7), $data & 0xffff, std::marker::PhantomData)
    };
}

/// Convenience macro for encoding an `H6` jump entry for a `HuffmanTable`. See `val16` for
/// `jmp16`'s companion entry.
#[macro_export]
macro_rules! jmp16 {
    ($offset:expr, $len:expr) => {
        ($len & 0x7, $offset & 0xffff, std::marker::PhantomData)
    };
}

mod private {
    use std::io;

    pub trait FetchBitsLtr {
        /// Discard any remaining bits in the source and fetch new bits.
        fn fetch_bits(&mut self) -> io::Result<()>;

        /// Fetch new bits, and append them after the remaining bits.
        fn fetch_bits_partial(&mut self) -> io::Result<()>;

        /// Get all the bits in the source.
        fn get_bits(&self) -> u64;

        /// Get the number of bits left in the source.
        fn num_bits_left(&self) -> u32;

        /// Consume `num` bits from the source.
        fn consume_bits(&mut self, num: u32);
    }

    pub trait FetchBitsRtl {
        /// Discard any remaining bits in the source and fetch new bits.
        fn fetch_bits(&mut self) -> io::Result<()>;

        /// Fetch new bits, and append them after the remaining bits.
        fn fetch_bits_partial(&mut self) -> io::Result<()>;

        /// Get all the bits in the source.
        fn get_bits(&self) -> u64;

        /// Get the number of bits left in the source.
        fn num_bits_left(&self) -> u32;

        /// Consume `num` bits from the source.
        fn consume_bits(&mut self, num: u32);
    }
}

/// A `FiniteBitStream` is a bit stream that has a known length in bits.
pub trait FiniteBitStream {
    /// Gets the number of bits left unread.
    fn bits_left(&self) -> u64;
}

/// `ReadBitsLtr` reads bits from most-significant to least-significant.
pub trait ReadBitsLtr : private::FetchBitsLtr {
    /// Discards any saved bits and resets the `BitStream` to prepare it for a byte-aligned read.
    #[inline(always)]
    fn realign(&mut self) {
        let skip = self.num_bits_left() & 0x7;
        self.consume_bits(skip);
    }

    /// Ignores the specified number of bits from the stream or returns an error.
    #[inline(always)]
    fn ignore_bits(&mut self, mut num_bits: u32) -> io::Result<()> {
        if num_bits <= self.num_bits_left() {
            self.consume_bits(num_bits);
        }
        else {
            // Consume whole bit caches directly.
            while num_bits > self.num_bits_left() {
                num_bits -= self.num_bits_left();
                self.fetch_bits()?;
            }

            if num_bits > 0 {
                // Shift out in two parts to prevent panicing when num_bits == 64.
                self.consume_bits(num_bits - 1);
                self.consume_bits(1);
            }
        }

        Ok(())
    }

    /// Ignores one bit from the stream or returns an error.
    #[inline(always)]
    fn ignore_bit(&mut self) -> io::Result<()> {
        self.ignore_bits(1)
    }

    /// Read a single bit as a boolean value or returns an error.
    #[inline(always)]
    fn read_bit(&mut self) -> io::Result<bool> {
        if self.num_bits_left() < 1 {
            self.fetch_bits()?;
        }

        let bit = self.get_bits() & (1 << 63) != 0;

        self.consume_bits(1);
        Ok(bit)
    }

    /// Reads up to 32-bits and interprets them as a signed two's complement integer or returns an
    /// error.
    #[inline(always)]
    fn read_bits_leq32(&mut self, mut bit_width: u32) -> io::Result<u32> {
        debug_assert!(bit_width <= u32::BITS);

        // Shift in two 32-bit operations instead of a single 64-bit operation to avoid panicing
        // when bit_width == 0 (and thus shifting right 64-bits). This is preferred to branching
        // the bit_width == 0 case, since reading up-to 32-bits at a time is a hot code-path.
        let mut bits = (self.get_bits() >> u32::BITS) >> (u32::BITS - bit_width);

        while bit_width > self.num_bits_left() {
            bit_width -= self.num_bits_left();

            self.fetch_bits()?;

            // Unlike the first shift, bit_width is always > 0 here so this operation will never
            // shift by > 63 bits.
            bits |= self.get_bits() >> (u64::BITS - bit_width);
        }

        self.consume_bits(bit_width);

        Ok(bits as u32)
    }

    /// Reads up to 32-bits and interprets them as a signed two's complement integer or returns an
    /// error.
    #[inline(always)]
    fn read_bits_leq32_signed(&mut self, bit_width: u32) -> io::Result<i32> {
        let value = self.read_bits_leq32(bit_width)?;
        Ok(sign_extend_leq32_to_i32(value, bit_width))
    }

    /// Reads up to 64-bits and interprets them as a signed two's complement integer or returns an
    /// error.
    #[inline(always)]
    fn read_bits_leq64(&mut self, mut bit_width: u32) -> io::Result<u64> {
        debug_assert!(bit_width <= u64::BITS);

        // Hard-code the bit_width == 0 case as it's not possible to handle both the bit_width == 0
        // and bit_width == 64 cases branchlessly. This should be optimized out when bit_width is
        // known at compile time. Since it's generally rare to need to read up-to 64-bits at a time
        // (as oppopsed to 32-bits), this is an acceptable solution.
        if bit_width == 0 {
            Ok(0)
        }
        else {
            // Since bit_width is always > 0, this shift operation is always < 64, and will
            // therefore never panic.
            let mut bits = self.get_bits() >> (u64::BITS - bit_width);

            while bit_width > self.num_bits_left() {
                bit_width -= self.num_bits_left();

                self.fetch_bits()?;

                bits |= self.get_bits() >> (u64::BITS - bit_width);
            }

            // Shift in two parts to prevent panicing when bit_width == 64.
            self.consume_bits(bit_width - 1);
            self.consume_bits(1);

            Ok(bits)
        }
    }

    /// Reads up to 64-bits and interprets them as a signed two's complement integer or returns an
    /// error.
    #[inline(always)]
    fn read_bits_leq64_signed(&mut self, bit_width: u32) -> io::Result<i64> {
        let value = self.read_bits_leq64(bit_width)?;
        Ok(sign_extend_leq64_to_i64(value, bit_width))
    }

    /// Reads and returns a unary zeros encoded integer or an error.
    #[inline(always)]
    fn read_unary_zeros(&mut self) -> io::Result<u32> {
        let mut num = 0;

        loop {
            // Get the number of trailing zeros.
            let n_zeros = self.get_bits().leading_zeros();

            if n_zeros >= self.num_bits_left() {
                // If the number of zeros exceeds the number of bits left then all the remaining
                // bits were 0.
                num += self.num_bits_left();
                self.fetch_bits()?;
            }
            else {
                // Otherwise, a 1 bit was encountered after `n_zeros` 0 bits.
                num += n_zeros;

                // Since bits are shifted off the cache after they're consumed, for there to be a
                // 1 bit there must be atleast one extra available bit in the cache that can be
                // consumed after the 0 bits.
                self.consume_bits(n_zeros);
                self.consume_bits(1);

                // Done decoding.
                break;
            }
        }

        Ok(num)
    }

    /// Reads and returns a unary ones encoded integer or an error.
    #[inline(always)]
    fn read_unary_ones(&mut self) -> io::Result<u32> {
        // Note: This algorithm is identical to read_unary_zeros except flipped for 1s.
        let mut num = 0;

        loop {
            let n_ones = self.get_bits().leading_ones();

            if n_ones >= self.num_bits_left() {
                num += self.num_bits_left();
                self.fetch_bits()?;
            }
            else {
                num += n_ones;

                self.consume_bits(n_ones);
                self.consume_bits(1);

                break;
            }
        }

        Ok(num)
    }

    /// Reads a Huffman code from the `BitStream` using the provided `HuffmanTable` and returns the
    /// decoded value or an error.
    fn read_huffman<H: huffman::HuffmanEntry>(
        &mut self,
        table: &huffman::HuffmanTable<H>,
        _: u32,
    ) -> io::Result<(H::ValueType, u32)> {

        debug_assert!(!table.data.is_empty());

        let mut code_len = 0;
        let mut jmp_read_len = 0;

        let mut entry = H::root(table.n_init_bits as usize);

        while entry.is_jump() {
            // Consume bits from the last jump.
            self.consume_bits(jmp_read_len);

            // Update decoded code length.
            code_len += jmp_read_len;

            // The length of the next run of bits to read.
            jmp_read_len = entry.next_len();

            let addr = self.get_bits() >> (u64::BITS - jmp_read_len);

            // Jump!
            let jmp_offset = entry.jump_offset();

            entry = table.data[jmp_offset + addr as usize];

            // The bit cache cannot fully service next lookup. Try to use the remaining bits (addr)
            // as a prefix. If it points to a value entry that has a code length that's <= the
            // remaining number of bits, then no further reads are necessary.
            if self.num_bits_left() < jmp_read_len {

                if entry.is_value() && entry.code_len() <= self.num_bits_left() {
                    break;
                }

                // Fetch more bits without discarding the unconsumed bits.
                self.fetch_bits_partial()?;

                let addr = self.get_bits() >> (u64::BITS - jmp_read_len);

                entry = table.data[jmp_offset + addr as usize];
            }
        }

        // Consume the bits from the value entry.
        let entry_code_len = entry.code_len();

        self.consume_bits(entry_code_len);

        Ok((entry.into_value(), code_len + entry_code_len))
    }
}

/// `BitStreamLtr` reads bits from most-significant to least-significant from any source
/// that implements [`ReadBytes`].
///
/// Stated another way, if N-bits are read from a `BitReaderLtr` then bit 0, the first bit read,
/// is the most-significant bit, and bit N-1, the last bit read, is the least-significant.
pub struct BitStreamLtr<'a, B: ReadBytes> {
    reader: &'a mut B,
    bits: u64,
    n_bits_left: u32,
}

impl<'a, B: ReadBytes> BitStreamLtr<'a, B> {
    /// Instantiate a new `BitStreamLtr` with the given source.
    pub fn new(reader: &'a mut B) -> Self {
        BitStreamLtr {
            reader,
            bits: 0,
            n_bits_left: 0,
        }
    }
}

impl<'a, B: ReadBytes> private::FetchBitsLtr for BitStreamLtr<'a, B> {
    #[inline(always)]
    fn fetch_bits(&mut self) -> io::Result<()> {
        self.bits = u64::from(self.reader.read_u8()?) << 56;
        self.n_bits_left = u8::BITS;
        Ok(())
    }

    #[inline(always)]
    fn fetch_bits_partial(&mut self) -> io::Result<()> {
        self.bits |= u64::from(self.reader.read_u8()?) << (u64::BITS - self.n_bits_left);
        self.n_bits_left += u8::BITS;
        todo!()
    }

    #[inline(always)]
    fn get_bits(&self) -> u64 {
        self.bits
    }

    #[inline(always)]
    fn num_bits_left(&self) -> u32 {
        self.n_bits_left
    }

    #[inline(always)]
    fn consume_bits(&mut self, num: u32) {
        self.n_bits_left -= num;
        self.bits <<= num;
    }
}

impl<'a, B: ReadBytes> ReadBitsLtr for BitStreamLtr<'a, B> { }

/// `BitReaderLtr` reads bits from most-significant to least-significant from any `&[u8]`.
///
/// Stated another way, if N-bits are read from a `BitReaderLtr` then bit 0, the first bit read,
/// is the most-significant bit, and bit N-1, the last bit read, is the least-significant.
pub struct BitReaderLtr<'a> {
    buf: &'a [u8],
    bits: u64,
    n_bits_left: u32,
}

impl<'a> BitReaderLtr<'a> {
    /// Instantiate a new `BitReaderLtr` with the given buffer.
    pub fn new(buf: &'a [u8]) -> Self {
        BitReaderLtr {
            buf,
            bits: 0,
            n_bits_left: 0,
        }
    }
}

impl<'a> private::FetchBitsLtr for BitReaderLtr<'a> {
    fn fetch_bits_partial(&mut self) -> io::Result<()> {
        let mut buf = [0u8; std::mem::size_of::<u64>()];

        let read_len = min(self.buf.len(), (u64::BITS - self.n_bits_left) as usize >> 3);

        if read_len == 0 {
            return end_of_bitstream_error();
        }

        buf[..read_len].copy_from_slice(&self.buf[..read_len]);

        self.buf = &self.buf[read_len..];

        self.bits |= u64::from_be_bytes(buf) >> self.n_bits_left;
        self.n_bits_left += (read_len as u32) << 3;

        Ok(())
    }

    fn fetch_bits(&mut self) -> io::Result<()> {
        let mut buf = [0u8; std::mem::size_of::<u64>()];

        let read_len = min(self.buf.len(), std::mem::size_of::<u64>());

        if read_len == 0 {
            return end_of_bitstream_error();
        }

        buf[..read_len].copy_from_slice(&self.buf[..read_len]);

        self.buf = &self.buf[read_len..];

        self.bits = u64::from_be_bytes(buf);
        self.n_bits_left = (read_len as u32) << 3;

        Ok(())
    }

    #[inline(always)]
    fn get_bits(&self) -> u64 {
        self.bits
    }

    #[inline(always)]
    fn num_bits_left(&self) -> u32 {
        self.n_bits_left
    }

    #[inline(always)]
    fn consume_bits(&mut self, num: u32) {
        self.n_bits_left -= num;
        self.bits <<= num;
    }
}

impl<'a> ReadBitsLtr for BitReaderLtr<'a> { }

impl<'a> FiniteBitStream for BitReaderLtr<'a> {
    fn bits_left(&self) -> u64 {
        (8 * self.buf.len() as u64) + u64::from(self.n_bits_left)
    }
}

/// `ReadBitsRtl` reads bits from least-significant to most-significant.
pub trait ReadBitsRtl : private::FetchBitsRtl {
    /// Discards any saved bits and resets the `BitStream` to prepare it for a byte-aligned read.
    #[inline(always)]
    fn realign(&mut self) {
        let skip = self.num_bits_left() & 0x7;
        self.consume_bits(skip);
    }

    /// Ignores the specified number of bits from the stream or returns an error.
    #[inline(always)]
    fn ignore_bits(&mut self, mut num_bits: u32) -> io::Result<()> {
        if num_bits <= self.num_bits_left() {
            self.consume_bits(num_bits);
        }
        else {
            // Consume whole bit caches directly.
            while num_bits > self.num_bits_left() {
                num_bits -= self.num_bits_left();
                self.fetch_bits()?;
            }

            if num_bits > 0 {
                // Shift out in two parts to prevent panicing when num_bits == 64.
                self.consume_bits(num_bits - 1);
                self.consume_bits(1);
            }
        }

        Ok(())
    }

    /// Ignores one bit from the stream or returns an error.
    #[inline(always)]
    fn ignore_bit(&mut self) -> io::Result<()> {
        self.ignore_bits(1)
    }

    /// Read a single bit as a boolean value or returns an error.
    #[inline(always)]
    fn read_bit(&mut self) -> io::Result<bool> {
        if self.num_bits_left() < 1 {
            self.fetch_bits()?;
        }

        let bit = (self.get_bits() & 1) == 1;

        self.consume_bits(1);
        Ok(bit)
    }

    /// Reads up to 32-bits and interprets them as a signed two's complement integer or returns an
    /// error.
    #[inline(always)]
    fn read_bits_leq32(&mut self, bit_width: u32) -> io::Result<u32> {
        debug_assert!(bit_width <= u32::BITS);

        let mut bits = self.get_bits();
        let mut bits_needed = bit_width;

        while bits_needed > self.num_bits_left() {
            bits_needed -= self.num_bits_left();

            self.fetch_bits()?;

            bits |= self.get_bits() << (bit_width - bits_needed);
        }

        self.consume_bits(bits_needed);

        // Since bit_width is <= 32, this shift will never panic.
        let mask = !(!0 << bit_width);

        Ok((bits & mask) as u32)
    }

    /// Reads up to 32-bits and interprets them as a signed two's complement integer or returns an
    /// error.
    #[inline(always)]
    fn read_bits_leq32_signed(&mut self, bit_width: u32) -> io::Result<i32> {
        let value = self.read_bits_leq32(bit_width)?;
        Ok(sign_extend_leq32_to_i32(value, bit_width))
    }

    /// Reads up to 64-bits and interprets them as a signed two's complement integer or returns an
    /// error.
    #[inline(always)]
    fn read_bits_leq64(&mut self, bit_width: u32) -> io::Result<u64> {
        debug_assert!(bit_width <= u64::BITS);

        // Hard-code the bit_width == 0 case as it's not possible to handle both the bit_width == 0
        // and bit_width == 64 cases branchlessly. This should be optimized out when bit_width is
        // known at compile time. Since it's generally rare to need to read up-to 64-bits at a time
        // (as oppopsed to 32-bits), this is an acceptable solution.
        if bit_width == 0 {
            Ok(0)
        }
        else {
            let mut bits = self.get_bits();
            let mut bits_needed = bit_width;

            while bits_needed > self.num_bits_left() {
                bits_needed -= self.num_bits_left();

                self.fetch_bits()?;

                // Since bits_needed will always be > 0, this will never shift by > 63 bits if
                // bit_width == 64 and therefore will never panic.
                bits |= self.get_bits() << (bit_width - bits_needed);
            }

            // Shift in two parts to prevent panicing when bit_width == 64.
            self.consume_bits(bits_needed - 1);
            self.consume_bits(1);

            // Generate the mask in two parts to prevent panicing when bit_width == 64.
            let mask = !((!0 << bit_width - 1) << 1);

            Ok(bits & mask)
        }
    }

    /// Reads up to 64-bits and interprets them as a signed two's complement integer or returns an
    /// error.
    #[inline(always)]
    fn read_bits_leq64_signed(&mut self, bit_width: u32) -> io::Result<i64> {
        let value = self.read_bits_leq64(bit_width)?;
        Ok(sign_extend_leq64_to_i64(value, bit_width))
    }

    /// Reads and returns a unary zeros encoded integer or an error.
    #[inline(always)]
    fn read_unary_zeros(&mut self) -> io::Result<u32> {
        let mut num = 0;

        loop {
            // Get the number of trailing zeros.
            let n_zeros = self.get_bits().trailing_zeros();

            if n_zeros >= self.num_bits_left() {
                // If the number of zeros exceeds the number of bits left then all the remaining
                // bits were 0.
                num += self.num_bits_left();
                self.fetch_bits()?;
            }
            else {
                // Otherwise, a 1 bit was encountered after `n_zeros` 0 bits.
                num += n_zeros;

                // Since bits are shifted off the cache after they're consumed, for there to be a
                // 1 bit there must be atleast one extra available bit in the cache that can be
                // consumed after the 0 bits.
                self.consume_bits(n_zeros);
                self.consume_bits(1);

                // Done decoding.
                break;
            }
        }

        Ok(num)
    }

    /// Reads and returns a unary ones encoded integer or an error.
    #[inline(always)]
    fn read_unary_ones(&mut self) -> io::Result<u32> {
        // Note: This algorithm is identical to read_unary_zeros except flipped for 1s.
        let mut num = 0;

        loop {
            let n_ones = self.get_bits().trailing_ones();

            if n_ones >= self.num_bits_left() {
                num += self.num_bits_left();
                self.fetch_bits()?;
            }
            else {
                num += n_ones;

                self.consume_bits(n_ones);
                self.consume_bits(1);

                break;
            }
        }

        Ok(num)
    }

    /// Reads a Huffman code from the `BitStream` using the provided `HuffmanTable` and returns the
    /// decoded value or an error.
    fn read_huffman<H: huffman::HuffmanEntry>(
        &mut self,
        table: &huffman::HuffmanTable<H>,
        _: u32,
    ) -> io::Result<(H::ValueType, u32)> {

        debug_assert!(!table.data.is_empty());

        let mut code_len = 0;
        let mut jmp_read_len = 0;

        let mut entry = H::root(table.n_init_bits as usize);

        while entry.is_jump() {
            // Consume bits from the last jump.
            self.consume_bits(jmp_read_len);

            // Update decoded code length.
            code_len += jmp_read_len;

            // The length of the next run of bits to read.
            jmp_read_len = entry.next_len();

            let addr = self.get_bits() & ((1 << jmp_read_len) - 1);

            // Jump!
            let jmp_offset = entry.jump_offset();

            entry = table.data[jmp_offset + addr as usize];

            // The bit cache cannot fully service next lookup. Try to use the remaining bits (addr)
            // as a prefix. If it points to a value entry that has a code length that's <= the
            // remaining number of bits, then no further reads are necessary.
            if self.num_bits_left() < jmp_read_len {

                if entry.is_value() && entry.code_len() <= self.num_bits_left() {
                    break;
                }

                // Fetch more bits without discarding the unconsumed bits.
                self.fetch_bits_partial()?;

                let addr = self.get_bits() & ((1 << jmp_read_len) - 1);

                entry = table.data[jmp_offset + addr as usize];
            }
        }

        // Consume the bits from the value entry.
        let entry_code_len = entry.code_len();

        self.consume_bits(entry_code_len);

        Ok((entry.into_value(), code_len + entry_code_len))
    }
}

/// `BitStreamRtl` reads bits from least-significant to most-significant from any source
/// that implements [`ReadBytes`].
///
/// Stated another way, if N-bits are read from a `BitReaderLtr` then bit 0, the first bit read,
/// is the least-significant bit, and bit N-1, the last bit read, is the most-significant.
pub struct BitStreamRtl<'a, B: ReadBytes> {
    reader: &'a mut B,
    bits: u64,
    n_bits_left: u32,
}

impl<'a, B: ReadBytes> BitStreamRtl<'a, B> {
    /// Instantiate a new `BitStreamRtl` with the given buffer.
    pub fn new(reader: &'a mut B) -> Self {
        BitStreamRtl {
            reader,
            bits: 0,
            n_bits_left: 0,
        }
    }
}

impl<'a, B: ReadBytes> private::FetchBitsRtl for BitStreamRtl<'a, B> {
    #[inline(always)]
    fn fetch_bits(&mut self) -> io::Result<()> {
        self.bits = u64::from(self.reader.read_u8()?);
        self.n_bits_left = u8::BITS;
        Ok(())
    }

    #[inline(always)]
    fn fetch_bits_partial(&mut self) -> io::Result<()> {
        self.bits |= u64::from(self.reader.read_u8()?) << self.n_bits_left;
        self.n_bits_left += u8::BITS;
        todo!()
    }

    #[inline(always)]
    fn get_bits(&self) -> u64 {
        self.bits
    }

    #[inline(always)]
    fn num_bits_left(&self) -> u32 {
        self.n_bits_left
    }

    #[inline(always)]
    fn consume_bits(&mut self, num: u32) {
        self.n_bits_left -= num;
        self.bits >>= num;
    }
}

impl<'a, B: ReadBytes> ReadBitsRtl for BitStreamRtl<'a, B> { }

/// `BitReaderRtl` reads bits from least-significant to most-significant from any `&[u8]`.
///
/// Stated another way, if N-bits are read from a `BitReaderRtl` then bit 0, the first bit read,
/// is the least-significant bit, and bit N-1, the last bit read, is the most-significant.
pub struct BitReaderRtl<'a> {
    buf: &'a [u8],
    bits: u64,
    n_bits_left: u32,
}

impl<'a> BitReaderRtl<'a> {
    /// Instantiate a new `BitReaderRtl` with the given buffer.
    pub fn new(buf: &'a [u8]) -> Self {
        BitReaderRtl {
            buf,
            bits: 0,
            n_bits_left: 0,
        }
    }
}

impl<'a> private::FetchBitsRtl for BitReaderRtl<'a> {
    fn fetch_bits_partial(&mut self) -> io::Result<()> {
        let mut buf = [0u8; std::mem::size_of::<u64>()];

        let read_len = min(self.buf.len(), (u64::BITS - self.n_bits_left) as usize >> 3);

        if read_len == 0 {
            return end_of_bitstream_error();
        }

        buf[..read_len].copy_from_slice(&self.buf[..read_len]);

        self.buf = &self.buf[read_len..];

        self.bits |= u64::from_le_bytes(buf) << self.n_bits_left;
        self.n_bits_left += (read_len as u32) << 3;

        Ok(())
    }

    fn fetch_bits(&mut self) -> io::Result<()> {
        let mut buf = [0u8; std::mem::size_of::<u64>()];

        let read_len = min(self.buf.len(), std::mem::size_of::<u64>());

        if read_len == 0 {
            return end_of_bitstream_error();
        }

        buf[..read_len].copy_from_slice(&self.buf[..read_len]);

        self.buf = &self.buf[read_len..];

        self.bits = u64::from_le_bytes(buf);
        self.n_bits_left = (read_len as u32) << 3;

        Ok(())
    }

    #[inline(always)]
    fn get_bits(&self) -> u64 {
        self.bits
    }

    #[inline(always)]
    fn num_bits_left(&self) -> u32 {
        self.n_bits_left
    }

    #[inline(always)]
    fn consume_bits(&mut self, num: u32) {
        self.n_bits_left -= num;
        self.bits >>= num;
    }
}

impl<'a> ReadBitsRtl for BitReaderRtl<'a> { }

impl<'a> FiniteBitStream for BitReaderRtl<'a> {
    fn bits_left(&self) -> u64 {
        (8 * self.buf.len() as u64) + u64::from(self.n_bits_left)
    }
}

#[cfg(test)]
mod tests {
    use super::{BitReaderLtr, ReadBitsLtr};
    use super::{BitReaderRtl, ReadBitsRtl};
    use super::huffman::{HuffmanTable, H8};

    #[test]
    fn verify_bitstreamltr_ignore_bits() {
        let mut bs = BitReaderLtr::new(
            &[
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xc0, 0x10, 0x00, 0x01, 0x00, 0x00, 0x00, 0x0a,
            ]
        );

        assert_eq!(bs.read_bit().unwrap(), true);

        bs.ignore_bits(128).unwrap();

        assert_eq!(bs.read_bit().unwrap(), true);
        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), false);

        bs.ignore_bits(7).unwrap();

        assert_eq!(bs.read_bit().unwrap(), true);

        bs.ignore_bits(19).unwrap();

        assert_eq!(bs.read_bit().unwrap(), true);

        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), false);

        bs.ignore_bits(24).unwrap();

        assert_eq!(bs.read_bit().unwrap(), true);
        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), true);
        assert_eq!(bs.read_bit().unwrap(), false);

        // Lower limit test.
        let mut bs = BitReaderLtr::new(&[ 0x00 ]);

        assert!(bs.ignore_bits(0).is_ok());

        let mut bs = BitReaderLtr::new(&[]);

        assert!(bs.ignore_bits(0).is_ok());
        assert!(bs.ignore_bits(1).is_err());

        // Upper limit test.
        let mut bs = BitReaderLtr::new(
            &[
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            ]
        );

        assert!(bs.ignore_bits(64).is_ok());
        assert!(bs.ignore_bits(64).is_ok());
        assert!(bs.ignore_bits(32).is_ok());
        assert!(bs.ignore_bits(32).is_ok());
        assert!(bs.ignore_bits(64).is_ok());
    }

    #[test]
    fn verify_bitstreamltr_read_bit() {
        // General tests.
        let mut bs = BitReaderLtr::new(&[0b1010_1010]);

        assert_eq!(bs.read_bit().unwrap(), true);
        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), true);
        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), true);
        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), true);
        assert_eq!(bs.read_bit().unwrap(), false);

        // Error test.
        let mut bs = BitReaderLtr::new(&[]);

        assert!(bs.read_bit().is_err());
    }

    #[test]
    fn verify_bitstreamltr_read_bits_leq32() {
        // General tests.
        let mut bs = BitReaderLtr::new(
            &[
                0b1010_0101, 0b0111_1110, 0b1101_0011
            ]
        );

        assert_eq!(bs.read_bits_leq32( 4).unwrap(), 0b0000_0000_0000_1010);
        assert_eq!(bs.read_bits_leq32( 4).unwrap(), 0b0000_0000_0000_0101);
        assert_eq!(bs.read_bits_leq32(13).unwrap(), 0b0000_1111_1101_1010);
        assert_eq!(bs.read_bits_leq32( 3).unwrap(), 0b0000_0000_0000_0011);

        // Lower limit test.
        let mut bs = BitReaderLtr::new(&[ 0xff, 0xff, 0xff, 0xff ]);

        assert_eq!(bs.read_bits_leq32(0).unwrap(), 0);

        // Upper limit test.
        let mut bs = BitReaderLtr::new(&[ 0xff, 0xff, 0xff, 0xff, 0x01 ]);

        assert_eq!(bs.read_bits_leq32(32).unwrap(), u32::MAX);
        assert_eq!(bs.read_bits_leq32( 8).unwrap(), 0x01);

        // Cache fetch test.
        let mut bs = BitReaderLtr::new(&[ 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01 ]);

        assert_eq!(bs.read_bits_leq32(32).unwrap(), u32::MAX);
        assert_eq!(bs.read_bits_leq32(32).unwrap(), u32::MAX);
        assert_eq!(bs.read_bits_leq32( 8).unwrap(), 0x01);

        // Test error cases.
        let mut bs = BitReaderLtr::new(&[0xff]);

        assert!(bs.read_bits_leq32(9).is_err());
    }

    #[test]
    fn verify_bitstreamltr_read_bits_leq64() {
        // General tests.
        let mut bs = BitReaderLtr::new(
            &[
                0x99, 0xaa, 0x55, 0xff, 0xff, 0x55, 0xaa, 0x99,
                0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88
            ]
        );

        assert_eq!(bs.read_bits_leq64(40).unwrap(), 0x99aa55ffff);
        assert_eq!(bs.read_bits_leq64( 4).unwrap(), 0x05);
        assert_eq!(bs.read_bits_leq64( 4).unwrap(), 0x05);
        assert_eq!(bs.read_bits_leq64(16).unwrap(), 0xaa99);
        assert_eq!(bs.read_bits_leq64(64).unwrap(), 0x1122334455667788);

        // Lower limit test.
        let mut bs = BitReaderLtr::new(&[ 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff ]);

        assert_eq!(bs.read_bits_leq64(0).unwrap(), 0);

        // Upper limit test.
        let mut bs = BitReaderLtr::new(&[ 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01 ]);

        assert_eq!(bs.read_bits_leq64(64).unwrap(), u64::MAX);
        assert_eq!(bs.read_bits_leq64( 8).unwrap(), 0x01);

        // Test error cases.
        let mut bs = BitReaderLtr::new(&[0xff]);

        assert!(bs.read_bits_leq64(9).is_err());
    }

    #[test]
    fn verify_bitstreamltr_read_unary_zeros() {
        // General tests
        let mut bs = BitReaderLtr::new(
            &[
                0b0000_0001, 0b0001_0000, 0b0000_0000, 0b1000_0000, 0b1111_1011
            ]
        );

        assert_eq!(bs.read_unary_zeros().unwrap(),  7);
        assert_eq!(bs.read_unary_zeros().unwrap(),  3);
        assert_eq!(bs.read_unary_zeros().unwrap(), 12);
        assert_eq!(bs.read_unary_zeros().unwrap(),  7);
        assert_eq!(bs.read_unary_zeros().unwrap(),  0);
        assert_eq!(bs.read_unary_zeros().unwrap(),  0);
        assert_eq!(bs.read_unary_zeros().unwrap(),  0);
        assert_eq!(bs.read_unary_zeros().unwrap(),  0);
        assert_eq!(bs.read_unary_zeros().unwrap(),  1);
        assert_eq!(bs.read_unary_zeros().unwrap(),  0);

        // Upper limit test
        let mut bs = BitReaderLtr::new(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01]);

        assert_eq!(bs.read_unary_zeros().unwrap(), 63);

        // Lower limit test
        let mut bs = BitReaderLtr::new(&[0x80]);

        assert_eq!(bs.read_unary_zeros().unwrap(), 0);

        // Error test.
        let mut bs = BitReaderLtr::new(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        assert!(bs.read_unary_zeros().is_err());
    }

    #[test]
    fn verify_bitstreamltr_read_unary_ones() {
        // General tests
        let mut bs = BitReaderLtr::new(
            &[
                0b1111_1110, 0b1110_1111, 0b1111_1111, 0b0111_1111, 0b0000_0100
            ]
        );

        assert_eq!(bs.read_unary_ones().unwrap(),  7);
        assert_eq!(bs.read_unary_ones().unwrap(),  3);
        assert_eq!(bs.read_unary_ones().unwrap(), 12);
        assert_eq!(bs.read_unary_ones().unwrap(),  7);
        assert_eq!(bs.read_unary_ones().unwrap(),  0);
        assert_eq!(bs.read_unary_ones().unwrap(),  0);
        assert_eq!(bs.read_unary_ones().unwrap(),  0);
        assert_eq!(bs.read_unary_ones().unwrap(),  0);
        assert_eq!(bs.read_unary_ones().unwrap(),  1);
        assert_eq!(bs.read_unary_ones().unwrap(),  0);

        // Upper limit test
        let mut bs = BitReaderLtr::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe]);

        assert_eq!(bs.read_unary_ones().unwrap(), 63);

        // Lower limit test
        let mut bs = BitReaderLtr::new(&[0x7f]);

        assert_eq!(bs.read_unary_ones().unwrap(), 0);

        // Error test.
        let mut bs = BitReaderLtr::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);

        assert!(bs.read_unary_ones().is_err());
    }

        #[test]
    fn verify_bitstreamltr_read_huffman() {
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

        let mut bs = BitReaderLtr::new(
            &[
                0b010_00000, 0b0_00001_00, 0b0001_001_0
            ]
        );

        assert_eq!(bs.read_huffman(&TABLE, 0).unwrap().0, 0x1 );
        assert_eq!(bs.read_huffman(&TABLE, 0).unwrap().0, 0x22);
        assert_eq!(bs.read_huffman(&TABLE, 0).unwrap().0, 0x12);
        assert_eq!(bs.read_huffman(&TABLE, 0).unwrap().0, 0x2 );
        assert_eq!(bs.read_huffman(&TABLE, 0).unwrap().0, 0x11);
    }

    // BitStreamRtl

    #[test]
    fn verify_bitstreamrtl_ignore_bits() {
        let mut bs = BitReaderRtl::new(
            &[
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0x02, 0x08, 0x00, 0x80, 0x00, 0x00, 0x00, 0x50,
            ]
        );

        assert_eq!(bs.read_bit().unwrap(), true);

        bs.ignore_bits(128).unwrap();

        assert_eq!(bs.read_bit().unwrap(), true);
        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), false);

        bs.ignore_bits(7).unwrap();

        assert_eq!(bs.read_bit().unwrap(), true);

        bs.ignore_bits(19).unwrap();

        assert_eq!(bs.read_bit().unwrap(), true);

        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), false);

        bs.ignore_bits(24).unwrap();

        assert_eq!(bs.read_bit().unwrap(), true);
        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), true);
        assert_eq!(bs.read_bit().unwrap(), false);

        // Lower limit test.
        let mut bs = BitReaderRtl::new(&[ 0x00 ]);

        assert!(bs.ignore_bits(0).is_ok());

        let mut bs = BitReaderRtl::new(&[]);

        assert!(bs.ignore_bits(0).is_ok());
        assert!(bs.ignore_bits(1).is_err());

        // Upper limit test.
        let mut bs = BitReaderRtl::new(
            &[
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            ]
        );

        assert!(bs.ignore_bits(64).is_ok());
        assert!(bs.ignore_bits(64).is_ok());
        assert!(bs.ignore_bits(32).is_ok());
        assert!(bs.ignore_bits(32).is_ok());
        assert!(bs.ignore_bits(64).is_ok());
    }

    #[test]
    fn verify_bitstreamrtl_read_bit() {
        // General tests.
        let mut bs = BitReaderRtl::new(&[0b1010_1010]);

        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), true);
        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), true);
        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), true);
        assert_eq!(bs.read_bit().unwrap(), false);
        assert_eq!(bs.read_bit().unwrap(), true);

        // Error test.
        let mut bs = BitReaderRtl::new(&[]);

        assert!(bs.read_bit().is_err());
    }

    #[test]
    fn verify_bitstreamrtl_read_bits_leq32() {
        // General tests.
        let mut bs = BitReaderRtl::new(
            &[
                0b1010_0101, 0b0111_1110, 0b1101_0011
            ]
        );

        assert_eq!(bs.read_bits_leq32( 4).unwrap(), 0b0000_0000_0000_0101);
        assert_eq!(bs.read_bits_leq32( 4).unwrap(), 0b0000_0000_0000_1010);
        assert_eq!(bs.read_bits_leq32(13).unwrap(), 0b0001_0011_0111_1110);
        assert_eq!(bs.read_bits_leq32( 3).unwrap(), 0b0000_0000_0000_0110);

        // Lower limit test.
        let mut bs = BitReaderRtl::new(&[ 0xff, 0xff, 0xff, 0xff ]);

        assert_eq!(bs.read_bits_leq32(0).unwrap(), 0);

        // Upper limit test.
        let mut bs = BitReaderRtl::new(&[ 0xff, 0xff, 0xff, 0xff, 0x01 ]);

        assert_eq!(bs.read_bits_leq32(32).unwrap(), u32::MAX);
        assert_eq!(bs.read_bits_leq32( 8).unwrap(), 0x01);

        // Cache fetch test.
        let mut bs = BitReaderRtl::new(&[ 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01 ]);

        assert_eq!(bs.read_bits_leq32(32).unwrap(), u32::MAX);
        assert_eq!(bs.read_bits_leq32(32).unwrap(), u32::MAX);
        assert_eq!(bs.read_bits_leq32( 8).unwrap(), 0x01);

        // Test error cases.
        let mut bs = BitReaderRtl::new(&[0xff]);

        assert!(bs.read_bits_leq32(9).is_err());
    }

    #[test]
    fn verify_bitstreamrtl_read_bits_leq64() {
        // General tests.
        let mut bs = BitReaderRtl::new(
            &[
                0x99, 0xaa, 0x55, 0xff, 0xff, 0x55, 0xaa, 0x99,
                0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
                0x00, 0x11, 0x22, 0x33, 0x00, 0x11, 0x22, 0x33,
                0x44, 0x55, 0x66, 0x77,
            ]
        );

        assert_eq!(bs.read_bits_leq64(40).unwrap(), 0xffff55aa99);
        assert_eq!(bs.read_bits_leq64( 4).unwrap(), 0x05);
        assert_eq!(bs.read_bits_leq64( 4).unwrap(), 0x05);
        assert_eq!(bs.read_bits_leq64(16).unwrap(), 0x99aa);
        assert_eq!(bs.read_bits_leq64(64).unwrap(), 0x8877665544332211);
        assert_eq!(bs.read_bits_leq64(32).unwrap(), 0x33221100);
        assert_eq!(bs.read_bits_leq64(64).unwrap(), 0x7766554433221100);

        // Lower limit test.
        let mut bs = BitReaderRtl::new(&[ 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff ]);

        assert_eq!(bs.read_bits_leq64(0).unwrap(), 0);

        // Upper limit test.
        let mut bs = BitReaderRtl::new(&[ 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01 ]);

        assert_eq!(bs.read_bits_leq64(64).unwrap(), u64::MAX);
        assert_eq!(bs.read_bits_leq64( 8).unwrap(), 0x01);

        // Test error cases.
        let mut bs = BitReaderRtl::new(&[0xff]);

        assert!(bs.read_bits_leq64(9).is_err());
    }


    #[test]
    fn verify_bitstreamrtl_read_unary_zeros() {
        // General tests
        let mut bs = BitReaderRtl::new(
            &[
                0b1000_0000, 0b0000_1000, 0b0000_0000, 0b0000_0001, 0b1101_1111
            ]
        );

        assert_eq!(bs.read_unary_zeros().unwrap(),  7);
        assert_eq!(bs.read_unary_zeros().unwrap(),  3);
        assert_eq!(bs.read_unary_zeros().unwrap(), 12);
        assert_eq!(bs.read_unary_zeros().unwrap(),  7);
        assert_eq!(bs.read_unary_zeros().unwrap(),  0);
        assert_eq!(bs.read_unary_zeros().unwrap(),  0);
        assert_eq!(bs.read_unary_zeros().unwrap(),  0);
        assert_eq!(bs.read_unary_zeros().unwrap(),  0);
        assert_eq!(bs.read_unary_zeros().unwrap(),  1);
        assert_eq!(bs.read_unary_zeros().unwrap(),  0);

        // Upper limit test
        let mut bs = BitReaderRtl::new(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80]);

        assert_eq!(bs.read_unary_zeros().unwrap(), 63);

        // Lower limit test
        let mut bs = BitReaderRtl::new(&[0x01]);

        assert_eq!(bs.read_unary_zeros().unwrap(), 0);

        // Error test.
        let mut bs = BitReaderRtl::new(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        assert!(bs.read_unary_zeros().is_err());
    }

    #[test]
    fn verify_bitstreamrtl_read_unary_ones() {
        // General tests
        let mut bs = BitReaderRtl::new(
            &[
                0b0111_1111, 0b1111_0111, 0b1111_1111, 0b1111_1110, 0b0010_0000
            ]
        );

        assert_eq!(bs.read_unary_ones().unwrap(),  7);
        assert_eq!(bs.read_unary_ones().unwrap(),  3);
        assert_eq!(bs.read_unary_ones().unwrap(), 12);
        assert_eq!(bs.read_unary_ones().unwrap(),  7);
        assert_eq!(bs.read_unary_ones().unwrap(),  0);
        assert_eq!(bs.read_unary_ones().unwrap(),  0);
        assert_eq!(bs.read_unary_ones().unwrap(),  0);
        assert_eq!(bs.read_unary_ones().unwrap(),  0);
        assert_eq!(bs.read_unary_ones().unwrap(),  1);
        assert_eq!(bs.read_unary_ones().unwrap(),  0);

        // Upper limit test
        let mut bs = BitReaderRtl::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]);

        assert_eq!(bs.read_unary_ones().unwrap(), 63);

        // Lower limit test
        let mut bs = BitReaderRtl::new(&[0xfe]);

        assert_eq!(bs.read_unary_ones().unwrap(), 0);

        // Error test.
        let mut bs = BitReaderRtl::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);

        assert!(bs.read_unary_ones().is_err());
    }

    #[test]
    fn verify_bitstreamtrl_read_huffman() {
        // A simple Huffman table.
        const TABLE: HuffmanTable<H8> = HuffmanTable {
            data: &[
                // [LSb] 0b ... (0)
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

                // [LSb] 0b0000 ... (16)
                val8!(0x22, 2),    // 0b00
                val8!(0x2, 2),    // 0b01
                val8!(0x12, 1),    // 0b10
                val8!(0x12, 1),    // 0b11

                // [LSb] 0b0001 ... (20)
                val8!(0x21, 1),    // 0b0
                val8!(0x20, 1),    // 0b1
            ],
            n_init_bits: 4,
            n_table_bits: 8,
        };

        let mut bs = BitReaderRtl::new(
            &[
                0b01_000000, 0b00000_100, 0b000100_1_0
            ]
        );

        assert_eq!(bs.read_huffman(&TABLE, 0).unwrap().0, 0x22);
        assert_eq!(bs.read_huffman(&TABLE, 0).unwrap().0, 0x20);
        assert_eq!(bs.read_huffman(&TABLE, 0).unwrap().0, 0x22);
        assert_eq!(bs.read_huffman(&TABLE, 0).unwrap().0, 0x0 );
        assert_eq!(bs.read_huffman(&TABLE, 0).unwrap().0, 0x1 );
    }

}