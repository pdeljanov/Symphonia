// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Yamaha ADPCM (as used by the Yamaha AICA, YMZ280B, and Y8950 sound chips).

const STEP_SCALE: [i32; 16] =
    [230, 230, 230, 230, 307, 409, 512, 614, 230, 230, 230, 230, 307, 409, 512, 614];

const DIFF: [i32; 16] = [1, 3, 5, 7, 9, 11, 13, 15, -1, -3, -5, -7, -9, -11, -13, -15];

const MIN_STEP: i32 = 127;
const MAX_STEP: i32 = 24576;

/// The state of a channel.
#[derive(Clone)]
pub(crate) struct YamahaChannel {
    predictor: i32,
    step: i32,
}

impl YamahaChannel {
    pub(crate) fn new() -> Self {
        YamahaChannel { predictor: 0, step: MIN_STEP }
    }

    /// Decode a 4-bit code to a 16-bit sample.
    pub(crate) fn decode(&mut self, nibble: u8) -> i32 {
        let nibble = usize::from(nibble & 0xf);

        self.predictor = (self.predictor + self.step * DIFF[nibble] / 8).clamp(-32768, 32767);
        self.step = ((self.step * STEP_SCALE[nibble]) >> 8).clamp(MIN_STEP, MAX_STEP);

        self.predictor
    }
}
