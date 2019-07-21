// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use sonata_core::checksum::Crc16;
use sonata_core::errors::{Result, decode_error, unsupported_error};
use sonata_core::io::{BufStream, BitStream, BitStreamLtr, Bytestream, huffman::{H8, HuffmanTable}};

use super::huffman_tables::*;

/// Bit-rate lookup table for MPEG version 1 layer 1.
static BIT_RATES_MPEG1_L1: [u32; 15] = 
[
    0,
    32_000,  64_000,  96_000, 128_000, 160_000, 192_000, 224_000,
    256_000, 288_000, 320_000, 352_000, 384_000, 416_000, 448_000,
];

/// Bit-rate lookup table for MPEG version 1 layer 2.
static BIT_RATES_MPEG1_L2: [u32; 15] = 
[
    0,
    32_000,  48_000,  56_000,  64_000,  80_000,  96_000, 112_000,
    128_000, 160_000, 192_000, 224_000, 256_000, 320_000, 384_000,
];

/// Bit-rate lookup table for MPEG version 1 layer 3.
static BIT_RATES_MPEG1_L3: [u32; 15] = 
[
    0,
    32_000,  40_000,  48_000,  56_000,  64_000,  80_000,  96_000,
    112_000, 128_000, 160_000, 192_000, 224_000, 256_000, 320_000
];

/// Bit-rate lookup table for MPEG version 2 & 2.5 audio layer 1.
static BIT_RATES_MPEG2_L1: [u32; 15] =
[
    0,
    32_000,  48_000,  56_000,  64_000,  80_000,  96_000,  112_000,
    128_000, 144_000, 160_000, 176_000, 192_000, 224_000, 256_000,
];

/// Bit-rate lookup table for MPEG version 2 & 2.5 audio layers 2 & 3.
static BIT_RATES_MPEG2_L23: [u32; 15] =
[
    0,
    8_000,  16_000, 24_000, 32_000,  40_000,  48_000,  56_000,
    64_000, 80_000, 96_000, 112_000, 128_000, 144_000, 160_000,
];

/// Number of bits for MPEG version 1 scale factors. Indexed by scalefac_compress.
static SCALE_FACTOR_SLEN: [(u32, u32); 16] = 
[
    (0, 0), (0, 1), (0, 2), (0, 3), (3, 0), (1, 1), (1, 2), (1, 3), 
    (2, 1), (2, 2), (2, 3), (3, 1), (3, 2), (3, 3), (4, 2), (4, 3),
];

/// Number of bits for MPEG version 2 scale factors. Indexed by scalefac_compress, and block_type.
const SCALE_FACTOR_NSFB: [[[usize; 4]; 3]; 6] = [
    // Intensity stereo channel modes.
    [[ 6,  5, 5, 5], [ 9,  9,  9, 9], [ 6,  9,  9, 9]],
    [[ 6,  5, 7, 3], [ 9,  9, 12, 6], [ 6,  9, 12, 6]],
    [[11, 10, 0, 0], [18, 18,  0, 0], [15, 18,  0, 0]],
    // Other channel modes.
    [[ 7,  7, 7, 0], [12, 12, 12, 0], [ 6, 15, 12, 0]],
    [[ 6,  6, 6, 3], [12,  9,  9, 6], [ 6, 12,  9, 6]],
    [[ 8,  8, 5, 0], [15, 12,  9, 0], [ 6, 18,  9, 0]],
];

/// Startng indicies of each scale factor band at various sampling rates for long blocks.
const SCALE_FACTOR_LONG_BANDS: [[usize; 23]; 9] = [
    // 44.1 kHz, MPEG version 1
    [
         0,  4,   8,  12,  16,  20,  24,  30,  36,  42,  50, 60, 
        72, 88, 106, 128, 156, 190, 230, 276, 330, 384, 576,
    ],
    // 48 kHz
    [
         0,   4,   8,  12,  16,  20,  24,  30,  36,  44,  54, 66, 
        82, 102, 126, 156, 194, 240, 296, 364, 448, 550, 576,
    ],
    // 32 kHz
    [
         0,   4,   8,  12,  16,  20,  24,  30,  36,  44,   52, 62,
        74,  90, 110,  134, 162, 196, 238, 288, 342, 418, 576,
    ],
    // 22.050 kHz, MPEG version 2
    [
         0,  4,   8,  12,  16,  20,  24,  30,  36,  44,  52, 62, 
        74, 90, 110, 134, 162, 196, 238, 288, 342, 418, 576,
    ],
    // 24 kHz
    [
        0, 4, 8, 12, 16, 20, 24, 30, 36, 42, 50, 60, 
        72, 88, 106, 128, 156, 190, 230, 276, 330, 384, 576,
    ],
    // 16 kHz
    [
         0,   4,   8,  12,  16,  20,  24,  30,  36,  44,  54, 66, 
        82, 102, 126, 156, 194, 240, 296, 364, 448, 550, 576,
    ],
    // 11.025 kHz, MPEG version 2.5
    [
          0,   6,  12,  18,  24,  30,  36,  44,  54,  66,  80, 96, 
        116, 140, 168, 200, 238, 284, 336, 396, 464, 522, 576,
    ],
    // 12 kHz
    [
          0,   6,  12,  18,  24,  30,  36,  44,  54,  66,  80, 96, 
        116, 140, 168, 200, 238, 284, 336, 396, 464, 522, 576,
    ],
    // 8 kHz
    [
          0,  12,  24 , 36,  48,  60,  72,  88, 108, 132, 160, 192, 
        232, 280, 336, 400, 476, 566, 568, 570, 572, 574, 576,
    ],
];

