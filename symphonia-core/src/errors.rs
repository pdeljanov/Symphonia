// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The `errors` module defines the common error type.

use alloc::boxed::Box;
use core::fmt;
use core::fmt::Display;
use core::result;

#[cfg(not(feature = "std"))]
use core::error::Error as StdError;

use core::ops::Deref;
#[cfg(feature = "std")]
use std::error::Error as StdError;

/// `SeekErrorKind` is a list of generic reasons why a seek may fail.
#[derive(Debug)]
pub enum SeekErrorKind {
    /// The stream is not seekable at all.
    Unseekable,
    /// The stream can only be seeked forward.
    ForwardOnly,
    /// The timestamp to seek to is out of range.
    OutOfRange,
    /// The track ID provided is invalid.
    InvalidTrack,
}

impl SeekErrorKind {
    fn as_str(&self) -> &'static str {
        match *self {
            SeekErrorKind::Unseekable => "stream is not seekable",
            SeekErrorKind::ForwardOnly => "stream can only be seeked forward",
            SeekErrorKind::OutOfRange => "requested seek timestamp is out-of-range for stream",
            SeekErrorKind::InvalidTrack => "invalid track id",
        }
    }
}

/// `Error` provides an enumeration of all possible errors reported by Symphonia.
#[derive(Debug)]
pub enum SymphoniaError {
    /// An IO error occurred while reading, writing, or seeking the stream.
    IoError(Box<dyn StdError>),
    /// An IO error occurred while reading, writing, or seeking the stream that is retryable.
    IoInterruptedError(Box<dyn StdError>),
    /// The stream contained malformed data and could not be decoded or demuxed.
    DecodeError(&'static str),
    /// The stream could not be seeked.
    SeekError(SeekErrorKind),
    /// An unsupported container or codec feature was encounted.
    Unsupported(&'static str),
    /// A default or user-defined limit was reached while decoding or demuxing the stream. Limits
    /// are used to prevent denial-of-service attacks from malicious streams.
    LimitError(&'static str),
    /// The demuxer or decoder needs to be reset before continuing.
    ResetRequired,
    EndOfFile,
    Other(&'static str),
}

impl fmt::Display for SymphoniaError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            SymphoniaError::IoError(ref err) => {
                write!(f, "io error {:?}", err)
            }
            SymphoniaError::IoInterruptedError(ref err) => {
                write!(f, "io error {:?}", err)
            }
            SymphoniaError::DecodeError(msg) => {
                write!(f, "malformed stream: {}", msg)
            }
            SymphoniaError::SeekError(ref kind) => {
                write!(f, "seek error: {}", kind.as_str())
            }
            SymphoniaError::Unsupported(feature) => {
                write!(f, "unsupported feature: {}", feature)
            }
            SymphoniaError::LimitError(constraint) => {
                write!(f, "limit reached: {}", constraint)
            }
            SymphoniaError::ResetRequired => {
                write!(f, "decoder needs to be reset")
            }
            SymphoniaError::EndOfFile => {
                write!(f, "unexpected end of file")
            }
            SymphoniaError::Other(msg) => {
                write!(f, "other error: {}", msg)
            }
        }
    }
}

impl StdError for SymphoniaError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match *self {
            SymphoniaError::IoError(ref err) => Some(err.deref()),
            SymphoniaError::IoInterruptedError(ref err) => Some(err.deref()),
            _ => None,
        }
    }
}

#[cfg(feature = "std")]
impl From<std::io::Error> for SymphoniaError {
    fn from(err: std::io::Error) -> SymphoniaError {
        match err.kind() {
            std::io::ErrorKind::Interrupted => SymphoniaError::IoInterruptedError(Box::new(err)),
            std::io::ErrorKind::UnexpectedEof => SymphoniaError::EndOfFile,
            _ => SymphoniaError::IoError(Box::new(err)),
        }
    }
}

pub type Result<T> = result::Result<T, SymphoniaError>;

/// Convenience function to create a decode error.
pub fn decode_error<T>(desc: &'static str) -> Result<T> {
    Err(SymphoniaError::DecodeError(desc))
}

/// Convenience function to create a seek error.
pub fn seek_error<T>(kind: SeekErrorKind) -> Result<T> {
    Err(SymphoniaError::SeekError(kind))
}

/// Convenience function to create an unsupport feature error.
pub fn unsupported_error<T>(feature: &'static str) -> Result<T> {
    Err(SymphoniaError::Unsupported(feature))
}

/// Convenience function to create a limit error.
pub fn limit_error<T>(constraint: &'static str) -> Result<T> {
    Err(SymphoniaError::LimitError(constraint))
}

/// Convenience function to create a reset required error.
pub fn reset_error<T>() -> Result<T> {
    Err(SymphoniaError::ResetRequired)
}

/// Convenience function to create an end-of-stream error.
pub fn end_of_stream_error<T>() -> Result<T> {
    Err(SymphoniaError::EndOfFile)
}
