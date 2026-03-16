// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Arbitrary-length IMDCT implementation using `rustfft`.
//!
//! This module provides [`ImdctArb`], which implements the same IMDCT algorithm as
//! `symphonia_core::dsp::mdct::Imdct` but uses `rustfft` for the FFT step instead of
//! symphonia's built-in FFT. This enables support for all MDCT sizes used in MPEG-4 AAC:
//!
//! | Size | Use case                                           |
//! |------|----------------------------------------------------|
//! | 1024 | AAC-LC / HE-AAC long window                        |
//! |  128 | AAC-LC / HE-AAC short window                       |
//! |  960 | DAB+ AAC long window (ISO 14496-3 short-frame)     |
//! |  120 | DAB+ AAC short window (ISO 14496-3 short-frame)    |
//! |  512 | AAC-ELD long window                                |
//! |  480 | AAC-LD / AAC-ELD long window                       |
//! |  256 | AAC-ELD short window                               |
//! |  240 | AAC-LD short window                                |
//!
//! Symphonia's built-in FFT only supports power-of-2 sizes, so this module is required
//! for the non-power-of-2 variants (960, 120, 480, 240). It also works correctly for
//! power-of-2 sizes (1024, 128, 512, 256) but the built-in FFT is preferred for those.

use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use symphonia_core::dsp::complex::Complex;

/// An Inverse Modified Discrete Cosine Transform (IMDCT) that supports arbitrary even sizes.
///
/// This is functionally equivalent to `symphonia_core::dsp::mdct::Imdct` but delegates the
/// FFT computation to `rustfft`, which supports arbitrary sizes. Valid sizes include
/// 120, 128, 240, 256, 480, 512, 960, and 1024.
pub struct ImdctArb {
    fft: Arc<dyn Fft<f32>>,
    n2: usize,
    fft_scratch: Vec<Complex32>,
    twiddle: Box<[Complex]>,
}

impl ImdctArb {
    /// Create a new `ImdctArb` of size `n` with the given `scale`.
    ///
    /// `n` is the MDCT size (number of spectral coefficients). The FFT is performed at size
    /// `n / 2`. Unlike symphonia-core's `Imdct`, `n` does NOT need to be a power of two.
    ///
    /// The `scale` parameter controls the output scaling. A positive scale applies a forward
    /// scaling factor of `sqrt(|scale|)`. A negative scale shifts the twiddle phase by `n/2`.
    pub fn new_scaled(n: usize, scale: f64) -> Self {
        assert!(n % 2 == 0, "n must be even");

        let n2 = n / 2;

        // Build twiddle factors, identical to symphonia-core's Imdct.
        let mut twiddle = Vec::with_capacity(n2);

        let alpha = 1.0 / 8.0 + if scale.is_sign_positive() { 0.0 } else { n2 as f64 };
        let pi_n = std::f64::consts::PI / n as f64;
        let sqrt_scale = scale.abs().sqrt();

        for k in 0..n2 {
            let theta = pi_n * (alpha + k as f64);
            let re = sqrt_scale * theta.cos();
            let im = sqrt_scale * theta.sin();
            twiddle.push(Complex::new(re as f32, im as f32));
        }

        // Create the FFT plan for size n2.
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(n2);
        let fft_scratch = vec![Complex32::new(0.0, 0.0); fft.get_inplace_scratch_len()];

        ImdctArb { fft, n2, fft_scratch, twiddle: twiddle.into_boxed_slice() }
    }

