//! Contains utility types and functions copied mostly from std/core

#[cfg(not(feature = "std"))]
pub use io_slice::IoSliceMut;
#[cfg(feature = "std")]
pub use std::io::IoSliceMut;

use alloc::vec::Vec;
use core::{
    fmt::{Debug, Formatter},
    mem::MaybeUninit,
};

use super::{BufRead, Error, ErrorKind, ErrorType, Read, ReadExactError, Result, Seek, SeekFrom};

#[cfg(not(feature = "std"))]
mod io_slice {
    use core::ops::{Deref, DerefMut};

    #[derive(Debug, PartialEq, Eq)]
    pub struct IoSliceMut<'a>(&'a mut [u8]);

    impl<'a> IoSliceMut<'a> {
        pub(crate) fn new(buf: &'a mut [u8]) -> Self {
            Self(buf)
        }
    }

    impl<'a> Deref for IoSliceMut<'a> {
        type Target = [u8];

        #[inline]
        fn deref(&self) -> &[u8] {
            self.0
        }
    }

    impl<'a> DerefMut for IoSliceMut<'a> {
        #[inline]
        fn deref_mut(&mut self) -> &mut [u8] {
            self.0
        }
    }
}

pub(crate) fn default_read_vectored<F>(read: F, bufs: &mut [IoSliceMut<'_>]) -> Result<usize>
where
    F: FnOnce(&mut [u8]) -> Result<usize>,
{
    let buf = bufs.iter_mut().find(|b| !b.is_empty()).map_or(&mut [][..], |b| &mut **b);
    read(buf)
}

#[derive(Debug)]
pub struct Cursor<T> {
    inner: T,
    pos: u64,
}

impl<T> Cursor<T> {
    pub const fn new(inner: T) -> Self {
        Self { inner, pos: 0 }
    }

    pub fn into_inner(self) -> T {
        self.inner
    }

    pub const fn get_ref(&self) -> &T {
        &self.inner
    }

    pub const fn get_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    pub const fn position(&self) -> u64 {
        self.pos
    }

    pub const fn set_position(&mut self, pos: u64) {
        self.pos = pos;
    }
}

impl<T> Cursor<T>
where
    T: AsRef<[u8]>,
{
    pub fn split(&self) -> (&[u8], &[u8]) {
        let slice = self.inner.as_ref();
        let pos = self.pos.min(slice.len() as u64);
        slice.split_at(pos as usize)
    }
}

impl<T> Clone for Cursor<T>
where
    T: Clone,
{
    #[inline]
    fn clone(&self) -> Self {
        Cursor { inner: self.inner.clone(), pos: self.pos }
    }

    #[inline]
    fn clone_from(&mut self, other: &Self) {
        self.inner.clone_from(&other.inner);
        self.pos = other.pos;
    }
}

impl<T> ErrorType for Cursor<T> {
    type Error = Error;
}

impl<T> Seek for Cursor<T>
where
    T: AsRef<[u8]>,
{
    fn seek(&mut self, style: SeekFrom) -> Result<u64> {
        let (base_pos, offset) = match style {
            SeekFrom::Start(n) => {
                self.pos = n;
                return Ok(n);
            }
            SeekFrom::End(n) => (self.inner.as_ref().len() as u64, n),
            SeekFrom::Current(n) => (self.pos, n),
        };
        match base_pos.checked_add_signed(offset) {
            Some(n) => {
                self.pos = n;
                Ok(self.pos)
            }
            None => Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid seek to a negative or overflowing position",
            )),
        }
    }

    fn rewind(&mut self) -> Result<()> {
        self.pos = 0;
        Ok(())
    }

    fn stream_position(&mut self) -> Result<u64> {
        Ok(self.pos)
    }
}

