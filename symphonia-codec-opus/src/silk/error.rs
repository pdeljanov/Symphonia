//! Errors of the SILK layer
//! ([RFC 6716 Section 4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2>)).

use symphonia_core::errors::Error as SymphoniaError;
use thiserror::Error;

/// Errors of SILK decoding; each converts into a decode error carrying its message.
#[derive(Error, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Error {
    /// NLSF parameters or an LPC filter order outside the codebooks (RFC 6716 Section 4.2.7.5).
    #[error("{}", self.message())]
    InvalidLSFCoefficients,
    /// A resampler output buffer shorter than the resampled frame (RFC 6716 Section 4.2.9).
    #[error("{}", self.message())]
    BufferTooSmall,
    /// A pair of internal and output rates the resampler does not support
    /// (RFC 6716 Section 4.2.9).
    #[error("{}", self.message())]
    UnsupportedConfig,
    /// An empty or oversized frame handed to stereo unmixing or resampling
    /// (RFC 6716 Section 4.2.8, Section 4.2.9).
    #[error("{}", self.message())]
    InvalidFrameSize,
    /// A frame without subframes, or whose subframe count disagrees with its layout
    /// (RFC 6716 Section 4.2.2).
    #[error("{}", self.message())]
    MismatchFrameSubframes,
    /// An Opus frame duration without a SILK layer (RFC 6716 Section 4.2.2).
    #[error("{}", self.message())]
    UnsupportedFrameSize,
    /// A shell partition size outside the shell coder (RFC 6716 Section 4.2.7.8.3).
    #[error("{}", self.message())]
    InvalidPartitionSize,
    /// An output plane shorter than the resampled frame (RFC 6716 Section 4.2.9).
    #[error("{}", self.message())]
    BufferOverflow,
    /// More shell blocks or samples than one SILK frame holds (RFC 6716 Section 4.2.7.8).
    #[error("{}", self.message())]
    FrameLengthExceedsMaximum,
}

impl Error {
    /// The message of the error, which is also the static message of the decode error it
    /// converts into.
    const fn message(&self) -> &'static str {
        return match self {
            Self::InvalidLSFCoefficients => "opus silk: invalid LSF coefficients",
            Self::BufferTooSmall => "opus silk: resampler buffer too small",
            Self::UnsupportedConfig => "opus silk: unsupported resampler configuration",
            Self::InvalidFrameSize => "opus silk: invalid frame size",
            Self::MismatchFrameSubframes => "opus silk: frame and subframe count mismatch",
            Self::UnsupportedFrameSize => "opus silk: unsupported frame size",
            Self::InvalidPartitionSize => "opus silk: invalid shell partition size",
            Self::BufferOverflow => "opus silk: output buffer too small",
            Self::FrameLengthExceedsMaximum => "opus silk: frame length exceeds maximum",
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
            Error::InvalidLSFCoefficients,
            Error::BufferTooSmall,
            Error::UnsupportedConfig,
            Error::InvalidFrameSize,
            Error::MismatchFrameSubframes,
            Error::UnsupportedFrameSize,
            Error::InvalidPartitionSize,
            Error::BufferOverflow,
            Error::FrameLengthExceedsMaximum,
        ]
        .map(|error| error.message());
        assert!(messages.iter().all(|message| message.starts_with("opus silk: ")));
        assert!(messages.iter().enumerate().all(|(i, message)| !messages[..i].contains(message)));
    }
}