    /// Compute the IMDCT of `spec` and write the result to `out`.
    ///
    /// `spec` must have length `n` (the MDCT size). `out` must have length `2 * n`.
    /// The algorithm is identical to symphonia-core's `Imdct::imdct`.
    pub fn imdct(&mut self, spec: &[f32], out: &mut [f32]) {
        let n = self.n2 << 1;
        let n2 = self.n2;
        let n4 = n2 >> 1;

        assert_eq!(spec.len(), n, "spec length must equal n");
        assert_eq!(out.len(), 2 * n, "out length must equal 2*n");

        // Pre-FFT twiddling: build the FFT input buffer.
        //
        // For each i in 0..n2:
        //   even = spec[2*i]
        //   odd  = -spec[n - 1 - 2*i]
        //   z[i] = Complex(odd * w[i].im - even * w[i].re,
        //                  odd * w[i].re + even * w[i].im)
        let mut fft_buf: Vec<Complex32> = Vec::with_capacity(n2);

        for (i, w) in self.twiddle.iter().enumerate() {
            let even = spec[i * 2];
            let odd = -spec[n - 1 - i * 2];
            let re = odd * w.im - even * w.re;
            let im = odd * w.re + even * w.im;
            fft_buf.push(Complex32::new(re, im));
        }

        // Perform the FFT in-place.
        self.fft.process_with_scratch(&mut fft_buf, &mut self.fft_scratch);

        // Post-FFT twiddling and output mapping.
        //
        // The output is split into four quadrants of size n2 each:
        //   vec0 = out[0      .. n2]
        //   vec1 = out[n2     .. 2*n2]
        //   vec2 = out[2*n2   .. 3*n2]
        //   vec3 = out[3*n2   .. 4*n2]
        //
        // For i in 0..n4 (first half of FFT output):
        //   val = w[i] * conj(fft_out[i])
        //   where conj(x) = (x.re, -x.im)
        //   and mul: (a.re*b.re - a.im*b.im, a.re*b.im + a.im*b.re)
        //   so val.re = w.re*x.re + w.im*x.im
        //      val.im = w.im*x.re - w.re*x.im
        let (vec0, rest) = out.split_at_mut(n2);
        let (vec1, rest) = rest.split_at_mut(n2);
        let (vec2, vec3) = rest.split_at_mut(n2);

        for i in 0..n4 {
            let x = &fft_buf[i];
            let w = &self.twiddle[i];

            // val = w * conj(x)
            let val_re = w.re * x.re + w.im * x.im;
            let val_im = w.im * x.re - w.re * x.im;

            let fi = 2 * i;
            let ri = n2 - 1 - 2 * i;

            vec0[ri] = -val_im;
            vec1[fi] = val_im;
            vec2[ri] = val_re;
            vec3[fi] = val_re;
        }

        for i in 0..n4 {
            let x = &fft_buf[n4 + i];
            let w = &self.twiddle[n4 + i];

            // val = w * conj(x)
            let val_re = w.re * x.re + w.im * x.im;
            let val_im = w.im * x.re - w.re * x.im;

            let fi = 2 * i;
            let ri = n2 - 1 - 2 * i;

            vec0[fi] = -val_re;
            vec1[ri] = val_re;
            vec2[fi] = val_im;
            vec3[ri] = val_im;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All MDCT sizes used by MPEG-4 AAC profiles.
    const ALL_SIZES: [usize; 8] = [120, 128, 240, 256, 480, 512, 960, 1024];

    /// Verify that `ImdctArb` can be constructed and executed for every supported size.
    #[test]
    fn verify_imdct_arb_all_sizes() {
        for &n in &ALL_SIZES {
            let scale = 1.0 / (2 * n) as f64;
            let mut imdct = ImdctArb::new_scaled(n, scale);

            let spec = vec![0.0f32; n];
            let mut out = vec![0.0f32; 2 * n];
            imdct.imdct(&spec, &mut out);

            // Zero input must produce zero output.
            assert!(
                out.iter().all(|&v| v == 0.0),
                "zero input produced non-zero output for n={}",
                n
            );
        }
    }

    /// Verify that non-zero input produces non-zero output for every supported size.
    #[test]
    fn verify_imdct_arb_nonzero_output() {
        for &n in &ALL_SIZES {
            let scale = 1.0 / (2 * n) as f64;
            let mut imdct = ImdctArb::new_scaled(n, scale);

            // Single impulse at DC.
            let mut spec = vec![0.0f32; n];
            spec[0] = 1.0;

            let mut out = vec![0.0f32; 2 * n];
            imdct.imdct(&spec, &mut out);

            let energy: f32 = out.iter().map(|v| v * v).sum();
            assert!(energy > 0.0, "DC impulse produced zero energy for n={}", n);
        }
    }

    /// Verify that the output has the expected energy scaling.
    ///
    /// For a unit impulse at bin 0 with scale = 1/(2N), the output energy should be
    /// finite and positive (Parseval-like relationship).
    #[test]
    fn verify_imdct_arb_energy_scaling() {
        for &n in &ALL_SIZES {
            let scale = 1.0 / (2 * n) as f64;
            let mut imdct = ImdctArb::new_scaled(n, scale);

            let mut spec = vec![0.0f32; n];
            spec[0] = 1.0;

            let mut out = vec![0.0f32; 2 * n];
            imdct.imdct(&spec, &mut out);

            let energy: f64 = out.iter().map(|v| (*v as f64) * (*v as f64)).sum();

            // The energy should be finite and proportional to the scaling.
            assert!(energy.is_finite() && energy > 0.0, "invalid energy {} for n={}", energy, n);
        }
    }

    /// Verify linearity: IMDCT(a*x + b*y) == a*IMDCT(x) + b*IMDCT(y).
    #[test]
    fn verify_imdct_arb_linearity() {
        for &n in &ALL_SIZES {
            let scale = 1.0 / (2 * n) as f64;

            let mut spec_x = vec![0.0f32; n];
            let mut spec_y = vec![0.0f32; n];
            spec_x[0] = 1.0;
            spec_y[n / 4] = 1.0;

            let a = 2.0f32;
            let b = 0.5f32;

            // Compute IMDCT(a*x + b*y) directly.
            let spec_combined: Vec<f32> =
                spec_x.iter().zip(spec_y.iter()).map(|(&x, &y)| a * x + b * y).collect();
            let mut imdct = ImdctArb::new_scaled(n, scale);
            let mut out_combined = vec![0.0f32; 2 * n];
            imdct.imdct(&spec_combined, &mut out_combined);

            // Compute a*IMDCT(x) + b*IMDCT(y) separately.
            let mut out_x = vec![0.0f32; 2 * n];
            let mut out_y = vec![0.0f32; 2 * n];
            imdct.imdct(&spec_x, &mut out_x);
            imdct.imdct(&spec_y, &mut out_y);

            for k in 0..2 * n {
                let expected = a * out_x[k] + b * out_y[k];
                let diff = (out_combined[k] - expected).abs();
                assert!(
                    diff < 1e-4,
                    "linearity violated at k={} for n={}: got {}, expected {}",
                    k,
                    n,
                    out_combined[k],
                    expected
                );
            }
        }
    }

    /// Verify that positive vs negative scale shifts the twiddle phase correctly.
    #[test]
    fn verify_imdct_arb_negative_scale() {
        for &n in &ALL_SIZES {
            let mut imdct_pos = ImdctArb::new_scaled(n, 1.0);
            let mut imdct_neg = ImdctArb::new_scaled(n, -1.0);

            let mut spec = vec![0.0f32; n];
            spec[0] = 1.0;

            let mut out_pos = vec![0.0f32; 2 * n];
            let mut out_neg = vec![0.0f32; 2 * n];

            imdct_pos.imdct(&spec, &mut out_pos);
            imdct_neg.imdct(&spec, &mut out_neg);

            // With different phase offsets the outputs should differ.
            let differs = out_pos.iter().zip(out_neg.iter()).any(|(a, b)| (a - b).abs() > 1e-6);
            assert!(differs, "positive and negative scale produced identical output for n={}", n);
        }
    }
}
