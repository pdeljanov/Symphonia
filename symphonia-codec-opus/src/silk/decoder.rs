//! SILK frame decoder orchestration
//! ([RFC 6716 Section 4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2>)).
//!
//! Decodes the header flags of an Opus frame, parses its LBRR frames, decodes and synthesises
//! its regular frames, and unmixes, resamples and writes every frame pair as soon as it is
//! decoded.

use symphonia_core::errors::Result;

use crate::entropy;
use crate::planes::Planes;
use crate::sample_rate::SampleRate;
use crate::silk::error::Error;
use crate::silk::excitation::{decode_excitation, decode_lcg_seed};
use crate::silk::params::{
    decode_frame_type, decode_mid_only_flag, decode_silk_header, decode_stereo_prediction_weights,
};
use crate::silk::synthesis::{
    STEREO_HISTORY_SAMPLES, buffer_mono_mid, resample_to_f32, unmix_mid_side_to_left_right,
};
use crate::silk::{
    ChannelDecoder, CodingMode, CodingReferences, Frame, LAST_GAIN_INDEX_AFTER_RESET,
    MAX_FRAME_SAMPLES, MAX_SILK_FRAMES, SignalType, SilkChannelFramePair, SilkHeaderFlags,
    SilkLayout, StereoState, StereoWeights,
};
use crate::toc::{Bandwidth, FrameDuration};

/// Samples of one channel frame buffer for stereo unmixing: the two history samples followed
/// by the decoded frame ([RFC 6716 Section 4.2.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.8>)).
const UNMIX_BUFFER_SAMPLES: usize = STEREO_HISTORY_SAMPLES + MAX_FRAME_SAMPLES;

/// SILK layer decoder: configuration, the two channel states and the stereo unmixing state
/// ([RFC 6716 Section 4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2>)).
#[derive(Debug, Clone)]
pub(crate) struct Decoder {
    /// Frame geometry and bandwidth of the current Opus frame (RFC 6716 Section 4.2.2).
    layout: SilkLayout,
    /// Output sample rate of the resampler in Hz (RFC 6716 Section 4.2.9).
    output_sample_rate: u32,
    /// Mid (or mono) and side channel state.
    channels: [ChannelDecoder; 2],
    /// Coding references of the LBRR frames of the current Opus frame, a chain separate from the
    /// regular frames (RFC 6716 Section 4.2.7.4).
    lbrr_references: [CodingReferences; 2],
    /// Stereo unmixing history (RFC 6716 Section 4.2.8).
    stereo: StereoState,
    /// Whether the previous regular frame coded the mid channel only (RFC 6716 Section 4.2.7.2).
    prev_decode_only_middle: bool,
    /// Coded channel count of the previous Opus frame (`0` before the first frame).
    prev_internal_channels: usize,
    /// Whether the last configuration changed the internal rate (RFC 6716 Section 4.2.9).
    internal_rate_changed: bool,
}

impl Decoder {
    /// Creates a new SILK layer decoder with default unconfigured state, resampling its output
    /// to `output_sample_rate`.
    pub(crate) fn new(output_sample_rate: SampleRate) -> Self {
        return Self {
            layout: SilkLayout::UNCONFIGURED,
            output_sample_rate: output_sample_rate.hz(),
            channels: [ChannelDecoder::new(), ChannelDecoder::new()],
            lbrr_references: [CodingReferences::new(); 2],
            stereo: StereoState::new(),
            prev_decode_only_middle: false,
            prev_internal_channels: 0,
            internal_rate_changed: false,
        };
    }

    /// Resets the SILK decoder state across stream discontinuities
    /// ([RFC 6716 Section 4.5.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.2>),
    /// [RFC 8251 Section 3](<https://datatracker.ietf.org/doc/html/rfc8251#section-3>)).
    pub(crate) fn reset(&mut self) {
        self.prev_decode_only_middle = false;
        self.prev_internal_channels = 0;
        self.stereo = StereoState::new();
        self.channels.iter_mut().for_each(ChannelDecoder::reset);
        self.lbrr_references = [CodingReferences::new(); 2];
    }

