// Symphonia
// Copyright (c) 2019 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::cmp::min;
use std::{f32, f64};

use symphonia_core::io::{ReadBitsLtr, huffman::{H8, HuffmanTable}};
use symphonia_core::errors::Result;

use lazy_static::lazy_static;
use log::info;

use crate::common::*;
use crate::huffman_tables::*;
use super::GranuleChannel;

lazy_static! {
    /// Lookup table for computing x(i) = s(i)^(4/3) where s(i) is a decoded Huffman sample. The
    /// value of s(i) is bound between 0..8207.
    static ref REQUANTIZE_POW43: [f32; 8207] = {
        // It is wasteful to initialize to 0.. however, Symphonia policy is to limit unsafe code to
        // only symphonia-core.
        //
        // TODO: Implement generic lookup table initialization in the core library.
        let mut pow43 = [0f32; 8207];
        for i in 0..8207 {
            pow43[i] = f32::powf(i as f32, 4.0 / 3.0);
        }
        pow43
    };
}

struct MpegHuffmanTable {
    /// The Huffman decode table.
    huff_table: &'static HuffmanTable<H8>,
    /// Number of extra bits to read if the decoded Huffman value is saturated.
    linbits: u32,
}

const HUFFMAN_TABLES: [MpegHuffmanTable; 32] = [
    // Table 0
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_0,  linbits:  0 },
    // Table 1
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_1,  linbits:  0 },
    // Table 2
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_2,  linbits:  0 },
    // Table 3
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_3,  linbits:  0 },
    // Table 4 (not used)
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_0,  linbits:  0 },
    // Table 5
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_5,  linbits:  0 },
    // Table 6
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_6,  linbits:  0 },
    // Table 7
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_7,  linbits:  0 },
    // Table 8
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_8,  linbits:  0 },
    // Table 9
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_9,  linbits:  0 },
    // Table 10
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_10, linbits:  0 },
    // Table 11
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_11, linbits:  0 },
    // Table 12
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_12, linbits:  0 },
    // Table 13
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_13, linbits:  0 },
    // Table 14 (not used)
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_0,  linbits:  0 },
    // Table 15
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_15, linbits:  0 },
    // Table 16
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_16, linbits:  1 },
    // Table 17
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_16, linbits:  2 },
    // Table 18
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_16, linbits:  3 },
    // Table 19
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_16, linbits:  4 },
    // Table 20
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_16, linbits:  6 },
    // Table 21
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_16, linbits:  8 },
    // Table 22
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_16, linbits: 10 },
    // Table 23
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_16, linbits: 13 },
    // Table 24
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_24, linbits:  4 },
    // Table 25
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_24, linbits:  5 },
    // Table 26
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_24, linbits:  6 },
    // Table 27
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_24, linbits:  7 },
    // Table 28
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_24, linbits:  8 },
    // Table 29
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_24, linbits:  9 },
    // Table 30
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_24, linbits: 11 },
    // Table 31
    MpegHuffmanTable { huff_table: &HUFFMAN_TABLE_24, linbits: 13 },
];

