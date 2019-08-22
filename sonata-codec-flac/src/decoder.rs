// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::cmp;
use std::num::Wrapping;

use sonata_core::audio::{AudioBuffer, AudioBufferRef, AsAudioBufferRef};
use sonata_core::audio::{Duration, Signal, SignalSpec};
use sonata_core::checksum::{Crc8, Crc16};
use sonata_core::codecs::{CODEC_TYPE_FLAC, CodecParameters, CodecDescriptor};
use sonata_core::codecs::{Decoder, DecoderOptions};
use sonata_core::errors::{Result, decode_error, unsupported_error};
use sonata_core::formats::Packet;
use sonata_core::io::*;
use sonata_core::util::bits::sign_extend_leq32_to_i32;
use sonata_core::support_codec;

use crate::validate::Md5AudioValidator;

#[derive(Debug)]
enum BlockingStrategy {
    Fixed,
    Variable
}

#[derive(Debug)]
enum BlockSequence {
    BySample(u64),
    ByFrame(u32)
}

/// `ChannelAssignment` describes the mapping between the samples decoded from a subframe and the
/// channel those samples belong to. It is also through the `ChannelAssignment` that the decoder is
/// instructed on how to decorrelate stereo channels.
//
/// For LeftSide or RightSide channel assignments, one channel is stored independantly while the
/// other stores a difference. The Difference is always stored as Left - Right. For the MidSide
/// channel assignment, no channels are stored independantly, rather, a Mid (average) channel and a
/// Difference channel are stored.
#[derive(Debug)]
enum ChannelAssignment {
    /// All channels are independantly coded and no decorrelation step is required.
    Independant(u32),
    /// Channel 0 is the Left channel, and channel 1 is a Difference channel. The Right channel
    /// is restored by subtracting the Difference channel from the Left channel (R = L - D).
    LeftSide,
    /// Channel 0 is the Mid channel (Left/2 + Right/2), and channel 1 is the Difference channel
    /// (Left - Right). Therefore, if M = L/2 + R/2 and D = L - R, solving for L and R the left
    /// and right channels are: L = S/2 + M, and R = M - S/2.
    MidSide,
    /// Channel 0 is the Difference channel, and channel 1 is the Right channel. The Left channel
    /// is restored by adding the Difference channel to the Right channel (L = R + D).
    RightSide
}

fn decorrelate_left_side(left: &[i32], side: &mut [i32]) {
    for (s, l) in side.iter_mut().zip(left) {
        *s = *l - *s;
    }
}

fn decorrelate_mid_side(mid: &mut [i32], side: &mut [i32]) {
    for (m, s) in mid.iter_mut().zip(side) {
        // Mid (M) is given as M = L/2 + R/2, while Side (S) is given as S = L - R.
        //
        // To calculate the individual channels, the following equations can be used:
        //      - L = S/2 + M
        //      - R = M - S/2
        //
        // Ideally, this would work, but since samples are represented as integers, division yields
        // the floor of the divided value. Therefore, the channel restoration equations actually
        // yield:
        //      - L = floor(S/2) + M
        //      - R = M - floor(S/2)
        //
        // This will produce incorrect samples whenever the sample S is odd. For example:
        //      - 2/2 = 1
        //      - 3/2 = 1 (should be 2 if rounded!)
        //
        // To get the proper rounding behaviour, the solution is to add one to the result if S is
        // odd:
        //      - L = floor(S/2) + M + (S%2) = M + (S%2) + floor(S/2)
        //      - R = M - floor(S/2) + (S%2) = M + (S%2) - floor(S/2)
        //
        // Further, to prevent loss of accuracy, instead of dividing S/2 and adding or subtracting
        // it from M, multiply M*2, then add or subtract S, and then divide the whole result by 2.
        // This gives one extra bit of precision for the intermediate computations.
        //
        // Conveniently, since M should be doubled, the LSB will always be 0. This allows S%2 to
        // be added simply by bitwise ORing S&1 to M<<1.
        //
        // Therefore the final equations yield:
        //      - L = (2*M + (S%2) + S) / 2
        //      - R = (2*M + (S%2) - S) / 2
        let mid = (*m << 1) | (*s & 1);
        let side = *s;
        *m = (mid + side) >> 1;
        *s = (mid - side) >> 1;
    }
}

fn decorrelate_right_side(right: &[i32], side: &mut [i32]) {
    for (s, r) in side.iter_mut().zip(right) {
        *s += *r;
    }
}

