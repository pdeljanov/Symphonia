//! SILK Low-Bitrate Speech Layer
//!
//! This module implements the SILK LP layer of the Opus audio codec (RFC 6716 Section 4.2),
//! decomposed into focused stages:
//! - [`constant`]: Static codebooks, ICDF tables, and filter coefficients.
//! - [`params`]: Header flags, stereo weights, frame type, subframe gains, and LTP parameters.
//! - [`lsf`]: Two-stage Normalized Line Spectral Frequency (NLSF) decoding, stabilization,
//!   subframe interpolation, and NLSF-to-LPC conversion.
//! - [`excitation`]: Rate level, pulse count, shell location, sign, and LCG scaling decoding.
//! - [`synthesis`]: LTP and LPC synthesis filtering and Mid-Side stereo unmixing.
//! - [`resampler`]: Polyphase IIR/FIR resampling from internal SILK rate to output sample rate.
//! - [`decoder`]: Frame-level SILK decoder state and LBRR/regular frame orchestration.

pub(crate) mod constant;
pub(crate) mod decoder;
pub(crate) mod error;
pub(crate) mod excitation;
pub(crate) mod lsf;
pub(crate) mod params;
pub(crate) mod resampler;
pub(crate) mod synthesis;

pub(crate) use decoder::Decoder;
pub(crate) use resampler::ResamplerState;

use std::convert::TryFrom;
use symphonia_core::audio::{Channels, Position};
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

/// Maximum number of 16-sample shell blocks in a single SILK frame (`320 / 16 = 20`).
pub const MAX_SHELL_BLOCKS: usize = MAX_FRAME_SAMPLES / SHELL_BLOCK_SAMPLES;

/// LTP memory duration in milliseconds (`LTP_MEM_LENGTH_MS = 20`).
pub const LTP_MEM_LENGTH_MS: usize = 20;

/// Maximum number of samples in the LTP history buffer (`20 ms * 16 kHz = 320`).
pub const MAX_LTP_MEM_SAMPLES: usize = 320;

/// Length of per-channel output history buffer (`MAX_FRAME_SAMPLES + 2 * MAX_SUBFRAME_SAMPLES = 480`).
pub const OUT_BUF_SAMPLES: usize = MAX_FRAME_SAMPLES + 2 * MAX_SUBFRAME_SAMPLES;

/// Signed `32 x 16` fixed-point multiply returning `(a32 * b16) >> 16` (`silk_SMULWB`).
#[inline(always)]
pub(crate) const fn mul_q16_i16(a32: i32, b16: i16) -> i32 {
    return (((a32 as i64) * (b16 as i64)) >> 16) as i32;
}

/// Signed `32 x 16` fixed-point multiply-accumulate returning `acc32 + ((a32 * b16) >> 16)` (`silk_SMLAWB`).
#[inline(always)]
pub(crate) const fn mla_q16_i16(acc32: i32, a32: i32, b16: i16) -> i32 {
    return acc32.wrapping_add(mul_q16_i16(a32, b16));
}

/// Signed `32 x 32` fixed-point multiply returning `(a32 * b32) >> 16` (`silk_SMULWW`).
#[inline(always)]
pub(crate) const fn mul_q16(a32: i32, b32: i32) -> i32 {
    return (((a32 as i64) * (b32 as i64)) >> 16) as i32;
}

/// Signed `32 x 32` fixed-point multiply-accumulate returning `acc32 + ((a32 * b32) >> 16)` (`silk_SMLAWW`).
#[inline(always)]
pub(crate) const fn mla_q16(acc32: i32, a32: i32, b32: i32) -> i32 {
    return acc32.wrapping_add(mul_q16(a32, b32));
}

/// Signed `32 x 32` high-word multiply returning `(a32 * b32) >> 32` (`silk_SMMUL`).
#[inline(always)]
pub(crate) const fn mul_high_i32(a32: i32, b32: i32) -> i32 {
    return (((a32 as i64) * (b32 as i64)) >> 32) as i32;
}

/// Arithmetic right shift with round-to-nearest (`silk_RSHIFT_ROUND`, `shift >= 1`).
#[inline(always)]
pub(crate) const fn rshift_round(a: i32, shift: u32) -> i32 {
    if shift == 1 {
        return (a >> 1) + (a & 1);
    }
    return ((a >> (shift - 1)) + 1) >> 1;
}

/// 64-bit arithmetic right shift with round-to-nearest (`silk_RSHIFT_ROUND64`, `shift >= 1`).
#[inline(always)]
pub(crate) const fn rshift_round64(a: i64, shift: u32) -> i64 {
    if shift == 1 {
        return (a >> 1) + (a & 1);
    }
    return ((a >> (shift - 1)) + 1) >> 1;
}