    /// Configures the decoder for an Opus frame of `frame_duration` coded at `bandwidth` with
    /// `coded_channels` (`1` or `2`) SILK channels
    /// ([RFC 6716 Section 4.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.2>)).
    ///
    /// The coded channel count is decided per frame by the stereo flag of the TOC byte, so it
    /// is accepted here only for symmetry with the caller and not stored.
    pub(crate) fn configure(
        &mut self,
        coded_channels: usize,
        frame_duration: FrameDuration,
        bandwidth: Bandwidth,
    ) -> Result<()> {
        debug_assert!(matches!(coded_channels, 1 | 2));
        let layout = SilkLayout::new(bandwidth, frame_duration)?;
        self.internal_rate_changed =
            self.channels[0].core.sample_rate_khz != layout.sample_rate_khz();
        let output_sample_rate = self.output_sample_rate;
        for channel in self.channels.iter_mut() {
            channel.set_sample_rate(layout.sample_rate_khz(), output_sample_rate)?;
        }
        self.layout = layout;
        return Ok(());
    }

    /// Decodes a single Opus frame's SILK layer from `range_decoder`, unmixes Mid-Side stereo
    /// when applicable, resamples to `output_sample_rate`, and writes `f32` PCM into the planar
    /// output [`Planes`]. Returns the total number of samples per channel written.
    pub(crate) fn decode_frame_planar(
        &mut self,
        range_decoder: &mut entropy::Decoder<'_>,
        is_stereo: bool,
        mut out: Planes<'_>,
    ) -> Result<usize> {
        let stereo_to_mono = self.begin_frame(is_stereo, out.is_stereo());

        let mut total_written = 0usize;
        self.decode_opus_frame(range_decoder, is_stereo, |decoder, kind, i, pair| {
            if kind == FrameKind::Lbrr {
                return Ok(());
            }
            let stereo_to_mono = stereo_to_mono && i == 0;
            let out = out.range(total_written..);
            total_written += decoder.output_frame_pair(pair, is_stereo, stereo_to_mono, out)?;
            return Ok(());
        })?;

        return Ok(total_written);
    }

