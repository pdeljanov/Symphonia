// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::f64::consts::PI;

/// The number of coefficients of a transform.
pub const N: usize = 128;

/// A direct form windowed inverse MDCT with overlap-add of 128 coefficients into 128 samples.
pub struct Imdct {
    basis: Vec<f32>,
    window: [f32; 2 * N],
    overlap: [f32; N],
}

impl Imdct {
    pub fn new() -> Self {
        let mut basis = Vec::with_capacity(2 * N * N);
        for n in 0..2 * N {
            for k in 0..N {
                let phase = PI / N as f64 * (n as f64 + 0.5 + N as f64 / 2.0) * (k as f64 + 0.5);
                basis.push(phase.cos() as f32);
            }
        }

        let mut window = [0f32; 2 * N];
        for (i, w) in window.iter_mut().enumerate() {
            *w = (((i as f64 + 0.5) / N as f64 * PI / 2.0).sin() / 8.0) as f32;
        }

        Imdct { basis, window, overlap: [0.0; N] }
    }

    pub fn reset(&mut self) {
        self.overlap = [0.0; N];
    }

    /// Transforms the coefficients and writes the next 128 output samples.
    pub fn process(&mut self, coefs: &[f32; N], out: &mut [f32]) {
        let mut next = [0f32; N];

        for n in 0..2 * N {
            let row = &self.basis[n * N..(n + 1) * N];
            let sum: f32 = row.iter().zip(coefs.iter()).map(|(b, c)| b * c).sum();
            let y = sum * self.window[n];
            if n < N {
                out[n] = self.overlap[n] + y;
            }
            else {
                next[n - N] = y;
            }
        }

        self.overlap = next;
    }
}