/// Startng indicies of each scale factor band at various sampling rates for short blocks.
const SCALE_FACTOR_SHORT_BANDS: [[u32; 14]; 9] = [
    // 44.1 kHz
    [0, 4,  8, 12, 16, 22, 30, 40,  52,  66,  84, 106, 136, 192],
    // 48 kHz
    [0, 4,  8, 12, 16, 22, 28, 38,  50,  64,  80, 100, 126, 192],
    // 32 kHz
    [0, 4,  8, 12, 16, 22, 30, 42,  58,  78, 104, 138, 180, 192],
    // 22.050 kHz
    [0, 4,  8, 12, 16, 22, 30, 40,  52,  66,  84, 106, 136, 192],
    // 24 kHz
    [0, 4,  8, 12, 16, 22, 28, 38,  50,  64,  80, 100, 126, 192],
    // 16 kHz
    [0, 4,  8, 12, 16, 22, 30, 42,  58,  78, 104, 138, 180, 192],
    // 11.025 kHz
    [0, 4,  8, 12, 18, 26, 36, 48,  62,  80, 104, 134, 174, 192],
    // 12 kHz
    [0, 4,  8, 12, 18, 26, 36, 48,  62,  80, 104, 134, 174, 192],
    // 8 kHz
    [0, 8, 16, 24, 36, 52, 72, 96, 124, 160, 162, 164, 166, 192],
];

struct MpegHuffmanTable {
    /// The Huffman decode table.
    table: &'static HuffmanTable<H8>,
    /// Number of extra bits to read if the decoded Huffman value is saturated.
    linbits: u32,
}

const HUFFMAN_TABLES: [MpegHuffmanTable; 32] = [
    MpegHuffmanTable { table: &HUFFMAN_TABLE_0,  linbits:  0 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_1,  linbits:  0 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_2,  linbits:  0 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_3,  linbits:  0 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_0,  linbits:  0 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_5,  linbits:  0 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_6,  linbits:  0 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_7,  linbits:  0 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_8,  linbits:  0 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_9,  linbits:  0 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_10, linbits:  0 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_11, linbits:  0 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_12, linbits:  0 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_13, linbits:  0 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_0,  linbits:  0 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_15, linbits:  0 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_16, linbits:  1 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_16, linbits:  2 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_16, linbits:  3 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_16, linbits:  4 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_16, linbits:  6 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_16, linbits:  8 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_16, linbits: 10 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_16, linbits: 13 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_24, linbits:  4 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_24, linbits:  5 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_24, linbits:  6 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_24, linbits:  7 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_24, linbits:  8 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_24, linbits:  9 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_24, linbits: 11 },
    MpegHuffmanTable { table: &HUFFMAN_TABLE_24, linbits: 13 },
];

/// The MPEG audio version.
#[derive(Copy,Clone,Debug,PartialEq)]
enum MpegVersion { 
    /// Version 2.5
    Mpeg2p5,
    /// Version 2
    Mpeg2,
    // Version 1
    Mpeg1,
}

/// The MPEG audio layer.
#[derive(Copy,Clone,Debug,PartialEq)]
enum MpegLayer {
    /// Layer 1
    Layer1,
    /// Layer 2
    Layer2,
    /// Layer 3
    Layer3,
}

/// For Joint Stereo mode, the mode extension describes the features and parameters of the Joint
/// Stereo encoding.
#[derive(Copy,Clone,Debug,PartialEq)]
enum ModeExtension {
    /// Joint Stereo in layer 3 may use both Mid-Side and Intensity encoding.
    Layer3 { mid_side: bool, intensity: bool },
    /// Joint Stereo in layers 1 and 2 may only use Intensity encoding on a set of bands. The range
    /// of bands is [bound..32].
    Intensity { bound: u32 },
}