struct FrameHeader {
    block_sequence: BlockSequence,
    block_num_samples: u16,
    channel_assignment: ChannelAssignment,
    bits_per_sample: Option<u32>,
}

pub struct ParsedPacket {
    /// The timestamp of the first audio frame in the packet.
    pub packet_ts: u64,
    /// The number of audio frames in the packet.
    pub n_frames: u32,
    // The number of bytes of the packet that were consumed while parsing.
    pub parsed_len: usize,
}

pub struct PacketParser;

impl PacketParser {

    pub fn parse_packet(reader: &mut MediaSourceStream) -> Result<ParsedPacket> {
        let mut byte_offset;

        let header = loop {
            let sync = sync_frame(reader)?;

            byte_offset = reader.pos() - 2;

            if let Ok(header) = read_frame_header(reader, sync) {
                break header
            }
        };

        let packet_ts = match header.block_sequence {
            BlockSequence::ByFrame(seq) => u64::from(seq) * u64::from(header.block_num_samples),
            BlockSequence::BySample(seq) => seq,
        };

        Ok(ParsedPacket {
            packet_ts,
            n_frames: u32::from(header.block_num_samples),
            parsed_len: (reader.pos() - byte_offset) as usize,
        })
    }
}

/// `FlacDecoder` implements a decoder for the FLAC codec bitstream. The decoder is compatible with
/// OGG encapsulated FLAC.
pub struct FlacDecoder {
    params: CodecParameters,
    is_validating: bool,
    validator: Md5AudioValidator,
    buf: AudioBuffer<i32>,
}

impl Decoder for FlacDecoder {

    fn try_new(params: &CodecParameters, options: &DecoderOptions) -> Result<Self> {
        // Initialize the AudioBuffer.
        //
        // TODO: Some of the required parameters are not necessarily provided in the StreamInfo
        // block, however, it is possible to get all the required parameters using from the packet.
        // Consider supporting this.
        let frames = match params.max_frames_per_packet {
            Some(frames) => frames,
            None => return unsupported_error("Variable frames per packet are unsupported."),
        };

        let spec = {
            let sample_rate = match params.sample_rate {
                Some(rate) => rate,
                None       => return unsupported_error("Variable sample rate is unsupported."),
            };

            let channels = match params.channels {
                Some(channels) => channels,
                None           => return unsupported_error("Dynamic channels are unsupported."),
            };

            SignalSpec::new(sample_rate, channels)
        };

        Ok(FlacDecoder {
            params: params.clone(),
            is_validating: options.verify,
            validator: Md5AudioValidator::new(),
            buf: AudioBuffer::new(Duration::Frames(frames), &spec),
        })
    }

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[ support_codec!(CODEC_TYPE_FLAC, "flac", "Free Lossless Audio Codec") ]
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: Packet<'_>) -> Result<AudioBufferRef<'_>> {
        let mut reader = packet.into_stream();

        // Synchronize to a frame and get the synchronization code.
        let sync = sync_frame(&mut reader)?;

        // The entire frame is checksummed with a CRC16, wrap the main reader in a CRC16 error
        // detection stream. Include the sync code in the CRC.
        let mut crc16 = Crc16::new();
        crc16.process_buf_bytes(&sync.to_be_bytes());

        let mut reader_crc16 = MonitorStream::new(reader, crc16);

        let header = read_frame_header(&mut reader_crc16, sync)?;

        // Use the bits per sample and sample rate as stated in the frame header, falling back to
        // the stream information if provided. If neither are available, return an error.
        let bits_per_sample = if let Some(bps) = header.bits_per_sample { bps }
                              else if let Some(bps) = self.params.bits_per_sample { bps }
                              else {
                                  return decode_error("Bits per sample not provided.");
                              };

        // eprintln!("Frame: [{:?}] strategy={:?}, n_samples={}, bps={}, channels={:?}",
        //     header.block_sequence,
        //     header.blocking_strategy,
        //     header.block_num_samples,
        //     bits_per_sample,
        //     &header.channel_assignment);

        // Reserve a writeable chunk in the buffer equal to the number of samples in the block.
        self.buf.clear();
        self.buf.render_reserved(Some(header.block_num_samples as usize));

        // Only Bitstream reading for subframes.
        {
            // Sub-frames don't have any byte-aligned content, so use a BitReader.
            let mut bs = BitStreamLtr::new(&mut reader_crc16);

            // Read each subframe based on the channel assignment into a planar buffer.
            match header.channel_assignment {
                ChannelAssignment::Independant(channels) => {
                    for i in 0..channels as u8 {
                        read_subframe(&mut bs, bits_per_sample, self.buf.chan_mut(i))?;
                    }
                },
                // For Left/Side, Mid/Side, and Right/Side channel configurations, the Side
                // (Difference) channel requires an extra bit per sample.
                ChannelAssignment::LeftSide => {
                    let (mut left, mut side) = self.buf.chan_pair_mut(0, 1);

                    read_subframe(&mut bs, bits_per_sample, &mut left)?;
                    read_subframe(&mut bs, bits_per_sample + 1, &mut side)?;

                    decorrelate_left_side(&left, &mut side);
                },
                ChannelAssignment::MidSide => {
                    let (mut mid, mut side) = self.buf.chan_pair_mut(0, 1);

                    read_subframe(&mut bs, bits_per_sample, &mut mid)?;
                    read_subframe(&mut bs, bits_per_sample + 1, &mut side)?;

                    decorrelate_mid_side(&mut mid, &mut side);
                },
                ChannelAssignment::RightSide => {
                    let (mut side, mut right) = self.buf.chan_pair_mut(0, 1);

                    read_subframe(&mut bs, bits_per_sample + 1, &mut side)?;
                    read_subframe(&mut bs, bits_per_sample, &mut right)?;

                    decorrelate_right_side(&right, &mut side);
                }
            }
        }

        // Feed the validator if validation is enabled.
        if self.is_validating {
            self.validator.update(&self.buf, bits_per_sample);
        }

        // The decoder uses a 32bit sample format as a common denominator, but that doesn't mean
        // the encoded audio samples are actually 32bit. Shift all samples in the output buffer
        // so that regardless the encoded bits/sample, the output is always 32bits/sample.
        if bits_per_sample < 32 {
            let shift = 32 - bits_per_sample;
            self.buf.transform(| sample | sample << shift);
        }

        // Retrieve the CRC16 before the reading the footer.
        let crc16_expected = reader_crc16.monitor().crc();
        let crc16_computed = read_frame_footer(&mut reader_crc16.to_inner())?;

        if crc16_computed != crc16_expected {
            return decode_error("Computed frame CRC does not match expected CRC.");
        }

        Ok(self.buf.as_audio_buffer_ref())
    }

