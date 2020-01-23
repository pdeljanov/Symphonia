// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::cmp::max;
use std::{f64, f32};

use sonata_core::errors::{Result, decode_error};
use sonata_core::util::bits;

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

/// Decodes channel 0 of the intensity stereo coded signal into left and right channels for MPEG1
/// bitstreams.
///
/// The intensity position for the scale factor band covering the samples in ch0 and ch1 slices is a
/// required argument.
///
/// As per ISO/IEC 11172-3, the following calculation may be performed to decode the intensity
/// stereo coded signal into left and right channels.
///
///      l[i] = ch0[i] * k_l
///      r[i] = ch0[i] * l_r
///
/// where:
///      l[i], and r[i] are the left and right channels, respectively.
///      ch0[i] is the intensity stereo coded signal found in channel 0.
///      k_l, and k_r are the left and right channel ratios, respectively.
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

/// Decodes channel 0 of the intensity stereo coded signal into left and right channels for MPEG2
/// and MPEG2.5 bitstreams.
///
/// The intensity position for the scale factor band covering the samples in ch0 and ch1 slices is
/// required. Additionally, the appropriate intensity stereo ratios table is required, selected
/// based on the least-significant bit of scalefac_compress of channel 1.
///
/// As per ISO/IEC 13818-3, the following calculation may be performed to decode the intensity
/// stereo coded signal into left and right channels.
///
///      l[i] = ch0[i] * k_l
///      r[i] = ch0[i] * l_r
///
/// where:
///      l[i], and r[i] are the left and right channels, respectively.
///      ch0[i] is the intensity stereo coded signal found in channel 0.
///      k_l, and k_r are the left and right channel ratios, respectively.
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

