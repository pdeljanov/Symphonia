// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// Previous Author: Kostya Shishkov <kostya.shiskov@gmail.com>
//
// This source file includes code originally written for the NihAV
// project. With the author's permission, it has been relicensed for,
// and ported to the Symphonia project.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::dsp::mdct::Imdct;

use crate::aac::common::*;
use crate::aac::imdct_arb::ImdctArb;
use crate::aac::window::*;

/// IMDCT engine that abstracts over power-of-2 and non-power-of-2 sizes.
///
/// Supports all AAC MDCT sizes: 120, 128, 240, 256, 480, 512, 960, and 1024.
/// Power-of-2 sizes use the optimised symphonia-core IMDCT; others use `rustfft`.
enum ImdctEngine {
    /// Power-of-2 IMDCT from symphonia-core (128, 256, 512, 1024).
    Pow2(Imdct),
    /// Arbitrary-length IMDCT using rustfft (120, 240, 480, 960).
    Arbitrary(ImdctArb),
}

impl ImdctEngine {
    fn imdct(&mut self, spec: &[f32], out: &mut [f32]) {
        match self {
            ImdctEngine::Pow2(imdct) => imdct.imdct(spec, out),
            ImdctEngine::Arbitrary(imdct) => imdct.imdct(spec, out),
        }
    }
}

pub struct Dsp {
    /// Frame length for long windows (960 or 1024).
    frame_len: usize,
    /// Frame length for short windows (120 or 128).
    short_len: usize,
    kbd_long_win: Vec<f32>,
    kbd_short_win: Vec<f32>,
    sine_long_win: Vec<f32>,
    sine_short_win: Vec<f32>,
    imdct_long: ImdctEngine,
    imdct_short: ImdctEngine,
    pcm_long: Vec<f32>,
    pcm_short: Vec<f32>,
}

impl Dsp {
    /// Create a new DSP instance for the specified frame length.
    ///
    /// Supported frame lengths: 240, 256, 480, 512, 960, 1024.
    /// The short window length is derived as `frame_len / 8`.
    pub fn with_frame_len(frame_len: usize) -> Self {
        let short_len = frame_len / 8;

        let mut kbd_long_win = vec![0.0f32; frame_len];
        let mut kbd_short_win = vec![0.0f32; short_len];
        generate_window(WindowType::KaiserBessel(4.0), 1.0, frame_len, true, &mut kbd_long_win);
        generate_window(WindowType::KaiserBessel(6.0), 1.0, short_len, true, &mut kbd_short_win);
        let mut sine_long_win = vec![0.0f32; frame_len];
        let mut sine_short_win = vec![0.0f32; short_len];
        generate_window(WindowType::Sine, 1.0, frame_len, true, &mut sine_long_win);
        generate_window(WindowType::Sine, 1.0, short_len, true, &mut sine_short_win);

        let imdct_long = if frame_len.is_power_of_two() {
            ImdctEngine::Pow2(Imdct::new_scaled(frame_len, 1.0 / (2 * frame_len) as f64))
        }
        else {
            ImdctEngine::Arbitrary(ImdctArb::new_scaled(frame_len, 1.0 / (2 * frame_len) as f64))
        };

        let imdct_short = if short_len.is_power_of_two() {
            ImdctEngine::Pow2(Imdct::new_scaled(short_len, 1.0 / (2 * short_len) as f64))
        }
        else {
            ImdctEngine::Arbitrary(ImdctArb::new_scaled(short_len, 1.0 / (2 * short_len) as f64))
        };

        // pcm_short: short_len * 8 + short_len = frame_len + short_len
        let pcm_short_len = frame_len + short_len;

        Self {
            frame_len,
            short_len,
            kbd_long_win,
            kbd_short_win,
            sine_long_win,
            sine_short_win,
            imdct_long,
            imdct_short,
            pcm_long: vec![0.0; 2 * frame_len],
            pcm_short: vec![0.0; pcm_short_len],
        }
    }