    fn close(&mut self) {
        if self.is_validating {
            eprintln!("{:?}", self.validator.finalize());
        }
    }

}

fn sync_frame<B: Bytestream>(reader: &mut B) -> Result<u16> {
    let mut sync = 0u16;

    // Synchronize stream to Frame Header. FLAC specifies a byte-aligned 14 bit sync code of
    // `0b11_1111_1111_1110`. This would be difficult to find on its own. Expand the search to
    // a 16-bit field of `0b1111_1111_1111_10xx` and search a word at a time.
    while (sync & 0xfffc) != 0xfff8 {
        sync = sync.wrapping_shl(8) | u16::from(reader.read_u8()?);
    }

    Ok(sync)
}

fn read_frame_header<B: Bytestream>(reader: &mut B, sync: u16) -> Result<FrameHeader> {

    // The header is checksummed with a CRC8 hash. Include the sync code in this CRC.
    let mut crc8 = Crc8::new();
    crc8.process_buf_bytes(&sync.to_be_bytes());

    let mut reader_crc8 = MonitorStream::new(reader, crc8);

    // Extract the blocking strategy from the expanded synchronization code.
    let blocking_strategy = match sync & 0x1 {
        0 => BlockingStrategy::Fixed,
        _ => BlockingStrategy::Variable
    };

    // Read all the standard frame description fields as one 16-bit value and extract the
    // fields.
    let desc = reader_crc8.read_be_u16()?;

    let block_size_enc      = u32::from((desc & 0xf000) >> 12);
    let sample_rate_enc     = u32::from((desc & 0x0f00) >>  8);
    let channels_enc        = u32::from((desc & 0x00f0) >>  4);
    let bits_per_sample_enc = u32::from((desc & 0x000e) >>  1);

    if (desc & 0x0001) == 1 {
        return decode_error("Frame header reserved bit is not set to mandatory value.");
    }

    let block_sequence = match blocking_strategy {
        // Fixed-blocksize stream sequence blocks by a frame number.
        BlockingStrategy::Fixed => {
            let frame = match utf8_decode_be_u64(&mut reader_crc8)? {
                Some(frame) => frame,
                None => return decode_error("Frame sequence number is not valid."),
            };

            // The frame number should only be 31-bits. Since it is UTF8 encoded, the actual length
            // cannot be enforced by the decoder. Return an error if the frame number exceeds the
            // maximum 31-bit value.
            if frame > 0x7fff_ffff {
                return decode_error("Frame sequence number exceeds 31-bits.");
            }

            BlockSequence::ByFrame(frame as u32)
        },
        // Variable-blocksize streams sequence blocks by a sample number.
        BlockingStrategy::Variable => {
            let sample = match utf8_decode_be_u64(&mut reader_crc8)? {
                Some(sample) => sample,
                None => return decode_error("Frame sequence number is not valid."),
            };

            // The sample number should only be 36-bits. Since it is UTF8 encoded, the actual length
            // cannot be enforced by the decoder. Return an error if the frame number exceeds the
            // maximum 36-bit value.
            if sample > 0xffff_fffff {
                return decode_error("Sample sequence number exceeds 36-bits");
            }

            BlockSequence::BySample(sample)
        }
    };

    let block_num_samples = match block_size_enc {
        0x1       => 192,
        0x2..=0x5 => 576 * (1 << (block_size_enc - 2)),
        0x6       => u16::from(reader_crc8.read_u8()?) + 1,
        0x7       => {
            let block_size = reader_crc8.read_be_u16()?;
            if block_size == 0xffff {
                return decode_error("Block size not allowed to be greater than 65535.");
            }
            block_size + 1
        },
        0x8..=0xf => 256 * (1 << (block_size_enc - 8)),
        _         => {
            return decode_error("Block size set to reserved value.");
        }
    };

    let sample_rate = match sample_rate_enc {
        0x0 => None, // Get from StreamInfo if possible.
        0x1 => Some( 88_200),
        0x2 => Some(176_400),
        0x3 => Some(192_000),
        0x4 => Some(  8_000),
        0x5 => Some( 16_000),
        0x6 => Some( 22_050),
        0x7 => Some( 24_000),
        0x8 => Some( 32_000),
        0x9 => Some( 44_100),
        0xa => Some( 48_000),
        0xb => Some( 96_000),
        0xc => Some(u32::from(reader_crc8.read_u8()?)),
        0xd => Some(u32::from(reader_crc8.read_be_u16()?)),
        0xe => Some(u32::from(reader_crc8.read_be_u16()?) * 10),
        _   => {
            return decode_error("Sample rate set to reserved value.");
        }
    };

    if let Some(rate) = sample_rate {
        if rate < 1 || rate > 655_350 {
            return decode_error("Sample rate out of bounds.");
        }
    }

    let bits_per_sample = match bits_per_sample_enc {
        0x0 => None, // Get from StreamInfo if possible.
        0x1 => Some( 8),
        0x2 => Some(12),
        0x4 => Some(16),
        0x5 => Some(20),
        0x6 => Some(24),
        _   => {
            return decode_error("Bits per sample set to reserved value.");
        }
    };

    let channel_assignment = match channels_enc {
        0x0..=0x7 => ChannelAssignment::Independant(channels_enc + 1),
        0x8       => ChannelAssignment::LeftSide,
        0x9       => ChannelAssignment::RightSide,
        0xa       => ChannelAssignment::MidSide,
        _ => {
            return decode_error("Channel assignment set to reserved value.");
        }
    };

    // End of freame header, pop off CRC8 checksum.
    let crc8_computed = reader_crc8.monitor().crc();

    // Get expected CRC8 checksum from the header.
    let crc8_expected = reader_crc8.to_inner().read_u8()?;

    if crc8_expected != crc8_computed {
        return decode_error("Computed frame header CRC does not match expected CRC.");
    }

    Ok(FrameHeader {
        block_sequence,
        block_num_samples,
        channel_assignment,
        bits_per_sample,
    })
}

