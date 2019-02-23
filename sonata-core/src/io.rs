// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This library is free software; you can redistribute it and/or
// modify it under the terms of the GNU Lesser General Public
// License as published by the Free Software Foundation; either
// version 2.1 of the License, or (at your option) any later version.
//
// This library is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
// Lesser General Public License for more details.
//
// You should have received a copy of the GNU Lesser General Public
// License along with this library; if not, write to the Free Software
// Foundation, Inc., 51 Franklin Street, Fifth Floor, Boston, MA  02110-1301 USA

use std::io;
use std::io::Read;
use std::cmp;
use super::checksum::Checksum;

/// Sign extends an arbitrary, 8-bit or less, signed two's complement integer stored within an u8
/// to a full width i8.
#[inline(always)]
pub fn sign_extend_leq8_to_i8(value: u8, width: u32) -> i8 {
    // Rust uses an arithmetic shift right (the original sign bit is repeatedly shifted on) for
    // signed integer types. Therefore, shift the value to the right-hand side of the integer,
    // then shift it back to extend the sign bit.
    ((value << (8 - width)) as i8) >> (8 - width)
}

/// Sign extends an arbitrary, 16-bit or less, signed two's complement integer stored within an u16
/// to a full width i16.
#[inline(always)]
pub fn sign_extend_leq16_to_i16(value: u16, width: u32) -> i16 {
    ((value << (16 - width) )as i16) >> (16 - width)
}

/// Sign extends an arbitrary, 32-bit or less, signed two's complement integer stored within an u32
/// to a full width i32.
#[inline(always)]
pub fn sign_extend_leq32_to_i32(value: u32, width: u32) -> i32 {
    ((value << (32 - width)) as i32) >> (32 - width)
}

/// Sign extends an arbitrary, 64-bit or less, signed two's complement integer stored within an u64
/// to a full width i64.
#[inline(always)]
pub fn sign_extend_leq64_to_i64(value: u64, width: u32) -> i64 {
    ((value << (64 - width)) as i64) >> (64 - width)
}

/// Masks the bit at the specified bit index.
#[inline(always)]
fn mask_at(idx: u32) -> u8 {
    debug_assert!(idx <= 7);
    1 << idx
}
/// Masks all bits with an index greater than or equal to idx.
#[inline(always)]
pub fn mask_upper_eq(idx: u32) -> u8 {
    debug_assert!(idx <= 7);
    !((1 << idx) - 1)
}

#[inline(always)]
pub fn mask_upper(idx: u32) -> u8 {
    debug_assert!(idx <= 7);
    !((1 << idx) - 1) ^ (1 << idx)
}

/// Masks all bits with an index less than or equal to idx.
#[inline(always)]
pub fn mask_lower_eq(idx: u32) -> u8 {
    debug_assert!(idx <= 7);
    ((1 << idx) - 1) ^ (1 << idx)
}

#[inline(always)]
pub fn mask_lower(idx: u32) -> u8 {
    debug_assert!(idx <= 7);
    ((1 << idx) - 1)
}

/// Masks out all bits in positions less than upper, but greater than or equal to lower
/// (upper < bit <= lower)
#[inline(always)]
pub fn mask_range(upper: u32, lower: u32) -> u8 {
    debug_assert!(upper <= 8);
    debug_assert!(lower <= 8);
    (((0xff as u32) << upper) ^ ((0xff as u32) << lower)) as u8
}

/// A `MediaSource` is a composite trait of `std::io::Read` and `std::io::Seek`. Seeking is an optional capability and 
/// support for it can be checked using the provided method.
pub trait MediaSource: io::Read + io::Seek {
    /// Returns if the source is seekable.
    fn is_seekable(&self) -> bool;
}

impl MediaSource for std::fs::File {
    fn is_seekable(&self) -> bool {
        true
    }
}

impl<T: std::convert::AsRef<[u8]>> MediaSource for io::Cursor<T> {
    fn is_seekable(&self) -> bool {
        true
    }
}

/// A `MediaSourceStream` is the common reader type for Sonata. `MediaSourceStream` uses type erasure to mask the 
/// inner reader from the consumer. Allowing any typical source to be used. Dynamic dispatch overhead is reduced by 
/// buffering the inner reader.
pub struct MediaSourceStream {
    /// The source reader.
    inner: Box<dyn MediaSource>,

