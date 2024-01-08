// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The `io` module implements composable bit- and byte-level I/O.
//!
//! The following nomenclature is used to denote where the data being read is sourced from:
//!  * A `Stream` consumes any source implementing [`ReadBytes`] one byte at a time.
//!  * A `Reader` consumes a `&[u8]`.
//!
//! The sole exception to this rule is [`MediaSourceStream`] which consumes sources implementing
//! [`MediaSource`] (aka. [`std::io::Read`]).
//!
//! All `Reader`s and `Stream`s operating on bytes of data at a time implement the [`ReadBytes`]
//! trait. Likewise, all `Reader`s and `Stream`s operating on bits of data at a time implement
//! either the [`ReadBitsLtr`] or [`ReadBitsRtl`] traits depending on the order in which they
//! consume bits.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::mem;
use crate::errors::{SymphoniaError, Result};

#[cfg(feature = "std")]
use std::io;

#[cfg(feature = "std")]
use std::io::IoSliceMut;

#[cfg(not(feature = "std"))]
use no_std_compat::IoSliceMut;

mod bit;
mod buf_reader;
mod media_source_stream;
mod monitor_stream;
mod scoped_stream;

pub use bit::*;
pub use buf_reader::BufReader;
pub use media_source_stream::{MediaSourceStream, MediaSourceStreamOptions};
pub use monitor_stream::{Monitor, MonitorStream};
pub use scoped_stream::ScopedStream;

pub trait Seek {
    fn seek(&mut self, _: SeekFrom) -> Result<u64>;
}

pub enum SeekFrom {
    /// Sets the offset to the provided number of bytes.
    Start(u64),

    /// Sets the offset to the size of this object plus the specified number of
    /// bytes.
    ///
    /// It is possible to seek beyond the end of an object, but it's an error to
    /// seek before byte 0.
    End(i64),

    /// Sets the offset to the current position plus the specified number of
    /// bytes.
    ///
    /// It is possible to seek beyond the end of an object, but it's an error to
    /// seek before byte 0.
    Current(i64),
}

pub trait Read {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;

    fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> Result<usize> {
        default_read_vectored(|b| self.read(b), bufs)
    }

    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        default_slow_read_to_end(self, buf)
    }
}

/// Warning: The default implementation in io::Read is much faster
fn default_slow_read_to_end<R: Read + ?Sized>(
    r: &mut R,
    buf: &mut Vec<u8>
) -> Result<usize> {

    let mut cnt: usize = 0;
    let mut read_buf: [u8; 1024] = [0; 1024];

    loop {
        let r =  r.read(&mut read_buf);
        let n = match r {
            Ok(0) => break,
            Ok(n) => n,
            Err(SymphoniaError::IoInterruptedError(_))  => 0, // Ignored
            Err(err) => return Err(err),
        };

        buf.extend_from_slice(&read_buf[0..n]);
        cnt += n;
    }

    Ok(cnt)
}