fn read_frame_footer<B: Bytestream>(reader: &mut B) -> Result<u16> {
    Ok(reader.read_be_u16()?)
}


// Subframe business



#[derive(Debug)]
enum SubFrameType {
    Constant,
    Verbatim,
    FixedLinear(u32),
    Linear(u32),
}

fn read_subframe<B: BitStream>(bs: &mut B, frame_bps: u32, buf: &mut [i32]) -> Result<()> {

    // First sub-frame bit must always 0.
    if bs.read_bit()? {
        return decode_error("Subframe padding is not 0.");
    }

    // Next 6 bits designate the sub-frame type.
    let subframe_type_enc = bs.read_bits_leq32(6)?;

    let subframe_type = match subframe_type_enc {
        0x00        => SubFrameType::Constant,
        0x01        => SubFrameType::Verbatim,
        0x08..=0x0f => {
            let order = subframe_type_enc & 0x07;
            // The Fixed Predictor only supports orders between 0 and 4.
            if order > 4 {
                return decode_error("Fixed predictor orders of greater than 4 are invalid.");
            }
            SubFrameType::FixedLinear(order)
        },
        0x20..=0x3f => SubFrameType::Linear((subframe_type_enc & 0x1f) + 1),
        _ => {
            return decode_error("Subframe type set to reserved value.");
        }
    };

    // Bit 7 of the sub-frame header designates if there are any dropped (wasted in FLAC terms)
    // bits per sample in the audio sub-block. If the bit is set, unary decode the number of
    // dropped bits per sample.
    let dropped_bps = if bs.read_bit()? {
        bs.read_unary()? + 1
    }
    else {
        0
    };

    // The bits per sample stated in the frame header is for the decoded audio sub-block samples.
    // However, it is likely that the lower order bits of all the samples are simply 0. Therefore,
    // the encoder will truncate `dropped_bps` of lower order bits for every sample in a sub-block.
    // The decoder simply needs to shift left all samples by `dropped_bps` after decoding the
    // sub-frame and obtaining the truncated audio sub-block samples.
    let bps = frame_bps - dropped_bps;

    // eprintln!("\tSubframe: type={:?}, bps={}, dropped_bps={}",
    //     &subframe_type,
    //     bps,
    //     dropped_bps);

    match subframe_type {
        SubFrameType::Constant           => decode_constant(bs, bps, buf)?,
        SubFrameType::Verbatim           => decode_verbatim(bs, bps, buf)?,
        SubFrameType::FixedLinear(order) => decode_fixed_linear(bs, bps, order as u32, buf)?,
        SubFrameType::Linear(order)      => decode_linear(bs, bps, order as u32, buf)?,
    };

    // Shift the samples to account for the dropped bits.
    samples_shl(dropped_bps, buf);

    Ok(())
}