    /// The read-ahead buffer filled from the inner reader.
    buf: Box<[u8]>,

    /// The index of the next readable byte in buf.
    pos: usize,

    /// The total number of valid bytes in buf.
    byte_length: usize,
}

impl MediaSourceStream {

    pub fn new(source: Box<dyn MediaSource>) -> Self {
        MediaSourceStream {
            inner: source,
            buf: vec![0u8; 32 * 1024].into_boxed_slice(),
            pos: 0, 
            byte_length: 0,
        }
    }

    fn fetch_buffer(&mut self) -> io::Result<&[u8]> {
        if self.pos >= self.byte_length {
            self.byte_length = self.inner.read(&mut self.buf)?;
            self.pos = 0;
        }
        Ok(&self.buf[self.pos..self.byte_length])
    }

    fn fetch_buffer_or_eof(&mut self) -> io::Result<()> {
        let buffer = self.fetch_buffer()?;

        // The returned buffer will have a length of 0 when EoF is reached. Return an
        // UnexpectedEof in this case since the caller is responsible for ensuring reading past the
        // end of the stream does not occur when using the Bytestream interface.
        if buffer.is_empty() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Reached end of stream."));
        }

        Ok(())
    }

}

impl io::Read for MediaSourceStream { 
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // If there is no buffered data, and the read is larger than the read-ahead buffer size,
        // then read directly from the underlying reader.
        if self.pos == self.byte_length && buf.len() >= self.buf.len() {
            return self.inner.read(buf);
        }

        // Fetch the latest read-ahead buffer, and read bytes it.
        let bytes_read = self.fetch_buffer()?.read(buf)?;

        // Advance the read-ahead buffer.
        self.pos += bytes_read;

        Ok(bytes_read)
    }
}

impl io::Seek for MediaSourceStream {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl MediaSource for MediaSourceStream {
    fn is_seekable(&self) -> bool {
        self.inner.is_seekable()
    }
} 

/// A `Bytestream` provides functions to read bytes and interpret them as little- or big-endian
/// unsigned integers of varying widths.
pub trait Bytestream {

    /// Reads a single byte from the stream and returns it or an error.
    fn read_byte(&mut self) -> io::Result<u8>;

    // Reads two bytes from the stream and returns them in read-order or an error.
    fn read_double_bytes(&mut self) -> io::Result<[u8; 2]>;

    // Reads three bytes from the stream and returns them in read-order or an error.
    fn read_triple_bytes(&mut self) -> io::Result<[u8; 3]>;

    // Reads four bytes from the stream and returns them in read-order or an error.
    fn read_quad_bytes(&mut self) -> io::Result<[u8; 4]>;

    // Reads exactly the number of bytes required to fill be provided buffer or returns an error.
    fn read_buf_bytes(&mut self, buf: &mut [u8]) -> io::Result<()>;

    /// Reads a single unsigned byte from the stream and returns it or an error.
    #[inline(always)]
    fn read_u8(&mut self) -> io::Result<u8> {
        self.read_byte()
    }

    /// Reads two bytes from the stream and interprets them as an unsigned 16-bit little-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.read_double_bytes()?))
    }

    /// Reads two bytes from the stream and interprets them as an unsigned 16-bit big-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_be_u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_be_bytes(self.read_double_bytes()?))
    }

    /// Reads three bytes from the stream and interprets them as an unsigned 24-bit little-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_u24(&mut self) -> io::Result<u32> {
        let mut b = [0u8; 4];
        b[0..3].clone_from_slice(&self.read_triple_bytes()?);
        Ok(u32::from_le_bytes(b))
    }

    /// Reads three bytes from the stream and interprets them as an unsigned 24-bit big-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_be_u24(&mut self) -> io::Result<u32> {
        let mut b = [0u8; 4];
        b[0..3].clone_from_slice(&self.read_triple_bytes()?);
        Ok(u32::from_be_bytes(b) >> 8)
    }

    /// Reads four bytes from the stream and interprets them as an unsigned 32-bit little-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.read_quad_bytes()?))
    }

    /// Reads four bytes from the stream and interprets them as an unsigned 32-bit big-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_be_u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_be_bytes(self.read_quad_bytes()?))
    }

    /// Reads eight bytes from the stream and interprets them as an unsigned 64-bit little-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_u64(&mut self) -> io::Result<u64> {
        let mut b = [0u8; 8];
        self.read_buf_bytes(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    /// Reads eight bytes from the stream and interprets them as an unsigned 64-bit big-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_be_u64(&mut self) -> io::Result<u64> {
        let mut b = [0u8; 8];
        self.read_buf_bytes(&mut b)?;
        Ok(u64::from_be_bytes(b))
    }

    /// Ignores the specified number of bytes from the stream or returns an error.
    fn ignore_bytes(&mut self, count: usize) -> io::Result<()>;
}