impl<T> Read for Cursor<T>
where
    T: AsRef<[u8]>,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let Ok(n) = Read::read(&mut Cursor::split(self).1, buf).map_err(|e| match e {});
        self.pos += n as u64;
        Ok(n)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> core::result::Result<(), ReadExactError<Error>> {
        let result = Read::read_exact(&mut Cursor::split(self).1, buf);

        match result {
            Ok(_) => self.pos += buf.len() as u64,
            // The only possible error condition is EOF, so place the cursor at "EOF"
            Err(_) => self.pos = self.inner.as_ref().len() as u64,
        }

        result.map_err(|e| match e {
            embedded_io::ReadExactError::UnexpectedEof => {
                embedded_io::ReadExactError::UnexpectedEof
            }
        })
    }
}

impl<T> BufRead for Cursor<T>
where
    T: AsRef<[u8]>,
{
    fn fill_buf(&mut self) -> Result<&[u8]> {
        Ok(Cursor::split(self).1)
    }

    fn consume(&mut self, amt: usize) {
        self.pos += amt as u64;
    }
}

/// A borrowed byte buffer which is incrementally filled and initialized.
///
/// This type is a sort of "double cursor". It tracks three regions in the buffer: a region at the beginning of the
/// buffer that has been logically filled with data, a region that has been initialized at some point but not yet
/// logically filled, and a region at the end that is fully uninitialized. The filled region is guaranteed to be a
/// subset of the initialized region.
///
/// In summary, the contents of the buffer can be visualized as:
/// ```not_rust
/// [             capacity              ]
/// [ filled |         unfilled         ]
/// [    initialized    | uninitialized ]
/// ```
///
/// A `BorrowedBuf` is created around some existing data (or capacity for data) via a unique reference
/// (`&mut`). The `BorrowedBuf` can be configured (e.g., using `clear` or `set_init`), but cannot be
/// directly written. To write into the buffer, use `unfilled` to create a `BorrowedCursor`. The cursor
/// has write-only access to the unfilled portion of the buffer (you can think of it as a
/// write-only iterator).
///
/// The lifetime `'data` is a bound on the lifetime of the underlying data.
pub struct BorrowedBuf<'data> {
    /// The buffer's underlying data.
    buf: &'data mut [MaybeUninit<u8>],
    /// The length of `self.buf` which is known to be filled.
    filled: usize,
    /// The length of `self.buf` which is known to be initialized.
    init: usize,
}

impl Debug for BorrowedBuf<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BorrowedBuf")
            .field("init", &self.init)
            .field("filled", &self.filled)
            .field("capacity", &self.capacity())
            .finish()
    }
}

/// Creates a new `BorrowedBuf` from a fully initialized slice.
impl<'data> From<&'data mut [u8]> for BorrowedBuf<'data> {
    #[inline]
    fn from(slice: &'data mut [u8]) -> BorrowedBuf<'data> {
        let len = slice.len();

        BorrowedBuf {
            // SAFETY: initialized data never becoming uninitialized is an invariant of BorrowedBuf
            buf: unsafe {
                core::mem::transmute::<*mut [u8], *mut [core::mem::MaybeUninit<u8>]>(
                    slice as *mut [u8],
                )
                .as_mut()
                .unwrap()
            },
            filled: 0,
            init: len,
        }
    }
}

/// Creates a new `BorrowedBuf` from an uninitialized buffer.
///
/// Use `set_init` if part of the buffer is known to be already initialized.
impl<'data> From<&'data mut [MaybeUninit<u8>]> for BorrowedBuf<'data> {
    #[inline]
    fn from(buf: &'data mut [MaybeUninit<u8>]) -> BorrowedBuf<'data> {
        BorrowedBuf { buf, filled: 0, init: 0 }
    }
}

/// Creates a new `BorrowedBuf` from a cursor.
///
/// Use `BorrowedCursor::with_unfilled_buf` instead for a safer alternative.
impl<'data> From<BorrowedCursor<'data>> for BorrowedBuf<'data> {
    #[inline]
    fn from(mut buf: BorrowedCursor<'data>) -> BorrowedBuf<'data> {
        let init = buf.init_mut().len();
        BorrowedBuf {
            // SAFETY: no initialized byte is ever uninitialized as per
            // `BorrowedBuf`'s invariant
            buf: unsafe { buf.buf.buf.get_unchecked_mut(buf.buf.filled..) },
            filled: 0,
            init,
        }
    }
}