    #[allow(clippy::cognitive_complexity)]
    pub fn synth(
        &mut self,
        coeffs: &[f32],
        delay: &mut [f32],
        seq: u8,
        window_shape: bool,
        prev_window_shape: bool,
        dst: &mut [f32],
    ) {
        let n = self.frame_len;
        let s = self.short_len;
        let half = n / 2;
        let short_win_point0 = half - s / 2;
        let short_win_point1 = half + s / 2;

        let (long_win, short_win) = match window_shape {
            true => (self.kbd_long_win.as_slice(), self.kbd_short_win.as_slice()),
            false => (self.sine_long_win.as_slice(), self.sine_short_win.as_slice()),
        };

        let (prev_long_win, prev_short_win) = match prev_window_shape {
            true => (self.kbd_long_win.as_slice(), self.kbd_short_win.as_slice()),
            false => (self.sine_long_win.as_slice(), self.sine_short_win.as_slice()),
        };

        // Inverse MDCT
        if seq != EIGHT_SHORT_SEQUENCE {
            self.imdct_long.imdct(&coeffs[..n], &mut self.pcm_long[..2 * n]);
        }
        else {
            for (ain, aout) in
                coeffs[..n].chunks_exact(s).zip(self.pcm_long[..2 * n].chunks_exact_mut(2 * s))
            {
                self.imdct_short.imdct(ain, aout);
            }

            // Zero the eight short sequence buffer.
            self.pcm_short[..n + s].fill(0.0);

            for (w, src) in self.pcm_long[..2 * n].chunks_exact(2 * s).enumerate() {
                if w > 0 {
                    for i in 0..s {
                        self.pcm_short[w * s + i] += src[i] * short_win[i];
                        self.pcm_short[w * s + i + s] += src[i + s] * short_win[s - 1 - i];
                    }
                }
                else {
                    for i in 0..s {
                        self.pcm_short[i] = src[i] * prev_short_win[i];
                        self.pcm_short[i + s] = src[i + s] * short_win[s - 1 - i];
                    }
                }
            }
        }

        // Output new audio samples.
        match seq {
            ONLY_LONG_SEQUENCE | LONG_START_SEQUENCE => {
                for i in 0..n {
                    dst[i] = delay[i] + (self.pcm_long[i] * prev_long_win[i]);
                }
            }
            EIGHT_SHORT_SEQUENCE => {
                dst[..short_win_point0].copy_from_slice(&delay[..short_win_point0]);

                for i in short_win_point0..n {
                    dst[i] = delay[i] + self.pcm_short[i - short_win_point0];
                }
            }
            LONG_STOP_SEQUENCE => {
                dst[..short_win_point0].copy_from_slice(&delay[..short_win_point0]);

                for i in short_win_point0..short_win_point1 {
                    dst[i] = delay[i] + self.pcm_long[i] * prev_short_win[i - short_win_point0];
                }
                for i in short_win_point1..n {
                    dst[i] = delay[i] + self.pcm_long[i];
                }
            }
            _ => unreachable!(),
        };

        // Save delay for overlap.
        match seq {
            ONLY_LONG_SEQUENCE | LONG_STOP_SEQUENCE => {
                for i in 0..n {
                    delay[i] = self.pcm_long[i + n] * long_win[n - 1 - i];
                }
            }
            EIGHT_SHORT_SEQUENCE => {
                for i in 0..short_win_point1 {
                    // Last part is already windowed.
                    delay[i] = self.pcm_short[i + half + s / 2];
                }

                delay[short_win_point1..n].fill(0.0);
            }
            LONG_START_SEQUENCE => {
                delay[..short_win_point0]
                    .copy_from_slice(&self.pcm_long[n..(short_win_point0 + n)]);

                for i in short_win_point0..short_win_point1 {
                    delay[i] = self.pcm_long[i + n] * short_win[s - 1 - (i - short_win_point0)];
                }

                delay[short_win_point1..n].fill(0.0);
            }
            _ => unreachable!(),
        };
    }
}