impl Bytestream for MediaSourceStream {

    #[inline(always)]
    fn read_byte(&mut self) -> io::Result<u8> {
        // This function, read_u8, is inlined for performance. To reduce code bloat, place the
        // read-ahead buffer replenishment in a seperate function. Call overhead will be negligible
        // compared to the actual underlying read.
        if self.pos >= self.byte_length {
            self.fetch_buffer_or_eof()?;
        }

        let byte = unsafe { *self.buf.get_unchecked(self.pos) };
        self.pos += 1;


        Ok(byte)
    }

    // Reads two bytes from the stream and returns them in read-order or an error.
    #[inline(always)]
    fn read_double_bytes(&mut self) -> io::Result<[u8; 2]> {
        let mut double_byte = [0u8; 2];

        // If the buffer has two bytes available, copy directly from it and skip any safety or
        // buffering checks.
        if self.pos + 2 < self.byte_length {
            unsafe { *double_byte.get_unchecked_mut(0) = *self.buf.get_unchecked(self.pos) }
            self.pos += 1;
            unsafe { *double_byte.get_unchecked_mut(1) = *self.buf.get_unchecked(self.pos) }
            self.pos += 1;
        }
        // If the by buffer does not have two bytes available, copy one byte at a time from the
        // buffer, checking if it needs to be replenished.
        else {
            for i in 0..2 {
                if self.pos >= self.byte_length {
                    self.fetch_buffer_or_eof()?;
                }
                unsafe { *double_byte.get_unchecked_mut(i) = *self.buf.get_unchecked(self.pos) }
                self.pos += 1;
            }
        }

        return Ok(double_byte);
    }

    // Reads three bytes from the stream and returns them in read-order or an error.
    fn read_triple_bytes(&mut self) -> io::Result<[u8; 3]> {
        let mut triple_byte = [0u8; 3];

        // If the buffer has three bytes available, copy directly from it and skip any safety or
        // buffering checks.
        if self.pos + 3 < self.byte_length {
            unsafe { *triple_byte.get_unchecked_mut(0) = *self.buf.get_unchecked(self.pos) }
            self.pos += 1;
            unsafe { *triple_byte.get_unchecked_mut(1) = *self.buf.get_unchecked(self.pos) }
            self.pos += 1;
            unsafe { *triple_byte.get_unchecked_mut(2) = *self.buf.get_unchecked(self.pos) }
            self.pos += 1;
        }
        // If the by buffer does not have three bytes available, copy one byte at a time from the
        // buffer, checking if it needs to be replenished.
        else {
            for i in 0..3 {
                if self.pos >= self.byte_length {
                    self.fetch_buffer_or_eof()?;
                }
                unsafe { *triple_byte.get_unchecked_mut(i) = *self.buf.get_unchecked(self.pos) }
                self.pos += 1;
            }
        }

        return Ok(triple_byte);
    }