    /// Conceals a lost or DTX SILK Opus frame with packet loss concealment and comfort noise,
    /// unmixes stereo when applicable, and writes resampled `f32` PCM into the planar output
    /// [`Planes`] (non-normative,
    /// [RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    pub(crate) fn conceal_frame_planar(
        &mut self,
        is_stereo: bool,
        mut out: Planes<'_>,
    ) -> Result<usize> {
        let stereo_to_mono = self.begin_frame(is_stereo, out.is_stereo());

        let mut total_written = 0usize;
        for i in 0..self.layout.frame_count {
            let pair = self.conceal_frame_pair(is_stereo)?;
            let stereo_to_mono = stereo_to_mono && i == 0;
            let out = out.range(total_written..);
            total_written += self.output_frame_pair(&pair, is_stereo, stereo_to_mono, out)?;
        }

        return Ok(total_written);
    }

    /// Applies the coded channel count of the Opus frame about to be decoded or concealed, for
    /// a stereo (`api_stereo`) or mono output stream, and returns whether a stereo stream just
    /// turned mono at an unchanged internal rate.
    ///
    /// A side channel that the previous frame did not code restarts as after a reset, so its next
    /// frame skips the gain clamp and the NLSF interpolation
    /// ([RFC 6716 Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>),
    /// [Section 4.2.7.5.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.5>)).
    /// Stereo output then restarts the unmixing history from zero and the side resampler from the
    /// mid one ([Section 4.2.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.8>)).
    fn begin_frame(&mut self, is_stereo: bool, api_stereo: bool) -> bool {
        let internal_channels = if is_stereo { 2 } else { 1 };
        let previous = self.prev_internal_channels;
        self.prev_internal_channels = internal_channels;
        if internal_channels > previous {
            self.channels[1].reset();
        }
        if api_stereo && internal_channels == 2 && previous == 1 {
            self.stereo.reset_side();
            self.channels[1].core.resampler = self.channels[0].core.resampler;
        }
        return api_stereo
            && internal_channels == 1
            && previous == 2
            && !self.internal_rate_changed;
    }

    /// Conceals one channel's lost SILK frame (non-normative,
    /// [RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    fn conceal_channel_frame(&mut self, channel: usize) -> Result<Frame> {
        let layout = self.layout;
        let mut frame = Frame::for_layout(&layout);
        let sample_count = frame.sample_count;
        self.channels[channel].conceal(&layout, &mut frame.output_pcm[..sample_count])?;
        return Ok(frame);
    }

    /// Conceals a mid/side SILK frame pair for packet loss or DTX; a side channel that the
    /// previous frame left uncoded stays silent and restarts its gain reference
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    fn conceal_frame_pair(&mut self, is_stereo: bool) -> Result<SilkChannelFramePair> {
        let mid = self.conceal_channel_frame(0)?;
        if is_stereo && self.prev_decode_only_middle {
            self.channels[1].core.references.last_gain_index = LAST_GAIN_INDEX_AFTER_RESET;
        }
        let side = (is_stereo && !self.prev_decode_only_middle)
            .then(|| self.conceal_channel_frame(1))
            .transpose()?;
        return Ok(SilkChannelFramePair { stereo_weights: None, mid, side });
    }

    /// Decodes a single Opus frame payload (10, 20, 40, or 60 ms) containing 1 to 3 SILK frames
    /// ([RFC 6716 Section 4.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.2>)).
    ///
    /// LBRR frame pairs ([Section 4.2.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.5>))
    /// are parsed against fresh coding references so the range decoder advances past them; the
    /// regular frame pairs ([Section 4.2.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.6>))
    /// are synthesised. Every frame pair is handed to `sink` with its kind and frame index
    /// as soon as it is decoded, so only one pair is held at a time.
    fn decode_opus_frame<F>(
        &mut self,
        range_decoder: &mut entropy::Decoder<'_>,
        is_stereo: bool,
        mut sink: F,
    ) -> Result<SilkHeaderFlags>
    where
        F: FnMut(&mut Self, FrameKind, usize, &SilkChannelFramePair) -> Result<()>,
    {
        let header = decode_silk_header(range_decoder, is_stereo, self.layout.frame_count)?;

        self.lbrr_references = [CodingReferences::new(); 2];
        self.parse_lbrr_frames(range_decoder, &header, is_stereo, &mut sink)?;

        for i in 0..header.num_silk_frames {
            let plan = FramePairPlan::regular(&header, is_stereo, i, self.prev_decode_only_middle);
            let pair = self.decode_silk_frame_pair(range_decoder, plan)?;
            sink(self, plan.kind, i, &pair)?;
        }

        return Ok(header);
    }

    /// Parses every LBRR frame pair flagged in `header`, handing each to `sink`
    /// ([RFC 6716 Section 4.2.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.5>)).
    fn parse_lbrr_frames<F>(
        &mut self,
        range_decoder: &mut entropy::Decoder<'_>,
        header: &SilkHeaderFlags,
        is_stereo: bool,
        sink: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Self, FrameKind, usize, &SilkChannelFramePair) -> Result<()>,
    {
        for i in (0..header.num_silk_frames).filter(|&i| header.has_frame_lbrr(i)) {
            let plan = FramePairPlan::lbrr(header, is_stereo, i);
            let pair = self.decode_silk_frame_pair(range_decoder, plan)?;
            sink(self, FrameKind::Lbrr, i, &pair)?;
        }
        return Ok(());
    }

    /// Decodes one mid/side SILK frame pair according to `plan`
    /// ([RFC 6716 Section 4.2.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7>)).
    fn decode_silk_frame_pair(
        &mut self,
        range_decoder: &mut entropy::Decoder<'_>,
        plan: FramePairPlan,
    ) -> Result<SilkChannelFramePair> {
        let is_lbrr = plan.kind == FrameKind::Lbrr;
        let decode_mid = !is_lbrr || plan.mid_active;

        let (stereo_weights, mid_only) = match plan.is_stereo && decode_mid {
            true => {
                let weights = decode_stereo_prediction_weights(range_decoder)?;
                (Some(weights), decode_mid_only_flag(range_decoder, plan.side_active)?)
            }
            false => (None, false),
        };

        let layout = self.layout;
        let mut mid = match decode_mid {
            true => self.decode_single_silk_frame(range_decoder, 0, &plan, plan.mid_coding)?,
            false => Frame::for_layout(&layout),
        };
        mid.stereo_weights = stereo_weights;
        mid.mid_only = mid_only;

        let decode_side = match plan.kind {
            FrameKind::Lbrr => plan.is_stereo && plan.side_active,
            FrameKind::Regular { .. } => plan.is_stereo && !mid_only,
        };
        if decode_side && !is_lbrr && self.prev_decode_only_middle {
            self.channels[1].reset_after_mid_only();
        }
        let side = match decode_side {
            true => {
                let mut side =
                    self.decode_single_silk_frame(range_decoder, 1, &plan, plan.side_coding)?;
                side.stereo_weights = stereo_weights;
                side.mid_only = mid_only;
                Some(side)
            }
            false => None,
        };

        if plan.is_stereo && !is_lbrr {
            self.prev_decode_only_middle = mid_only;
        }

        return Ok(SilkChannelFramePair { stereo_weights, mid, side });
    }

    /// Decodes the parameters of one SILK channel frame and, for a regular frame, synthesises
    /// its output ([RFC 6716 Section 4.2.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7>)).
    /// An LBRR frame stops after excitation decoding: its parameters only advance the range
    /// decoder ([Section 4.2.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.5>)).
    fn decode_single_silk_frame(
        &mut self,
        range_decoder: &mut entropy::Decoder<'_>,
        channel: usize,
        plan: &FramePairPlan,
        coding_mode: CodingMode,
    ) -> Result<Frame> {
        let layout = self.layout;
        let is_lbrr = plan.kind == FrameKind::Lbrr;
        let vad_flag = match channel {
            0 => plan.mid_active,
            _ => plan.side_active,
        };
        let mut frame = Frame::for_layout(&layout);
        frame.vad_flag = vad_flag;
        frame.lbrr_flag = match plan.kind {
            FrameKind::Regular { lbrr_present } => lbrr_present,
            FrameKind::Lbrr => true,
        };
        frame.frame_type = decode_frame_type(range_decoder, vad_flag, is_lbrr)?;

        let references = match plan.kind {
            FrameKind::Lbrr => &mut self.lbrr_references[channel],
            FrameKind::Regular { .. } => &mut self.channels[channel].core.references,
        };
        references.decode_gains(range_decoder, &mut frame, coding_mode)?;
        references.decode_lsf(range_decoder, &mut frame, &layout)?;
        if frame.frame_type.signal_type == SignalType::Voiced {
            references.decode_ltp(range_decoder, &mut frame, &layout, coding_mode)?;
        }
        references.finish_frame(frame.frame_type.signal_type);
        decode_lcg_seed(range_decoder, &mut frame)?;
        decode_excitation(range_decoder, &mut frame)?;
        if is_lbrr {
            return Ok(frame);
        }

        let decoder = &mut self.channels[channel];
        decoder.plc.expand_lpc_after_loss(&mut frame);
        decoder.core.synthesize_frame(&mut frame)?;
        decoder.finish_decoded_frame(&mut frame)?;
        return Ok(frame);
    }

    /// Unmixes (or, for mono output, delays) a decoded frame pair, resamples it to the output
    /// rate and writes it into `out`, returning the samples written per channel
    /// ([RFC 6716 Section 4.2.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.8>),
    /// [Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)). A mono
    /// stream to stereo output is duplicated, except on the first frame after a stereo stream
    /// turned mono at an unchanged internal rate, which the side resampler renders once more
    /// (RFC 6716 Appendix A).
    fn output_frame_pair(
        &mut self,
        pair: &SilkChannelFramePair,
        is_stereo: bool,
        stereo_to_mono: bool,
        out: Planes<'_>,
    ) -> Result<usize> {
        let frame_length = pair.mid.sample_count;
        if frame_length == 0 || frame_length > MAX_FRAME_SAMPLES {
            return Err(Error::InvalidFrameSize.into());
        }
        let Planes { left, right } = out;
        let [mid, side] = self.unmix_frame_pair(pair, is_stereo && right.is_some());

        let delayed = 1..1 + frame_length;
        let left_resampler = &mut self.channels[0].core.resampler;
        let written = resample_to_f32(left_resampler, &mid[delayed.clone()], left)?;
        let Some(right) = right else {
            return Ok(written);
        };
        if right.len() < written {
            return Err(Error::BufferOverflow.into());
        }
        let right_source = match (is_stereo, stereo_to_mono) {
            (true, _) => &side,
            (false, true) => &mid,
            (false, false) => {
                right[..written].copy_from_slice(&left[..written]);
                return Ok(written);
            }
        };
        resample_to_f32(&mut self.channels[1].core.resampler, &right_source[delayed], right)?;
        return Ok(written);
    }

    /// Places the decoded mid and side frames after the two samples of stereo history and
    /// unmixes them into left and right, or only delays the mid channel when `unmix` is off
    /// ([RFC 6716 Section 4.2.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.8>)).
    fn unmix_frame_pair(
        &mut self,
        pair: &SilkChannelFramePair,
        unmix: bool,
    ) -> [[i16; UNMIX_BUFFER_SAMPLES]; 2] {
        let frame_length = pair.mid.sample_count;
        let frame = STEREO_HISTORY_SAMPLES..STEREO_HISTORY_SAMPLES + frame_length;
        let mut mid = [0i16; UNMIX_BUFFER_SAMPLES];
        let mut side = [0i16; UNMIX_BUFFER_SAMPLES];
        mid[frame.clone()].copy_from_slice(&pair.mid.output_pcm[..frame_length]);
        if let Some(side_frame) = &pair.side {
            side[frame.clone()].copy_from_slice(&side_frame.output_pcm[..frame_length]);
        }

        let buffered = frame.end;
        if !unmix {
            buffer_mono_mid(&mut self.stereo, &mut mid[..buffered], frame_length);
            return [mid, side];
        }
        let [w0_q13, w1_q13] = self.stereo.prediction_prev_q13;
        let weights = pair.stereo_weights.unwrap_or(StereoWeights { w0_q13, w1_q13 });
        let sample_rate_khz = self.channels[0].core.sample_rate_khz;
        let (mid_frame, side_frame) = (&mut mid[..buffered], &mut side[..buffered]);
        unmix_mid_side_to_left_right(
            &mut self.stereo,
            mid_frame,
            side_frame,
            weights,
            sample_rate_khz,
            frame_length,
        );
        return [mid, side];
    }
}

