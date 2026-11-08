//! SILK LTP/LPC Synthesis Filtering and Mid-Side Stereo Unmixing
//!
//! Implements stateful Long-Term Prediction (LTP) rewhitening and synthesis,
//! Linear Predictive Coding (LPC) synthesis filtering, and adaptive Mid-Side to
//! Left-Right stereo unmixing (RFC 6716 Sections 4.2.7.9 and 4.2.8).

use symphonia_core::audio::{Audio, AudioBuffer, AudioMut};
use symphonia_core::errors::Result;

use crate::silk::decoder::Decoder;
use crate::silk::error::Error;
use crate::silk::{
    Frame, LTP_ORDER, MAX_FRAME_SAMPLES, MAX_LPC_ORDER, MAX_LTP_MEM_SAMPLES, MAX_SUBFRAME_SAMPLES,
    SignalType, SilkChannelFramePair, StereoState, StereoWeights, add_sat32, div32_var_q,
    inverse32_var_q, lshift_sat32, mla_q16_i16, mul_q16, mul_q16_i16, rshift_round, sat16,
};

/// Maximum number of output samples per 20-ms SILK frame at 48 kHz (`20 ms * 48 kHz = 960`).
const MAX_RESAMPLED_FRAME_SAMPLES: usize = 960;

impl Decoder {
    /// Performs LPC analysis (whitening) filtering on `input` using Q12 coefficients `b_q12`
    /// (`silk_LPC_analysis_filter` in `silk/LPC_analysis_filter.c`).
    ///
    /// The first `d` output samples are set to zero.
    pub(crate) fn lpc_analysis_filter(
        out: &mut [i16],
        input: &[i16],
        b_q12: &[i16],
        d: usize,
    ) {
        let len = out.len().min(input.len());
        if len <= d || d == 0 {
            out[..len].fill(0);
            return;
        }

        out[..d].fill(0);

        for i in d..len {
            let out32_q12 = b_q12[..d].iter().enumerate().fold(0i32, |acc, (j, &b)| {
                acc.wrapping_add((input[i - 1 - j] as i32) * (b as i32))
            });
            let residual_q12 = ((input[i] as i32) << 12).wrapping_sub(out32_q12);
            out[i] = sat16(rshift_round(residual_q12, 12));
        }
    }

