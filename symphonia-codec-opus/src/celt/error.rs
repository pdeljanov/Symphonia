//! CELT Layer Error Types

use symphonia_core::errors::Error as SymphoniaError;
use thiserror::Error;

/// Errors that can occur during CELT frame decoding and synthesis.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("opus celt: unsupported frame duration for CELT mode")]
    UnsupportedFrameDuration,

    #[error("opus celt: invalid band range ({start}..{end})")]
    InvalidBandRange { start: usize, end: usize },

    #[error("opus celt: invalid channel count ({0})")]
    InvalidChannelCount(usize),

    #[error("opus celt: invalid post-filter period ({0})")]
    InvalidPostFilterPeriod(usize),

    #[error("opus celt: invalid PVQ pulse count or vector dimension")]
    InvalidPvqParameters,

    #[error("opus celt: invalid IMDCT block size ({0})")]
    InvalidMdctSize(usize),

    #[error("opus celt: output buffer too small")]
    BufferTooSmall,
}

impl From<Error> for SymphoniaError {
    fn from(err: Error) -> Self {
        return match err {
            Error::UnsupportedFrameDuration => {
                SymphoniaError::DecodeError("opus celt: unsupported frame duration for CELT mode")
            }
            Error::InvalidBandRange { .. } => {
                SymphoniaError::DecodeError("opus celt: invalid band range")
            }
            Error::InvalidChannelCount(_) => {
                SymphoniaError::DecodeError("opus celt: invalid channel count")
            }
            Error::InvalidPostFilterPeriod(_) => {
                SymphoniaError::DecodeError("opus celt: invalid post-filter period")
            }
            Error::InvalidPvqParameters => {
                SymphoniaError::DecodeError("opus celt: invalid PVQ parameters")
            }
            Error::InvalidMdctSize(_) => {
                SymphoniaError::DecodeError("opus celt: invalid IMDCT block size")
            }
            Error::BufferTooSmall => {
                SymphoniaError::DecodeError("opus celt: output buffer too small")
            }
        };
    }
}