/// Whether a SILK frame pair carries regular audio or LBRR redundancy
/// ([RFC 6716 Section 4.2.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.5>),
/// [Section 4.2.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.6>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    /// Regular frame; `lbrr_present` records whether either channel carried an LBRR frame for
    /// the same interval ([Section 4.2.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.4>)).
    Regular { lbrr_present: bool },
    /// LBRR frame, parsed for its range decoder side effects only.
    Lbrr,
}

/// Schedule of one mid/side SILK frame pair inside an Opus frame: which channels are coded and
/// how each codes its gains, pitch lags and LTP scaling
/// ([RFC 6716 Section 4.2.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.5>),
/// [Section 4.2.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.6>),
/// [Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FramePairPlan {
    /// Whether the Opus frame codes a side channel.
    is_stereo: bool,
    kind: FrameKind,
    /// VAD flag (regular, [Section 4.2.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.3>))
    /// or per-frame LBRR flag (LBRR, [Section 4.2.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.4>))
    /// of the mid channel.
    mid_active: bool,
    /// The same flag for the side channel.
    side_active: bool,
    mid_coding: CodingMode,
    side_coding: CodingMode,
}

impl FramePairPlan {
    /// Plan of LBRR frame pair `i` ([RFC 6716 Section 4.2.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.5>)).
    fn lbrr(header: &SilkHeaderFlags, is_stereo: bool, i: usize) -> Self {
        let [mid, side] = &header.channels;
        return Self {
            is_stereo,
            kind: FrameKind::Lbrr,
            mid_active: mid.per_frame_lbrr[i],
            side_active: side.per_frame_lbrr[i],
            mid_coding: Self::lbrr_coding_mode(&mid.per_frame_lbrr, i),
            side_coding: Self::lbrr_coding_mode(&side.per_frame_lbrr, i),
        };
    }

