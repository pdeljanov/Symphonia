use crate::silk::error::Error;
use std::convert::TryFrom;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Inactive,
    Voiced,
    Unvoiced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantizationOffsetType {
    Low,
    High,
}

pub struct Frame {
    pub frame_type: FrameType,
    pub quantization_offset_type: QuantizationOffsetType,
    pub gains: Vec<f32>,
    pub nlsf: Vec<f32>,
    pub pitch_lags: Vec<u16>,
    pub ltp_filter: Vec<f32>,
    pub excitation: Vec<f32>,
    pub sample_count: usize,
}

impl Frame {
    pub fn new(
        frame_type: FrameType,
        quantization_offset_type: QuantizationOffsetType,
        sample_count: usize,
    ) -> Self {
        return Self {
            frame_type,
            quantization_offset_type,
            gains: Vec::new(),
            nlsf: Vec::new(),
            pitch_lags: Vec::new(),
            ltp_filter: Vec::new(),
            excitation: Vec::new(),
            sample_count,
        }
    }

    pub fn set_gains(&mut self, gains: &[f32]) {
       return self.gains = gains.to_vec();
    }

    pub fn set_nlsf(&mut self, nlsf: &[f32]) {
        return self.nlsf = nlsf.to_vec();
    }

    pub fn set_pitch_lags(&mut self, pitch_lags: &[u16]) {
        return self.pitch_lags = pitch_lags.to_vec();
    }

    pub fn set_ltp_filter(&mut self, ltp_filter: Vec<f32>) {
        self.ltp_filter = ltp_filter;
    }

    pub fn set_excitation(&mut self, excitation: Vec<f32>) {
        self.excitation = excitation;
    }
}

impl TryFrom<u8> for FrameType {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
       return match value {
            0 | 1 => Ok(FrameType::Inactive),
            2 | 3 => Ok(FrameType::Unvoiced),
            4 | 5 => Ok(FrameType::Voiced),
            _ => Err(Error::InvalidFrameType),
        }
    }
}

impl TryFrom<u8> for QuantizationOffsetType {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        return match value {
            0 | 2 | 4 => Ok(QuantizationOffsetType::Low),
            1 | 3 | 5 => Ok(QuantizationOffsetType::High),
            _ => Err(Error::InvalidQuantizationOffsetType),
        }
    }
}