/// The channel mode.
#[derive(Copy,Clone,Debug,PartialEq)]
enum Channels {
    /// Single mono audio channel.
    Mono,
    /// Dual mono audio channels.
    DualMono,
    /// Stereo channels.
    Stereo,
    /// Joint Stereo encoded channels (decodes to Stereo).
    JointStereo(ModeExtension),
}

impl Channels {
    #[inline(always)]
    fn count(&self) -> usize {
        match *self {
            Channels::Mono => 1,
            _              => 2,
        }
    }
}

/// The emphasis applied during encoding.
#[derive(Copy,Clone,Debug,PartialEq)]
enum Emphasis {
    // No emphasis
    None,
    /// 50/15us
    Fifty15,
    /// CCIT J.17
    CcitJ17,
}

/// A MPEG 1, 2, or 2.5 audio frame header.
#[derive(Debug)]
struct FrameHeader { 
    version: MpegVersion,
    layer: MpegLayer,
    bitrate: u32,
    sample_rate: u32,
    sample_rate_idx: usize,
    channels: Channels,
    emphasis: Emphasis,
    is_copyrighted: bool,
    is_original: bool,
    has_padding: bool,
    crc: Option<u16>,
    frame_size: usize,
}

impl FrameHeader {
    #[inline(always)]
    fn is_mpeg1(&self) -> bool {
        self.version == MpegVersion::Mpeg1
    }

    #[inline(always)]
    fn is_mpeg2(&self) -> bool {
        self.version != MpegVersion::Mpeg1
    }
    
    #[inline(always)]
    fn is_layer1(&self) -> bool {
        self.layer == MpegLayer::Layer1
    }

    #[inline(always)]
    fn is_layer2(&self) -> bool {
        self.layer == MpegLayer::Layer2
    }

    #[inline(always)]
    fn is_layer3(&self) -> bool {
        self.layer == MpegLayer::Layer3
    }

    #[inline(always)]
    fn n_granules(&self) -> usize {
        if self.layer == MpegLayer::Layer1 { 2 } else { 2 }
    }

    #[inline(always)]
    fn n_channels(&self) -> usize {
        self.channels.count()
    }

    #[inline(always)]
    fn is_intensity_stereo(&self) -> bool {
        match self.channels {
            Channels::JointStereo(ModeExtension::Intensity { .. }) => true,
            Channels::JointStereo(ModeExtension::Layer3 { intensity, ..}) => intensity,
            _ => false,
        }
    }
}

#[derive(Debug, Default)]
struct SideInfoL3 {
    /// The byte offset into the bit resevoir indicating the location of the first bit of main_data.
    /// If 0, main_data begins after the side_info of this frame.
    main_data_begin: u16,
    /// Scale factor selector information, per channel. Each channel has 4 groups of bands that may
    /// be scaled in each granule. Scale factors may optionally be used by both granules to save 
    /// bits. Bands that share scale factors for both granules are indicated by a true. Otherwise, 
    /// each granule must store its own set of scale factors.
    /// 
    /// Mapping of array indicies to bands [0-5, 6-10, 11-15, 16-20].
    scfsi: [[bool; 4]; 2],
    /// Granules
    granules: [GranuleSideInfoL3; 2],
    /// The size of the side_info.
    size: usize,
}

impl SideInfoL3 {
    #[inline(always)]
    fn granules_mut(&mut self, version: MpegVersion) -> &mut [GranuleSideInfoL3] {
        match version {
            MpegVersion::Mpeg1 => &mut self.granules[..2],
            _                  => &mut self.granules[..1],
        }
    }

    #[inline(always)]
    fn granules_ref(&self, version: MpegVersion) -> &[GranuleSideInfoL3] {
        match version {
            MpegVersion::Mpeg1 => &self.granules[..2],
            _                  => &self.granules[..1],
        }
    }

}

#[derive(Debug, Default)]
struct GranuleSideInfoL3 {
    /// Channels in the granule.
    channels: [GranuleChannelSideInfoL3; 2],
}

#[derive(Debug)]
enum BlockType {
    // Default case when window switching is off. Also the normal case when window switching is
    // on. Granule contains one long block.
    Long,
    Start,
    Short { is_mixed: bool },
    End
}

impl Default for BlockType {
    fn default() -> BlockType {
        BlockType::Long
    }
}

#[derive(Debug, Default)]
struct GranuleChannelSideInfoL3 {
    /// Nums of bits used for scale factors (part2), and Huffman encoded data (part3).
    part2_3_length: u16,
    /// Big values (sum of region0, region1, region2) partition size.
    big_values: u16,
    /// Quantization step size.
    global_gain: u16,
    // Index into SCALE_FACTOR_SLEN for number of bits used per scale factor in MPEG version 1.
    // In MPEG version 2, decoded into slen[1-4] to determine number of bits per scale factor.
    scalefac_compress: u16,
    /// Indicates the type of window for the granule.
    block_type: BlockType,

