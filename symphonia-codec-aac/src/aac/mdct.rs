// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! An IMDCT for frame lengths that are not a power of two (the 960 and 120 coefficient transforms
//! of 960-sample AAC frames). The power-of-two transforms use the IMDCT of `symphonia-core`.

use symphonia_core::dsp::complex::Complex;
use symphonia_core::dsp::mdct::Imdct;

/// The largest prime factor of a supported FFT size.
const MAX_RADIX: usize = 5;

/// A forward complex FFT of a size whose only prime factors are 2, 3, and 5.
struct MixedFft {
    size: usize,
    factors: Vec<usize>,
    twiddle: Vec<Complex<f32>>,
}

impl MixedFft {
    fn new(size: usize) -> Self {
        let mut factors = Vec::new();
        let mut rest = size;

        for radix in [5, 3, 2] {
            while rest % radix == 0 {
                factors.push(radix);
                rest /= radix;
            }
        }

        assert_eq!(rest, 1, "fft size must only have the prime factors 2, 3, and 5");

        let twiddle = (0..size)
            .map(|k| {
                let theta = -2.0 * std::f64::consts::PI * k as f64 / size as f64;
                Complex::new(theta.cos() as f32, theta.sin() as f32)
            })
            .collect();

        MixedFft { size, factors, twiddle }
    }

    fn fft(&self, input: &[Complex<f32>], output: &mut [Complex<f32>]) {
        self.recurse(output, input, self.size, 1, &self.factors);
    }

    /// Compute the DFT of the `n` elements `input[i * stride]` into `output[..n]`.
    fn recurse(
        &self,
        output: &mut [Complex<f32>],
        input: &[Complex<f32>],
        n: usize,
        stride: usize,
        factors: &[usize],
    ) {
        let Some((&radix, rest)) = factors.split_first()
        else {
            output[0] = input[0];
            return;
        };

        let m = n / radix;

        for q in 0..radix {
            self.recurse(
                &mut output[q * m..(q + 1) * m],
                &input[q * stride..],
                m,
                stride * radix,
                rest,
            );
        }

        let tw_step = self.size / n;
        let mut gathered = [Complex::new(0.0, 0.0); MAX_RADIX];

        for k in 0..m {
            for (q, g) in gathered.iter_mut().enumerate().take(radix) {
                *g = output[q * m + k];
            }

            for r in 0..radix {
                let bin = k + r * m;
                let mut sum = gathered[0];

                for (q, g) in gathered.iter().enumerate().take(radix).skip(1) {
                    sum += *g * self.twiddle[(q * bin % n) * tw_step];
                }

                output[bin] = sum;
            }
        }
    }
}

/// An IMDCT of `n` spectral coefficients that, other than `Imdct`, supports sizes of 2^a * 3 * 5.
pub struct MixedImdct {
    fft: MixedFft,
    input: Vec<Complex<f32>>,
    output: Vec<Complex<f32>>,
    twiddle: Vec<Complex<f32>>,
}

impl MixedImdct {
    pub fn new_scaled(n: usize, scale: f64) -> Self {
        let n2 = n / 2;
        let pi_n = std::f64::consts::PI / n as f64;
        let sqrt_scale = scale.abs().sqrt();

        let twiddle = (0..n2)
            .map(|k| {
                let theta = pi_n * (1.0 / 8.0 + k as f64);
                Complex::new((sqrt_scale * theta.cos()) as f32, (sqrt_scale * theta.sin()) as f32)
            })
            .collect();

        MixedImdct {
            fft: MixedFft::new(n2),
            input: vec![Complex::new(0.0, 0.0); n2],
            output: vec![Complex::new(0.0, 0.0); n2],
            twiddle,
        }
    }

    /// Same contract as `Imdct::imdct`: `spec` has `n` coefficients, `out` has `2 * n` samples.
    pub fn imdct(&mut self, spec: &[f32], out: &mut [f32]) {
        let n = self.fft.size << 1;
        let n2 = n >> 1;
        let n4 = n >> 2;

        assert_eq!(spec.len(), n);
        assert_eq!(out.len(), 2 * n);

        for (i, (&w, t)) in self.twiddle.iter().zip(self.input.iter_mut()).enumerate() {
            let even = spec[i * 2];
            let odd = -spec[n - 1 - i * 2];

            *t = Complex::new(odd * w.im - even * w.re, odd * w.re + even * w.im);
        }

        self.fft.fft(&self.input, &mut self.output);

        let (vec0, vec1) = out.split_at_mut(n2);
        let (vec1, vec2) = vec1.split_at_mut(n2);
        let (vec2, vec3) = vec2.split_at_mut(n2);

        for (i, (x, &w)) in self.output[..n4].iter().zip(self.twiddle[..n4].iter()).enumerate() {
            let val = w * x.conj();

            let fi = 2 * i;
            let ri = n2 - 1 - 2 * i;

            vec0[ri] = -val.im;
            vec1[fi] = val.im;
            vec2[ri] = val.re;
            vec3[fi] = val.re;
        }

        for (i, (x, &w)) in self.output[n4..].iter().zip(self.twiddle[n4..].iter()).enumerate() {
            let val = w * x.conj();

            let fi = 2 * i;
            let ri = n2 - 1 - 2 * i;

            vec0[fi] = -val.re;
            vec1[ri] = val.re;
            vec2[fi] = val.im;
            vec3[ri] = val.im;
        }
    }
}

/// An IMDCT of either a power-of-two or a mixed-radix size.
pub enum FrameImdct {
    Pow2(Imdct),
    Mixed(MixedImdct),
}

impl FrameImdct {
    pub fn new_scaled(n: usize, scale: f64) -> Self {
        if n.is_power_of_two() {
            FrameImdct::Pow2(Imdct::new_scaled(n, scale))
        }
        else {
            FrameImdct::Mixed(MixedImdct::new_scaled(n, scale))
        }
    }

    pub fn imdct(&mut self, spec: &[f32], out: &mut [f32]) {
        match self {
            FrameImdct::Pow2(imdct) => imdct.imdct(spec, out),
            FrameImdct::Mixed(imdct) => imdct.imdct(spec, out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imdct_analytical(x: &[f32], scale: f64) -> Vec<f32> {
        let n_in = x.len();
        let n_out = n_in * 2;
        let pi_2n = std::f64::consts::PI / (2 * n_out) as f64;

        (0..n_out)
            .map(|i| {
                let accum: f64 = x
                    .iter()
                    .enumerate()
                    .map(|(j, &v)| {
                        f64::from(v) * (pi_2n * ((2 * i + 1 + n_in) * (2 * j + 1)) as f64).cos()
                    })
                    .sum();
                (scale * accum) as f32
            })
            .collect()
    }

    #[test]
    fn mixed_imdct_matches_analytical() {
        for n in [120usize, 960] {
            let spec: Vec<f32> = (0..n).map(|i| ((i * 7 + 3) % 17) as f32 - 8.0).collect();
            let scale = 1.0 / (2 * n) as f64;

            let expected = imdct_analytical(&spec, scale);
            let mut actual = vec![0.0; 2 * n];
            MixedImdct::new_scaled(n, scale).imdct(&spec, &mut actual);

            for (a, e) in actual.iter().zip(&expected) {
                assert!((a - e).abs() < 1e-3, "{n}: {a} != {e}");
            }
        }
    }
}