impl<'data> BorrowedBuf<'data> {
    /// Returns the total capacity of the buffer.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Returns the length of the filled part of the buffer.
    #[inline]
    pub fn len(&self) -> usize {
        self.filled
    }

    /// Returns the length of the initialized part of the buffer.
    #[inline]
    pub fn init_len(&self) -> usize {
        self.init
    }

    /// Returns a shared reference to the filled portion of the buffer.
    #[inline]
    pub fn filled(&self) -> &[u8] {
        // SAFETY: We only slice the filled part of the buffer, which is always valid
        unsafe {
            let buf = self.buf.get_unchecked(..self.filled);
            buf.assume_init_ref()
        }
    }

    /// Returns a mutable reference to the filled portion of the buffer.
    #[inline]
    pub fn filled_mut(&mut self) -> &mut [u8] {
        // SAFETY: We only slice the filled part of the buffer, which is always valid
        unsafe {
            let buf = self.buf.get_unchecked_mut(..self.filled);
            buf.assume_init_mut()
        }
    }

    /// Returns a shared reference to the filled portion of the buffer with its original lifetime.
    #[inline]
    pub fn into_filled(self) -> &'data [u8] {
        // SAFETY: We only slice the filled part of the buffer, which is always valid
        unsafe {
            let buf = self.buf.get_unchecked(..self.filled);
            buf.assume_init_ref()
        }
    }

    /// Returns a mutable reference to the filled portion of the buffer with its original lifetime.
    #[inline]
    pub fn into_filled_mut(self) -> &'data mut [u8] {
        // SAFETY: We only slice the filled part of the buffer, which is always valid
        unsafe {
            let buf = self.buf.get_unchecked_mut(..self.filled);
            buf.assume_init_mut()
        }
    }

    /// Returns a cursor over the unfilled part of the buffer.
    #[inline]
    pub fn unfilled<'this>(&'this mut self) -> BorrowedCursor<'this> {
        BorrowedCursor {
            // SAFETY: we never assign into `BorrowedCursor::buf`, so treating its
            // lifetime covariantly is safe.
            buf: unsafe {
                core::mem::transmute::<&'this mut BorrowedBuf<'data>, &'this mut BorrowedBuf<'this>>(
                    self,
                )
            },
        }
    }

    /// Clears the buffer, resetting the filled region to empty.
    ///
    /// The number of initialized bytes is not changed, and the contents of the buffer are not modified.
    #[inline]
    pub fn clear(&mut self) -> &mut Self {
        self.filled = 0;
        self
    }

    /// Asserts that the first `n` bytes of the buffer are initialized.
    ///
    /// `BorrowedBuf` assumes that bytes are never de-initialized, so this method does nothing when called with fewer
    /// bytes than are already known to be initialized.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the first `n` unfilled bytes of the buffer have already been initialized.
    #[inline]
    pub unsafe fn set_init(&mut self, n: usize) -> &mut Self {
        self.init = core::cmp::max(self.init, n);
        self
    }
}

/// A writeable view of the unfilled portion of a [`BorrowedBuf`].
///
/// The unfilled portion consists of an initialized and an uninitialized part; see [`BorrowedBuf`]
/// for details.
///
/// Data can be written directly to the cursor by using [`append`](BorrowedCursor::append) or
/// indirectly by getting a slice of part or all of the cursor and writing into the slice. In the
/// indirect case, the caller must call [`advance`](BorrowedCursor::advance) after writing to inform
/// the cursor how many bytes have been written.
///
/// Once data is written to the cursor, it becomes part of the filled portion of the underlying
/// `BorrowedBuf` and can no longer be accessed or re-written by the cursor. I.e., the cursor tracks
/// the unfilled part of the underlying `BorrowedBuf`.
///
/// The lifetime `'a` is a bound on the lifetime of the underlying buffer (which means it is a bound
/// on the data in that buffer by transitivity).
#[derive(Debug)]
pub struct BorrowedCursor<'a> {
    /// The underlying buffer.
    // Safety invariant: we treat the type of buf as covariant in the lifetime of `BorrowedBuf` when
    // we create a `BorrowedCursor`. This is only safe if we never replace `buf` by assigning into
    // it, so don't do that!
    buf: &'a mut BorrowedBuf<'a>,
}

