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
//! Both filterbanks use FFT-based modulation for O(N log N) complexity.
//!
//! ## Analysis (32 subbands from 64 polyphase samples)
//!
//! The full QMF analysis output can be expressed as:
//!   out[k] = (1/2) · Σ_{n=0}^{63} z[n] · exp(jπn(2k+1)/64)
//!
//! This equals `(1/2) · conj(FFT_64(q))[k]` where `q[n] = z[n] · exp(-jπn/64)`,
//! reducing the computation from O(N²) to O(N log N) using a single 64-point FFT.
//!
//! ## Synthesis (64 samples from 64 subbands)
//!
//! The synthesis DCT-IV and DST-IV of size N are computed using 2N-point FFTs:
//!   1. Pre-twiddle: q[n] = x[n] · exp(-jπn/(2N)), zero-padded to 2N
//!   2. Forward 2N-point FFT → F[k]
//!   3. Post-twiddle: T[k] = exp(jπ(2k+1)/(4N)) · conj(F[k])
//!   4. DCT-IV[k] = Re(T[k]),  DST-IV[k] = Im(T[k])

use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use symphonia_core::dsp::complex::Complex;

use super::tables;
use super::SBR_BANDS;

const ZERO: Complex = Complex { re: 0.0, im: 0.0 };
const CZ: Complex32 = Complex32 { re: 0.0, im: 0.0 };

// ---------------------------------------------------------------------------
// Shared DSP state
// ---------------------------------------------------------------------------

/// Shared pre-computed tables and scratch buffers for QMF operations.
///
/// Uses FFT-based transforms for O(N log N) complexity:
/// - Analysis: single 64-point FFT (replaces two 32×32 matrix multiplies)
/// - Synthesis: two 128-point FFTs (replaces two 64×64 matrix multiplies)
pub struct SbrDsp {
    // --- Analysis filterbank (64-point FFT) ---
    /// 64-point forward FFT plan.
    ana_fft: Arc<dyn Fft<f32>>,
    /// Pre-twiddle factors for analysis: e^{-jπn/64}, n = 0..63.
    ana_pre_tw: [Complex32; 64],
    /// Pre-allocated FFT work buffer (64 entries).
    ana_buf: Vec<Complex32>,
    /// FFT scratch buffer.
    ana_scratch: Vec<Complex32>,

    // --- Synthesis filterbank (128-point FFT for 64-point DCT-IV/DST-IV) ---
    /// 128-point forward FFT plan.
    syn_fft: Arc<dyn Fft<f32>>,
    /// Pre-twiddle factors for synthesis: e^{-jπn/128}, n = 0..63.
    syn_pre_tw: [Complex32; 64],
    /// Post-twiddle factors for synthesis: e^{jπ(2k+1)/256}, k = 0..63.
    syn_post_tw: [Complex32; 64],
    /// Pre-allocated FFT work buffer (128 entries).
    syn_buf: Vec<Complex32>,
    /// FFT scratch buffer.
    syn_scratch: Vec<Complex32>,

    // --- Scratch space for intermediate results ---
    work_a: [f32; 64],
}

impl SbrDsp {
    pub fn new() -> Self {
        let mut planner = FftPlanner::new();

        // Analysis: 64-point FFT
        let ana_fft = planner.plan_fft_forward(64);
        let ana_scratch_len = ana_fft.get_inplace_scratch_len();

        let mut ana_pre_tw = [CZ; 64];
        for n in 0..64 {
            let angle = std::f64::consts::PI * n as f64 / 64.0;
            ana_pre_tw[n] = Complex32::new(angle.cos() as f32, -(angle.sin() as f32));
        }

        // Synthesis: 128-point FFT for 64-point DCT-IV/DST-IV
        let syn_fft = planner.plan_fft_forward(128);
        let syn_scratch_len = syn_fft.get_inplace_scratch_len();

        let mut syn_pre_tw = [CZ; 64];
        for n in 0..64 {
            let angle = std::f64::consts::PI * n as f64 / 128.0;
            syn_pre_tw[n] = Complex32::new(angle.cos() as f32, -(angle.sin() as f32));
        }

        let mut syn_post_tw = [CZ; 64];
        for k in 0..64 {
            let angle = std::f64::consts::PI * (2 * k + 1) as f64 / 256.0;
            syn_post_tw[k] = Complex32::new(angle.cos() as f32, angle.sin() as f32);
        }

        Self {
            ana_fft,
            ana_pre_tw,
            ana_buf: vec![CZ; 64],
            ana_scratch: vec![CZ; ana_scratch_len],

            syn_fft,
            syn_pre_tw,
            syn_post_tw,
            syn_buf: vec![CZ; 128],
            syn_scratch: vec![CZ; syn_scratch_len],

            work_a: [0.0; 64],
        }
    }

