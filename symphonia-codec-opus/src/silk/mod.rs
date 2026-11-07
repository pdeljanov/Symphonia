//! SILK Low-Bitrate Speech Layer
//!
//! This module implements the SILK LP layer of the Opus audio codec (RFC 6716 Section 4.2),
//! decomposed into focused stages:
//! - [`constant`]: Static codebooks, ICDF tables, and filter coefficients.
//! - [`params`]: Header flags, stereo weights, frame type, subframe gains, and LTP parameters.
//! - [`lsf`]: Two-stage Normalized Line Spectral Frequency (NLSF) decoding, stabilization,
//!   subframe interpolation, and NLSF-to-LPC conversion.
//! - [`excitation`]: Rate level, pulse count, shell location, sign, and LCG scaling decoding.
//! - [`synthesis`]: LTP and LPC synthesis filtering.
//! - [`decoder`]: Frame-level SILK decoder state and LBRR/regular frame orchestration.

pub(crate) mod constant;
pub(crate) mod decoder;
pub(crate) mod error;
pub(crate) mod excitation;
pub(crate) mod lsf;
pub(crate) mod params;
pub(crate) mod synthesis;

pub(crate) use decoder::Decoder;

use std::convert::TryFrom;
use symphonia_core::audio::Channels;
use symphonia_core::errors::Result;

use crate::silk::error::Error;
use crate::toc::{Bandwidth, FrameDuration};

/// Maximum number of 20-ms SILK frames inside a single Opus frame (60 ms = 3 x 20 ms).
pub const MAX_SILK_FRAMES: usize = 3;

/// Maximum number of 5-ms subframes inside a single SILK frame (20 ms = 4 x 5 ms).
pub const MAX_SUBFRAMES: usize = 4;

/// Maximum LPC filter order (16 for WB, 10 for NB/MB).
pub const MAX_LPC_ORDER: usize = 16;

/// Number of taps in the voiced Long-Term Prediction (LTP) filter.
pub const LTP_ORDER: usize = 5;

/// Maximum number of samples in a single 5-ms SILK subframe (80 samples at 16 kHz WB).
pub const MAX_SUBFRAME_SAMPLES: usize = 80;

/// Maximum number of samples in a single 20-ms SILK frame (320 samples at 16 kHz WB).
pub const MAX_FRAME_SAMPLES: usize = MAX_SUBFRAMES * MAX_SUBFRAME_SAMPLES;

/// Number of samples in each shell-coding excitation block.
pub const SHELL_BLOCK_SAMPLES: usize = 16;

/// Decoded SILK header flags for a single Opus frame (10, 20, 40, or 60 ms).
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.3>
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.4>
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SilkHeaderFlags {
    pub num_channels: usize,
    pub num_silk_frames: usize,
    pub vad_flags: [[bool; MAX_SILK_FRAMES]; 2],
    pub lbrr_flags: [bool; 2],
    pub per_frame_lbrr: [[bool; MAX_SILK_FRAMES]; 2],
}

impl SilkHeaderFlags {
    /// Returns `true` if any channel has LBRR data enabled in this Opus frame.
    #[inline]
    pub fn has_any_lbrr(&self) -> bool {
        return self.lbrr_flags[0] || (self.num_channels > 1 && self.lbrr_flags[1]);
    }

    /// Returns `true` if SILK sub-frame `frame_idx` (`0..num_silk_frames`) has LBRR data
    /// in either the mid (mono) or side channel.
    #[inline]
    pub fn has_frame_lbrr(&self, frame_idx: usize) -> bool {
        if frame_idx >= self.num_silk_frames {
            return false;
        }

        return self.per_frame_lbrr[0][frame_idx]
            || (self.num_channels > 1 && self.per_frame_lbrr[1][frame_idx]);
    }
}

/// Stereo prediction weights (`w0_Q13`, `w1_Q13`) decoded at the start of a stereo SILK frame.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.1>
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StereoWeights {
    pub w0_q13: i32,
    pub w1_q13: i32,
}

/// Decoded SILK frame pair for one 10 ms or 20 ms SILK interval (Mid/Mono + optional Side).
#[derive(Debug, Default, Clone)]
pub struct SilkChannelFramePair {
    pub stereo_weights: Option<StereoWeights>,
    pub mid_only: bool,
    pub mid: Frame,
    pub side: Option<Frame>,
}

/// Decoded SILK frames (LBRR and regular) for one 10/20/40/60 ms Opus frame.
#[derive(Debug, Default)]
pub struct DecodedOpusFrame {
    pub header: SilkHeaderFlags,
    pub lbrr_frames: Vec<SilkChannelFramePair>,
    pub regular_frames: Vec<SilkChannelFramePair>,
}

