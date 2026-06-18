// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Arbitrary-length IMDCT via `rustfft`.
//!
//! Provides an IMDCT that works for all sizes used by MPEG-4 AAC, including
//! non-power-of-2 sizes (960, 480, 240, 120) required by DAB+ and AAC-LD/ELD.
//! Uses the pre-twiddle → FFT → post-twiddle decomposition to express the
//! size-N IMDCT as a size-N/2 complex FFT.

use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

/// IMDCT for arbitrary even sizes via rustfft.
///
/// Supported sizes: 120, 128, 240, 256, 480, 512, 960, 1024.
/// Uses the algorithm: pre-twiddle spectral input, apply N/2-point FFT,
/// post-twiddle and fold into 2N time-domain output.
pub struct ImdctArb {
    /// N/2-point forward FFT plan.
    fft_plan: Arc<dyn Fft<f32>>,
    /// Half-size of the IMDCT (N/2).
    half_n: usize,
    /// Twiddle factors used for both pre- and post-FFT rotation.
    twiddle: Box<[Complex32]>,
    /// Pre-allocated FFT work buffer (avoids per-frame heap allocation).
    fft_buf: Vec<Complex32>,
    /// Scratch buffer for in-place FFT.
    scratch: Vec<Complex32>,
}

impl ImdctArb {
    /// Create a new IMDCT of size `n` with scaling factor `scale`.
    ///
    /// `n` must be even. The FFT is performed at size `n/2`.
    /// Positive `scale` applies `sqrt(|scale|)` gain.
    /// Negative `scale` additionally shifts the twiddle phase by `n/2`.
    pub fn new_scaled(n: usize, scale: f64) -> Self {
        assert!(n % 2 == 0, "IMDCT size must be even");

        let half_n = n / 2;
        let phase_offset = 1.0 / 8.0 + if scale >= 0.0 { 0.0 } else { half_n as f64 };
        let gain = scale.abs().sqrt();
        let step = std::f64::consts::PI / n as f64;

        let mut twiddle = Vec::with_capacity(half_n);

        for k in 0..half_n {
            let angle = step * (phase_offset + k as f64);
            twiddle.push(Complex32::new((gain * angle.cos()) as f32, (gain * angle.sin()) as f32));
        }

        let mut planner = FftPlanner::new();
        let fft_plan = planner.plan_fft_forward(half_n);
        let scratch_len = fft_plan.get_inplace_scratch_len();

        Self {
            fft_plan,
            half_n,
            twiddle: twiddle.into_boxed_slice(),
            fft_buf: vec![Complex32::new(0.0, 0.0); half_n],
            scratch: vec![Complex32::new(0.0, 0.0); scratch_len],
        }
    }