/// Saturates a 32-bit signed integer to `i16::MIN..=i16::MAX` (`silk_SAT16`).
#[inline(always)]
pub(crate) fn sat16(a: i32) -> i16 {
    return a.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
}

/// Saturating 32-bit signed addition (`silk_ADD_SAT32`).
#[inline(always)]
pub(crate) const fn add_sat32(a: i32, b: i32) -> i32 {
    return a.saturating_add(b);
}

/// Saturating 32-bit signed subtraction (`silk_SUB_SAT32`).
#[inline(always)]
pub(crate) const fn sub_sat32(a: i32, b: i32) -> i32 {
    return a.saturating_sub(b);
}

/// Saturating 32-bit signed left shift (`silk_LSHIFT_SAT32`).
#[inline(always)]
pub(crate) fn lshift_sat32(a: i32, shift: u32) -> i32 {
    if shift >= 31 {
        if a > 0 {
            return i32::MAX;
        } else if a < 0 {
            return i32::MIN;
        } else {
            return 0;
        }
    }
    return ((a as i64) << shift).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
}

/// Approximates `(1 << q_res) / b32` (`silk_INVERSE32_varQ` in `silk/Inlines.h`).
#[inline]
pub(crate) fn inverse32_var_q(b32: i32, q_res: i32) -> i32 {
    if b32 == 0 || q_res <= 0 {
        return 0;
    }

    let b_headrm = (b32.unsigned_abs().leading_zeros() as i32) - 1;
    let b32_nrm = b32.wrapping_shl(b_headrm as u32);
    let denom = (b32_nrm >> 16) as i16 as i32;
    if denom == 0 {
        return 0;
    }

    let b32_inv = (i32::MAX >> 2) / denom;
    let mut result = b32_inv.wrapping_shl(16);
    let err_q32 = ((1i32 << 29) - mul_q16_i16(b32_nrm, b32_inv as i16)).wrapping_shl(3);
    result = mla_q16(result, err_q32, b32_inv);

    let lshift = 61 - b_headrm - q_res;
    if lshift <= 0 {
        return lshift_sat32(result, (-lshift) as u32);
    } else if lshift < 32 {
        return result >> lshift;
    } else {
        return 0;
    }
}

/// Approximates `(a32 << q_res) / b32` (`silk_DIV32_varQ` in `silk/Inlines.h`).
#[inline]
pub(crate) fn div32_var_q(a32: i32, b32: i32, q_res: i32) -> i32 {
    if b32 == 0 || q_res < 0 {
        return 0;
    }
    if a32 == 0 {
        return 0;
    }

    let a_headrm = (a32.unsigned_abs().leading_zeros() as i32) - 1;
    let mut a32_nrm = a32.wrapping_shl(a_headrm as u32);
    let b_headrm = (b32.unsigned_abs().leading_zeros() as i32) - 1;
    let b32_nrm = b32.wrapping_shl(b_headrm as u32);
    let denom = (b32_nrm >> 16) as i16 as i32;
    if denom == 0 {
        return 0;
    }

    let b32_inv = (i32::MAX >> 2) / denom;
    let mut result = mul_q16_i16(a32_nrm, b32_inv as i16);
    a32_nrm = a32_nrm.wrapping_sub(mul_high_i32(b32_nrm, result).wrapping_shl(3));
    result = mla_q16_i16(result, a32_nrm, b32_inv as i16);

    let lshift = 29 + a_headrm - b_headrm - q_res;
    if lshift < 0 {
        return lshift_sat32(result, (-lshift) as u32);
    } else if lshift < 32 {
        return result >> lshift;
    } else {
        return 0;
    }
}

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

/// Mid-Side stereo unmixing state (`stereo_dec_state` in RFC 6716 Section 4.2.8).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StereoState {
    pub pred_prev_q13: [i32; 2],
    pub s_mid: [i16; 2],
    pub s_side: [i16; 2],
}

