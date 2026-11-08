// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Spectral Band Replication (SBR) tool (ISO/IEC 14496-3:2009, 4.6.18).
//!
//! [`SbrElement`] combines the SBR bitstream decoder, the per-channel SBR signal processing, the
//! parametric stereo (PS) tool (Annex 8.A), and the 64-band synthesis QMF banks for one syntactic
//! element (SCE or CPE) of the AAC core.

pub mod bands;
pub mod channel;
pub mod envelope;
pub mod hfadj;
pub mod hfgen;
pub mod huffman;
pub mod qmf;
pub mod syntax;
pub mod types;

use log::{debug, warn};
use symphonia_core::dsp::complex::Complex;

use crate::aac::ps::PsDecoder;

use channel::SbrChannelProcessor;
use qmf::QmfSynthesis;
use syntax::SbrBitstreamDecoder;
use types::{PS_LOOKAHEAD, QMF_ANALYSIS_BANDS, QMF_BANDS, QMF_SLOTS, QmfFrame, QmfSlots, SbrFrame};

/// The number of time-domain samples per channel of one AAC core frame (numTimeSlots * RATE * 32
/// for 1024-sample frames).
pub const CORE_FRAME_LEN: usize = QMF_SLOTS * QMF_ANALYSIS_BANDS;

/// The number of time-domain output samples per channel of one SBR frame (numTimeSlots * RATE * 64
/// for 1024-sample core frames).
pub const SBR_FRAME_LEN: usize = QMF_SLOTS * QMF_BANDS;

const ZERO: Complex<f32> = Complex { re: 0.0, im: 0.0 };

/// The parametric stereo stage of a single channel element with stereo output (Annex 8.A.3).
struct PsStage {
    /// The PS tool.
    decoder: PsDecoder,
    /// The left and right channel 64-band QMF representations produced by the PS tool.
    out: [QmfSlots; 2],
}

/// The SBR decoding state of one syntactic element (SCE or CPE) of the AAC core.
///
/// A single channel element may be combined with the PS tool to produce two output channels
/// (Annex 8.A). Frames without valid SBR data are upsampled only (4.5.2.8.2.1, 4.6.18.5).
pub struct SbrElement {
    /// The SBR bitstream decoder.
    bitstream: SbrBitstreamDecoder,
    /// The SBR signal processing of each core channel.
    channels: Vec<SbrChannelProcessor>,
    /// The 64-band synthesis QMF bank of each output channel (4.6.18.4.2).
    synthesis: Vec<QmfSynthesis>,
    /// The PS stage, for a single channel element with stereo output.
    ps: Option<Box<PsStage>>,
    /// X_input of each core channel (see [`QmfFrame`]).
    qmf: Vec<QmfFrame>,
    /// The SBR data decoded for the current frame, if any.
    frame: Option<SbrFrame>,
    /// True if the SBR data of the last payload was decoded successfully. Used to report decoding
    /// errors once, rather than for every frame until the decoder re-synchronizes.
    synced: bool,
}

impl SbrElement {
    /// Create the SBR state of a syntactic element with `num_channels` core channels (1 for a
    /// single channel element, 2 for a channel pair element; other values are clamped to this
    /// range).
    ///
    /// If `ps` is true and the element has one core channel, the element uses the PS tool and
    /// produces two output channels (Annex 8.A).
    pub fn new(num_channels: usize, ps: bool) -> Self {
        let num_channels = num_channels.clamp(1, 2);
        let ps = ps && num_channels == 1;
        let num_outputs = if ps { 2 } else { num_channels };

        SbrElement {
            bitstream: SbrBitstreamDecoder::new(num_channels == 2),
            channels: (0..num_channels).map(|_| SbrChannelProcessor::new()).collect(),
            synthesis: (0..num_outputs).map(|_| QmfSynthesis::new()).collect(),
            ps: ps.then(|| {
                Box::new(PsStage {
                    decoder: PsDecoder::new(),
                    out: [[[ZERO; QMF_BANDS]; QMF_SLOTS]; 2],
                })
            }),
            qmf: vec![[[ZERO; QMF_BANDS]; QMF_SLOTS + PS_LOOKAHEAD]; num_channels],
            frame: None,
            synced: false,
        }
    }

