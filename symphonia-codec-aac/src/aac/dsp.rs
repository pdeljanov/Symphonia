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
use crate::aac::window::*;

const SHORT_WIN_POINT0: usize = 512 - 64;
const SHORT_WIN_POINT1: usize = 512 + 64;

pub struct Dsp {
    kbd_long_win: [f32; 1024],
    kbd_short_win: [f32; 128],
    sine_long_win: [f32; 1024],
    sine_short_win: [f32; 128],
    imdct_long: Imdct,
    imdct_short: Imdct,
    tmp: [f32; 2048],
    ew_buf: [f32; 1152],
}

impl Dsp {
    pub fn new() -> Self {
        let mut kbd_long_win: [f32; 1024] = [0.0; 1024];
        let mut kbd_short_win: [f32; 128] = [0.0; 128];
        generate_window(WindowType::KaiserBessel(4.0), 1.0, 1024, true, &mut kbd_long_win);
        generate_window(WindowType::KaiserBessel(6.0), 1.0, 128, true, &mut kbd_short_win);
        let mut sine_long_win: [f32; 1024] = [0.0; 1024];
        let mut sine_short_win: [f32; 128] = [0.0; 128];
        generate_window(WindowType::Sine, 1.0, 1024, true, &mut sine_long_win);
        generate_window(WindowType::Sine, 1.0, 128, true, &mut sine_short_win);

        Self {
            kbd_long_win,
            kbd_short_win,
            sine_long_win,
            sine_short_win,
            imdct_long: Imdct::new_scaled(1024, 1.0 / 2048.0),
            imdct_short: Imdct::new_scaled(128, 1.0 / 256.0),
            tmp: [0.0; 2048],
            ew_buf: [0.0; 1152],
        }
    }

    #[allow(clippy::cognitive_complexity)]
    pub fn synth(
        &mut self,
        coeffs: &[f32; 1024],
        delay: &mut [f32; 1024],
        seq: u8,
        window_shape: bool,
        prev_window_shape: bool,
        dst: &mut [f32],
    ) {
        let (long_win, short_win) = match window_shape {
            true => (&self.kbd_long_win, &self.kbd_short_win),
            false => (&self.sine_long_win, &self.sine_short_win),
        };

        let (prev_long_win, prev_short_win) = match prev_window_shape {
            true => (&self.kbd_long_win, &self.kbd_short_win),
            false => (&self.sine_long_win, &self.sine_short_win),
        };

        // Zero the output buffer.
        self.tmp = [0.0; 2048];

        // Inverse MDCT
        if seq != EIGHT_SHORT_SEQUENCE {
            self.imdct_long.imdct(coeffs, &mut self.tmp);
        }
        else {
            for (ain, aout) in coeffs.chunks(128).zip(self.tmp.chunks_mut(256)) {
                self.imdct_short.imdct(ain, aout);
            }

            self.ew_buf = [0.0; 1152];

            for (w, src) in self.tmp.chunks(256).enumerate() {
                if w > 0 {
                    for i in 0..128 {
                        self.ew_buf[w * 128 + i + 0] += src[i + 0] * short_win[i];
                        self.ew_buf[w * 128 + i + 128] += src[i + 128] * short_win[127 - i];
                    }
                }
                else {
                    for i in 0..128 {
                        self.ew_buf[i + 0] = src[i + 0] * prev_short_win[i];
                        self.ew_buf[i + 128] = src[i + 128] * short_win[127 - i];
                    }
                }
            }
        }

        // output new data
        match seq {
            ONLY_LONG_SEQUENCE | LONG_START_SEQUENCE => {
                for i in 0..1024 {
                    dst[i] = delay[i] + (self.tmp[i] * prev_long_win[i]);
                }
            }
            EIGHT_SHORT_SEQUENCE => {
                dst[..SHORT_WIN_POINT0].copy_from_slice(&delay[..SHORT_WIN_POINT0]);

                for i in SHORT_WIN_POINT0..1024 {
                    dst[i] = delay[i] + self.ew_buf[i - SHORT_WIN_POINT0];
                }
            }
            LONG_STOP_SEQUENCE => {
                dst[..SHORT_WIN_POINT0].copy_from_slice(&delay[..SHORT_WIN_POINT0]);

                for i in SHORT_WIN_POINT0..SHORT_WIN_POINT1 {
                    dst[i] = delay[i] + self.tmp[i] * prev_short_win[i - SHORT_WIN_POINT0];
                }
                for i in SHORT_WIN_POINT1..1024 {
                    dst[i] = delay[i] + self.tmp[i];
                }
            }
            _ => unreachable!(),
        };

        // save delay
        match seq {
            ONLY_LONG_SEQUENCE | LONG_STOP_SEQUENCE => {
                for i in 0..1024 {
                    delay[i] = self.tmp[i + 1024] * long_win[1023 - i];
                }
            }
            EIGHT_SHORT_SEQUENCE => {
                for i in 0..SHORT_WIN_POINT1 {
                    // last part is already windowed
                    delay[i] = self.ew_buf[i + 512 + 64];
                }
                for i in SHORT_WIN_POINT1..1024 {
                    delay[i] = 0.0;
                }
            }
            LONG_START_SEQUENCE => {
                delay[..SHORT_WIN_POINT0]
                    .copy_from_slice(&self.tmp[1024..(SHORT_WIN_POINT0 + 1024)]);

                for i in SHORT_WIN_POINT0..SHORT_WIN_POINT1 {
                    delay[i] = self.tmp[i + 1024] * short_win[127 - (i - SHORT_WIN_POINT0)];
                }
                for i in SHORT_WIN_POINT1..1024 {
                    delay[i] = 0.0;
                }
            }
            _ => unreachable!(),
        };
    }
}