impl StereoState {
    pub const fn new() -> Self {
        return Self {
            pred_prev_q13: [0; 2],
            s_mid: [0; 2],
            s_side: [0; 2],
        };
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn reset_side(&mut self) {
        self.pred_prev_q13 = [0; 2];
        self.s_side = [0; 2];
    }
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
/// (RFC 6716 Section 4.2.7.4 - 4.2.9).
#[derive(Debug, Clone, Copy)]
pub struct ChannelState {
    pub last_gain_index: i32,
    pub prev_nlsf_q15: [i16; MAX_LPC_ORDER],
    pub first_frame_after_reset: bool,
    pub prev_signal_type: SignalType,
    pub ec_prev_signal_type: SignalType,
    pub prev_lag_index: i16,
    pub prev_gain_q16: i32,
    pub lag_prev: i32,
    pub s_lpc_q14_buf: [i32; MAX_LPC_ORDER],
    pub out_buf: [i16; OUT_BUF_SAMPLES],
    pub fs_khz: usize,
    pub fs_api_hz: u32,
    pub ltp_mem_length: usize,
    pub resampler: ResamplerState,
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
            ec_prev_signal_type: SignalType::Inactive,
            prev_lag_index: 0,
            prev_gain_q16: 1 << 16,
            lag_prev: 100,
            s_lpc_q14_buf: [0; MAX_LPC_ORDER],
            out_buf: [0; OUT_BUF_SAMPLES],
            fs_khz: 0,
            fs_api_hz: 0,
            ltp_mem_length: MAX_LTP_MEM_SAMPLES,
            resampler: ResamplerState::new(),
        };
    }

    pub fn reset(&mut self) {
        let fs_khz = self.fs_khz;
        let fs_api_hz = self.fs_api_hz;
        *self = Self::new();
        if fs_khz != 0 && fs_api_hz != 0 {
            let _ = self.set_sample_rate(fs_khz, fs_api_hz);
        }
    }

    /// Resets side-channel prediction state when transitioning from `mid_only == true`
    /// to active side coding (`dec_API.c:320-327`).
    pub fn reset_after_mid_only(&mut self) {
        self.out_buf = [0; OUT_BUF_SAMPLES];
        self.s_lpc_q14_buf = [0; MAX_LPC_ORDER];
        self.lag_prev = 100;
        self.last_gain_index = 10;
        self.prev_signal_type = SignalType::Inactive;
        self.first_frame_after_reset = true;
    }

    /// Updates internal and API sample rates (`silk_decoder_set_fs` in `decoder_set_fs.c`).
    pub fn set_sample_rate(&mut self, fs_khz: usize, fs_api_hz: u32) -> Result<()> {
        if self.fs_khz != fs_khz || self.fs_api_hz != fs_api_hz {
            self.resampler.init((fs_khz as u32) * 1000, fs_api_hz)?;
            self.fs_api_hz = fs_api_hz;
        }

        if self.fs_khz != fs_khz {
            self.ltp_mem_length = LTP_MEM_LENGTH_MS * fs_khz;
            self.first_frame_after_reset = true;
            self.lag_prev = 100;
            self.last_gain_index = 10;
            self.prev_signal_type = SignalType::Inactive;
            self.out_buf = [0; OUT_BUF_SAMPLES];
            self.s_lpc_q14_buf = [0; MAX_LPC_ORDER];
            self.fs_khz = fs_khz;
        }

        return Ok(());
    }
}

/// SILK Decoder State across consecutive frames.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>
#[derive(Debug, Clone)]
pub struct State {
    pub(crate) sample_rate: u32,
    pub(crate) output_sample_rate: u32,
    pub(crate) channels: Channels,
    pub(crate) frame_size: FrameDuration,
    pub(crate) bandwidth: Bandwidth,
    pub(crate) prev_frame_type: FrameType,
    pub(crate) lbrr_flag: bool,
    pub(crate) lpc_order: usize,
    pub(crate) channel_states: [ChannelState; 2],
    pub(crate) stereo_state: StereoState,
    pub(crate) prev_decode_only_middle: bool,
    pub(crate) prev_internal_channels: usize,
}

impl Default for State {
    fn default() -> Self {
        return Self {
            sample_rate: 0,
            output_sample_rate: 48_000,
            channels: Channels::Positioned(Position::FRONT_LEFT),
            frame_size: FrameDuration::default(),
            bandwidth: Bandwidth::default(),
            prev_frame_type: FrameType::default(),
            lbrr_flag: false,
            lpc_order: 0,
            channel_states: [ChannelState::new(), ChannelState::new()],
            stereo_state: StereoState::new(),
            prev_decode_only_middle: false,
            prev_internal_channels: 0,
        };
    }
}

impl State {
    pub fn try_new(
        channels: Channels,
        frame_size: FrameDuration,
        bandwidth: Bandwidth,
    ) -> Result<Self> {
        let mut state = Self::default();
        state.configure(channels, frame_size, bandwidth, 48_000)?;
        return Ok(state);
    }

