// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! QMF analysis and synthesis filterbanks for SBR.
//!
//! Implements the complex-valued 32-band analysis and 64-band synthesis
//! Quadrature Mirror Filterbanks specified in ISO/IEC 14496-3, 4.6.18.6.
//! The analysis bank decomposes 32 time-domain samples into 32 complex
//! QMF subbands; the synthesis bank reconstructs 64 time-domain samples
//! from 64 complex QMF subbands at twice the bandwidth.
//!
//! The ISO modulation equations are factored into fixed-size FFTs with
//! precomputed twiddles, reducing the per-slot QMF work while preserving the
//! direct matrix result.

use super::Complex;

use super::tables;
use super::SBR_BANDS;
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

const ZERO: Complex = Complex { re: 0.0, im: 0.0 };

// ---------------------------------------------------------------------------
// Shared DSP state
// ---------------------------------------------------------------------------

/// Shared pre-computed tables and scratch buffers for QMF operations.
///
/// The trigonometric factors are derived from the ISO modulation equations and
/// stored as pre/post twiddles around the FFTs.
pub struct SbrDsp {
    /// Pre-twiddle for the 64-point FFT analysis path.
    ana_pre_twiddle: [Complex32; 64],
    /// Post-twiddle for the 64-point FFT analysis path.
    ana_post_twiddle: [Complex32; 32],
    /// 64-point inverse FFT used by the analysis filterbank.
    ana_fft: Arc<dyn Fft<f32>>,
    /// Scratch buffer for the analysis FFT.
    ana_scratch: Vec<Complex32>,
    /// In-place FFT buffer for the analysis filterbank.
    ana_fft_buf: [Complex32; 64],
    /// Pre-twiddle for the 128-point FFT synthesis path.
    syn_pre_twiddle: [Complex32; SBR_BANDS],
    /// Post-twiddle for the 128-point FFT synthesis path.
    syn_post_twiddle: [Complex32; 128],
    /// 128-point inverse FFT used by the synthesis filterbank.
    syn_fft: Arc<dyn Fft<f32>>,
    /// Scratch buffer for the synthesis FFT.
    syn_scratch: Vec<Complex32>,
    /// In-place FFT buffer for the synthesis filterbank.
    syn_fft_buf: [Complex32; 128],
    /// Scratch space for the analysis polyphase sum u[n].
    work_a: [f32; 64],
}

impl SbrDsp {
    pub fn new() -> Self {
        let mut ana_pre_twiddle = [Complex32 { re: 0.0, im: 0.0 }; 64];
        for (n, twiddle) in ana_pre_twiddle.iter_mut().enumerate() {
            let angle = std::f32::consts::PI * n as f32 / 64.0;
            *twiddle = Complex32 { re: angle.cos(), im: angle.sin() };
        }

        let mut ana_post_twiddle = [Complex32 { re: 0.0, im: 0.0 }; 32];
        for (k, twiddle) in ana_post_twiddle.iter_mut().enumerate() {
            let angle = -std::f32::consts::PI * (k as f32 + 0.5) / 128.0;
            *twiddle = Complex32 { re: angle.cos(), im: angle.sin() };
        }

        let mut syn_pre_twiddle = [Complex32 { re: 0.0, im: 0.0 }; SBR_BANDS];
        for (k, twiddle) in syn_pre_twiddle.iter_mut().enumerate() {
            let angle = std::f32::consts::PI * k as f32 / 128.0;
            *twiddle = Complex32 { re: angle.cos(), im: angle.sin() };
        }

        let mut syn_post_twiddle = [Complex32 { re: 0.0, im: 0.0 }; 128];
        for (n, twiddle) in syn_post_twiddle.iter_mut().enumerate() {
            let angle = std::f32::consts::PI * (n as f32 + 0.5) / 128.0;
            *twiddle = Complex32 { re: angle.cos(), im: angle.sin() };
        }

        let mut planner = FftPlanner::new();
        let ana_fft = planner.plan_fft_inverse(64);
        let ana_scratch = vec![Complex32 { re: 0.0, im: 0.0 }; ana_fft.get_inplace_scratch_len()];
        let syn_fft = planner.plan_fft_inverse(128);
        let syn_scratch = vec![Complex32 { re: 0.0, im: 0.0 }; syn_fft.get_inplace_scratch_len()];

        Self {
            ana_pre_twiddle,
            ana_post_twiddle,
            ana_fft,
            ana_scratch,
            ana_fft_buf: [Complex32 { re: 0.0, im: 0.0 }; 64],
            syn_pre_twiddle,
            syn_post_twiddle,
            syn_fft,
            syn_scratch,
            syn_fft_buf: [Complex32 { re: 0.0, im: 0.0 }; 128],
            work_a: [0.0; 64],
        }
    }
}

