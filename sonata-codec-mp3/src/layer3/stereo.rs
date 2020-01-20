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
use super::Granule;

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
            1.0 / f64::sqrt(f64::consts::SQRT_2),
            f64::consts::FRAC_1_SQRT_2,
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

/// Decorrelates mid and side channels into left and right channels.
///
/// In mid-side (MS) stereo, the left and right channels are encoded as average (mid) and
/// difference (side) components.
///
/// As per ISO/IEC 11172-3, to reconstruct the left and right channels, the following calculation
/// is performed:
///
///      l[i] = (m[i] + s[i]) / sqrt(2)
///      r[i] = (m[i] - s[i]) / sqrt(2)
/// where:
///      l[i], and r[i] are the left and right channels, respectively.
///      m[i], and s[i] are the mid and side channels, respectively.
///
/// In the bitstream, m[i] is transmitted in channel 0, while s[i] in channel 1. After decoding,
/// the left channel replaces m[i] in channel 0, and the right channel replaces s[i] in channel
/// 1.
fn process_mid_side(mid: &mut [f32], side: &mut [f32]) {
    debug_assert!(mid.len() == side.len());

    for (m, s) in mid.iter_mut().zip(side) {
        let left = (*m + *s) * f32::consts::FRAC_1_SQRT_2;
        let right = (*m - *s) * f32::consts::FRAC_1_SQRT_2;
        *m = left;
        *s = right;
    }
}

/// Processes channel 0 of the intensity coded signal into left and right channels for MPEG1
/// bitstreams.
///
/// The intensity position for the scale factor band covering the samples in ch0 and ch1 slices is
/// required.
///
/// As per ISO/IEC 11172-3, the following calculation may be performed to decode the intensity
/// coded signal into left and right channels.
///
///      l[i] = ch0[i] * k_l
///      r[i] = ch0[i] * l_r
///
/// where:
///      l[i], and r[i] are the left and right channels, respectively.
///      ch0[i] is the intensity coded signal store in channel 0.
///      k_l, and k_r are the left and right channel ratios.
fn process_intensity_mpeg1(is_pos: usize, mid_side: bool, ch0: &mut [f32], ch1: &mut [f32]) {
    // For MPEG1 bitstreams, a scalefac can only be up-to 4-bits long. Therefore it is not possible
    // for a bitstream to specify an is_pos > 15. However, assert here to protect against logic
    // errors in the decoder.
    debug_assert!(is_pos <= 15);

    // A position of 7 is considered invalid and should be decoded as mid-side stereo if enabled.
    // Additionally, since a 4-bit scalefac, and thus is_pos, can exceed a value of 7 and index
    // outside the bounds of the intesity stereo ratio table, ignore is_pos values > 7.
    if is_pos < 7 {
        let (ratio_l, ratio_r) = INTENSITY_STEREO_RATIOS[is_pos];

        for (l, r) in ch0.iter_mut().zip(ch1) {
            let is = *l;
            *l = ratio_l * is;
            *r = ratio_r * is;
        }
    }
    else if is_pos == 7 && mid_side {
        process_mid_side(ch0, ch1);
    }
}

