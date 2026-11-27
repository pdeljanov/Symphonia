// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
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

use crate::aac::common::*;
use crate::aac::mdct::FrameImdct;
use crate::aac::window::*;

pub struct Dsp {
    frame_len: usize,
    short_len: usize,
    kbd_long_win: Vec<f32>,
    kbd_short_win: Vec<f32>,
    sine_long_win: Vec<f32>,
    sine_short_win: Vec<f32>,
    imdct_long: FrameImdct,
    imdct_short: FrameImdct,
    pcm_long: Vec<f32>,
    pcm_short: Vec<f32>,
}

fn window(mode: WindowType, size: usize) -> Vec<f32> {
    let mut win = vec![0.0; size];
    generate_window(mode, 1.0, size, true, &mut win);
    win
}

impl Dsp {
    /// Create the synthesis filterbank for frames of `frame_len` (1024 or 960) coefficients.
    pub fn new(frame_len: usize) -> Self {
        let short_len = frame_len / 8;

        Self {
            frame_len,
            short_len,
            kbd_long_win: window(WindowType::KaiserBessel(4.0), frame_len),
            kbd_short_win: window(WindowType::KaiserBessel(6.0), short_len),
            sine_long_win: window(WindowType::Sine, frame_len),
            sine_short_win: window(WindowType::Sine, short_len),
            imdct_long: FrameImdct::new_scaled(frame_len, 1.0 / (2 * frame_len) as f64),
            imdct_short: FrameImdct::new_scaled(short_len, 1.0 / (2 * short_len) as f64),
            pcm_long: vec![0.0; 2 * frame_len],
            pcm_short: vec![0.0; 9 * short_len],
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
        let frame_len = self.frame_len;
        let short_len = self.short_len;
        let win_point0 = frame_len / 2 - short_len / 2;
        let win_point1 = frame_len / 2 + short_len / 2;

        let (long_win, short_win) = match window_shape {
            true => (&self.kbd_long_win, &self.kbd_short_win),
            false => (&self.sine_long_win, &self.sine_short_win),
        };

        let (prev_long_win, prev_short_win) = match prev_window_shape {
            true => (&self.kbd_long_win, &self.kbd_short_win),
            false => (&self.sine_long_win, &self.sine_short_win),
        };

        // Inverse MDCT
        if seq != EIGHT_SHORT_SEQUENCE {
            self.imdct_long.imdct(&coeffs[..frame_len], &mut self.pcm_long);
        }
        else {
            for (ain, aout) in coeffs[..frame_len]
                .chunks_exact(short_len)
                .zip(self.pcm_long.chunks_exact_mut(2 * short_len))
            {
                self.imdct_short.imdct(ain, aout);
            }

            // Zero the eight short sequence buffer.
            self.pcm_short.fill(0.0);

            for (w, src) in self.pcm_long.chunks_exact(2 * short_len).enumerate() {
                if w > 0 {
                    for i in 0..short_len {
                        self.pcm_short[w * short_len + i] += src[i] * short_win[i];
                        self.pcm_short[w * short_len + i + short_len] +=
                            src[i + short_len] * short_win[short_len - 1 - i];
                    }
                }
                else {
                    for i in 0..short_len {
                        self.pcm_short[i] = src[i] * prev_short_win[i];
                        self.pcm_short[i + short_len] =
                            src[i + short_len] * short_win[short_len - 1 - i];
                    }
                }
            }
        }

        // Output new audio samples.
        match seq {
            ONLY_LONG_SEQUENCE | LONG_START_SEQUENCE => {
                for i in 0..frame_len {
                    dst[i] = delay[i] + (self.pcm_long[i] * prev_long_win[i]);
                }
            }
            EIGHT_SHORT_SEQUENCE => {
                dst[..win_point0].copy_from_slice(&delay[..win_point0]);

                for i in win_point0..frame_len {
                    dst[i] = delay[i] + self.pcm_short[i - win_point0];
                }
            }
            LONG_STOP_SEQUENCE => {
                dst[..win_point0].copy_from_slice(&delay[..win_point0]);

                for i in win_point0..win_point1 {
                    dst[i] = delay[i] + self.pcm_long[i] * prev_short_win[i - win_point0];
                }
                for i in win_point1..frame_len {
                    dst[i] = delay[i] + self.pcm_long[i];
                }
            }
            _ => unreachable!(),
        };

        // Save delay for overlap.
        match seq {
            ONLY_LONG_SEQUENCE | LONG_STOP_SEQUENCE => {
                for i in 0..frame_len {
                    delay[i] = self.pcm_long[i + frame_len] * long_win[frame_len - 1 - i];
                }
            }
            EIGHT_SHORT_SEQUENCE => {
                // Last part is already windowed.
                delay[..win_point1].copy_from_slice(&self.pcm_short[win_point1..2 * win_point1]);

                delay[win_point1..frame_len].fill(0.0);
            }
            LONG_START_SEQUENCE => {
                delay[..win_point0]
                    .copy_from_slice(&self.pcm_long[frame_len..(win_point0 + frame_len)]);

                for i in win_point0..win_point1 {
                    delay[i] =
                        self.pcm_long[i + frame_len] * short_win[short_len - 1 - (i - win_point0)];
                }

                delay[win_point1..frame_len].fill(0.0);
            }
            _ => unreachable!(),
        };
    }
}
