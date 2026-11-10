//! SILK linear-prediction layer
//! ([RFC 6716 Section 4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2>)).
//!
//! The decoder modules and the data passed between them
//! ([RFC 6716 Section 4.2.1, Figure 14](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.1>)):
//!
//! ```text
//!         +---------+    +------------+
//!      -->| Range   |--->| Decode     |---------------------------+
//!       1 | Decoder | 2  | Parameters |----------+       5        |
//!         +---------+    +------------+     4    |                |
//!                             3 |                |                |
//!                              \/               \/               \/
//!                        +------------+   +------------+   +------------+
//!                        | Generate   |-->| LTP        |-->| LPC        |
//!                        | Excitation |   | Synthesis  |   | Synthesis  |
//!                        +------------+   +------------+   +------------+
//!                                                ^                |
//!                                                |                |
//!                            +-------------------+----------------+
//!                            |                                      6
//!                            |   +------------+   +-------------+
//!                            +-->| Stereo     |-->| Sample Rate |-->
//!                                | Unmixing   | 7 | Conversion  | 8
//!                                +------------+   +-------------+
//!
//!   1: range-coded bitstream             5: LPC coefficients and gains
//!   2: coded parameters                  6: decoded signal (mono or mid-side stereo)
//!   3: pulses, LSBs and signs            7: unmixed signal (mono or left-right stereo)
//!   4: pitch lags, LTP coefficients      8: resampled signal
//! ```
//!
//! Submodules, each with the RFC 6716 sections it implements:
//! - [`constant`]: Codebooks, inverse-CDF tables and filter coefficients
//!   ([RFC 6716 Section 4.2.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7>)).
//! - [`params`]: Header flags, stereo prediction weights, mid-only flag, frame type, subframe
//!   gains and LTP parameters
//!   ([RFC 6716 Section 4.2.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.3>),
//!   [Section 4.2.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.1>) to
//!   [Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>),
//!   [Section 4.2.7.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6>)).
//! - [`lsf`]: Two-stage normalized line spectral frequency decoding, stabilization and
//!   interpolation
//!   ([RFC 6716 Section 4.2.7.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5>)).
//! - [`excitation`]: LCG seed, rate level, pulse counts, shell-coded pulse locations, LSBs and
//!   signs
//!   ([RFC 6716 Section 4.2.7.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.7>),
//!   [Section 4.2.7.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8>)).
//! - [`lpc`]: NLSF to LPC conversion, coefficient limiting and the prediction kernels shared by
//!   frame reconstruction and concealment
//!   ([RFC 6716 Section 4.2.7.5.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.6>)
//!   to [Section 4.2.7.5.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.8>),
//!   [Section 4.2.7.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>)).
//! - [`synthesis`]: LTP and LPC synthesis of a frame and mid-side stereo unmixing
//!   ([RFC 6716 Section 4.2.7.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>),
//!   [Section 4.2.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.8>)).
//! - [`resampler`]: Conversion from the internal SILK rate to the output rate
//!   ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
//! - [`plc`]: Packet loss concealment and comfort noise
//!   ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
//! - [`decoder`]: Per-channel state and the LBRR and regular frame schedule of an Opus frame
//!   ([RFC 6716 Section 4.2.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.4>) to
//!   [Section 4.2.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.6>)).
//! - [`error`]: Errors of the SILK layer.

pub(crate) mod constant;
pub(crate) mod decoder;
pub(crate) mod error;
pub(crate) mod excitation;
pub(crate) mod lpc;
pub(crate) mod lsf;
pub(crate) mod params;
pub(crate) mod plc;
pub(crate) mod resampler;
pub(crate) mod synthesis;

pub(crate) use decoder::Decoder;
pub(crate) use resampler::ResamplerState;

use symphonia_core::errors::Result;

use crate::silk::error::Error;
use crate::toc::{Bandwidth, FrameDuration};

/// Maximum number of 20-ms SILK frames inside a single Opus frame (60 ms = 3 x 20 ms).
pub(crate) const MAX_SILK_FRAMES: usize = 3;

/// Maximum number of 5-ms subframes inside a single SILK frame (20 ms = 4 x 5 ms).
pub(crate) const MAX_SUBFRAMES: usize = 4;

/// Maximum LPC filter order (16 for WB, 10 for NB/MB).
pub(crate) const MAX_LPC_ORDER: usize = 16;

/// Number of taps in the voiced Long-Term Prediction (LTP) filter.
pub(crate) const LTP_ORDER: usize = 5;

