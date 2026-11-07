//! SILK decoder implementation (symphonia crate).
//!
//! The decoder's LP layer uses a modified version of the SILK codec
//! (herein simply called "SILK"), which runs a decoded excitation signal
//! through adaptive long-term and short-term prediction synthesis
//! filters.  It runs at NB, MB, and WB sample rates internally.  When
//! used in a SWB or FB Hybrid frame, the LP layer itself still only runs
//! in WB.
//! <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2>
//!
//! SILK Decoder Modules
//!
//! ```text
//!    An overview of the decoder is given in Figure 14.
//!
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
//!      1: Range encoded bitstream
//!      2: Coded parameters
//!      3: Pulses, LSBs, and signs
//!      4: Pitch lags, Long-Term Prediction (LTP) coefficients
//!      5: Linear Predictive Coding (LPC) coefficients and gains
//!      6: Decoded signal (mono or mid-side stereo)
//!      7: Unmixed signal (mono or left-right stereo)
//!      8: Resampled signal
//!
//!
//!                           Figure 14: SILK Decoder
//! ```
//!
//! <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.1>
use std::convert::TryFrom;
use crate::entropy::{self, RangeDecoder};
use crate::packet::FramePacket;
use crate::silk::error::Error;
use crate::toc::{Bandwidth, FrameDuration};
use crate::silk::constant;

use symphonia_core::audio::{AsGenericAudioBufferRef, Audio, AudioBuffer, AudioMut, AudioSpec, Channels, GenericAudioBufferRef};
use symphonia_core::codecs::audio::{AudioCodecParameters, FinalizeResult};
use symphonia_core::errors::Result;
use symphonia_core::io::BitReaderLtr;
use symphonia_core::packet::PacketRef;

/// SILK Frame Structure
/// ```text
/// +----------------------------------+
/// |           Header Bits            |
/// | +------------------------------+ |
/// | |    VAD flag (1 bit/frame)    | |
/// | +------------------------------+ |
/// | |        LBRR flag (1 bit)     | |
/// | +------------------------------+ |
/// +----------------------------------+
/// |      Per-Frame LBRR Flags        |
/// |          (optional)              |
/// +----------------------------------+
/// |        LBRR Frames               |
/// |          (optional)              |
/// | +------------------------------+ |
/// | |        LBRR Frame 1          | |
/// | +------------------------------+ |
/// | |        LBRR Frame 2          | |
/// | +------------------------------+ |
/// | |        LBRR Frame 3          | |
/// | +------------------------------+ |
/// +----------------------------------+
/// |         Regular SILK Frame       |
/// | +------------------------------+ |
/// | |        Frame Type            | |
/// | +------------------------------+ |
/// | |     Quantization Gains       | |
/// | +------------------------------+ |
/// | | Normalized LSF Stage1 Index  | |
/// | +------------------------------+ |
/// | |Normalized LSF Stage2 Residual| |
/// | +------------------------------+ |
/// | |   LSF Interpolation Weight   | |
/// | |      (optional, 20 ms)       | |
/// | +------------------------------+ |
/// | |    Primary Pitch Lag         | |
/// | |    (optional, voiced)        | |
/// | +------------------------------+ |
/// | | Subframe Pitch Contour       | |
/// | |      (optional, voiced)      | |
/// | +------------------------------+ |
/// | |    Periodicity Index         | |
/// | |    (optional, voiced)        | |
/// | +------------------------------+ |
/// | |      LTP Filter Coeffs       | |
/// | |    (optional, voiced)        | |
/// | +------------------------------+ |
/// | |       LTP Scaling            | |
/// | |    (optional, conditional)   | |
/// | +------------------------------+ |
/// | |         LCG Seed             | |
/// | +------------------------------+ |
/// | |    Excitation Rate Level     | |
/// | +------------------------------+ |
/// | |   Excitation Pulse Counts    | |
/// | +------------------------------+ |
/// | |  Excitation Pulse Locations  | |
/// | +------------------------------+ |
/// | |      Excitation LSBs         | |
/// | +------------------------------+ |
/// | |     Excitation Signs         | |
/// | +------------------------------+ |
/// +----------------------------------+
/// ```
/// 1. The size and presence of each component can vary based on frame type,
///    signal characteristics, and coding decisions.
/// 2. **LBRR (Low Bit-Rate Redundancy) frames** are optional and may not be present.
/// 3. Some elements (like LTP parameters) are only present in **voiced frames**.
/// 4. The **LSF interpolation weight** is only present in **20 ms frames**.
/// 5. **LTP scaling** is only present under certain conditions specified in the RFC.
/// 6. The excitation coding process includes multiple steps with variable sizes.
/// 7. **Additional Clarifications:**
///    - **VAD Flag:** Indicates whether Voice Activity Detection is active for the frame.
///    - **LBRR Flags:** Each LBRR frame has its own flag indicating its presence.
///    - **Frame Type:** Specifies whether the frame is voiced, unvoiced, or a transition.
///    - **LSF Indices:** Represent the Line Spectral Frequencies used for spectral envelope modeling.
///    - **Pitch Parameters:** Include primary pitch lag and subframe pitch contours for voiced frames.
///    - **LTP (Long-Term Prediction) Parameters:** Enhance the coding of periodic signals.
///    - **Excitation Parameters:** Define the excitation signal's characteristics, crucial for synthesizing the speech signal.
///
/// This structure reflects the complex and variable nature of SILK frames
/// as described in RFC 6716, Section 4.2.7.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7>
pub struct Decoder {
    params: AudioCodecParameters,
    channels: Channels,
    buffer: AudioBuffer<f32>,
    state: State,
}

impl Decoder {
    pub fn try_new(params: AudioCodecParameters) -> Result<Self> {
        let params = params.to_owned();
        let state = State::default();

        let sample_rate = params.sample_rate.ok_or(Error::UnsupportedConfig)?;
        let channels = params.channels.clone().ok_or(Error::UnsupportedConfig)?;
        let spec = AudioSpec::new(sample_rate, channels.clone());
        let buffer: AudioBuffer<f32> = AudioBuffer::new(spec, sample_rate as usize);

        return Ok(Self { params, channels, buffer, state });
    }

    pub fn reset(&mut self) {
        self.state.reset();
        self.buffer.clear();
    }

    pub fn codec_params(&self) -> &AudioCodecParameters {
        return &self.params;
    }

    pub fn finalize(&mut self) -> FinalizeResult {
        return FinalizeResult::default();
    }