    pub fn configure(
        &mut self,
        channels: Channels,
        frame_size: FrameDuration,
        bandwidth: Bandwidth,
        output_sample_rate: u32,
    ) -> Result<()> {
        let _ = Self::silk_frame_count(frame_size)?;
        let sample_rate = bandwidth.sample_rate();
        let _ = Self::calculate_frame_length(sample_rate, frame_size)?;
        let lpc_order = LpcOrder::from(bandwidth);
        let fs_khz = (sample_rate / 1000) as usize;

        for ch_state in self.channel_states.iter_mut() {
            ch_state.set_sample_rate(fs_khz, output_sample_rate)?;
        }

        self.sample_rate = sample_rate;
        self.output_sample_rate = output_sample_rate;
        self.channels = channels;
        self.frame_size = frame_size;
        self.bandwidth = bandwidth;
        self.lpc_order = lpc_order;

        return Ok(());
    }

    pub fn reset(&mut self) {
        self.lbrr_flag = false;
        self.prev_frame_type = FrameType::default();
        self.prev_decode_only_middle = false;
        self.prev_internal_channels = 0;
        self.stereo_state.reset();
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
            FrameDuration::Ms20 | FrameDuration::Ms40 | FrameDuration::Ms60 => {
                Ok(FrameDuration::Ms20)
            }
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
    pub lcg_seed: u8,
    pub rate_level: u8,
    pub pulse_counts: [u8; MAX_SHELL_BLOCKS],
    pub lsb_counts: [u8; MAX_SHELL_BLOCKS],
    pub pulses: [i16; MAX_FRAME_SAMPLES],
    pub exc_q14: [i32; MAX_FRAME_SAMPLES],
    pub output_pcm: [i16; MAX_FRAME_SAMPLES],
}

impl Default for Frame {
    fn default() -> Self {
        return Self::new(0, 0, MAX_LPC_ORDER);
    }
}

impl Frame {
    pub fn new(sample_count: usize, num_subframes: usize, lpc_order: usize) -> Self {
        let clamped_samples = sample_count.min(MAX_FRAME_SAMPLES);
        let clamped_subframes = num_subframes.min(MAX_SUBFRAMES);
        let subfr_samples = clamped_samples
            .checked_div(clamped_subframes)
            .map_or(0, |s| s.min(MAX_SUBFRAME_SAMPLES));

        return Self {
            frame_type: FrameType::default(),
            vad_flag: false,
            lbrr_flag: false,
            sample_count: clamped_samples,
            num_subframes: clamped_subframes,
            lpc_order: lpc_order.min(MAX_LPC_ORDER),
            subframes: std::array::from_fn(|_| Subframe {
                excitation_len: subfr_samples,
                ..Subframe::default()
            }),
            ltp_scale: 0.0,
            ltp_scale_q14: 0,
            lsf_interpolation_index: None,
            stereo_weights: None,
            mid_only: false,
            lcg_seed: 0,
            rate_level: 0,
            pulse_counts: [0; MAX_SHELL_BLOCKS],
            lsb_counts: [0; MAX_SHELL_BLOCKS],
            pulses: [0; MAX_FRAME_SAMPLES],
            exc_q14: [0; MAX_FRAME_SAMPLES],
            output_pcm: [0; MAX_FRAME_SAMPLES],
        };
    }

    /// Returns the number of 16-sample shell blocks in this frame (`ceil(sample_count / 16)`).
    #[inline]
    pub fn num_shell_blocks(&self) -> usize {
        return self.sample_count.div_ceil(SHELL_BLOCK_SAMPLES);
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
    pub lpc_coeffs_q12: [i16; MAX_LPC_ORDER],
    pub ltp_coeffs: [i8; LTP_ORDER],
    pub pitch_lag: u16,
    pub excitation_q14: [i32; MAX_SUBFRAME_SAMPLES],
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
            lpc_coeffs_q12: [0; MAX_LPC_ORDER],
            ltp_coeffs: [0; LTP_ORDER],
            pitch_lag: 0,
            excitation_q14: [0; MAX_SUBFRAME_SAMPLES],
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

    /// Returns the active Q12 LPC synthesis coefficients (`0..lpc_order`) for this subframe.
    #[inline]
    pub fn lpc_coeffs(&self, lpc_order: usize) -> &[i16] {
        return &self.lpc_coeffs_q12[..lpc_order.min(MAX_LPC_ORDER)];
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
    pub const fn new(
        signal_type: SignalType,
        quantization_offset_type: QuantizationOffsetType,
    ) -> Self {
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