    subblock_gain: [u8; 3],

    table_select: [u8; 3],
    region0_count: u8,
    region1_count: u8,

    preflag: bool,
    scalefac_scale: bool,
    count1table_select: bool,
}

#[derive(Default)]
struct MainData {
    granules: [MainDataGranule; 2],
}

#[derive(Default)]
struct MainDataGranule {
    channels: [MainDataGranuleChannel; 2],
}

struct MainDataGranuleChannel {
    /// Long (scalefac_l) and short (scalefac_s) window scale factor bands. Must be interpreted 
    /// based on block type.
    /// 
    /// For block_type == Short, is_mixed == false: 
    ///     scalefac_s = scalefacs[0..36]
    /// 
    /// For block_type == Short, is_mixed == true: 
    ///     scalefac_l[0..8]  = scalefacs[0..8] 
    ///     scalefac_s[0..27] = scalefacs[8..35]
    /// 
    /// For block_type != Short:
    ///     scalefac_l[0..21] = scalefacs[0..21]
    scalefacs: [u8; 36],
}

impl Default for MainDataGranuleChannel {
    fn default() -> Self {
        MainDataGranuleChannel { scalefacs: [0; 36] }
    }
}

/// Synchronize the provided reader to the end of the frame header, and return the frame header as
/// as `u32`.
fn sync_frame<B: Bytestream>(reader: &mut B) -> Result<u32> {
    let mut sync = 0u32;

    // Synchronize stream to the next frame using the sync word. The MP3 frame header always starts
    // at a byte boundary with 0xffe (11 consecutive 1 bits.) if supporting up to MPEG version 2.5.
    while (sync & 0xffe00000) != 0xffe00000 {
        sync = sync.wrapping_shl(8) | reader.read_u8()? as u32;
    }

    Ok(sync)
}

