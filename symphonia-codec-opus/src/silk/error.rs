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

    #[error("Invalid periodicity index")]
    InvalidPeriodicityIndex,

    #[error("Invalid LTP scaling index")]
    InvalidLTPScalingIndex,

    #[error("Invalid LBRR frame")]
    InvalidLBRRFrame,

    #[error("Mismatch frame subframes")]
    MismatchFrameSubframes,

    #[error("Unsupported frame size")]
    UnsupportedFrameSize,

    #[error("Invalid partition size")]
    InvalidPartitionSize,

    #[error("Invalid number of partitions")]
    SynthesizedFrameLengthMismatch,

    #[error("Invalid pulse count")]
    InvalidPulseCount,
    
    #[error("Buffer overflow")]
    BufferOverflow,
    
    #[error("Frame length exceeds maximum")]
    FrameLengthExceedsMaximum,
    
    #[error("Frame length exceeds data size")]
    FrameLengthExceedsDataSize,
}

impl From<Error> for SymphoniaError {
    fn from(err: Error) -> Self {
        return SymphoniaError::DecodeError(err.to_string().leak());
    }
}