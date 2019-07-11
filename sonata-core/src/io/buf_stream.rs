// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::cmp;
use std::io;

use super::{Bytestream, FiniteStream};

/// `BufStream` is a stream backed by a buffer.
pub struct BufStream<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> BufStream<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        BufStream {
            buf,
            pos: 0,
        }
    }

    #[inline(always)]
    pub fn scan_bytes_ref(&mut self, pattern: &[u8], max_len: usize) -> io::Result<&'a [u8]> {
        self.scan_bytes_aligned_ref(pattern, 1, max_len)
    }

    pub fn scan_bytes_aligned_ref(&mut self, pattern: &[u8], align: usize, max_len: usize) -> io::Result<&'a [u8]> {
        // The pattern must be atleast one byte.
        debug_assert!(pattern.len() > 0);
        // The output buffer must be atleast the length of the pattern. 
        debug_assert!(pattern.len() <= max_len);

        let start = self.pos;
        let remaining = self.buf.len() - start;

        if remaining < pattern.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "would exceed buffer"));
        }

        let end = start + cmp::min(remaining, max_len);

        let mut j = start;
        let mut i = start + pattern.len();

        while i < end {
            if &self.buf[j..i] == pattern {
                break;
            }
            i += align;
            j += align;
        }

        self.pos = cmp::min(i, self.buf.len());
        Ok(&self.buf[start..self.pos])
    }
}

impl<'a> Bytestream for BufStream<'a> {

    #[inline(always)]
    fn read_byte(&mut self) -> io::Result<u8> {
        if self.pos >= self.buf.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "would exceed buffer"));
        }
        self.pos += 1;
        Ok(self.buf[self.pos - 1])
    }

    #[inline(always)]
    fn read_double_bytes(&mut self) -> io::Result<[u8; 2]> {
        if self.pos + 2 > self.buf.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "would exceed buffer"));
        }

        let mut double_bytes: [u8; 2] = unsafe { std::mem::uninitialized() };
        double_bytes.copy_from_slice(&self.buf[self.pos..self.pos + 2]);
        self.pos += 2;

        Ok(double_bytes)
    }

    #[inline(always)]
    fn read_triple_bytes(&mut self) -> io::Result<[u8; 3]> {
        if self.pos + 3 > self.buf.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "would exceed buffer"));
        }

        let mut triple_bytes: [u8; 3] = unsafe { std::mem::uninitialized() };
        triple_bytes.copy_from_slice(&self.buf[self.pos..self.pos + 3]);
        self.pos += 3;

        Ok(triple_bytes)
    }

    #[inline(always)]
    fn read_quad_bytes(&mut self) -> io::Result<[u8; 4]> {
        if self.pos + 4 > self.buf.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "would exceed buffer"));
        }

        let mut quad_bytes: [u8; 4] = unsafe { std::mem::uninitialized() };
        quad_bytes.copy_from_slice(&self.buf[self.pos..self.pos + 4]);
        self.pos += 4;

        Ok(quad_bytes)
    }

    fn read_buf_bytes(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let len = buf.len();

        if self.pos + len > self.buf.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "would exceed buffer"));
        }

        buf.copy_from_slice(&self.buf[self.pos..self.pos + len]);
        self.pos += len;

        Ok(())
    }

    fn scan_bytes_aligned<'b>(&mut self, pattern: &[u8], align: usize, buf: &'b mut [u8]) -> io::Result<&'b mut [u8]> {
        let result = self.scan_bytes_aligned_ref(pattern, align, buf.len())?;
        buf[..result.len()].copy_from_slice(result);
        Ok(&mut buf[..result.len()])
    }

    fn ignore_bytes(&mut self, count: u64) -> io::Result<()> {
        if self.pos + count as usize > self.buf.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "would exceed buffer"));
        }
        self.pos += count as usize;
        Ok(())
     }
}

impl<'a> FiniteStream for BufStream<'a> {
    #[inline(always)]
    fn len(&self) -> u64 {
        self.buf.len() as u64
    }

    #[inline(always)]
    fn bytes_read(&self) -> u64 {
        self.pos as u64
    }

    #[inline(always)]
    fn bytes_available(&self) -> u64 {
        (self.buf.len() - self.pos) as u64
    }
}
