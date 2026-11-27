// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The pitch and formant postfilters of ITU-T Recommendation G.723.1.

use crate::frame::{FRAME_LEN, SUBFRAME_LEN};
use crate::lsp::{Lpc, ORDER, mult, saturate};
use crate::synthesis::{HISTORY, Subframe};
use crate::tables::{POSTFILTER_POLE_Q15, POSTFILTER_ZERO_Q15};

const SEARCH_RADIUS: i32 = 3;
const LAG_MIN: i32 = 18;
const LAG_MAX: i32 = 142;

fn isqrt(n: i128) -> i64 {
    if n <= 0 {
        return 0;
    }
    let mut x = (n as f64).sqrt() as i128;
    while x * x > n {
        x -= 1;
    }
    while (x + 1) * (x + 1) <= n {
        x += 1;
    }
    x as i64
}

#[derive(Copy, Clone)]
struct Candidate {
    lag: usize,
    corr: i64,
    energy: i64,
}

impl Candidate {
    fn merit(&self) -> i128 {
        i128::from(self.corr) * i128::from(self.corr) / i128::from(self.energy.max(1))
    }
}

/// Keeps the candidate with the highest normalized correlation.
fn consider(best: &mut Option<Candidate>, cand: Candidate) {
    if cand.corr <= 0 || cand.energy <= 0 {
        return;
    }
    let better = best.is_none_or(|b| {
        let lhs = i128::from(cand.corr) * i128::from(cand.corr) * i128::from(b.energy);
        let rhs = i128::from(b.corr) * i128::from(b.corr) * i128::from(cand.energy);
        lhs > rhs
    });
    if better {
        *best = Some(cand);
    }
}

/// Applies the long term postfilter to a subframe of the excitation of the current frame.
///
/// `history` holds the excitation that preceded the frame, `frame` the excitation of the whole
/// frame, and `lag` is the pitch lag of the first or second half of the frame.
pub fn pitch_postfilter(
    history: &[i32; HISTORY],
    frame: &[i32; FRAME_LEN],
    start: usize,
    lag: i32,
    high_rate: bool,
) -> Subframe {
    let sub = &frame[start..start + SUBFRAME_LEN];
    let energy: i64 = sub.iter().map(|&v| i64::from(v) * i64::from(v)).sum();
    if energy == 0 {
        return std::array::from_fn(|n| sub[n]);
    }

    let centre = lag.clamp(LAG_MIN, LAG_MAX);
    let lags = (centre - SEARCH_RADIUS).max(1) as usize..=(centre + SEARCH_RADIUS) as usize;

    let sample = |index: isize| -> i64 {
        if index >= 0 {
            i64::from(frame[index as usize])
        }
        else {
            let back = (-index) as usize;
            if back <= HISTORY { i64::from(history[HISTORY - back]) } else { 0 }
        }
    };

    let mut forward = None;
    let mut backward = None;
    for m in lags {
        if start + SUBFRAME_LEN - 1 + m < FRAME_LEN {
            let (mut corr, mut en) = (0i64, 0i64);
            for n in 0..SUBFRAME_LEN {
                let x = i64::from(frame[start + n + m]);
                corr += i64::from(sub[n]) * x;
                en += x * x;
            }
            consider(&mut forward, Candidate { lag: m, corr, energy: en });
        }

        let (mut corr, mut en) = (0i64, 0i64);
        for n in 0..SUBFRAME_LEN {
            let x = sample((start + n) as isize - m as isize);
            corr += i64::from(sub[n]) * x;
            en += x * x;
        }
        consider(&mut backward, Candidate { lag: m, corr, energy: en });
    }

    let (cand, is_forward) = match (forward, backward) {
        (None, None) => return std::array::from_fn(|n| sub[n]),
        (Some(f), None) => (f, true),
        (None, Some(b)) => (b, false),
        (Some(f), Some(b)) => {
            if f.merit() >= b.merit() {
                (f, true)
            }
            else {
                (b, false)
            }
        }
    };

    if 4 * i128::from(cand.corr) * i128::from(cand.corr)
        < i128::from(cand.energy) * i128::from(energy)
    {
        return std::array::from_fn(|n| sub[n]);
    }

    let gain = if cand.corr >= cand.energy {
        32767
    }
    else {
        ((cand.corr << 15) / cand.energy).clamp(0, 32767)
    };
    let weight: i64 = if high_rate { 6144 } else { 8192 };
    let gain = (gain * weight) >> 15;

    let mut out = [0i64; SUBFRAME_LEN];
    let mut out_energy = 0i64;
    for n in 0..SUBFRAME_LEN {
        let x = if is_forward {
            i64::from(frame[start + n + cand.lag])
        }
        else {
            sample((start + n) as isize - cand.lag as isize)
        };
        out[n] = i64::from(sub[n]) + ((gain * x) >> 15);
        out_energy += out[n] * out[n];
    }

    if out_energy < energy || out_energy == 0 {
        return std::array::from_fn(|n| out[n] as i32);
    }

    let scale = isqrt((i128::from(energy) << 30) / i128::from(out_energy));
    std::array::from_fn(|n| ((out[n] * scale + (1 << 14)) >> 15) as i32)
}