/// Maximum number of samples in a single 5-ms SILK subframe (80 samples at 16 kHz WB).
pub(crate) const MAX_SUBFRAME_SAMPLES: usize = 80;

/// Maximum number of samples in a single 20-ms SILK frame (320 samples at 16 kHz WB).
pub(crate) const MAX_FRAME_SAMPLES: usize = MAX_SUBFRAMES * MAX_SUBFRAME_SAMPLES;

/// Number of samples in each shell-coding excitation block.
pub(crate) const SHELL_BLOCK_SAMPLES: usize = 16;

/// Maximum number of 16-sample shell blocks in a single SILK frame (`320 / 16 = 20`).
pub(crate) const MAX_SHELL_BLOCKS: usize = MAX_FRAME_SAMPLES / SHELL_BLOCK_SAMPLES;

/// LTP memory duration in milliseconds (`LTP_MEM_LENGTH_MS = 20`).
pub(crate) const LTP_MEM_LENGTH_MS: usize = 20;

/// Maximum number of samples in the LTP history buffer (`20 ms * 16 kHz = 320`).
pub(crate) const MAX_LTP_MEM_SAMPLES: usize = 320;

/// Length of per-channel output history buffer (`MAX_FRAME_SAMPLES + 2 * MAX_SUBFRAME_SAMPLES = 480`).
pub(crate) const OUT_BUF_SAMPLES: usize = MAX_FRAME_SAMPLES + 2 * MAX_SUBFRAME_SAMPLES;

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
    vad_flags: [bool; MAX_SILK_FRAMES],
    /// Whether any LBRR frame of this channel follows the header (Section 4.2.3).
    lbrr_flag: bool,
    /// Which SILK frames carry an LBRR frame for this channel (Section 4.2.4).
    per_frame_lbrr: [bool; MAX_SILK_FRAMES],
}

/// Decoded SILK header flags for a single Opus frame (10, 20, 40, or 60 ms)
/// ([RFC 6716 Section 4.2.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.3>),
/// [Section 4.2.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.4>)).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SilkHeaderFlags {
    /// Number of coded channels (`1` mono or mid only, `2` mid and side).
    num_channels: usize,
    /// Number of SILK frames in the Opus frame (`1..=3`).
    num_silk_frames: usize,
    /// Flags of the mid (or mono) channel and of the side channel; the side entry stays at its
    /// default for a mono frame.
    channels: [ChannelHeader; 2],
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
    w0_q13: i32,
    w1_q13: i32,
}

/// Two-sample mid and side history and the previous prediction weights carried across SILK
/// frames by the stereo unmixing filter
/// ([RFC 6716 Section 4.2.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.8>)).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StereoState {
    /// Prediction weights `w0_Q13`, `w1_Q13` of the previous frame, the interpolation start.
    prediction_prev_q13: [i32; 2],
    /// Last two mid samples of the previous frame.
    mid_history: [i16; 2],
    /// Last two side samples of the previous frame.
    side_history: [i16; 2],
}

impl StereoState {
    /// Zeroed history and weights, the state after a decoder reset.
    pub(crate) const fn new() -> Self {
        return Self { prediction_prev_q13: [0; 2], mid_history: [0; 2], side_history: [0; 2] };
    }

    /// Clears the weights and the side history when a mono stream turns stereo
    /// ([RFC 8251 Section 3](<https://datatracker.ietf.org/doc/html/rfc8251#section-3>)).
    pub(crate) fn reset_side(&mut self) {
        self.prediction_prev_q13 = [0; 2];
        self.side_history = [0; 2];
    }
}