    /// Compute the IMDCT: `spec` has length N, `out` has length 2N.
    pub fn imdct(&mut self, spec: &[f32], out: &mut [f32]) {
        let n = self.half_n * 2;
        let half = self.half_n;
        let quarter = half / 2;

        assert_eq!(spec.len(), n);
        assert_eq!(out.len(), 2 * n);

        // Step 1: Pre-twiddle — form N/2-point complex input for the FFT.
        //   z[k] = (odd · w.im - even · w.re) + j(odd · w.re + even · w.im)
        //   where even = spec[2k], odd = -spec[N-1-2k]
        for k in 0..half {
            let w = &self.twiddle[k];
            let ev = spec[2 * k];
            let od = -spec[n - 1 - 2 * k];
            self.fft_buf[k] = Complex32::new(od * w.im - ev * w.re, od * w.re + ev * w.im);
        }

        // Step 2: In-place FFT.
        self.fft_plan.process_with_scratch(&mut self.fft_buf, &mut self.scratch);

        // Step 3: Post-twiddle and output mapping.
        //
        // Post-twiddle: val[k] = post_tw[k] · conj(fft_out[k])
        //   val.re = w.re · x.re + w.im · x.im
        //   val.im = w.im · x.re - w.re · x.im
        //
        // Map into four quadrants of the 2N output:
        //   Q0 = out[0 .. N/2]
        //   Q1 = out[N/2 .. N]
        //   Q2 = out[N .. 3N/2]
        //   Q3 = out[3N/2 .. 2N]
        //
        // First quarter (k = 0..N/4):
        //   Q0[N/2-1-2k] = -val.im
        //   Q1[2k]       =  val.im
        //   Q2[N/2-1-2k] =  val.re
        //   Q3[2k]       =  val.re
        //
        // Second quarter (k = N/4..N/2):
        //   Q0[2(k-N/4)]       = -val.re
        //   Q1[N/2-1-2(k-N/4)] =  val.re
        //   Q2[2(k-N/4)]       =  val.im
        //   Q3[N/2-1-2(k-N/4)] =  val.im

        for k in 0..quarter {
            let x = &self.fft_buf[k];
            let w = &self.twiddle[k];
            let vr = w.re * x.re + w.im * x.im;
            let vi = w.im * x.re - w.re * x.im;

            let fwd = 2 * k;
            let rev = half - 1 - fwd;

            out[rev] = -vi;
            out[half + fwd] = vi;
            out[2 * half + rev] = vr;
            out[3 * half + fwd] = vr;
        }

        for k in 0..quarter {
            let x = &self.fft_buf[quarter + k];
            let w = &self.twiddle[quarter + k];
            let vr = w.re * x.re + w.im * x.im;
            let vi = w.im * x.re - w.re * x.im;

            let fwd = 2 * k;
            let rev = half - 1 - fwd;

            out[fwd] = -vr;
            out[half + rev] = vr;
            out[2 * half + fwd] = vi;
            out[3 * half + rev] = vi;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZES: [usize; 8] = [120, 128, 240, 256, 480, 512, 960, 1024];

    #[test]
    fn zero_input_zero_output() {
        for &n in &SIZES {
            let mut m = ImdctArb::new_scaled(n, 1.0 / (2 * n) as f64);
            let spec = vec![0.0f32; n];
            let mut out = vec![0.0f32; 2 * n];
            m.imdct(&spec, &mut out);
            assert!(out.iter().all(|&v| v == 0.0), "n={}", n);
        }
    }

    #[test]
    fn impulse_produces_energy() {
        for &n in &SIZES {
            let mut m = ImdctArb::new_scaled(n, 1.0 / (2 * n) as f64);
            let mut spec = vec![0.0f32; n];
            spec[0] = 1.0;
            let mut out = vec![0.0f32; 2 * n];
            m.imdct(&spec, &mut out);
            let e: f32 = out.iter().map(|v| v * v).sum();
            assert!(e > 0.0, "n={}", n);
        }
    }

    #[test]
    fn linearity() {
        for &n in &SIZES {
            let s = 1.0 / (2 * n) as f64;
            let mut m = ImdctArb::new_scaled(n, s);

            let mut x = vec![0.0f32; n];
            let mut y = vec![0.0f32; n];
            x[0] = 1.0;
            y[n / 4] = 1.0;

            let combined: Vec<f32> = x.iter().zip(&y).map(|(&a, &b)| 2.0 * a + 0.5 * b).collect();

            let mut ox = vec![0.0f32; 2 * n];
            let mut oy = vec![0.0f32; 2 * n];
            let mut oc = vec![0.0f32; 2 * n];

            m.imdct(&x, &mut ox);
            m.imdct(&y, &mut oy);
            m.imdct(&combined, &mut oc);

            for k in 0..2 * n {
                let expected = 2.0 * ox[k] + 0.5 * oy[k];
                assert!((oc[k] - expected).abs() < 1e-4, "n={} k={}", n, k);
            }
        }
    }

    #[test]
    fn sign_of_scale_matters() {
        for &n in &SIZES {
            let mut mp = ImdctArb::new_scaled(n, 1.0);
            let mut mn = ImdctArb::new_scaled(n, -1.0);

            let mut spec = vec![0.0f32; n];
            spec[0] = 1.0;

            let mut op = vec![0.0f32; 2 * n];
            let mut on = vec![0.0f32; 2 * n];
            mp.imdct(&spec, &mut op);
            mn.imdct(&spec, &mut on);

            let differ = op.iter().zip(&on).any(|(a, b)| (a - b).abs() > 1e-6);
            assert!(differ, "n={}", n);
        }
    }
}