    /// The number of core channels of the element.
    pub fn num_channels(&self) -> usize {
        self.channels.len()
    }

    /// The number of output channels of the element.
    pub fn num_output_channels(&self) -> usize {
        self.synthesis.len()
    }

    /// Reset all state that depends on previously decoded frames, e.g., after a seek, but keep the
    /// most recently received SBR header (see [`SbrBitstreamDecoder::flush`]).
    pub fn flush(&mut self) {
        self.bitstream.flush();
        self.reset_signal_state();
    }

    /// Reset the signal processing and QMF synthesis state, and flush the PS state.
    fn reset_signal_state(&mut self) {
        for channel in self.channels.iter_mut() {
            channel.reset();
        }
        for synthesis in self.synthesis.iter_mut() {
            synthesis.reset();
        }
        if let Some(ps) = self.ps.as_deref_mut() {
            ps.decoder.flush();
        }
        self.frame = None;
        self.synced = false;
    }

    /// Discard any SBR data decoded for the current frame. Must be called before the SBR extension
    /// payload of a new frame is decoded.
    pub fn start_frame(&mut self) {
        self.frame = None;
    }

    /// Decode the SBR extension payload, sbr_extension_data() (Table 4.62), of the current frame.
    ///
    /// `payload` holds the bits following the 4-bit extension_type of the fill element, and
    /// `num_bits` is the exact length of the payload in bits. `crc` is true for EXT_SBR_DATA_CRC.
    /// `sbr_rate` is the SBR sampling frequency.
    ///
    /// Returns true if SBR data is available for the current frame. Invalid SBR data is logged, and
    /// the frame is processed without SBR data.
    pub fn decode_payload(
        &mut self,
        payload: &[u8],
        num_bits: usize,
        crc: bool,
        sbr_rate: u32,
    ) -> bool {
        let ps = self.ps.as_deref_mut().map(|ps| &mut ps.decoder);

        self.frame = match self.bitstream.decode(payload, num_bits, crc, sbr_rate, ps) {
            Ok(Some(frame)) => {
                self.synced = true;
                Some(frame)
            }
            Ok(None) => {
                debug!("sbr: no sbr header received yet");
                None
            }
            Err(err) => {
                // Errors are expected after a reset until the encoder sends data that does not
                // depend on previous frames, or a header. Only report errors of a synchronized
                // decoder as warnings.
                if self.synced {
                    warn!("sbr: discarding invalid sbr data: {}", err);
                }
                else {
                    debug!("sbr: discarding invalid sbr data: {}", err);
                }
                self.synced = false;
                None
            }
        };

        self.frame.is_some()
    }

    /// Process the current frame.
    ///
    /// `core` holds the 1024 time-domain samples of each core channel of the element, and the 2048
    /// samples of each output channel are written to `out`. This must be called for every frame,
    /// whether or not it carried SBR data.
    pub fn process(&mut self, core: &[&[f32]], out: &mut [&mut [f32]]) {
        let frame = self.frame.take();
        let header = self.bitstream.header();
        let bands = self.bitstream.freq_bands();
        let reset = frame.as_ref().is_some_and(|frame| frame.reset);

        // HF generation and adjustment of each channel (4.6.18.6, 4.6.18.7), producing X.
        for (ch, (channel, x)) in self.channels.iter_mut().zip(self.qmf.iter_mut()).enumerate() {
            let samples = core.get(ch).copied().unwrap_or_default();
            let data = frame.as_ref().and_then(|frame| frame.channels.get(ch));
            channel.process(samples, header, bands, data, reset, x);
        }

        // The PS tool generates the left and right channels from the monaural X (Annex 8.A.3). It
        // outputs the monaural signal in both channels in frames without valid ps_data() (Annex
        // 8.A.1).
        let ps_out = match self.ps.as_deref_mut() {
            Some(ps) => {
                let ps_data_present = frame.as_ref().is_some_and(|frame| frame.ps_data_present);
                let kx_plus_m = bands.map_or(QMF_ANALYSIS_BANDS, |b| b.kx.saturating_add(b.m));

                if let Some(x) = self.qmf.first() {
                    let [left, right] = &mut ps.out;
                    ps.decoder.process(x, ps_data_present, kx_plus_m, left, right);
                }

                Some(&ps.out)
            }
            None => None,
        };

        // The 64-band QMF synthesis of each output channel (4.6.18.4.2).
        for (i, synthesis) in self.synthesis.iter_mut().enumerate() {
            let x = match ps_out {
                Some(ps_out) => ps_out.get(i).map(|x| &x[..]),
                None => self.qmf.get(i).map(|x| &x[..QMF_SLOTS]),
            };
            synthesize(synthesis, x.unwrap_or_default(), out.get_mut(i));
        }
    }
}