#[inline(always)]
fn samples_shl(shift: u32, buf: &mut [i32]) {
    if shift > 0 {
        for sample in buf.iter_mut() {
            *sample = sample.wrapping_shl(shift);
        }
    }
}

fn decode_constant<B: BitStream>(bs: &mut B, bps: u32, buf: &mut [i32]) -> Result<()> {
    let const_sample = sign_extend_leq32_to_i32(bs.read_bits_leq32(bps)?, bps);

    for sample in buf.iter_mut() {
        *sample = const_sample;
    }

    Ok(())
}

fn decode_verbatim<B: BitStream>(bs: &mut B, bps: u32, buf: &mut [i32]) -> Result<()> {
    for sample in buf.iter_mut() {
        *sample = sign_extend_leq32_to_i32(bs.read_bits_leq32(bps)?, bps);
    }

    Ok(())
}

fn decode_fixed_linear<B: BitStream>(
    bs: &mut B,
    bps: u32,
    order: u32,
    buf: &mut [i32]
) -> Result<()> {
    // The first `order` samples are encoded verbatim to warm-up the LPC decoder.
    decode_verbatim(bs, bps, &mut buf[..order as usize])?;

    // Decode the residuals for the predicted samples.
    decode_residual(bs, order, buf)?;

    // Run the Fixed predictor (appends to residuals).
    //
    // TODO: The fixed predictor uses 64-bit accumulators by default to support bps > 26. On 64-bit
    // machines, this is preferable, but on 32-bit machines if bps <= 26, run a 32-bit predictor,
    // and fallback to the 64-bit predictor if necessary (which is basically never).
    fixed_predict(order, buf)?;

    Ok(())
}