/// Decodes all intensity stereo coded bands within an entire long block for MPEG1, MPEG2, and
/// MPEG2.5 bitstreams, and returns the intensity bound.
fn process_intensity_long_block(
    header: &FrameHeader,
    granule: &Granule,
    mid_side: bool,
    ch0: &mut [f32; 576],
    ch1: &mut [f32; 576],
) -> usize {
    // As per ISO/IEC 11172-3 and ISO/IEC 13818-3, for long blocks that have intensity stereo
    // coding enabled, all bands starting after the last non-zero band in channel 1 may be
    // intensity stereo coded.
    //
    // The scale-factors in channel 1 for those respective bands determine the intensity position.

    // The rzero sample index is the index of last non-zero sample plus 1. If a band's start index
    // is >= the rzero sample index then that band could be intensity stereo coded.
    let rzero = granule.channels[1].rzero;

    let bands = &SFB_LONG_BANDS[header.sample_rate_idx];

    // Create an iterator that yields a band start-end pair, and scale-factor.
    let bands_iter = bands.iter()
                          .zip(&bands[1..])
                          .zip(granule.channels[1].scalefacs.iter());

    // Decode intensity stereo coded bands based on bitstream version.
    if header.is_mpeg1() {
        // Iterate over each band and decode the intensity stereo coding if the band is zero.
        for ((start, end), is_pos) in bands_iter {
            if *start >= rzero {
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
        // MPEG1, except that the position ratio table changes based on the least-significant bit of
        // scalefac_compress.
        //
        // Select the ratio table, then process each band one-by-one just like MPEG1.
        let is_pos_table =
            &INTENSITY_STEREO_RATIOS_MPEG2[granule.channels[1].scalefac_compress as usize & 0x1];

        for ((start, end), is_pos) in bands_iter {
            if *start >= rzero {
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

    // The intensity bound (where intensity stereo coding begins) is rzero sample index. Note that
    // the actual bound should be rounded up to the next band, but the sample between rzero and the
    // next band are 0, therefore it's okay to ignore them.
    rzero
}

/// Decodes all intensity stereo coded bands within an entire short block for MPEG1 bitstreams, and
/// returns the intensity bound.
fn process_intensity_short_block_mpeg1(
    header: &FrameHeader,
    granule: &Granule,
    is_mixed: bool,
    mid_side: bool,
    ch0: &mut [f32; 576],
    ch1: &mut [f32; 576],
) -> usize {
    // As per ISO/IEC 11172-3, for short blocks that have intensity stereo coding enabled, all bands
    // starting after the last non-zero band in channel 1 may be intensity stereo coded.
    //
    // The scale-factors in channel 1 for those respective bands determine the intensity position.
    let rzero = granule.channels[1].rzero;

    // If the short block is mixed, then use the mixed bands table, otherwise use the short band
    // table.
    let bands = if is_mixed {
        SFB_MIXED_BANDS[header.sample_rate_idx]
    }
    else {
        &SFB_SHORT_BANDS[header.sample_rate_idx]
    };

    // Create an iterator that yields a band start-end pair, and scale-factor.
    let bands_iter = bands.iter()
                          .zip(&bands[1..])
                          .zip(granule.channels[1].scalefacs.iter());

    // Iterate over each band and decode the intensity stereo coding if the band is zero.
    for ((start, end), is_pos) in bands_iter {
        // TODO: If one window is non-zero in the band, should the entire band be considered
        // non-zero? Or could the next window(s) be intensity stereo coded? This check only supports
        // the latter case.
        if *start >= rzero {
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

/// Decodes all intensity stereo coded bands within an entire short block for MPEG2 and MPEG2.5
/// bitstreams, and returns the intensity bound.
fn process_intensity_short_block_mpeg2(
    header: &FrameHeader,
    granule: &Granule,
    is_mixed: bool,
    mid_side: bool,
    ch0: &mut [f32; 576],
    ch1: &mut [f32; 576],
) -> usize {
    // Intensity stereo coding for short blocks in ISO/IEC 13818-3 is significantly more complex
    // than any other bitstream version.
    //
    // For short, non-mixed, blocks, each band is composed of 3 windows (windows 0 thru 2). Windows
    // are interleaved in each band.
    //
    // +--------------+--------------+--------------+-------+
    // |     sfb0     |     sfb1     |     sfb2     |  ...  |
    // +--------------+--------------+--------------+-------+
    // | w0 | w1 | w2 | w0 | w1 | w2 | w0 | w1 | w2 |  ...  |
    // +--------------+--------------+--------------+-------+
    //
    // However, each window of the same index is logically contiguous as depicted below.
    //
    // +------+------+------+------+
    // | sfb0 | sfb1 | sfb2 | .... |
    // +------+------+------+------+
    // |  w0  |  w0  |  w0  | .... |
    // +-------------+------+------+
    // |  w1  |  w1  |  w1  | .... |
    // +-------------+------+------+
    // |  w2  |  w2  |  w2  | .... |
    // +------+------+------+------+
    //
    // Unlike ISO/IEC 11172-3, where the intensity bound is a band boundary, each logically
    // contiguous window may have it's own intensity bound. For example, in the example below, the
    // intensity bound for window 0 is sfb0, for window 1 it's sfb2, and for window 2 it's sfb1.
    //
    //      +------+------+------+------+
    //      | sfb0 | sfb1 | sfb2 | .... |
    //      +------+------+------+------+
    //  w0  | 0000 | 0000 | 0000 | 0... |
    //      +-------------+------+------+
    //  w1  | abcd | xyzw | 0000 | 0... |
    //      +-------------+------+------+
    //  w2  | xyz0 | 0000 | 0000 | 0... |
    //      +------+------+------+------+
    //
    // For short blocks that are mixed, the long bands at the start follow the same rules as long
    // blocks (see above). For example, for the block below, if sfb1 is the intensity bound, then
    // all samples from sfb1 onwards must be zero. If the intensity bound is not within the long
    // bands then the rules stated above are followed whereby each window has it's own intensity
    // bound.
    //
    // |> Long bands        |> Short bands (3 windows)
    // +------+------+------+--------+--------+------+
    // | sfb0 | sfb1 | .... | sfbN-2 | sfbN-1 | sfbN |
    // |------+------+------+--------+--------+------+
    // |      |      |      |   w0   |   w0   |  w0  |
    // |      |      |      +--------+--------+------+
    // |      |      | .... |   w1   |   w1   |  w1  |
    // |      |      |      +--------+--------+------+
    // |      |      |      |   w2   |   w2   |  w2  |
    // +------+------+------+--------+--------+------+
    //
    // Regardless of the intensity bound, if a long band or short window is intensity stereo coded,
    // then it is decoded as per usual. If the long band or short window is not intensity stereo
    // coded, and mid-side stereo coding is enabled, then it should be mid-side stereo decoded.

    // This constant is the bit pattern 0b001 repeated 21 times over (63 bits total). When used as a
    // mask value, it retrieves every 3rd bit start from the least-significant bit.
    const WIN_MASK: u64 = 0x4924_9249_2492_4924;

    // If the short block is mixed, then use the mixed bands table, otherwise use the short band
    // table.
    let bands = if is_mixed {
        SFB_MIXED_BANDS[header.sample_rate_idx]
    }
    else {
        &SFB_SHORT_BANDS[header.sample_rate_idx]
    };

    // Retrieve the intensity stereo ratios table.
    let is_pos_table =
        &INTENSITY_STEREO_RATIOS_MPEG2[granule.channels[1].scalefac_compress as usize & 0x1];

    let rzero = max(granule.channels[0].rzero, granule.channels[1].rzero);

    // Build a bitmap where non-zero bands and/or windows are marked with a 1.
    let mut nz_map = 0u64;

    for (i, (start, end)) in bands.iter().zip(&bands[1..]).enumerate() {
        // Bands or windows starting at or beyond rzero are all 0 so there is no need to process
        // them.
        if *start >= rzero {
            break;
        }

        // Check if a band is non-zero, and if so, record it in the non-zero band map.
        if ch1[*start..*end].iter().find(|&&x| x != 0.0).is_some() {
            nz_map |= 0x1 << i;
        }
    }

    // If there is a set of contiguous non-zero bands and/or windows, these will never be intensity
    // stereo coded. They may be mid-side stereo encoded, however. Therefore, a conservative
    // estimate of the intensity bound is the end of the last non-zero band in this set.
    let is_bound = bits::trailing_ones_u64(nz_map) as usize;

    // Consume all bands before the intensity bound.
    nz_map >>= is_bound;

    // Create an iterator that yields a band start-end pair, and scale-factor.
    let bands_iter = bands.iter()
                          .zip(&bands[1..])
                          .zip(granule.channels[1].scalefacs.iter())
                          .skip(is_bound);

    // Iterate over each band and process it accordingly.
    for ((start, end), is_pos) in bands_iter {
        // Bands or windows starting at or beyond rzero are all 0 so there is no need to process
        // them.
        if *start >= rzero {
            break;
        }

        // This band only contains zeros. It *may* be intensity stereo coded if the remaining bands
        // and/or windows are all zero as well.
        let do_intensity = if nz_map & 1 == 0 {
            // If the block is mixed, and the sample index is < 36, then the current band is a long
            // band. A long band in a mixed block can only be intensity stereo coded if all the
            // remaining long and short bands are zero.
            if is_mixed && (*start < 36) {
                nz_map == 0
            }
            // Otherwise, the band is short (mixed block or otherwise). Short bands are composed of
            // 3 windows. If the corresponding windows in the remaining bands are also zero, then
            // this window is intensity stereo coded.
            else {
                nz_map & WIN_MASK == 0
            }
        }
        else {
            // If the band is non-zero then it is never intensity encoded.
            false
        };

        // If the band or window was coded with intensity stereo encoding then decode it now.
        if do_intensity {
            process_intensity_mpeg2(
                is_pos_table,
                *is_pos as usize,
                mid_side,
                &mut ch0[*start..*end],
                &mut ch1[*start..*end],
            );
        }
        // If intensity stereo coding was not used, but mid-side stereo coding is enabled, decode
        // the band or window's mid-side stereo coding.
        else if mid_side {
            process_mid_side(&mut ch0[*start..*end], &mut ch1[*start..*end]);
        }

        // Consume the bitmap. Shifting right lets us use the same window mask for all windows in a
        // band.
        nz_map >>= 1;
    }

    // Since short/mixed blocks can be sparse, mid-side stereo decoding is done on a band-by-band,
    // window-by-window basis in this case. So the intensity bound is 0.
    bands[is_bound]
}

/// Perform joint stereo decoding on the channel pair.
pub(super) fn stereo(
    header: &FrameHeader,
    granule: &mut Granule,
    ch: &mut [[f32; 576]; 2],
) -> Result<()> {

    // Determine whether mid-side, and/or intensity stereo coding is used.
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

    // Split the sample buffer into two channels.
    let (ch0, ch1) = {
        let (ch0, ch1) = ch.split_first_mut().unwrap();
        (ch0, &mut ch1[0])
    };

    // Joint stereo processing as specified in layer 3 is a combination of mid-side, and intensity
    // encoding schemes. Each scale-factor band may use either mid-side, intensity, or no stereo
    // encoding. The type of encoding used for each scale-factor band is determined by the MPEG
    // bitstream version, the mode extension, the block type, and the content of the scale-factor
    // bands.
    let end = max(granule.channels[0].rzero, granule.channels[1].rzero);

    // Decode intensity stereo coded bands if it is enabled and get the intensity bound.
    let is_bound = if intensity {
        // Decode intensity stereo coded bands based on bitstream version and block type.
        match granule.channels[1].block_type {
            BlockType::Short { is_mixed } if header.is_mpeg1() => {
                process_intensity_short_block_mpeg1(header, granule, is_mixed, mid_side, ch0, ch1)
            },
            BlockType::Short { is_mixed } => {
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

    // If mid-side stereo coding is enabled, all samples up to the intensity bound should be
    // decoded as mid-side stereo.
    if mid_side && is_bound > 0 {
        process_mid_side(&mut ch0[0..is_bound], &mut ch1[0..is_bound]);
    }

    // With joint stereo encoding, there is usually a mismatch between the number of samples 
    // initially read from the bitstream for each channel. This count is stored as the rzero sample
    // index. However, after joint stereo decoding, both channels will have the same number of
    // samples. Update rzero for both channels with the actual number of samples.
    if intensity || mid_side {
        granule.channels[0].rzero = end;
        granule.channels[1].rzero = end;
    }

    Ok(())
}