    /// Synthesizes a single SILK channel frame (`frame.output_pcm`) from `frame.exc_q14`
    /// using stateful LTP rewhitening, LTP synthesis, and LPC synthesis
    /// (`silk_decode_core` in `silk/decode_core.c` and `silk_decode_frame` in `silk/decode_frame.c`).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9>
    pub(crate) fn synthesize_channel_frame(
        &mut self,
        frame: &mut Frame,
        channel: usize,
    ) -> Result<()> {
        let nb_subfr = frame.num_subframes;
        let frame_length = frame.sample_count;
        if nb_subfr == 0 || frame_length == 0 || frame_length > MAX_FRAME_SAMPLES {
            return Err(Error::MismatchFrameSubframes.into());
        }

        let subfr_length = frame_length / nb_subfr;
        let lpc_order = frame.lpc_order;
        if (lpc_order != 10 && lpc_order != 16) || subfr_length > MAX_SUBFRAME_SAMPLES {
            return Err(Error::InvalidLSFCoefficients.into());
        }

        let ch_state = &mut self.state.channel_states[channel];
        let ltp_mem_length = ch_state.ltp_mem_length;
        let nlsf_interp_flag = frame.lsf_interpolation_index.unwrap_or(4) < 4;

        let mut s_ltp = [0i16; MAX_LTP_MEM_SAMPLES];
        let mut s_ltp_q15 = [0i32; MAX_LTP_MEM_SAMPLES + MAX_FRAME_SAMPLES];
        let mut res_q14 = [0i32; MAX_SUBFRAME_SAMPLES];
        let mut s_lpc_q14 = [0i32; MAX_LPC_ORDER + MAX_SUBFRAME_SAMPLES];

        s_lpc_q14[..MAX_LPC_ORDER].copy_from_slice(&ch_state.s_lpc_q14_buf);

        let mut s_ltp_buf_idx = ltp_mem_length;
        let signal_type = frame.frame_type.signal_type;

        for k in 0..nb_subfr {
            let a_q12: [i16; MAX_LPC_ORDER] = frame.subframes[k].lpc_coeffs_q12;
            let b_q14: [i16; LTP_ORDER] =
                std::array::from_fn(|j| (frame.subframes[k].ltp_coeffs[j] as i16) << 7);
            let gain_q16 = frame.subframes[k].gain_q16;
            let gain_q10 = gain_q16 >> 6;
            let mut inv_gain_q31 = inverse32_var_q(gain_q16, 47);

            let gain_adj_q16 = if gain_q16 != ch_state.prev_gain_q16 {
                let adj = div32_var_q(ch_state.prev_gain_q16, gain_q16, 16);
                for lpc_state in s_lpc_q14[..MAX_LPC_ORDER].iter_mut() {
                    *lpc_state = mul_q16(adj, *lpc_state);
                }
                adj
            } else {
                1i32 << 16
            };
            ch_state.prev_gain_q16 = gain_q16;

            let pexc_q14 = &frame.exc_q14[k * subfr_length..(k + 1) * subfr_length];

            if signal_type == SignalType::Voiced {
                let lag = frame.subframes[k].pitch_lag as usize;
                let rewhiten = k == 0 || (k == 2 && nlsf_interp_flag);

                match (rewhiten, gain_adj_q16 != (1i32 << 16)) {
                    (true, _) => {
                        let start_idx = ltp_mem_length - lag - lpc_order - (LTP_ORDER / 2);

                        if k == 2 {
                            ch_state.out_buf[ltp_mem_length..ltp_mem_length + 2 * subfr_length]
                                .copy_from_slice(&frame.output_pcm[..2 * subfr_length]);
                        }

                        let in_start = start_idx + k * subfr_length;
                        let filter_len = ltp_mem_length - start_idx;
                        Self::lpc_analysis_filter(
                            &mut s_ltp[start_idx..ltp_mem_length],
                            &ch_state.out_buf[in_start..in_start + filter_len],
                            &a_q12[..lpc_order],
                            lpc_order,
                        );

                        if k == 0 {
                            inv_gain_q31 = mul_q16_i16(inv_gain_q31, frame.ltp_scale_q14) << 2;
                        }
                        for i in 0..(lag + LTP_ORDER / 2) {
                            s_ltp_q15[s_ltp_buf_idx - i - 1] =
                                mul_q16_i16(inv_gain_q31, s_ltp[ltp_mem_length - i - 1]);
                        }
                    }
                    (false, true) => {
                        for i in 0..(lag + LTP_ORDER / 2) {
                            s_ltp_q15[s_ltp_buf_idx - i - 1] =
                                mul_q16(gain_adj_q16, s_ltp_q15[s_ltp_buf_idx - i - 1]);
                        }
                    }
                    (false, false) => {}
                }

                let pred_lag_base = s_ltp_buf_idx - lag + (LTP_ORDER / 2);
                for i in 0..subfr_length {
                    let pred_lag_idx = pred_lag_base + i;
                    let ltp_pred_q13 = b_q14.iter().enumerate().fold(2i32, |acc, (j, &b_tap)| {
                        mla_q16_i16(acc, s_ltp_q15[pred_lag_idx - j], b_tap)
                    });

                    res_q14[i] = pexc_q14[i].wrapping_add(ltp_pred_q13 << 1);
                    s_ltp_q15[s_ltp_buf_idx] = res_q14[i] << 1;
                    s_ltp_buf_idx += 1;
                }
            } else {
                res_q14[..subfr_length].copy_from_slice(pexc_q14);
            }

            let pxq = &mut frame.output_pcm[k * subfr_length..(k + 1) * subfr_length];
            for i in 0..subfr_length {
                let lpc_pred_q10 = a_q12[..lpc_order]
                    .iter()
                    .enumerate()
                    .fold((lpc_order >> 1) as i32, |acc, (j, &a_tap)| {
                        mla_q16_i16(acc, s_lpc_q14[MAX_LPC_ORDER + i - 1 - j], a_tap)
                    });

                let lpc_out_q14 = add_sat32(res_q14[i], lshift_sat32(lpc_pred_q10, 4));
                s_lpc_q14[MAX_LPC_ORDER + i] = lpc_out_q14;
                pxq[i] = sat16(rshift_round(mul_q16(lpc_out_q14, gain_q10), 8));
            }

            s_lpc_q14.copy_within(subfr_length..subfr_length + MAX_LPC_ORDER, 0);
        }

        ch_state.s_lpc_q14_buf.copy_from_slice(&s_lpc_q14[..MAX_LPC_ORDER]);

        let mv_len = ltp_mem_length - frame_length;
        if mv_len > 0 {
            ch_state
                .out_buf
                .copy_within(frame_length..frame_length + mv_len, 0);
        }
        ch_state.out_buf[mv_len..mv_len + frame_length]
            .copy_from_slice(&frame.output_pcm[..frame_length]);

        ch_state.lag_prev = frame.subframes[nb_subfr - 1].pitch_lag as i32;

        return Ok(());
    }