/// Processes channel 0 of the intensity coded signal into left and right channels for MPEG2
/// bitstreams.
///
/// The intensity position for the scale factor band covering the samples in ch0 and ch1 slices is
/// required. Additionally, the appropriate intensity stereo ratios table is required, selected
/// based on the least-significant bit of scalefac_compress of channel 1.
///
/// As per ISO/IEC 13818-3, the following calculation may be performed to decode the intensity
/// coded signal into left and right channels.
///
///      l[i] = ch0[i] * k_l
///      r[i] = ch0[i] * l_r
///
/// where:
///      l[i], and r[i] are the left and right channels, respectively.
///      ch0[i] is the intensity coded signal store in channel 0.
///      k_l, and k_r are the left and right channel ratios.
fn process_intensity_mpeg2(
    is_pos_table: &[(f32, f32); 32],
    is_pos: usize,
    mid_side: bool,
    ch0: &mut [f32],
    ch1: &mut [f32],
) {
    // For MPEG2 bitstreams, a scalefac can only be upto 5-bits long, so it's not possible for a
    // bitstream to specify an is_pos > 31. However, assert here to protect against logic errors in
    // the decoder.
    debug_assert!(is_pos <= 31);

    // A position of 7 is considered invalid and should be decoded as mid-side stereo if enabled.
    if is_pos != 7 {
        let (ratio_l, ratio_r) = is_pos_table[is_pos];

        for (l, r) in ch0.iter_mut().zip(ch1) {
            let is = *l;
            *l = ratio_l * is;
            *r = ratio_r * is;
        }
    }
    else if mid_side {
        process_mid_side(ch0, ch1);
    }
}

fn process_intensity_long_block(
    header: &FrameHeader,
    granule: &Granule,
    mid_side: bool,
    ch0: &mut [f32],
    ch1: &mut [f32],
) -> usize {
    // For long blocks of both MPEG1 and MPEG2 bitstreams, the intensity coded band(s) start after
    // the last non-zero band (the band containing channel 1's rzero sample).
    let rzero = granule.channels[1].rzero;

    let bands = &SFB_LONG_BANDS[header.sample_rate_idx];

    let bands_iter = bands.iter()
                          .zip(&bands[1..])
                          .zip(granule.channels[1].scalefacs.iter());

    if header.is_mpeg1() {
        // Every band after the last non-zero band could be encoded with intensity stereo encoding.
        // The intensity position for that band is stored in channel 1's scale-factors. Process
        // these bands one-by-one.
        for((start, end), is_pos) in bands_iter {
            if *start > rzero {
                process_intensity_mpeg1(
                    *is_pos as usize,
                    mid_side,
                    &mut ch0[*start..*end],
                    &mut ch1[*start..*end],
                );
            }
        }

    }
    else {
        // The process for decoding intensity stereo coded bands for MPEG2 bitstreams is the same as
        // MPEG1, except that the ratio table changes based on the least-significant bit of
        // scalefac_compress.
        //
        // Select the table, then process each band one-by-one like MPEG1.
        let is_pos_table =
            &INTENSITY_STEREO_RATIOS_MPEG2[granule.channels[1].scalefac_compress as usize & 0x1];

        for((start, end), is_pos) in bands_iter {
            if *start > rzero {
                process_intensity_mpeg2(
                    is_pos_table,
                    *is_pos as usize,
                    mid_side,
                    &mut ch0[*start..*end],
                    &mut ch1[*start..*end],
                );
            }
        }
    }

    rzero
}

fn process_intensity_short_block_mpeg1(
    header: &FrameHeader,
    granule: &Granule,
    is_mixed: bool,
    mid_side: bool,
    ch0: &mut [f32],
    ch1: &mut [f32],
) -> usize {

    let rzero = granule.channels[1].rzero;

    let bands = if is_mixed {
        SFB_MIXED_BANDS[header.sample_rate_idx]
    }
    else {
        &SFB_SHORT_BANDS[header.sample_rate_idx]
    };

    let bands_iter = bands.iter()
                          .zip(&bands[1..])
                          .zip(granule.channels[1].scalefacs.iter());

    for((start, end), is_pos) in bands_iter {
        if *start > rzero {
            process_intensity_mpeg1(
                *is_pos as usize,
                mid_side,
                &mut ch0[*start..*end],
                &mut ch1[*start..*end],
            );
        }
    }

    rzero
}

