pub struct Frame {
    pub vad_flag: bool,
    pub quantization_offset_type: QuantizationOffsetType,
    pub frame_type: FrameType,
    pub gains: Vec<f32>,
    pub nlsf: Vec<f32>,
    pub pitch_lags: Vec<u16>,
    pub ltp_filter: Vec<f32>,
    pub excitation: Vec<f32>,
    pub sample_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantizationOffsetType {
    Low,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Inactive,
    Voiced,
    Unvoiced,
}