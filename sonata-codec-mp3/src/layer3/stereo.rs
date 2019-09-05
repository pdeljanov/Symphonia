// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::cmp::max;
use std::{f64, f32};

use sonata_core::errors::{Result, decode_error};

use lazy_static::lazy_static;

use crate::common::*;
use super::{Granule, GranuleChannel};

lazy_static! {
    /// (Left, right) channel coefficients for decoding intensity stereo in MPEG2 bitstreams.
    ///
    /// These coefficients are derived from section 2.4.3.2 of ISO/IEC 13818-3.
    ///
    /// As per the specification, for a given intensity position, is_pos (0 <= is_pos < 32), the
    /// channel coefficients, k_l and k_r, may be calculated as per the table below:
    ///
    /// ```text
    /// If...            | k_l                     | k_r
    /// -----------------+-------------------------+-------------------
    /// is_pos     == 0  | 1.0                     | 1.0
    /// is_pos & 1 == 1  | i0 ^ [(is_pos + 1) / 2] | 1.0
    /// is_pos & 1 == 0  | 1.0                     | i0 ^ (is_pos / 2)
    /// ```
    ///
    /// The value of i0 is dependant on the least significant bit of scalefac_compress.
    ///
    ///  ```text
    /// scalefac_compress & 1 | i0
    /// ----------------------+---------------------
    /// 0                     | 1 / sqrt(sqrt(2.0))
    /// 1                     | 1 / sqrt(2.0)
    /// ```
    ///
    /// The first dimension of this table is indexed by scalefac_compress & 1 to select i0. The
    /// second dimension is indexed by is_pos to obtain the channel coefficients. Note that
    /// is_pos == 7 is considered an invalid position, but IS included in the table.
    static ref INTENSITY_STEREO_RATIOS_MPEG2: [[(f32, f32); 32]; 2] = {
        let is_scale: [f64; 2] = [
            1.0 / f64::sqrt(f64::sqrt(2.0)),
            1.0 / f64::sqrt(2.0),
        ];

        let mut ratios = [[(0.0, 0.0); 32]; 2];

        for (i, is_pos) in (0..32).enumerate() {
            if is_pos & 1 != 0 {
                ratios[0][i] = (f64::powi(is_scale[0], (is_pos + 1) >> 1) as f32, 1.0);
                ratios[1][i] = (f64::powi(is_scale[1], (is_pos + 1) >> 1) as f32, 1.0);
            }
            else {
                ratios[0][i] = (1.0, f64::powi(is_scale[0], is_pos >> 1) as f32);
                ratios[1][i] = (1.0, f64::powi(is_scale[1], is_pos >> 1) as f32);
            }
        }

        ratios
    };
}

lazy_static! {
    /// (Left, right) channel coeffcients for decoding intensity stereo in MPEG1 bitstreams.
    ///
    /// These coefficients are derived from section 2.4.3.4.9.3 of ISO/IEC 11172-3.
    ///
    /// As per the specification, for a given intensity position, is_pos (0 <= is_pos < 7), a ratio,
    /// is_ratio, is calculated as follows:
    ///
    /// ```text
    /// is_ratio = tan(is_pos * PI/12)
    /// ```
    ///
    /// Then, the channel coefficients, k_l and k_r, are calculated as follows:
    ///
    /// ```text
    /// k_l = is_ratio / (1 + is_ratio)
    /// k_r =        1 / (1 + is_ratio)
    /// ```
    ///
    /// This table is indexed by is_pos. Note that is_pos == 7 is invalid and is NOT included in the
    /// table.
    static ref INTENSITY_STEREO_RATIOS: [(f32, f32); 7] = {
        const PI_12: f64 = f64::consts::PI / 12.0;

        let mut ratios = [(0.0, 0.0); 7];

        for is_pos in 0..6 {
            let ratio = (PI_12 * is_pos as f64).tan();
            ratios[is_pos] = ((ratio / (1.0 + ratio)) as f32, 1.0 / (1.0 + ratio) as f32);
        }

        ratios[6] = (1.0, 0.0);

        ratios
    };
}

