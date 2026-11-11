//! State carried across SILK frames: the per-channel decoder state and history, the stereo
//! unmixing history and the previous-frame references of conditional coding
//! ([RFC 6716 Section 4.2.7.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>),
//! [Section 4.2.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.8>),
//! [Section 4.5.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.2>)).

use crate::silk::ResamplerState;
use crate::silk::cng;
use crate::silk::plc;
use crate::silk::{
    Frame, LTP_MEM_LENGTH_MS, MAX_LPC_ORDER, MAX_LTP_MEM_SAMPLES, MAX_SUBFRAMES, OUT_BUF_SAMPLES,
    SignalType, SilkLayout,
};
use symphonia_core::errors::Result;

/// Two-sample mid and side history and the previous prediction weights carried across SILK
/// frames by the stereo unmixing filter
/// ([RFC 6716 Section 4.2.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.8>)).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StereoState {
    /// Prediction weights `w0_Q13`, `w1_Q13` of the previous frame, the interpolation start.
    pub(crate) prediction_prev_q13: [i32; 2],
    /// Last two mid samples of the previous frame.
    pub(crate) mid_history: [i16; 2],
    /// Last two side samples of the previous frame.
    pub(crate) side_history: [i16; 2],
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
    pub(crate) last_gain_index: i32,
    /// NLSFs of the previous frame, the start of the first-half interpolation (Section 4.2.7.5.5).
    pub(crate) prev_nlsf_q15: [i16; MAX_LPC_ORDER],
    /// Whether no frame has been decoded since the last reset (Section 4.2.7.5.5).
    pub(crate) first_frame_after_reset: bool,
    /// Signal type of the previous frame (Section 4.2.7.6.1).
    pub(crate) prev_signal_type: SignalType,
    /// Primary pitch lag index of the previous frame (Section 4.2.7.6.1).
    pub(crate) prev_lag_index: i16,
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
    pub(crate) references: CodingReferences,
    /// Subframe gain the LPC synthesis state was last scaled to (Section 4.2.7.9.2).
    pub(crate) prev_gain_q16: i32,
    /// Pitch lag of the last subframe, kept for concealment (Section 4.4).
    pub(crate) prev_pitch_lag: i32,
    /// LPC synthesis filter history in Q14 (Section 4.2.7.9.2).
    pub(crate) lpc_state_q14: [i32; MAX_LPC_ORDER],
    /// Previous output samples the LTP synthesis rewhitens (Section 4.2.7.9.1).
    pub(crate) output_history: [i16; OUT_BUF_SAMPLES],
    /// Internal sample rate in kHz (`0` before the first configuration).
    pub(crate) sample_rate_khz: usize,
    /// Output sample rate of the resampler in Hz (`0` before the first configuration).
    pub(crate) output_sample_rate: u32,
    /// Samples of `output_history` covering `LTP_MEM_LENGTH_MS` at the internal rate.
    pub(crate) ltp_mem_length: usize,
    pub(crate) resampler: ResamplerState,
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
    pub(crate) core: ChannelState,
    pub(crate) plc: plc::PlcState,
    pub(crate) cng: cng::CngState,
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
            cng: cng::CngState::new(),
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
        self.cng = cng::CngState::new();
        let sample_rate_khz = self.core.sample_rate_khz;
        if sample_rate_khz == 0 {
            return;
        }
        let frame_length = Self::concealment_frame_length(sample_rate_khz);
        let lpc_order = SilkLayout::lpc_order_for_rate(sample_rate_khz);
        self.plc.ensure_sample_rate(sample_rate_khz, frame_length);
        self.cng.ensure_sample_rate(sample_rate_khz, lpc_order);
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
            self.plc.ensure_sample_rate(sample_rate_khz, frame_length);
            self.cng.ensure_sample_rate(sample_rate_khz, lpc_order);
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
        let decoded = cng::CngFrameInput {
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
