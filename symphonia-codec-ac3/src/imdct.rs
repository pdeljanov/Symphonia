// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The inverse transforms of ETSI TS 102 366, clause 6.9.4.

use std::f64::consts::PI;

const N: usize = 512;

/// The zeroth order modified Bessel function of the first kind.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..64 {
        term *= q / (k * k) as f64;
        sum += term;
    }
    sum
}

/// The Kaiser-Bessel derived window (alpha = 5) the transform uses, first half only.
fn kbd_window() -> [f32; N / 2] {
    const ALPHA: f64 = 5.0;

    let mut kaiser = [0f64; N / 2 + 1];
    for (i, k) in kaiser.iter_mut().enumerate() {
        let r = 2.0 * i as f64 / (N / 2) as f64 - 1.0;
        *k = bessel_i0(PI * ALPHA * (1.0 - r * r).max(0.0).sqrt());
    }

    let total: f64 = kaiser.iter().sum();
    let mut window = [0f32; N / 2];
    let mut acc = 0.0;
    for (w, k) in window.iter_mut().zip(kaiser.iter()) {
        acc += k;
        *w = (acc / total).sqrt() as f32;
    }
    window
}

/// An in-place complex inverse FFT of a fixed power of two length.
struct Ifft {
    len: usize,
    twiddle: Vec<(f32, f32)>,
    bitrev: Vec<usize>,
}

