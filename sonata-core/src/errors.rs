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

use std::fmt;
use std::error;
use std::result;
use std::io;

/// `Error` provides an enumeration of all possible errors reported by Sonata.
#[derive(Debug)]
pub enum Error {
    /// An IO error occured while reading, writing, or seeking the stream.
    IoError(std::io::Error),
    /// The stream contained malformed data and could not be decoded or demuxed.
    DecodeError(&'static str),
    /// An unsupported container or codec feature was encounted.
    Unsupported(&'static str),
    /// A default or user-defined limit was reached while decoding or demuxing the stream. Limits
    /// are used to prevent denial-of-service attacks from malicious streams.
    LimitError(&'static str, usize),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::IoError(ref err) => err.fmt(f),
            Error::DecodeError(msg) => {
                f.write_str("Malformed stream encountered: ")?;
                f.write_str(msg)
            },
            Error::Unsupported(feature) => {
                f.write_str("Unsupported feature encountered: ")?;
                f.write_str(feature)
            },
            Error::LimitError(constraint, limit) => {
                f.write_fmt(format_args!("Limit reached: {} ({})", constraint, limit))
            },
        }
    }
}

impl std::error::Error for Error {
    fn cause(&self) -> Option<&error::Error> {
        match *self {
            Error::IoError(ref err) => Some(err),
            Error::DecodeError(_) => None,
            Error::Unsupported(_) => None,
            Error::LimitError(_, _) => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Error {
        Error::IoError(err)
    }
}

pub type Result<T> = result::Result<T, Error>;

/// Convenience function to create a decode error.
pub fn decode_error<T>(desc: &'static str) -> Result<T> {
    Err(Error::DecodeError(desc))
}

/// Convenience function to create an unsupport feature error.
pub fn unsupported_error<T>(feature: &'static str) -> Result<T> {
    Err(Error::Unsupported(feature))
}

/// Convenience function to create a limit error.
pub fn limit_error<T>(constraint: &'static str, limit: usize) -> Result<T> {
    Err(Error::LimitError(constraint, limit))
}
