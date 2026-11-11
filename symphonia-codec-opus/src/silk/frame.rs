//! Data of one SILK frame as decoded from the bitstream: the frame geometry of an Opus frame,
//! header flags, stereo prediction weights, the frame and subframe parameters and their
//! enumerations
//! ([RFC 6716 Section 4.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.2>) to
//! [Section 4.2.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7>)).

use crate::silk::error::Error;
use crate::silk::{
    LTP_ORDER, MAX_FRAME_SAMPLES, MAX_LPC_ORDER, MAX_SHELL_BLOCKS, MAX_SILK_FRAMES, MAX_SUBFRAMES,
    SHELL_BLOCK_SAMPLES,
};
use crate::toc::{Bandwidth, FrameDuration};
use symphonia_core::errors::Result;

/// Geometry of the SILK frames carried by one Opus frame
/// ([RFC 6716 Section 4.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.2>)).
///
/// SILK runs at 8, 12 or 16 kHz regardless of the Opus bandwidth, splits every Opus frame into
/// one to three SILK frames of 10 or 20 ms, and every SILK frame into 5 ms subframes:
///
/// ```text
///   Opus frame   SILK frames   subframes each   subframe samples (NB / MB / WB)   LPC order
///   10 ms        1 x 10 ms     2                40 / 60 / 80                      10 / 10 / 16
///   20 ms        1 x 20 ms     4                40 / 60 / 80                      10 / 10 / 16
///   40 ms        2 x 20 ms     4                40 / 60 / 80                      10 / 10 / 16
///   60 ms        3 x 20 ms     4                40 / 60 / 80                      10 / 10 / 16
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SilkLayout {
    /// Coded audio bandwidth, which selects the NLSF and pitch lag codebooks
    /// ([RFC 6716 Section 4.2.7.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5>),
    /// [Section 4.2.7.6.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6.1>)).
    pub(crate) bandwidth: Bandwidth,
    /// Internal SILK sample rate in Hz (`8_000`, `12_000` or `16_000`).
    pub(crate) sample_rate: u32,
    /// Number of SILK frames in the Opus frame (`1..=3`).
    pub(crate) frame_count: usize,
    /// Duration of each SILK frame (`10 ms` or `20 ms`).
    pub(crate) frame_duration: FrameDuration,
    /// Number of 5 ms subframes per SILK frame (`2` or `4`).
    pub(crate) subframes: usize,
    /// Samples per subframe at `sample_rate` (`40`, `60` or `80`).
    pub(crate) subframe_samples: usize,
    /// Order of the LPC synthesis filter (`10` for NB and MB, `16` for WB).
    pub(crate) lpc_order: usize,
}

impl SilkLayout {
    /// Milliseconds per subframe.
    const SUBFRAME_MS: u32 = 5;

    /// Layout used before the first `configure` call: narrowband, one 20 ms frame.
    pub(crate) const UNCONFIGURED: Self = Self {
        bandwidth: Bandwidth::Narrowband,
        sample_rate: 8_000,
        frame_count: 1,
        frame_duration: FrameDuration::Ms20,
        subframes: 4,
        subframe_samples: 40,
        lpc_order: 10,
    };

    /// Derives the layout for an Opus frame of `opus_frame_duration` coded at `bandwidth`.
    ///
    /// Opus frames of 2.5 ms and 5 ms carry no SILK layer and are rejected.
    pub(crate) fn new(bandwidth: Bandwidth, opus_frame_duration: FrameDuration) -> Result<Self> {
        let (frame_count, frame_duration) = match opus_frame_duration {
            FrameDuration::Ms10 => (1, FrameDuration::Ms10),
            FrameDuration::Ms20 => (1, FrameDuration::Ms20),
            FrameDuration::Ms40 => (2, FrameDuration::Ms20),
            FrameDuration::Ms60 => (3, FrameDuration::Ms20),
            FrameDuration::Ms2_5 | FrameDuration::Ms5 => {
                return Err(Error::UnsupportedFrameSize.into());
            }
        };
        let sample_rate = bandwidth.sample_rate().min(16_000);
        return Ok(Self {
            bandwidth,
            sample_rate,
            frame_count,
            frame_duration,
            subframes: Self::subframes_in(frame_duration),
            subframe_samples: (sample_rate / 1_000 * Self::SUBFRAME_MS) as usize,
            lpc_order: Self::lpc_order_for(bandwidth),
        });
    }