    /// Plan of regular frame pair `i` ([RFC 6716 Section 4.2.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.6>));
    /// the side channel loses LTP scaling when the previous pair coded the mid channel only
    /// ([Section 4.2.7.6.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6.3>)).
    fn regular(
        header: &SilkHeaderFlags,
        is_stereo: bool,
        i: usize,
        prev_decode_only_middle: bool,
    ) -> Self {
        let [mid, side] = &header.channels;
        let mid_coding = match i {
            0 => CodingMode::Independent,
            _ => CodingMode::Conditional,
        };
        let side_coding = match (i == 0, prev_decode_only_middle) {
            (true, _) => CodingMode::Independent,
            (false, true) => CodingMode::IndependentWithoutLtpScaling,
            (false, false) => CodingMode::Conditional,
        };
        return Self {
            is_stereo,
            kind: FrameKind::Regular {
                lbrr_present: mid.per_frame_lbrr[i] || side.per_frame_lbrr[i],
            },
            mid_active: mid.vad_flags[i],
            side_active: side.vad_flags[i],
            mid_coding,
            side_coding,
        };
    }

    /// An LBRR frame is coded conditionally when the same channel carried the previous LBRR
    /// frame of the packet ([RFC 6716 Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>)).
    fn lbrr_coding_mode(flags: &[bool; MAX_SILK_FRAMES], i: usize) -> CodingMode {
        if i.checked_sub(1).is_some_and(|j| flags[j]) {
            return CodingMode::Conditional;
        }
        return CodingMode::Independent;
    }
}