fn default_read_vectored<F>(read: F, bufs: &mut [IoSliceMut<'_>]) -> Result<usize>
    where
        F: FnOnce(&mut [u8]) -> Result<usize>,
{
    let buf = bufs.iter_mut().find(|b| !b.is_empty()).map_or(&mut [][..], |b| &mut **b);
    read(buf)
}

#[cfg(feature = "std")]
impl <T: std::io::Read> Read for T {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.read(buf).map_err(|e| { SymphoniaError::from(e) })
    }
}

#[cfg(feature = "std")]
impl <T: std::io::Seek> Seek for T  {
    fn seek(&mut self, from: SeekFrom) -> Result<u64> {
        let from = match from {
            SeekFrom::Start(x) => io::SeekFrom::Start(x),
            SeekFrom::End(x) => io::SeekFrom::End(x),
            SeekFrom::Current(x) => io::SeekFrom::Current(x),
        };
        self.seek(from).map_err(|e| { SymphoniaError::from(e) })
    }
}

/// `MediaSource` is a composite trait of [`std::io::Read`] and [`std::io::Seek`]. A source *must*
/// implement this trait to be used by [`MediaSourceStream`].
///
/// Despite requiring the [`std::io::Seek`] trait, seeking is an optional capability that can be
/// queried at runtime.
// pub trait MediaSource: io::Read + io::Seek + Send + Sync {
pub trait MediaSource: Read + Seek  {
    /// Returns if the source is seekable. This may be an expensive operation.
    fn is_seekable(&self) -> bool;

    /// Returns the length in bytes, if available. This may be an expensive operation.
    fn byte_len(&self) -> Option<u64>;
}

#[cfg(feature = "std")]
impl MediaSource for std::fs::File {
    /// Returns if the `std::io::File` backing the `MediaSource` is seekable.
    ///
    /// Note: This operation involves querying the underlying file descriptor for information and
    /// may be moderately expensive. Therefore it is recommended to cache this value if used often.
    fn is_seekable(&self) -> bool {
        // If the file's metadata is available, and the file is a regular file (i.e., not a FIFO,
        // etc.), then the MediaSource will be seekable. Otherwise assume it is not. Note that
        // metadata() follows symlinks.
        match self.metadata() {
            Ok(metadata) => metadata.is_file(),
            _ => false,
        }
    }

    /// Returns the length in bytes of the `std::io::File` backing the `MediaSource`.
    ///
    /// Note: This operation involves querying the underlying file descriptor for information and
    /// may be moderately expensive. Therefore it is recommended to cache this value if used often.
    fn byte_len(&self) -> Option<u64> {
        match self.metadata() {
            Ok(metadata) => Some(metadata.len()),
            _ => None,
        }
    }
}

#[cfg(feature = "std")]
impl<T: std::convert::AsRef<[u8]> + Send + Sync> MediaSource for std::io::Cursor<T> {
    /// Always returns true since a `io::Cursor<u8>` is always seekable.
    fn is_seekable(&self) -> bool {
        true
    }

    /// Returns the length in bytes of the `io::Cursor<u8>` backing the `MediaSource`.
    fn byte_len(&self) -> Option<u64> {
        // Get the underlying container, usually &Vec<T>.
        let inner = self.get_ref();
        // Get slice from the underlying container, &[T], for the len() function.
        Some(inner.as_ref().len() as u64)
    }
}

/// `ReadOnlySource` wraps any source implementing [`std::io::Read`] in an unseekable
/// [`MediaSource`].
pub struct ReadOnlySource<R: Read> {
    inner: R,
}

impl<R: Read + Send> ReadOnlySource<R> {
    /// Instantiates a new `ReadOnlySource<R>` by taking ownership and wrapping the provided
    /// `Read`er.
    pub fn new(inner: R) -> Self {
        ReadOnlySource { inner }
    }

    /// Gets a reference to the underlying reader.
    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    /// Gets a mutable reference to the underlying reader.
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Unwraps this `ReadOnlySource<R>`, returning the underlying reader.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read + Send + Sync> MediaSource for ReadOnlySource<R> {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

impl<R: Read> Read for ReadOnlySource<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.inner.read(buf)
    }
}

impl<R: Read> Seek for ReadOnlySource<R> {
    fn seek(&mut self, _: SeekFrom) -> Result<u64> {
        Err(SymphoniaError::Other("source does not support seeking"))
    }
}

/// `ReadBytes` provides methods to read bytes and interpret them as little- or big-endian
/// unsigned integers or floating-point values of standard widths.
pub trait ReadBytes {
    /// Reads a single byte from the stream and returns it or an error.
    fn read_byte(&mut self) -> Result<u8>;

    /// Reads two bytes from the stream and returns them in read-order or an error.
    fn read_double_bytes(&mut self) -> Result<[u8; 2]>;

    /// Reads three bytes from the stream and returns them in read-order or an error.
    fn read_triple_bytes(&mut self) -> Result<[u8; 3]>;

    /// Reads four bytes from the stream and returns them in read-order or an error.
    fn read_quad_bytes(&mut self) -> Result<[u8; 4]>;

    /// Reads up-to the number of bytes required to fill buf or returns an error.
    fn read_buf(&mut self, buf: &mut [u8]) -> Result<usize>;

    /// Reads exactly the number of bytes required to fill be provided buffer or returns an error.
    fn read_buf_exact(&mut self, buf: &mut [u8]) -> Result<()>;

    /// Reads a single unsigned byte from the stream and returns it or an error.
    #[inline(always)]
    fn read_u8(&mut self) -> Result<u8> {
        self.read_byte()
    }

    /// Reads a single signed byte from the stream and returns it or an error.
    #[inline(always)]
    fn read_i8(&mut self) -> Result<i8> {
        Ok(self.read_byte()? as i8)
    }

    /// Reads two bytes from the stream and interprets them as an unsigned 16-bit little-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.read_double_bytes()?))
    }

    /// Reads two bytes from the stream and interprets them as an signed 16-bit little-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.read_double_bytes()?))
    }

    /// Reads two bytes from the stream and interprets them as an unsigned 16-bit big-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_be_u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.read_double_bytes()?))
    }

    /// Reads two bytes from the stream and interprets them as an signed 16-bit big-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_be_i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.read_double_bytes()?))
    }

    /// Reads three bytes from the stream and interprets them as an unsigned 24-bit little-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_u24(&mut self) -> Result<u32> {
        let mut buf = [0u8; mem::size_of::<u32>()];
        buf[0..3].clone_from_slice(&self.read_triple_bytes()?);
        Ok(u32::from_le_bytes(buf))
    }

    /// Reads three bytes from the stream and interprets them as an signed 24-bit little-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_i24(&mut self) -> Result<i32> {
        Ok(((self.read_u24()? << 8) as i32) >> 8)
    }

    /// Reads three bytes from the stream and interprets them as an unsigned 24-bit big-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_be_u24(&mut self) -> Result<u32> {
        let mut buf = [0u8; mem::size_of::<u32>()];
        buf[0..3].clone_from_slice(&self.read_triple_bytes()?);
        Ok(u32::from_be_bytes(buf) >> 8)
    }

    /// Reads three bytes from the stream and interprets them as an signed 24-bit big-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_be_i24(&mut self) -> Result<i32> {
        Ok(((self.read_be_u24()? << 8) as i32) >> 8)
    }

    /// Reads four bytes from the stream and interprets them as an unsigned 32-bit little-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.read_quad_bytes()?))
    }

    /// Reads four bytes from the stream and interprets them as an signed 32-bit little-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.read_quad_bytes()?))
    }

    /// Reads four bytes from the stream and interprets them as an unsigned 32-bit big-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_be_u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.read_quad_bytes()?))
    }

    /// Reads four bytes from the stream and interprets them as a signed 32-bit big-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_be_i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.read_quad_bytes()?))
    }

    /// Reads eight bytes from the stream and interprets them as an unsigned 64-bit little-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_u64(&mut self) -> Result<u64> {
        let mut buf = [0u8; mem::size_of::<u64>()];
        self.read_buf_exact(&mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    /// Reads eight bytes from the stream and interprets them as an signed 64-bit little-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_i64(&mut self) -> Result<i64> {
        let mut buf = [0u8; mem::size_of::<i64>()];
        self.read_buf_exact(&mut buf)?;
        Ok(i64::from_le_bytes(buf))
    }

    /// Reads eight bytes from the stream and interprets them as an unsigned 64-bit big-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_be_u64(&mut self) -> Result<u64> {
        let mut buf = [0u8; mem::size_of::<u64>()];
        self.read_buf_exact(&mut buf)?;
        Ok(u64::from_be_bytes(buf))
    }

    /// Reads eight bytes from the stream and interprets them as an signed 64-bit big-endian
    /// integer or returns an error.
    #[inline(always)]
    fn read_be_i64(&mut self) -> Result<i64> {
        let mut buf = [0u8; mem::size_of::<i64>()];
        self.read_buf_exact(&mut buf)?;
        Ok(i64::from_be_bytes(buf))
    }

    /// Reads four bytes from the stream and interprets them as a 32-bit little-endian IEEE-754
    /// floating-point value.
    #[inline(always)]
    fn read_f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.read_quad_bytes()?))
    }

    /// Reads four bytes from the stream and interprets them as a 32-bit big-endian IEEE-754
    /// floating-point value.
    #[inline(always)]
    fn read_be_f32(&mut self) -> Result<f32> {
        Ok(f32::from_be_bytes(self.read_quad_bytes()?))
    }

    /// Reads four bytes from the stream and interprets them as a 64-bit little-endian IEEE-754
    /// floating-point value.
    #[inline(always)]
    fn read_f64(&mut self) -> Result<f64> {
        let mut buf = [0u8; mem::size_of::<u64>()];
        self.read_buf_exact(&mut buf)?;
        Ok(f64::from_le_bytes(buf))
    }

    /// Reads four bytes from the stream and interprets them as a 64-bit big-endian IEEE-754
    /// floating-point value.
    #[inline(always)]
    fn read_be_f64(&mut self) -> Result<f64> {
        let mut buf = [0u8; mem::size_of::<u64>()];
        self.read_buf_exact(&mut buf)?;
        Ok(f64::from_be_bytes(buf))
    }

    /// Reads up-to the number of bytes requested, and returns a boxed slice of the data or an
    /// error.
    fn read_boxed_slice(&mut self, len: usize) -> Result<Box<[u8]>> {
        let mut buf = vec![0u8; len];
        let actual_len = self.read_buf(&mut buf)?;
        buf.truncate(actual_len);
        Ok(buf.into_boxed_slice())
    }

    /// Reads exactly the number of bytes requested, and returns a boxed slice of the data or an
    /// error.
    fn read_boxed_slice_exact(&mut self, len: usize) -> Result<Box<[u8]>> {
        let mut buf = vec![0u8; len];
        self.read_buf_exact(&mut buf)?;
        Ok(buf.into_boxed_slice())
    }

    /// Reads bytes from the stream into a supplied buffer until a byte pattern is matched. Returns
    /// a mutable slice to the valid region of the provided buffer.
    #[inline(always)]
    fn scan_bytes<'a>(&mut self, pattern: &[u8], buf: &'a mut [u8]) -> Result<&'a mut [u8]> {
        self.scan_bytes_aligned(pattern, 1, buf)
    }

    /// Reads bytes from a stream into a supplied buffer until a byte patter is matched on an
    /// aligned byte boundary. Returns a mutable slice to the valid region of the provided buffer.
    fn scan_bytes_aligned<'a>(
        &mut self,
        pattern: &[u8],
        align: usize,
        buf: &'a mut [u8],
    ) -> Result<&'a mut [u8]>;

    /// Ignores the specified number of bytes from the stream or returns an error.
    fn ignore_bytes(&mut self, count: u64) -> Result<()>;

    /// Gets the position of the stream.
    fn pos(&self) -> u64;
}

