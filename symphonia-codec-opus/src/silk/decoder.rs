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
        for f in 0..header.num_silk_frames {
            if header.has_frame_lbrr(f) {
                let lbrr_pair = self.decode_silk_frame_pair(
                    range_decoder,
                    silk_frame_duration,
                    is_stereo,
                    header.per_frame_lbrr[0][f],
                    header.per_frame_lbrr[1][f],
                    true,
                )?;
                lbrr_frames.push(lbrr_pair);
            }
        }

        let mut regular_frames = Vec::with_capacity(header.num_silk_frames);
        for f in 0..header.num_silk_frames {
            let regular_pair = self.decode_silk_frame_pair(
                range_decoder,
                silk_frame_duration,
                is_stereo,
                header.vad_flags[0][f],
                header.vad_flags[1][f],
                header.per_frame_lbrr[0][f] || header.per_frame_lbrr[1][f],
            )?;
            regular_frames.push(regular_pair);
        }

        return Ok(DecodedOpusFrame {
            header,
            lbrr_frames,
            regular_frames,
        });
    }

    fn decode_silk_frame_pair<R: RangeDecoder>(
        &mut self,
        range_decoder: &mut R,
        silk_frame_duration: FrameDuration,
        is_stereo: bool,
        mid_vad: bool,
        side_vad: bool,
        lbrr_flag: bool,
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
            silk_frame_duration,
            mid_vad,
            lbrr_flag,
        )?;
        mid.stereo_weights = stereo_weights;
        mid.mid_only = mid_only;

        let side = if is_stereo && !mid_only {
            let mut side_frame = self.decode_single_silk_frame(
                range_decoder,
                silk_frame_duration,
                side_vad,
                lbrr_flag,
            )?;
            side_frame.stereo_weights = stereo_weights;
            side_frame.mid_only = mid_only;
            Some(side_frame)
        } else {
            None
        };

        return Ok(SilkChannelFramePair {
            stereo_weights,
            mid_only,
            mid,
            side,
        });
    }

    fn decode_single_silk_frame<R: RangeDecoder>(
        &mut self,
        range_decoder: &mut R,
        silk_frame_duration: FrameDuration,
        vad_flag: bool,
        lbrr_flag: bool,
    ) -> Result<Frame> {
        let sample_count = State::calculate_frame_length(self.state.sample_rate, silk_frame_duration)?;
        let num_subframes = SubframeSize::from(silk_frame_duration);
        let mut frame = Frame::new(sample_count, num_subframes);
        frame.vad_flag = vad_flag;
        frame.lbrr_flag = lbrr_flag;

        frame.frame_type = self.decode_frame_type(range_decoder, vad_flag)?;

        self.decode_gains(range_decoder, &mut frame)?;

        self.decode_lsf(range_decoder, &mut frame, silk_frame_duration)?;

        if frame.frame_type.signal_type == SignalType::Voiced {
            self.decode_ltp(range_decoder, &mut frame, silk_frame_duration)?;
        }

        self.decode_excitation(range_decoder, &mut frame)?;

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

    /// Decodes the SILK frame type
    ///
    /// The frame type is encoded using a context-dependent codebook.
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.3>
    fn decode_frame_type<R: RangeDecoder>(&mut self, decoder: &mut R, vad_flag: bool) -> Result<FrameType> {
        let icdf: &[u32] = if vad_flag { &constant::ICDF_FRAME_TYPE_VAD_ACTIVE } else { &constant::ICDF_FRAME_TYPE_VAD_INACTIVE };
        let frame_type_symbol = decoder.decode_symbol_with_icdf(icdf)?;

        let (signal_type, quantization_offset_type) = match (vad_flag, frame_type_symbol) {
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


    /// Decodes the Line Spectral Frequencies (LSF).
    ///
    /// LSF coefficients are decoded using a two-stage process and may include
    /// interpolation for 20 ms frames.
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5>
    fn decode_lsf<R: RangeDecoder>(
        &self,
        decoder: &mut R,
        frame: &mut Frame,
        silk_frame_duration: FrameDuration,
    ) -> Result<()> {
        for subframe in frame.subframes.iter_mut() {
            let i1 = self.decode_lsf_stage1(decoder, frame.vad_flag, frame.frame_type.signal_type)?;
            let (d_lpc, res_q10) = self.decode_lsf_stage2(decoder, i1)?;
            let nlsf_q15 = self.reconstruct_nlsf(d_lpc, &res_q10, i1)?;
            let stabilized_nlsf_q15 = self.stabilize_nlsf(&nlsf_q15)?;
            subframe.nlsf_q15 = stabilized_nlsf_q15;
        }

        if silk_frame_duration == FrameDuration::Ms20 {
            let interpolation_index = decoder.decode_symbol_with_icdf(&constant::ICDF_NORMALIZED_LSF_INTERPOLATION_INDEX)?;
            frame.lsf_interpolation_index = Some(interpolation_index);

            if let Some(w_q2) = frame.lsf_interpolation_index {
                let n0_q15 = &frame.subframes[0].nlsf_q15;
                let n2_q15 = &frame.subframes[2].nlsf_q15;
                let mut n1_q15 = vec![0; self.state.lpc_order];

                for k in 0..self.state.lpc_order {
                    n1_q15[k] = n0_q15[k] + ((w_q2 as i32 * (n2_q15[k] as i32 - n0_q15[k] as i32)) >> 2) as i16;
                }

                frame.subframes[1].nlsf_q15 = n1_q15;
            }
        }

        return Ok(());
    }

    fn decode_lsf_stage1(&self, decoder: &mut impl RangeDecoder, vad_flag: bool, signal_type: SignalType) -> Result<u32> {
        let icdf = match (vad_flag, signal_type) {
            (false, _) => &constant::ICDF_NORMALIZED_LSF_STAGE_ONE_INDEX_NARROWBAND_OR_MEDIUMBAND_UNVOICED,
            (true, SignalType::Voiced) => &constant::ICDF_NORMALIZED_LSF_STAGE_ONE_INDEX_NARROWBAND_OR_MEDIUMBAND_VOICED,
            (true, _) => &constant::ICDF_NORMALIZED_LSF_STAGE_ONE_INDEX_WIDEBAND_UNVOICED,
        };

        return decoder.decode_symbol_with_icdf(icdf);
    }

    fn decode_lsf_stage2<R: RangeDecoder>(&self, decoder: &mut R, i1: u32) -> Result<(usize, Vec<i16>)> {
        let d_lpc = self.state.lpc_order;
        let mut res_q10 = vec![0i16; d_lpc];

        let codebook_selector: &[u8] = match self.state.bandwidth {
            Bandwidth::NarrowBand | Bandwidth::MediumBand => {
                &constant::CODEBOOK_NORMALIZED_LSF_STAGE_TWO_INDEX_NARROWBAND_OR_MEDIUMBAND[i1 as usize]
            }
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => {
                &constant::CODEBOOK_NORMALIZED_LSF_STAGE_TWO_INDEX_WIDEBAND[i1 as usize]
            }
        };

        for (k, res_q10) in res_q10.iter_mut().enumerate() {
            let codebook_idx = codebook_selector[k] as usize;
            let icdf = &constant::ICDF_NORMALIZED_LSF_STAGE_TWO_INDEX[codebook_idx];

            let symbol = decoder.decode_symbol_with_icdf(icdf)?;

            *res_q10 = (symbol as i16) - 4;

            if *res_q10 == -4 || *res_q10 == 4 {
                let extension = decoder.decode_symbol_with_icdf(&constant::ICDF_NORMALIZED_LSF_STAGE_TWO_INDEX_EXTENSION)?;
                *res_q10 += if *res_q10 < 0 {
                    -(extension as i16)
                } else {
                    extension as i16
                };
            }
        }

        return Ok((d_lpc, res_q10));
    }


    fn reconstruct_nlsf(&self, d_lpc: usize, res_q10: &[i16], i1: u32) -> Result<Vec<i16>> {
        let mut nlsf_q15 = vec![0i16; d_lpc];
        let (cb1_q8, step_q16): (&[u8], &[i32]) = match self.state.bandwidth {
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => (
                &constant::CODEBOOK_NORMALIZED_LSF_STAGE_ONE_WIDEBAND[i1 as usize],
                &constant::LSF_STAGE_TWO_QUANTIZATION_STEP_Q16_WB,
            ),
            Bandwidth::NarrowBand | Bandwidth::MediumBand => (
                &constant::CODEBOOK_NORMALIZED_LSF_STAGE_ONE_NARROWBAND_OR_MEDIUMBAND[i1 as usize],
                &constant::LSF_STAGE_TWO_QUANTIZATION_STEP_Q16_NB_MB,
            ),
        };

        for k in 0..d_lpc {
            let cb_value = cb1_q8[k] as i32;
            let res_value = res_q10[k] as i32;
            nlsf_q15[k] = ((cb_value << 7) + ((res_value * step_q16[k]) >> 16)) as i16;
        }

        return Ok(nlsf_q15);
    }

    fn stabilize_nlsf(&self, nlsf_q15: &[i16]) -> Result<Vec<i16>> {
        let mut stable_nlsf = nlsf_q15.to_vec();
        let min_delta: &[i32] = match self.state.bandwidth {
            Bandwidth::WideBand => &constant::MINIMUM_SPACING_NORMALIZED_LSF_WB,
            _ => &constant::MINIMUM_SPACING_NORMALIZED_LSF_NARROWBAND_MEDIUMBAND,
        };

        const MAX_STABILIZATION_ITERATIONS: usize = 20;
        for _ in 0..MAX_STABILIZATION_ITERATIONS {
            let mut min_diff = i32::MAX;
            let mut min_diff_index = 0;

            for i in 1..stable_nlsf.len() {
                let diff = stable_nlsf[i] as i32 - stable_nlsf[i - 1] as i32 - min_delta[i];
                if diff < min_diff {
                    min_diff = diff;
                    min_diff_index = i;
                }
            }

            if min_diff >= 0 {
                break;
            }

            let center = (stable_nlsf[min_diff_index - 1] as i32 + stable_nlsf[min_diff_index] as i32) / 2;
            stable_nlsf[min_diff_index - 1] = (center - min_delta[min_diff_index] / 2) as i16;
            stable_nlsf[min_diff_index] = (stable_nlsf[min_diff_index - 1] as i32 + min_delta[min_diff_index]) as i16;
        }

        return Ok(stable_nlsf);
    }

    fn decode_pitch_lags<R: RangeDecoder>(
        &self,
        decoder: &mut R,
        silk_frame_duration: FrameDuration,
    ) -> Result<Vec<u16>> {
        let num_subframes = SubframeSize::from(silk_frame_duration);

        let mut pitch_lags = vec![0u16; num_subframes];

        let primary_lag = self.decode_primary_lag(decoder)?;

        for pitch_lag in pitch_lags.iter_mut().take(num_subframes) {
            let k = decoder.decode_symbol_with_icdf(&constant::ICDF_SUBFRAME_PITCH_CONTOUR_NARROWBAND_20_MS)?;
            let contour = constant::CODEBOOK_SUBFRAME_PITCH_CONTOUR_NARROWBAND_20MS[k as usize];
            *pitch_lag = (primary_lag as i32 + contour[0] as i32) as u16;
        }

        return Ok(pitch_lags);
    }

    fn decode_primary_lag<R: RangeDecoder>(&self, decoder: &mut R) -> Result<u16> {
        let high_part = decoder.decode_symbol_with_icdf(&constant::ICDF_PRIMARY_PITCH_LAG_HIGH_PART)?;
        let low_part = decoder.decode_symbol_with_icdf(&constant::ICDF_PRIMARY_PITCH_LAG_LOW_PART_NARROWBAND)?;

        return Ok((high_part as u16 * 4) + low_part as u16 + 16);
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

    fn decode_ltp_scaling<R: RangeDecoder>(&self, decoder: &mut R) -> Result<f32> {
        const LTP_SCALE_Q14_DIVISOR: f32 = 16384.0;

        let i = decoder.decode_symbol_with_icdf(&constant::ICDF_LTP_SCALING_PARAMETER)? as usize;

        let scale_q14 = *constant::CODEBOOK_LTP_SCALING_Q14
            .get(i)
            .ok_or(Error::InvalidLTPScalingIndex)?;

        return Ok(scale_q14 as f32 / LTP_SCALE_Q14_DIVISOR);
    }

    /// Decodes Long-Term Prediction (LTP) parameters.
    ///
    /// This includes pitch lags, LTP coefficients, and LTP scaling.
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6>
    fn decode_ltp<R: RangeDecoder>(
        &mut self,
        decoder: &mut R,
        frame: &mut Frame,
        silk_frame_duration: FrameDuration,
    ) -> Result<()> {
        let pitch_lags = self.decode_pitch_lags(decoder, silk_frame_duration)?;
        let ltp_coeffs = self.decode_ltp_coeffs(decoder, silk_frame_duration)?;
        let ltp_scale = self.decode_ltp_scaling(decoder)?;

        if pitch_lags.len() != frame.subframes.len() || ltp_coeffs.len() != frame.subframes.len() {
            return Err(Error::MismatchFrameSubframes.into());
        }

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

    fn decode_gains<R: RangeDecoder>(&self, decoder: &mut R, frame: &mut Frame) -> Result<()> {
        for subframe in frame.subframes.iter_mut() {
            let gain = self.decode_subframe_gain(decoder)?;
            subframe.gain = gain;
        }

        return Ok(());
    }

    fn decode_subframe_gain<R: RangeDecoder>(&self, decoder: &mut R) -> Result<f32> {
        let icdf_gain_msb = match self.state.prev_frame_type.signal_type {
            SignalType::Voiced => &constant::ICDF_INDEPENDENT_QUANTIZATION_GAIN_MSB_VOICED,
            SignalType::Unvoiced | SignalType::Inactive => &constant::ICDF_INDEPENDENT_QUANTIZATION_GAIN_MSB_UNVOICED,
        };

        let gain_msb = decoder.decode_symbol_with_icdf(icdf_gain_msb)? as u8;
        let gain_lsb = decoder.decode_symbol_with_icdf(&constant::ICDF_INDEPENDENT_QUANTIZATION_GAIN_LSB)? as u8;

        let gain = ((gain_msb as u16) << 8) | (gain_lsb as u16);

        const GAIN_NORMALIZATION_FACTOR: f32 = 256.0;
        let normalized_gain = gain as f32 / GAIN_NORMALIZATION_FACTOR;

        return Ok(normalized_gain);
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

        return Ok(Self {
            sample_rate,
            channels,
            frame_size,
            bandwidth,
            prev_frame_type,
            prev_samples,
            lbrr_flag,
            lpc_order,
        });
    }

    pub fn reset(&mut self) {
        self.lbrr_flag = false;
        self.prev_samples.fill(0.0);
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
}