/// Decoded SILK frame pair for one 10 ms or 20 ms SILK interval: mid (or mono) and the optional
/// side channel ([RFC 6716 Section 4.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.2>)).
#[derive(Debug, Default, Clone)]
pub(crate) struct SilkChannelFramePair {
    stereo_weights: Option<StereoWeights>,
    mid: Frame,
    side: Option<Frame>,
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

/// `previous_log_gain` of a channel after a reset, the floor reference of the first independent
/// gain ([RFC 6716 Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>)).
pub(crate) const LAST_GAIN_INDEX_AFTER_RESET: i32 = 10;

/// Unit gain (`1.0`) in Q16, the subframe gain the LPC state is scaled to after a reset
/// ([RFC 6716 Section 4.2.7.9.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9.2>)).
const UNIT_GAIN_Q16: i32 = 1 << 16;

/// Pitch lag (in samples) concealment assumes before any voiced frame has been decoded
/// (non-normative,
/// [RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
const PITCH_LAG_AFTER_RESET: i32 = 100;

/// The values of the previous frame of one channel that conditional coding and NLSF
/// interpolation refer to
/// ([RFC 6716 Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>),
/// [Section 4.2.7.5.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.5>),
/// [Section 4.2.7.6.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6.1>)).
/// LBRR and regular frames form separate chains: a frame refers only to the previous frame of
/// its own type in the same channel, so the LBRR frames of an Opus frame are parsed against a
/// fresh set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodingReferences {
    /// Log gain index of the last subframe, the reference of conditional gain coding
    /// (Section 4.2.7.4).
    last_gain_index: i32,
    /// NLSFs of the previous frame, the start of the first-half interpolation (Section 4.2.7.5.5).
    prev_nlsf_q15: [i16; MAX_LPC_ORDER],
    /// Whether no frame has been decoded since the last reset (Section 4.2.7.5.5).
    first_frame_after_reset: bool,
    /// Signal type of the previous frame (Section 4.2.7.6.1).
    prev_signal_type: SignalType,
    /// Primary pitch lag index of the previous frame (Section 4.2.7.6.1).
    prev_lag_index: i16,
}

impl CodingReferences {
    /// References of a channel that has not decoded any frame yet.
    pub(crate) const fn new() -> Self {
        return Self {
            last_gain_index: LAST_GAIN_INDEX_AFTER_RESET,
            prev_nlsf_q15: [0; MAX_LPC_ORDER],
            first_frame_after_reset: true,
            prev_signal_type: SignalType::Inactive,
            prev_lag_index: 0,
        };
    }

    /// Records the signal type of a decoded frame, which ends the first frame after a reset
    /// ([RFC 6716 Section 4.2.7.6.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6.1>)).
    pub(crate) fn finish_frame(&mut self, signal_type: SignalType) {
        self.prev_signal_type = signal_type;
        self.first_frame_after_reset = false;
    }

    /// Restarts gain and signal type references and NLSF interpolation, as on the first frame
    /// after a reset ([RFC 6716 Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>)).
    fn restart(&mut self) {
        self.last_gain_index = LAST_GAIN_INDEX_AFTER_RESET;
        self.prev_signal_type = SignalType::Inactive;
        self.first_frame_after_reset = true;
    }
}

/// Per-channel SILK decoding history carried across frames: coding references,
/// LTP and LPC synthesis memory and the output resampler
/// ([RFC 6716 Section 4.2.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7>)).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ChannelState {
    /// References of the regular frames of this channel.
    references: CodingReferences,
    /// Subframe gain the LPC synthesis state was last scaled to (Section 4.2.7.9.2).
    prev_gain_q16: i32,
    /// Pitch lag of the last subframe, kept for concealment (Section 4.4).
    prev_pitch_lag: i32,
    /// LPC synthesis filter history in Q14 (Section 4.2.7.9.2).
    lpc_state_q14: [i32; MAX_LPC_ORDER],
    /// Previous output samples the LTP synthesis rewhitens (Section 4.2.7.9.1).
    output_history: [i16; OUT_BUF_SAMPLES],
    /// Internal sample rate in kHz (`0` before the first configuration).
    sample_rate_khz: usize,
    /// Output sample rate of the resampler in Hz (`0` before the first configuration).
    output_sample_rate: u32,
    /// Samples of `output_history` covering `LTP_MEM_LENGTH_MS` at the internal rate.
    ltp_mem_length: usize,
    resampler: ResamplerState,
}

impl Default for ChannelState {
    fn default() -> Self {
        return Self::new();
    }
}

impl ChannelState {
    /// History of a channel that has not decoded any frame yet.
    pub(crate) fn new() -> Self {
        return Self {
            references: CodingReferences::new(),
            prev_gain_q16: UNIT_GAIN_Q16,
            prev_pitch_lag: PITCH_LAG_AFTER_RESET,
            lpc_state_q14: [0; MAX_LPC_ORDER],
            output_history: [0; OUT_BUF_SAMPLES],
            sample_rate_khz: 0,
            output_sample_rate: 0,
            ltp_mem_length: MAX_LTP_MEM_SAMPLES,
            resampler: ResamplerState::new(),
        };
    }

