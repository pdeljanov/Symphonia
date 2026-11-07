//! SILK Normalized Line Spectral Frequency (NLSF) Decoding and LPC Conversion
//!
//! Implements two-stage NLSF codebook decoding, backward prediction, inverse weighting,
//! minimum-spacing stabilization, 4-subframe interpolation, and NLSF-to-LPC conversion
//! (RFC 6716 Sections 4.2.7.5 and 4.2.7.9).

use symphonia_core::errors::Result;

use crate::entropy;
use crate::silk::constant;
use crate::silk::decoder::Decoder;
use crate::silk::error::Error;
use crate::silk::{Frame, MAX_LPC_ORDER, SignalType};
use crate::toc::{Bandwidth, FrameDuration};

impl Decoder {
    /// Decodes the Normalized Line Spectral Frequencies (NLSF) for a SILK frame
    /// and interpolates across subframes when applicable (RFC 6716 Section 4.2.7.5).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5>
    pub(crate) fn decode_lsf(
        &mut self,
        decoder: &mut entropy::Decoder<'_>,
        frame: &mut Frame,
        channel: usize,
        silk_frame_duration: FrameDuration,
    ) -> Result<()> {
        let i1 = self.decode_lsf_stage1(decoder, frame.frame_type.signal_type)?;
        let (d_lpc, res_q10) = self.decode_lsf_stage2(decoder, i1)?;
        let mut nlsf_q15 = self.reconstruct_nlsf(d_lpc, &res_q10[..d_lpc], i1)?;
        self.stabilize_nlsf(&mut nlsf_q15[..d_lpc])?;

        let ch_state = &mut self.state.channel_states[channel];
        let effective_w_q2 = if silk_frame_duration == FrameDuration::Ms20 {
            let raw_interp_idx =
                decoder.decode_symbol_with_icdf(&constant::ICDF_NORMALIZED_LSF_INTERPOLATION_INDEX)?;
            let w_q2 = if ch_state.first_frame_after_reset {
                4
            } else {
                raw_interp_idx
            };
            frame.lsf_interpolation_index = Some(w_q2);
            w_q2
        } else {
            frame.lsf_interpolation_index = None;
            4
        };

        if effective_w_q2 < 4 && frame.num_subframes == 4 {
            let mut nlsf0_q15 = [0i16; MAX_LPC_ORDER];
            for k in 0..d_lpc {
                let prev = ch_state.prev_nlsf_q15[k] as i32;
                let curr = nlsf_q15[k] as i32;
                nlsf0_q15[k] = (prev + (((curr - prev) * (effective_w_q2 as i32)) >> 2)) as i16;
            }
            frame.subframes[0].nlsf_q15 = nlsf0_q15;
            frame.subframes[1].nlsf_q15 = nlsf0_q15;
            frame.subframes[2].nlsf_q15 = nlsf_q15;
            frame.subframes[3].nlsf_q15 = nlsf_q15;
        } else {
            for subframe in frame.active_subframes_mut() {
                subframe.nlsf_q15 = nlsf_q15;
            }
        }

        ch_state.prev_nlsf_q15 = nlsf_q15;
        return Ok(());
    }