    /// Converts adaptive Mid-Side channels (`x1`, `x2`, each of length `frame_length + 2` with
    /// decoded frame samples placed at `2..2 + frame_length`) into Left and Right channels at
    /// `1..1 + frame_length` (`silk_stereo_MS_to_LR` in `silk/stereo_MS_to_LR.c`).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.8>
    pub(crate) fn unmix_mid_side_to_left_right(
        stereo_state: &mut StereoState,
        x1: &mut [i16],
        x2: &mut [i16],
        weights: StereoWeights,
        fs_khz: usize,
        frame_length: usize,
    ) {
        const STEREO_INTERP_LEN_MS: usize = 8;

        x1[..2].copy_from_slice(&stereo_state.s_mid);
        x2[..2].copy_from_slice(&stereo_state.s_side);
        stereo_state
            .s_mid
            .copy_from_slice(&x1[frame_length..frame_length + 2]);
        stereo_state
            .s_side
            .copy_from_slice(&x2[frame_length..frame_length + 2]);

        let interp_samples = STEREO_INTERP_LEN_MS * fs_khz;
        let mut pred0_q13 = stereo_state.pred_prev_q13[0];
        let mut pred1_q13 = stereo_state.pred_prev_q13[1];
        let denom_q16 = (1i32 << 16) / (interp_samples as i32);

        let delta0_q13 = rshift_round(
            ((weights.w0_q13 - stereo_state.pred_prev_q13[0]) as i16 as i32)
                * (denom_q16 as i16 as i32),
            16,
        );
        let delta1_q13 = rshift_round(
            ((weights.w1_q13 - stereo_state.pred_prev_q13[1]) as i16 as i32)
                * (denom_q16 as i16 as i32),
            16,
        );

        let interp_end = interp_samples.min(frame_length);
        for i in 0..interp_end {
            pred0_q13 += delta0_q13;
            pred1_q13 += delta1_q13;
            let sum_lpf_q11 =
                (((x1[i] as i32) + (x1[i + 2] as i32)) + ((x1[i + 1] as i32) << 1)) << 9;
            let mut sum_q8 = mla_q16_i16((x2[i + 1] as i32) << 8, sum_lpf_q11, pred0_q13 as i16);
            sum_q8 = mla_q16_i16(sum_q8, (x1[i + 1] as i32) << 11, pred1_q13 as i16);
            x2[i + 1] = sat16(rshift_round(sum_q8, 8));
        }

        pred0_q13 = weights.w0_q13;
        pred1_q13 = weights.w1_q13;
        for i in interp_end..frame_length {
            let sum_lpf_q11 =
                (((x1[i] as i32) + (x1[i + 2] as i32)) + ((x1[i + 1] as i32) << 1)) << 9;
            let mut sum_q8 = mla_q16_i16((x2[i + 1] as i32) << 8, sum_lpf_q11, pred0_q13 as i16);
            sum_q8 = mla_q16_i16(sum_q8, (x1[i + 1] as i32) << 11, pred1_q13 as i16);
            x2[i + 1] = sat16(rshift_round(sum_q8, 8));
        }

        stereo_state.pred_prev_q13 = [weights.w0_q13, weights.w1_q13];

        for i in 0..frame_length {
            let mid_val = x1[i + 1] as i32;
            let side_val = x2[i + 1] as i32;
            x1[i + 1] = sat16(mid_val + side_val);
            x2[i + 1] = sat16(mid_val - side_val);
        }
    }