fn process_intensity_short_block_mpeg2(
    header: &FrameHeader,
    granule: &Granule,
    is_mixed: bool,
    mid_side: bool,
    ch0: &mut [f32],
    ch1: &mut [f32],
) -> usize {

    let bands = if is_mixed {
        SFB_MIXED_BANDS[header.sample_rate_idx]
    }
    else {
        &SFB_SHORT_BANDS[header.sample_rate_idx]
    };

    let is_pos_table =
        &INTENSITY_STEREO_RATIOS_MPEG2[granule.channels[1].scalefac_compress as usize & 0x1];

    // Build a bitmap where non-zero bands and/or windows are marked with 1.
    let mut zero_bitmap = 0u64;

    for (i, (start, end)) in bands.iter().zip(&bands[1..]).enumerate() {
        // If a band is all 0, record it in the zero bitmap.
        if ch1[*start..*end].iter().find(|&&x| x != 0.0).is_some() {
            zero_bitmap |= 0x1 << i;
        }
    }

    let bands_iter = bands.iter()
                          .zip(&bands[1..])
                          .zip(granule.channels[1].scalefacs.iter());

    // This constant is 0b001 repeating 21 times (63 bits). When masking a value, it retrieves every
    // 3rd bit.
    const WIN_MASK: u64 = 0x4924_9249_2492_4924;

    for ((start, end), is_pos) in bands_iter {
        // This band only contains 0. It *may* be intensity encoded if the remaining bands
        // (or windows, when applicable) are all 0 as well.
        let do_intensity = if zero_bitmap & 1 == 0 {
            // If the block is mixed, and the sample index is < 36, then the current band is a long
            // band. A long band in a mixed block can only be intensity encoded if all the remaining
            // long bands (and their windows in the case of short bands) are zero.
            if is_mixed && (*start < 36) {
                zero_bitmap == 0
            }
            // Otherwise, the band is short (mixed block or otherwise). Short bands are composed of
            // 3 indexable windows. If subsequent windows of the same index in the remaining bands
            // are 0, then this band is intensity encoded.
            else {
                zero_bitmap & WIN_MASK == 0
            }
        }
        else {
            // If the band is non-zero then it is never intensity encoded.
            false
        };

        // If the band/window was processed with intensity encoding, then process it now.
        if do_intensity {
            process_intensity_mpeg2(
                is_pos_table,
                *is_pos as usize,
                mid_side,
                &mut ch0[*start..*end],
                &mut ch1[*start..*end],
            );
        }
        // If intensity encoding is not used, but mid-side coding is enabled, process the
        // band/window as mid-side.
        else if mid_side {
            process_mid_side(&mut ch0[*start..*end], &mut ch1[*start..*end]);
        }

        // Consume the bitmap. Shifting right lets us use the same window mask for all windows in a
        // band.
        zero_bitmap >>= 1;
    }

    // Since short/mixed blocks can be sparse, mid-side processing is done on a band-by-band,
    // window-by-window basis in this case. So the intensity bound is 0.
    0
}