/// SILK Frame Conditional Coding Mode (RFC 6716 Section 4.2.7).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CodingMode {
    /// Frame parameters are coded independently (`CODE_INDEPENDENTLY`).
    #[default]
    CodeIndependently,
    /// Frame parameters are coded independently, but LTP scaling is omitted (`CODE_INDEPENDENTLY_NO_LTP_SCALING`).
    CodeIndependentlyNoLtpScaling,
    /// Frame parameters are coded conditionally relative to the previous frame (`CODE_CONDITIONALLY`).
    CodeConditionally,
}

/// Per-channel SILK decoder state maintained across consecutive frames
/// (RFC 6716 Section 4.2.7.4 - 4.2.7.6).
#[derive(Debug, Clone, Copy)]
pub struct ChannelState {
    pub last_gain_index: i32,
    pub prev_nlsf_q15: [i16; MAX_LPC_ORDER],
    pub first_frame_after_reset: bool,
    pub prev_signal_type: SignalType,
    pub prev_lag_index: i16,
}

impl Default for ChannelState {
    fn default() -> Self {
        return Self::new();
    }
}

impl ChannelState {
    pub const fn new() -> Self {
        return Self {
            last_gain_index: 10,
            prev_nlsf_q15: [0; MAX_LPC_ORDER],
            first_frame_after_reset: true,
            prev_signal_type: SignalType::Inactive,
            prev_lag_index: 0,
        };
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn reset_after_mid_only(&mut self) {
        self.last_gain_index = 10;
        self.first_frame_after_reset = true;
        self.prev_signal_type = SignalType::Inactive;
        self.prev_lag_index = 0;
    }
}

/// SILK Decoder State across consecutive frames.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>
#[derive(Debug, Default, Clone)]
pub struct State {
    pub(crate) sample_rate: u32,
    pub(crate) channels: Channels,
    pub(crate) frame_size: FrameDuration,
    pub(crate) bandwidth: Bandwidth,
    pub(crate) prev_frame_type: FrameType,
    pub(crate) lbrr_flag: bool,
    pub(crate) lpc_order: usize,
    pub(crate) channel_states: [ChannelState; 2],
    pub(crate) prev_decode_only_middle: bool,
}

impl State {
    pub fn try_new(channels: Channels, frame_size: FrameDuration, bandwidth: Bandwidth) -> Result<Self> {
        let _ = Self::silk_frame_count(frame_size)?;
        let sample_rate = bandwidth.sample_rate();
        let _ = Self::calculate_frame_length(sample_rate, frame_size)?;
        let lpc_order = LpcOrder::from(bandwidth);

        return Ok(Self {
            sample_rate,
            channels,
            frame_size,
            bandwidth,
            prev_frame_type: FrameType::default(),
            lbrr_flag: false,
            lpc_order,
            channel_states: [ChannelState::new(), ChannelState::new()],
            prev_decode_only_middle: false,
        });
    }

    pub fn reset(&mut self) {
        self.lbrr_flag = false;
        self.prev_frame_type = FrameType::default();
        self.prev_decode_only_middle = false;
        for ch_state in self.channel_states.iter_mut() {
            ch_state.reset();
        }
    }

    /// Returns the number of 10 ms or 20 ms SILK frames contained in an Opus frame of `frame_size`.
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.2>
    pub fn silk_frame_count(frame_size: FrameDuration) -> Result<usize> {
        return match frame_size {
            FrameDuration::Ms10 | FrameDuration::Ms20 => Ok(1),
            FrameDuration::Ms40 => Ok(2),
            FrameDuration::Ms60 => Ok(3),
            FrameDuration::Ms2_5 | FrameDuration::Ms5 => Err(Error::UnsupportedFrameSize.into()),
        };
    }

    /// Returns the duration of each individual SILK frame inside an Opus frame of `frame_size`.
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.2>
    pub fn silk_frame_duration(frame_size: FrameDuration) -> Result<FrameDuration> {
        return match frame_size {
            FrameDuration::Ms10 => Ok(FrameDuration::Ms10),
            FrameDuration::Ms20 | FrameDuration::Ms40 | FrameDuration::Ms60 => Ok(FrameDuration::Ms20),
            FrameDuration::Ms2_5 | FrameDuration::Ms5 => Err(Error::UnsupportedFrameSize.into()),
        };
    }

