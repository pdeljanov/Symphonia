// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::error;
use core::fmt;

/// `MediaErrorKind` classifies a [`MediaError`].
///
/// The kind is the only part of an error that the reading stack or its consumers depend on for
/// control flow, so it must be retained when an error crosses the I/O boundary.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaErrorKind {
    /// The stream was exhausted. The read extended past the end of the source, or past the bound of
    /// a scoped stream.
    Eof,
    /// The I/O operation was interrupted and may be retried.
    Interrupted,
    /// The end of a read-ahead buffer or bitstream was reached.
    EndOfBitstream,
    /// A generic I/O error with a static message.
    Message(&'static str),
}

impl MediaErrorKind {
    fn as_str(&self) -> &'static str {
        match self {
            MediaErrorKind::Eof => "unexpected end of stream",
            MediaErrorKind::Interrupted => "I/O operation interrupted",
            MediaErrorKind::EndOfBitstream => "unexpected end of bitstream",
            MediaErrorKind::Message(msg) => msg,
        }
    }
}

/// `MediaError` is the error type used by all Symphonia I/O readers and streams.
///
/// It is the I/O equivalent of `std::io::Error` without the allocation requirement. The I/O error
/// kinds consumed by the reading stack are preserved exactly when an error is converted to or from
/// `std::io::Error` (see `From<MediaError> for std::io::Error` and the `MediaSource` adapters).
#[derive(Debug)]
pub struct MediaError {
    kind: MediaErrorKind,
    message: Option<&'static str>,
}

impl MediaError {
    /// The stream was exhausted.
    pub const fn eof() -> Self {
        MediaError { kind: MediaErrorKind::Eof, message: None }
    }

    /// The stream was exhausted, with an additional static message describing the boundary that was
    /// exceeded.
    pub(crate) const fn eof_message(message: &'static str) -> Self {
        MediaError { kind: MediaErrorKind::Eof, message: Some(message) }
    }

    /// The end of a read-ahead buffer or bitstream was reached.
    pub const fn end_of_bitstream() -> Self {
        MediaError { kind: MediaErrorKind::EndOfBitstream, message: None }
    }

    /// A generic I/O error with the given message.
    pub const fn message(msg: &'static str) -> Self {
        MediaError { kind: MediaErrorKind::Message(msg), message: None }
    }

    /// The I/O operation was interrupted and may be retried.
    pub const fn interrupted() -> Self {
        MediaError { kind: MediaErrorKind::Interrupted, message: None }
    }

    /// Returns the kind of this error.
    pub fn kind(&self) -> MediaErrorKind {
        self.kind
    }

    /// Returns the static message of this error, if one was provided.
    pub fn as_message(&self) -> Option<&'static str> {
        self.message
    }
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.message {
            Some(msg) => f.write_str(msg),
            None => f.write_str(self.kind.as_str()),
        }
    }
}

impl error::Error for MediaError {}

/// `MediaResult` is a convenience type alias for `Result<T, MediaError>`.
pub type MediaResult<T> = Result<T, MediaError>;

#[cfg(feature = "std")]
impl From<MediaError> for std::io::Error {
    /// Converts a [`MediaError`] into a `std::io::Error`, preserving the I/O error kind so that
    /// callers matching on `std::io::ErrorKind` (e.g., `ErrorKind::UnexpectedEof`) continue to
    /// work.
    fn from(err: MediaError) -> Self {
        match (err.kind, err.message) {
            (MediaErrorKind::Eof, Some(msg)) => {
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, msg)
            }
            (MediaErrorKind::Eof, None) => std::io::Error::from(std::io::ErrorKind::UnexpectedEof),
            (MediaErrorKind::EndOfBitstream, Some(msg)) => std::io::Error::other(msg),
            (MediaErrorKind::EndOfBitstream, None) => {
                std::io::Error::other("unexpected end of bitstream")
            }
            (MediaErrorKind::Interrupted, _) => {
                std::io::Error::from(std::io::ErrorKind::Interrupted)
            }
            (MediaErrorKind::Message(msg), _) => std::io::Error::other(msg),
        }
    }
}

#[cfg(feature = "std")]
impl From<std::io::Error> for MediaError {
    /// Converts a `std::io::Error` into a [`MediaError`]. Only the kinds meaningful to the reading
    /// stack are remapped; all other kinds collapse to a generic message error. The source message
    /// is dropped since a `MediaError` cannot own an allocation.
    fn from(err: std::io::Error) -> Self {
        match err.kind() {
            std::io::ErrorKind::UnexpectedEof => MediaError::eof(),
            std::io::ErrorKind::Interrupted => MediaError::interrupted(),
            std::io::ErrorKind::Other => MediaError::end_of_bitstream(),
            _ => MediaError::message("I/O error"),
        }
    }
}