pub(super) fn stereo(
    header: &FrameHeader,
    granule: &Granule,
    ch: &mut [[f32; 576]; 2],
) -> Result<()> {

    let (ch0, ch1) = {
        let (ch0, ch1) = ch.split_first_mut().unwrap();
        (ch0, &mut ch1[0])
    };

    let (mid_side, intensity) = match header.channels {
        Channels::JointStereo(Mode::Layer3 { mid_side, intensity }) => (mid_side, intensity),
        Channels::JointStereo(Mode::Intensity { .. })               => (false, true),
        _ => (false, false),
    };

    // If mid-side (MS) stereo is used, then the left and right channels are encoded as an average
    // (mid) and difference (side) components.
    //
    // As per ISO/IEC 11172-3, to reconstruct the left and right channels, the following calculation
    // is performed:
    //
    //      l[i] = (m[i] + s[i]) / sqrt(2)
    //      r[i] = (m[i] - s[i]) / sqrt(2)
    // where:
    //      l[i], and r[i] are the left and right channels, respectively.
    //      m[i], and s[i] are the mid and side channels, respectively.
    //
    // In the bitstream, m[i] is transmitted in channel 0, while s[i] in channel 1. After decoding,
    // the left channel replaces m[i] in channel 0, and the right channel replaces s[i] in channel
    // 1.
    if mid_side {
        let end = max(granule.channels[0].rzero, granule.channels[1].rzero);

        for i in 0..end {
            let left = (ch0[i] + ch1[i]) * f32::consts::FRAC_1_SQRT_2;
            let right = (ch0[i] - ch1[i]) * f32::consts::FRAC_1_SQRT_2;
            ch0[i] = left;
            ch1[i] = right;
        }
    }

    // If intensity stereo is used, then samples within the rzero partition are coded using
    // intensity stereo. Intensity stereo codes both channels (left and right) into channel 0.
    // In channel 1, the scale factors, for the scale factor bands within the rzero partition
    // corresponding to the intensity coded bands of channel 0, contain the intensity position.
    // Using the intensity position for each band, the intensity signal may be decoded into left
    // and right channels.
    //
    // As per ISO/IEC 11172-3 and ISO/IEC 13818-3, the following calculation may be performed to
    // decode the intensity coded signal into left and right channels.
    //
    //      l[i] = ch0[i] * k_l
    //      r[i] = ch0[i] * l_r
    // where:
    //      l[i], and r[i] are the left and right channels, respectively.
    //      ch0[i] is the intensity coded signal store in channel 0.
    //      k_l, and k_r are the left and right channel ratios.
    //
    // The channel ratios are dependant on MPEG version. For MPEG1:
    //
    //      r = tan(pos[sfb] * PI/12
    //      k_l = r / (1 + r)
    //      k_r = 1 / (1 + r)
    // where:
    //      pos[sfb] is the position for the scale factor band.
    //
    //  For MPEG2:
    //
    //  If...              | k_l                       | k_r
    //  -------------------+---------------------------+---------------------
    //  pos[sfb]     == 0  | 1.0                       | 1.0
    //  pos[sfb] & 1 == 1  | i0 ^ [(pos[sfb] + 1) / 2] | 1.0
    //  pos[sfb] & 1 == 0  | 1.0                       | i0 ^ (pos[sfb] / 2)
    //
    // where:
    //      pos[sfb] is the position for the scale factor band.
    //      i0 = 1 / sqrt(2)        if (intensity_scale = scalefac_compress & 1) == 1
    //      i0 = 1 / sqrt(sqrt(2))  if (intensity_scale = scalefac_compress & 1) == 0
    //
    // Note: regardless of version, pos[sfb] == 7 is forbidden and indicates intensity stereo
    //       decoding should not be used.
    if intensity {
        // The block types must be the same.
        if granule.channels[0].block_type != granule.channels[1].block_type {
            return decode_error("stereo channel pair block_type mismatch");
        }

        let ch1_rzero = granule.channels[1].rzero;

        // Determine which bands are entirely contained within the rzero partition. Intensity stereo
        // is applied to these bands only.
        match granule.channels[1].block_type {
            // For short blocks, every scale factor band is repeated thrice (for the three windows).
            // Multiply each band start index by 3 before checking if it is above or below the rzero
            // partition.
            BlockType::Short { is_mixed: false } => {
                let short_indicies = &SCALE_FACTOR_SHORT_BANDS[header.sample_rate_idx];

                let short_band = short_indicies[..13].iter()
                                                     .map(|i| 3 * i)
                                                     .position(|i| i >= ch1_rzero);

                if let Some(start) = short_band {
                    intensity_stereo_short(header, &granule.channels[1], start, ch0, ch1);
                }
            },
            // For mixed blocks, the first 36 samples are part of a long block, and the remaining
            // samples are part of short blocks.
            BlockType::Short { is_mixed: true } => {
                let long_indicies = &SCALE_FACTOR_LONG_BANDS[header.sample_rate_idx];

                // Check is rzero begins in the long block.
                let long_band = long_indicies[..8].iter().position(|i| *i >= ch1_rzero);

                // If rzero begins in the long block, then all short blocks are also part of rzero.
                if let Some(start) = long_band {
                    intensity_stereo_long(header, &granule.channels[1], start, 8, ch0, ch1);
                    intensity_stereo_short(header, &granule.channels[1], 3, ch0, ch1);
                }
                // Otherwise, find where rzero begins in the short blocks.
                else {
                    let short_indicies = &SCALE_FACTOR_SHORT_BANDS[header.sample_rate_idx];

                    let short_band = short_indicies[3..13].iter()
                                                          .map(|i| 3 * i)
                                                          .position(|i| i >= ch1_rzero);

                    if let Some(start) = short_band {
                        intensity_stereo_short(header, &granule.channels[1], start, ch0, ch1);
                    }
                };
            },
            // For long blocks, simply find the first scale factor band that is fully in the rzero
            // partition.
            _ => {
                let long_indicies = &SCALE_FACTOR_LONG_BANDS[header.sample_rate_idx];

                let long_band = long_indicies[..22].iter().position(|i| *i >= ch1_rzero);

                if let Some(start) = long_band {
                    intensity_stereo_long(header, &granule.channels[1], start, 22, ch0, ch1);
                }
            },
        }
    }

    Ok(())
}

