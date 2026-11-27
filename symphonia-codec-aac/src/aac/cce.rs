// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Coupling channel elements (ISO/IEC 14496-3, 4.6.8).

use symphonia_core::errors::Result;
use symphonia_core::io::ReadBitsLtr;
use symphonia_core::io::vlc::{Codebook, Entry8x16};

use symphonia_common::mpeg::audio::AudioObjectType;

use crate::aac::codebooks;
use crate::aac::common::*;
use crate::aac::ics::Ics;

/// The base of the gain of a gain element for `gain_element_scale` 0 to 3: 2^(1/8), 2^(1/4),
/// 2^(1/2), and 2.
const GAIN_ELEMENT_SCALE: [f32; 4] = [1.090_507_7, 1.189_207_1, std::f32::consts::SQRT_2, 2.0];

/// The channel of a single channel or channel pair element that a gain element list applies to.
#[derive(Clone, Copy)]
pub struct CouplingTarget {
    pub is_cpe: bool,
    pub tag: u32,
    /// The channel of the target element, or `None` if the list applies to no channel.
    pub channel: Option<usize>,
}

/// The side information of a coupling channel element. The coupling channel itself is decoded to
/// the `Ics` that is passed to [`Cce::read`].
pub struct Cce {
    pub tag: usize,
    /// If the coupling channel is independently switched, it is added to its targets in the time
    /// domain with a single gain per target.
    pub ind_sw: bool,
    /// For dependent coupling, if the coupling channel is added after TNS rather than before.
    pub after_tns: bool,
    pub targets: Vec<CouplingTarget>,
    /// The gain of each target: a single value for independent coupling, a value for each
    /// coefficient for dependent coupling.
    pub gains: Vec<Vec<f32>>,
}

/// The gain of a gain element with the value `value` (4.6.8.3.3).
fn gain_value(value: i32, sign: bool, scale: f32) -> f32 {
    if sign {
        let negative = value & 1 == 1;
        let exponent = -(value >> 1);
        let magnitude = scale.powi(exponent);
        if negative { -magnitude } else { magnitude }
    }
    else {
        scale.powi(-value)
    }
}

impl Cce {
    /// Read a coupling channel element after its element_instance_tag.
    pub fn read<B: ReadBitsLtr>(
        bs: &mut B,
        ics: &mut Ics,
        lcg: &mut Lcg,
        aot: AudioObjectType,
        tag: usize,
    ) -> Result<Self> {
        let ind_sw = bs.read_bool()?;
        let num_coupled = bs.read_bits_leq32(3)? as usize + 1;

        let mut targets = Vec::with_capacity(num_coupled + 1);

        for _ in 0..num_coupled {
            let is_cpe = bs.read_bool()?;
            let target_tag = bs.read_bits_leq32(4)?;

            if is_cpe {
                let left = bs.read_bool()?;
                let right = bs.read_bool()?;

                if left {
                    targets.push(CouplingTarget { is_cpe, tag: target_tag, channel: Some(0) });
                }
                if right {
                    targets.push(CouplingTarget { is_cpe, tag: target_tag, channel: Some(1) });
                }
                if !left && !right {
                    targets.push(CouplingTarget { is_cpe, tag: target_tag, channel: None });
                }
            }
            else {
                targets.push(CouplingTarget { is_cpe, tag: target_tag, channel: Some(0) });
            }
        }

        let after_tns = bs.read_bool()?;
        let sign = bs.read_bool()?;
        let scale = GAIN_ELEMENT_SCALE[bs.read_bits_leq32(2)? as usize];

        ics.decode(bs, lcg, aot, false)?;

        let frame_len = ics.frame_len();
        let scf_cb: &Codebook<Entry8x16> = &codebooks::SCALEFACTORS;

        let mut gains = Vec::with_capacity(targets.len());
        gains.push(if ind_sw { vec![1.0] } else { vec![1.0; frame_len] });

        for _ in 1..targets.len() {
            let common_gain = if ind_sw { true } else { bs.read_bool()? };

            if common_gain {
                let value = i32::from(bs.read_codebook(scf_cb)?.0) - 60;
                // Empirically, the sign applies only to the gains of individual bands.
                let gain = gain_value(value, false, scale);
                gains.push(if ind_sw { vec![gain] } else { vec![gain; frame_len] });
                continue;
            }

            let mut list = vec![0.0; frame_len];
            let mut value = 0;
            let bands = ics.get_bands();
            let short_len = ics.win_len();

            for g in 0..ics.info.window_groups {
                let first_window = ics.info.get_group_start(g);
                let end_window = ics.info.get_group_start(g + 1);

                for sfb in 0..ics.info.max_sfb {
                    if ics.is_zero(g, sfb) {
                        continue;
                    }

                    value += i32::from(bs.read_codebook(scf_cb)?.0) - 60;
                    let gain = gain_value(value, sign, scale);

                    for w in first_window..end_window {
                        list[w * short_len + bands[sfb]..w * short_len + bands[sfb + 1]].fill(gain);
                    }
                }
            }

            gains.push(list);
        }

        Ok(Cce { tag, ind_sw, after_tns, targets, gains })
    }
}
