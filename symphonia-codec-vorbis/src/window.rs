// Symphonia
// Copyright (c) 2021 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::f64::consts;

#[derive(Copy, Clone, Debug)]
pub struct WindowHalf {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug)]
pub struct Window {
    pub left: WindowHalf,
    pub right: WindowHalf,
    pub window: Vec<f32>,
}

pub struct WindowBuilder {
    blocksize0: usize,
    blocksize1: usize,
}

impl WindowBuilder {
    fn new(blocksize0: usize, blocksize1: usize) -> Self {
        assert!(blocksize0 <= blocksize1);
        WindowBuilder { blocksize0, blocksize1 }
    }

    fn generate(&self, prev_window_flag: bool, block_flag: bool, next_window_flag: bool) -> Window {
        // A block flag of false (0) is a short block.
        let n = if block_flag { self.blocksize1 } else { self.blocksize0 };

        // Calculate window parameters.
        let window_centre = n / 2;

        let (left, left_n) = if block_flag && !prev_window_flag {
            let start = (n / 4) - (self.blocksize0 / 4);
            let end = (n / 4) + (self.blocksize0 / 4);
            let size = self.blocksize0 / 2;

            (WindowHalf { start, end }, size)
        }
        else {
            let start = 0;
            let end = window_centre;
            let size = n / 2;

            (WindowHalf { start, end }, size)
        };

        let (right, right_n) = if block_flag && !next_window_flag {
            let start = ((n * 3) / 4) - (self.blocksize0 / 4);
            let end = ((n * 3) / 4) + (self.blocksize0 / 4);
            let size = self.blocksize0 / 2;

            (WindowHalf { start, end }, size)
        }
        else {
            let start = window_centre;
            let end = n;
            let size = n / 2;

            (WindowHalf { start, end }, size)
        };

        // Generate the window.
        let mut window = Vec::<f32>::with_capacity(n);

        window.resize(n, 0.0);

        for (i, w) in window[left.start..left.end].iter_mut().enumerate() {
            let a = f64::from(i as u32) + 0.5;
            let b = f64::from(left_n as u32);
            let c = consts::FRAC_PI_2 * (a / b);

            *w = (consts::FRAC_PI_2 * c.sin().powi(2)).sin() as f32
        }

        for w in window[left.end..right.start].iter_mut() {
            *w = 1.0;
        }

        for (i, w) in window[right.start..right.end].iter_mut().enumerate() {
            let a = f64::from(i as u32) + 0.5;
            let b = f64::from(right_n as u32);
            let c = consts::FRAC_PI_2 * (a / b);

            *w = (consts::FRAC_PI_2 * (consts::FRAC_PI_2 + c).sin().powi(2)).sin() as f32
        }

        Window { left, right, window }
    }
}

#[derive(Debug)]
pub struct Windows {
    /// Window for a long block, after a short block, and followed by a short block.
    pub short_long_short: Window,
    /// Window for a long block, after a short block, and followed by a long block.
    pub short_long_long: Window,
    /// Window for a long block, after a long block, and followed by a short block.
    pub long_long_short: Window,
    /// Window for a long block, after a long block, and followed by a long block.
    pub long_long_long: Window,
    /// Window for a short block regardless of what came before it, or comes after.
    pub short: Window,
}

impl Windows {
    pub fn new(blocksize0: usize, blocksize1: usize) -> Self {
        let builder = WindowBuilder::new(blocksize0, blocksize1);

        // Generate windows for all possible block transitions.
        Windows {
            short_long_short: builder.generate(false, true, false),
            short_long_long: builder.generate(false, true, true),
            long_long_short: builder.generate(true, true, false),
            long_long_long: builder.generate(true, true, true),
            short: builder.generate(false, false, false),
        }
    }
}