    // Reads four bytes from the stream and returns them in read-order or an error.
    fn read_quad_bytes(&mut self) -> io::Result<[u8; 4]> {
        let mut quad_byte = [0u8; 4];

        // If the buffer has four bytes available, copy directly from it and skip any safety or
        // buffering checks.
        if self.pos + 4 < self.byte_length {
            unsafe { *quad_byte.get_unchecked_mut(0) = *self.buf.get_unchecked(self.pos) }
            self.pos += 1;
            unsafe { *quad_byte.get_unchecked_mut(1) = *self.buf.get_unchecked(self.pos) }
            self.pos += 1;
            unsafe { *quad_byte.get_unchecked_mut(2) = *self.buf.get_unchecked(self.pos) }
            self.pos += 1;
            unsafe { *quad_byte.get_unchecked_mut(3) = *self.buf.get_unchecked(self.pos) }
            self.pos += 1;
        }
        // If the by buffer does not have four bytes available, copy one byte at a time from the
        // buffer, checking if it needs to be replenished.
        else {
            for i in 0..4 {
                if self.pos >= self.byte_length {
                    self.fetch_buffer_or_eof()?;
                }
                unsafe { *quad_byte.get_unchecked_mut(i) = *self.buf.get_unchecked(self.pos) }
                self.pos += 1;
            }
        }

        return Ok(quad_byte);
    }

    fn read_buf_bytes(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.read_exact(buf)
    }

    fn ignore_bytes(&mut self, mut count: usize) -> io::Result<()> {
        while count > 0 {
            let buffer = self.fetch_buffer()?;
            let discard_count = cmp::min(buffer.len(), count);
            self.pos += discard_count;
            count -= discard_count;
        }
        Ok(())
    }

}

impl<'b, B: Bytestream> Bytestream for &'b mut B {
    #[inline(always)]
    fn read_byte(&mut self) -> io::Result<u8> {
        (*self).read_byte()
    }

    #[inline(always)]
    fn read_double_bytes(&mut self) -> io::Result<[u8; 2]> {
        (*self).read_double_bytes()
    }

    #[inline(always)]
    fn read_triple_bytes(&mut self) -> io::Result<[u8; 3]> {
        (*self).read_triple_bytes()
    }

    #[inline(always)]
    fn read_quad_bytes(&mut self) -> io::Result<[u8; 4]> {
        (*self).read_quad_bytes()
    }

    #[inline(always)]
    fn read_buf_bytes(&mut self, buf: &mut [u8]) -> io::Result<()> {
        (*self).read_buf_bytes(buf)
    }

    #[inline(always)]
    fn ignore_bytes(&mut self, count: usize) -> io::Result<()> {
        (*self).ignore_bytes(count)
    }
}

/// An `ErrorDetectingStream` is a passive monitoring stream which computes one or more checksums
/// on the data passing through it. Checksumming algorithms may be pushed and popped onto the
/// stream to begin and end error detection.
pub struct ErrorDetectingStream<B: Bytestream, C: Checksum> {
    inner: B,
    checksum: C,
}

impl<B: Bytestream, C: Checksum> ErrorDetectingStream<B, C> {
    pub fn new(checksum: C, inner: B) -> ErrorDetectingStream<B, C> {
        ErrorDetectingStream {
            inner: inner,
            checksum: checksum,
        }
    }

    pub fn inner(&self) -> &B {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut B {
        &mut self.inner
    }

    pub fn to_inner(self) -> B {
        self.inner
    }

    pub fn checksum(&self) -> &C {
        &self.checksum
    }

    pub fn checksum_mut(&mut self) -> &mut C {
        &mut self.checksum
    }

}

impl<B : Bytestream, C: Checksum> Bytestream for ErrorDetectingStream<B, C> {

    /// Reads a single byte from the stream and returns it or an error.
    #[inline(always)]
    fn read_byte(&mut self) -> io::Result<u8> {
        let byte = self.inner.read_byte()?;
        self.checksum.process_byte(&byte);
        Ok(byte)
    }

    // Reads two bytes from the stream and returns them in read-order or an error.
    #[inline(always)]
    fn read_double_bytes(&mut self) -> io::Result<[u8; 2]> {
        let bytes = self.inner.read_double_bytes()?;
        self.checksum.process_double_bytes(&bytes);
        Ok(bytes)
    }

    // Reads three bytes from the stream and returns them in read-order or an error.
    #[inline(always)]
    fn read_triple_bytes(&mut self) -> io::Result<[u8; 3]> {
        let bytes = self.inner.read_triple_bytes()?;
        self.checksum.process_triple_bytes(&bytes);
        Ok(bytes)
    }

    // Reads four bytes from the stream and returns them in read-order or an error.
    #[inline(always)]
    fn read_quad_bytes(&mut self) -> io::Result<[u8; 4]> {
        let bytes = self.inner.read_quad_bytes()?;
        self.checksum.process_quad_bytes(&bytes);
        Ok(bytes)
    }

