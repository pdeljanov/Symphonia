// Symphonia
// Copyright (c) 2019 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::cmp;
use std::io;

use super::{ByteStream, FiniteStream};

const OUT_OF_BOUNDS_ERROR_STR: &str = "out of bounds";

/// A `ScopedStream` restricts the number of bytes read to a specified limit.
pub struct ScopedStream<B: ByteStream> {
    inner: B,
    len: u64,
    read: u64,
}

impl<B: ByteStream> ScopedStream<B> {
    pub fn new(inner: B, len: u64) -> Self {
        ScopedStream {
            inner,
            len,
            read: 0,
        }
    }

    /// Returns an immutable reference to the inner `ByteStream`.
    pub fn inner(&self) -> &B {
        &self.inner
    }

    /// Returns a mutable reference to the inner `ByteStream`.
    pub fn inner_mut(&mut self) -> &mut B {
        &mut self.inner
    }

    /// Ignores the remainder of the `ScopedStream`.
    pub fn ignore(&mut self) -> io::Result<()> {
        self.inner.ignore_bytes(self.len - self.read)
    }

    /// Convert the `ScopedStream` to the inner `ByteStream`.
    pub fn into_inner(self) -> B {
        self.inner
    }
}

impl<B: ByteStream> FiniteStream for ScopedStream<B> {
    /// Returns the length of the the `ScopedStream`.
    fn len(&self) -> u64 {
        self.len
    }

    /// Returns the number of bytes read.
    fn bytes_read(&self) -> u64 {
        self.read
    }

    /// Returns the number of bytes available to read.
    fn bytes_available(&self) -> u64 {
        self.len - self.read
    }
}

impl<B: ByteStream,> ByteStream for ScopedStream<B> {

    #[inline(always)]
    fn read_byte(&mut self) -> io::Result<u8> {
        if self.len - self.read < 1 {
            return Err(io::Error::new(io::ErrorKind::Other, OUT_OF_BOUNDS_ERROR_STR));
        }

        self.read += 1;
        self.inner.read_byte()
    }

    #[inline(always)]
    fn read_double_bytes(&mut self) -> io::Result<[u8; 2]> {
        if self.len - self.read < 2 {
            return Err(io::Error::new(io::ErrorKind::Other, OUT_OF_BOUNDS_ERROR_STR));
        }

        self.read += 2;
        self.inner.read_double_bytes()
    }

    #[inline(always)]
    fn read_triple_bytes(&mut self) -> io::Result<[u8; 3]> {
        if self.len - self.read < 3 {
            return Err(io::Error::new(io::ErrorKind::Other, OUT_OF_BOUNDS_ERROR_STR));
        }

        self.read += 3;
        self.inner.read_triple_bytes()
    }

    #[inline(always)]
    fn read_quad_bytes(&mut self) -> io::Result<[u8; 4]> {
        if self.len - self.read < 4 {
            return Err(io::Error::new(io::ErrorKind::Other, OUT_OF_BOUNDS_ERROR_STR));
        }

        self.read += 4;
        self.inner.read_quad_bytes()
    }

    fn read_buf(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Limit read_buf() to the remainder of the scoped bytes if buf has a greater length.
        let scoped_len = cmp::min(self.len - self.read, buf.len() as u64) as usize;
        let result = self.inner.read_buf(&mut buf[0..scoped_len])?;
        self.read += result as u64;
        Ok(result)
    }

    fn read_buf_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        if self.len - self.read < buf.len() as u64 {
            return Err(io::Error::new(io::ErrorKind::Other, OUT_OF_BOUNDS_ERROR_STR));
        }

        self.read += buf.len() as u64;
        self.inner.read_buf_exact(buf)
    }

    #[inline(always)]
    fn scan_bytes_aligned<'a>(
        &mut self,
        pattern: &[u8],
        align: usize,
        buf: &'a mut [u8]
    ) -> io::Result<&'a mut [u8]> {
        if self.len - self.read < buf.len() as u64 {
            return Err(io::Error::new(io::ErrorKind::Other, OUT_OF_BOUNDS_ERROR_STR));
        }

        let result = self.inner.scan_bytes_aligned(pattern, align, buf)?;
        self.read += result.len() as u64;
        Ok(result)
    }

    fn ignore_bytes(&mut self, count: u64) -> io::Result<()> {
        if self.len - self.read < count {
            return Err(io::Error::new(io::ErrorKind::Other, OUT_OF_BOUNDS_ERROR_STR));
        }

        self.read += count;
        self.inner.ignore_bytes(count)
    }
}