    /// Number of 5 ms subframes in a SILK frame of `frame_duration`.
    pub(crate) const fn subframes_in(frame_duration: FrameDuration) -> usize {
        return match frame_duration {
            FrameDuration::Ms2_5 | FrameDuration::Ms5 => 1,
            FrameDuration::Ms10 => 2,
            FrameDuration::Ms20 => 4,
            FrameDuration::Ms40 => 8,
            FrameDuration::Ms60 => 12,
        };
    }

    /// LPC synthesis filter order for `bandwidth`
    /// ([RFC 6716 Section 4.2.7.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5>)).
    pub(crate) const fn lpc_order_for(bandwidth: Bandwidth) -> usize {
        return match bandwidth {
            Bandwidth::Narrowband | Bandwidth::MediumBand => 10,
            Bandwidth::Wideband | Bandwidth::SuperWideband | Bandwidth::Fullband => 16,
        };
    }

    /// LPC synthesis filter order at an internal rate of `sample_rate_khz`: `16` at 16 kHz,
    /// `10` below ([RFC 6716 Section 4.2.7.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5>)).
    pub(crate) const fn lpc_order_for_rate(sample_rate_khz: usize) -> usize {
        return match sample_rate_khz {
            16 => 16,
            _ => 10,
        };
    }

    /// Samples per SILK frame at the internal rate.
    #[inline]
    pub(crate) const fn frame_samples(&self) -> usize {
        return self.subframes * self.subframe_samples;
    }

    /// Internal sample rate in kHz (`8`, `12` or `16`).
    #[inline]
    pub(crate) const fn sample_rate_khz(&self) -> usize {
        return (self.sample_rate / 1_000) as usize;
    }
}

/// Header flags of one SILK channel inside an Opus frame
/// ([RFC 6716 Section 4.2.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.3>),
/// [Section 4.2.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.4>)).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChannelHeader {
    /// Voice activity flag of each SILK frame (Section 4.2.3).
    pub(crate) vad_flags: [bool; MAX_SILK_FRAMES],
    /// Whether any LBRR frame of this channel follows the header (Section 4.2.3).
    pub(crate) lbrr_flag: bool,
    /// Which SILK frames carry an LBRR frame for this channel (Section 4.2.4).
    pub(crate) per_frame_lbrr: [bool; MAX_SILK_FRAMES],
}

/// Decoded SILK header flags for a single Opus frame (10, 20, 40, or 60 ms)
/// ([RFC 6716 Section 4.2.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.3>),
/// [Section 4.2.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.4>)).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SilkHeaderFlags {
    /// Number of coded channels (`1` mono or mid only, `2` mid and side).
    pub(crate) num_channels: usize,
    /// Number of SILK frames in the Opus frame (`1..=3`).
    pub(crate) num_silk_frames: usize,
    /// Flags of the mid (or mono) channel and of the side channel; the side entry stays at its
    /// default for a mono frame.
    pub(crate) channels: [ChannelHeader; 2],
}

impl SilkHeaderFlags {
    /// Returns `true` if SILK frame `i` (`0..num_silk_frames`) carries an LBRR frame in any
    /// coded channel.
    #[inline]
    pub(crate) fn has_frame_lbrr(&self, i: usize) -> bool {
        if i >= self.num_silk_frames {
            return false;
        }
        return self.channels[..self.num_channels].iter().any(|channel| channel.per_frame_lbrr[i]);
    }
}

/// Stereo prediction weights (`w0_Q13`, `w1_Q13`) decoded at the start of a stereo SILK frame.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.1>
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StereoWeights {
    pub(crate) w0_q13: i32,
    pub(crate) w1_q13: i32,
}

/// Decoded SILK frame pair for one 10 ms or 20 ms SILK interval: mid (or mono) and the optional
/// side channel ([RFC 6716 Section 4.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.2>)).
#[derive(Debug, Default, Clone)]
pub(crate) struct SilkChannelFramePair {
    pub(crate) stereo_weights: Option<StereoWeights>,
    pub(crate) mid: Frame,
    pub(crate) side: Option<Frame>,
}

/// Whether a SILK frame codes its gains, pitch lags and LTP scaling on their own or relative to
/// the previous frame of the same channel
/// ([RFC 6716 Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>),
/// [Section 4.2.7.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6>)).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodingMode {
    /// Coded independently: absolute first gain, absolute pitch lag, LTP scaling present.
    #[default]
    Independent,
    /// Coded independently, but the LTP scaling parameter is omitted (the first SILK frame of
    /// an LBRR side channel following a coded frame).
    IndependentWithoutLtpScaling,
    /// Coded relative to the previous SILK frame of the same channel.
    Conditional,
}