impl<'a> BorrowedCursor<'a> {
    /// Reborrows this cursor by cloning it with a smaller lifetime.
    ///
    /// Since a cursor maintains unique access to its underlying buffer, the borrowed cursor is
    /// not accessible while the new cursor exists.
    #[inline]
    pub fn reborrow<'this>(&'this mut self) -> BorrowedCursor<'this> {
        BorrowedCursor {
            // SAFETY: we never assign into `BorrowedCursor::buf`, so treating its
            // lifetime covariantly is safe.
            buf: unsafe {
                core::mem::transmute::<&'this mut BorrowedBuf<'a>, &'this mut BorrowedBuf<'this>>(
                    self.buf,
                )
            },
        }
    }

    /// Returns the available space in the cursor.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.buf.capacity() - self.buf.filled
    }

    /// Returns the number of bytes written to the `BorrowedBuf` this cursor was created from.
    ///
    /// In particular, the count returned is shared by all reborrows of the cursor.
    #[inline]
    pub fn written(&self) -> usize {
        self.buf.filled
    }

    /// Returns a mutable reference to the initialized portion of the cursor.
    #[inline]
    pub fn init_mut(&mut self) -> &mut [u8] {
        // SAFETY: We only slice the initialized part of the buffer, which is always valid
        unsafe {
            let buf = self.buf.buf.get_unchecked_mut(self.buf.filled..self.buf.init);
            buf.assume_init_mut()
        }
    }

    /// Returns a mutable reference to the whole cursor.
    ///
    /// # Safety
    ///
    /// The caller must not uninitialize any bytes in the initialized portion of the cursor.
    #[inline]
    pub unsafe fn as_mut(&mut self) -> &mut [MaybeUninit<u8>] {
        // SAFETY: always in bounds
        unsafe { self.buf.buf.get_unchecked_mut(self.buf.filled..) }
    }

    /// Advances the cursor by asserting that `n` bytes have been filled.
    ///
    /// After advancing, the `n` bytes are no longer accessible via the cursor and can only be
    /// accessed via the underlying buffer. I.e., the buffer's filled portion grows by `n` elements
    /// and its unfilled portion (and the capacity of this cursor) shrinks by `n` elements.
    ///
    /// If less than `n` bytes initialized (by the cursor's point of view), `set_init` should be
    /// called first.
    ///
    /// # Panics
    ///
    /// Panics if there are less than `n` bytes initialized.
    #[inline]
    pub fn advance(&mut self, n: usize) -> &mut Self {
        // The subtraction cannot underflow by invariant of this type.
        assert!(n <= self.buf.init - self.buf.filled);

        self.buf.filled += n;
        self
    }

    /// Advances the cursor by asserting that `n` bytes have been filled.
    ///
    /// After advancing, the `n` bytes are no longer accessible via the cursor and can only be
    /// accessed via the underlying buffer. I.e., the buffer's filled portion grows by `n` elements
    /// and its unfilled portion (and the capacity of this cursor) shrinks by `n` elements.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the first `n` bytes of the cursor have been properly
    /// initialised.
    #[inline]
    pub unsafe fn advance_unchecked(&mut self, n: usize) -> &mut Self {
        self.buf.filled += n;
        self.buf.init = core::cmp::max(self.buf.init, self.buf.filled);
        self
    }

    /// Initializes all bytes in the cursor.
    #[inline]
    pub fn ensure_init(&mut self) -> &mut Self {
        // SAFETY: always in bounds and we never uninitialize these bytes.
        let uninit = unsafe { self.buf.buf.get_unchecked_mut(self.buf.init..) };

        // SAFETY: 0 is a valid value for MaybeUninit<u8> and the length matches the allocation
        // since it is comes from a slice reference.
        unsafe {
            core::ptr::write_bytes(uninit.as_mut_ptr(), 0, uninit.len());
        }
        self.buf.init = self.buf.capacity();

        self
    }

    /// Asserts that the first `n` unfilled bytes of the cursor are initialized.
    ///
    /// `BorrowedBuf` assumes that bytes are never de-initialized, so this method does nothing when
    /// called with fewer bytes than are already known to be initialized.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the first `n` bytes of the buffer have already been initialized.
    #[inline]
    pub unsafe fn set_init(&mut self, n: usize) -> &mut Self {
        self.buf.init = core::cmp::max(self.buf.init, self.buf.filled + n);
        self
    }

    /// Appends data to the cursor, advancing position within its buffer.
    ///
    /// # Panics
    ///
    /// Panics if `self.capacity()` is less than `buf.len()`.
    #[inline]
    pub fn append(&mut self, buf: &[u8]) {
        assert!(self.capacity() >= buf.len());

        // SAFETY: we do not de-initialize any of the elements of the slice
        unsafe {
            self.as_mut()[..buf.len()].write_copy_of_slice(buf);
        }

        // SAFETY: We just added the entire contents of buf to the filled section.
        unsafe {
            self.set_init(buf.len());
        }
        self.buf.filled += buf.len();
    }

    /// Runs the given closure with a `BorrowedBuf` containing the unfilled part
    /// of the cursor.
    ///
    /// This enables inspecting what was written to the cursor.
    ///
    /// # Panics
    ///
    /// Panics if the `BorrowedBuf` given to the closure is replaced by another
    /// one.
    pub fn with_unfilled_buf<T>(&mut self, f: impl FnOnce(&mut BorrowedBuf<'_>) -> T) -> T {
        let mut buf = BorrowedBuf::from(self.reborrow());
        let prev_ptr = buf.buf as *const _;
        let res = f(&mut buf);

        // Check that the caller didn't replace the `BorrowedBuf`.
        // This is necessary for the safety of the code below: if the check wasn't
        // there, one could mark some bytes as initialized even though there aren't.
        assert!(core::ptr::addr_eq(prev_ptr, buf.buf));

        let filled = buf.filled;
        let init = buf.init;

        // Update `init` and `filled` fields with what was written to the buffer.
        // `self.buf.filled` was the starting length of the `BorrowedBuf`.
        //
        // SAFETY: These amounts of bytes were initialized/filled in the `BorrowedBuf`,
        // and therefore they are initialized/filled in the cursor too, because the
        // buffer wasn't replaced.
        self.buf.init = self.buf.filled + init;
        self.buf.filled += filled;

        res
    }
}

