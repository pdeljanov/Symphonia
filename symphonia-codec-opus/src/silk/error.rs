use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Invalid frame type")]
    InvalidFrameType,

    #[error("Invalid quantization offset type")]
    InvalidQuantizationOffsetType,

    #[error("Invalid LSF coefficients")]
    InvalidLSFCoefficients,

    #[error("Decoding error: {0}")]
    DecodingError(String),

    #[error("Buffer too small")]
    BufferTooSmall,

    #[error("Unsupported SILK configuration")]
    UnsupportedConfig,
}


impl From<crate::silk::decoder::Error> for symphonia_core::errors::Error {
    fn from(err: crate::silk::decoder::Error) -> Self {
        return symphonia_core::errors::Error::DecodeError(err.to_string().leak());
    }
}