fn intensity_stereo_short(
    header: &FrameHeader,
    channel: &GranuleChannel,
    sfb_start: usize,
    ch0: &mut [f32; 576],
    ch1: &mut [f32; 576],
) {
    let sfb_indicies = &SCALE_FACTOR_SHORT_BANDS[header.sample_rate_idx];

    // If MPEG1...
    if header.is_mpeg1() {
        for sfb in sfb_start..13 {
            let win_len = sfb_indicies[sfb+1] - sfb_indicies[sfb];

            let mut start = 3 * sfb_indicies[sfb];

            for win in 0..3 {
                let is_pos = channel.scalefacs[3*sfb + win] as usize;

                if is_pos < 7 {
                    let (ratio_l, ratio_r) = INTENSITY_STEREO_RATIOS[is_pos];

                    // Process each sample within the scale factor band.
                    for i in start..(start + win_len) {
                        let is = ch0[i];
                        ch0[i] = ratio_l * is;
                        ch1[i] = ratio_r * is;
                    }
                }

                start += win_len;
            }
        }
    }
    // Otherwise, if MPEG2 or 2.5...
    else {
        let is_pos_table = &INTENSITY_STEREO_RATIOS_MPEG2[channel.scalefac_compress as usize & 0x1];

        for sfb in sfb_start..13 {
            let win_len = sfb_indicies[sfb+1] - sfb_indicies[sfb];

            let mut start = 3 * sfb_indicies[sfb];

            for win in 0..3 {
                let is_pos = channel.scalefacs[3*sfb + win] as usize;

                if is_pos != 7 {
                    let (ratio_l, ratio_r) = is_pos_table[is_pos];

                    // Process each sample within the scale factor band.
                    for i in start..(start + win_len) {
                        let is = ch0[i];
                        ch0[i] = ratio_l * is;
                        ch1[i] = ratio_r * is;
                    }
                }

                start += win_len;
            }
        }
    }
}

fn intensity_stereo_long(
    header: &FrameHeader,
    channel: &GranuleChannel,
    sfb_start: usize,
    sfb_end: usize,
    ch0: &mut [f32; 576],
    ch1: &mut [f32; 576],
) {
    let sfb_indicies = &SCALE_FACTOR_LONG_BANDS[header.sample_rate_idx];

    // If MPEG1...
    if header.is_mpeg1() {
        for sfb in sfb_start..sfb_end {
            let is_pos = channel.scalefacs[sfb] as usize;

            // A position of 7 is considered invalid. Additionally, for MPEG1 bitstreams, a scalefac
            // may be up to 4-bits long. A 4 bit scalefac is clearly invalid for intensity coded
            // scale factor bands since the maximum value is 7, but a maliciously crafted file could
            // conceivably make it happen. Therefore, any position > 7 is ignored, thus protecting
            // the table look-up from going out-of-bounds.
            if is_pos < 7 {
                let (ratio_l, ratio_r) = INTENSITY_STEREO_RATIOS[is_pos];

                // Process each sample within the scale factor band.
                let start = sfb_indicies[sfb];
                let end = sfb_indicies[sfb+1];

                for i in start..end {
                    let is = ch0[i];
                    ch0[i] = ratio_l * is;
                    ch1[i] = ratio_r * is;
                }
            }
        }
    }
    // Otherwise, if MPEG2 or 2.5...
    else {
        let is_pos_table = &INTENSITY_STEREO_RATIOS_MPEG2[channel.scalefac_compress as usize & 0x1];

        for sfb in sfb_start..sfb_end {
            let is_pos = channel.scalefacs[sfb] as usize;

            // A position of 7 is considered invalid.
            if is_pos != 7 {
                // For MPEG2 bitstreams, a scalefac can be up to 5-bits long and may index the
                // intensity stereo coefficients table directly.
                let (ratio_l, ratio_r) = is_pos_table[is_pos];

                // Process each sample within the scale factor band.
                let start = sfb_indicies[sfb];
                let end = sfb_indicies[sfb+1];

                for i in start..end {
                    let is = ch0[i];
                    ch0[i] = ratio_l * is;
                    ch1[i] = ratio_r * is;
                }
            }
        }
    }
}
