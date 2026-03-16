// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! QMF analysis and synthesis filterbanks for SBR (ISO/IEC 14496-3 4.6.18.6).
//!
//! Uses DCT-IV/DST-IV modulation for the complex QMF filterbank.
//! The analysis filterbank decomposes 32 time-domain samples into 32 complex
//! QMF subbands. The synthesis filterbank reconstructs 64 time-domain samples
//! from 64 complex QMF subbands (twice the bandwidth).

use symphonia_core::dsp::complex::Complex;

use super::tables;
use super::SBR_BANDS;

const COMPLEX_ZERO: Complex = Complex { re: 0.0, im: 0.0 };

/// Precomputed cosine/sine tables for DCT-IV and DST-IV of size N.
struct DctTable {
    cos: Box<[f32]>,
    sin: Box<[f32]>,
    n: usize,
}

impl DctTable {
    fn new(n: usize) -> Self {
        let pi_n = std::f64::consts::PI / n as f64;
        let mut cos = Vec::with_capacity(n * n);
        let mut sin = Vec::with_capacity(n * n);
        for k in 0..n {
            for i in 0..n {
                let angle = pi_n * (i as f64 + 0.5) * (k as f64 + 0.5);
                cos.push(angle.cos() as f32);
                sin.push(angle.sin() as f32);
            }
        }
        Self { cos: cos.into_boxed_slice(), sin: sin.into_boxed_slice(), n }
    }

    /// Compute DCT-IV: X[k] = Σ_{n=0}^{N-1} x[n] * cos(π/N * (n+0.5) * (k+0.5))
    fn dct_iv(&self, input: &[f32], output: &mut [f32]) {
        let n = self.n;
        for k in 0..n {
            let row = &self.cos[k * n..(k + 1) * n];
            let mut sum = 0.0f32;
            for (x, &c) in input[..n].iter().zip(row.iter()) {
                sum += x * c;
            }
            output[k] = sum;
        }
    }

    /// Compute DST-IV: X[k] = Σ_{n=0}^{N-1} x[n] * sin(π/N * (n+0.5) * (k+0.5))
    fn dst_iv(&self, input: &[f32], output: &mut [f32]) {
        let n = self.n;
        for k in 0..n {
            let row = &self.sin[k * n..(k + 1) * n];
            let mut sum = 0.0f32;
            for (x, &s) in input[..n].iter().zip(row.iter()) {
                sum += x * s;
            }
            output[k] = sum;
        }
    }
}

/// Precomputed analysis post-rotation: angle = π(2k+1) / 128 per band.
struct AnalysisRotation {
    cos: [f32; 32],
    sin: [f32; 32],
}

impl AnalysisRotation {
    fn new() -> Self {
        let mut cos = [0.0f32; 32];
        let mut sin = [0.0f32; 32];
        for k in 0..32 {
            let angle = std::f64::consts::PI * (2 * k + 1) as f64 / 128.0;
            cos[k] = angle.cos() as f32;
            sin[k] = angle.sin() as f32;
        }
        Self { cos, sin }
    }
}

/// Shared DSP state for SBR QMF operations.
pub struct SbrDsp {
    dct32: DctTable,
    dct64: DctTable,
    ana_rot: AnalysisRotation,
    tmp0: [f32; 64],
    tmp1: [f32; 64],
    tmp2: [f32; 64],
    tmp3: [f32; 64],
}

impl SbrDsp {
    pub fn new() -> Self {
        Self {
            dct32: DctTable::new(32),
            dct64: DctTable::new(64),
            ana_rot: AnalysisRotation::new(),
            tmp0: [0.0; 64],
            tmp1: [0.0; 64],
            tmp2: [0.0; 64],
            tmp3: [0.0; 64],
        }
    }
}

/// QMF 32-band analysis filterbank (ISO/IEC 14496-3 4.6.18.6.2).
#[derive(Clone)]
pub struct SbrAnalysis {
    hist: [f32; 320],
    pos: usize,
}

impl SbrAnalysis {
    pub fn new() -> Self {
        Self { hist: [0.0; 320], pos: 0 }
    }