    pub fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        return self.buffer.as_generic_audio_buffer_ref();
    }

    /// Decodes a SILK packet
    ///
    /// This method implements the main decoding process for SILK packets,
    /// including handling of regular frames and LBRR (Low Bit-Rate Redundancy) frames.
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7>
    pub fn decode(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        self.buffer.clear();

        let frame_packet = FramePacket::new(packet.data)?;

        if self.state.frame_size != frame_packet.frame_size || self.state.bandwidth != frame_packet.bandwidth || self.state.channels != self.channels {
            self.state = State::try_new(self.channels.clone(), frame_packet.frame_size, frame_packet.bandwidth)?;
        }

        for frame_data in frame_packet.frames.iter() {
            if frame_data.is_empty() {
                continue;
            }

            let decoded_opus_frame = self.decode_opus_frame(frame_data, frame_packet.stereo)?;

            for lbrr_frame in decoded_opus_frame.lbrr_frames.iter() {
                self.synthesize_frame(&lbrr_frame.mid)?;
            }

            for regular_frame in decoded_opus_frame.regular_frames.iter() {
                self.synthesize_frame(&regular_frame.mid)?;
            }
        }

        return Ok(self.buffer.as_generic_audio_buffer_ref());
    }

    /// Decodes a single Opus frame payload (10, 20, 40, or 60 ms) containing 1 to 3 SILK frames.
    ///
    /// ```text
    /// +-------------------------------------------------------------------+
    /// | 1. Header Bits (RFC 6716 Section 4.2.3)                           |
    /// |    - Mono:   [VAD_0 .. VAD_{F-1}, LBRR_0]                         |
    /// |    - Stereo: [VAD_mid_0 .. VAD_mid_{F-1}, LBRR_mid,               |
    /// |               VAD_side_0 .. VAD_side_{F-1}, LBRR_side]            |
    /// +-------------------------------------------------------------------+
    /// | 2. Per-Frame LBRR Flags (RFC 6716 Section 4.2.4, Table 4)         |
    /// |    - Coded per channel c where LBRR_c == 1 (40 ms or 60 ms only)  |
    /// +-------------------------------------------------------------------+
    /// | 3. LBRR Frames 0..F-1 (RFC 6716 Section 4.2.5 - 4.2.6)            |
    /// |    - Present for SILK sub-frame f if any channel has LBRR[c][f]   |
    /// +-------------------------------------------------------------------+
    /// | 4. Regular SILK Frames 0..F-1 (RFC 6716 Section 4.2.7)            |
    /// |    - Stereo weights + conditional mid-only flag (if stereo)       |
    /// |    - Mid (or Mono) SILK frame                                     |
    /// |    - Side SILK frame (if stereo and !mid_only)                    |
    /// +-------------------------------------------------------------------+
    /// ```
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.2>
    fn decode_opus_frame(&mut self, data: &[u8], is_stereo: bool) -> Result<DecodedOpusFrame> {
        let reader = BitReaderLtr::new(data);
        let mut range_decoder = entropy::Decoder::new(reader)?;

        return self.decode_opus_frame_from_range_decoder(&mut range_decoder, is_stereo);
    }

    fn decode_opus_frame_from_range_decoder<R: RangeDecoder>(
        &mut self,
        range_decoder: &mut R,
        is_stereo: bool,
    ) -> Result<DecodedOpusFrame> {
        let header = self.decode_silk_header(range_decoder, is_stereo, self.state.frame_size)?;
        self.state.lbrr_flag = header.has_any_lbrr();

        let silk_frame_duration = State::silk_frame_duration(self.state.frame_size)?;

        let mut lbrr_frames = Vec::new();
        if header.has_any_lbrr() {
            let saved_channel_states = self.state.channel_states.clone();
            let saved_prev_decode_only_middle = self.state.prev_decode_only_middle;
            for ch_state in self.state.channel_states.iter_mut() {
                ch_state.reset(self.state.lpc_order);
            }
            self.state.prev_decode_only_middle = false;

            for f in 0..header.num_silk_frames {
                if header.has_frame_lbrr(f) {
                    let mid_coding_mode = if f > 0 && header.per_frame_lbrr[0][f - 1] {
                        CodingMode::CodeConditionally
                    } else {
                        CodingMode::CodeIndependently
                    };
                    let side_coding_mode = if f == 0 || !header.per_frame_lbrr[1][f - 1] {
                        CodingMode::CodeIndependently
                    } else if self.state.prev_decode_only_middle {
                        CodingMode::CodeIndependentlyNoLtpScaling
                    } else {
                        CodingMode::CodeConditionally
                    };
                    let lbrr_pair = self.decode_silk_frame_pair(
                        range_decoder,
                        silk_frame_duration,
                        is_stereo,
                        header.per_frame_lbrr[0][f],
                        header.per_frame_lbrr[1][f],
                        true,
                        true,
                        mid_coding_mode,
                        side_coding_mode,
                    )?;
                    lbrr_frames.push(lbrr_pair);
                }
            }

            self.state.channel_states = saved_channel_states;
            self.state.prev_decode_only_middle = saved_prev_decode_only_middle;
        }

        let mut regular_frames = Vec::with_capacity(header.num_silk_frames);
        for f in 0..header.num_silk_frames {
            let mid_coding_mode = if f == 0 {
                CodingMode::CodeIndependently
            } else {
                CodingMode::CodeConditionally
            };
            let side_coding_mode = if f == 0 {
                CodingMode::CodeIndependently
            } else if self.state.prev_decode_only_middle {
                CodingMode::CodeIndependentlyNoLtpScaling
            } else {
                CodingMode::CodeConditionally
            };
            let regular_pair = self.decode_silk_frame_pair(
                range_decoder,
                silk_frame_duration,
                is_stereo,
                header.vad_flags[0][f],
                header.vad_flags[1][f],
                header.per_frame_lbrr[0][f] || header.per_frame_lbrr[1][f],
                false,
                mid_coding_mode,
                side_coding_mode,
            )?;
            regular_frames.push(regular_pair);
        }

        return Ok(DecodedOpusFrame {
            header,
            lbrr_frames,
            regular_frames,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn decode_silk_frame_pair<R: RangeDecoder>(
        &mut self,
        range_decoder: &mut R,
        silk_frame_duration: FrameDuration,
        is_stereo: bool,
        mid_vad: bool,
        side_vad: bool,
        lbrr_flag: bool,
        is_lbrr_frame: bool,
        mid_coding_mode: CodingMode,
        side_coding_mode: CodingMode,
    ) -> Result<SilkChannelFramePair> {
        let (stereo_weights, mid_only) = if is_stereo {
            let weights = self.decode_stereo_prediction_weights(range_decoder)?;
            let mid_only_flag = self.decode_mid_only_flag(range_decoder, side_vad)?;
            (Some(weights), mid_only_flag)
        } else {
            (None, false)
        };

        let mut mid = self.decode_single_silk_frame(
            range_decoder,
            0,
            mid_coding_mode,
            silk_frame_duration,
            mid_vad,
            lbrr_flag,
            is_lbrr_frame,
        )?;
        mid.stereo_weights = stereo_weights;
        mid.mid_only = mid_only;

        let side = if is_stereo && !mid_only {
            if self.state.prev_decode_only_middle {
                self.state.channel_states[1].reset_after_mid_only();
            }
            let mut side_frame = self.decode_single_silk_frame(
                range_decoder,
                1,
                side_coding_mode,
                silk_frame_duration,
                side_vad,
                lbrr_flag,
                is_lbrr_frame,
            )?;
            side_frame.stereo_weights = stereo_weights;
            side_frame.mid_only = mid_only;
            Some(side_frame)
        } else {
            None
        };

        if is_stereo {
            self.state.prev_decode_only_middle = mid_only;
        }

        return Ok(SilkChannelFramePair {
            stereo_weights,
            mid_only,
            mid,
            side,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn decode_single_silk_frame<R: RangeDecoder>(
        &mut self,
        range_decoder: &mut R,
        channel: usize,
        coding_mode: CodingMode,
        silk_frame_duration: FrameDuration,
        vad_flag: bool,
        lbrr_flag: bool,
        is_lbrr_frame: bool,
    ) -> Result<Frame> {
        let sample_count = State::calculate_frame_length(self.state.sample_rate, silk_frame_duration)?;
        let num_subframes = SubframeSize::from(silk_frame_duration);
        let mut frame = Frame::new(sample_count, num_subframes);
        frame.vad_flag = vad_flag;
        frame.lbrr_flag = lbrr_flag;

        frame.frame_type = self.decode_frame_type(range_decoder, vad_flag, is_lbrr_frame)?;

        self.decode_gains(range_decoder, &mut frame, channel, coding_mode)?;

        self.decode_lsf(range_decoder, &mut frame, channel, silk_frame_duration)?;

        if frame.frame_type.signal_type == SignalType::Voiced {
            self.decode_ltp(range_decoder, &mut frame, channel, coding_mode, silk_frame_duration)?;
        }

        self.decode_excitation(range_decoder, &mut frame)?;

        self.state.prev_frame_type = frame.frame_type;
        self.state.channel_states[channel].prev_signal_type = frame.frame_type.signal_type;
        self.state.channel_states[channel].first_frame_after_reset = false;

        return Ok(frame);
    }

    /// Decodes the SILK header bits and optional per-frame LBRR flags at the start of an Opus frame.
    ///
    /// The SILK layer begins with two to eight header bits depending on frame duration (10/20/40/60 ms)
    /// and channel count (mono vs. stereo). Each channel codes `1..=3` VAD flags (one per 10/20 ms
    /// SILK frame) followed by one global channel LBRR flag. Immediately afterward, any channel with
    /// `lbrr_flag == true` in a 40 ms or 60 ms packet codes a per-frame LBRR bitmask using Table 4.
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.3>
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.4>
    fn decode_silk_header<R: RangeDecoder>(
        &self,
        decoder: &mut R,
        is_stereo: bool,
        frame_size: FrameDuration,
    ) -> Result<SilkHeaderFlags> {
        let num_channels = if is_stereo { 2 } else { 1 };
        let num_silk_frames = State::silk_frame_count(frame_size)?;

        let mut vad_flags = [[false; 3]; 2];
        let mut lbrr_flags = [false; 2];
        let mut per_frame_lbrr = [[false; 3]; 2];

        for ch in 0..num_channels {
            for vad_flag in vad_flags[ch].iter_mut().take(num_silk_frames) {
                *vad_flag = decoder.decode_symbol_logp(1)? == 1;
            }
            lbrr_flags[ch] = decoder.decode_symbol_logp(1)? == 1;
        }

        for ch in 0..num_channels {
            if lbrr_flags[ch] {
                match frame_size {
                    FrameDuration::Ms10 | FrameDuration::Ms20 => {
                        per_frame_lbrr[ch][0] = true;
                    }
                    FrameDuration::Ms40 => {
                        let mask = decoder.decode_symbol_with_icdf(&constant::ICDF_PER_FRAME_LBRR_FLAGS_40_MS)? + 1;
                        for (f, flag) in per_frame_lbrr[ch].iter_mut().enumerate().take(num_silk_frames) {
                            *flag = ((mask >> f) & 1) != 0;
                        }
                    }
                    FrameDuration::Ms60 => {
                        let mask = decoder.decode_symbol_with_icdf(&constant::ICDF_PER_FRAME_LBRR_FLAGS_60_MS)? + 1;
                        for (f, flag) in per_frame_lbrr[ch].iter_mut().enumerate().take(num_silk_frames) {
                            *flag = ((mask >> f) & 1) != 0;
                        }
                    }
                    FrameDuration::Ms2_5 | FrameDuration::Ms5 => {
                        return Err(Error::UnsupportedFrameSize.into());
                    }
                }
            }
        }

        return Ok(SilkHeaderFlags {
            num_channels,
            num_silk_frames,
            vad_flags,
            lbrr_flags,
            per_frame_lbrr,
        });
    }

    /// Decodes stereo prediction weights (`w0_Q13`, `w1_Q13`) at the start of a stereo SILK frame.
    ///
    /// ```text
    /// wi0 = i0 + 3 * (n / 5)
    /// wi1 = i2 + 3 * (n % 5)
    /// w1_Q13 = w_Q13[wi1] + (((w_Q13[wi1 + 1] - w_Q13[wi1]) * 6554) >> 16) * (2 * i3 + 1)
    /// w0_Q13 = w_Q13[wi0] + (((w_Q13[wi0 + 1] - w_Q13[wi0]) * 6554) >> 16) * (2 * i1 + 1) - w1_Q13
    /// ```
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.1>
    fn decode_stereo_prediction_weights<R: RangeDecoder>(&self, decoder: &mut R) -> Result<StereoWeights> {
        let n = decoder.decode_symbol_with_icdf(&constant::ICDF_STEREO_PREDICTION_WEIGHT_STAGE_ONE)? as usize;
        let i0 = decoder.decode_symbol_with_icdf(&constant::ICDF_STEREO_PREDICTION_WEIGHT_STAGE_TWO)? as usize;
        let i1 = decoder.decode_symbol_with_icdf(&constant::ICDF_STEREO_PREDICTION_WEIGHT_STAGE_THREE)? as i32;
        let i2 = decoder.decode_symbol_with_icdf(&constant::ICDF_STEREO_PREDICTION_WEIGHT_STAGE_TWO)? as usize;
        let i3 = decoder.decode_symbol_with_icdf(&constant::ICDF_STEREO_PREDICTION_WEIGHT_STAGE_THREE)? as i32;

        let wi0 = i0 + 3 * (n / 5);
        let wi1 = i2 + 3 * (n % 5);

        let w_q13 = &constant::CODEBOOK_STEREO_QUANTIZATION_BOUNDARIES_Q13;
        if wi0 + 1 >= w_q13.len() || wi1 + 1 >= w_q13.len() {
            return Err(Error::InvalidData.into());
        }

        let w1_low = w_q13[wi1] as i32;
        let w1_high = w_q13[wi1 + 1] as i32;
        let w1_q13 = w1_low + ((((w1_high - w1_low) * 6554) >> 16) * (2 * i3 + 1));

        let w0_low = w_q13[wi0] as i32;
        let w0_high = w_q13[wi0 + 1] as i32;
        let w0_q13 = w0_low + ((((w0_high - w0_low) * 6554) >> 16) * (2 * i1 + 1)) - w1_q13;

        return Ok(StereoWeights { w0_q13, w1_q13 });
    }

    /// Decodes the conditional mid-only stereo flag when the side channel is inactive.
    ///
    /// When `side_active` is `true` (side VAD is 1 for a regular frame, or side LBRR is 1 for an
    /// LBRR frame), the side channel must be coded and the mid-only flag is omitted (`false`).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.2>
    fn decode_mid_only_flag<R: RangeDecoder>(&self, decoder: &mut R, side_active: bool) -> Result<bool> {
        if side_active {
            return Ok(false);
        }

        let mid_only = decoder.decode_symbol_with_icdf(&constant::ICDF_STEREO_MID_ONLY)?;
        return Ok(mid_only == 1);
    }

    /// Decodes the SILK frame type (RFC 6716 Section 4.2.7.3).
    ///
    /// The frame type is encoded using a context-dependent codebook selected by
    /// whether the frame is active (`vad_flag || is_lbrr_frame`).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.3>
    fn decode_frame_type<R: RangeDecoder>(
        &mut self,
        decoder: &mut R,
        vad_flag: bool,
        is_lbrr_frame: bool,
    ) -> Result<FrameType> {
        let active = vad_flag || is_lbrr_frame;
        let icdf: &[u32] = if active {
            &constant::ICDF_FRAME_TYPE_VAD_ACTIVE
        } else {
            &constant::ICDF_FRAME_TYPE_VAD_INACTIVE
        };
        let frame_type_symbol = decoder.decode_symbol_with_icdf(icdf)?;

        let (signal_type, quantization_offset_type) = match (active, frame_type_symbol) {
            (false, 0) => (SignalType::Inactive, QuantizationOffsetType::Low),
            (false, 1) => (SignalType::Inactive, QuantizationOffsetType::High),
            (true, 0) => (SignalType::Unvoiced, QuantizationOffsetType::Low),
            (true, 1) => (SignalType::Unvoiced, QuantizationOffsetType::High),
            (true, 2) => (SignalType::Voiced, QuantizationOffsetType::Low),
            (true, 3) => (SignalType::Voiced, QuantizationOffsetType::High),
            _ => return Err(Error::InvalidFrameType.into()),
        };

        return Ok(FrameType::new(signal_type, quantization_offset_type));
    }

    /// Decodes the Normalized Line Spectral Frequencies (NLSF) for a SILK frame
    /// and interpolates across subframes when applicable (RFC 6716 Section 4.2.7.5).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5>
    fn decode_lsf<R: RangeDecoder>(
        &mut self,
        decoder: &mut R,
        frame: &mut Frame,
        channel: usize,
        silk_frame_duration: FrameDuration,
    ) -> Result<()> {
        let i1 = self.decode_lsf_stage1(decoder, frame.frame_type.signal_type)?;
        let (d_lpc, res_q10) = self.decode_lsf_stage2(decoder, i1)?;
        let raw_nlsf_q15 = self.reconstruct_nlsf(d_lpc, &res_q10, i1)?;
        let nlsf_q15 = self.stabilize_nlsf(&raw_nlsf_q15)?;

        let raw_interp_idx = if silk_frame_duration == FrameDuration::Ms20 {
            let idx = decoder.decode_symbol_with_icdf(&constant::ICDF_NORMALIZED_LSF_INTERPOLATION_INDEX)?;
            frame.lsf_interpolation_index = Some(idx);
            idx
        } else {
            frame.lsf_interpolation_index = None;
            4
        };

        let ch_state = &mut self.state.channel_states[channel];
        let effective_w_q2 = if ch_state.first_frame_after_reset || ch_state.prev_nlsf_q15.len() != d_lpc {
            4
        } else {
            raw_interp_idx
        };

        if effective_w_q2 < 4 && frame.subframes.len() == 4 {
            let mut nlsf0_q15 = vec![0i16; d_lpc];
            for k in 0..d_lpc {
                let prev = ch_state.prev_nlsf_q15[k] as i32;
                let curr = nlsf_q15[k] as i32;
                nlsf0_q15[k] = (prev + (((curr - prev) * (effective_w_q2 as i32)) >> 2)) as i16;
            }
            frame.subframes[0].nlsf_q15 = nlsf0_q15.clone();
            frame.subframes[1].nlsf_q15 = nlsf0_q15;
            frame.subframes[2].nlsf_q15 = nlsf_q15.clone();
            frame.subframes[3].nlsf_q15 = nlsf_q15.clone();
        } else {
            for subframe in frame.subframes.iter_mut() {
                subframe.nlsf_q15 = nlsf_q15.clone();
            }
        }

        ch_state.prev_nlsf_q15 = nlsf_q15;

        return Ok(());
    }

    fn decode_lsf_stage1(&self, decoder: &mut impl RangeDecoder, signal_type: SignalType) -> Result<u32> {
        let icdf: &[u32] = match (self.state.bandwidth, signal_type) {
            (Bandwidth::NarrowBand | Bandwidth::MediumBand, SignalType::Voiced) => {
                &constant::ICDF_NORMALIZED_LSF_STAGE_ONE_INDEX_NARROWBAND_OR_MEDIUMBAND_VOICED
            }
            (Bandwidth::NarrowBand | Bandwidth::MediumBand, SignalType::Inactive | SignalType::Unvoiced) => {
                &constant::ICDF_NORMALIZED_LSF_STAGE_ONE_INDEX_NARROWBAND_OR_MEDIUMBAND_UNVOICED
            }
            (Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand, SignalType::Voiced) => {
                &constant::ICDF_NORMALIZED_LSF_STAGE_ONE_INDEX_WIDEBAND_VOICED
            }
            (Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand, SignalType::Inactive | SignalType::Unvoiced) => {
                &constant::ICDF_NORMALIZED_LSF_STAGE_ONE_INDEX_WIDEBAND_UNVOICED
            }
        };

        return decoder.decode_symbol_with_icdf(icdf);
    }

    fn decode_lsf_stage2<R: RangeDecoder>(&self, decoder: &mut R, i1: u32) -> Result<(usize, Vec<i16>)> {
        let d_lpc = self.state.lpc_order;
        let i1_idx = i1 as usize;
        if i1_idx >= 32 {
            return Err(Error::InvalidLSFCoefficients.into());
        }

        let codebook_selector: &[u8] = match self.state.bandwidth {
            Bandwidth::NarrowBand | Bandwidth::MediumBand => {
                &constant::CODEBOOK_NORMALIZED_LSF_STAGE_TWO_INDEX_NARROWBAND_OR_MEDIUMBAND[i1_idx]
            }
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => {
                &constant::CODEBOOK_NORMALIZED_LSF_STAGE_TWO_INDEX_WIDEBAND[i1_idx]
            }
        };

        let mut stage2_indices = vec![0i16; d_lpc];
        for (k, idx_out) in stage2_indices.iter_mut().enumerate() {
            let codebook_idx = codebook_selector[k] as usize;
            let icdf = &constant::ICDF_NORMALIZED_LSF_STAGE_TWO_INDEX[codebook_idx];

            let symbol = decoder.decode_symbol_with_icdf(icdf)?;
            let mut idx = (symbol as i16) - 4;

            if idx == -4 || idx == 4 {
                let extension = decoder.decode_symbol_with_icdf(&constant::ICDF_NORMALIZED_LSF_STAGE_TWO_INDEX_EXTENSION)? as i16;
                idx += if idx < 0 { -extension } else { extension };
            }

            *idx_out = idx;
        }

        let qstep_q16 = match self.state.bandwidth {
            Bandwidth::NarrowBand | Bandwidth::MediumBand => {
                constant::LSF_STAGE_TWO_QUANTIZATION_STEP_Q16_NB_MB
            }
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => {
                constant::LSF_STAGE_TWO_QUANTIZATION_STEP_Q16_WB
            }
        };

        let mut res_q10 = vec![0i16; d_lpc];
        for k in (0..d_lpc).rev() {
            let pred_q10 = if k + 1 < d_lpc {
                let pred_q8 = match self.state.bandwidth {
                    Bandwidth::NarrowBand | Bandwidth::MediumBand => {
                        let sel = constant::PREDICTION_WEIGHT_SELECTION_NB_MB[i1_idx][k] as usize;
                        constant::PREDICTION_WEIGHTS_NB_MB[sel][k] as i32
                    }
                    Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => {
                        let sel = constant::PREDICTION_WEIGHT_SELECTION_WB[i1_idx][k] as usize;
                        constant::PREDICTION_WEIGHTS_WB[sel][k] as i32
                    }
                };
                ((res_q10[k + 1] as i32) * pred_q8) >> 8
            } else {
                0
            };

            let i2_k = stage2_indices[k] as i32;
            let out_q10 = if i2_k > 0 {
                (i2_k << 10) - constant::LSF_STAGE_TWO_QUANTIZATION_LEVEL_OFFSET_Q10
            } else if i2_k < 0 {
                (i2_k << 10) + constant::LSF_STAGE_TWO_QUANTIZATION_LEVEL_OFFSET_Q10
            } else {
                0
            };

            res_q10[k] = (pred_q10 + ((((out_q10 as i64) * (qstep_q16 as i64)) >> 16) as i32)) as i16;
        }

        return Ok((d_lpc, res_q10));
    }

    fn reconstruct_nlsf(&self, d_lpc: usize, res_q10: &[i16], i1: u32) -> Result<Vec<i16>> {
        let i1_idx = i1 as usize;
        if i1_idx >= 32 {
            return Err(Error::InvalidLSFCoefficients.into());
        }

        let cb1_q8: &[u8] = match self.state.bandwidth {
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => {
                &constant::CODEBOOK_NORMALIZED_LSF_STAGE_ONE_WIDEBAND[i1_idx]
            }
            Bandwidth::NarrowBand | Bandwidth::MediumBand => {
                &constant::CODEBOOK_NORMALIZED_LSF_STAGE_ONE_NARROWBAND_OR_MEDIUMBAND[i1_idx]
            }
        };

        let mut nlsf_q15 = vec![0i16; d_lpc];
        for k in 0..d_lpc {
            let cb1_cur = cb1_q8[k] as i32;
            let cb1_prev = if k == 0 { 0 } else { cb1_q8[k - 1] as i32 };
            let cb1_next = if k + 1 == d_lpc { 256 } else { cb1_q8[k + 1] as i32 };

            let diff_prev = (cb1_cur - cb1_prev).max(1);
            let diff_next = (cb1_next - cb1_cur).max(1);
            let w2_q18 = ((1024 / diff_prev) + (1024 / diff_next)) << 16;

            let i = 32 - (w2_q18 as u32).leading_zeros() as i32;
            let f = (w2_q18 >> (i - 8)) & 0x7F;
            let y = (if (i & 1) != 0 { 32768 } else { 46214 }) >> ((32 - i) >> 1);
            let w_q9 = (y + ((((213 * f * y) as i64) >> 16) as i32)).max(1);

            let nlsf = ((cb1_cur << 7) + (((res_q10[k] as i32) << 14) / w_q9)).clamp(0, 32767);
            nlsf_q15[k] = nlsf as i16;
        }

        return Ok(nlsf_q15);
    }

    fn stabilize_nlsf(&self, nlsf_q15: &[i16]) -> Result<Vec<i16>> {
        let d_lpc = nlsf_q15.len();
        let mut stable_nlsf = nlsf_q15.to_vec();
        let min_delta: &[i32] = match self.state.bandwidth {
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => {
                &constant::MINIMUM_SPACING_NORMALIZED_LSF_WB
            }
            Bandwidth::NarrowBand | Bandwidth::MediumBand => {
                &constant::MINIMUM_SPACING_NORMALIZED_LSF_NARROWBAND_MEDIUMBAND
            }
        };

        if d_lpc == 0 || min_delta.len() < d_lpc + 1 {
            return Err(Error::InvalidLSFCoefficients.into());
        }

        const MAX_STABILIZATION_ITERATIONS: usize = 20;
        let mut loops = 0;
        while loops < MAX_STABILIZATION_ITERATIONS {
            let mut min_diff = stable_nlsf[0] as i32 - min_delta[0];
            let mut min_diff_index = 0usize;

            for i in 1..d_lpc {
                let diff = stable_nlsf[i] as i32 - (stable_nlsf[i - 1] as i32 + min_delta[i]);
                if diff < min_diff {
                    min_diff = diff;
                    min_diff_index = i;
                }
            }

            let upper_diff = (1 << 15) - (stable_nlsf[d_lpc - 1] as i32 + min_delta[d_lpc]);
            if upper_diff < min_diff {
                min_diff = upper_diff;
                min_diff_index = d_lpc;
            }

            if min_diff >= 0 {
                return Ok(stable_nlsf);
            }

            if min_diff_index == 0 {
                stable_nlsf[0] = min_delta[0] as i16;
            } else if min_diff_index == d_lpc {
                stable_nlsf[d_lpc - 1] = ((1 << 15) - min_delta[d_lpc]) as i16;
            } else {
                let min_center_q15: i32 = min_delta[0..min_diff_index].iter().sum::<i32>()
                    + (min_delta[min_diff_index] >> 1);
                let max_center_q15: i32 = (1 << 15)
                    - min_delta[(min_diff_index + 1)..=d_lpc].iter().sum::<i32>()
                    - (min_delta[min_diff_index] - (min_delta[min_diff_index] >> 1));

                let raw_center = (stable_nlsf[min_diff_index - 1] as i32 + stable_nlsf[min_diff_index] as i32 + 1) >> 1;
                let center_freq_q15 = raw_center.clamp(min_center_q15, max_center_q15);

                stable_nlsf[min_diff_index - 1] = (center_freq_q15 - (min_delta[min_diff_index] >> 1)) as i16;
                stable_nlsf[min_diff_index] = (stable_nlsf[min_diff_index - 1] as i32 + min_delta[min_diff_index]) as i16;
            }

            loops += 1;
        }

        // Fallback after MAX_STABILIZATION_ITERATIONS: sort and enforce lower/upper spacing bounds
        stable_nlsf.sort_unstable();
        for i in 0..d_lpc {
            let prev = if i == 0 { 0 } else { stable_nlsf[i - 1] as i32 };
            stable_nlsf[i] = (stable_nlsf[i] as i32).max(prev + min_delta[i]) as i16;
        }
        for i in (0..d_lpc).rev() {
            let next = if i + 1 == d_lpc { 1 << 15 } else { stable_nlsf[i + 1] as i32 };
            stable_nlsf[i] = (stable_nlsf[i] as i32).min(next - min_delta[i + 1]) as i16;
        }

        return Ok(stable_nlsf);
    }

    /// Decodes subframe pitch lags (primary lag and pitch contour) for a voiced SILK frame
    /// (RFC 6716 Section 4.2.7.6.1).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6.1>
    fn decode_pitch_lags<R: RangeDecoder>(
        &mut self,
        decoder: &mut R,
        channel: usize,
        coding_mode: CodingMode,
        silk_frame_duration: FrameDuration,
    ) -> Result<Vec<u16>> {
        let num_subframes = SubframeSize::from(silk_frame_duration);
        let (min_lag, max_lag) = Self::pitch_lag_bounds(self.state.bandwidth);

        let lag_index = self.decode_primary_lag_index(decoder, channel, coding_mode)?;
        let base_lag = min_lag + lag_index;

        let contour: &[i8] = match (self.state.bandwidth, silk_frame_duration) {
            (Bandwidth::NarrowBand, FrameDuration::Ms10) => {
                let idx = decoder.decode_symbol_with_icdf(&constant::ICDF_SUBFRAME_PITCH_CONTOUR_NARROWBAND_10_MS)? as usize;
                &constant::CODEBOOK_SUBFRAME_PITCH_CONTOUR_NB_10MS[idx]
            }
            (Bandwidth::NarrowBand, _) => {
                let idx = decoder.decode_symbol_with_icdf(&constant::ICDF_SUBFRAME_PITCH_CONTOUR_NARROWBAND_20_MS)? as usize;
                &constant::CODEBOOK_SUBFRAME_PITCH_CONTOUR_NARROWBAND_20MS[idx]
            }
            (Bandwidth::MediumBand | Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand, FrameDuration::Ms10) => {
                let idx = decoder.decode_symbol_with_icdf(&constant::ICDF_SUBFRAME_PITCH_CONTOUR_MEDIUMBAND_OR_WIDEBAND_10_MS)? as usize;
                &constant::CODEBOOK_SUBFRAME_PITCH_CONTOUR_MEDIUMBAND_WIDEBAND_10MS[idx]
            }
            (Bandwidth::MediumBand | Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand, _) => {
                let idx = decoder.decode_symbol_with_icdf(&constant::ICDF_SUBFRAME_PITCH_CONTOUR_MEDIUMBAND_OR_WIDEBAND_20_MS)? as usize;
                &constant::CODEBOOK_SUBFRAME_PITCH_CONTOUR_MEDIUMBAND_WIDEBAND_20MS[idx]
            }
        };

        let mut pitch_lags = Vec::with_capacity(num_subframes);
        for &offset in contour.iter().take(num_subframes) {
            let lag = (base_lag + (offset as i32)).clamp(min_lag, max_lag) as u16;
            pitch_lags.push(lag);
        }

        return Ok(pitch_lags);
    }

    fn pitch_lag_bounds(bandwidth: Bandwidth) -> (i32, i32) {
        return match bandwidth {
            Bandwidth::NarrowBand => (16, 144),
            Bandwidth::MediumBand => (24, 216),
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => (32, 288),
        };
    }

    fn decode_primary_lag_index<R: RangeDecoder>(
        &mut self,
        decoder: &mut R,
        channel: usize,
        coding_mode: CodingMode,
    ) -> Result<i32> {
        let prev_signal_type = self.state.channel_states[channel].prev_signal_type;
        let prev_lag_index = self.state.channel_states[channel].prev_lag_index as i32;

        let mut lag_index = 0i32;
        let decode_absolute = if coding_mode == CodingMode::CodeConditionally && prev_signal_type == SignalType::Voiced {
            let delta_lag_index = decoder.decode_symbol_with_icdf(&constant::ICDF_RELATIVE_DELTA_PITCH_LAG)? as i32;
            if delta_lag_index > 0 {
                lag_index = prev_lag_index + delta_lag_index - 9;
                false
            } else {
                true
            }
        } else {
            true
        };

        if decode_absolute {
            let high_part = decoder.decode_symbol_with_icdf(&constant::ICDF_PRIMARY_PITCH_LAG_HIGH_PART)? as i32;
            let (low_icdf, scale): (&[u32], i32) = match self.state.bandwidth {
                Bandwidth::NarrowBand => (&constant::ICDF_PRIMARY_PITCH_LAG_LOW_PART_NARROWBAND, 4),
                Bandwidth::MediumBand => (&constant::ICDF_PRIMARY_PITCH_LAG_LOW_PART_MEDIUMBAND, 6),
                Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => {
                    (&constant::ICDF_PRIMARY_PITCH_LAG_LOW_PART_WIDEBAND, 8)
                }
            };
            let low_part = decoder.decode_symbol_with_icdf(low_icdf)? as i32;
            lag_index = high_part * scale + low_part;
        }

        self.state.channel_states[channel].prev_lag_index = lag_index as i16;
        return Ok(lag_index);
    }

    fn decode_ltp_coeffs<R: RangeDecoder>(
        &self,
        decoder: &mut R,
        silk_frame_duration: FrameDuration,
    ) -> Result<Vec<Vec<i8>>> {
        let num_subframes = SubframeSize::from(silk_frame_duration);
        let mut ltp_coeffs = vec![vec![0i8; 5]; num_subframes];

        let periodicity_index = decoder.decode_symbol_with_icdf(&constant::ICDF_PERIODICITY_INDEX)?;

        for subframe in ltp_coeffs.iter_mut() {
            let filter_index = match periodicity_index {
                0 => decoder.decode_symbol_with_icdf(&constant::ICDF_LTP_FILTER_INDEX_0)?,
                1 => decoder.decode_symbol_with_icdf(&constant::ICDF_LTP_FILTER_INDEX_1)?,
                2 => decoder.decode_symbol_with_icdf(&constant::ICDF_LTP_FILTER_INDEX_2)?,
                _ => return Err(Error::InvalidPeriodicityIndex.into()),
            } as usize;

            let filter = match periodicity_index {
                0 => constant::CODEBOOK_LTP_FILTER_PERIODICITY_INDEX_0[filter_index],
                1 => constant::CODEBOOK_LTP_FILTER_PERIODICITY_INDEX_1[filter_index],
                2 => constant::CODEBOOK_LTP_FILTER_PERIODICITY_INDEX_2[filter_index],
                _ => return Err(Error::InvalidPeriodicityIndex.into()),
            };

            subframe.copy_from_slice(&filter);
        }

        return Ok(ltp_coeffs);
    }

    fn decode_ltp_scaling<R: RangeDecoder>(
        &self,
        decoder: &mut R,
        coding_mode: CodingMode,
    ) -> Result<(i16, f32)> {
        const LTP_SCALE_Q14_DIVISOR: f32 = 16384.0;

        let i = if coding_mode == CodingMode::CodeIndependently {
            decoder.decode_symbol_with_icdf(&constant::ICDF_LTP_SCALING_PARAMETER)? as usize
        } else {
            0
        };

        let scale_q14 = *constant::CODEBOOK_LTP_SCALING_Q14
            .get(i)
            .ok_or(Error::InvalidLTPScalingIndex)?;

        return Ok((scale_q14, scale_q14 as f32 / LTP_SCALE_Q14_DIVISOR));
    }

    /// Decodes Long-Term Prediction (LTP) parameters (RFC 6716 Section 4.2.7.6).
    ///
    /// This includes pitch lags, LTP coefficients, and conditional LTP scaling.
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6>
    fn decode_ltp<R: RangeDecoder>(
        &mut self,
        decoder: &mut R,
        frame: &mut Frame,
        channel: usize,
        coding_mode: CodingMode,
        silk_frame_duration: FrameDuration,
    ) -> Result<()> {
        let pitch_lags = self.decode_pitch_lags(decoder, channel, coding_mode, silk_frame_duration)?;
        let ltp_coeffs = self.decode_ltp_coeffs(decoder, silk_frame_duration)?;
        let (ltp_scale_q14, ltp_scale) = self.decode_ltp_scaling(decoder, coding_mode)?;

        if pitch_lags.len() != frame.subframes.len() || ltp_coeffs.len() != frame.subframes.len() {
            return Err(Error::MismatchFrameSubframes.into());
        }

        frame.ltp_scale_q14 = ltp_scale_q14;
        frame.ltp_scale = ltp_scale;

        frame.subframes.iter_mut()
            .zip(pitch_lags.iter().zip(ltp_coeffs.iter()))
            .for_each(|(subframe, (&pitch_lag, ltp_coeff))| {
                subframe.pitch_lag = pitch_lag;
                subframe.ltp_coeffs = ltp_coeff.to_vec();
            });

        return Ok(());
    }

    fn ltp_synthesis(excitation: &[f32], pitch_lag: u16, ltp_coeffs: &[i8], ltp_scale: f32) -> Vec<f32> {
        let mut ltp_signal = vec![0.0; excitation.len()];

        for i in 0..excitation.len() {
            let mut pred = 0.0;
            for (j, &coeff) in ltp_coeffs.iter().enumerate() {
                if i >= pitch_lag as usize + j {
                    pred += ltp_signal[i - pitch_lag as usize - j] * (coeff as f32 / 128.0);
                }
            }
            ltp_signal[i] = excitation[i] + ltp_scale * pred;
        }

        return ltp_signal;
    }

    /// Decodes quantization gains for all subframes in a SILK frame (RFC 6716 Section 4.2.7.4).
    ///
    /// Subframe 0 is coded independently when `coding_mode != CodingMode::CodeConditionally`,
    /// and via a delta quantization index otherwise. Subframes `1..num_subframes` are always
    /// coded via delta quantization indices relative to the previous subframe's `log_gain`.
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>
    fn decode_gains<R: RangeDecoder>(
        &mut self,
        decoder: &mut R,
        frame: &mut Frame,
        channel: usize,
        coding_mode: CodingMode,
    ) -> Result<()> {
        let signal_type = frame.frame_type.signal_type;
        for (subframe_idx, subframe) in frame.subframes.iter_mut().enumerate() {
            let is_independent = subframe_idx == 0 && coding_mode != CodingMode::CodeConditionally;
            let (log_gain, gain_q16, gain) = self.decode_subframe_gain(
                decoder,
                channel,
                signal_type,
                is_independent,
            )?;
            subframe.log_gain = log_gain;
            subframe.gain_q16 = gain_q16;
            subframe.gain = gain;
        }

        return Ok(());
    }

    fn decode_subframe_gain<R: RangeDecoder>(
        &mut self,
        decoder: &mut R,
        channel: usize,
        signal_type: SignalType,
        is_independent: bool,
    ) -> Result<(i32, i32, f32)> {
        let prev_log_gain = self.state.channel_states[channel].last_gain_index;

        let log_gain = if is_independent {
            let icdf_gain_msb: &[u32] = match signal_type {
                SignalType::Inactive => &constant::ICDF_INDEPENDENT_QUANTIZATION_GAIN_MSB_INACTIVE,
                SignalType::Unvoiced => &constant::ICDF_INDEPENDENT_QUANTIZATION_GAIN_MSB_UNVOICED,
                SignalType::Voiced => &constant::ICDF_INDEPENDENT_QUANTIZATION_GAIN_MSB_VOICED,
            };

            let gain_msb = decoder.decode_symbol_with_icdf(icdf_gain_msb)? as i32;
            let gain_lsb = decoder.decode_symbol_with_icdf(&constant::ICDF_INDEPENDENT_QUANTIZATION_GAIN_LSB)? as i32;
            let gain_index = (gain_msb << 3) + gain_lsb;

            gain_index.max(prev_log_gain - 16).clamp(0, 63)
        } else {
            let delta_gain = decoder.decode_symbol_with_icdf(&constant::ICDF_DELTA_QUANTIZATION_GAIN)? as i32;

            (2 * delta_gain - 16)
                .max(prev_log_gain + delta_gain - 4)
                .clamp(0, 63)
        };

        self.state.channel_states[channel].last_gain_index = log_gain;

        let gain_q16 = Self::quantized_log_gain_to_linear_q16(log_gain);
        let gain = gain_q16 as f32 / 65536.0;

        return Ok((log_gain, gain_q16, gain));
    }

    /// Converts a quantized logarithmic gain index (`0..=63`) to a Q16 linear gain
    /// using the `silk_gains_dequant` and `silk_log2lin` approximation (RFC 6716 Section 4.2.7.4).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>
    fn quantized_log_gain_to_linear_q16(log_gain: i32) -> i32 {
        const INV_SCALE_Q16: i64 = 1_907_828;
        const OFFSET_Q7: i32 = 2090;

        let in_log_q7 = (((INV_SCALE_Q16 * (log_gain as i64)) >> 16) as i32 + OFFSET_Q7).min(3967);
        return Self::silk_log2lin(in_log_q7);
    }

    /// Evaluates the SILK base-2 exponential approximation `2^(in_log_q7 / 128.0)`.
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>
    fn silk_log2lin(in_log_q7: i32) -> i32 {
        if in_log_q7 < 0 {
            return 0;
        }
        if in_log_q7 >= 3968 {
            return i32::MAX;
        }

        let out = 1_i32 << (in_log_q7 >> 7);
        let frac_q7 = in_log_q7 & 0x7F;
        let parabolic = frac_q7 + ((((frac_q7 * (128 - frac_q7)) as i64 * -174) >> 16) as i32);

        if in_log_q7 < 2048 {
            return out + ((out * parabolic) >> 7);
        } else {
            return out + (out >> 7) * parabolic;
        }
    }

    /// Decodes the excitation signal.
    ///
    /// The excitation is coded using a modified version of the Pyramid Vector Quantizer (PVQ).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8>
    fn decode_excitation<R: RangeDecoder>(
        &mut self,
        decoder: &mut R,
        frame: &mut Frame,
    ) -> Result<()> {
        let rate_level = self.decode_rate_level(decoder, frame.frame_type.signal_type)?;
        let (pulse_counts, lsb_counts) = self.decode_pulse_counts(decoder, rate_level)?;
        let lcg_seed = decoder.decode_symbol_with_icdf(&constant::ICDF_LINEAR_CONGRUENTIAL_GENERATOR_SEED)?;

        for subframe in frame.subframes.iter_mut() {
            let pulse_locations = self.decode_pulse_locations(decoder, pulse_counts, lsb_counts)?;
            subframe.excitation = pulse_locations;

            self.decode_excitation_signs(
                decoder,
                &mut subframe.excitation,
                frame.frame_type.signal_type,
                frame.frame_type.quantization_offset_type,
                pulse_counts,
            )?;

            self.apply_sign_and_scaling(
                &mut subframe.excitation,
                frame.frame_type,
                lcg_seed,
            )?;
        }

        return Ok(());
    }


    fn apply_sign_and_scaling(
        &self,
        excitation: &mut [f32],
        frame_type: FrameType,
        mut lcg_seed: u32,
    ) -> Result<()> {
        const SCALE_FACTOR: f32 = 256.0;
        const SIGN_OFFSET: i32 = 20;
        const LCG_MULTIPLIER: u32 = 196_314_165;
        const LCG_INCREMENT: u32 = 907_633_515;
        const LCG_SIGN_MASK: u32 = 0x8000_0000;

        let offset_q23 = self.get_quantization_offset(frame_type.signal_type, frame_type.quantization_offset_type) as f32;

        for sample in excitation.iter_mut() {
            let e_raw = *sample;
            let mut e_q23 = (e_raw * SCALE_FACTOR) - (e_raw.signum() * SIGN_OFFSET as f32) + offset_q23;

            lcg_seed = lcg_seed.wrapping_mul(LCG_MULTIPLIER).wrapping_add(LCG_INCREMENT);

            if (lcg_seed & LCG_SIGN_MASK) != 0 {
                e_q23 = -e_q23;
            }

            *sample = e_q23;
            lcg_seed = lcg_seed.wrapping_add(e_raw as u32);
        }

        return Ok(());
    }

    fn get_quantization_offset(&self, signal_type: SignalType, quantization_offset_type: QuantizationOffsetType) -> i32 {
        let signal_idx = match signal_type {
            SignalType::Inactive => 0,
            SignalType::Unvoiced => 1,
            SignalType::Voiced => 2,
        };
        let quant_idx = match quantization_offset_type {
            QuantizationOffsetType::Low => 0,
            QuantizationOffsetType::High => 1,
        };
        return constant::QUANTIZATION_OFFSETS[signal_idx][quant_idx];
    }
    fn decode_rate_level<R: RangeDecoder>(&self, decoder: &mut R, signal_type: SignalType) -> Result<u32> {
        let icdf = match signal_type {
            SignalType::Voiced => &constant::ICDF_RATE_LEVEL_VOICED,
            SignalType::Unvoiced | SignalType::Inactive => &constant::ICDF_RATE_LEVEL_UNVOICED,
        };

        return decoder.decode_symbol_with_icdf(icdf);
    }

    fn decode_pulse_counts<R: RangeDecoder>(&self, decoder: &mut R, rate_level: u32) -> Result<(u8, u8)> {
        const MAX_PULSE_COUNT: u32 = 17;
        const MAX_LSB_COUNT: u8 = 10;

        let mut count = decoder.decode_symbol_with_icdf(&constant::ICDF_PULSE_COUNT[rate_level as usize])?;
        let mut lsb_count = 0u8;

        while count == MAX_PULSE_COUNT && lsb_count < MAX_LSB_COUNT {
            count = decoder.decode_symbol_with_icdf(&constant::ICDF_PULSE_COUNT[9])?;
            lsb_count += 1;
        }

        if lsb_count == MAX_LSB_COUNT {
            count = decoder.decode_symbol_with_icdf(&constant::ICDF_PULSE_COUNT[MAX_LSB_COUNT as usize])?;
        }

        return Ok((count as u8, lsb_count));
    }

    fn decode_pulse_locations<R: RangeDecoder>(
        &self,
        decoder: &mut R,
        pulse_count: u8,
        lsb_count: u8,
    ) -> Result<Vec<f32>> {
        const SHELL_BLOCK_SIZE: usize = 16;
        const MAX_PULSE_COUNT: u8 = 17;

        let mut excitation = Vec::with_capacity(SHELL_BLOCK_SIZE);

        if pulse_count == 0 {
            excitation.extend(std::iter::repeat(0.0).take(SHELL_BLOCK_SIZE));
            return Ok(excitation);
        }

        let mut pulse_partitions = [pulse_count; SHELL_BLOCK_SIZE];

        for &block_size in &[16, 8, 4, 2] {
            let half_block_size = block_size / 2;

            for i in (0..SHELL_BLOCK_SIZE).step_by(block_size) {
                let icdf = match block_size {
                    16 => &constant::ICDF_PULSE_COUNT_SPLIT_16_SAMPLE_PARTITIONS[pulse_partitions[i] as usize],
                    8 => &constant::ICDF_PULSE_COUNT_SPLIT_8_SAMPLE_PARTITIONS[pulse_partitions[i] as usize],
                    4 => &constant::ICDF_PULSE_COUNT_SPLIT_4_SAMPLE_PARTITIONS[pulse_partitions[i] as usize],
                    2 => &constant::ICDF_PULSE_COUNT_SPLIT_2_SAMPLE_PARTITIONS[pulse_partitions[i] as usize],
                    _ => return Err(Error::InvalidPartitionSize.into()),
                };

                let left_pulses = decoder.decode_symbol_with_icdf(icdf)? as u8;
                let right_pulses = pulse_partitions[i].saturating_sub(left_pulses);

                pulse_partitions[i] = left_pulses;
                pulse_partitions[i + half_block_size] = right_pulses;
            }
        }

        for partition in pulse_partitions.iter() {
            let mut value = *partition as f32;

            if pulse_count == MAX_PULSE_COUNT {
                for _ in 0..lsb_count {
                    let lsb = decoder.decode_symbol_with_icdf(&constant::ICDF_EXCITATION_LSB)?;
                    value = value * 2.0 + lsb as f32;
                }
            }

            excitation.push(value);
        }

        return Ok(excitation);
    }


    fn decode_excitation_signs<R: RangeDecoder>(
        &self,
        decoder: &mut R,
        excitation: &mut [f32],
        signal_type: SignalType,
        quantization_offset_type: QuantizationOffsetType,
        pulse_count: u8,
    ) -> Result<()> {
        if excitation.is_empty() {
            return Ok(());
        }

        let signal_idx = match signal_type {
            SignalType::Inactive => 0,
            SignalType::Unvoiced => 1,
            SignalType::Voiced => 2,
        };
        let quant_idx = match quantization_offset_type {
            QuantizationOffsetType::Low => 0,
            QuantizationOffsetType::High => 1,
        };
        let pulse_idx = (pulse_count as usize).min(6);
        let icdf = &constant::ICDF_EXCITATION_SIGN[signal_idx][quant_idx][pulse_idx];

        for sample in excitation.iter_mut() {
            if *sample != 0.0 {
                let sign = decoder.decode_symbol_with_icdf(icdf)?;
                if sign == 0 {
                    *sample = -*sample;
                }
            }
        }

        return Ok(());
    }


    fn lpc_synthesis(lpc_coeffs: &[i32], excitation: &[f32], subframe_size: usize) -> Vec<f32> {
        let order = lpc_coeffs.len();
        let mut output = vec![0.0; subframe_size];

        for i in 0..subframe_size {
            let mut y = excitation[i];
            for j in 0..order.min(i) {
                y -= lpc_coeffs[j] as f32 / 4096.0 * output[i - j - 1];
            }
            output[i] = y;
        }

        return output;
    }

    fn lsf_to_lpc(lsf_q15: &[i16], bandwidth: Bandwidth) -> Result<Vec<i32>> {
        let order = lsf_q15.len();
        let lsf_ordering: &[u8] = match bandwidth {
            Bandwidth::NarrowBand | Bandwidth::MediumBand => &constant::LSF_ORDERING_POLYNOMIAL_EVALUATION_NB_MB,
            _ => &constant::LSF_ORDERING_POLYNOMIAL_EVALUATION_WB,
        };

        let mut lpc_coeffs = vec![0; order];
        let mut p = vec![1 << 16; order + 1];
        let mut q = vec![1 << 16; order + 1];

        for &index in lsf_ordering.iter().take(order) {
            let f = constant::Q12_COSINE_TABLE_FOR_LSF_CONVERSION[(lsf_q15[index as usize] >> 5) as usize];

            let update_vector = |vec: &mut Vec<i32>| {
                let mut prev = vec[0];
                vec[0] = (((vec[0] as i64) * ((1 << 16) - f as i64)) >> 16) as i32;
                for elem in vec.iter_mut().take(order + 1).skip(1) {
                    let tmp = *elem;
                    *elem = (((tmp as i64) * ((1 << 16) - f as i64)) >> 16) as i32 - prev;
                    prev = tmp;
                }
            };

            update_vector(&mut p);
            update_vector(&mut q);
        }

        for i in 0..order {
            lpc_coeffs[i] = -((q[i + 1] - q[i]) - (p[i + 1] - p[i]) + (1 << 7)) >> 8;
        }

        let gamma = match bandwidth {
            Bandwidth::NarrowBand => 0.98,
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => 0.99,
            Bandwidth::MediumBand => 1.0,
        } as f32;

        for (i, coeff) in lpc_coeffs.iter_mut().enumerate() {
            *coeff = (*coeff as f32 * gamma.powi(i as i32)) as i32;
        }

        return Ok(lpc_coeffs);
    }


    fn synthesize_frame(&mut self, frame: &Frame) -> Result<()> {
        let samples_per_subframe = frame.sample_count / frame.subframes.len();
        let channels = self.buffer.spec().channels().count();

        if self.buffer.frames() + frame.sample_count > self.buffer.capacity() {
            return Err(Error::BufferOverflow.into());
        }

        let start = self.buffer.frames();

        // Mark frames as committed by updating the internal frame count before accessing plane_mut
        self.buffer.render_uninit(Some(frame.sample_count));

        for ch in 0..channels {
            let dst = self.buffer.plane_mut(ch).ok_or(Error::BufferOverflow)?;
            for (s, subframe) in frame.subframes.iter().enumerate() {
                let offset = start + s * samples_per_subframe;

                let ltp_signal = Self::ltp_synthesis(
                    &subframe.excitation,
                    subframe.pitch_lag,
                    &subframe.ltp_coeffs,
                    frame.ltp_scale,
                );

                let lpc_coeffs = Self::lsf_to_lpc(&subframe.nlsf_q15, self.state.bandwidth)?;
                let lpc_signal = Self::lpc_synthesis(&lpc_coeffs, &ltp_signal, samples_per_subframe);

                for i in 0..samples_per_subframe {
                    dst[offset + i] = ltp_signal[i] + lpc_signal[i];
                }
            }
        }

        return Ok(());
    }
}

/// Decoded SILK header flags for a single Opus frame (10, 20, 40, or 60 ms).
///
/// ```text
/// +---------------------------------------------------------------+
/// | Channel c in 0..channels (1 = Mono, 2 = Mid/Side Stereo)      |
/// |   +-------------------------------------------------------+   |
/// |   | VAD flags: 1 bit per SILK frame (1 for 10/20 ms,      |   |
/// |   |            2 for 40 ms, 3 for 60 ms)                  |   |
/// |   +-------------------------------------------------------+   |
/// |   | LBRR flag: 1 bit indicating LBRR presence in channel  |   |
/// |   +-------------------------------------------------------+   |
/// +---------------------------------------------------------------+
/// | Per-Frame LBRR Flags (for each channel c where lbrr_flag[c]): |
/// |   - 10/20 ms: implicit frame 0 active (0 range coder bits)    |
/// |   - 40 ms:    Table 4 2-bit mask (1..=3)                      |
/// |   - 60 ms:    Table 4 3-bit mask (1..=7)                      |
/// +---------------------------------------------------------------+
/// ```
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.3>
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.4>
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SilkHeaderFlags {
    pub num_channels: usize,
    pub num_silk_frames: usize,
    pub vad_flags: [[bool; 3]; 2],
    pub lbrr_flags: [bool; 2],
    pub per_frame_lbrr: [[bool; 3]; 2],
}

impl SilkHeaderFlags {
    /// Returns `true` if any channel has LBRR data enabled in this Opus frame.
    pub fn has_any_lbrr(&self) -> bool {
        return self.lbrr_flags[0] || (self.num_channels > 1 && self.lbrr_flags[1]);
    }

    /// Returns `true` if SILK sub-frame `frame_idx` (`0..num_silk_frames`) has LBRR data
    /// in either the mid (mono) or side channel.
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
#[derive(Debug, Default)]
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
#[derive(Debug, Clone)]
pub struct ChannelState {
    pub last_gain_index: i32,
    pub prev_nlsf_q15: Vec<i16>,
    pub first_frame_after_reset: bool,
    pub prev_signal_type: SignalType,
    pub prev_lag_index: i16,
}

impl Default for ChannelState {
    fn default() -> Self {
        return Self::new(16);
    }
}

impl ChannelState {
    pub fn new(lpc_order: usize) -> Self {
        return Self {
            last_gain_index: 10,
            prev_nlsf_q15: vec![0i16; lpc_order],
            first_frame_after_reset: true,
            prev_signal_type: SignalType::Inactive,
            prev_lag_index: 0,
        };
    }

    pub fn reset(&mut self, lpc_order: usize) {
        self.last_gain_index = 10;
        self.prev_nlsf_q15.clear();
        self.prev_nlsf_q15.resize(lpc_order, 0);
        self.first_frame_after_reset = true;
        self.prev_signal_type = SignalType::Inactive;
        self.prev_lag_index = 0;
    }

    pub fn reset_after_mid_only(&mut self) {
        self.last_gain_index = 10;
        self.first_frame_after_reset = true;
        self.prev_signal_type = SignalType::Inactive;
        self.prev_lag_index = 0;
    }
}

/// SILK Decoder State
///
/// This structure maintains the state of the SILK decoder across frames,
/// including information about the previous frame and current configuration.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>
#[derive(Debug, Default, Clone)]
pub struct State {
    sample_rate: u32,
    channels: Channels,
    frame_size: FrameDuration,
    bandwidth: Bandwidth,
    prev_frame_type: FrameType,
    prev_samples: Vec<f32>,
    lbrr_flag: bool,
    lpc_order: usize,
    channel_states: [ChannelState; 2],
    prev_decode_only_middle: bool,
}


impl State {
    pub fn try_new(channels: Channels, frame_size: FrameDuration, bandwidth: Bandwidth) -> Result<Self> {
        let _ = Self::silk_frame_count(frame_size)?;
        let sample_rate = bandwidth.sample_rate();
        let frame_length = Self::calculate_frame_length(sample_rate, frame_size)?;
        let channel_count = channels.count();
        let lpc_order = LpcOrder::from(bandwidth);
        let prev_frame_type = FrameType::default();
        let prev_samples = vec![0.0; frame_length * channel_count];
        let lbrr_flag = false;
        let channel_states = [ChannelState::new(lpc_order), ChannelState::new(lpc_order)];
        let prev_decode_only_middle = false;

        return Ok(Self {
            sample_rate,
            channels,
            frame_size,
            bandwidth,
            prev_frame_type,
            prev_samples,
            lbrr_flag,
            lpc_order,
            channel_states,
            prev_decode_only_middle,
        });
    }

    pub fn reset(&mut self) {
        self.lbrr_flag = false;
        self.prev_samples.fill(0.0);
        self.prev_frame_type = FrameType::default();
        self.prev_decode_only_middle = false;
        for ch_state in self.channel_states.iter_mut() {
            ch_state.reset(self.lpc_order);
        }
    }

    /// Returns the number of 10 ms or 20 ms SILK frames contained in an Opus frame of `frame_size`.
    ///
    /// - `10 ms` and `20 ms` Opus frames contain `1` SILK frame.
    /// - `40 ms` Opus frames contain `2` 20-ms SILK frames.
    /// - `60 ms` Opus frames contain `3` 20-ms SILK frames.
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

    fn calculate_frame_length(sample_rate: u32, frame_size: FrameDuration) -> Result<usize> {
        let samples = (sample_rate as u128)
            .checked_mul(frame_size.duration().as_nanos())
            .and_then(|ns| ns.checked_div(1_000_000_000))
            .ok_or(Error::CalculationOverflow)?;

        return usize::try_from(samples).map_err(|_| Error::CalculationOverflow.into());
    }
}

/// SILK Frame
///
/// Represents a decoded SILK frame, containing all the relevant information
/// extracted from the bitstream.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7>
#[derive(Debug, Default)]
pub struct Frame {
    pub frame_type: FrameType,
    pub vad_flag: bool,
    pub lbrr_flag: bool,
    pub sample_count: usize,
    pub subframes: Vec<Subframe>,
    pub ltp_scale: f32,
    pub ltp_scale_q14: i16,
    pub lsf_interpolation_index: Option<u32>,
    pub stereo_weights: Option<StereoWeights>,
    pub mid_only: bool,
}

impl Frame {
    pub fn new(sample_count: usize, num_subframes: usize) -> Self {
        return Self {
            frame_type: FrameType::default(),
            vad_flag: false,
            lbrr_flag: false,
            sample_count,
            subframes: vec![Subframe::default(); num_subframes],
            ltp_scale: 0.0,
            ltp_scale_q14: 0,
            lsf_interpolation_index: None,
            stereo_weights: None,
            mid_only: false,
        };
    }
}

/// SILK Subframe
///
/// Represents a subframe within a SILK frame, containing the decoded parameters
/// specific to that subframe.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>
#[derive(Debug, Clone, Default)]
pub struct Subframe {
    pub gain: f32,
    pub gain_q16: i32,
    pub log_gain: i32,
    pub nlsf_q15: Vec<i16>,
    pub ltp_coeffs: Vec<i8>,
    pub pitch_lag: u16,
    pub excitation: Vec<f32>,
}

#[derive(Debug, Clone, Default, Copy, PartialEq, Eq)]
pub struct FrameType {
    signal_type: SignalType,
    quantization_offset_type: QuantizationOffsetType,
}
impl FrameType {
    pub fn new(signal_type: SignalType, quantization_offset_type: QuantizationOffsetType) -> Self {
        return Self { signal_type, quantization_offset_type };
    }
}

/// SILK Signal Type
///
/// Represents the type of signal in a SILK frame.
///
/// - Inactive: Silence or background noise
/// - Voiced: Speech with periodic character
/// - Unvoiced: Speech without periodic character
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.3>
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SignalType {
    #[default]
    Inactive,
    Voiced,
    Unvoiced,
}

/// SILK Quantization Offset Type
///
/// Represents the type of quantization offset used in a SILK frame.
///
/// - High: Higher offset, typically used for higher quality or bitrate
/// - Low: Lower offset, typically used for lower quality or bitrate
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.3>
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum QuantizationOffsetType {
    #[default]
    High,
    Low,
}

/// Represents the number of subframes in a SILK frame
///
/// This type alias and its implementation provide a mapping from the frame size
/// to the number of subframes it contains.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>
type SubframeSize = usize;
impl From<FrameDuration> for SubframeSize {
    /// Converts a FrameSize to the number of subframes it contains
    ///
    /// The number of subframes varies based on the frame duration:
    /// - 2.5 ms and 5 ms frames have 1 subframe
    /// - 10 ms frames have 2 subframes
    /// - 20 ms frames have 4 subframes
    /// - 40 ms frames have 8 subframes
    /// - 60 ms frames have 12 subframes
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

/// Represents the order of the Linear Predictive Coding (LPC) filter
///
/// This type alias and its implementation provide a mapping from the audio bandwidth
/// to the LPC order used in the SILK codec.
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5>
type LpcOrder = usize;
impl From<Bandwidth> for LpcOrder {
    fn from(value: Bandwidth) -> Self {
        return match value {
            Bandwidth::NarrowBand | Bandwidth::MediumBand => 10,
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => 16,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use symphonia_core::audio::Position;
    use symphonia_core::codecs::audio::well_known::CODEC_ID_OPUS;

    /// Deterministic scripted `RangeDecoder` for testing exact RFC 6716 symbol sequences.
    struct MockRangeDecoder {
        logp_bits: VecDeque<u32>,
        icdf_symbols: VecDeque<u32>,
    }

    impl MockRangeDecoder {
        fn new(logp_bits: &[u32], icdf_symbols: &[u32]) -> Self {
            return Self {
                logp_bits: logp_bits.iter().copied().collect(),
                icdf_symbols: icdf_symbols.iter().copied().collect(),
            };
        }
    }

    impl RangeDecoder for MockRangeDecoder {
        fn decode_symbol_logp(&mut self, _logp: u32) -> Result<u32> {
            return Ok(self.logp_bits.pop_front().unwrap_or(0));
        }

        fn decode_symbol_with_icdf(&mut self, _icdf: &[u32]) -> Result<u32> {
            return Ok(self.icdf_symbols.pop_front().unwrap_or(0));
        }
    }

    fn create_test_silk_decoder(stereo: bool, frame_size: FrameDuration) -> Decoder {
        let mut params = AudioCodecParameters::new();
        let channels = if stereo {
            Channels::Positioned(Position::FRONT_LEFT | Position::FRONT_RIGHT)
        } else {
            Channels::Positioned(Position::FRONT_LEFT)
        };
        params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(channels.clone());

        let mut decoder = Decoder::try_new(params).unwrap();
        decoder.state = State::try_new(channels, frame_size, Bandwidth::NarrowBand).unwrap();
        return decoder;
    }

    #[test]
    fn silk_header_mono_10ms_and_20ms_flags() {
        let decoder = create_test_silk_decoder(false, FrameDuration::Ms20);

        // Mono 20 ms: [VAD_0 = 1, LBRR = 0] -> 2 header bits, no per-frame LBRR iCDF symbol
        let mut mock = MockRangeDecoder::new(&[1, 0], &[]);
        let header = decoder
            .decode_silk_header(&mut mock, false, FrameDuration::Ms20)
            .unwrap();

        assert_eq!(header.num_channels, 1);
        assert_eq!(header.num_silk_frames, 1);
        assert_eq!(header.vad_flags[0], [true, false, false]);
        assert!(!header.lbrr_flags[0]);
        assert_eq!(header.per_frame_lbrr[0], [false, false, false]);
        assert!(!header.has_any_lbrr());

        // Mono 10 ms with LBRR = 1: [VAD_0 = 0, LBRR = 1] -> implicit per_frame_lbrr[0][0] = true
        let mut mock_lbrr = MockRangeDecoder::new(&[0, 1], &[]);
        let header_lbrr = decoder
            .decode_silk_header(&mut mock_lbrr, false, FrameDuration::Ms10)
            .unwrap();

        assert!(header_lbrr.lbrr_flags[0]);
        assert_eq!(header_lbrr.per_frame_lbrr[0], [true, false, false]);
        assert!(header_lbrr.has_any_lbrr());
        assert!(header_lbrr.has_frame_lbrr(0));
    }

    #[test]
    fn silk_header_mono_40ms_and_60ms_per_frame_lbrr_masks() {
        let decoder = create_test_silk_decoder(false, FrameDuration::Ms40);

        // Mono 40 ms: [VAD_0 = 1, VAD_1 = 0, LBRR = 1], Table 4 symbol = 1 -> mask = 2 (0b10)
        let mut mock_40 = MockRangeDecoder::new(&[1, 0, 1], &[1]);
        let header_40 = decoder
            .decode_silk_header(&mut mock_40, false, FrameDuration::Ms40)
            .unwrap();

        assert_eq!(header_40.num_silk_frames, 2);
        assert_eq!(header_40.vad_flags[0], [true, false, false]);
        assert_eq!(header_40.per_frame_lbrr[0], [false, true, false]);
        assert!(!header_40.has_frame_lbrr(0));
        assert!(header_40.has_frame_lbrr(1));

        // Mono 60 ms: [VAD_0 = 1, VAD_1 = 1, VAD_2 = 0, LBRR = 1], Table 4 symbol = 4 -> mask = 5 (0b101)
        let mut mock_60 = MockRangeDecoder::new(&[1, 1, 0, 1], &[4]);
        let header_60 = decoder
            .decode_silk_header(&mut mock_60, false, FrameDuration::Ms60)
            .unwrap();

        assert_eq!(header_60.num_silk_frames, 3);
        assert_eq!(header_60.vad_flags[0], [true, true, false]);
        assert_eq!(header_60.per_frame_lbrr[0], [true, false, true]);
        assert!(header_60.has_frame_lbrr(0));
        assert!(!header_60.has_frame_lbrr(1));
        assert!(header_60.has_frame_lbrr(2));
    }

    #[test]
    fn silk_header_stereo_60ms_8_bits_and_dual_channel_lbrr() {
        let decoder = create_test_silk_decoder(true, FrameDuration::Ms60);

        // Stereo 60 ms reads 8 header bits:
        // Mid:  VAD = [1, 0, 1], LBRR = 1
        // Side: VAD = [0, 1, 0], LBRR = 1
        // Followed by 2 Table 4 symbols:
        // Mid symbol = 0 -> mask = 1 (0b001 -> frame 0)
        // Side symbol = 5 -> mask = 6 (0b110 -> frames 1 and 2)
        let mut mock = MockRangeDecoder::new(&[1, 0, 1, 1, 0, 1, 0, 1], &[0, 5]);
        let header = decoder
            .decode_silk_header(&mut mock, true, FrameDuration::Ms60)
            .unwrap();

        assert_eq!(header.num_channels, 2);
        assert_eq!(header.num_silk_frames, 3);
        assert_eq!(header.vad_flags[0], [true, false, true]);
        assert_eq!(header.vad_flags[1], [false, true, false]);
        assert_eq!(header.lbrr_flags, [true, true]);
        assert_eq!(header.per_frame_lbrr[0], [true, false, false]);
        assert_eq!(header.per_frame_lbrr[1], [false, true, true]);
        assert!(header.has_frame_lbrr(0));
        assert!(header.has_frame_lbrr(1));
        assert!(header.has_frame_lbrr(2));
    }

    #[test]
    fn silk_stereo_prediction_weights_rfc6716_formula() {
        let decoder = create_test_silk_decoder(true, FrameDuration::Ms20);

        // Choose n = 7 (n/5 = 1, n%5 = 2), i0 = 1, i1 = 2, i2 = 2, i3 = 0:
        // wi0 = 1 + 3*1 = 4 (w_Q13[4] = -6500, w_Q13[5] = -5000, diff = 1500)
        // wi1 = 2 + 3*2 = 8 (w_Q13[8] = 820,   w_Q13[9] = 2950,  diff = 2130)
        // w1_Q13 = 820 + (((2130 * 6554) >> 16) * (2*0 + 1)) = 820 + 213 * 1 = 1033
        // w0_Q13 = -6500 + (((1500 * 6554) >> 16) * (2*2 + 1)) - 1033 = -6500 + 150 * 5 - 1033 = -6783
        let mut mock = MockRangeDecoder::new(&[], &[7, 1, 2, 2, 0]);
        let weights = decoder.decode_stereo_prediction_weights(&mut mock).unwrap();

        assert_eq!(
            weights,
            StereoWeights {
                w0_q13: -6783,
                w1_q13: 1033,
            }
        );
    }

    #[test]
    fn silk_mid_only_flag_conditional_decoding() {
        let decoder = create_test_silk_decoder(true, FrameDuration::Ms20);

        // When side channel is active, mid-only flag is omitted (not read from range coder)
        let mut mock_active = MockRangeDecoder::new(&[], &[1]);
        assert!(!decoder.decode_mid_only_flag(&mut mock_active, true).unwrap());
        assert_eq!(mock_active.icdf_symbols.len(), 1);

        // When side channel is inactive, mid-only flag is decoded from ICDF_STEREO_MID_ONLY
        assert!(decoder.decode_mid_only_flag(&mut mock_active, false).unwrap());
        assert_eq!(mock_active.icdf_symbols.len(), 0);
    }

    fn create_test_silk_decoder_with_bw(
        stereo: bool,
        frame_size: FrameDuration,
        bandwidth: Bandwidth,
    ) -> Decoder {
        let mut decoder = create_test_silk_decoder(stereo, frame_size);
        let channels = decoder.channels.clone();
        decoder.state = State::try_new(channels, frame_size, bandwidth).unwrap();
        return decoder;
    }

    #[test]
    fn silk_multi_frame_40ms_and_lbrr_full_opus_frame_decode() {
        let mut decoder = create_test_silk_decoder(false, FrameDuration::Ms40);

        // 40 ms Mono with LBRR on frame 0 only:
        // Header bits: VAD_0 = 0, VAD_1 = 0, LBRR = 1
        // Per-frame LBRR symbol: 0 -> mask = 1 (LBRR frame 0 active)
        // Remaining iCDF symbols default to 0 (valid zero-pulse frame parameters)
        let mut mock = MockRangeDecoder::new(&[0, 0, 1], &[0]);
        let decoded = decoder
            .decode_opus_frame_from_range_decoder(&mut mock, false)
            .unwrap();

        assert_eq!(decoded.header.num_silk_frames, 2);
        assert_eq!(decoded.lbrr_frames.len(), 1);
        assert_eq!(decoded.regular_frames.len(), 2);

        // Each 20-ms SILK frame in a 40-ms Opus frame has 4 subframes and 160 samples at 8 kHz NB
        assert_eq!(decoded.lbrr_frames[0].mid.subframes.len(), 4);
        assert_eq!(decoded.lbrr_frames[0].mid.sample_count, 160);
        assert!(decoded.lbrr_frames[0].mid.lsf_interpolation_index.is_some());

        for reg in decoded.regular_frames.iter() {
            assert_eq!(reg.mid.subframes.len(), 4);
            assert_eq!(reg.mid.sample_count, 160);
            assert!(reg.mid.lsf_interpolation_index.is_some());
        }
    }

    #[test]
    fn silk_gains_independent_delta_clamping_and_log2lin() {
        let mut decoder = create_test_silk_decoder(false, FrameDuration::Ms20);
        let mut frame = Frame::new(160, 4);
        frame.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);

        // Initial last_gain_index is 10.
        // Subframe 0 (independent): MSB = 3, LSB = 5 -> gain_index = (3 << 3) + 5 = 29 -> log_gain = 29.
        // Subframe 1 (delta): delta_gain = 2 -> max(2*2 - 16, 29 + 2 - 4) = max(-12, 27) = 27.
        // Subframe 2 (delta, double-step): delta_gain = 30 -> max(2*30 - 16, 27 + 30 - 4) = max(44, 53) = 53.
        //   Wait: for double-step to win, 2*delta - 16 > prev + delta - 4 <=> delta > prev + 12.
        //   With prev = 27, choose delta_gain = 40 -> max(2*40 - 16, 27 + 40 - 4) = max(64, 63) -> clamped to 63.
        //   Choose delta_gain = 35 with prev = 10 -> max(54, 41) = 54.
        // Let's test Subframe 1 with delta_gain = 0 -> log_gain = 29 - 4 = 25;
        // Subframe 2 with delta_gain = 38 -> max(2*38 - 16, 25 + 38 - 4) = max(60, 59) = 60;
        // Subframe 3 with delta_gain = 10 -> max(4, 60 + 10 - 4) = 66 -> clamped to 63.
        let mut mock = MockRangeDecoder::new(&[], &[3, 5, 0, 38, 10]);
        decoder
            .decode_gains(&mut mock, &mut frame, 0, CodingMode::CodeIndependently)
            .unwrap();

        assert_eq!(frame.subframes[0].log_gain, 29);
        assert_eq!(frame.subframes[0].gain_q16, 7_929_856);
        assert_eq!(frame.subframes[1].log_gain, 25);
        assert_eq!(frame.subframes[2].log_gain, 60);
        assert_eq!(frame.subframes[3].log_gain, 63);
        assert_eq!(decoder.state.channel_states[0].last_gain_index, 63);

        // Next frame coded independently with MSB = 0, LSB = 0 (gain_index = 0):
        // Clamped by prev_log_gain - 16 = 63 - 16 = 47.
        let mut frame2 = Frame::new(80, 2);
        let mut mock2 = MockRangeDecoder::new(&[], &[0, 0, 4]);
        decoder
            .decode_gains(&mut mock2, &mut frame2, 0, CodingMode::CodeIndependently)
            .unwrap();
        assert_eq!(frame2.subframes[0].log_gain, 47);
        assert_eq!(frame2.subframes[1].log_gain, 47);

        // Conditional frame (CodeConditionally) uses delta coding even on Subframe 0:
        // delta_gain = 1 -> log_gain = 47 + 1 - 4 = 44.
        let mut frame3 = Frame::new(80, 2);
        let mut mock3 = MockRangeDecoder::new(&[], &[1, 4]);
        decoder
            .decode_gains(&mut mock3, &mut frame3, 0, CodingMode::CodeConditionally)
            .unwrap();
        assert_eq!(frame3.subframes[0].log_gain, 44);
        assert_eq!(frame3.subframes[1].log_gain, 44);
        assert_eq!(mock3.icdf_symbols.len(), 0);
    }

    #[test]
    fn silk_lsf_stage1_stage2_backward_prediction_and_stabilization_nb_and_wb() {
        // Test NB (10 coefficients) with Stage-2 extension symbol and backward prediction
        let decoder_nb = create_test_silk_decoder_with_bw(false, FrameDuration::Ms20, Bandwidth::NarrowBand);
        // Symbol 4 -> i2 = 0; Symbol 8 -> i2 = +4, followed by extension = 2 -> i2 = +6;
        // Symbol 0 -> i2 = -4, followed by extension = 1 -> i2 = -5.
        let stage2_symbols_nb = [4, 4, 4, 4, 4, 4, 4, 4, 8, 2, 0, 1];
        let mut mock_nb = MockRangeDecoder::new(&[], &stage2_symbols_nb);
        let (d_lpc_nb, res_q10_nb) = decoder_nb.decode_lsf_stage2(&mut mock_nb, 0).unwrap();
        assert_eq!(d_lpc_nb, 10);
        assert_eq!(mock_nb.icdf_symbols.len(), 0);

        // Verify backward prediction at k = 9 and k = 8:
        // k = 9: i2 = -5 -> out_q10 = (-5 << 10) + 102 = -5018
        //   res_q10[9] = (-5018 * 11796) >> 16 = -904
        // k = 8: i2 = +6 -> out_q10 = (6 << 10) - 102 = 6042
        //   For i1 = 0, k = 8: PREDICTION_WEIGHT_SELECTION_NB_MB[0][8] = 0 -> weight = 163
        //   pred_q10 = (-904 * 163) >> 8 = -576
        //   step_term = (6042 * 11796) >> 16 = 1087
        //   res_q10[8] = -576 + 1087 = 511
        assert_eq!(res_q10_nb[9], -904);
        assert_eq!(res_q10_nb[8], 511);

        let raw_nlsf_nb = decoder_nb.reconstruct_nlsf(d_lpc_nb, &res_q10_nb, 0).unwrap();
        let stable_nb = decoder_nb.stabilize_nlsf(&raw_nlsf_nb).unwrap();
        assert_eq!(stable_nb.len(), 10);

        // Check strict minimum spacing invariants for NB/MB (including lower and upper borders)
        let min_delta_nb = &constant::MINIMUM_SPACING_NORMALIZED_LSF_NARROWBAND_MEDIUMBAND;
        assert!(stable_nb[0] as i32 >= min_delta_nb[0]);
        for i in 1..10 {
            assert!(
                stable_nb[i] as i32 - stable_nb[i - 1] as i32 >= min_delta_nb[i],
                "NB NLSF spacing violated at index {i}"
            );
        }
        assert!((1 << 15) - (stable_nb[9] as i32) >= min_delta_nb[10]);

        // Test WB (16 coefficients) stabilization with pathological disordered/out-of-bounds input
        let decoder_wb = create_test_silk_decoder_with_bw(false, FrameDuration::Ms20, Bandwidth::WideBand);
        let pathological_wb = [32760, 10, 32700, 5, 20000, 100, 50, 40, 30, 20, 15, 12, 8, 4, 2, 0];
        let stable_wb = decoder_wb.stabilize_nlsf(&pathological_wb).unwrap();
        let min_delta_wb = &constant::MINIMUM_SPACING_NORMALIZED_LSF_WB;
        assert!(stable_wb[0] as i32 >= min_delta_wb[0]);
        for i in 1..16 {
            assert!(
                stable_wb[i] as i32 - stable_wb[i - 1] as i32 >= min_delta_wb[i],
                "WB NLSF spacing violated at index {i}"
            );
        }
        assert!((1 << 15) - (stable_wb[15] as i32) >= min_delta_wb[16]);
    }

    #[test]
    fn silk_lsf_4_subframe_interpolation_across_consecutive_frames() {
        let mut decoder = create_test_silk_decoder_with_bw(false, FrameDuration::Ms20, Bandwidth::NarrowBand);

        // Frame 0 (first frame after reset):
        // Stage-1 i1 = 0, Stage-2 all 4 (zero residual), interp index w_q2 = 1.
        // Because first_frame_after_reset == true, all 4 subframes must use Frame 0's NLSF without interpolating with 0.
        let mut symbols_f0 = vec![0]; // Stage-1 i1 = 0
        symbols_f0.extend(std::iter::repeat_n(4, 10)); // 10 Stage-2 symbols (i2 = 0)
        symbols_f0.push(1); // w_q2 = 1
        let mut mock_f0 = MockRangeDecoder::new(&[], &symbols_f0);
        let mut frame0 = Frame::new(160, 4);
        decoder
            .decode_lsf(&mut mock_f0, &mut frame0, 0, FrameDuration::Ms20)
            .unwrap();
        decoder.state.channel_states[0].first_frame_after_reset = false;

        assert_eq!(mock_f0.icdf_symbols.len(), 0);
        assert_eq!(frame0.lsf_interpolation_index, Some(1));
        let nlsf_f0 = frame0.subframes[2].nlsf_q15.clone();
        assert_eq!(frame0.subframes[0].nlsf_q15, nlsf_f0);
        assert_eq!(frame0.subframes[1].nlsf_q15, nlsf_f0);
        assert_eq!(frame0.subframes[3].nlsf_q15, nlsf_f0);

        // Frame 1 (second frame, first_frame_after_reset == false):
        // Stage-1 i1 = 10, Stage-2 all 4 (zero residual), interp index w_q2 = 2 (50% interpolation).
        let mut symbols_f1 = vec![10];
        symbols_f1.extend(std::iter::repeat_n(4, 10));
        symbols_f1.push(2);
        let mut mock_f1 = MockRangeDecoder::new(&[], &symbols_f1);
        let mut frame1 = Frame::new(160, 4);
        decoder
            .decode_lsf(&mut mock_f1, &mut frame1, 0, FrameDuration::Ms20)
            .unwrap();

        let nlsf_f1 = frame1.subframes[2].nlsf_q15.clone();
        assert_ne!(nlsf_f0, nlsf_f1);
        assert_eq!(frame1.subframes[2].nlsf_q15, nlsf_f1);
        assert_eq!(frame1.subframes[3].nlsf_q15, nlsf_f1);

        // Subframes 0 and 1 must equal nlsf_f0 + ((nlsf_f1 - nlsf_f0) * 2 >> 2)
        for k in 0..10 {
            let expected = nlsf_f0[k] + (((nlsf_f1[k] as i32 - nlsf_f0[k] as i32) * 2) >> 2) as i16;
            assert_eq!(frame1.subframes[0].nlsf_q15[k], expected);
            assert_eq!(frame1.subframes[1].nlsf_q15[k], expected);
        }
    }

    #[test]
    fn silk_pitch_lags_nb_mb_wb_absolute_relative_delta_and_ltp_scaling() {
        // 1. WideBand 20 ms independent voiced frame:
        //    high_part = 5, low_part = 3 -> lag_index = 5 * 8 + 3 = 43 -> base_lag = 32 + 43 = 75.
        //    contour_idx = 8 -> CODEBOOK_SUBFRAME_PITCH_CONTOUR_MEDIUMBAND_WIDEBAND_20MS[8] = [-1, 0, 1, 2]
        //    subframe pitch lags = [74, 75, 76, 77]
        //    periodicity_index = 1, filter_indices = [0, 1, 2, 3], ltp_scaling_idx = 2 (8192 Q14 = 0.5)
        let mut decoder = create_test_silk_decoder_with_bw(false, FrameDuration::Ms20, Bandwidth::WideBand);
        let mut frame1 = Frame::new(320, 4);
        frame1.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);

        let mut mock1 = MockRangeDecoder::new(&[], &[5, 3, 8, 1, 0, 1, 2, 3, 2]);
        decoder
            .decode_ltp(
                &mut mock1,
                &mut frame1,
                0,
                CodingMode::CodeIndependently,
                FrameDuration::Ms20,
            )
            .unwrap();
        decoder.state.channel_states[0].prev_signal_type = SignalType::Voiced;

        assert_eq!(mock1.icdf_symbols.len(), 0);
        assert_eq!(decoder.state.channel_states[0].prev_lag_index, 43);
        assert_eq!(
            frame1.subframes.iter().map(|s| s.pitch_lag).collect::<Vec<_>>(),
            vec![74, 75, 76, 77]
        );
        assert_eq!(frame1.ltp_scale_q14, 8192);
        assert!((frame1.ltp_scale - 0.5).abs() < f32::EPSILON);

        // 2. Conditional voiced frame with non-zero relative delta pitch lag:
        //    delta_lag_index = 12 -> lag_index = 43 + 12 - 9 = 46 -> base_lag = 32 + 46 = 78.
        //    contour_idx = 0 -> [0, 0, 0, 0] -> lags = [78, 78, 78, 78].
        //    periodicity_index = 0, filter_indices = [0, 0, 0, 0].
        //    LTP scaling is NOT read in CodeConditionally (defaults to index 0 -> 15565 Q14).
        let mut frame2 = Frame::new(320, 4);
        frame2.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);
        let mut mock2 = MockRangeDecoder::new(&[], &[12, 0, 0, 0, 0, 0, 0]);
        decoder
            .decode_ltp(
                &mut mock2,
                &mut frame2,
                0,
                CodingMode::CodeConditionally,
                FrameDuration::Ms20,
            )
            .unwrap();

        assert_eq!(mock2.icdf_symbols.len(), 0);
        assert_eq!(decoder.state.channel_states[0].prev_lag_index, 46);
        assert_eq!(
            frame2.subframes.iter().map(|s| s.pitch_lag).collect::<Vec<_>>(),
            vec![78, 78, 78, 78]
        );
        assert_eq!(frame2.ltp_scale_q14, 15565);

        // 3. Conditional voiced frame with delta_lag_index == 0 falls back to absolute lag decoding:
        //    delta_lag_index = 0 -> high_part = 0, low_part = 0 -> lag_index = 0 -> base_lag = 32.
        //    contour_idx = 33 -> [-9, -3, 3, 9] -> clamped at min_lag = 32 -> [32, 32, 35, 41].
        let mut frame3 = Frame::new(320, 4);
        frame3.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);
        let mut mock3 = MockRangeDecoder::new(&[], &[0, 0, 0, 33, 0, 0, 0, 0, 0]);
        decoder
            .decode_ltp(
                &mut mock3,
                &mut frame3,
                0,
                CodingMode::CodeConditionally,
                FrameDuration::Ms20,
            )
            .unwrap();

        assert_eq!(mock3.icdf_symbols.len(), 0);
        assert_eq!(
            frame3.subframes.iter().map(|s| s.pitch_lag).collect::<Vec<_>>(),
            vec![32, 32, 35, 41]
        );

        // 4. MediumBand 10 ms frame (2 subframes, scale = 6, min_lag = 24, max_lag = 216):
        let mut decoder_mb = create_test_silk_decoder_with_bw(false, FrameDuration::Ms10, Bandwidth::MediumBand);
        let mut frame_mb = Frame::new(120, 2);
        frame_mb.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);
        // high_part = 2, low_part = 5 -> lag_index = 2 * 6 + 5 = 17 -> base_lag = 24 + 17 = 41.
        // contour_idx = 5 -> CODEBOOK_SUBFRAME_PITCH_CONTOUR_MEDIUMBAND_WIDEBAND_10MS[5] = [-1, 2] -> [40, 43].
        let mut mock_mb = MockRangeDecoder::new(&[], &[2, 5, 5, 0, 0, 0, 1]);
        decoder_mb
            .decode_ltp(
                &mut mock_mb,
                &mut frame_mb,
                0,
                CodingMode::CodeIndependently,
                FrameDuration::Ms10,
            )
            .unwrap();
        assert_eq!(mock_mb.icdf_symbols.len(), 0);
        assert_eq!(
            frame_mb.subframes.iter().map(|s| s.pitch_lag).collect::<Vec<_>>(),
            vec![40, 43]
        );
        assert_eq!(frame_mb.ltp_scale_q14, 12288);
    }
}