    pub(crate) fn decode_lsf_stage1(
        &self,
        decoder: &mut entropy::Decoder<'_>,
        signal_type: SignalType,
    ) -> Result<u32> {
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

    pub(crate) fn decode_lsf_stage2(
        &self,
        decoder: &mut entropy::Decoder<'_>,
        i1: u32,
    ) -> Result<(usize, [i16; MAX_LPC_ORDER])> {
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

        let mut stage2_indices = [0i16; MAX_LPC_ORDER];
        for (k, idx_out) in stage2_indices[..d_lpc].iter_mut().enumerate() {
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

        let mut res_q10 = [0i16; MAX_LPC_ORDER];
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

    pub(crate) fn reconstruct_nlsf(
        &self,
        d_lpc: usize,
        res_q10: &[i16],
        i1: u32,
    ) -> Result<[i16; MAX_LPC_ORDER]> {
        let i1_idx = i1 as usize;
        if i1_idx >= 32 || d_lpc > MAX_LPC_ORDER || res_q10.len() < d_lpc {
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

        let mut nlsf_q15 = [0i16; MAX_LPC_ORDER];
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

    pub(crate) fn stabilize_nlsf(&self, nlsf_q15: &mut [i16]) -> Result<()> {
        let d_lpc = nlsf_q15.len();
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
            let mut min_diff = nlsf_q15[0] as i32 - min_delta[0];
            let mut min_diff_index = 0usize;

            for i in 1..d_lpc {
                let diff = nlsf_q15[i] as i32 - (nlsf_q15[i - 1] as i32 + min_delta[i]);
                if diff < min_diff {
                    min_diff = diff;
                    min_diff_index = i;
                }
            }

            let upper_diff = (1 << 15) - (nlsf_q15[d_lpc - 1] as i32 + min_delta[d_lpc]);
            if upper_diff < min_diff {
                min_diff = upper_diff;
                min_diff_index = d_lpc;
            }

            if min_diff >= 0 {
                return Ok(());
            }

            if min_diff_index == 0 {
                nlsf_q15[0] = min_delta[0] as i16;
            } else if min_diff_index == d_lpc {
                nlsf_q15[d_lpc - 1] = ((1 << 15) - min_delta[d_lpc]) as i16;
            } else {
                let min_center_q15: i32 = min_delta[0..min_diff_index].iter().sum::<i32>()
                    + (min_delta[min_diff_index] >> 1);
                let max_center_q15: i32 = (1 << 15)
                    - min_delta[(min_diff_index + 1)..=d_lpc].iter().sum::<i32>()
                    - (min_delta[min_diff_index] - (min_delta[min_diff_index] >> 1));

                let raw_center = (nlsf_q15[min_diff_index - 1] as i32 + nlsf_q15[min_diff_index] as i32 + 1) >> 1;
                let center_freq_q15 = raw_center.clamp(min_center_q15, max_center_q15);

                nlsf_q15[min_diff_index - 1] = (center_freq_q15 - (min_delta[min_diff_index] >> 1)) as i16;
                nlsf_q15[min_diff_index] = (nlsf_q15[min_diff_index - 1] as i32 + min_delta[min_diff_index]) as i16;
            }

            loops += 1;
        }

        nlsf_q15.sort_unstable();
        for i in 0..d_lpc {
            let prev = if i == 0 { 0 } else { nlsf_q15[i - 1] as i32 };
            nlsf_q15[i] = (nlsf_q15[i] as i32).max(prev + min_delta[i]) as i16;
        }
        for i in (0..d_lpc).rev() {
            let next = if i + 1 == d_lpc { 1 << 15 } else { nlsf_q15[i + 1] as i32 };
            nlsf_q15[i] = (nlsf_q15[i] as i32).min(next - min_delta[i + 1]) as i16;
        }

        return Ok(());
    }

    pub(crate) fn lsf_to_lpc(lsf_q15: &[i16], bandwidth: Bandwidth) -> Result<[i32; MAX_LPC_ORDER]> {
        let order = lsf_q15.len().min(MAX_LPC_ORDER);
        let lsf_ordering: &[u8] = match bandwidth {
            Bandwidth::NarrowBand | Bandwidth::MediumBand => &constant::LSF_ORDERING_POLYNOMIAL_EVALUATION_NB_MB,
            _ => &constant::LSF_ORDERING_POLYNOMIAL_EVALUATION_WB,
        };

        let mut lpc_coeffs = [0i32; MAX_LPC_ORDER];
        let mut p = [1 << 16; MAX_LPC_ORDER + 1];
        let mut q = [1 << 16; MAX_LPC_ORDER + 1];

        for &index in lsf_ordering.iter().take(order) {
            let f = constant::Q12_COSINE_TABLE_FOR_LSF_CONVERSION[(lsf_q15[index as usize] >> 5) as usize];

            let update_polynomial = |poly: &mut [i32]| {
                let mut prev = poly[0];
                poly[0] = (((poly[0] as i64) * ((1 << 16) - f as i64)) >> 16) as i32;
                for elem in poly.iter_mut().take(order + 1).skip(1) {
                    let tmp = *elem;
                    *elem = (((tmp as i64) * ((1 << 16) - f as i64)) >> 16) as i32 - prev;
                    prev = tmp;
                }
            };

            update_polynomial(&mut p[..=order]);
            update_polynomial(&mut q[..=order]);
        }

        for i in 0..order {
            lpc_coeffs[i] = -((q[i + 1] - q[i]) - (p[i + 1] - p[i]) + (1 << 7)) >> 8;
        }

        let gamma = match bandwidth {
            Bandwidth::NarrowBand => 0.98,
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => 0.99,
            Bandwidth::MediumBand => 1.0,
        } as f32;

        for (i, coeff) in lpc_coeffs[..order].iter_mut().enumerate() {
            *coeff = (*coeff as f32 * gamma.powi(i as i32)) as i32;
        }

        return Ok(lpc_coeffs);
    }
}