impl<'b, R: ReadBytes> ReadBytes for &'b mut R {
    #[inline(always)]
    fn read_byte(&mut self) -> Result<u8> {
        (*self).read_byte()
    }

    #[inline(always)]
    fn read_double_bytes(&mut self) -> Result<[u8; 2]> {
        (*self).read_double_bytes()
    }

    #[inline(always)]
    fn read_triple_bytes(&mut self) -> Result<[u8; 3]> {
        (*self).read_triple_bytes()
    }

    #[inline(always)]
    fn read_quad_bytes(&mut self) -> Result<[u8; 4]> {
        (*self).read_quad_bytes()
    }

    #[inline(always)]
    fn read_buf(&mut self, buf: &mut [u8]) -> Result<usize> {
        (*self).read_buf(buf)
    }

    #[inline(always)]
    fn read_buf_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        (*self).read_buf_exact(buf)
    }

    #[inline(always)]
    fn scan_bytes_aligned<'a>(
        &mut self,
        pattern: &[u8],
        align: usize,
        buf: &'a mut [u8],
    ) -> Result<&'a mut [u8]> {
        (*self).scan_bytes_aligned(pattern, align, buf)
    }

    #[inline(always)]
    fn ignore_bytes(&mut self, count: u64) -> Result<()> {
        (*self).ignore_bytes(count)
    }

    #[inline(always)]
    fn pos(&self) -> u64 {
        (**self).pos()
    }
}