/// Reads the Huffman coded spectral samples for a given channel in a granule from a `BitStream`
/// into a provided sample buffer. Returns the number of decoded samples (the starting index of the
/// rzero partition).
///
/// Note, each spectral sample is raised to the (4/3)-rd power. This is not actually part of the
/// Huffman decoding process, but, by converting the integer sample to floating point here we don't
/// need to do pointless casting or use an extra buffer.
pub(super) fn read_huffman_samples<B: ReadBitsLtr>(
    bs: &mut B,
    channel: &GranuleChannel,
    part3_bits: u32,
    buf: &mut [f32; 576],
) -> Result<usize> {

    // If there are no Huffman code bits, zero all samples and return immediately.
    if part3_bits == 0 {
        for sample in buf.iter_mut() {
            *sample = 0.0;
        }
        return Ok(0);
    }

    // Dereference the POW43 table once per granule since there is a tiny overhead each time a
    // lazy_static is dereferenced that should be amortized over as many samples as possible.
    let pow43_table: &[f32; 8207] = &REQUANTIZE_POW43;

    let mut bits_read = 0;
    let mut i = 0;

    // There are two samples per big_value, therefore multiply big_values by 2 to get number of
    // samples in the big_value partition.
    let big_values_len = 2 * channel.big_values as usize;

    // There are up-to 3 regions in the big_value partition. Determine the sample index denoting the
    // end of each region (non-inclusive). Clamp to the end of the big_values partition.
    let regions: [usize; 3] = [
        min(channel.region1_start as usize, big_values_len),
        min(channel.region2_start as usize, big_values_len),
        min(                           576, big_values_len),
    ];

    // Iterate over each region in big_values.
    for (region_idx, region_end) in regions.iter().enumerate() {

        // Select the Huffman table based on the region's table select value.
        let table = &HUFFMAN_TABLES[channel.table_select[region_idx] as usize];

        // If the table for a region is empty, fill the region with zeros and move on to the next
        // region.
        if table.huff_table.data.is_empty() {
            while i < *region_end {
                buf[i] = 0.0;
                i += 1;
                buf[i] = 0.0;
                i += 1;
            }
            continue;
        }

        // Otherwise, read the big_values.
        while i < *region_end && bits_read < part3_bits {
            // Decode the next Huffman code.
            let (value, code_len) = bs.read_huffman(&table.huff_table, 0)?;
            bits_read += code_len;

            // In the big_values partition, each Huffman code decodes to two sample, x and y. Each
            // sample being 4-bits long.
            let mut x = (value >> 4) as usize;
            let mut y = (value & 0xf) as usize;

            // If the first sample, x, is not 0, further process it.
            if x > 0 {
                // If x is saturated (it is at the maximum possible value), and the table specifies
                // linbits, then read linbits more bits and add it to the sample.
                if x == 15 && table.linbits > 0 {
                    x += bs.read_bits_leq32(table.linbits)? as usize;
                    bits_read += table.linbits;
                }

                // The next bit is the sign bit. The value of the sample is raised to the (4/3)
                // power.
                buf[i] = if bs.read_bit()? { -pow43_table[x] } else { pow43_table[x] };
                bits_read += 1;
            }
            else {
                buf[i] = 0.0;
            }

            i += 1;

            // Likewise, repeat the previous two steps for the second sample, y.
            if y > 0 {
                if y == 15 && table.linbits > 0 {
                    y += bs.read_bits_leq32(table.linbits)? as usize;
                    bits_read += table.linbits;
                }

                buf[i] = if bs.read_bit()? { -pow43_table[y] } else { pow43_table[y] };
                bits_read += 1;
            }
            else {
                buf[i] = 0.0;
            }

            i += 1;
        }
    }

    // Select the Huffman table for the count1 partition.
    let count1_table = if channel.count1table_select {
        QUADS_HUFFMAN_TABLE_B
    }
    else {
        QUADS_HUFFMAN_TABLE_A
    };

    // Read the count1 partition.
    while i <= 572 && bits_read < part3_bits {
        // Decode the next Huffman code. Note that we allow the Huffman decoder a few extra bits in
        // case of a count1 overrun (see below for more details).
        let (value, code_len) = bs.read_huffman(&count1_table, 0)?;
        bits_read += code_len;

        // In the count1 partition, each Huffman code decodes to 4 samples: v, w, x, and y.
        // Each sample is 1-bit long (1 or 0).
        //
        // For each 1-bit sample, if it is 0, then then dequantized sample value is 0 as well. If
        // the 1-bit sample is 1, then read the sign bit (the next bit). The dequantized sample is
        // then either +/-1.0 depending on the sign bit.
        if value & 0x8 != 0 {
            buf[i] = if bs.read_bit()? { -1.0 } else { 1.0 };
            bits_read += 1;
        }
        else {
            buf[i] = 0.0;
        }

        i += 1;

        if value & 0x4 != 0 {
            buf[i] = if bs.read_bit()? { -1.0 } else { 1.0 };
            bits_read += 1;
        }
        else {
            buf[i] = 0.0;
        }

        i += 1;

        if value & 0x2 != 0 {
            buf[i] = if bs.read_bit()? { -1.0 } else { 1.0 };
            bits_read += 1;
        }
        else {
            buf[i] = 0.0;
        }

        i += 1;

        if value & 0x1 != 0 {
            buf[i] = if bs.read_bit()? { -1.0 } else { 1.0 };
            bits_read += 1;
        }
        else {
            buf[i] = 0.0;
        }

        i += 1;
    }

    // Ignore any extra "stuffing" bits.
    if bits_read < part3_bits {
        bs.ignore_bits(part3_bits - bits_read)?;
    }
    // Word on the street is that some encoders are poor at "stuffing" bits, resulting in part3_len
    // being ever so slightly too large. This causes the Huffman decode loop to decode the next few
    // bits as a sample. However, these bits are random data and not a real sample, so erase it!
    // The caller will be reponsible for re-aligning the bitstream reader. Candy Pop confirms this.
    else if bits_read > part3_bits {
        info!("count1 overrun, malformed bitstream");
        i -= 4;
    }

    // The final partition after the count1 partition is the rzero partition. Samples in this
    // partition are all 0.
    for j in (i..576).step_by(2) {
        buf[j+0] = 0.0;
        buf[j+1] = 0.0;
    }

    Ok(i)
}