    /// Compute 64-point DCT-IV of `input[0..63]` into `cos_out[0..63]`,
    /// and 64-point DST-IV into `sin_out[0..63]`, using a single 128-point FFT.
    ///
    /// Both transforms are obtained simultaneously from:
    ///   T[k] = exp(jπ(2k+1)/256) · conj(FFT_128(x[n]·exp(-jπn/128), zero-padded)[k])
    ///   DCT-IV[k] = Re(T[k]),  DST-IV[k] = Im(T[k])
    fn dct4_dst4_64(&mut self, input: &[f32], cos_out: &mut [f32], sin_out: &mut [f32]) {
        // Pre-twiddle and zero-pad to 128.
        for n in 0..64 {
            let tw = self.syn_pre_tw[n];
            let x = input[n];
            self.syn_buf[n] = Complex32::new(x * tw.re, x * tw.im);
        }
        for n in 64..128 {
            self.syn_buf[n] = CZ;
        }

        // 128-point FFT.
        self.syn_fft.process_with_scratch(&mut self.syn_buf, &mut self.syn_scratch);

        // Post-twiddle: T[k] = post_tw[k] · conj(F[k])
        for k in 0..64 {
            let f = self.syn_buf[k];
            let tw = self.syn_post_tw[k];
            // conj(f) = (f.re, -f.im)
            // tw · conj(f) = (tw.re*f.re + tw.im*f.im) + j(tw.im*f.re - tw.re*f.im)
            cos_out[k] = tw.re * f.re + tw.im * f.im;
            sin_out[k] = tw.im * f.re - tw.re * f.im;
        }
    }
}

// ---------------------------------------------------------------------------
// 32-band analysis filterbank
// ---------------------------------------------------------------------------

/// State for the QMF 32-band analysis filterbank (ISO/IEC 14496-3, 4.6.18.6.2).
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
    ///
    /// Uses a single 64-point FFT based on the identity:
    ///   out[k] = (1/2) · conj(FFT_64(z[n] · e^{-jπn/64}))[k],  k = 0..31
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

        // Pre-twiddle: q[n] = z[n] · e^{-jπn/64}
        for n in 0..64 {
            let tw = dsp.ana_pre_tw[n];
            let v = z[n];
            dsp.ana_buf[n] = Complex32::new(v * tw.re, v * tw.im);
        }

        // 64-point FFT.
        dsp.ana_fft.process_with_scratch(&mut dsp.ana_buf, &mut dsp.ana_scratch);

        // Output: out[k] = (1/2) · conj(F[k]) for k = 0..31
        *out = [ZERO; SBR_BANDS];
        for k in 0..32 {
            let f = dsp.ana_buf[k];
            out[k].re = f.re * 0.5;
            out[k].im = -f.im * 0.5;
        }
    }
}

// ---------------------------------------------------------------------------
// 64-band synthesis filterbank
// ---------------------------------------------------------------------------

/// State for the QMF 64-band synthesis filterbank (ISO/IEC 14496-3, 4.6.18.6.3).
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
    /// Uses FFT-based DCT-IV and DST-IV via 128-point FFTs.
    pub fn synthesis(&mut self, dsp: &mut SbrDsp, input: &[Complex; SBR_BANDS], out: &mut [f32]) {
        // Advance cursor for new 128-sample segment.
        self.cursor = (self.cursor + 1280 - 128) % 1280;

        // Scale for unity roundtrip gain (analysis × synthesis).
        const NORM: f32 = 1.0 / 8.0;

        // Separate and scale real/imaginary parts into stack-local arrays.
        let mut xr = [0.0f32; 64];
        let mut xi = [0.0f32; 64];
        for k in 0..64 {
            xr[k] = input[k].re * NORM;
            xi[k] = input[k].im * NORM;
        }

        // DCT-IV of real part and DST-IV of imaginary part via 128-point FFTs.
        let mut cr = [0.0f32; 64];
        let mut ci = [0.0f32; 64];
        let mut unused = [0.0f32; 64];

        dsp.dct4_dst4_64(&xr, &mut cr, &mut unused);
        dsp.dct4_dst4_64(&xi, &mut unused, &mut ci);

        // Combine DCT/DST outputs and write 128 samples into history ring.
        let h = &mut self.ring[self.cursor..self.cursor + 128];
        for i in 0..32 {
            let dr = -cr[i];
            let di = -ci[i];
            let er = -cr[63 - i];
            let ei = -ci[63 - i];

            h[i] = (dr - di) * 0.5;
            h[64 + 63 - i] = -(dr + di) * 0.5;
            h[63 - i] = (er - ei) * 0.5;
            h[64 + i] = -(er + ei) * 0.5;
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
                syn.synthesis(&mut dsp, slot, dst);
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