    /// Clears every history of the channel while keeping its sample rates and the resampler
    /// configuration ([RFC 6716 Section 4.5.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.2>)).
    fn reset(&mut self) {
        let Self { sample_rate_khz, output_sample_rate, ltp_mem_length, mut resampler, .. } = *self;
        resampler.clear_history();
        *self =
            Self { sample_rate_khz, output_sample_rate, ltp_mem_length, resampler, ..Self::new() };
    }

    /// Clears the synthesis history and the conditional-coding references, as on the first frame
    /// after a reset ([RFC 6716 Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>),
    /// [Section 4.2.7.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>)).
    fn reset_synthesis_history(&mut self) {
        self.output_history = [0; OUT_BUF_SAMPLES];
        self.lpc_state_q14 = [0; MAX_LPC_ORDER];
        self.prev_pitch_lag = PITCH_LAG_AFTER_RESET;
        self.references.restart();
    }

    /// Appends `frame_pcm` to the output history, dropping the oldest samples so the newest
    /// `ltp_mem_length` samples stay available to LTP rewhitening
    /// ([RFC 6716 Section 4.2.7.9.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9.1>)).
    pub(crate) fn push_output_history(&mut self, frame_pcm: &[i16]) {
        let frame_length = frame_pcm.len();
        let kept = self.ltp_mem_length.saturating_sub(frame_length);
        if kept > 0 {
            self.output_history.copy_within(frame_length..frame_length + kept, 0);
        }
        self.output_history[kept..kept + frame_length].copy_from_slice(frame_pcm);
    }

    /// Updates the internal and output sample rates, clearing the synthesis history when the
    /// internal rate changes
    /// ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
    #[cfg(test)]
    pub(crate) fn set_sample_rate(
        &mut self,
        sample_rate_khz: usize,
        output_sample_rate: u32,
    ) -> Result<()> {
        self.update_sample_rate(sample_rate_khz, output_sample_rate)?;
        return Ok(());
    }

    /// Updates the internal and output sample rates and returns whether the internal rate
    /// changed, in which case the synthesis history is cleared
    /// ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
    fn update_sample_rate(
        &mut self,
        sample_rate_khz: usize,
        output_sample_rate: u32,
    ) -> Result<bool> {
        if self.sample_rate_khz != sample_rate_khz || self.output_sample_rate != output_sample_rate
        {
            self.resampler.init((sample_rate_khz as u32) * 1000, output_sample_rate)?;
            self.output_sample_rate = output_sample_rate;
        }

        if self.sample_rate_khz == sample_rate_khz {
            return Ok(false);
        }
        self.ltp_mem_length = LTP_MEM_LENGTH_MS * sample_rate_khz;
        self.reset_synthesis_history();
        self.sample_rate_khz = sample_rate_khz;
        return Ok(true);
    }
}

/// The complete mutable state of one SILK channel: decoding history (`core`), packet loss
/// concealment (`plc`) and comfort noise generation (`cng`), kept apart so concealment borrows
/// the history without copying
/// ([RFC 6716 Section 4.2.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7>),
/// [Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ChannelDecoder {
    core: ChannelState,
    plc: plc::PlcState,
    cng: plc::CngState,
}

impl Default for ChannelDecoder {
    fn default() -> Self {
        return Self::new();
    }
}

impl ChannelDecoder {
    /// State of a channel that has not decoded any frame yet.
    pub(crate) fn new() -> Self {
        return Self {
            core: ChannelState::new(),
            plc: plc::PlcState::new(),
            cng: plc::CngState::new(),
        };
    }