/// Reads a MPEG frame header from the stream and return it or an error.
fn read_frame_header<B: Bytestream>(reader: &mut B) -> Result<FrameHeader> {
    // Synchronize and read the frame header.
    let header = sync_frame(reader)?;

    // The MP3 header is as follows:
    // 
    // 0b1111_1111 0b111v_vlly 0brrrr_hhpx 0bmmmm_coee
    // where:
    //     vv   = version, ll = layer      , y = crc
    //     rrrr = bitrate, hh = sample rate, p = padding , x  = private bit
    //     mmmm = mode   , c  = copyright  , o = original, ee = emphasis

    let version = match (header & 0x18_0000) >> 19 {
        0b00 => MpegVersion::Mpeg2p5,
        0b10 => MpegVersion::Mpeg2,
        0b11 => MpegVersion::Mpeg1,
        _    => return decode_error("Invalid MPEG version."),
    };

    let layer = match (header & 0x6_0000) >> 17 {
        0b01 => MpegLayer::Layer3,
        0b10 => MpegLayer::Layer2,
        0b11 => MpegLayer::Layer1,
        _    => return decode_error("Invalid MPEG layer."),
    };

    let bitrate = match ((header & 0xf000) >> 12, version, layer) {
        // "Free" bit-rate. Note, this is NOT variable bit-rate and is not a mandatory feature of
        // MP3 decoders.
        (0b0000, _, _) => return unsupported_error("Free bit-rate is not supported."),
        // Invalid bit-rate.
        (0b1111, _, _) => return decode_error("Invalid bit-rate."),
        // MPEG 1 bit-rates.
        (i, MpegVersion::Mpeg1, MpegLayer::Layer1) => BIT_RATES_MPEG1_L1[i as usize],
        (i, MpegVersion::Mpeg1, MpegLayer::Layer2) => BIT_RATES_MPEG1_L2[i as usize],
        (i, MpegVersion::Mpeg1, MpegLayer::Layer3) => BIT_RATES_MPEG1_L3[i as usize],
        // MPEG 2 bit-rates.
        (i,                  _, MpegLayer::Layer1) => BIT_RATES_MPEG2_L1[i as usize],
        (i,                  _,                 _) => BIT_RATES_MPEG2_L23[i as usize],
    };

    let (sample_rate, sample_rate_idx) = match ((header & 0xc00) >> 10, version) {
        (0b00, MpegVersion::Mpeg1)   => (44_100, 0),
        (0b01, MpegVersion::Mpeg1)   => (48_000, 1),
        (0b10, MpegVersion::Mpeg1)   => (32_000, 2),
        (0b00, MpegVersion::Mpeg2)   => (22_050, 3),
        (0b01, MpegVersion::Mpeg2)   => (24_000, 4),
        (0b10, MpegVersion::Mpeg2)   => (16_000, 5),
        (0b00, MpegVersion::Mpeg2p5) => (11_025, 6),
        (0b01, MpegVersion::Mpeg2p5) => (12_000, 7),
        (0b10, MpegVersion::Mpeg2p5) => ( 8_000, 8),
        _                            => return decode_error("Invalid sample rate."),
    };

    let channels = match ((header & 0xc0) >> 6, layer) {
        // Stereo, for layers 1, 2, and 3.
        (0b00,                 _) => Channels::Stereo,
        // Dual mono, for layers 1, 2, and 3.
        (0b10,                 _) => Channels::DualMono,
        // Mono, for layers 1, 2, and 3.
        (0b11,                 _) => Channels::Mono,
        // Joint stereo mode for layer 3 supports a combination of Mid-Side and Intensity Stereo 
        // depending on the mode extension bits.
        (0b01, MpegLayer::Layer3) => Channels::JointStereo(ModeExtension::Layer3 {
            mid_side:  header & 0x20 != 0x0,
            intensity: header & 0x10 != 0x0,
        }),
        // Joint stereo mode for layers 1 and 2 only supports Intensity Stereo. The mode extension
        // bits indicate for which sub-bands intensity stereo coding is applied.
        (0b01,                 _) => Channels::JointStereo(ModeExtension::Intensity { 
            bound: (1 + (header & 0x30) >> 4) << 2,
        }),
        _                         => unreachable!(),
    };

    // Some layer 2 channel and bit-rate combinations are not allowed. Check that the frame does not
    // use them.
    if layer == MpegLayer::Layer2 {
        if channels == Channels::Mono {
            if bitrate == 224_000 
                || bitrate == 256_000 
                || bitrate == 320_000 
                || bitrate == 384_000
            {
                return decode_error("Invalid Layer 2 bitrate for mono channel mode.");
            }
        }
        else {
            if bitrate == 32_000 
                || bitrate == 48_000 
                || bitrate == 56_000 
                || bitrate == 80_000
            {
                return decode_error("Invalid Layer 2 bitrate for non-mono channel mode.");
            }
        }
    }

    let emphasis = match header & 0x3 {
        0b00 => Emphasis::None,
        0b01 => Emphasis::Fifty15,
        0b11 => Emphasis::CcitJ17,
        _    => return decode_error("Invalid emphasis."),
    };

    let is_copyrighted = header & 0x8 != 0x0;
    let is_original = header & 0x4 != 0x0;
    let has_padding = header & 0x200 != 0;

    let crc = if header & 0x1_0000 == 0 {
        Some(reader.read_be_u16()?)
    }
    else {
        None
    };

    // Calculate the size of the frame excluding this header.
    let frame_size = 
        (if version == MpegVersion::Mpeg1 { 144 } else { 72 } * bitrate / sample_rate) as usize
        + if has_padding { 1 } else { 0 }
        - if crc.is_some() { 2 } else { 0 }
        - 4;

    Ok(FrameHeader{
        version,
        layer,
        bitrate,
        sample_rate,
        sample_rate_idx,
        channels,
        emphasis,
        is_copyrighted,
        is_original,
        has_padding,
        crc,
        frame_size,
    })
}

fn read_granule_channel_side_info_l3<B: BitStream>(
    bs: &mut B,
    granule: &mut GranuleChannelSideInfoL3,
    header: &FrameHeader,
) -> Result<()> {

    granule.part2_3_length = bs.read_bits_leq32(12)? as u16;
    granule.big_values = bs.read_bits_leq32(9)? as u16;

    if granule.big_values > 288 {
        return decode_error("Granule big_values > 288.");
    }

    granule.global_gain = bs.read_bits_leq32(8)? as u16;

    granule.scalefac_compress = if header.is_mpeg1() {
        bs.read_bits_leq32(4)
    }
    else {
        bs.read_bits_leq32(9)
    }? as u16;
    
    let window_switching = bs.read_bit()?;

    if window_switching {
        let block_type_enc = bs.read_bits_leq32(2)?;

        let is_mixed = bs.read_bit()?;

        granule.block_type = match block_type_enc {
            0b00 => return decode_error("Invalid block_type."),
            0b01 => BlockType::Start,
            0b10 => BlockType::Short { is_mixed },
            0b11 => BlockType::End,
            _ => unreachable!(),
        };

        for i in 0..2 {
            granule.table_select[i] = bs.read_bits_leq32(5)? as u8;
        }

        for i in 0..3 {
            granule.subblock_gain[i] = bs.read_bits_leq32(3)? as u8;
        }

        granule.region0_count = match granule.block_type {
            BlockType::Short { is_mixed: false } => 8,
            _                                    => 7,
        };

        granule.region1_count = 20 - granule.region0_count;
    }
    else {
        granule.block_type = BlockType::Long;

        for i in 0..3 {
            granule.table_select[i] = bs.read_bits_leq32(5)? as u8;
        }

        granule.region0_count = bs.read_bits_leq32(4)? as u8;
        granule.region1_count = bs.read_bits_leq32(3)? as u8;
    }

    granule.preflag = if header.is_mpeg1() { 
        bs.read_bit()? 
    } 
    else { 
        granule.scalefac_compress >= 500
    };

    granule.scalefac_scale = bs.read_bit()?;
    granule.count1table_select = bs.read_bit()?;

    Ok(())
}