// ---------------------------------------------------------------------------
// 32-band analysis filterbank
// ---------------------------------------------------------------------------

/// State for the QMF 32-band analysis filterbank (ISO/IEC 14496-3, 4.6.18.4.1).
#[derive(Clone)]
pub struct SbrAnalysis {
    /// Circular input sample history buffer (5 × 64 = 320 samples).
    ring: [f32; 320],
    /// Current write position in the circular buffer.
    cursor: usize,
}

impl SbrAnalysis {
    pub fn new() -> Self {
        Self { ring: [0.0; 320], cursor: 0 }
    }

    /// Feed 32 time-domain samples and produce 32 complex QMF subbands.
    pub fn analysis(&mut self, dsp: &mut SbrDsp, input: &[f32], out: &mut [Complex; SBR_BANDS]) {
        // Advance cursor and store new samples in time-reversed order.
        self.cursor = (self.cursor + 320 - 32) % 320;
        for (i, &s) in input.iter().rev().enumerate() {
            self.ring[self.cursor + i] = s;
        }

        // Polyphase prototype filtering: 64-sample windowed sum.
        // Uses even-indexed coefficients from the 640-entry QMF window table.
        let z = &mut dsp.work_a;
        for n in 0..64 {
            let mut s = 0.0f32;
            for p in 0..5 {
                let idx = (self.cursor + n + p * 64) % 320;
                s += self.ring[idx] * tables::QMF_WINDOW[(n + p * 64) * 2];
            }
            z[n] = s;
        }

        for (dst, (&u, &twiddle)) in
            dsp.ana_fft_buf.iter_mut().zip(z.iter().zip(dsp.ana_pre_twiddle.iter()))
        {
            dst.re = u * twiddle.re;
            dst.im = u * twiddle.im;
        }

        dsp.ana_fft.process_with_scratch(&mut dsp.ana_fft_buf, &mut dsp.ana_scratch);

        *out = [ZERO; SBR_BANDS];
        for (dst, (&x, &twiddle)) in
            out[..32].iter_mut().zip(dsp.ana_fft_buf[..32].iter().zip(dsp.ana_post_twiddle.iter()))
        {
            dst.re = 2.0 * (x.re * twiddle.re - x.im * twiddle.im);
            dst.im = 2.0 * (x.re * twiddle.im + x.im * twiddle.re);
        }
    }
}

// ---------------------------------------------------------------------------
// 64-band synthesis filterbank
// ---------------------------------------------------------------------------

/// State for the QMF 64-band synthesis filterbank (ISO/IEC 14496-3, 4.6.18.4.2).
#[derive(Clone)]
pub struct SbrSynthesis {
    /// Circular history buffer for polyphase output (10 × 128 = 1280 samples).
    ring: [f32; 1280],
    /// Current write position in the circular buffer.
    cursor: usize,
}

impl SbrSynthesis {
    pub fn new() -> Self {
        Self { ring: [0.0; 1280], cursor: 0 }
    }