/// Requantize long block samples in `buf`.
fn requantize_long(channel: &GranuleChannel, bands: &[usize], buf: &mut [f32; 576]) {
    // For long blocks dequantization and scaling is governed by the following equation:
    //
    //                     xr(i) = s(i)^(4/3) * 2^(0.25*A) * 2^(-B)
    // where:
    //       s(i) is the decoded Huffman sample
    //      xr(i) is the dequantized sample
    // and:
    //      A = global_gain[gr] - 210
    //      B = scalefac_multiplier * (scalefacs[gr][ch][sfb] + (preflag[gr] * pretab[sfb]))
    //
    // Note: The samples in buf are the result of s(i)^(4/3) for each sample i.
    debug_assert!(bands.len() <= 23);

    // The preemphasis table is from table B.6 in ISO/IEC 11172-3.
    const PRE_EMPHASIS: [u8; 22] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        1, 1, 1, 1, 2, 2, 3, 3, 3, 2, 0,
    ];

    // Calculate A, it is constant for the entire requantization.
    let a = i32::from(channel.global_gain) - 210;

    let scalefac_shift = if channel.scalefac_scale { 2 } else { 1 };

    // Requantize each scale-factor band in buf.
    for (i, (start, end)) in bands.iter().zip(&bands[1..]).enumerate() {
        // Do not requantize bands starting after the rzero sample since all samples from there on
        // are 0.
        if *start >= channel.rzero {
            break;
        }

        // Lookup the pre-emphasis amount if required.
        let pre_emphasis = if channel.preflag { PRE_EMPHASIS[i] } else { 0 };

        // Calculate B.
        let b = i32::from((channel.scalefacs[i] + pre_emphasis) << scalefac_shift);

        // Calculate 2^(0.25*A) * 2^(-B). This can be rewritten as 2^{ 0.25 * (A - 4 * B) }.
        // Since scalefac_shift was multiplies by 4 above, the final equation becomes
        // 2^{ 0.25 * (A - B) }.
        let pow2ab = f64::powf(2.0, 0.25 * f64::from(a - b)) as f32;

        // Calculate the ending sample index for the scale-factor band, clamping it to the length of
        // the sample buffer.
        let band_end = min(*end, channel.rzero);

        // The sample buffer contains s(i)^(4/3), now multiply in 2^(0.25*A) * 2^(-B) to get xr(i).
        for sample in &mut buf[*start..band_end] {
            *sample *= pow2ab;
        }
    }
}