fn read_granule_side_info_l3<B: BitStream>(
    bs: &mut B, 
    granules: &mut GranuleSideInfoL3, 
    header: &FrameHeader,
) -> Result<()> {
    for channel_granule in &mut granules.channels[..header.channels.count()] {
        read_granule_channel_side_info_l3(bs, channel_granule, header)?;
    }
    Ok(())
}

fn l3_read_side_info<B: Bytestream>(reader: &mut B, header: &FrameHeader) -> Result<SideInfoL3> {
    let mut side_info: SideInfoL3 = Default::default();

    let mut bs = BitStreamLtr::new(reader);

    // For MPEG version 1...
    if header.is_mpeg1() {
        // First 9 bits is main_data_begin.
        side_info.main_data_begin = bs.read_bits_leq32(9)? as u16;

        // Next 3 (>1 channel) or 5 (1 channel) bits are private and should be ignored.
        match header.channels {
            Channels::Mono => bs.ignore_bits(5)?,
            _              => bs.ignore_bits(3)?,
        };

        // Next four (or 8, if more than one channel) are the SCFSI bits.
        for scfsi in &mut side_info.scfsi[..header.n_channels()] {
            for i in 0..4 {
                scfsi[i] = bs.read_bit()?;
            }
        }

        // The size of the side_info, fixed for layer 3.
        side_info.size = match header.channels {
            Channels::Mono => 17,
            _              => 32,
        };
    }
    // For MPEG version 2...
    else {
        // First 8 bits is main_data_begin.
        side_info.main_data_begin = bs.read_bits_leq32(8)? as u16;

        // Next 1 (1 channel) or 2 (>1 channel) bits are private and should be ignored.
        match header.channels {
            Channels::Mono => bs.ignore_bits(1)?,
            _              => bs.ignore_bits(2)?,
        };

        // The size of the side_info, fixed for layer 3.
        side_info.size = match header.channels {
            Channels::Mono =>  9,
            _              => 17,
        };
    }

    // Read the to granules
    for granule in side_info.granules_mut(header.version) {
        read_granule_side_info_l3(&mut bs, granule, header)?;
    }

    Ok(side_info)
}

/// Reads the scale factors for a single channel in a granule in a MPEG version 1 frame.
fn l3_read_scale_factors<B: BitStream>(
    bs: &mut B, 
    gr: usize,
    ch: usize,
    side_info: &SideInfoL3,
    main_data: &mut MainData, 
) -> Result<(usize)> {

    let mut bits_read = 0;

    let channel = &side_info.granules[gr].channels[ch];

    let (slen1, slen2) = SCALE_FACTOR_SLEN[channel.scalefac_compress as usize];

    // Short or Mixed windows...
    if let BlockType::Short { is_mixed } = channel.block_type {
        let data = &mut main_data.granules[gr].channels[ch];

        // If the block is mixed, there are three total scale factor partitions. The first is a long 
        // scale factor partition for bands 0..8 (scalefacs[0..8] with each scale factor being slen1
        // bits long. Following this is a short scale factor partition covering bands 8..11 with a 
        // window of 3 (scalefacs[8..17]) and each scale factoring being slen1 bits long.
        //
        // If a block is not mixed, then there are a total of two scale factor partitions. The first
        // is a short scale factor partition for bands 0..6 with a window length of 3 
        // (scalefacs[0..18]) and each scale factor being slen1 bits long.
        let n_sfb = if is_mixed { 8 + 3 * 3 } else { 6 * 3 };

        if slen1 > 0 {
            for sfb in 0..n_sfb {
                data.scalefacs[sfb] = bs.read_bits_leq32(slen1)? as u8;
            }
            bits_read += n_sfb * slen1 as usize;
        }

        // The final scale factor partition is always a a short scale factor window. It covers bands
        // 11..17 (scalefacs[17..35]) if the block is mixed, or bands 6..12 (scalefacs[18..36]) if 
        // not. Each band has a window of 3 with each scale factor being slen2 bits long.
        if slen2 > 0 {
            for sfb in n_sfb..(n_sfb + (6 * 3)) {
                data.scalefacs[sfb] = bs.read_bits_leq32(slen2)? as u8;
            }
            bits_read += 6 * 3 * slen2 as usize;
        }
    }
    // Normal (long, start, end) windows...
    else {
        // For normal windows there are 21 scale factor bands. These bands are divivided into four 
        // band ranges. Scale factors in the first two band ranges: [0..6], [6..11], have scale 
        // factors that are slen1 bits long, while the last two band ranges: [11..16], [16..21] have
        // scale factors that are slen2 bits long.
        const SCALE_FACTOR_BANDS: [(usize, usize); 4] = [(0, 6), (6, 11), (11, 16), (16, 21)];

        for (i, (start, end)) in SCALE_FACTOR_BANDS.iter().enumerate() {
            let slen = if i < 2 { slen1 } else { slen2 };

            // Scale factors are already zeroed out, so don't do anything if slen is 0.
            if slen > 0 {
                // The scale factor selection information for this channel indicates that the scale
                // factors should be copied from granule 0 for this channel.
                if gr > 0 && side_info.scfsi[gr][i] {
                    let (granule0, granule1) = main_data.granules.split_first_mut().unwrap();

                    granule1[0].channels[ch].scalefacs[*start..*end]
                        .copy_from_slice(&granule0.channels[ch].scalefacs[*start..*end]);
                }
                // Otherwise, read the scale factors from the bitstream.
                else {
                    for sfb in *start..*end { 
                        main_data.granules[gr].channels[ch].scalefacs[sfb] = 
                            bs.read_bits_leq32(slen)? as u8;
                    }
                    bits_read += slen as usize * (end - start);
                }
            }
        }
    }

    Ok(bits_read)
}