impl Ifft {
    fn new(len: usize) -> Self {
        let bits = len.trailing_zeros();
        let twiddle = (0..len / 2)
            .map(|k| {
                let a = 2.0 * PI * k as f64 / len as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .collect();
        let bitrev = (0..len).map(|i| i.reverse_bits() >> (usize::BITS - bits)).collect();
        Ifft { len, twiddle, bitrev }
    }

    fn run(&self, re: &mut [f32], im: &mut [f32]) {
        for (i, &j) in self.bitrev.iter().enumerate() {
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }

        let mut half = 1;
        while half < self.len {
            let step = self.len / (2 * half);
            for base in (0..self.len).step_by(2 * half) {
                for k in 0..half {
                    let (wr, wi) = self.twiddle[k * step];
                    let (a, b) = (base + k, base + k + half);
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            half *= 2;
        }
    }
}

pub struct Imdct {
    window: [f32; N / 2],
    fft128: Ifft,
    fft64: Ifft,
    cos1: [f32; N / 4],
    sin1: [f32; N / 4],
    cos2: [f32; N / 8],
    sin2: [f32; N / 8],
}

impl Imdct {
    pub fn new() -> Self {
        let mut cos1 = [0f32; N / 4];
        let mut sin1 = [0f32; N / 4];
        for k in 0..N / 4 {
            let a = 2.0 * PI * (8 * k + 1) as f64 / (8 * N) as f64;
            cos1[k] = -a.cos() as f32;
            sin1[k] = -a.sin() as f32;
        }

        let mut cos2 = [0f32; N / 8];
        let mut sin2 = [0f32; N / 8];
        for k in 0..N / 8 {
            let a = 2.0 * PI * (8 * k + 1) as f64 / (4 * N) as f64;
            cos2[k] = -a.cos() as f32;
            sin2[k] = -a.sin() as f32;
        }

        Imdct {
            window: kbd_window(),
            fft128: Ifft::new(N / 4),
            fft64: Ifft::new(N / 8),
            cos1,
            sin1,
            cos2,
            sin2,
        }
    }

    /// Transforms 256 coefficients into 512 windowed samples.
    pub fn long(&self, x: &[f32; N / 2], out: &mut [f32; N]) {
        let mut zr = [0f32; N / 4];
        let mut zi = [0f32; N / 4];

        for k in 0..N / 4 {
            let (a, b) = (x[N / 2 - 2 * k - 1], x[2 * k]);
            zr[k] = a * self.cos1[k] - b * self.sin1[k];
            zi[k] = b * self.cos1[k] + a * self.sin1[k];
        }

        self.fft128.run(&mut zr, &mut zi);

        let mut yr = [0f32; N / 4];
        let mut yi = [0f32; N / 4];
        for n in 0..N / 4 {
            yr[n] = zr[n] * self.cos1[n] - zi[n] * self.sin1[n];
            yi[n] = zi[n] * self.cos1[n] + zr[n] * self.sin1[n];
        }

        let w = &self.window;
        for n in 0..N / 8 {
            out[2 * n] = -yi[N / 8 + n] * w[2 * n];
            out[2 * n + 1] = yr[N / 8 - n - 1] * w[2 * n + 1];
            out[N / 4 + 2 * n] = -yr[n] * w[N / 4 + 2 * n];
            out[N / 4 + 2 * n + 1] = yi[N / 4 - n - 1] * w[N / 4 + 2 * n + 1];
            out[N / 2 + 2 * n] = -yr[N / 8 + n] * w[N / 2 - 2 * n - 1];
            out[N / 2 + 2 * n + 1] = yi[N / 8 - n - 1] * w[N / 2 - 2 * n - 2];
            out[3 * N / 4 + 2 * n] = yi[n] * w[N / 4 - 2 * n - 1];
            out[3 * N / 4 + 2 * n + 1] = -yr[N / 4 - n - 1] * w[N / 4 - 2 * n - 2];
        }
    }

    /// Transforms 256 interleaved coefficients of two short blocks into 512 windowed samples.
    pub fn short(&self, x: &[f32; N / 2], out: &mut [f32; N]) {
        let mut yr = [[0f32; N / 8]; 2];
        let mut yi = [[0f32; N / 8]; 2];

        for half in 0..2 {
            let mut zr = [0f32; N / 8];
            let mut zi = [0f32; N / 8];

            // The coefficients of the two short transforms are interleaved.
            let coef = |i: usize| x[2 * i + half];

            for k in 0..N / 8 {
                let (a, b) = (coef(N / 4 - 2 * k - 1), coef(2 * k));
                zr[k] = a * self.cos2[k] - b * self.sin2[k];
                zi[k] = b * self.cos2[k] + a * self.sin2[k];
            }

            self.fft64.run(&mut zr, &mut zi);

            for n in 0..N / 8 {
                yr[half][n] = zr[n] * self.cos2[n] - zi[n] * self.sin2[n];
                yi[half][n] = zi[n] * self.cos2[n] + zr[n] * self.sin2[n];
            }
        }

        let w = &self.window;
        for n in 0..N / 8 {
            out[2 * n] = -yi[0][n] * w[2 * n];
            out[2 * n + 1] = yr[0][N / 8 - n - 1] * w[2 * n + 1];
            out[N / 4 + 2 * n] = -yr[0][n] * w[N / 4 + 2 * n];
            out[N / 4 + 2 * n + 1] = yi[0][N / 8 - n - 1] * w[N / 4 + 2 * n + 1];
            out[N / 2 + 2 * n] = -yr[1][n] * w[N / 2 - 2 * n - 1];
            out[N / 2 + 2 * n + 1] = yi[1][N / 8 - n - 1] * w[N / 2 - 2 * n - 2];
            out[3 * N / 4 + 2 * n] = yi[1][n] * w[N / 4 - 2 * n - 1];
            out[3 * N / 4 + 2 * n + 1] = -yr[1][N / 8 - n - 1] * w[N / 4 - 2 * n - 2];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_kbd_window_matches_specification_table() {
        let w = kbd_window();
        assert!((w[0] - 0.00014).abs() < 1e-5);
        assert!((w[100] - 0.44030).abs() < 1e-5);
        assert!((w[136] - 0.77762).abs() < 1e-5);
        assert!((w[200] - 0.99558).abs() < 1e-5);
        assert!((w[255] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn verify_ifft_matches_direct_evaluation() {
        let fft = Ifft::new(16);
        let mut re: Vec<f32> = (0..16).map(|i| (i * 7 % 5) as f32).collect();
        let mut im: Vec<f32> = (0..16).map(|i| (i * 3 % 4) as f32).collect();
        let (r0, i0) = (re.clone(), im.clone());
        fft.run(&mut re, &mut im);
        for n in 0..16 {
            let (mut er, mut ei) = (0.0f64, 0.0f64);
            for k in 0..16 {
                let a = 2.0 * PI * (k * n) as f64 / 16.0;
                er += f64::from(r0[k]) * a.cos() - f64::from(i0[k]) * a.sin();
                ei += f64::from(r0[k]) * a.sin() + f64::from(i0[k]) * a.cos();
            }
            assert!((f64::from(re[n]) - er).abs() < 1e-4 && (f64::from(im[n]) - ei).abs() < 1e-4);
        }
    }
}