fn decode_linear<B: BitStream>(bs: &mut B, bps: u32, order: u32, buf: &mut [i32]) -> Result<()> {
    // The order of the Linear Predictor should be between 1 and 32.
    debug_assert!(order > 0 && order <= 32);

    // The first `order` samples are encoded verbatim to warm-up the LPC decoder.
    decode_verbatim(bs, bps, &mut buf[0..order as usize])?;

    // Quantized linear predictor (QLP) coefficients precision in bits.
    let qlp_precision = bs.read_bits_leq32(4)? + 1;
    if qlp_precision > 15 {
        return decode_error("QLP precision set to reserved value.");
    }

    // QLP coefficients bit shift [-16, 15].
    let qlp_coeff_shift = sign_extend_leq32_to_i32(bs.read_bits_leq32(5)?, 5);

    if qlp_coeff_shift >= 0 {
        // Pick the best sized linear predictor to use based on the order. Most if not all FLAC
        // streams apppear to have an order <= 12. Specializing a predictor for orders <= 6 and
        // <= 12 appears to give the best performance.
        //
        // TODO: Reduce code duplication here.
        if order <= 4 {
            let mut qlp_coeffs = [0i32; 4];

            for c in qlp_coeffs[4 - order as usize..4].iter_mut().rev() {
                *c = sign_extend_leq32_to_i32(bs.read_bits_leq32(qlp_precision)?, qlp_precision);
            }

            decode_residual(bs, order, buf)?;

            lpc_predict_4(order as usize, &qlp_coeffs, qlp_coeff_shift as u32, buf)?;
        }
        else if order <= 8 {
            let mut qlp_coeffs = [0i32; 8];

            for c in qlp_coeffs[8 - order as usize..8].iter_mut().rev() {
                *c = sign_extend_leq32_to_i32(bs.read_bits_leq32(qlp_precision)?, qlp_precision);
            }

            decode_residual(bs, order, buf)?;

            lpc_predict_8(order as usize, &qlp_coeffs, qlp_coeff_shift as u32, buf)?;
        }
        else if order <= 12 {
            let mut qlp_coeffs = [0i32; 12];

            for c in qlp_coeffs[12 - order as usize..12].iter_mut().rev() {
                *c = sign_extend_leq32_to_i32(bs.read_bits_leq32(qlp_precision)?, qlp_precision);
            }

            decode_residual(bs, order, buf)?;

            lpc_predict_12(order as usize, &qlp_coeffs, qlp_coeff_shift as u32, buf)?;
        }
        else {
            let mut qlp_coeffs = [0i32; 32];

            for c in qlp_coeffs[32 - order as usize..32].iter_mut().rev() {
                *c = sign_extend_leq32_to_i32(bs.read_bits_leq32(qlp_precision)?, qlp_precision);
            }

            decode_residual(bs, order, buf)?;

            lpc_predict_32(order as usize, &qlp_coeffs, qlp_coeff_shift as u32, buf)?;
        }
    }
    else {
        return unsupported_error("LPC shifts less than 0 are not supported.");
    }

    Ok(())
}

fn decode_residual<B: BitStream>(
    bs: &mut B,
    n_prelude_samples: u32,
    buf: &mut [i32]
) -> Result<()> {
    let method_enc = bs.read_bits_leq32(2)?;

    // The FLAC specification defines two residual coding methods: Rice and Rice2. The
    // only difference between the two is the bit width of the Rice parameter. Note the
    // bit width based on the residual encoding method and use the same code path for
    // both cases.
    let param_bit_width = match method_enc {
        0x0 => 4,
        0x1 => 5,
        _ => {
            return decode_error("Residual method set to reserved value.");
        }
    };

    // Read the partition order.
    let order = bs.read_bits_leq32(4)?;

    // The number of paritions is equal to 2^order.
    let n_partitions = 1usize << order;

    // In general, all partitions have the same number of samples such that the sum of all partition
    // lengths equal the block length. The number of samples in a partition can therefore be
    // calculated with block_size / 2^order *in general*. However, since there are warm-up samples
    // stored verbatim, the first partition has n_prelude_samples less samples. Likewise, if there
    // is only one partition, then it too has n_prelude_samples less samples.
    let n_partition_samples = buf.len() >> order;

    // The size of the first (and/or only) partition as per the specification is n_partition_samples
    // minus the number of warm-up samples (which is the predictor order). Ensure the number of
    // samples in these types of partitions cannot be negative.
    if n_prelude_samples as usize > n_partition_samples {
        return decode_error("Residual partition too small for given predictor order.");
    }

    // Ensure that the sum of all partition lengths equal the block size.
    if n_partitions * n_partition_samples != buf.len() {
        return decode_error("Block size is not same as encoded residual.");
    }

    // eprintln!("\t\tResidual: n_partitions={}, n_partition_samples={}, n_prelude_samples={}",
    //     n_partitions,
    //     n_partition_samples,
    //     n_prelude_samples);

    // Decode the first partition as it may have less than n_partition_samples samples.
    decode_rice_partition(
        bs,
        param_bit_width,
        &mut buf[n_prelude_samples as usize..n_partition_samples]
    )?;

    // Decode the remaining partitions.
    for buf_chunk in buf[n_partition_samples..].chunks_mut(n_partition_samples) {
        decode_rice_partition(bs, param_bit_width, buf_chunk)?;
    }

    Ok(())
}

