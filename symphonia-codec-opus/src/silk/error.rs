use symphonia_core::errors::Error as SymphoniaError;
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

    #[error("Invalid synthesized samples")]
    InvalidSynthesizedSamples,
    
    #[error("Calculation overflow")]
    CalculationOverflow,
    
    #[error("Invalid data")]
    InvalidData,
    
    #[error("Invalid frame size")]
    InvalidFrameSize,
}

impl From<Error> for SymphoniaError {
    fn from(err: Error) -> Self {
        return SymphoniaError::DecodeError(err.to_string().leak());
    }
}