// Copied straight outta std with some modifications
// Here we must serve many masters with conflicting goals:
//
// - avoid allocating unless necessary
// - avoid overallocating if we know the exact size (#89165)
// - avoid passing large buffers to readers that always initialize the free capacity if they perform short reads (#23815, #23820)
// - pass large buffers to readers that do not initialize the spare capacity. this can amortize per-call overheads
// - and finally pass not-too-small and not-too-large buffers to Windows read APIs because they manage to suffer from both problems
//   at the same time, i.e. small reads suffer from syscall overhead, all reads incur costs proportional to buffer size (#110650)
//
pub(crate) fn default_read_to_end<R: Read + ?Sized>(
    r: &mut R,
    buf: &mut Vec<u8>,
    size_hint: Option<usize>,
) -> Result<usize>
where
    Error: core::convert::From<<R as embedded_io::ErrorType>::Error>,
{
    let start_len = buf.len();
    let start_cap = buf.capacity();
    // Optionally limit the maximum bytes read on each iteration.
    // This adds an arbitrary fiddle factor to allow for more data than we expect.
    let mut max_read_size =
        size_hint.and_then(|s| s.checked_add(1024)?.checked_next_multiple_of(4096)).unwrap_or(4096);

    let mut initialized = 0; // Extra initialized bytes from previous loop iteration

    const PROBE_SIZE: usize = 32;

    fn small_probe_read<R: Read + ?Sized>(r: &mut R, buf: &mut Vec<u8>) -> Result<usize>
    where
        Error: core::convert::From<<R as embedded_io::ErrorType>::Error>,
    {
        let mut probe = [0u8; PROBE_SIZE];

        loop {
            match r.read(&mut probe) {
                Ok(n) => {
                    // there is no way to recover from allocation failure here
                    // because the data has already been read.
                    buf.extend_from_slice(&probe[..n]);
                    return Ok(n);
                }
                Err(e) => {
                    let e: Error = e.into();
                    if e.kind() == ErrorKind::Interrupted {
                        continue;
                    }

                    return Err(e.into());
                }
            }
        }
    }

    fn read_from_buf<F>(read: F, mut cursor: BorrowedCursor<'_>) -> Result<()>
    where
        F: FnOnce(&mut [u8]) -> Result<usize>,
    {
        let n = read(cursor.ensure_init().init_mut())?;
        cursor.advance(n);
        Ok(())
    }

    // avoid inflating empty/small vecs before we have determined that there's anything to read
    if (size_hint.is_none() || size_hint == Some(0)) && buf.capacity() - buf.len() < PROBE_SIZE {
        let read = small_probe_read(r, buf)?;

        if read == 0 {
            return Ok(0);
        }
    }

    let mut consecutive_short_reads = 0;

    loop {
        if buf.len() == buf.capacity() && buf.capacity() == start_cap {
            // The buffer might be an exact fit. Let's read into a probe buffer
            // and see if it returns `Ok(0)`. If so, we've avoided an
            // unnecessary doubling of the capacity. But if not, append the
            // probe buffer to the primary buffer and let its capacity grow.
            let read = small_probe_read(r, buf)?;

            if read == 0 {
                return Ok(buf.len() - start_len);
            }
        }

        if buf.len() == buf.capacity() {
            // buf is full, need more space
            buf.try_reserve(PROBE_SIZE).map_err(|_| Error::other("failed to grow buf"))?;
        }

        let mut spare = buf.spare_capacity_mut();
        let buf_len = core::cmp::min(spare.len(), max_read_size);
        spare = &mut spare[..buf_len];
        let mut read_buf: BorrowedBuf<'_> = spare.into();

        // SAFETY: These bytes were initialized but not filled in the previous loop
        unsafe {
            read_buf.set_init(initialized);
        }

        let mut cursor = read_buf.unfilled();
        let result = loop {
            match read_from_buf(|b| r.read(b).map_err(Into::into), cursor.reborrow()) {
                Err(e) => {
                    if e.kind() == ErrorKind::Interrupted {
                        continue;
                    }

                    return Err(e.into());
                }
                // Do not stop now in case of error: we might have received both data
                // and an error
                res => break res,
            }
        };

        let unfilled_but_initialized = cursor.init_mut().len();
        let bytes_read = cursor.written();
        let was_fully_initialized = read_buf.init_len() == buf_len;

        // SAFETY: BorrowedBuf's invariants mean this much core::memory is initialized.
        unsafe {
            let new_len = bytes_read + buf.len();
            buf.set_len(new_len);
        }

        // Now that all data is pushed to the vector, we can fail without data loss
        result?;

        if bytes_read == 0 {
            return Ok(buf.len() - start_len);
        }

        if bytes_read < buf_len {
            consecutive_short_reads += 1;
        } else {
            consecutive_short_reads = 0;
        }

        // store how much was initialized but not filled
        initialized = unfilled_but_initialized;

        // Use heuristics to determine the max read size if no initial size hint was provided
        if size_hint.is_none() {
            // The reader is returning short reads but it doesn't call ensure_init().
            // In that case we no longer need to restrict read sizes to avoid
            // initialization costs.
            // When reading from disk we usually don't get any short reads except at EOF.
            // So we wait for at least 2 short reads before uncapping the read buffer;
            // this helps with the Windows issue.
            if !was_fully_initialized && consecutive_short_reads > 1 {
                max_read_size = usize::MAX;
            }

            // we have passed a larger buffer than previously and the
            // reader still hasn't returned a short read
            if buf_len >= max_read_size && bytes_read == buf_len {
                max_read_size = max_read_size.saturating_mul(2);
            }
        }
    }
}
