// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use sonata_core::errors::{Result, decode_error, unsupported_error};
use sonata_core::io::{BufStream, BitStream, BitStreamLtr, Bytestream};

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

static SCALEFAC_SIZES: [(u8, u8); 16] = 
[
    (0, 0), (0, 1), (0, 2), (0, 3), (3, 0), (1, 1), (1, 2), (1, 3), 
    (2, 1), (2, 2), (2, 3), (3, 1), (3, 2), (3, 3), (4, 2), (4, 3),
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
    /// of bands is [band..32].
    Intensity { band: u32 },
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
    /// 50/15 ms
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
    channels: Channels,
    emphasis: Emphasis,
    is_copyrighted: bool,
    is_original: bool,
    has_padding: bool,
    crc: Option<u16>,
    frame_size: usize,
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
    /// Mapping of array indicies to bands [1-5, 6-10, 11-15, 16-20].
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

    fn size(version: MpegVersion, channels: Channels) -> usize {
        // MPEG version 2 & 2.5, one channel.
        if version != MpegVersion::Mpeg1 && channels == Channels::Mono {
            9
        }
        // MPEG version 1, two channel.
        else if version == MpegVersion::Mpeg1 && channels != Channels::Mono {
            32
        }
        // MPEG version 2 & 2.5, two channel OR MPEG version 1, one channel.
        else {
            17
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
    // Scale factor bands 0-5 from scale factor table for long blocks, and bands 3-11 from
    // scale factor table for short blocks.
    Mixed,
    Short,
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
    // Number of bits used for scale factors.
    scalefac_compress: u16,
    /// Indicates the type of window for the granule.
    block_type: BlockType,
    /// Indicates different windows are used for lower and higher frequencies.
    mixed_block: bool,

    subblock_gain: [f32; 3],

    table_select: [u8; 3],
    region0_count: u8,
    region1_count: u8,

    preflag: bool,
    scalefac_scale: bool,
    count1table_select: bool,
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

    let sample_rate = match ((header & 0xc00) >> 10, version) {
        (0b00, MpegVersion::Mpeg1)   => 44_100,
        (0b00, MpegVersion::Mpeg2)   => 22_050,
        (0b00, MpegVersion::Mpeg2p5) => 11_025,
        (0b01, MpegVersion::Mpeg1)   => 48_000,
        (0b01, MpegVersion::Mpeg2)   => 24_000,
        (0b01, MpegVersion::Mpeg2p5) => 12_000,
        (0b10, MpegVersion::Mpeg1)   => 32_000,
        (0b10, MpegVersion::Mpeg2)   => 16_000,
        (0b10, MpegVersion::Mpeg2p5) =>  8_000,
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
            band: (1 + (header & 0x30) >> 4) << 2,
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
    header: &FrameHeader) -> Result<()>
{
    granule.part2_3_length = bs.read_bits_leq32(12)? as u16;
    granule.big_values = bs.read_bits_leq32(9)? as u16;

    if granule.big_values > 288 {
        return decode_error("Granule big_values > 288.");
    }

    granule.global_gain = bs.read_bits_leq32(8)? as u16;

    granule.scalefac_compress = match header.version {
        MpegVersion::Mpeg1 => bs.read_bits_leq32(4),
        _                  => bs.read_bits_leq32(9),
    }? as u16;

    let window_switching = bs.read_bit()?;

    if window_switching {
        let block_type_enc = bs.read_bits_leq32(2)?;

        granule.mixed_block = bs.read_bit()?;

        granule.block_type = match block_type_enc {
            0b00 => return decode_error("Invalid block_type."),
            0b01 => BlockType::Start,
            0b10 => if granule.mixed_block { BlockType::Mixed } else { BlockType::Short },
            0b11 => BlockType::End,
            _ => unreachable!(),
        };

        for i in 0..2 {
            granule.table_select[i] = bs.read_bits_leq32(5)? as u8;
        }

        for i in 0..3 {
            granule.subblock_gain[i] = bs.read_bits_leq32(3)? as f32;
        }

        granule.region0_count = match granule.block_type {
            BlockType::Short => 8,
            _                => 7,
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

    granule.preflag = match header.version {
        MpegVersion::Mpeg1 => bs.read_bit()?,
        _                  => granule.scalefac_compress >= 500,
    };

    granule.scalefac_scale = bs.read_bit()?;
    granule.count1table_select = bs.read_bit()?;

    Ok(())
}

fn read_granule_side_info_l3<B: BitStream>(
    bs: &mut B, 
    granules: &mut GranuleSideInfoL3, 
    header: &FrameHeader) -> Result<()>
{
    for channel_granule in &mut granules.channels[..header.channels.count()] {
        read_granule_channel_side_info_l3(bs, channel_granule, header)?;
    }
    Ok(())
}

fn read_audio_data_l3<B: Bytestream>(reader: &mut B, header: &FrameHeader) -> Result<SideInfoL3> {
    let mut side_info: SideInfoL3 = Default::default();

    let mut bs = BitStreamLtr::new(reader);

    // For MPEG version 1...
    if header.version == MpegVersion::Mpeg1 {
        // First 9 bits is main_data_begin.
        side_info.main_data_begin = bs.read_bits_leq32(9)? as u16;

        // Next 3 (>1 channel) or 5 (1 channel) bits are private and should be ignored.
        match header.channels {
            Channels::Mono => bs.ignore_bits(5)?,
            _              => bs.ignore_bits(3)?,
        };

        // Next four (or 8, if more than one channel) are the SCFSI bits.
        for scfsi in &mut side_info.scfsi[..header.channels.count()] {
            scfsi[0] = bs.read_bit()?;
            scfsi[1] = bs.read_bit()?;
            scfsi[2] = bs.read_bit()?;
            scfsi[3] = bs.read_bit()?;
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

/// `BitResevoir` implements the bit resevoir mechanism for main_data. Since frames have a 
/// deterministic length based on the bit-rate, low-complexity portions of the audio may not need
/// every byte allocated to the frame. The bit resevoir mechanism allows these unused portions of 
/// frames to be used by future frames.
pub struct BitResevoir {
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
                let side_info = read_audio_data_l3(&mut self.reader, &header)?;

                // Buffer main_data into the bit resevoir.
                self.resevoir.fill(
                    &mut self.reader, 
                    side_info.main_data_begin as usize,
                    header.frame_size - side_info.size)?;

                // Read the main_data from the bit resevoir.
                {
                    let bs = BitStreamLtr::new(BufStream::new(self.resevoir.bytes_ref()));
                }

                eprintln!("{:#?}", &side_info);
            },
            _ => return unsupported_error("Unsupported MPEG Layer."),
        }

        Ok(())
    }

}