    pub fn calculate_frame_length(sample_rate: u32, frame_size: FrameDuration) -> Result<usize> {
        let samples = (sample_rate as u128)
            .checked_mul(frame_size.duration().as_nanos())
            .and_then(|ns| ns.checked_div(1_000_000_000))
            .ok_or(Error::CalculationOverflow)?;

        return usize::try_from(samples).map_err(|_| Error::CalculationOverflow.into());
    }
}

/// Decoded SILK Frame with fixed-capacity stack storage for up to 4 subframes.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7>
#[derive(Debug, Clone)]
pub struct Frame {
    pub frame_type: FrameType,
    pub vad_flag: bool,
    pub lbrr_flag: bool,
    pub sample_count: usize,
    pub num_subframes: usize,
    pub lpc_order: usize,
    pub subframes: [Subframe; MAX_SUBFRAMES],
    pub ltp_scale: f32,
    pub ltp_scale_q14: i16,
    pub lsf_interpolation_index: Option<u32>,
    pub stereo_weights: Option<StereoWeights>,
    pub mid_only: bool,
}

impl Default for Frame {
    fn default() -> Self {
        return Self::new(0, 0, MAX_LPC_ORDER);
    }
}

impl Frame {
    pub fn new(sample_count: usize, num_subframes: usize, lpc_order: usize) -> Self {
        return Self {
            frame_type: FrameType::default(),
            vad_flag: false,
            lbrr_flag: false,
            sample_count,
            num_subframes: num_subframes.min(MAX_SUBFRAMES),
            lpc_order: lpc_order.min(MAX_LPC_ORDER),
            subframes: std::array::from_fn(|_| Subframe::default()),
            ltp_scale: 0.0,
            ltp_scale_q14: 0,
            lsf_interpolation_index: None,
            stereo_weights: None,
            mid_only: false,
        };
    }

    /// Returns the active subframes (`0..num_subframes`) for this SILK frame.
    #[inline]
    pub fn active_subframes(&self) -> &[Subframe] {
        return &self.subframes[..self.num_subframes];
    }

    /// Returns a mutable slice of the active subframes (`0..num_subframes`) for this SILK frame.
    #[inline]
    pub fn active_subframes_mut(&mut self) -> &mut [Subframe] {
        return &mut self.subframes[..self.num_subframes];
    }
}

/// Decoded SILK Subframe with fixed-size stack arrays.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>
#[derive(Debug, Clone)]
pub struct Subframe {
    pub gain: f32,
    pub gain_q16: i32,
    pub log_gain: i32,
    pub nlsf_q15: [i16; MAX_LPC_ORDER],
    pub ltp_coeffs: [i8; LTP_ORDER],
    pub pitch_lag: u16,
    pub excitation: [f32; MAX_SUBFRAME_SAMPLES],
    pub excitation_len: usize,
}

impl Default for Subframe {
    fn default() -> Self {
        return Self {
            gain: 0.0,
            gain_q16: 0,
            log_gain: 0,
            nlsf_q15: [0; MAX_LPC_ORDER],
            ltp_coeffs: [0; LTP_ORDER],
            pitch_lag: 0,
            excitation: [0.0; MAX_SUBFRAME_SAMPLES],
            excitation_len: 0,
        };
    }
}

impl Subframe {
    /// Returns the active NLSF coefficients (`0..lpc_order`) for this subframe.
    #[inline]
    pub fn nlsf(&self, lpc_order: usize) -> &[i16] {
        return &self.nlsf_q15[..lpc_order.min(MAX_LPC_ORDER)];
    }

    /// Returns the active excitation samples (`0..excitation_len`) for this subframe.
    #[inline]
    pub fn active_excitation(&self) -> &[f32] {
        return &self.excitation[..self.excitation_len];
    }

    /// Returns a mutable slice of the active excitation samples (`0..excitation_len`).
    #[inline]
    pub fn active_excitation_mut(&mut self) -> &mut [f32] {
        return &mut self.excitation[..self.excitation_len];
    }
}

/// Combined SILK signal type and quantization offset type for a frame.
#[derive(Debug, Clone, Default, Copy, PartialEq, Eq)]
pub struct FrameType {
    pub signal_type: SignalType,
    pub quantization_offset_type: QuantizationOffsetType,
}

impl FrameType {
    pub const fn new(signal_type: SignalType, quantization_offset_type: QuantizationOffsetType) -> Self {
        return Self {
            signal_type,
            quantization_offset_type,
        };
    }
}

/// SILK Signal Type (RFC 6716 Section 4.2.7.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SignalType {
    #[default]
    Inactive,
    Voiced,
    Unvoiced,
}

/// SILK Quantization Offset Type (RFC 6716 Section 4.2.7.3).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum QuantizationOffsetType {
    #[default]
    High,
    Low,
}

/// Number of 5-ms subframes in a SILK frame.
pub(crate) type SubframeSize = usize;

impl From<FrameDuration> for SubframeSize {
    fn from(frame_size: FrameDuration) -> Self {
        return match frame_size {
            FrameDuration::Ms2_5 => 1,
            FrameDuration::Ms5 => 1,
            FrameDuration::Ms10 => 2,
            FrameDuration::Ms20 => 4,
            FrameDuration::Ms40 => 8,
            FrameDuration::Ms60 => 12,
        };
    }
}

/// Order of the Linear Predictive Coding (LPC) synthesis filter (`10` for NB/MB, `16` for WB).
pub(crate) type LpcOrder = usize;

impl From<Bandwidth> for LpcOrder {
    fn from(value: Bandwidth) -> Self {
        return match value {
            Bandwidth::NarrowBand | Bandwidth::MediumBand => 10,
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => 16,
        };
    }
}