// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io;

use sonata_core::errors::Result;
use sonata_core::io::{Bytestream, FiniteStream};

pub fn read_syncsafe_leq32<B: Bytestream>(reader: &mut B, bit_width: u32) -> Result<u32> {
    debug_assert!(bit_width <= 32);

    let mut result = 0u32;
    let mut bits_read = 0;

    while bits_read < bit_width {
        bits_read += 7;
        result |= ((reader.read_u8()? & 0x7f) as u32) << (bit_width - bits_read);
    }

    Ok(result & (0xffffffff >> (32 - bit_width)))
}

pub fn decode_unsynchronisation<'a>(buf: &'a mut [u8]) -> &'a mut [u8] {
    let len = buf.len();
    let mut src = 0;
    let mut dst = 0;

    // Decode the unsynchronisation scheme in-place.
    while src < len - 1 {
        buf[dst] = buf[src];
        dst += 1;
        src += 1;

        if buf[src - 1] == 0xff && buf[src] == 0x00 {
            src += 1;
        }
    }

    if src < len {
        buf[dst] = buf[src];
        dst += 1;
    }

    &mut buf[..dst]
}

pub struct UnsyncStream<B: Bytestream + FiniteStream> {
    inner: B,
    byte: u8,
}

impl<B: Bytestream + FiniteStream> UnsyncStream<B> {
    pub fn new(inner: B) -> Self {
        UnsyncStream {
            inner,
            byte: 0,
        }
    }
}

impl<B: Bytestream + FiniteStream> FiniteStream for UnsyncStream<B> {
    #[inline(always)]
    fn len(&self) -> u64 {
        self.inner.len()
    }

    #[inline(always)]
    fn bytes_read(&self) -> u64 {
        self.inner.bytes_read()
    }
    
    #[inline(always)]
    fn bytes_available(&self) -> u64 {
        self.inner.bytes_available()
    }
}

impl<B: Bytestream + FiniteStream> Bytestream for UnsyncStream<B> {

    fn read_byte(&mut self) -> io::Result<u8> {
        let last = self.byte;

        self.byte = self.inner.read_byte()?;

        // If the last byte was 0xff, and the current byte is 0x00, the current byte should be 
        // dropped and the next byte read instead.
        if last == 0xff && self.byte == 0x00 {
            self.byte = self.inner.read_byte()?;
        }

        Ok(self.byte)
    }

    fn read_double_bytes(&mut self) -> io::Result<[u8; 2]> {
        Ok([
            self.read_byte()?,
            self.read_byte()?,
        ])
    }

    fn read_triple_bytes(&mut self) -> io::Result<[u8; 3]> {
        Ok([
            self.read_byte()?,
            self.read_byte()?,
            self.read_byte()?,
        ])
    }

    fn read_quad_bytes(&mut self) -> io::Result<[u8; 4]> {
        Ok([
            self.read_byte()?,
            self.read_byte()?,
            self.read_byte()?,
            self.read_byte()?,
        ])
    }

    fn read_buf_bytes(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let len = buf.len();

        if len > 0 { 
            // Fill the provided buffer directly from the underlying reader.
            self.inner.read_buf_bytes(buf)?;

            // If the last seen byte was 0xff, and the first byte in buf is 0x00, skip the first 
            // byte of buf.
            let mut src = if self.byte == 0xff && buf[0] == 0x00 { 1 } else { 0 };
            let mut dst = 0;

            // Record the last byte in buf to continue unsychronisation streaming later.
            self.byte = buf[len - 1];

            // Decode the unsynchronisation scheme in-place.
            while src < len - 1 {
                buf[dst] = buf[src];
                dst += 1;
                src += 1;

                if buf[src - 1] == 0xff && buf[src] == 0x00 {
                    src += 1;
                }
            }

            // When the final two src bytes are [ 0xff, 0x00 ], src will always equal len. 
            // Therefore, if src < len, then the final byte should always be copied to dst.
            if src < len {
                buf[dst] = buf[src];
                dst += 1;
            }

            // If dst < len, then buf is not full. Read the remaining bytes manually to completely 
            // fill buf.
            while dst < len {
                buf[dst] = self.read_byte()?;
                dst += 1;
            }
        }

        Ok(())
    }

    fn scan_bytes_aligned<'a>(
        &mut self, 
        _: &[u8], 
        _: usize, 
        _: &'a mut [u8]
    ) -> io::Result<&'a mut [u8]> {
        // Intentionally left unimplemented.
        unimplemented!();
    }

    fn ignore_bytes(&mut self, count: u64) -> io::Result<()> {
        for _ in 0..count {
            self.inner.read_byte()?;
        }
        Ok(())
    }
}