/// The formant postfilter, with its tilt compensation and gain control.
pub struct FormantFilter {
    numerator: [i32; ORDER],
    denominator: [i32; ORDER],
    tilt_input: i32,
    tilt: i32,
    gain: i64,
}

impl FormantFilter {
    pub fn new() -> Self {
        FormantFilter {
            numerator: [0; ORDER],
            denominator: [0; ORDER],
            tilt_input: 0,
            tilt: 0,
            gain: 4096,
        }
    }

    pub fn process(&mut self, lpc: &Lpc, input: &Subframe) -> Subframe {
        let zeros: [i64; ORDER] =
            std::array::from_fn(|i| i64::from(mult(lpc[i], i32::from(POSTFILTER_ZERO_Q15[i]))));
        let poles: [i64; ORDER] =
            std::array::from_fn(|i| i64::from(mult(lpc[i], i32::from(POSTFILTER_POLE_Q15[i]))));

        let mut filtered = [0i32; SUBFRAME_LEN];
        for n in 0..SUBFRAME_LEN {
            let x = input[n];
            let mut acc = i64::from(x) << 13;
            for k in 0..ORDER {
                acc -= zeros[k] * i64::from(self.numerator[k]);
                acc += poles[k] * i64::from(self.denominator[k]);
            }
            let y = saturate(((acc + 4096) >> 13).clamp(i32::MIN as i64, i32::MAX as i64) as i32);
            self.numerator.copy_within(0..ORDER - 1, 1);
            self.denominator.copy_within(0..ORDER - 1, 1);
            self.numerator[0] = x;
            self.denominator[0] = y;
            filtered[n] = y;
        }

        let r0: i64 = input.iter().map(|&v| i64::from(v) * i64::from(v)).sum();
        let r1: i64 =
            (1..SUBFRAME_LEN).map(|n| i64::from(input[n]) * i64::from(input[n - 1])).sum();
        let k = if r0 > 0 { ((r1 << 15).div_euclid(r0)).clamp(-32768, 32767) as i32 } else { 0 };
        self.tilt = saturate((3 * self.tilt + k + 2) >> 2);
        let mu = self.tilt >> 2;

        let mut tilted = [0i32; SUBFRAME_LEN];
        let mut prev = self.tilt_input;
        for n in 0..SUBFRAME_LEN {
            tilted[n] = saturate(filtered[n] - ((mu * prev) >> 15));
            prev = filtered[n];
        }
        self.tilt_input = prev;

        let e_in = r0;
        let e_out: i64 = tilted.iter().map(|&v| i64::from(v) * i64::from(v)).sum();
        let target = if e_out == 0 {
            4096
        }
        else {
            isqrt((i128::from(e_in) << 24) / i128::from(e_out)).min(1 << 20)
        };

        std::array::from_fn(|n| {
            self.gain += (target - self.gain) >> 4;
            saturate(
                ((i64::from(tilted[n]) * self.gain * 17 + (1 << 15)) >> 16).clamp(-40000, 40000)
                    as i32,
            )
        })
    }
}