    /// Prepends the 2-sample `s_mid` delay buffer to a mono SILK frame (`dec_API.c:386-389`),
    /// placing the resampler input at `x1[1..1 + frame_length]`.
    pub(crate) fn buffer_mono_mid(
        stereo_state: &mut StereoState,
        x1: &mut [i16],
        frame_length: usize,
    ) {
        x1[..2].copy_from_slice(&stereo_state.s_mid);
        stereo_state
            .s_mid
            .copy_from_slice(&x1[frame_length..frame_length + 2]);
    }

    /// Unmixes (or mono-buffers) a decoded `SilkChannelFramePair`, resamples from the internal
    /// SILK sample rate (`8`, `12`, or `16` kHz) to `self.state.output_sample_rate`, and writes
    /// normalized `f32` PCM (`i16 / 32768.0`) into planar slices `out_ch0` and `out_ch1`.
    pub(crate) fn output_frame_pair_planar(
        &mut self,
        pair: &SilkChannelFramePair,
        is_stereo_packet: bool,
        stereo_to_mono: bool,
        out_ch0: &mut [f32],
        out_ch1: Option<&mut [f32]>,
    ) -> Result<usize> {
        const INT16_TO_F32_SCALE: f32 = 1.0 / 32768.0;

        let frame_length = pair.mid.sample_count;
        if frame_length == 0 || frame_length > MAX_FRAME_SAMPLES {
            return Err(Error::InvalidFrameSize.into());
        }

        let api_channels = if out_ch1.is_some() { 2 } else { 1 };
        let internal_channels = if is_stereo_packet { 2 } else { 1 };
        let fs_khz = self.state.channel_states[0].fs_khz;

        let mut samples_tmp0 = [0i16; MAX_FRAME_SAMPLES + 2];
        let mut samples_tmp1 = [0i16; MAX_FRAME_SAMPLES + 2];
        samples_tmp0[2..2 + frame_length].copy_from_slice(&pair.mid.output_pcm[..frame_length]);

        if let Some(ref side) = pair.side {
            samples_tmp1[2..2 + frame_length].copy_from_slice(&side.output_pcm[..frame_length]);
        }

        if api_channels == 2 && internal_channels == 2 {
            let weights = pair.stereo_weights.unwrap_or(StereoWeights {
                w0_q13: self.state.stereo_state.pred_prev_q13[0],
                w1_q13: self.state.stereo_state.pred_prev_q13[1],
            });
            Self::unmix_mid_side_to_left_right(
                &mut self.state.stereo_state,
                &mut samples_tmp0[..frame_length + 2],
                &mut samples_tmp1[..frame_length + 2],
                weights,
                fs_khz,
                frame_length,
            );
        } else {
            Self::buffer_mono_mid(
                &mut self.state.stereo_state,
                &mut samples_tmp0[..frame_length + 2],
                frame_length,
            );
        }

        let mut resampled0 = [0i16; MAX_RESAMPLED_FRAME_SAMPLES];
        let n_out = self.state.channel_states[0].resampler.resample(
            &mut resampled0,
            &samples_tmp0[1..1 + frame_length],
        )?;

        if out_ch0.len() < n_out {
            return Err(Error::BufferOverflow.into());
        }

        for (dst, &src) in out_ch0[..n_out].iter_mut().zip(resampled0[..n_out].iter()) {
            *dst = (src as f32) * INT16_TO_F32_SCALE;
        }

        if let Some(dst1) = out_ch1 {
            if dst1.len() < n_out {
                return Err(Error::BufferOverflow.into());
            }
            match (internal_channels == 2, stereo_to_mono) {
                (true, _) | (false, true) => {
                    let src_samples = if internal_channels == 2 {
                        &samples_tmp1[1..1 + frame_length]
                    } else {
                        &samples_tmp0[1..1 + frame_length]
                    };
                    let mut resampled1 = [0i16; MAX_RESAMPLED_FRAME_SAMPLES];
                    let n_out1 = self.state.channel_states[1]
                        .resampler
                        .resample(&mut resampled1, src_samples)?;
                    for (dst, &src) in dst1[..n_out1].iter_mut().zip(resampled1[..n_out1].iter()) {
                        *dst = (src as f32) * INT16_TO_F32_SCALE;
                    }
                }
                (false, false) => {
                    dst1[..n_out].copy_from_slice(&out_ch0[..n_out]);
                }
            }
        }

        return Ok(n_out);
    }