pub(super) fn stereo(
    header: &FrameHeader,
    granule: &mut Granule,
    ch: &mut [[f32; 576]; 2],
) -> Result<()> {

    let (ch0, ch1) = {
        let (ch0, ch1) = ch.split_first_mut().unwrap();
        (ch0, &mut ch1[0])
    };

    let (mid_side, intensity) = match header.channels {
        Channels::JointStereo(Mode::Layer3 { mid_side, intensity }) => (mid_side, intensity),
        Channels::JointStereo(Mode::Intensity { .. }) => {
            // This function only supports decoding Layer 3 stereo encodings, it is a fundamental
            // error in the decoder logic if layer 1 or 2 stereo encodings are being decoded with
            // this function.
            panic!("invalid mode extension for layer 3 stereo decoding")
        },
        _ => return Ok(()),
    };

    // The block types must be the same.
    if granule.channels[0].block_type != granule.channels[1].block_type {
        return decode_error("stereo channel pair block_type mismatch");
    }

    // Joint stereo processing as specified in layer 3 is a combination of mid-side, and intensity
    // encoding schemes. Each scale-factor band may use either mid-side, intensity, or no stereo
    // encoding. The type of encoding used for each scale-factor band is determined by the MPEG
    // version, the mode extension, the block type, and the content of the scale-factor bands.
    //
    // For MPEG1:
    //
    //   If mid-side encoding is enabled in the mode extension, all scale-factor bands upto the band
    //   containing rzero is encoded with mid-side encoding.
    //
    //   If intensity stereo encoding is enabled in the mode extension, all scale-factor bands after
    //   the band containing rzero MAY be encoded with intensity stereo.
    //
    // For MPEG2 and MPEG2.5:
    //
    //   For long blocks, processing is the same as MPEG1.
    //
    //   For short or mixed blocks:
    //
    //     Each scale-factor band is composed of 3 windows (window 0 thru 2). Windows are
    //     interleaved by scale-factor band.
    //
    //     +--------------+--------------+--------------+-------+
    //     |     sfb0     |     sfb1     |     sfb2     |  ...  |
    //     +--------------+--------------+--------------+-------+
    //     | w0 | w1 | w2 | w0 | w1 | w2 | w0 | w1 | w2 |  ...  |
    //     +--------------+--------------+--------------+-------+
    //
    //     Find the FIRST scale-factor band, for each window, where every sample within the window
    //     from there-on is 0. This will yield 3 indicies that are the lower bounds of intensity
    //     stereo processing for each window.
    //
    //     +--------------+--------------+-------+--------------+-------+
    //     |     sfb0     |     sfb1     |  ...  |      sfb9    |  ...  |
    //     +--------------+--------------+-------+--------------+-------+
    //     | xx | 00 | xx | 00 | 00 | xx |  ...  | 00 | 00 | 00 |  ...  |
    //     +--------------+--------------+-------+--------------+-------+
    //
    //     In the above example, window 0 was first all 0 in sfb1. Likewise, window 1 was all 0s in
    //     sfb0, and last, window 2 was all 0 in sfb9. Therefore, the bounds are 1, 0, 9 for windows
    //     0 thru 2, respectively.
    //
    //     If mid-side encoding is enabled in the mode extension, then process all windows up-to
    //     their respective bound with mid-side encoding.
    //
    //     If intensity stereo encoding is enabled in the mode extension, then all windows after
    //     and including their respective bound MAY be encoded with intensity stereo.
    let end = max(granule.channels[0].rzero, granule.channels[1].rzero);

    // Decode intensity stereo coded bands if it is enabled and get the intensity bound.
    let is_bound = if intensity {
        // Process intensity stereo based on bitstream version and block type.
        match granule.channels[1].block_type {
            BlockType::Short { is_mixed } if header.is_mpeg1() => {
                process_intensity_short_block_mpeg1(header, granule, is_mixed, mid_side, ch0, ch1)
            },
            BlockType::Short { is_mixed } if !header.is_mpeg1() => {
                process_intensity_short_block_mpeg2(header, granule, is_mixed, mid_side, ch0, ch1)
            },
            _ => {
                process_intensity_long_block(header, granule, mid_side, ch0, ch1)
            }
        }
    }
    // If intensity stereo coding is not enabled, then the intensity bound is up-to the maximum
    // rzero.
    else {
        end
    };

    // If mid-side stereo is enabled, all samples up to the intensity bound should be processed with
    // mid-side stereo processing. For some complex cases of intensity stereo encoding the intensity
    // bound will be 0 even if mid-side coding is enabled.
    if mid_side && is_bound > 0 {
        process_mid_side(&mut ch0[0..is_bound], &mut ch1[0..is_bound]);
    }

    // With joint stereo processing, there is usually a mismatch between the number of samples read
    // for each channel. The number of non-zero samples read for each channel is the rzero index.
    // However, after joint stereo processing, both channels will have the same number of samples.
    // Update the rzero of both channels with the actual number of samples now.
    if intensity || mid_side {
        granule.channels[0].rzero = end;
        granule.channels[1].rzero = end;
    }

    Ok(())
}