    /// Process 32 input samples and produce 32 complex QMF subbands.
    pub fn analysis(&mut self, dsp: &mut SbrDsp, samples: &[f32], dst: &mut [Complex; SBR_BANDS]) {
        self.pos += self.hist.len() - 32;
        if self.pos >= self.hist.len() {
            self.pos -= self.hist.len();
        }

        for (d, &src) in self.hist[self.pos..][..32].iter_mut().zip(samples.iter().rev()) {
            *d = src;
        }

        // Polyphase windowed sum using even-indexed window coefficients.
        let z = &mut dsp.tmp0;
        for (n, d) in z.iter_mut().enumerate() {
            *d = 0.0;
            for j in 0..5 {
                *d += self.hist[(self.pos + n + j * 64) % self.hist.len()]
                    * tables::SBR_QMF_WINDOW[(n + j * 64) * 2];
            }
        }

        let r_sub = &mut dsp.tmp2[..32];
        let i_sub = &mut dsp.tmp3[..32];
        for k in 0..32 {
            let x = z[k] * 0.5;
            let y = z[63 - k] * 0.5;
            r_sub[k] = x - y;
            i_sub[k] = x + y;
        }

        let r_out = &mut dsp.tmp0[..32];
        dsp.dct32.dct_iv(r_sub, r_out);
        let i_out = &mut dsp.tmp1[..32];
        dsp.dct32.dst_iv(i_sub, i_out);

        // Post-rotation by e^{-jπ(2k+1)/128}.
        *dst = [COMPLEX_ZERO; SBR_BANDS];
        for k in 0..32 {
            let re = r_out[k];
            let im = i_out[k];
            let c = dsp.ana_rot.cos[k];
            let s = dsp.ana_rot.sin[k];
            dst[k].re = re * c + im * s;
            dst[k].im = im * c - re * s;
        }
    }
}

/// QMF 64-band synthesis filterbank (ISO/IEC 14496-3 4.6.18.6.3).
#[derive(Clone)]
pub struct SbrSynthesis {
    hist: [f32; 1280],
    pos: usize,
}

impl SbrSynthesis {
    pub fn new() -> Self {
        Self { hist: [0.0; 1280], pos: 0 }
    }