    /// Unmixes (or mono-buffers) a decoded `SilkChannelFramePair`, resamples from the internal
    /// SILK sample rate (`8`, `12`, or `16` kHz) to `self.state.output_sample_rate`, and appends
    /// normalized `f32` PCM (`i16 / 32768.0`) to `buffer` (`dec_API.c:382-468`).
    pub(crate) fn output_frame_pair(
        &mut self,
        pair: &SilkChannelFramePair,
        is_stereo_packet: bool,
        stereo_to_mono: bool,
        buffer: &mut AudioBuffer<f32>,
    ) -> Result<()> {
        let frame_length = pair.mid.sample_count;
        if frame_length == 0 || frame_length > MAX_FRAME_SAMPLES {
            return Err(Error::InvalidFrameSize.into());
        }

        let api_channels = buffer.spec().channels().count();
        let fs_khz = self.state.channel_states[0].fs_khz.max(8);
        let expected_out = (frame_length * (self.state.output_sample_rate as usize)) / (fs_khz * 1000);
        if buffer.frames() + expected_out > buffer.capacity() {
            return Err(Error::BufferOverflow.into());
        }

        let mut ch0_buf = [0.0f32; MAX_RESAMPLED_FRAME_SAMPLES];
        let mut ch1_buf = [0.0f32; MAX_RESAMPLED_FRAME_SAMPLES];
        let n_out = if api_channels == 2 {
            self.output_frame_pair_planar(
                pair,
                is_stereo_packet,
                stereo_to_mono,
                &mut ch0_buf,
                Some(&mut ch1_buf),
            )?
        } else {
            self.output_frame_pair_planar(
                pair,
                is_stereo_packet,
                stereo_to_mono,
                &mut ch0_buf,
                None,
            )?
        };

        let start = buffer.frames();
        buffer.render_uninit(Some(n_out));
        {
            let dst0 = buffer.plane_mut(0).ok_or(Error::BufferOverflow)?;
            dst0[start..start + n_out].copy_from_slice(&ch0_buf[..n_out]);
        }
        if api_channels == 2 {
            let dst1 = buffer.plane_mut(1).ok_or(Error::BufferOverflow)?;
            dst1[start..start + n_out].copy_from_slice(&ch1_buf[..n_out]);
        }

        return Ok(());
    }
}