    fn read_buf_bytes(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_buf_bytes(buf)?;
        self.checksum.process_buf_bytes(&buf);
        Ok(())
    }

    fn ignore_bytes(&mut self, count: usize) -> io::Result<()> {
        self.inner.ignore_bytes(count)
    }

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
}

/// A `BitReaderLtr` provides an implementation of a `BitReader` that interprets sequential bits in
/// a single read as descending in significance. That is to say, if N-bits are read from a
/// `BitReaderLtr` then bit 0 (the first bit read from the source) is the most-significant bit and
/// bit N-1 is the least-significant.
pub struct BitReaderLtr {
    byte: u8,
    bits_left: u32,
}

impl BitReaderLtr {

    /// Instantiates a new `BitReaderLtr`.
    pub fn new() -> BitReaderLtr {
        BitReaderLtr {
            byte: 0,
            bits_left: 0,
        }
    }

    #[inline(always)]
    fn get_byte<B: Bytestream>(&mut self, src: &mut B) -> io::Result<u8> {
        if self.bits_left == 0 {
            self.byte = src.read_u8()?;
            self.bits_left = 8;
        }
        Ok(self.byte)
    }
}

impl BitReader for BitReaderLtr {

    fn realign(&mut self) {
        self.bits_left = 0;
    }

    fn ignore_bits<B: Bytestream>(&mut self, src: &mut B, mut num_bits: u32) -> io::Result<()> {
        if num_bits <= self.bits_left {
            self.bits_left = 0;
        }
        else {
            while num_bits > 0 {
                self.get_byte(src)?;
                let skip = cmp::min(self.bits_left, num_bits);
                self.bits_left -= skip;
                num_bits -= skip;
            }
        }

        Ok(())
    }

    fn read_bit<B: Bytestream>(&mut self, src: &mut B) -> io::Result<bool> {
        let byte = self.get_byte(src)?;
        self.bits_left -= 1;
        let mask = mask_at(self.bits_left);
        Ok((byte & mask) == mask)
    }

    fn read_bits_leq32<B: Bytestream>(&mut self, src: &mut B, num_bits: u32) -> io::Result<u32> {
        debug_assert!(num_bits <= 32);

        let mut idx: u32 = 0;
        let mut res: u32 = 0;

        while idx < num_bits {
            // Obtain the currently readable byte.
            let byte = self.get_byte(src)?;

            // Calculate the maximum number bits to stream from the currently readable byte.
            let count = cmp::min(num_bits - idx, self.bits_left);

            // Create a bitmask from the current bit position, and the count.
            let mask = mask_range(self.bits_left, self.bits_left - count);

            // Extract the masked bits, which could be any range within the byte, and shift it so
            // that the LSB of the extracted bits, and the actual LSB align.
            let bits = ((byte & mask) as u32) >> (self.bits_left - count);

            idx += count;

            // Shift into final position.
            res |= bits << (num_bits - idx);

            self.bits_left -= count;
        }

        Ok(res)
    }

    fn read_bits_leq64<B: Bytestream>(&mut self, src: &mut B, num_bits: u32) -> io::Result<u64> {
        debug_assert!(num_bits <= 64);

        let mut idx: u32 = 0;
        let mut res: u64 = 0;

        while idx < num_bits {
            // Obtain the currently readable byte.
            let byte = self.get_byte(src)?;

            // Calculate the maximum number bits to stream from the currently readable byte.
            let count = cmp::min(num_bits - idx, self.bits_left);

            // Create a bitmask from the current bit position, and the count.
            let mask = mask_range(self.bits_left, self.bits_left - count);

            // Extract the masked bits, which could be any range within the byte, and shift it so
            // that the LSB of the extracted bits, and the actual LSB align.
            let bits = ((byte & mask) as u64) >> (self.bits_left - count);

            idx += count;

            // Shift into final position.
            res |= bits << (num_bits - idx);

            self.bits_left -= count;
        }

        Ok(res)
    }