    /// Reconstruct 64 time-domain samples from 64 complex QMF subbands.
    ///
    pub fn synthesis(
        &mut self,
        dsp: &mut SbrDsp,
        input: &[Complex; SBR_BANDS],
        active_bands: usize,
        out: &mut [f32],
    ) {
        // Advance cursor for new 128-sample segment.
        self.cursor = (self.cursor + 1280 - 128) % 1280;

        let active_bands = active_bands.min(SBR_BANDS);
        dsp.syn_fft_buf.fill(Complex32 { re: 0.0, im: 0.0 });

        for (dst, (&x, &twiddle)) in dsp.syn_fft_buf[..active_bands]
            .iter_mut()
            .zip(input[..active_bands].iter().zip(dsp.syn_pre_twiddle.iter()))
        {
            dst.re = x.re * twiddle.re - x.im * twiddle.im;
            dst.im = x.re * twiddle.im + x.im * twiddle.re;
        }

        dsp.syn_fft.process_with_scratch(&mut dsp.syn_fft_buf, &mut dsp.syn_scratch);

        let h = &mut self.ring[self.cursor..self.cursor + 128];
        for ((v, &x), &twiddle) in
            h.iter_mut().zip(dsp.syn_fft_buf.iter()).zip(dsp.syn_post_twiddle.iter())
        {
            *v = -(x.re * twiddle.re - x.im * twiddle.im) * (1.0 / 64.0);
        }

        // Polyphase windowed sum using the full 640-entry synthesis window.
        for k in 0..64 {
            let mut s = 0.0f32;
            for p in 0..5 {
                s +=
                    self.ring[(self.cursor + 256 * p + k) % 1280] * tables::QMF_WINDOW[128 * p + k];
                s += self.ring[(self.cursor + 256 * p + k + 192) % 1280]
                    * tables::QMF_WINDOW[128 * p + k + 64];
            }
            out[k] = s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32, tol: f32, what: &str) {
        let delta = (actual - expected).abs();
        assert!(
            delta <= tol,
            "{}: actual {}, expected {}, delta {} > {}",
            what,
            actual,
            expected,
            delta,
            tol
        );
    }

    #[test]
    fn analysis_fft_matches_direct_matrix() {
        let mut dsp = SbrDsp::new();
        let mut ana = SbrAnalysis::new();
        let input = std::array::from_fn::<_, 32, _>(|i| {
            (i as f32 * 0.17).sin() * 0.75 + (i as f32 * 0.31).cos() * 0.25
        });
        let mut out = [ZERO; SBR_BANDS];

        ana.analysis(&mut dsp, &input, &mut out);

        let mut z = [0.0f64; 64];
        for (n, zn) in z.iter_mut().enumerate() {
            for p in 0..5 {
                let idx = (ana.cursor + n + p * 64) % 320;
                *zn += f64::from(ana.ring[idx]) * f64::from(tables::QMF_WINDOW[(n + p * 64) * 2]);
            }
        }

        for (k, actual) in out[..32].iter().enumerate() {
            let mut re = 0.0f64;
            let mut im = 0.0f64;
            for (n, &u) in z.iter().enumerate() {
                let angle = std::f64::consts::PI / 64.0 * (k as f64 + 0.5) * (2.0 * n as f64 - 0.5);
                re += 2.0 * u * angle.cos();
                im += 2.0 * u * angle.sin();
            }
            assert_close(actual.re, re as f32, 1.0e-4, "analysis re");
            assert_close(actual.im, im as f32, 1.0e-4, "analysis im");
        }
    }

    #[test]
    fn synthesis_fft_matches_direct_matrix() {
        let mut dsp = SbrDsp::new();
        let mut syn = SbrSynthesis::new();
        let active_bands = 37;
        let mut input = [ZERO; SBR_BANDS];
        for (k, x) in input[..active_bands].iter_mut().enumerate() {
            x.re = (k as f32 * 0.11).sin() * 0.5;
            x.im = (k as f32 * 0.23).cos() * 0.25;
        }
        let mut out = [0.0f32; 64];

        syn.synthesis(&mut dsp, &input, active_bands, &mut out);

        for n in 0..128 {
            let mut expected = 0.0f64;
            for (k, x) in input[..active_bands].iter().enumerate() {
                let angle =
                    std::f64::consts::PI / 128.0 * (k as f64 + 0.5) * (2.0 * n as f64 - 255.0);
                expected +=
                    f64::from(x.re) * angle.cos() / 64.0 - f64::from(x.im) * angle.sin() / 64.0;
            }
            assert_close(syn.ring[syn.cursor + n], expected as f32, 1.0e-5, "synthesis");
        }
    }

    /// Verify that analysis followed by synthesis preserves signal energy.
    #[test]
    fn roundtrip_energy_preservation() {
        let mut dsp = SbrDsp::new();
        let mut ana = SbrAnalysis::new();
        let mut syn = SbrSynthesis::new();

        let fs = 24000.0f32;
        let tone = 1000.0f32;
        let amp = 0.5f32;
        let frames = 10;
        let core_len = 960;

        let signal: Vec<f32> = (0..frames * core_len)
            .map(|i| amp * (2.0 * std::f32::consts::PI * tone * i as f32 / fs).sin())
            .collect();

        let mut output = Vec::new();
        for f in 0..frames {
            let frame_in = &signal[f * core_len..(f + 1) * core_len];
            let mut slots = [[ZERO; SBR_BANDS]; 30];
            for (chunk, w) in frame_in.chunks(32).zip(slots.iter_mut()) {
                ana.analysis(&mut dsp, chunk, w);
            }
            let mut frame_out = vec![0.0f32; 30 * 64];
            for (slot, dst) in slots.iter().zip(frame_out.chunks_mut(64)) {
                syn.synthesis(&mut dsp, slot, SBR_BANDS, dst);
            }
            output.extend_from_slice(&frame_out);
        }

        // Skip warmup and check RMS ratio.
        let stable = &output[3 * 1920..];
        let rms = (stable.iter().map(|&s| s * s).sum::<f32>() / stable.len() as f32).sqrt();
        let expected = amp / 2.0f32.sqrt();
        let ratio = rms / expected;

        assert!(ratio > 0.5 && ratio < 2.0, "roundtrip RMS ratio {:.4} outside [0.5, 2.0]", ratio);
    }

    /// Verify frequency selectivity: energy concentrates in the expected band.
    #[test]
    fn frequency_selectivity() {
        let fs = 22050.0f32;
        let frames = 15;
        let n = 1024;

        for &freq in &[440.0, 2500.0, 8000.0] {
            let mut dsp = SbrDsp::new();
            let mut ana = SbrAnalysis::new();

            let signal: Vec<f32> = (0..frames * n)
                .map(|i| 0.5 * (2.0 * std::f32::consts::PI * freq * i as f32 / fs).sin())
                .collect();

            let mut energy = [0.0f32; 32];
            let mut w = [[ZERO; SBR_BANDS]; 32];

            // Process all frames to fill history, keep last.
            for f in 0..frames {
                let frame_in = &signal[f * n..(f + 1) * n];
                for (chunk, dst) in frame_in.chunks(32).zip(w.iter_mut()) {
                    ana.analysis(&mut dsp, chunk, dst);
                }
            }

            for slot in &w {
                for (k, c) in slot[..32].iter().enumerate() {
                    energy[k] += c.re * c.re + c.im * c.im;
                }
            }

            let peak_band = energy
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .map(|(i, _)| i)
                .unwrap();
            let expected_band = (freq * 64.0 / fs).round() as usize;

            // Peak energy should be within ±2 bands of expected.
            let diff = (peak_band as isize - expected_band as isize).unsigned_abs();
            assert!(
                diff <= 2,
                "freq {}: peak band {} vs expected {}, diff {}",
                freq,
                peak_band,
                expected_band,
                diff
            );
        }
    }
}
