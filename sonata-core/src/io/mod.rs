// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io;
use std::mem;

use super::util::bits::*;

mod buf_stream;
mod media_source_stream;
mod monitor_stream;
mod scoped_stream;

pub use buf_stream::BufStream;
pub use media_source_stream::MediaSourceStream;
pub use monitor_stream::{Monitor, MonitorStream};
pub use scoped_stream::ScopedStream;

/// A `MediaSource` is a composite trait of `std::io::Read` and `std::io::Seek`. Seeking is an optional capability and 
/// support for it can be queried at runtime.
pub trait MediaSource: io::Read + io::Seek {
    /// Returns if the source is seekable. This may be an expensive operation.
    fn is_seekable(&self) -> bool;

    /// Returns the length in bytes, if available. This may be an expensive operation.
    fn len(&self) -> Option<u64>;
}

impl MediaSource for std::fs::File {
    /// Returns if the `std::io::File` backing the `MediaSource` is seekable.
    /// 
    /// Note: This operation involves querying the underlying file descriptor for information and may be moderately
    /// expensive. Therefore it is recommended to cache this value if used often.
    fn is_seekable(&self) -> bool {
        // If the file's metadata is available, and the file is a regular file (i.e., not a FIFO, etc.), then the 
        // MediaSource will be seekable. Otherwise assume it is not. Note that metadata() follows symlinks.
        match self.metadata() {
            Ok(metadata) => metadata.is_file(),
            _ => false
        }
    }

    /// Returns the length in bytes of the `std::io::File` backing the `MediaSource`.
    /// 
    /// Note: This operation involves querying the underlying file descriptor for information and may be moderately
    /// expensive. Therefore it is recommended to cache this value if used often.
    fn len(&self) -> Option<u64> {
        match self.metadata() {
            Ok(metadata) => Some(metadata.len()),
            _ => None,
        }
    }
}

impl<T: std::convert::AsRef<[u8]>> MediaSource for io::Cursor<T> {
    /// Always returns true since a `io::Cursor<u8>` is always seekable.
    fn is_seekable(&self) -> bool {
        true
    }

    /// Returns the length in bytes of the `io::Cursor<u8>` backing the `MediaSource`.
    fn len(&self) -> Option<u64> {
        // Get the underlying container, usually &Vec<T>.
        let inner = self.get_ref();
        // Get slice from the underlying container, &[T], for the len() function.
        Some(inner.as_ref().len() as u64)
    }
}

/// A `Bytestream` provides functions to read bytes and interpret them as little- or big-endian unsigned integers or 
/// floating point values of standard widths.
pub trait Bytestream {

    /// Reads a single byte from the stream and returns it or an error.
    fn read_byte(&mut self) -> io::Result<u8>;

    /// Reads two bytes from the stream and returns them in read-order or an error.
    fn read_double_bytes(&mut self) -> io::Result<[u8; 2]>;

    /// Reads three bytes from the stream and returns them in read-order or an error.
    fn read_triple_bytes(&mut self) -> io::Result<[u8; 3]>;

    /// Reads four bytes from the stream and returns them in read-order or an error.
    fn read_quad_bytes(&mut self) -> io::Result<[u8; 4]>;

    /// Reads exactly the number of bytes required to fill be provided buffer or returns an error.
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
        let mut buf = [0u8; mem::size_of::<u32>()];
        buf[0..3].clone_from_slice(&self.read_triple_bytes()?);
        Ok(u32::from_le_bytes(buf))
    }

    /// Reads three bytes from the stream and interprets them as an unsigned 24-bit big-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_be_u24(&mut self) -> io::Result<u32> {
        let mut buf = [0u8; mem::size_of::<u32>()];
        buf[0..3].clone_from_slice(&self.read_triple_bytes()?);
        Ok(u32::from_be_bytes(buf) >> 8)
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
        let mut buf = [0u8; mem::size_of::<u64>()];
        self.read_buf_bytes(&mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    /// Reads eight bytes from the stream and interprets them as an unsigned 64-bit big-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_be_u64(&mut self) -> io::Result<u64> {
        let mut buf = [0u8; mem::size_of::<u64>()];
        self.read_buf_bytes(&mut buf)?;
        Ok(u64::from_be_bytes(buf))
    }

    /// Reads four bytes from the stream and interprets them as a 32-bit little-endiann IEEE-754 
    /// floating point value.
    #[inline(always)]
    fn read_f32(&mut self) -> io::Result<f32> {
        Ok(unsafe { *(&u32::from_le_bytes(self.read_quad_bytes()?) as *const u32 as *const f32) })
    }

    /// Reads four bytes from the stream and interprets them as a 32-bit big-endiann IEEE-754 
    /// floating point value.
    #[inline(always)]
    fn read_be_f32(&mut self) -> io::Result<f32> {
        Ok(unsafe { *(&u32::from_be_bytes(self.read_quad_bytes()?) as *const u32 as *const f32) })
    }

    /// Reads four bytes from the stream and interprets them as a 64-bit little-endiann IEEE-754 
    /// floating point value.
    #[inline(always)]
    fn read_f64(&mut self) -> io::Result<f64> {
        let mut buf = [0u8; mem::size_of::<u64>()];
        self.read_buf_bytes(&mut buf)?;
        Ok(unsafe { *(&u64::from_le_bytes(buf) as *const u64 as *const f64) })
    }

    /// Reads four bytes from the stream and interprets them as a 64-bit big-endiann IEEE-754 
    /// floating point value.
    #[inline(always)]
    fn read_be_f64(&mut self) -> io::Result<f64> {
        let mut buf = [0u8; mem::size_of::<u64>()];
        self.read_buf_bytes(&mut buf)?;
        Ok(unsafe { *(&u64::from_be_bytes(buf) as *const u64 as *const f64) })
    }

    /// Reads exactly the number of bytes requested, and returns a boxed slice of the data or an error.
    fn read_boxed_slice_bytes(&mut self, len: usize) -> io::Result<Box<[u8]>> {
        let mut buf = Vec::<u8>::with_capacity(len);
        unsafe { buf.set_len(len); }
        self.read_buf_bytes(&mut buf)?;
        Ok(buf.into_boxed_slice())
    }

    /// Reads bytes from the stream into a supplied buffer until a byte pattern is matched. Returns a mutable slice to
    /// the valid region of the provided buffer.
    #[inline(always)]
    fn scan_bytes<'a>(&mut self, pattern: &[u8], buf: &'a mut [u8]) -> io::Result<&'a mut [u8]> {
        self.scan_bytes_aligned(pattern, 1, buf)
    }

    /// Reads bytes from a stream into a supplied buffer until a byte patter is matched on an aligned byte boundary.
    /// Returns a mutable slice to the valid region of the provided buffer.
    fn scan_bytes_aligned<'a>(&mut self, pattern: &[u8], align: usize, buf: &'a mut [u8]) -> io::Result<&'a mut [u8]>;

    /// Ignores the specified number of bytes from the stream or returns an error.
    fn ignore_bytes(&mut self, count: u64) -> io::Result<()>;
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
    fn scan_bytes_aligned<'a>(&mut self, pattern: &[u8], align: usize, buf: &'a mut [u8]) -> io::Result<&'a mut [u8]> {
        (*self).scan_bytes_aligned(pattern, align, buf)
    }

    #[inline(always)]
    fn ignore_bytes(&mut self, count: u64) -> io::Result<()> {
        (*self).ignore_bytes(count)
    }
}