#[cfg(test)]
mod tests {
    // Fixtures: packets and expected values recorded from libopus 1.6.1. The behaviour they pin is
    // normative (RFC 6716 Section 4.2) and is verified against the RFC 8251 test vectors by
    // `tests/rfc6716_conformance.rs`.
    use super::*;
    use crate::silk::QuantizationOffsetType;

    fn create_test_silk_decoder(
        stereo: bool,
        frame_size: FrameDuration,
        bandwidth: Bandwidth,
    ) -> Decoder {
        let channels = if stereo { 2 } else { 1 };
        let mut decoder = Decoder::new(SampleRate::Hz48000);
        decoder.configure(channels, frame_size, bandwidth).unwrap();
        return decoder;
    }

    #[test]
    fn silk_multi_frame_40ms_and_lbrr_full_opus_frame_decode() {
        // Range-coded 40 ms Mono NB frame with LBRR on frame 0 and 2 regular 20 ms frames:
        const OPUS_FRAME_40MS_MONO_LBRR_BYTES: [u8; 10] =
            [0x20, 0x04, 0x62, 0x4a, 0xb3, 0x7b, 0x7c, 0x70, 0x97, 0x80];

        let mut decoder =
            create_test_silk_decoder(false, FrameDuration::Ms40, Bandwidth::Narrowband);
        let mut dec = entropy::Decoder::new(&OPUS_FRAME_40MS_MONO_LBRR_BYTES);
        let mut lbrr: [Option<SilkChannelFramePair>; MAX_SILK_FRAMES] =
            std::array::from_fn(|_| None);
        let mut regular: [Option<SilkChannelFramePair>; MAX_SILK_FRAMES] =
            std::array::from_fn(|_| None);
        let header = decoder
            .decode_opus_frame(&mut dec, false, |_, kind, i, pair| {
                match kind {
                    FrameKind::Lbrr => lbrr[i] = Some(pair.clone()),
                    FrameKind::Regular { .. } => regular[i] = Some(pair.clone()),
                }
                return Ok(());
            })
            .unwrap();

        assert_eq!(header.num_silk_frames, 2);
        assert_eq!(lbrr.iter().flatten().count(), 1);
        assert_eq!(regular.iter().flatten().count(), 2);

        let lbrr_mid = &lbrr[0].as_ref().unwrap().mid;
        assert_eq!(lbrr_mid.num_subframes, 4);
        assert_eq!(lbrr_mid.sample_count, 160);
        assert_eq!(lbrr_mid.lsf_interpolation_index, Some(4));

        for reg in regular.iter().flatten() {
            assert_eq!(reg.mid.num_subframes, 4);
            assert_eq!(reg.mid.sample_count, 160);
            assert_eq!(reg.mid.lsf_interpolation_index, Some(4));
        }
    }

    /// Decodes the regular frames of `payload` after the per-frame channel bookkeeping and
    /// returns the side channel frame of the first one.
    fn decode_side(decoder: &mut Decoder, payload: &[u8]) -> Frame {
        decoder.begin_frame(true, true);
        let mut side = None;
        let mut dec = entropy::Decoder::new(payload);
        decoder
            .decode_opus_frame(&mut dec, true, |_, kind, _, pair| {
                if kind != FrameKind::Lbrr && side.is_none() {
                    side = pair.side.clone();
                }
                return Ok(());
            })
            .unwrap();
        return side.unwrap();
    }