/// Decoded SILK frame with fixed-capacity stack storage for up to 4 subframes; each quantity is
/// held once, in the fixed-point format synthesis reads
/// ([RFC 6716 Section 4.2.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7>)).
#[derive(Debug, Clone)]
pub(crate) struct Frame {
    pub(crate) frame_type: FrameType,
    pub(crate) vad_flag: bool,
    pub(crate) lbrr_flag: bool,
    pub(crate) sample_count: usize,
    pub(crate) num_subframes: usize,
    pub(crate) lpc_order: usize,
    pub(crate) subframes: [Subframe; MAX_SUBFRAMES],
    pub(crate) ltp_scale_q14: i16,
    pub(crate) lsf_interpolation_index: Option<u32>,
    pub(crate) stereo_weights: Option<StereoWeights>,
    pub(crate) mid_only: bool,
    pub(crate) lcg_seed: u8,
    pub(crate) rate_level: u8,
    pub(crate) pulse_counts: [u8; MAX_SHELL_BLOCKS],
    pub(crate) lsb_counts: [u8; MAX_SHELL_BLOCKS],
    pub(crate) pulses: [i16; MAX_FRAME_SAMPLES],
    pub(crate) exc_q14: [i32; MAX_FRAME_SAMPLES],
    pub(crate) output_pcm: [i16; MAX_FRAME_SAMPLES],
}

impl Default for Frame {
    fn default() -> Self {
        return Self::new(0, 0, MAX_LPC_ORDER);
    }
}

impl Frame {
    /// Empty frame of `sample_count` samples in `num_subframes` subframes with an LPC filter of
    /// `lpc_order` ([RFC 6716 Section 4.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.2>)).
    pub(crate) fn new(sample_count: usize, num_subframes: usize, lpc_order: usize) -> Self {
        return Self {
            frame_type: FrameType::default(),
            vad_flag: false,
            lbrr_flag: false,
            sample_count: sample_count.min(MAX_FRAME_SAMPLES),
            num_subframes: num_subframes.min(MAX_SUBFRAMES),
            lpc_order: lpc_order.min(MAX_LPC_ORDER),
            subframes: std::array::from_fn(|_| Subframe::default()),
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

    /// Empty frame with the geometry of `layout`
    /// ([RFC 6716 Section 4.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.2>)).
    pub(crate) fn for_layout(layout: &SilkLayout) -> Self {
        return Self::new(layout.frame_samples(), layout.subframes, layout.lpc_order);
    }

    /// Returns the number of 16-sample shell blocks in this frame (`ceil(sample_count / 16)`).
    #[inline]
    pub(crate) fn num_shell_blocks(&self) -> usize {
        return self.sample_count.div_ceil(SHELL_BLOCK_SAMPLES);
    }

    /// Returns the active subframes (`0..num_subframes`) for this SILK frame.
    #[cfg(test)]
    #[inline]
    pub(crate) fn active_subframes(&self) -> &[Subframe] {
        return &self.subframes[..self.num_subframes];
    }

    /// Returns a mutable slice of the active subframes (`0..num_subframes`) for this SILK frame.
    #[inline]
    pub(crate) fn active_subframes_mut(&mut self) -> &mut [Subframe] {
        return &mut self.subframes[..self.num_subframes];
    }
}

/// Decoded parameters of one 5 ms SILK subframe; its excitation is the matching chunk of
/// `Frame::exc_q14`
/// ([RFC 6716 Section 4.2.7.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>)).
#[derive(Debug, Clone, Default)]
pub(crate) struct Subframe {
    pub(crate) gain_q16: i32,
    pub(crate) log_gain: i32,
    pub(crate) nlsf_q15: [i16; MAX_LPC_ORDER],
    pub(crate) lpc_coeffs_q12: [i16; MAX_LPC_ORDER],
    pub(crate) ltp_coeffs: [i8; LTP_ORDER],
    pub(crate) pitch_lag: u16,
}

/// Combined SILK signal type and quantization offset type for a frame.
#[derive(Debug, Clone, Default, Copy, PartialEq, Eq)]
pub(crate) struct FrameType {
    pub(crate) signal_type: SignalType,
    pub(crate) quantization_offset_type: QuantizationOffsetType,
}

impl FrameType {
    pub(crate) const fn new(
        signal_type: SignalType,
        quantization_offset_type: QuantizationOffsetType,
    ) -> Self {
        return Self { signal_type, quantization_offset_type };
    }
}

/// SILK Signal Type (RFC 6716 Section 4.2.7.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SignalType {
    #[default]
    Inactive,
    Voiced,
    Unvoiced,
}

/// SILK Quantization Offset Type (RFC 6716 Section 4.2.7.3).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuantizationOffsetType {
    #[default]
    High,
    Low,
}
