// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::Result;

use crate::bits::Bits;

const E_RATIO_QUANT: [f32; 4] = [0.25, 0.315, 0.397, 0.5];

/// The intensity stereo state, which expands the decoded mono signal to two channels.
pub(crate) struct Stereo {
    balance: f32,
    e_ratio: f32,
    smooth_left: f32,
    smooth_right: f32,
}

impl Stereo {
    pub(crate) fn new() -> Self {
        Stereo { balance: 1.0, e_ratio: 0.5, smooth_left: 1.0, smooth_right: 1.0 }
    }

    /// Reads the in-band request that carries the balance and the energy ratio.
    pub(crate) fn request(&mut self, bits: &mut Bits<'_>) -> Result<()> {
        let sign = if bits.read(1)? != 0 { -1.0 } else { 1.0 };
        let dexp = bits.read(5)?;
        self.balance = (sign * 0.25 * dexp as f32).exp();
        self.e_ratio = E_RATIO_QUANT[bits.read(2)? as usize];
        Ok(())
    }

    /// Expands `mono` into the interleaved `stereo`, which has twice as many samples.
    pub(crate) fn expand(&mut self, mono: &[f32], stereo: &mut [f32]) {
        let e_right = 1.0 / (self.e_ratio * (1.0 + self.balance)).sqrt();
        let e_left = self.balance.sqrt() * e_right;

        for i in (0..mono.len()).rev() {
            self.smooth_left = 0.98 * self.smooth_left + 0.02 * e_left;
            self.smooth_right = 0.98 * self.smooth_right + 0.02 * e_right;
            stereo[2 * i] = self.smooth_left * mono[i];
            stereo[2 * i + 1] = self.smooth_right * mono[i];
        }
    }
}
