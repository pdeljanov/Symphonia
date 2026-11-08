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

use symphonia_core::errors::Result;
use symphonia_core::io::ReadBitsLtr;

use crate::aac::common::{MAX_SFBS, MAX_WINDOWS};

/// Inverse quantization, sign(x) * |x|^(4/3).
#[inline(always)]
fn iquant(val: f32) -> f32 {
    let tab: &[f32; 8192] = &super::POW43_TABLE;
    let abs_val = val.abs();
    let idx = abs_val as usize;
    let result = if idx < tab.len() { tab[idx] } else { abs_val.powf(4.0 / 3.0) };
    if val < 0.0 { -result } else { result }
}

/// Re-quantization (x^{3/4}) for pulse data synthesis.
///
/// Recovers the integer quantized value from a dequantized coefficient, `val = iquant(q) * scale`
/// (ISO/IEC 14496-3, 4.6.3.3).
#[inline(always)]
fn requant(val: f32, scale: f32) -> f32 {
    if scale == 0.0 {
        return 0.0;
    }
    // Round since the quantized value is an integer and is used to index the x^(4/3) table.
    let result = (val / scale).abs().powf(3.0 / 4.0).round();
    if val < 0.0 { -result } else { result }
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct Pulse {
    number_pulse: usize,
    pulse_start_sfb: usize,
    pulse_offset: [u8; 4],
    pulse_amp: [u8; 4],
}

impl Pulse {
    pub fn read<B: ReadBitsLtr>(bs: &mut B) -> Result<Option<Self>> {
        let pulse_data_present = bs.read_bool()?;

        if !pulse_data_present {
            return Ok(None);
        }

        let number_pulse = bs.read_bits_leq32(2)? as usize + 1;
        let pulse_start_sfb = bs.read_bits_leq32(6)? as usize;

        let mut pulse_offset: [u8; 4] = [0; 4];
        let mut pulse_amp: [u8; 4] = [0; 4];

        for i in 0..number_pulse {
            pulse_offset[i] = bs.read_bits_leq32(5)? as u8;
            pulse_amp[i] = bs.read_bits_leq32(4)? as u8;
        }

        Ok(Some(Self { number_pulse, pulse_start_sfb, pulse_offset, pulse_amp }))
    }

    pub fn synth(
        &self,
        bands: &[usize],
        scales: &[[f32; MAX_SFBS]; MAX_WINDOWS],
        coeffs: &mut [f32; 1024],
    ) {
        if self.pulse_start_sfb >= bands.len() - 1 {
            return;
        }

        let mut k = bands[self.pulse_start_sfb];

        let mut band = self.pulse_start_sfb;

        for pno in 0..self.number_pulse {
            k += self.pulse_offset[pno] as usize;

            if k >= 1024 {
                return;
            }

            while bands[band + 1] <= k {
                band += 1;
            }

            let scale = scales[0][band];

            let mut base = coeffs[k];

            if base != 0.0 {
                base = requant(coeffs[k], scale);
            }

            if base > 0.0 {
                base += f32::from(self.pulse_amp[pno]);
            }
            else {
                base -= f32::from(self.pulse_amp[pno]);
            }
            coeffs[k] = iquant(base) * scale;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aac::common::SWB_OFFSET_48K_LONG;

    #[test]
    fn pulse_synth_adds_amplitude_to_quantized_value() {
        let pulse = Pulse {
            number_pulse: 3,
            pulse_start_sfb: 1,
            pulse_offset: [0, 1, 1, 0],
            pulse_amp: [2, 3, 4, 0],
        };

        let mut scales = [[0.0; MAX_SFBS]; MAX_WINDOWS];
        scales[0][1] = 0.75;

        // Band 1 starts at coefficient 4.
        let mut coeffs = [0.0; 1024];
        coeffs[4] = iquant(3.0) * 0.75;
        coeffs[5] = iquant(-7.0) * 0.75;
        coeffs[6] = 0.0;

        pulse.synth(&SWB_OFFSET_48K_LONG, &scales, &mut coeffs);

        let expected = [iquant(5.0) * 0.75, iquant(-10.0) * 0.75, iquant(-4.0) * 0.75];

        for (actual, expected) in coeffs[4..7].iter().zip(expected) {
            assert!((actual - expected).abs() <= 1e-4 * expected.abs(), "{actual} != {expected}");
        }
    }
}
