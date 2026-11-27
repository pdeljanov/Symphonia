// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The excitation reconstruction and synthesis filter of ITU-T Recommendation G.723.1.

use crate::frame::{FrameParams, SUBFRAME_LEN, unpack_positions};
use crate::lsp::{Lpc, ORDER, saturate};
use crate::tables::{
    ADAPTIVE_GAIN_85, ADAPTIVE_GAIN_170, FIXED_GAIN, PITCH_1TAP_GAIN, PITCH_1TAP_OFFSET,
};

/// The number of past excitation samples that are kept.
pub const HISTORY: usize = 146;
const GAIN_LEVELS: u32 = 24;
/// The lag below which the 6.3 kbit/s excitation may be a train of pulse repetitions.
const SHORT_LAG: i32 = 58;
/// The fixed codebook amplitude is twice the tabulated gain.
const FIXED_GAIN_SHIFT: u32 = 1;

pub type Subframe = [i32; SUBFRAME_LEN];

pub struct SubframeGain {
    pub taps: &'static [i16; 5],
    pub fixed: i32,
    pub vector_index: usize,
    pub pulse_train: bool,
}

/// Splits a 12 bit gain index into the adaptive codebook gain vector and the fixed codebook gain.
pub fn decode_gain(high_rate: bool, lag_base: i32, index: u32) -> SubframeGain {
    let short = high_rate && lag_base < SHORT_LAG;

    let (vector_index, level, pulse_train) = if short {
        let masked = index & 0x7ff;
        (((masked / GAIN_LEVELS) as usize).min(84), masked % GAIN_LEVELS, index & 0x800 != 0)
    }
    else {
        (((index / GAIN_LEVELS) as usize).min(169), index % GAIN_LEVELS, false)
    };

    let taps =
        if short { &ADAPTIVE_GAIN_85[vector_index] } else { &ADAPTIVE_GAIN_170[vector_index] };

    SubframeGain {
        taps,
        fixed: i32::from(FIXED_GAIN[level as usize]) << FIXED_GAIN_SHIFT,
        vector_index,
        pulse_train,
    }
}

/// The contribution of the five tap pitch predictor, which repeats the last `lag + 2` samples of
/// the history periodically.
pub fn adaptive(history: &[i32; HISTORY], lag: i32, taps: &[i16; 5]) -> Subframe {
    let lag = lag.clamp(1, HISTORY as i32 - 2) as usize;

    let mut repeated = [0i64; SUBFRAME_LEN + 4];
    for (m, r) in repeated.iter_mut().enumerate() {
        let back = if m < 2 { lag + 2 - m } else { lag - ((m - 2) % lag) };
        *r = i64::from(history[HISTORY - back]);
    }

    std::array::from_fn(|n| {
        let acc: i64 = taps.iter().enumerate().map(|(j, &t)| i64::from(t) * repeated[n + j]).sum();
        ((acc + (1 << 13)) >> 14).clamp(i32::MIN as i64, i32::MAX as i64) as i32
    })
}

/// The 6.3 kbit/s fixed codebook, five or six pulses on the even or odd samples.
pub fn fixed_high(
    p: &FrameParams,
    subframe: usize,
    gain: &SubframeGain,
    lag_base: i32,
) -> Subframe {
    let count = if subframe % 2 == 0 { 6 } else { 5 };
    let slots = unpack_positions(p.position[subframe], count);

    let mut v = [0i32; SUBFRAME_LEN];
    for (k, &slot) in slots.iter().take(count).enumerate() {
        let negative = (p.signs[subframe] >> (count - 1 - k)) & 1 != 0;
        let amplitude = if negative { -gain.fixed } else { gain.fixed };
        let first = 2 * slot + p.grid[subframe];
        if gain.pulse_train {
            for pos in (first..SUBFRAME_LEN).step_by(lag_base.max(1) as usize) {
                v[pos] = v[pos].saturating_add(amplitude);
            }
        }
        else if first < SUBFRAME_LEN {
            v[first] = v[first].saturating_add(amplitude);
        }
    }
    v
}

/// The 5.3 kbit/s fixed codebook, one pulse on each of four interleaved tracks.
pub fn fixed_low(p: &FrameParams, subframe: usize, gain: &SubframeGain, lag: i32) -> Subframe {
    let mut v = [0i32; SUBFRAME_LEN];
    for track in 0..4 {
        let slot = ((p.position[subframe] >> (3 * track)) & 7) as usize;
        let pos = 2 * track + 8 * slot + p.grid[subframe];
        if pos < SUBFRAME_LEN {
            let positive = (p.signs[subframe] >> track) & 1 != 0;
            v[pos] = v[pos].saturating_add(if positive { gain.fixed } else { -gain.fixed });
        }
    }

    let enhance = i64::from(PITCH_1TAP_GAIN[gain.vector_index]);
    let delay = lag + i32::from(PITCH_1TAP_OFFSET[gain.vector_index]);
    if lag < SUBFRAME_LEN as i32 && enhance != 0 && delay > 0 {
        let delay = delay as usize;
        for n in delay..SUBFRAME_LEN {
            let added = ((enhance * i64::from(v[n - delay])) >> 15) as i32;
            v[n] = v[n].saturating_add(added);
        }
    }
    v
}

/// Runs the all-pole synthesis filter over a subframe of the excitation.
pub fn synthesize(lpc: &Lpc, excitation: &Subframe, mem: &mut [i32; ORDER]) -> Subframe {
    std::array::from_fn(|n| {
        let mut acc = i64::from(excitation[n]) << 13;
        for j in 0..ORDER {
            acc += i64::from(lpc[j]) * i64::from(mem[j]);
        }
        let y = saturate(((acc + 4096) >> 13).clamp(i32::MIN as i64, i32::MAX as i64) as i32);
        mem.copy_within(0..ORDER - 1, 1);
        mem[0] = y;
        y
    })
}
