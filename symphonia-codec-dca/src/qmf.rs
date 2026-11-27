// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::f64::consts::PI;
use std::sync::OnceLock;

/// The cosine modulation coefficients of the 32-band synthesis filter bank.
struct CosMod {
    a: [f32; 256],
    b: [f32; 256],
    sum: [f32; 16],
    diff: [f32; 16],
}

fn cos_mod() -> &'static CosMod {
    static COS: OnceLock<CosMod> = OnceLock::new();
    COS.get_or_init(|| {
        let mut c = CosMod { a: [0.0; 256], b: [0.0; 256], sum: [0.0; 16], diff: [0.0; 16] };
        for k in 0..16 {
            for i in 0..16 {
                c.a[k * 16 + i] =
                    (f64::from((2 * i + 1) as u32 * (2 * k + 1) as u32) * PI / 64.0).cos() as f32;
                c.b[k * 16 + i] = (f64::from((i * (2 * k + 1)) as u32) * PI / 32.0).cos() as f32;
            }
            let x = f64::from((2 * k + 1) as u32) * PI / 128.0;
            c.sum[k] = (0.25 / (2.0 * x.cos())) as f32;
            c.diff[k] = (-0.25 / (2.0 * x.sin())) as f32;
        }
        c
    })
}

/// The synthesis (reconstruction) filter bank of a channel.
pub(crate) struct Qmf {
    x: [f32; 512],
    z: [f32; 64],
}

impl Qmf {
    pub(crate) fn new() -> Qmf {
        Qmf { x: [0.0; 512], z: [0.0; 64] }
    }

    pub(crate) fn reset(&mut self) {
        *self = Qmf::new();
    }

    /// Turns one sample of each of the 32 subbands into 32 PCM samples.
    pub(crate) fn synthesize(&mut self, input: &[f32; 32], coeff: &[f32; 512], out: &mut [f32]) {
        let c = cos_mod();

        let mut a = [0f32; 16];
        let mut b = [0f32; 16];
        for k in 0..16 {
            for i in 0..16 {
                a[k] += (input[2 * i] + input[2 * i + 1]) * c.a[k * 16 + i];
                let pair = if i > 0 { input[2 * i] + input[2 * i - 1] } else { input[0] };
                b[k] += pair * c.b[k * 16 + i];
            }
        }

        for k in 0..16 {
            self.x[k] = c.sum[k] * (a[k] + b[k]);
            self.x[31 - k] = c.diff[k] * (a[k] - b[k]);
        }

        for i in 0..32 {
            let k = 31 - i;
            for j in (0..512).step_by(64) {
                self.z[i] += coeff[i + j] * (self.x[i + j] - self.x[j + k]);
                self.z[32 + i] += coeff[32 + i + j] * (-self.x[i + j] - self.x[j + k]);
            }
        }

        out[..32].copy_from_slice(&self.z[..32]);

        self.x.copy_within(0..480, 32);
        self.z.copy_within(32..64, 0);
        self.z[32..].fill(0.0);
    }
}