/// Run the 64-band synthesis QMF bank (4.6.18.4.2) over the QMF subsamples `x` and write the
/// time-domain output to `out`.
fn synthesize(
    synthesis: &mut QmfSynthesis,
    x: &[[Complex<f32>; QMF_BANDS]],
    out: Option<&mut &mut [f32]>,
) {
    let out: &mut [f32] = match out {
        Some(out) => out,
        None => &mut [],
    };

    // Every subsample is synthesized, even if the output is discarded, to keep the state of the
    // filterbank consistent.
    for (l, x) in x.iter().enumerate() {
        let dst = out.get_mut(l * QMF_BANDS..).unwrap_or_default();
        synthesis.synthesize(x, dst);
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;
    use crate::aac::sbr::qmf::QmfAnalysis;

    /// The delay, in output samples, of X relative to the analysis of the core signal when no SBR
    /// data is present: X(l) = X_Low(l + t_HFAdj), where X_Low starts t_HFGen subsamples before
    /// the current frame (4.6.18.5).
    const LOW_BAND_DELAY: usize = (types::T_HF_GEN - types::T_HF_ADJ) * QMF_BANDS;

    /// A test signal of `num_frames` core frames.
    fn core_signal(num_frames: usize) -> Vec<f32> {
        (0..num_frames * CORE_FRAME_LEN)
            .map(|n| {
                let n = n as f64;
                (0.5 * (2.0 * PI * 0.013 * n).sin() + 0.25 * (2.0 * PI * 0.21 * n).cos()) as f32
            })
            .collect()
    }

    /// Process the core signal `core` (one channel per entry) with `element`, without SBR data,
    /// and return the output signal of each output channel.
    fn process_frames(element: &mut SbrElement, core: &[Vec<f32>]) -> Vec<Vec<f32>> {
        let num_frames = core[0].len() / CORE_FRAME_LEN;
        let mut out = vec![vec![0.0f32; num_frames * SBR_FRAME_LEN]; element.num_output_channels()];

        for i in 0..num_frames {
            let frame: Vec<&[f32]> =
                core.iter().map(|c| &c[i * CORE_FRAME_LEN..(i + 1) * CORE_FRAME_LEN]).collect();
            let mut dst: Vec<&mut [f32]> = out
                .iter_mut()
                .map(|o| &mut o[i * SBR_FRAME_LEN..(i + 1) * SBR_FRAME_LEN])
                .collect();
            element.process(&frame, &mut dst);
        }

        out
    }

    #[test]
    fn verify_element_configuration() {
        let cases = [(1, false, 1, 1), (1, true, 1, 2), (2, false, 2, 2), (2, true, 2, 2)];
        for (num_channels, ps, expected_channels, expected_outputs) in cases {
            let element = SbrElement::new(num_channels, ps);
            assert_eq!(element.num_channels(), expected_channels);
            assert_eq!(element.num_output_channels(), expected_outputs);
            assert_eq!(element.ps.is_some(), ps && num_channels == 1);
        }

        assert_eq!(SbrElement::new(0, false).num_channels(), 1);
        assert_eq!(SbrElement::new(3, false).num_channels(), 2);
    }

    #[test]
    fn verify_upsampling_without_sbr_data() {
        // Without SBR data, the output is the core signal passed through the 32-band analysis and
        // the 64-band synthesis filterbanks, delayed by the low band delay of the SBR tool.
        let core = core_signal(6);

        let mut element = SbrElement::new(1, false);
        let out = process_frames(&mut element, std::slice::from_ref(&core));

        let mut analysis = QmfAnalysis::new();
        let mut synthesis = QmfSynthesis::new();
        let mut expected = vec![0.0f32; 2 * core.len()];
        for (input, output) in core.chunks(32).zip(expected.chunks_mut(64)) {
            let mut w = [ZERO; QMF_ANALYSIS_BANDS];
            analysis.analyze(input, &mut w);
            let mut x = [ZERO; QMF_BANDS];
            x[..QMF_ANALYSIS_BANDS].copy_from_slice(&w);
            synthesis.synthesize(&x, output);
        }

        assert!(out[0][..LOW_BAND_DELAY].iter().all(|&s| s == 0.0));
        for (n, &y) in out[0].iter().enumerate().skip(LOW_BAND_DELAY) {
            let e = expected[n - LOW_BAND_DELAY];
            assert!((y - e).abs() <= 1e-5, "n={} y={} expected={}", n, y, e);
        }
        assert!(out[0].iter().map(|s| s * s).sum::<f32>() > 1.0);
    }

    #[test]
    fn verify_ps_element_without_ps_data() {
        // Without ps_data(), an element with PS outputs the monaural signal in both channels
        // (Annex 8.A.1).
        let core = vec![core_signal(4)];

        let mono = process_frames(&mut SbrElement::new(1, false), &core);
        let stereo = process_frames(&mut SbrElement::new(1, true), &core);

        assert_eq!(stereo.len(), 2);
        assert_eq!(stereo[0], mono[0]);
        assert_eq!(stereo[1], mono[0]);
    }

    #[test]
    fn verify_channel_pair_element() {
        // The channels of a channel pair element are processed independently.
        let a = core_signal(3);
        let b: Vec<f32> = a.iter().map(|s| -0.5 * s).collect();

        let pair = process_frames(&mut SbrElement::new(2, false), &[a.clone(), b.clone()]);
        let left = process_frames(&mut SbrElement::new(1, false), &[a]);
        let right = process_frames(&mut SbrElement::new(1, false), &[b]);

        assert_eq!(pair[0], left[0]);
        assert_eq!(pair[1], right[0]);
    }

    #[test]
    fn verify_discarded_output() {
        // The state of the filterbanks advances even if the output of a frame is discarded, or
        // the input is missing or short.
        let core = core_signal(3);
        let frames: Vec<&[f32]> = core.chunks(CORE_FRAME_LEN).collect();

        let mut a = SbrElement::new(1, true);
        let mut b = SbrElement::new(1, true);

        let mut out_a = [vec![0.0f32; SBR_FRAME_LEN], vec![0.0f32; SBR_FRAME_LEN]];
        let mut out_b = [vec![0.0f32; SBR_FRAME_LEN], vec![0.0f32; 100]];

        for frame in &frames[..2] {
            let [l, r] = &mut out_a;
            a.process(&[frame], &mut [l, r]);
        }
        b.process(&[frames[0]], &mut []);
        {
            let [l, r] = &mut out_b;
            b.process(&[frames[1]], &mut [l, r]);
        }
        assert_eq!(out_a[0], out_b[0]);
        assert_eq!(out_a[1][..100], out_b[1][..]);

        // Missing core channels are silent.
        let mut silent = vec![0.0f32; SBR_FRAME_LEN];
        let mut c = SbrElement::new(2, false);
        c.process(&[], &mut [&mut silent]);
        assert!(silent.iter().all(|&s| s == 0.0));
        c.process(&[&frames[0][..10]], &mut [&mut silent[..5]]);
    }

    #[test]
    fn verify_reset_and_invalid_payloads() {
        let core = vec![core_signal(3)];

        let mut element = SbrElement::new(1, true);
        let first = process_frames(&mut element, &core);

        // Invalid payloads do not provide SBR data.
        assert!(!element.decode_payload(&[], 0, false, 44100));
        assert!(!element.decode_payload(&[0xff; 4], 28, false, 44100));
        assert!(!element.decode_payload(&[0xff; 4], 28, true, 44100));
        assert!(!element.decode_payload(&[0x00; 4], 100, false, 44100));

        // After a flush, the element behaves as a new element. The SBR header that is kept is the
        // same as the one received in the first frame.
        element.flush();
        assert_eq!(process_frames(&mut element, &core), first);
    }
}