    /// A side channel that was not coded in the previous Opus frame starts its next coded frame
    /// without the gain clamp and without NLSF interpolation, like after a reset
    /// ([RFC 6716 Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>),
    /// [Section 4.2.7.5.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.5>)):
    /// after stereo, mono and stereo frames at one internal rate, the side channel decodes as in a
    /// fresh decoder.
    #[test]
    fn side_channel_restarts_after_a_mono_frame() {
        // TOC 0x2c: SILK-only mediumband 20 ms, stereo, one frame; the payload follows the TOC.
        let stereo_0 = &crate::test_vectors::STEREO_MB.packets[0].packet[1..];
        let stereo_1 = &crate::test_vectors::STEREO_MB.packets[1].packet[1..];
        let mut out = [[0.0_f32; 960]; 2];

        let mut switched =
            create_test_silk_decoder(true, FrameDuration::Ms20, Bandwidth::MediumBand);
        let mut dec = entropy::Decoder::new(stereo_0);
        switched.decode_frame_planar(&mut dec, true, Planes::from_rows(&mut out, true)).unwrap();
        switched.conceal_frame_planar(false, Planes::from_rows(&mut out, true)).unwrap();
        let switched_side = decode_side(&mut switched, stereo_1);

        let mut fresh = create_test_silk_decoder(true, FrameDuration::Ms20, Bandwidth::MediumBand);
        let fresh_side = decode_side(&mut fresh, stereo_1);

        assert_eq!(switched_side.output_pcm, fresh_side.output_pcm);
    }

    /// The first frame after a stereo stream turns mono renders the right channel through the side
    /// resampler only when the internal rate is unchanged; at a new rate that resampler still
    /// holds the old rate, so the mid channel is duplicated instead (RFC 6716 Appendix A).
    #[test]
    fn stereo_to_mono_resamples_the_right_channel_only_at_an_unchanged_rate() {
        let stereo_0 = &crate::test_vectors::STEREO_MB.packets[0].packet[1..];
        let mut out = [[0.0_f32; 960]; 2];
        let mut decoder =
            create_test_silk_decoder(true, FrameDuration::Ms20, Bandwidth::MediumBand);
        let mut dec = entropy::Decoder::new(stereo_0);
        decoder.decode_frame_planar(&mut dec, true, Planes::from_rows(&mut out, true)).unwrap();

        let mut same_rate = decoder.clone();
        same_rate.configure(1, FrameDuration::Ms20, Bandwidth::MediumBand).unwrap();
        assert!(same_rate.begin_frame(false, true));

        decoder.configure(1, FrameDuration::Ms20, Bandwidth::Narrowband).unwrap();
        assert!(!decoder.begin_frame(false, true));
    }

    /// Resetting the decoder after a stereo frame clears the stereo unmixing memory and the
    /// mid-only flag ([RFC 8251 Section 3](<https://datatracker.ietf.org/doc/html/rfc8251#section-3>)),
    /// so the next packet decodes identically to a fresh decoder.
    #[test]
    fn reset_clears_stereo_state_for_the_next_packet() {
        let stereo_0 = &crate::test_vectors::STEREO_MB.packets[0].packet[1..];
        let stereo_1 = &crate::test_vectors::STEREO_MB.packets[1].packet[1..];
        let mut out_reset = [[0.0_f32; 960]; 2];
        let mut out_fresh = [[0.0_f32; 960]; 2];

        let mut reset_decoder =
            create_test_silk_decoder(true, FrameDuration::Ms20, Bandwidth::MediumBand);
        let mut dec_0 = entropy::Decoder::new(stereo_0);
        reset_decoder
            .decode_frame_planar(&mut dec_0, true, Planes::from_rows(&mut out_reset, true))
            .unwrap();
        reset_decoder.reset();
        let mut dec_1 = entropy::Decoder::new(stereo_1);
        reset_decoder
            .decode_frame_planar(&mut dec_1, true, Planes::from_rows(&mut out_reset, true))
            .unwrap();

        let mut fresh_decoder =
            create_test_silk_decoder(true, FrameDuration::Ms20, Bandwidth::MediumBand);
        let mut dec_fresh = entropy::Decoder::new(stereo_1);
        fresh_decoder
            .decode_frame_planar(&mut dec_fresh, true, Planes::from_rows(&mut out_fresh, true))
            .unwrap();

        assert_eq!(out_reset, out_fresh);
    }

