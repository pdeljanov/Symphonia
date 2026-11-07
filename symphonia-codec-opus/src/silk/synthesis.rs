//! SILK LTP and LPC Synthesis Filtering
//!
//! Implements Long-Term Prediction (LTP) and Linear Predictive Coding (LPC)
//! synthesis filtering for SILK subframes (RFC 6716 Section 4.2.7.9).

use symphonia_core::audio::{Audio, AudioBuffer, AudioMut};
use symphonia_core::errors::Result;

use crate::silk::decoder::Decoder;
use crate::silk::error::Error;
use crate::silk::{Frame, MAX_SUBFRAME_SAMPLES};

impl Decoder {
    pub(crate) fn ltp_synthesis(
        excitation: &[f32],
        pitch_lag: u16,
        ltp_coeffs: &[i8],
        ltp_scale: f32,
    ) -> [f32; MAX_SUBFRAME_SAMPLES] {
        let mut ltp_signal = [0.0f32; MAX_SUBFRAME_SAMPLES];
        let len = excitation.len().min(MAX_SUBFRAME_SAMPLES);

        for i in 0..len {
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

    pub(crate) fn lpc_synthesis(
        lpc_coeffs: &[i32],
        excitation: &[f32],
        subframe_size: usize,
    ) -> [f32; MAX_SUBFRAME_SAMPLES] {
        let order = lpc_coeffs.len();
        let len = subframe_size.min(MAX_SUBFRAME_SAMPLES).min(excitation.len());
        let mut output = [0.0f32; MAX_SUBFRAME_SAMPLES];

        for i in 0..len {
            let mut y = excitation[i];
            for j in 0..order.min(i) {
                y -= lpc_coeffs[j] as f32 / 4096.0 * output[i - j - 1];
            }
            output[i] = y;
        }

        return output;
    }

    pub(crate) fn synthesize_frame(
        &mut self,
        frame: &Frame,
        buffer: &mut AudioBuffer<f32>,
    ) -> Result<()> {
        if frame.num_subframes == 0 {
            return Err(Error::MismatchFrameSubframes.into());
        }

        let samples_per_subframe = (frame.sample_count / frame.num_subframes).min(MAX_SUBFRAME_SAMPLES);
        let channels = buffer.spec().channels().count();

        if buffer.frames() + frame.sample_count > buffer.capacity() {
            return Err(Error::BufferOverflow.into());
        }

        let start = buffer.frames();
        buffer.render_uninit(Some(frame.sample_count));

        for ch in 0..channels {
            let dst = buffer.plane_mut(ch).ok_or(Error::BufferOverflow)?;
            for (s, subframe) in frame.active_subframes().iter().enumerate() {
                let offset = start + s * samples_per_subframe;
                let excitation = subframe.active_excitation();
                let synt_len = samples_per_subframe.min(excitation.len());

                let ltp_signal = Self::ltp_synthesis(
                    excitation,
                    subframe.pitch_lag,
                    &subframe.ltp_coeffs,
                    frame.ltp_scale,
                );

                let lpc_coeffs = Self::lsf_to_lpc(subframe.nlsf(frame.lpc_order), self.state.bandwidth)?;
                let lpc_signal = Self::lpc_synthesis(&lpc_coeffs[..frame.lpc_order], &ltp_signal[..synt_len], synt_len);

                for i in 0..synt_len {
                    dst[offset + i] = ltp_signal[i] + lpc_signal[i];
                }
            }
        }

        return Ok(());
    }
}
