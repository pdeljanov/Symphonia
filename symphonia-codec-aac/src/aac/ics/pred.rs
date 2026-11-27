// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The backward-adaptive prediction tool of the AAC Main profile (ISO/IEC 14496-3, 4.6.6).

use symphonia_core::errors::Result;
use symphonia_core::io::ReadBitsLtr;

use crate::aac::common::MAX_SFBS;

/// The number of spectral lines with a predictor.
pub const NUM_PREDICTORS: usize = 672;

/// The coefficient of the predictor's lattice.
const ALPHA: f32 = 0.90625;
/// The attenuation of the predictor's state.
const ATTENUATION: f32 = 0.953125;

/// The scale of the spectral coefficients of the decoder relative to those of the specification,
/// whose output is directly usable as 16-bit PCM.
const SPEC_SCALE: f32 = 1.0 / 16384.0;

/// The prediction side information of an ICS (Table 4.7).
#[derive(Clone, Copy)]
pub struct PredictorData {
    /// The group of predictors to reset after this frame, or 0 if none.
    pub reset_group: u8,
    /// If the prediction is used for each scale factor band.
    pub used: [bool; MAX_SFBS],
}

impl PredictorData {
    /// Read the prediction data of a Main profile ICS. `max_pred_sfb` is the maximum number of
    /// scale factor bands that can use prediction for the sampling frequency.
    pub fn read<B: ReadBitsLtr>(bs: &mut B, max_sfb: usize, max_pred_sfb: usize) -> Result<Self> {
        let mut data = PredictorData { reset_group: 0, used: [false; MAX_SFBS] };

        if bs.read_bool()? {
            data.reset_group = bs.read_bits_leq32(5)? as u8;
        }

        for used in data.used.iter_mut().take(max_sfb.min(max_pred_sfb)) {
            *used = bs.read_bool()?;
        }

        Ok(data)
    }
}

/// The maximum number of scale factor bands with prediction, by sampling frequency index.
const MAX_PRED_SFB: [usize; 12] = [33, 33, 38, 40, 40, 40, 41, 41, 37, 37, 37, 34];

/// Get the maximum number of scale factor bands that may use prediction for the sampling frequency
/// with the index `sample_rate_index`.
pub fn max_pred_sfb(sample_rate_index: usize) -> usize {
    MAX_PRED_SFB.get(sample_rate_index).copied().unwrap_or(0)
}

/// Truncate a value to a 16-bit float: the sign, 8 exponent bits, and 7 mantissa bits.
fn trunc16(value: f32) -> f32 {
    f32::from_bits(value.to_bits() & 0xffff_0000)
}

/// Round a value to a 16-bit float.
fn round16(value: f32) -> f32 {
    f32::from_bits(value.to_bits().wrapping_add(0x8000) & 0xffff_0000)
}

/// Round a value to a 16-bit float, rounding halves to even.
fn round16_even(value: f32) -> f32 {
    let bits = value.to_bits();
    f32::from_bits(bits.wrapping_add(0x7fff + ((bits >> 16) & 1)) & 0xffff_0000)
}

/// The state of the second-order backward-adaptive lattice predictor of a spectral line.
#[derive(Clone, Copy, Default)]
struct Predictor {
    r0: f32,
    r1: f32,
    cor0: f32,
    cor1: f32,
    var0: f32,
    var1: f32,
}

impl Predictor {
    fn new() -> Self {
        Predictor { var0: 1.0, var1: 1.0, ..Default::default() }
    }

    /// Predict the spectral value `coeff`, add the prediction to it if `enabled`, and update the
    /// predictor with the reconstructed value.
    fn process(&mut self, coeff: &mut f32, enabled: bool) {
        let k1 =
            if self.var0 > 1.0 { self.cor0 * round16_even(ATTENUATION / self.var0) } else { 0.0 };
        let k2 =
            if self.var1 > 1.0 { self.cor1 * round16_even(ATTENUATION / self.var1) } else { 0.0 };

        let prediction = round16(k1 * self.r0 + k2 * self.r1);

        if enabled {
            *coeff += prediction;
        }

        let e0 = *coeff;
        let e1 = e0 - k1 * self.r0;

        self.cor1 = trunc16(ALPHA * self.cor1 + self.r1 * e1);
        self.var1 = trunc16(ALPHA * self.var1 + 0.5 * (self.r1 * self.r1 + e1 * e1));
        self.cor0 = trunc16(ALPHA * self.cor0 + self.r0 * e0);
        self.var0 = trunc16(ALPHA * self.var0 + 0.5 * (self.r0 * self.r0 + e0 * e0));

        self.r1 = trunc16(ATTENUATION * (self.r0 - k1 * e0));
        self.r0 = trunc16(ATTENUATION * e0);
    }
}

/// The prediction state of a channel.
#[derive(Clone)]
pub struct PredictorState {
    predictors: Box<[Predictor; NUM_PREDICTORS]>,
}

impl PredictorState {
    pub fn new() -> Self {
        PredictorState { predictors: Box::new([Predictor::new(); NUM_PREDICTORS]) }
    }

    pub fn reset(&mut self) {
        self.predictors.fill(Predictor::new());
    }

    /// Reset the predictors of the group `group` (1 to 30).
    fn reset_group(&mut self, group: u8) {
        if group == 0 {
            return;
        }

        for predictor in self.predictors.iter_mut().skip(usize::from(group) - 1).step_by(30) {
            *predictor = Predictor::new();
        }
    }

    /// Predict the spectrum of a long window, whose scale factor bands are bounded by `bands`.
    pub fn apply_long(
        &mut self,
        coeffs: &mut [f32; 1024],
        bands: &[usize],
        max_pred_sfb: usize,
        data: Option<&PredictorData>,
    ) {
        for sfb in 0..max_pred_sfb.min(bands.len().saturating_sub(1)) {
            let enabled = data.is_some_and(|data| data.used[sfb]);

            for k in bands[sfb]..bands[sfb + 1].min(NUM_PREDICTORS) {
                let mut spec = coeffs[k] * (1.0 / SPEC_SCALE);
                self.predictors[k].process(&mut spec, enabled);
                coeffs[k] = spec * SPEC_SCALE;
            }
        }

        if let Some(data) = data {
            self.reset_group(data.reset_group);
        }
    }
}