fn decode_rice_partition<B: BitStream>(
    bs: &mut B,
    param_bit_width: u32,
    buf: &mut [i32]
) -> Result<()> {
    // Read the encoding parameter, generally the Rice parameter.
    let rice_param = bs.read_bits_leq32(param_bit_width)?;

    // If the Rice parameter is all 1s (e.g., 0xf for a 4bit parameter, 0x1f for a 5bit parameter),
    // then it indicates that residuals in this partition are not Rice encoded, rather they are
    // binary encoded. Conversely, if the parameter is less than this value, the residuals are Rice
    // encoded.
    if rice_param < (1 << param_bit_width) - 1 {

        // println!("\t\t\tPartition (Rice): n_residuals={}, rice_param={}", buf.len(), rice_param);

        // Read each rice encoded residual and store in buffer.
        for sample in buf.iter_mut() {
            let q = bs.read_unary()?;
            let r = bs.read_bits_leq32(rice_param)?;
            *sample = rice_signed_to_i32((q << rice_param) | r);
        }
    }
    else {
        let residual_bits = bs.read_bits_leq32(5)?;

        // eprintln!(
        //     "\t\t\tPartition (Binary): n_residuals={}, residual_bits={}",
        //     buf.len(),
        //     residual_bits
        // );

        // Read each binary encoded residual and store in buffer.
        for sample in buf.iter_mut() {
            *sample = sign_extend_leq32_to_i32(bs.read_bits_leq32(residual_bits)?, residual_bits);
        }
    }

    Ok(())
}

#[inline(always)]
fn rice_signed_to_i32(word: u32) -> i32 {
    // Input  => 0  1  2  3  4  5  6  7  8  9  10
    // Output => 0 -1  1 -2  2 -3  3 -4  4 -5   5
    //
    //  - If even: output = input / 2
    //  - If odd:  output = -(input + 1) / 2
    //                    =  (input / 2) - 1

    // Divide the input by 2 and convert to signed.
    let div2 = (word >> 1) as i32;

    // Using the LSB of the input, create a new signed integer that's either
    // -1 (0b1111_11110) or 0 (0b0000_0000). For odd inputs, this will be -1, for even
    // inputs it'll be 0.
    let sign = -((word & 0x1) as i32);

    // XOR the div2 result with the sign. If sign is 0, the XOR produces div2. If sign is -1, then
    // -div2 - 1 is returned.
    //
    // Example:  input = 9 => div2 = 0b0000_0100, sign = 0b1111_11110
    //
    //           div2 ^ sign =   0b0000_0100
    //                         ^ 0b1111_1110
    //                           -----------
    //                           0b1111_1011  (-5)
    div2 ^ sign
}

#[test]
fn verify_rice_signed_to_i32() {
    assert_eq!(rice_signed_to_i32(0),  0);
    assert_eq!(rice_signed_to_i32(1), -1);
    assert_eq!(rice_signed_to_i32(2),  1);
    assert_eq!(rice_signed_to_i32(3), -2);
    assert_eq!(rice_signed_to_i32(4),  2);
    assert_eq!(rice_signed_to_i32(5), -3);
    assert_eq!(rice_signed_to_i32(6),  3);
    assert_eq!(rice_signed_to_i32(7), -4);
    assert_eq!(rice_signed_to_i32(8),  4);
    assert_eq!(rice_signed_to_i32(9), -5);
    assert_eq!(rice_signed_to_i32(10), 5);

    assert_eq!(rice_signed_to_i32(u32::max_value()), -2_147_483_648);
}