    fn read_unary<B: Bytestream>(&mut self, src: &mut B) -> io::Result<u32> {
        let mut num = 0;

        loop {
            // Obtain the currently readable byte.
            let mut byte = self.get_byte(src)?;

            // Remove the previously read bits from the byte by lefting left, and appending 1s to
            // prevent reading the extra 0s shifted on.
            let shift = 8 - self.bits_left;
            byte = byte.wrapping_shl(shift) | mask_lower(shift);

            // Count the leading zeros.
            let zeros = byte.leading_zeros();

            // Increment the decoded number.
            num += zeros;

            // A unary encoded number is suffixed with a 1. If the number of bits remaining in the
            // currently readable byte is greater than the number of 0s counted this iteration,
            // then a 1 was encounted. The unary number is decoded at this point. Subtract an extra
            // bit from the bits_left value to account for the suffixed 1.
            if zeros < self.bits_left {
                self.bits_left -= zeros + 1;
                break;
            }

            self.bits_left -= zeros;
        }

        Ok(num)
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
}

pub struct BitStreamLtr<B: Bytestream> {
    inner: B,
    reader: BitReaderLtr,
}

impl<B: Bytestream> BitStreamLtr<B> {
    pub fn new(inner: B) -> BitStreamLtr<B> {
        BitStreamLtr {
            inner: inner,
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
}




// Look up table of masks given the number of bits remaining and number of bits desired.
const BIT_LOOKUP_MASKS: [u8; 72]= [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,   // 0 bits remaining => 0b0000_0000
    0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,   // 1 bits remaining => 0b0000_0001
    0x02, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03,   // 2 bits remaining => 0b0000_0011
    0x04, 0x06, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07,   // 3 bits remaining => 0b0000_0111
    0x08, 0x0c, 0x0e, 0x0f, 0x0f, 0x0f, 0x0f, 0x0f,   // 4 bits remaining => 0b0000_1111
    0x10, 0x18, 0x1c, 0x1e, 0x1f, 0x1f, 0x1f, 0x1f,   // 5 bits remaining => 0b0001_1111
    0x20, 0x30, 0x38, 0x3c, 0x3e, 0x3f, 0x3f, 0x3f,   // 6 bits remaining => 0b0011_1111
    0x40, 0x60, 0x70, 0x78, 0x7c, 0x7e, 0x7f, 0x7f,   // 7 bits remaining => 0b0111_1111
    0x80, 0xc0, 0xe0, 0xf0, 0xf8, 0xfc, 0xfe, 0xff,   // 8 bits remaining => 0b1111_1111
];

/*
/// A `BitReaderRtl` provides an implementation of a `BitReader` that interprets sequential bits in
/// a single read as increasing in significance. That is to say, if N-bits are read from a
/// `BitReaderRtl` then bit 0 (the first bit read from the source) is the least-significant bit and
/// bit N-1 is the most-significant.
pub struct BitReaderRtl {
    byte: u8,
    bit_pos: u32,
}
*/

/// Decodes a big-endiann unsigned integers encoded via extended UTF8. In this context, extended
/// UTF8 simply means the encoded UTF8 value may be up to 7 bytes for a maximum integer bit width
/// of 36-bits.
pub fn utf8_decode_be_u64<B: Bytestream>(src : &mut B) -> io::Result<Option<u64>> {
    // Read the first byte of the UTF8 encoded integer.
    let mut state = src.read_u8()? as u64;

    // UTF8 prefixes 1s followed by a 0 to indicate the total number of bytes within the multi-byte
    // sequence. Using ranges, determine the mask that will overlap the data bits within the first
    // byte of the sequence. For values 0-128, return the value immediately. If the value falls out
    // of range return None as this is either not the start of a UTF8 sequence or the prefix is
    // incorrect.
    let mask: u8 = match state {
        0x00...0x7f => return Ok(Some(state)),
        0xc0...0xdf => 0x1f,
        0xe0...0xef => 0x0f,
        0xf0...0xf7 => 0x07,
        0xf8...0xfb => 0x03,
        0xfc...0xfd => 0x01,
        0xfe        => 0x00,
        _           => return Ok(None)
    };

    // Obtain the data bits from the first byte by using the data mask.
    state = state & (mask as u64);

    // Read the remaining bytes within the UTF8 sequence. Since the mask 0s out the UTF8 prefix
    // of 1s which indicate the length of the multi-byte sequence in bytes, plus an additional 0
    // bit, the number of remaining bytes to read is the number of zeros in the mask minus 2.
    // To avoid extra computation, simply loop from 2 to the number of zeros.
    for _i in 2..mask.leading_zeros() {
        // Each subsequent byte after the first in UTF8 is prefixed with 0b10xx_xxxx, therefore
        // only 6 bits are useful. Append these six bits to the result by shifting the result left
        // by 6 bit positions, and appending the next subsequent byte with the first two high-order
        // bits masked out.
        state = (state << 6) | (src.read_u8()? & 0x3f) as u64;

        // TODO: Validation? Invalid if the byte is greater than 0x3f.
    }

    Ok(Some(state))
}

#[test]
fn verify_utf8_decode_be_u64() {
    let mut source = MediaSourceStream::new(Box::new(io::Cursor::new(vec![
        0x24, 0xc2, 0xa2, 0xe0, 0xa4, 0xb9, 0xe2, 0x82,
        0xac, 0xf0, 0x90, 0x8d, 0x88, 0xff, 0x80, 0xbf
    ])));

    assert_eq!(utf8_decode_be_u64(&mut source).unwrap(), Some(36));
    assert_eq!(utf8_decode_be_u64(&mut source).unwrap(), Some(162));
    assert_eq!(utf8_decode_be_u64(&mut source).unwrap(), Some(2361));
    assert_eq!(utf8_decode_be_u64(&mut source).unwrap(), Some(8364));
    assert_eq!(utf8_decode_be_u64(&mut source).unwrap(), Some(66376));
    assert_eq!(utf8_decode_be_u64(&mut source).unwrap(), None);
    assert_eq!(utf8_decode_be_u64(&mut source).unwrap(), None);
    assert_eq!(utf8_decode_be_u64(&mut source).unwrap(), None);
}

#[test]
fn verify_masks() {
    assert_eq!(mask_at(0), 0b0000_0001);
    assert_eq!(mask_at(1), 0b0000_0010);
    assert_eq!(mask_at(2), 0b0000_0100);
    assert_eq!(mask_at(3), 0b0000_1000);
    assert_eq!(mask_at(4), 0b0001_0000);
    assert_eq!(mask_at(5), 0b0010_0000);
    assert_eq!(mask_at(6), 0b0100_0000);
    assert_eq!(mask_at(7), 0b1000_0000);

    assert_eq!(mask_upper(0), 0b1111_1110);
    assert_eq!(mask_upper(1), 0b1111_1100);
    assert_eq!(mask_upper(2), 0b1111_1000);
    assert_eq!(mask_upper(3), 0b1111_0000);
    assert_eq!(mask_upper(4), 0b1110_0000);
    assert_eq!(mask_upper(5), 0b1100_0000);
    assert_eq!(mask_upper(6), 0b1000_0000);
    assert_eq!(mask_upper(7), 0b0000_0000);

    assert_eq!(mask_upper_eq(0), 0b1111_1111);
    assert_eq!(mask_upper_eq(1), 0b1111_1110);
    assert_eq!(mask_upper_eq(2), 0b1111_1100);
    assert_eq!(mask_upper_eq(3), 0b1111_1000);
    assert_eq!(mask_upper_eq(4), 0b1111_0000);
    assert_eq!(mask_upper_eq(5), 0b1110_0000);
    assert_eq!(mask_upper_eq(6), 0b1100_0000);
    assert_eq!(mask_upper_eq(7), 0b1000_0000);

    assert_eq!(mask_lower(0), 0b0000_0000);
    assert_eq!(mask_lower(1), 0b0000_0001);
    assert_eq!(mask_lower(2), 0b0000_0011);
    assert_eq!(mask_lower(3), 0b0000_0111);
    assert_eq!(mask_lower(4), 0b0000_1111);
    assert_eq!(mask_lower(5), 0b0001_1111);
    assert_eq!(mask_lower(6), 0b0011_1111);
    assert_eq!(mask_lower(7), 0b0111_1111);

    assert_eq!(mask_lower_eq(0), 0b0000_0001);
    assert_eq!(mask_lower_eq(1), 0b0000_0011);
    assert_eq!(mask_lower_eq(2), 0b0000_0111);
    assert_eq!(mask_lower_eq(3), 0b0000_1111);
    assert_eq!(mask_lower_eq(4), 0b0001_1111);
    assert_eq!(mask_lower_eq(5), 0b0011_1111);
    assert_eq!(mask_lower_eq(6), 0b0111_1111);
    assert_eq!(mask_lower_eq(7), 0b1111_1111);

    assert_eq!(mask_range(0, 0), 0b0000_0000);
    assert_eq!(mask_range(1, 1), 0b0000_0000);
    assert_eq!(mask_range(7, 7), 0b0000_0000);
    assert_eq!(mask_range(1, 0), 0b0000_0001);
    assert_eq!(mask_range(2, 0), 0b0000_0011);
    assert_eq!(mask_range(7, 0), 0b0111_1111);
    assert_eq!(mask_range(5, 2), 0b0001_1100);
    assert_eq!(mask_range(7, 2), 0b0111_1100);
    assert_eq!(mask_range(8, 2), 0b1111_1100);
}

#[test]
fn verify_read_bit() {
    let mut source = MediaSourceStream::new(Box::new(io::Cursor::new(vec![0b1010_1010])));
    let mut br = BitReaderLtr::new();

    assert_eq!(br.read_bit(&mut source).unwrap(), true);
    assert_eq!(br.read_bit(&mut source).unwrap(), false);
    assert_eq!(br.read_bit(&mut source).unwrap(), true);
    assert_eq!(br.read_bit(&mut source).unwrap(), false);
    assert_eq!(br.read_bit(&mut source).unwrap(), true);
    assert_eq!(br.read_bit(&mut source).unwrap(), false);
    assert_eq!(br.read_bit(&mut source).unwrap(), true);
    assert_eq!(br.read_bit(&mut source).unwrap(), false);
}

#[test]
fn verify_read_bits_leq32() {
    let mut source = MediaSourceStream::new(Box::new(io::Cursor::new(
        vec![0b1010_0101, 0b0111_1110, 0b1101_0011])));

    let mut br = BitReaderLtr::new();

    assert_eq!(br.read_bits_leq32(&mut source,  4).unwrap(), 0b0000_0000_0000_1010);
    assert_eq!(br.read_bits_leq32(&mut source,  4).unwrap(), 0b0000_0000_0000_0101);
    assert_eq!(br.read_bits_leq32(&mut source, 13).unwrap(), 0b0000_1111_1101_1010);
    assert_eq!(br.read_bits_leq32(&mut source,  3).unwrap(), 0b0000_0000_0000_0011);
}

#[test]
fn verify_read_bits_leq64() {
    let mut source = MediaSourceStream::new(Box::new(io::Cursor::new(
        vec![0x99, 0xaa, 0x55, 0xff, 0xff, 0x55, 0xaa, 0x99])));

    let mut br = BitReaderLtr::new();

    assert_eq!(br.read_bits_leq64(&mut source, 40).unwrap(), 0x99aa55ffff);
    assert_eq!(br.read_bits_leq64(&mut source,  8).unwrap(), 0x55);
    assert_eq!(br.read_bits_leq64(&mut source, 16).unwrap(), 0xaa99);
}

#[test]
fn verify_read_unary() {
    let mut source = MediaSourceStream::new(Box::new(io::Cursor::new(
        vec![0b0000_0001, 0b0001_0000, 0b0000_0000, 0b1000_0000, 0b1111_1011])));

    let mut br = BitReaderLtr::new();

    assert_eq!(br.read_unary(&mut source).unwrap(),  7);
    assert_eq!(br.read_unary(&mut source).unwrap(),  3);
    assert_eq!(br.read_unary(&mut source).unwrap(), 12);
    assert_eq!(br.read_unary(&mut source).unwrap(),  7);
    assert_eq!(br.read_unary(&mut source).unwrap(),  0);
    assert_eq!(br.read_unary(&mut source).unwrap(),  0);
    assert_eq!(br.read_unary(&mut source).unwrap(),  0);
    assert_eq!(br.read_unary(&mut source).unwrap(),  0);
    assert_eq!(br.read_unary(&mut source).unwrap(),  1);
    assert_eq!(br.read_unary(&mut source).unwrap(),  0);
}