impl<'b, S: SeekBuffered> SeekBuffered for &'b mut S {
    fn ensure_seekback_buffer(&mut self, len: usize) {
        (*self).ensure_seekback_buffer(len)
    }

    fn unread_buffer_len(&self) -> usize {
        (**self).unread_buffer_len()
    }

    fn read_buffer_len(&self) -> usize {
        (**self).read_buffer_len()
    }

    fn seek_buffered(&mut self, pos: u64) -> u64 {
        (*self).seek_buffered(pos)
    }

    fn seek_buffered_rel(&mut self, delta: isize) -> u64 {
        (*self).seek_buffered_rel(delta)
    }
}

/// `SeekBuffered` provides methods to seek within the buffered portion of a stream.
pub trait SeekBuffered {
    /// Ensures that `len` bytes will be available for backwards seeking if `len` bytes have been
    /// previously read.
    fn ensure_seekback_buffer(&mut self, len: usize);

    /// Get the number of bytes buffered but not yet read.
    ///
    /// Note: This is the maximum number of bytes that can be seeked forwards within the buffer.
    fn unread_buffer_len(&self) -> usize;

    /// Gets the number of bytes buffered and read.
    ///
    /// Note: This is the maximum number of bytes that can be seeked backwards within the buffer.
    fn read_buffer_len(&self) -> usize;

