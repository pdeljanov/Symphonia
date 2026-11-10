//! Errors of the CELT layer
//! ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)).

use symphonia_core::errors::Error as SymphoniaError;
use thiserror::Error;

/// Errors of CELT decoding; each converts into a decode error carrying its message, and the
/// fields keep the offending value for `Debug`.
#[derive(Error, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Error {
    /// A start band outside the coded bands of the frame (RFC 6716 Section 4.3).
    #[error("{}", self.message())]
    InvalidBandRange { start: usize, end: usize },
    /// A coded or output channel count other than `1` or `2` (RFC 6716 Section 4.3).
    #[error("{}", self.message())]
    InvalidChannelCount(usize),
    /// A pulse vector whose pulse count or dimension the codeword tables do not cover
    /// (RFC 6716 Section 4.3.4.2).
    #[error("{}", self.message())]
    InvalidPvqParameters,
    /// A frame size other than the four CELT block sizes (RFC 6716 Section 4.3.7).
    #[error("{}", self.message())]
    InvalidMdctSize(usize),
    /// An output plane shorter than the decoded frame (RFC 6716 Section 4.3.7.2).
    #[error("{}", self.message())]
    BufferTooSmall,
    /// A spreading symbol outside `0..=3` (RFC 6716 Section 4.3.4.3, Table 59).
    #[error("{}", self.message())]
    InvalidSpread(u32),
}

impl Error {
    /// The message of the error, which is also the static message of the decode error it
    /// converts into.
    const fn message(&self) -> &'static str {
        return match self {
            Self::InvalidBandRange { .. } => "opus celt: invalid band range",
            Self::InvalidChannelCount(_) => "opus celt: invalid channel count",
            Self::InvalidPvqParameters => "opus celt: invalid PVQ pulse count or vector dimension",
            Self::InvalidMdctSize(_) => "opus celt: invalid MDCT block size",
            Self::BufferTooSmall => "opus celt: output buffer too small",
            Self::InvalidSpread(_) => "opus celt: invalid spread symbol",
        };
    }
}

impl From<Error> for SymphoniaError {
    fn from(err: Error) -> Self {
        return SymphoniaError::DecodeError(err.message());
    }
}

#[cfg(test)]
impl Error {
    /// Whether `result` failed with this error; the decode error keeps only the message, which
    /// is unique per variant.
    pub(crate) fn matches<T>(&self, result: &Result<T, SymphoniaError>) -> bool {
        return matches!(result, Err(SymphoniaError::DecodeError(message)) if *message == self.message());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every message names the layer and differs from the others, so a decode error identifies
    /// its variant.
    #[test]
    fn messages_are_unique_and_name_the_layer() {
        let messages = [
            Error::InvalidBandRange { start: 0, end: 0 },
            Error::InvalidChannelCount(0),
            Error::InvalidPvqParameters,
            Error::InvalidMdctSize(0),
            Error::BufferTooSmall,
            Error::InvalidSpread(0),
        ]
        .map(|error| error.message());
        assert!(messages.iter().all(|message| message.starts_with("opus celt: ")));
        assert!(messages.iter().enumerate().all(|(i, message)| !messages[..i].contains(message)));
    }
}