    /// Samples of a 20 ms frame at `sample_rate_khz`, at least a narrowband frame, the frame
    /// length concealment is parameterised with (non-normative,
    /// [RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    fn concealment_frame_length(sample_rate_khz: usize) -> usize {
        return (sample_rate_khz * LTP_MEM_LENGTH_MS).max(MAX_SUBFRAMES * 40);
    }

    /// Resets the channel across a stream discontinuity, keeping the configured sample rates
    /// ([RFC 6716 Section 4.5.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.2>)).
    pub(crate) fn reset(&mut self) {
        self.core.reset();
        self.plc = plc::PlcState::new();
        self.cng = plc::CngState::new();
        let sample_rate_khz = self.core.sample_rate_khz;
        if sample_rate_khz == 0 {
            return;
        }
        let frame_length = Self::concealment_frame_length(sample_rate_khz);
        let lpc_order = SilkLayout::lpc_order_for_rate(sample_rate_khz);
        self.plc.ensure_sample_rate(sample_rate_khz as u32, frame_length);
        self.cng.ensure_sample_rate(sample_rate_khz as u32, lpc_order);
    }

    /// Resets the side channel when it is coded again after a mid-only frame
    /// ([RFC 6716 Section 4.2.7.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.2>),
    /// [RFC 8251 Section 3](<https://datatracker.ietf.org/doc/html/rfc8251#section-3>)).
    pub(crate) fn reset_after_mid_only(&mut self) {
        let frame_length = Self::concealment_frame_length(self.core.sample_rate_khz);
        self.plc.reset_frame_parameters(frame_length);
        self.cng.reset();
        self.core.reset_synthesis_history();
    }

    /// Updates the internal and output sample rates, resetting the history, concealment and
    /// comfort noise state when the internal rate changes
    /// ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
    pub(crate) fn set_sample_rate(
        &mut self,
        sample_rate_khz: usize,
        output_sample_rate: u32,
    ) -> Result<()> {
        if self.core.update_sample_rate(sample_rate_khz, output_sample_rate)? {
            let frame_length = Self::concealment_frame_length(sample_rate_khz);
            let lpc_order = SilkLayout::lpc_order_for_rate(sample_rate_khz);
            self.plc.ensure_sample_rate(sample_rate_khz as u32, frame_length);
            self.cng.ensure_sample_rate(sample_rate_khz as u32, lpc_order);
        }
        return Ok(());
    }

    /// Conceals one lost frame of this channel into `out_pcm`: concealment from the history,
    /// comfort noise on top, then the energy glue (non-normative,
    /// [RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    pub(crate) fn conceal(&mut self, layout: &SilkLayout, out_pcm: &mut [i16]) -> Result<()> {
        let Self { core, plc, cng } = self;
        plc.conceal_lost_frame(core, layout, out_pcm);
        cng.apply(core, plc, None, layout.lpc_order, out_pcm)?;
        plc.glue_frames(out_pcm);
        core.references.last_gain_index = LAST_GAIN_INDEX_AFTER_RESET;
        return Ok(());
    }

    /// Updates concealment and comfort noise from a decoded `frame` and applies them to its
    /// output (non-normative,
    /// [RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    pub(crate) fn finish_decoded_frame(&mut self, frame: &mut Frame) -> Result<()> {
        let Self { core, plc, cng } = self;
        plc.update_from_decoded_frame(core, frame);
        let Frame {
            output_pcm, exc_q14, subframes, sample_count, num_subframes, lpc_order, ..
        } = frame;
        let sample_count = *sample_count;
        let decoded = plc::CngFrameInput {
            gains_q16: std::array::from_fn(|i| subframes[i].gain_q16),
            exc_q14: &exc_q14[..sample_count],
            subframes: *num_subframes,
        };
        let frame_pcm = &mut output_pcm[..sample_count];
        cng.apply(core, plc, Some(decoded), *lpc_order, frame_pcm)?;
        plc.glue_frames(frame_pcm);
        return Ok(());
    }
}

/// Decoded SILK frame with fixed-capacity stack storage for up to 4 subframes; each quantity is
/// held once, in the fixed-point format synthesis reads
/// ([RFC 6716 Section 4.2.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7>)).
#[derive(Debug, Clone)]
pub(crate) struct Frame {
    frame_type: FrameType,
    vad_flag: bool,
    lbrr_flag: bool,
    sample_count: usize,
    num_subframes: usize,
    lpc_order: usize,
    subframes: [Subframe; MAX_SUBFRAMES],
    ltp_scale_q14: i16,
    lsf_interpolation_index: Option<u32>,
    stereo_weights: Option<StereoWeights>,
    mid_only: bool,
    lcg_seed: u8,
    rate_level: u8,
    pulse_counts: [u8; MAX_SHELL_BLOCKS],
    lsb_counts: [u8; MAX_SHELL_BLOCKS],
    pulses: [i16; MAX_FRAME_SAMPLES],
    exc_q14: [i32; MAX_FRAME_SAMPLES],
    output_pcm: [i16; MAX_FRAME_SAMPLES],
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
    gain_q16: i32,
    log_gain: i32,
    nlsf_q15: [i16; MAX_LPC_ORDER],
    lpc_coeffs_q12: [i16; MAX_LPC_ORDER],
    ltp_coeffs: [i8; LTP_ORDER],
    pitch_lag: u16,
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