/// A `FiniteStream` is a stream that has a definitive length. A `FiniteStream` therefore knows how many bytes are 
/// available for reading, or have been previously read.
pub trait FiniteStream {
    /// Returns the length of the the stream.
    fn len(&self) -> u64;

    /// Returns the number of bytes read.
    fn bytes_read(&self) -> u64;

    /// Returns the number of bytes available for reading.
    fn bytes_available(&self) -> u64;
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

}

impl BitReader for BitReaderLtr {
    
    #[inline(always)]
    fn realign(&mut self) {
        self.n_bits_left = 0;
    }

    #[inline(always)]
    fn ignore_bits<B: Bytestream>(&mut self, src: &mut B, mut num_bits: u32) -> io::Result<()> {
        // If the number of bits to ignore is less than the amount left, simply reduce the amount left.
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
                self.bits = src.read_u8()? as u32;
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
            self.bits = src.read_u8()? as u32;
            self.n_bits_left = 8;
        }
        self.n_bits_left -= 1;
        let mask = 1u32 << self.n_bits_left;
        Ok((self.bits & mask) == mask)
    }

    #[inline(always)]
    fn read_bits_leq32<B: Bytestream>(&mut self, src: &mut B, mut num_bits: u32) -> io::Result<u32> {
        debug_assert!(num_bits <= 32);

        let mask = !(0xffffffffffffffffu64 << num_bits) as u32;

        let mut res: u32 = self.bits;

        if num_bits <= self.n_bits_left {
            self.n_bits_left -= num_bits;
            res >>= self.n_bits_left;
        }
        else {
            num_bits -= self.n_bits_left;

            while num_bits >= 8 {
                res <<= 8;
                res |= src.read_u8()? as u32;
                num_bits -= 8;
            }

            if num_bits > 0 {
                res <<= num_bits;
                self.bits = src.read_u8()? as u32;
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
            let res = ((self.read_bits_leq32(src, 32)? as u64) << shift) | self.read_bits_leq32(src, shift)? as u64;
            return Ok(res);
        }
        
        Ok(self.read_bits_leq32(src, num_bits)? as u64)
    }

    #[inline(always)]
    fn read_unary<B: Bytestream>(&mut self, src: &mut B) -> io::Result<u32> {
        let mut num = 0;

        loop {

            let zeros = 
                if self.n_bits_left == 0 {
                    self.bits = src.read_u8()? as u32;
                    self.n_bits_left = 8;

                    (self.bits as u8).leading_zeros()
                }
                else {
                    // Count the number of valid leading zeros in bits by filling the upper unused 24 bits with 1s and 
                    // rotating right by the number of bits left. The leading bits will then contain the number of 
                    // unread bits.
                    let byte = (self.bits | 0xffffff00).rotate_right(self.n_bits_left);
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
        vec![0x99, 0xaa, 0x55, 0xff, 0xff, 0x55, 0xaa, 0x99, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88])));

    let mut br = BitReaderLtr::new();

    assert_eq!(br.read_bits_leq64(&mut source, 40).unwrap(), 0x99aa55ffff);
    assert_eq!(br.read_bits_leq64(&mut source,  4).unwrap(), 0x05);
    assert_eq!(br.read_bits_leq64(&mut source,  4).unwrap(), 0x05);
    assert_eq!(br.read_bits_leq64(&mut source, 16).unwrap(), 0xaa99);
    assert_eq!(br.read_bits_leq64(&mut source, 64).unwrap(), 0x1122334455667788);
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