fn fixed_predict(order: u32, buf: &mut [i32]) -> Result<()> {
    debug_assert!(order <= 4);

    // The Fixed Predictor is just a hard-coded version of the Linear Predictor up to order 4 and
    // with fixed coefficients. Some cases may be simplified such as orders 0 and 1. For orders 2
    // through 4, use the same IIR-style algorithm as the Linear Predictor.
    match order {
        // A 0th order predictor always predicts 0, and therefore adds nothing to any of the samples
        // in buf. Do nothing.
        0 => (),
        // A 1st order predictor always returns the previous sample since the polynomial is:
        // s(i) = 1*s(i),
        1 => {
            for i in 1..buf.len() {
                buf[i] += buf[i - 1];
            }
        },
        // A 2nd order predictor uses the polynomial: s(i) = 2*s(i-1) - 1*s(i-2).
        2 => {
            for i in 2..buf.len() {
                let a = Wrapping(-1) * Wrapping(i64::from(buf[i - 2]));
                let b = Wrapping( 2) * Wrapping(i64::from(buf[i - 1]));
                buf[i] += (a + b).0 as i32;
            }
        },
        // A 3rd order predictor uses the polynomial: s(i) = 3*s(i-1) - 3*s(i-2) + 1*s(i-3).
        3 => {
            for i in 3..buf.len() {
                let a = Wrapping( 1) * Wrapping(i64::from(buf[i - 3]));
                let b = Wrapping(-3) * Wrapping(i64::from(buf[i - 2]));
                let c = Wrapping( 3) * Wrapping(i64::from(buf[i - 1]));
                buf[i] += (a + b + c).0 as i32;
            }
        },
        // A 4th order predictor uses the polynomial:
        // s(i) = 4*s(i-1) - 6*s(i-2) + 4*s(i-3) - 1*s(i-4).
        4 => {
            for i in 4..buf.len() {
                let a = Wrapping(-1) * Wrapping(i64::from(buf[i - 4]));
                let b = Wrapping( 4) * Wrapping(i64::from(buf[i - 3]));
                let c = Wrapping(-6) * Wrapping(i64::from(buf[i - 2]));
                let d = Wrapping( 4) * Wrapping(i64::from(buf[i - 1]));
                buf[i] += (a + b + c + d).0 as i32;
            }
        }
        _ => unreachable!()
    };

    Ok(())
}

/// Generalized Linear Predictive Coding (LPC) decoder macro for orders >= 4. The exact number of
/// coefficients given is specified by `order`. Coefficients must be stored in reverse order in
/// `coeffs` with the first coefficient at index 31. Coefficients at indicies less than
/// 31 - `order` must be 0. It is expected that the first `order` samples in `buf` are warm-up
/// samples.
macro_rules! lpc_predictor {
    ($func_name:ident, $order:expr) => {
        fn $func_name(
            order: usize,
            coeffs: &[i32; $order],
            coeff_shift: u32,
            buf: &mut [i32]
        ) -> Result<()> {

            // Order must be less than or equal to the number of coefficients.
            debug_assert!(order as usize <= coeffs.len());

            // Order must be less than to equal to the number of samples the buffer can hold.
            debug_assert!(order as usize <= buf.len());

            let n_prefill = cmp::min($order, buf.len()) - order;

            // If the pre-fill computation filled the entire sample buffer, return immediately since
            // the main predictor requires atleast 32 samples to be present in the buffer.
            for i in order..order + n_prefill {
                let predicted = coeffs[$order - order..$order].iter()
                                                    .zip(&buf[i - order..i])
                                                    .map(|(&c, &sample)| c as i64 * sample as i64)
                                                    .sum::<i64>();

                buf[i] += (predicted >> coeff_shift) as i32;
            }

            if buf.len() <= $order {
                return Ok(());
            }

            for i in $order..buf.len() {
                // Predict each sample by applying what is essentially an IIR filter.
                //
                // This implementation supersedes an iterator based approach where coeffs and
                // samples were zipped together, multiplied together via map, and then summed. That
                // implementation did not pipeline well since summing was performed before the next
                // multiplication, introducing pipleine stalls. This unrolled approach is much
                // faster atleast on Intel hardware.
                let s = &buf[i - $order..i];

                let mut predicted = 0i64;

                for j in 0..($order / 4) {
                    let a = coeffs[4*j + 0] as i64 * s[4*j + 0] as i64;
                    let b = coeffs[4*j + 1] as i64 * s[4*j + 1] as i64;
                    let c = coeffs[4*j + 2] as i64 * s[4*j + 2] as i64;
                    let d = coeffs[4*j + 3] as i64 * s[4*j + 3] as i64;
                    predicted += a + b + c + d;
                }

                buf[i] += (predicted >> coeff_shift) as i32;
            }

            Ok(())
        }
    };
}

lpc_predictor!(lpc_predict_32, 32);
lpc_predictor!(lpc_predict_12, 12);
lpc_predictor!(lpc_predict_8, 8);
lpc_predictor!(lpc_predict_4, 4);