    /// Seek within the buffered data to an absolute position in the stream. Returns the position
    /// seeked to.
    fn seek_buffered(&mut self, pos: u64) -> u64;

    /// Seek within the buffered data relative to the current position in the stream. Returns the
    /// position seeked to.
    ///
    /// The range of `delta` is clamped to the inclusive range defined by
    /// `-read_buffer_len()..=unread_buffer_len()`.
    fn seek_buffered_rel(&mut self, delta: isize) -> u64;

    /// Seek backwards within the buffered data.
    ///
    /// This function is identical to [`SeekBuffered::seek_buffered_rel`] when a negative delta is
    /// provided.
    fn seek_buffered_rev(&mut self, delta: usize) {
        assert!(delta < core::isize::MAX as usize);
        self.seek_buffered_rel(-(delta as isize));
    }
}

impl<'b, F: FiniteStream> FiniteStream for &'b mut F {
    fn byte_len(&self) -> u64 {
        (**self).byte_len()
    }

    fn bytes_read(&self) -> u64 {
        (**self).bytes_read()
    }

    fn bytes_available(&self) -> u64 {
        (**self).bytes_available()
    }
}

/// A `FiniteStream` is a stream that has a known length in bytes.
pub trait FiniteStream {
    /// Returns the length of the the stream in bytes.
    fn byte_len(&self) -> u64;

    /// Returns the number of bytes that have been read.
    fn bytes_read(&self) -> u64;

    /// Returns the number of bytes available for reading.
    fn bytes_available(&self) -> u64;
}

#[cfg(not(feature = "std"))]
mod no_std_compat {
    use core::ops::{Deref, DerefMut};

    pub struct IoSliceMut<'a> {
        buf: &'a mut [u8],
    }

    impl <'a> IoSliceMut<'a> {
        pub fn new(buf: &'a mut [u8]) -> IoSliceMut<'a> {
            IoSliceMut {
                buf
            }
        }
    }

    impl<'a> Deref for IoSliceMut<'a> {
        type Target = [u8];

        #[inline]
        fn deref(&self) -> &[u8] {
            self.buf
        }
    }

    impl<'a> DerefMut for IoSliceMut<'a> {
        #[inline]
        fn deref_mut(&mut self) -> &mut [u8] {
            self.buf
        }
    }
}