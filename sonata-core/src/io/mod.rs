// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use byteorder::{BigEndian, ByteOrder, LittleEndian};
use std::io;
use std::mem;

mod bit;
mod buf_stream;
mod media_source_stream;
mod monitor_stream;
mod scoped_stream;

pub use bit::*;
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
        Ok(LittleEndian::read_f32(&self.read_quad_bytes()?))
    }

    /// Reads four bytes from the stream and interprets them as a 32-bit big-endiann IEEE-754 
    /// floating point value.
    #[inline(always)]
    fn read_be_f32(&mut self) -> io::Result<f32> {
        Ok(BigEndian::read_f32(&self.read_quad_bytes()?))
    }

    /// Reads four bytes from the stream and interprets them as a 64-bit little-endiann IEEE-754 
    /// floating point value.
    #[inline(always)]
    fn read_f64(&mut self) -> io::Result<f64> {
        let mut buf = [0u8; mem::size_of::<u64>()];
        self.read_buf_bytes(&mut buf)?;
        Ok(LittleEndian::read_f64(&buf))
    }

    /// Reads four bytes from the stream and interprets them as a 64-bit big-endiann IEEE-754 
    /// floating point value.
    #[inline(always)]
    fn read_be_f64(&mut self) -> io::Result<f64> {
        let mut buf = [0u8; mem::size_of::<u64>()];
        self.read_buf_bytes(&mut buf)?;
        Ok(BigEndian::read_f64(&buf))
    }

    /// Reads exactly the number of bytes requested, and returns a boxed slice of the data or an error.
    fn read_boxed_slice_bytes(&mut self, len: usize) -> io::Result<Box<[u8]>> {
        let mut buf = vec![0u8; len];
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

#[cfg(test)]
mod tests {
    use super::BufStream;
    use super::utf8_decode_be_u64;

    #[test]
    fn verify_utf8_decode_be_u64() {
        let mut stream = BufStream::new(&[
            0x24, 0xc2, 0xa2, 0xe0, 0xa4, 0xb9, 0xe2, 0x82,
            0xac, 0xf0, 0x90, 0x8d, 0x88, 0xff, 0x80, 0xbf]);

        assert_eq!(utf8_decode_be_u64(&mut stream).unwrap(), Some(36));
        assert_eq!(utf8_decode_be_u64(&mut stream).unwrap(), Some(162));
        assert_eq!(utf8_decode_be_u64(&mut stream).unwrap(), Some(2361));
        assert_eq!(utf8_decode_be_u64(&mut stream).unwrap(), Some(8364));
        assert_eq!(utf8_decode_be_u64(&mut stream).unwrap(), Some(66376));
        assert_eq!(utf8_decode_be_u64(&mut stream).unwrap(), None);
        assert_eq!(utf8_decode_be_u64(&mut stream).unwrap(), None);
        assert_eq!(utf8_decode_be_u64(&mut stream).unwrap(), None);
    }
}