/// Requantize short block samples in `buf` starting at scale-factor band `sfb_init`.
fn requantize_short(
    channel: &GranuleChannel,
    bands: &[usize],
    switch: usize,
    buf: &mut [f32; 576],
) {
    // For short blocks dequantization and scaling is governed by the following equation:
    //
    //                     xr(i) = s(i)^(4/3) * 2^(0.25*A) * 2^(-B)
    // where:
    //       s(i) is the decoded Huffman sample
    //      xr(i) is the dequantized sample
    // and:
    //      A = global_gain[gr] - 210 - (8 * subblock_gain[gr][win])
    //      B = scalefac_multiplier * scalefacs[gr][ch][sfb][win]
    //
    // Note: The samples in buf are the result of s(i)^(4/3) for each sample i.
    debug_assert!(bands.len() <= 40);

    // Calculate the window-independant part of A: global_gain[gr] - 210.
    let gain = i32::from(channel.global_gain) - 210;

    // Calculate A for each window.
    let a = [
        gain - 8 * i32::from(channel.subblock_gain[0]),
        gain - 8 * i32::from(channel.subblock_gain[1]),
        gain - 8 * i32::from(channel.subblock_gain[2]),
    ];

    // Likweise, the scalefac_multiplier is constant for the granule. The actual scale is multiplied
    // by 4 to combine the two pow2 operations into one by adding the exponents. The sum of the
    // exponent is multiplied by 0.25 so B must be multiplied by 4 to counter the quartering. A
    // bitshift operation is used for the actual multiplication, so scalefac_multiplier is named
    // scalefac_shift in this case.
    let scalefac_shift = if channel.scalefac_scale { 2 } else { 1 };

    for (i, (start, end)) in bands.iter().zip(&bands[1..]).enumerate() {
        // Do not requantize bands starting after the rzero sample since all samples from there on
        // are 0.
        if *start > channel.rzero {
            break;
        }

        // Calculate B.
        let b = i32::from(channel.scalefacs[switch + i] << scalefac_shift);

        // Calculate 2^(0.25*A) * 2^(-B). This can be rewritten as 2^{ 0.25 * (A - 4 * B) }.
        // Since scalefac_shift multiplies by 4 above, the final equation becomes
        // 2^{ 0.25 * (A - B) }.
        let pow2ab = f64::powf(2.0,  0.25 * f64::from(a[i % 3] - b)) as f32;

        // Clamp the ending sample index to the rzero sample index. Since samples starting from
        // rzero are 0, there is no point in requantizing them.
        let win_end = min(*end, channel.rzero);

        // The sample buffer contains s(i)^(4/3), now multiply in 2^(0.25*A) * 2^(-B) to get
        // xr(i).
        for sample in &mut buf[*start..win_end] {
            *sample *= pow2ab;
        }
    }

}

/// Requantize samples in `buf` regardless of block type.
pub(super) fn requantize(
    header: &FrameHeader,
    channel: &GranuleChannel,
    buf: &mut [f32; 576],
) {
    match channel.block_type {
        BlockType::Short { is_mixed: false } => {
            requantize_short(channel, &SFB_SHORT_BANDS[header.sample_rate_idx], 0, buf);
        },
        BlockType::Short { is_mixed: true } => {
            // A mixed block is a combination of a long block and short blocks. The first few scale
            // factor bands, and thus samples, belong to a single long block, while the remaining
            // bands and samples belong to short blocks. Therefore, requantization for mixed blocks
            // can be decomposed into short and long block requantizations.
            //
            // As per ISO/IEC 11172-3, the short scale factor band at which the long block ends and
            // the short blocks begin is denoted by switch_point_s.
            let bands = SFB_MIXED_BANDS[header.sample_rate_idx];
            let switch = SFB_MIXED_SWITCH_POINT[header.sample_rate_idx];

            requantize_long(channel, &bands[..switch], buf);
            requantize_short(channel, &bands[switch..], switch, buf);
        },
        _ => {
            requantize_long(channel, &SFB_LONG_BANDS[header.sample_rate_idx], buf);
        },
    }
}