    #[test]
    fn wideband_voiced_packet_decodes_to_recorded_values() {
        // A 20 ms wideband mono voiced SILK frame (TOC = 0x48) encoded from a 200 Hz harmonic
        // speech-like signal, and the parameters and output it decodes to:
        const WB_VOICED_FRAME: [u8; 26] = [
            0x9e, 0xb1, 0x56, 0x16, 0x50, 0xf5, 0x07, 0xbc, 0x1e, 0x76, 0xe6, 0x1d, 0xe3, 0xe8,
            0xf3, 0xe9, 0x8e, 0x07, 0x89, 0xf4, 0x49, 0xff, 0x51, 0x0d, 0x1d, 0x6b,
        ];
        const EXPECTED_NLSF_Q15: [i16; 16] = [
            674, 1292, 2214, 4949, 6390, 9088, 11264, 13184, 15232, 17536, 19712, 21888, 24192,
            26240, 28416, 30336,
        ];

        let mut decoder = create_test_silk_decoder(false, FrameDuration::Ms20, Bandwidth::Wideband);
        let layout = decoder.layout;
        let core = &mut decoder.channels[0].core;
        let mut dec = entropy::Decoder::new(&WB_VOICED_FRAME);

        let header = decode_silk_header(&mut dec, false, 1).unwrap();
        assert_eq!(header.channels[0].vad_flags, [true, false, false]);
        assert!(!header.channels[0].lbrr_flag);

        let mut frame = Frame::new(320, 4, 16);
        frame.vad_flag = header.channels[0].vad_flags[0];
        frame.frame_type = decode_frame_type(&mut dec, true, false).unwrap();
        assert_eq!(frame.frame_type.signal_type, SignalType::Voiced);
        assert_eq!(frame.frame_type.quantization_offset_type, QuantizationOffsetType::Low);

        core.references.decode_gains(&mut dec, &mut frame, CodingMode::Independent).unwrap();
        assert_eq!(core.references.last_gain_index, 25);
        assert_eq!(
            frame.active_subframes().iter().map(|s| s.gain_q16).collect::<Vec<_>>(),
            vec![4_194_304, 4_194_304, 4_194_304, 4_194_304]
        );

        core.references.decode_lsf(&mut dec, &mut frame, &layout).unwrap();
        assert_eq!(frame.lsf_interpolation_index, Some(4));
        assert_eq!(frame.subframes[0].nlsf_q15[..16], EXPECTED_NLSF_Q15);

        core.references.decode_ltp(&mut dec, &mut frame, &layout, CodingMode::Independent).unwrap();
        assert_eq!(core.references.prev_lag_index, 48);
        assert_eq!(
            frame.active_subframes().iter().map(|s| s.pitch_lag).collect::<Vec<_>>(),
            vec![80, 80, 80, 80]
        );
        assert_eq!(frame.ltp_scale_q14, 15565);

        const REAL_WB_FIRST_32_PCM: [i16; 32] = [
            3, 3, 61, 112, 156, 198, 298, 394, 476, 552, 615, 678, 737, 789, 830, 864, 897, 927,
            947, 963, 967, 958, 944, 919, 886, 845, 797, 749, 695, 639, 579, 520,
        ];
        const REAL_WB_LAST_32_PCM: [i16; 32] = [
            40, -60, -139, -208, -306, -407, -495, -575, -649, -716, -784, -848, -888, -897, -918,
            -948, -957, -968, -1000, -1027, -1054, -1115, -1160, -1192, -1258, -1320, -1373, -1421,
            -1447, -1434, -1389, -1324,
        ];
        const REAL_WB_SLPC_Q14_BUF: [i32; 16] = [
            -245066, -247746, -256088, -262800, -269758, -285468, -296864, -305186, -322048,
            -337976, -351410, -363806, -370326, -366994, -355556, -339060,
        ];

        decode_lcg_seed(&mut dec, &mut frame).unwrap();
        assert_eq!(frame.lcg_seed, 0);

        decode_excitation(&mut dec, &mut frame).unwrap();
        assert_eq!(dec.tell(), 209);

        core.synthesize_frame(&mut frame).unwrap();
        assert_eq!(&frame.output_pcm[..32], &REAL_WB_FIRST_32_PCM);
        assert_eq!(&frame.output_pcm[320 - 32..320], &REAL_WB_LAST_32_PCM);
        assert_eq!(&core.lpc_state_q14, &REAL_WB_SLPC_Q14_BUF);
        assert_eq!(core.prev_pitch_lag, 80);
    }
}