    /// Process 64 complex QMF subbands and produce 64 time-domain output samples.
    pub fn synthesis(&mut self, dsp: &mut SbrDsp, src: &[Complex; SBR_BANDS], dst: &mut [f32]) {
        self.pos += self.hist.len() - 128;
        if self.pos >= self.hist.len() {
            self.pos -= self.hist.len();
        }

        // Normalization for unity roundtrip gain through analysis + synthesis.
        const SYNTH_SCALE: f32 = 1.0 / 8.0;

        let t_real = &mut dsp.tmp0;
        let t_imag = &mut dsp.tmp1;
        for k in 0..64 {
            t_real[k] = src[k].re * SYNTH_SCALE;
            t_imag[k] = src[k].im * SYNTH_SCALE;
        }

        let dct_out = &mut dsp.tmp2;
        dsp.dct64.dct_iv(t_real, dct_out);
        let dst_out = &mut dsp.tmp3;
        dsp.dct64.dst_iv(t_imag, dst_out);

        // Combine DCT/DST outputs directly into history buffer.
        let h = &mut self.hist[self.pos..self.pos + 128];
        for i in 0..32 {
            let r1 = -dct_out[i];
            let i1 = -dst_out[i];
            let r2 = -dct_out[63 - i];
            let i2 = -dst_out[63 - i];

            h[i] = (r1 - i1) * 0.5;
            h[64 + 63 - i] = -(r1 + i1) * 0.5;
            h[63 - i] = (r2 - i2) * 0.5;
            h[64 + i] = -(r2 + i2) * 0.5;
        }

        // Polyphase windowed sum using the full 640-entry synthesis window.
        for (k, d) in dst[..64].iter_mut().enumerate() {
            *d = 0.0;
            for n in 0..5 {
                *d += self.hist[(self.pos + 256 * n + k) % self.hist.len()]
                    * tables::SBR_QMF_WINDOW[128 * n + k];
                *d += self.hist[(self.pos + 256 * n + k + 192) % self.hist.len()]
                    * tables::SBR_QMF_WINDOW[128 * n + k + 64];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test QMF analysis → bypass → synthesis round-trip gain.
    #[test]
    fn qmf_roundtrip_gain() {
        let mut dsp = SbrDsp::new();
        let mut analysis = SbrAnalysis::new();
        let mut synthesis = SbrSynthesis::new();

        let sample_rate = 24000.0f32;
        let freq = 1000.0f32;
        let amplitude = 0.5f32;
        let n_frames = 10;
        let frame_samples = 960;

        let input: Vec<f32> = (0..n_frames * frame_samples)
            .map(|i| amplitude * (2.0 * std::f32::consts::PI * freq * i as f32 / sample_rate).sin())
            .collect();

        let mut all_output = Vec::new();

        for frame in 0..n_frames {
            let frame_in = &input[frame * frame_samples..(frame + 1) * frame_samples];
            let mut w = [[COMPLEX_ZERO; SBR_BANDS]; 30];
            for (chunk, d) in frame_in.chunks(32).zip(w.iter_mut()) {
                analysis.analysis(&mut dsp, chunk, d);
            }
            let mut output = vec![0.0f32; 30 * 64];
            for (slot, dst_chunk) in w.iter().zip(output.chunks_mut(64)) {
                synthesis.synthesis(&mut dsp, slot, dst_chunk);
            }
            all_output.extend_from_slice(&output);
        }

        let warmup = 3 * 1920;
        let stable = &all_output[warmup..];
        let rms = (stable.iter().map(|&s| s * s).sum::<f32>() / stable.len() as f32).sqrt();
        let peak = stable.iter().map(|&s| s.abs()).fold(0.0f32, f32::max);
        let expected_rms = amplitude / 2.0f32.sqrt();
        let expected_peak = amplitude;
        let rms_ratio = rms / expected_rms;
        let peak_ratio = peak / expected_peak;

        eprintln!("QMF roundtrip: rms_ratio={:.4} peak_ratio={:.4}", rms_ratio, peak_ratio);

        assert!(
            rms_ratio > 0.5 && rms_ratio < 2.0,
            "QMF roundtrip RMS ratio {:.4} is out of range [0.5, 2.0]",
            rms_ratio
        );
    }

    /// Test QMF roundtrip gain across multiple frequencies.
    #[test]
    fn qmf_roundtrip_multi_freq() {
        let sample_rate = 24000.0f32;
        let amplitude = 0.5f32;
        let n_frames = 20;
        let frame_samples = 960;
        let output_samples = 1920;
        let warmup = 5 * output_samples;

        let freqs = [200.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 11000.0];

        for &freq in &freqs {
            let mut dsp = SbrDsp::new();
            let mut analysis = SbrAnalysis::new();
            let mut synthesis = SbrSynthesis::new();

            let input: Vec<f32> = (0..n_frames * frame_samples)
                .map(|i| {
                    amplitude * (2.0 * std::f32::consts::PI * freq * i as f32 / sample_rate).sin()
                })
                .collect();

            let mut all_output = Vec::new();
            for frame in 0..n_frames {
                let frame_in = &input[frame * frame_samples..(frame + 1) * frame_samples];
                let mut w = [[COMPLEX_ZERO; SBR_BANDS]; 30];
                for (chunk, d) in frame_in.chunks(32).zip(w.iter_mut()) {
                    analysis.analysis(&mut dsp, chunk, d);
                }
                let mut output = vec![0.0f32; output_samples];
                for (slot, dst_chunk) in w.iter().zip(output.chunks_mut(64)) {
                    synthesis.synthesis(&mut dsp, slot, dst_chunk);
                }
                all_output.extend_from_slice(&output);
            }

            let stable = &all_output[warmup..];
            let rms = (stable.iter().map(|&s| s * s).sum::<f32>() / stable.len() as f32).sqrt();
            let expected_rms = amplitude / 2.0f32.sqrt();
            let rms_ratio = rms / expected_rms;

            eprintln!("  freq={:5.0} Hz: rms_ratio={:.4}", freq, rms_ratio);
        }

        {
            let mut dsp = SbrDsp::new();
            let mut analysis = SbrAnalysis::new();
            let mut synthesis = SbrSynthesis::new();

            let mut rng = 12345u32;
            let input: Vec<f32> = (0..n_frames * frame_samples)
                .map(|_| {
                    rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
                    amplitude * ((rng >> 16) as f32 / 32768.0 - 1.0)
                })
                .collect();

            let mut all_output = Vec::new();
            for frame in 0..n_frames {
                let frame_in = &input[frame * frame_samples..(frame + 1) * frame_samples];
                let mut w = [[COMPLEX_ZERO; SBR_BANDS]; 30];
                for (chunk, d) in frame_in.chunks(32).zip(w.iter_mut()) {
                    analysis.analysis(&mut dsp, chunk, d);
                }
                let mut output = vec![0.0f32; output_samples];
                for (slot, dst_chunk) in w.iter().zip(output.chunks_mut(64)) {
                    synthesis.synthesis(&mut dsp, slot, dst_chunk);
                }
                all_output.extend_from_slice(&output);
            }

            let stable = &all_output[warmup..];
            let out_rms = (stable.iter().map(|&s| s * s).sum::<f32>() / stable.len() as f32).sqrt();
            let in_stable = &input[warmup / 2..];
            let in_rms =
                (in_stable.iter().map(|&s| s * s).sum::<f32>() / in_stable.len() as f32).sqrt();
            let rms_ratio = out_rms / in_rms;

            eprintln!("  noise: rms_ratio={:.4} (in={:.6}, out={:.6})", rms_ratio, in_rms, out_rms);
        }
    }

    /// Test QMF roundtrip mimicking the actual SBR pipeline data flow.
    #[test]
    fn qmf_roundtrip_pipeline_flow() {
        use crate::aac::sbr::{MAX_SLOTS, QMF_DELAY, SBR_BANDS};

        let mut dsp = SbrDsp::new();
        let mut analysis = SbrAnalysis::new();
        let mut synthesis = SbrSynthesis::new();

        let amplitude = 0.5f32;
        let n_frames = 20;
        let frame_samples = 960;
        let output_samples = 1920;
        let warmup = 5 * output_samples;
        let num_time_slots = 15usize;
        let num_qmf_slots = num_time_slots * 2;

        let mut rng = 54321u32;
        let input: Vec<f32> = (0..n_frames * frame_samples)
            .map(|_| {
                rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
                amplitude * ((rng >> 16) as f32 / 32768.0 - 1.0)
            })
            .collect();

        let in_rms = {
            let stable = &input[warmup / 2..];
            (stable.iter().map(|&s| s * s).sum::<f32>() / stable.len() as f32).sqrt()
        };

        let mut w = [[COMPLEX_ZERO; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2];
        let mut x = [[COMPLEX_ZERO; SBR_BANDS]; QMF_DELAY + MAX_SLOTS * 2];

        let mut all_output = Vec::new();

        for frame in 0..n_frames {
            let frame_in = &input[frame * frame_samples..(frame + 1) * frame_samples];

            for (src_chunk, d) in frame_in.chunks(32).zip(w[QMF_DELAY..].iter_mut()) {
                analysis.analysis(&mut dsp, src_chunk, d);
            }

            for slot in 0..num_qmf_slots {
                x[slot] = w[QMF_DELAY + slot];
            }

            let mut output = vec![0.0f32; output_samples];
            for (src_slot, dst_chunk) in x.iter().zip(output.chunks_mut(64)) {
                synthesis.synthesis(&mut dsp, src_slot, dst_chunk);
            }

            all_output.extend_from_slice(&output);

            let start_copy = num_qmf_slots;
            let (dst_w, src_w) = w.split_at_mut(QMF_DELAY);
            let src_offset = start_copy - QMF_DELAY;
            dst_w.copy_from_slice(&src_w[src_offset..src_offset + QMF_DELAY]);
        }

        let stable = &all_output[warmup..];
        let out_rms = (stable.iter().map(|&s| s * s).sum::<f32>() / stable.len() as f32).sqrt();
        let ratio = out_rms / in_rms;

        eprintln!(
            "Pipeline flow roundtrip: in_rms={:.6} out_rms={:.6} ratio={:.4}",
            in_rms, out_rms, ratio
        );

        assert!(
            ratio > 0.5 && ratio < 2.0,
            "Pipeline flow roundtrip gain {:.4} is out of range [0.5, 2.0]",
            ratio
        );
    }

    /// Check which QMF band a specific frequency maps to.
    #[test]
    fn qmf_frequency_to_band() {
        let sample_rate = 22050.0f32;
        let n_frames = 15;
        let frame_samples = 1024;

        for &freq in &[440.0, 1000.0, 2500.0, 5000.0, 8000.0, 10000.0] {
            let mut analysis = SbrAnalysis::new();
            let mut dsp = SbrDsp::new();

            let input: Vec<f32> = (0..n_frames * frame_samples)
                .map(|i| 0.5 * (2.0 * std::f32::consts::PI * freq * i as f32 / sample_rate).sin())
                .collect();

            let mut band_energy = [0.0f32; 32];
            for frame in 0..n_frames {
                let frame_in = &input[frame * frame_samples..(frame + 1) * frame_samples];
                let mut w = [[COMPLEX_ZERO; SBR_BANDS]; 32];
                for (chunk, d) in frame_in.chunks(32).zip(w.iter_mut()) {
                    analysis.analysis(&mut dsp, chunk, d);
                }

                if frame == n_frames - 1 {
                    for slot in &w {
                        for (k, c) in slot[..32].iter().enumerate() {
                            band_energy[k] += c.re * c.re + c.im * c.im;
                        }
                    }
                }
            }

            let total: f32 = band_energy.iter().sum();
            let max_band = band_energy
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .map(|(i, _)| i)
                .unwrap();
            let max_energy = band_energy[max_band];

            let expected_band = (freq * 64.0 / sample_rate).round() as usize;
            eprintln!(
                "  freq={:5.0} Hz: peak_band={:2} (expected ~{}) energy_frac={:.3}",
                freq,
                max_band,
                expected_band,
                max_energy / total,
            );
        }
    }
}
