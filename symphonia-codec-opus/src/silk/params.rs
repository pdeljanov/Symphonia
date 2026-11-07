//! SILK Frame and Subframe Parameter Decoding
//!
//! Implements decoding of SILK header flags, stereo prediction weights, frame type,
//! subframe quantization gains, and voiced Long-Term Prediction (LTP) parameters
//! (RFC 6716 Sections 4.2.3, 4.2.4, 4.2.7.1 - 4.2.7.4, and 4.2.7.6).

use symphonia_core::errors::Result;

use crate::entropy;
use crate::silk::constant;
use crate::silk::decoder::Decoder;
use crate::silk::error::Error;
use crate::silk::{
    CodingMode, Frame, FrameType, LTP_ORDER, MAX_SILK_FRAMES, MAX_SUBFRAMES,
    QuantizationOffsetType, SignalType, SilkHeaderFlags, State, StereoWeights, SubframeSize,
};
use crate::toc::{Bandwidth, FrameDuration};

impl Decoder {
    /// Decodes the SILK header bits and optional per-frame LBRR flags at the start of an Opus frame.
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.3>
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.4>
    pub(crate) fn decode_silk_header(
        &self,
        decoder: &mut entropy::Decoder<'_>,
        is_stereo: bool,
        frame_size: FrameDuration,
    ) -> Result<SilkHeaderFlags> {
        let num_channels = if is_stereo { 2 } else { 1 };
        let num_silk_frames = State::silk_frame_count(frame_size)?;

        let mut vad_flags = [[false; MAX_SILK_FRAMES]; 2];
        let mut lbrr_flags = [false; 2];
        let mut per_frame_lbrr = [[false; MAX_SILK_FRAMES]; 2];

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
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.1>
    pub(crate) fn decode_stereo_prediction_weights(
        &self,
        decoder: &mut entropy::Decoder<'_>,
    ) -> Result<StereoWeights> {
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
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.2>
    pub(crate) fn decode_mid_only_flag(
        &self,
        decoder: &mut entropy::Decoder<'_>,
        side_active: bool,
    ) -> Result<bool> {
        if side_active {
            return Ok(false);
        }

        let mid_only = decoder.decode_symbol_with_icdf(&constant::ICDF_STEREO_MID_ONLY)?;
        return Ok(mid_only == 1);
    }

    /// Decodes the SILK frame type (RFC 6716 Section 4.2.7.3).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.3>
    pub(crate) fn decode_frame_type(
        &mut self,
        decoder: &mut entropy::Decoder<'_>,
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

    /// Decodes quantization gains for all subframes in a SILK frame (RFC 6716 Section 4.2.7.4).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>
    pub(crate) fn decode_gains(
        &mut self,
        decoder: &mut entropy::Decoder<'_>,
        frame: &mut Frame,
        channel: usize,
        coding_mode: CodingMode,
    ) -> Result<()> {
        let signal_type = frame.frame_type.signal_type;
        let num_subframes = frame.num_subframes;

        for subframe_idx in 0..num_subframes {
            let is_independent = subframe_idx == 0 && coding_mode != CodingMode::CodeConditionally;
            let (log_gain, gain_q16, gain) = self.decode_subframe_gain(
                decoder,
                channel,
                signal_type,
                is_independent,
            )?;
            let subframe = &mut frame.subframes[subframe_idx];
            subframe.log_gain = log_gain;
            subframe.gain_q16 = gain_q16;
            subframe.gain = gain;
        }

        return Ok(());
    }

    pub(crate) fn decode_subframe_gain(
        &mut self,
        decoder: &mut entropy::Decoder<'_>,
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
    #[inline]
    pub(crate) fn quantized_log_gain_to_linear_q16(log_gain: i32) -> i32 {
        // C reference (`silk/gain_quant.c`):
        // INV_SCALE_Q16 = (65536 * (((88 - 2) * 128) / 6)) / (64 - 1) = (65536 * 1834) / 63 = 1_907_825
        const INV_SCALE_Q16: i64 = 1_907_825;
        const OFFSET_Q7: i32 = 2090;

        let in_log_q7 = (((INV_SCALE_Q16 * (log_gain as i64)) >> 16) as i32 + OFFSET_Q7).min(3967);
        return Self::silk_log2lin(in_log_q7);
    }

    /// Evaluates the SILK base-2 exponential approximation `2^(in_log_q7 / 128.0)`.
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>
    #[inline]
    pub(crate) fn silk_log2lin(in_log_q7: i32) -> i32 {
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

    /// Decodes Long-Term Prediction (LTP) parameters (RFC 6716 Section 4.2.7.6).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6>
    pub(crate) fn decode_ltp(
        &mut self,
        decoder: &mut entropy::Decoder<'_>,
        frame: &mut Frame,
        channel: usize,
        coding_mode: CodingMode,
        silk_frame_duration: FrameDuration,
    ) -> Result<()> {
        let num_subframes = SubframeSize::from(silk_frame_duration);
        if num_subframes != frame.num_subframes {
            return Err(Error::MismatchFrameSubframes.into());
        }

        let pitch_lags = self.decode_pitch_lags(decoder, channel, coding_mode, silk_frame_duration)?;
        let ltp_coeffs = self.decode_ltp_coeffs(decoder, silk_frame_duration)?;
        let (ltp_scale_q14, ltp_scale) = self.decode_ltp_scaling(decoder, coding_mode)?;

        frame.ltp_scale_q14 = ltp_scale_q14;
        frame.ltp_scale = ltp_scale;

        for (idx, subframe) in frame.active_subframes_mut().iter_mut().enumerate() {
            subframe.pitch_lag = pitch_lags[idx];
            subframe.ltp_coeffs = ltp_coeffs[idx];
        }

        return Ok(());
    }

    /// Decodes subframe pitch lags (primary lag and pitch contour) for a voiced SILK frame
    /// (RFC 6716 Section 4.2.7.6.1).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6.1>
    pub(crate) fn decode_pitch_lags(
        &mut self,
        decoder: &mut entropy::Decoder<'_>,
        channel: usize,
        coding_mode: CodingMode,
        silk_frame_duration: FrameDuration,
    ) -> Result<[u16; MAX_SUBFRAMES]> {
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

        let mut pitch_lags = [0u16; MAX_SUBFRAMES];
        for (dst, &offset) in pitch_lags.iter_mut().zip(contour.iter()).take(num_subframes) {
            *dst = (base_lag + (offset as i32)).clamp(min_lag, max_lag) as u16;
        }

        return Ok(pitch_lags);
    }

    #[inline]
    pub(crate) const fn pitch_lag_bounds(bandwidth: Bandwidth) -> (i32, i32) {
        return match bandwidth {
            Bandwidth::NarrowBand => (16, 144),
            Bandwidth::MediumBand => (24, 216),
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => (32, 288),
        };
    }

    pub(crate) fn decode_primary_lag_index(
        &mut self,
        decoder: &mut entropy::Decoder<'_>,
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

    pub(crate) fn decode_ltp_coeffs(
        &self,
        decoder: &mut entropy::Decoder<'_>,
        silk_frame_duration: FrameDuration,
    ) -> Result<[[i8; LTP_ORDER]; MAX_SUBFRAMES]> {
        let num_subframes = SubframeSize::from(silk_frame_duration);
        let mut ltp_coeffs = [[0i8; LTP_ORDER]; MAX_SUBFRAMES];

        let periodicity_index = decoder.decode_symbol_with_icdf(&constant::ICDF_PERIODICITY_INDEX)?;

        for subframe_coeffs in ltp_coeffs.iter_mut().take(num_subframes) {
            let filter_index = match periodicity_index {
                0 => decoder.decode_symbol_with_icdf(&constant::ICDF_LTP_FILTER_INDEX_0)?,
                1 => decoder.decode_symbol_with_icdf(&constant::ICDF_LTP_FILTER_INDEX_1)?,
                2 => decoder.decode_symbol_with_icdf(&constant::ICDF_LTP_FILTER_INDEX_2)?,
                _ => return Err(Error::InvalidPeriodicityIndex.into()),
            } as usize;

            *subframe_coeffs = match periodicity_index {
                0 => constant::CODEBOOK_LTP_FILTER_PERIODICITY_INDEX_0[filter_index],
                1 => constant::CODEBOOK_LTP_FILTER_PERIODICITY_INDEX_1[filter_index],
                2 => constant::CODEBOOK_LTP_FILTER_PERIODICITY_INDEX_2[filter_index],
                _ => return Err(Error::InvalidPeriodicityIndex.into()),
            };
        }

        return Ok(ltp_coeffs);
    }

    pub(crate) fn decode_ltp_scaling(
        &self,
        decoder: &mut entropy::Decoder<'_>,
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
}