/// Reads the scale factors for a single channel in a granule in a MPEG version 2 frame.
fn l3_read_scale_factors_lsf<B: BitStream>(
    bs: &mut B, 
    is_intensity_stereo: bool,
    side_info: &GranuleChannelSideInfoL3,
    channel: &mut MainDataGranuleChannel, 
) -> Result<(usize)> {

    let mut bits_read = 0;

    let bi = match side_info.block_type {
        BlockType::Short{ is_mixed: true  } => 2,
        BlockType::Short{ is_mixed: false } => 1,
        _                                   => 0,
    };

    let (slen_table, nsfb_table) = if is_intensity_stereo {
        let sfc = side_info.scalefac_compress as u32 >> 1;

        match sfc {
            0..=179   => ([
                (sfc / 36),
                (sfc % 36) / 6,
                (sfc % 36) % 6,
                0,
            ], 
            &SCALE_FACTOR_NSFB[0][bi]),
            180..=243 => ([
                ((sfc - 180) % 64) >> 4,
                ((sfc - 180) % 16) >> 2,
                ((sfc - 180) %  4),
                0,
            ], 
            &SCALE_FACTOR_NSFB[1][bi]),
            244..=255 => ([
                (sfc - 244) / 3,
                (sfc - 244) % 3,
                0,
                0,
            ], 
            &SCALE_FACTOR_NSFB[2][bi]),
            _ => unreachable!(),
        }
    }
    else {
        let sfc = side_info.scalefac_compress as u32;

        match sfc {
            0..=399   => ([
                (sfc >> 4) / 5, 
                (sfc >> 4) % 5, 
                (sfc % 16) >> 2, 
                (sfc %  4)
            ], 
            &SCALE_FACTOR_NSFB[3][bi]),
            400..=499 => ([
                ((sfc - 400) >> 2) / 5,
                ((sfc - 400) >> 2) % 5,
                (sfc - 400) % 4,
                0,
            ], 
            &SCALE_FACTOR_NSFB[4][bi]),
            500..=512 => ([
                (sfc - 500) / 3,
                (sfc - 500) % 3,
                0,
                0,
            ], 
            &SCALE_FACTOR_NSFB[5][bi]),
            _ => unreachable!(),
        }
    };

    let mut start = 0;

    for (&slen, &n_sfb) in slen_table.iter().zip(nsfb_table.iter()) {
        // TODO: A maximum value indicates an invalid position for Intensity Stereo. Deal with this
        // here? (ISO-13818-3 part 2.4.3.2)
        for sfb in start..(start + n_sfb) {
           channel.scalefacs[sfb] = bs.read_bits_leq32(slen)? as u8;
        }

        start += n_sfb;
        bits_read += n_sfb * slen as usize;
    }

    Ok(bits_read)
}



fn l3_read_huffman_samples<B: BitStream>(
    bs: &mut B,
    header: &FrameHeader,
    side_info: &GranuleChannelSideInfoL3,
    buf: &mut [f32; 576]
) -> Result<()> {

    let (region1_start, region2_start) = match side_info.block_type {
        BlockType::Short { is_mixed } => (36, 576),
        _ => {
            let region1_start_idx = side_info.region0_count as usize + 1;
            let region2_start_idx = side_info.region1_count as usize + region1_start_idx + 1;

            (
                SCALE_FACTOR_LONG_BANDS[header.sample_rate_idx][region1_start_idx],
                SCALE_FACTOR_LONG_BANDS[header.sample_rate_idx][region2_start_idx]
            )
        }
    };


    

    Ok(())
}

fn l3_read_main_data<B: BitStream>(
    bs: &mut B, 
    header: &FrameHeader, 
    side_info: &SideInfoL3,
) -> Result<MainData> {

    let mut main_data: MainData = Default::default();

    for gr in 0..header.n_granules() {
        for ch in 0..header.n_channels() {
            
            // Read the scale factors (part2) and get the number of bits read. For MPEG version 1...
            let part2_bits = if header.is_mpeg1() {
                l3_read_scale_factors(bs, gr, ch, side_info, &mut main_data)
            }
            // For MPEG version 2...
            else {
                l3_read_scale_factors_lsf(
                    bs, 
                    ch > 0 && header.is_intensity_stereo(), 
                    &side_info.granules[gr].channels[ch], 
                    &mut main_data.granules[gr].channels[ch])
            }?;

            // The Huffman code length (part3)
            let part3_bits = side_info.granules[gr].channels[ch].part2_3_length as usize 
                - part2_bits;

            eprintln!("part2_bits={}, part3_bits={}", part2_bits, part3_bits);

            let mut samples = [0f32; 576];

            l3_read_huffman_samples(
                bs, 
                header, 
                &side_info.granules[gr].channels[ch], 
                &mut samples
                )?;

        }
    }


    Ok(main_data)
}

/// `BitResevoir` implements the bit resevoir mechanism for main_data. Since frames have a 
/// deterministic length based on the bit-rate, low-complexity portions of the audio may not need
/// every byte allocated to the frame. The bit resevoir mechanism allows these unused portions of 
/// frames to be used by future frames.
struct BitResevoir {
    buf: Box<[u8]>,
    len: usize,
}

impl BitResevoir {
    fn new() -> Self {
        BitResevoir {
            buf: vec![0u8; 2048].into_boxed_slice(),
            len: 0,
        }
    }

    fn fill<B: Bytestream>(
        &mut self, 
        reader: &mut B,
        main_data_begin: usize, 
        main_data_size: usize) -> Result<()> 
    {
        // The value `main_data_begin` indicates the number of bytes from the previous frames to 
        // reuse. It must be less than or equal to the amount of bytes in the buffer.
        if main_data_begin > self.len {
            return decode_error("Invalid main_data_begin offset.");
        }

        // Shift the reused bytes to the beginning of the resevoir.
        // TODO: For Rust 1.37, use copy_within() for more efficient overlapping copies.
        // self.buf.copy_within(self.len - main_data_begin..self.len, 0);
        let prev = self.len - main_data_begin;
        for i in 0..main_data_begin {
            self.buf[i] = self.buf[prev + i];
        }
        self.len = main_data_begin;

        // Read the remaining amount of bytes.
        let read_len = main_data_size - main_data_begin;
        reader.read_buf_bytes(&mut self.buf[self.len..self.len + read_len])?;
        self.len += read_len;
        Ok(())
    }

    fn bytes_ref(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

pub struct Mp3Decoder<B: Bytestream> {
    reader: B,
    resevoir: BitResevoir,
}

impl<B: Bytestream> Mp3Decoder<B> {

    pub fn new(reader: B) -> Self {
        Mp3Decoder {
            reader,
            resevoir: BitResevoir::new(),
        }
    }

    pub fn read_frame(&mut self) -> Result<()> {
        let header = read_frame_header(&mut self.reader)?;
        eprintln!("{:#?}", &header);


        match header.layer {
            MpegLayer::Layer3 => {
                // Read the side information.
                let side_info = l3_read_side_info(&mut self.reader, &header)?;
                eprintln!("{:#?}", &side_info);

                // Buffer main_data into the bit resevoir.
                self.resevoir.fill(
                    &mut self.reader, 
                    side_info.main_data_begin as usize,
                    header.frame_size - side_info.size)?;

                // Read the main_data from the bit resevoir.
                {
                    let mut bs = BitStreamLtr::new(BufStream::new(self.resevoir.bytes_ref()));

                    let main_data = l3_read_main_data(&mut bs, &header, &side_info)?;
                }

            },
            _ => return unsupported_error("Unsupported MPEG Layer."),
        }

        Ok(())
    }

}
