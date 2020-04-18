// Sonata
// Copyright (c) 2020 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use sonata_core::checksum::Crc8Ccitt;
use sonata_core::errors::{Result, decode_error};
use sonata_core::io::{ByteStream, MediaSourceStream, Monitor, MonitorStream, utf8_decode_be_u64};

#[derive(Debug)]
enum BlockingStrategy {
    Fixed,
    Variable
}

#[derive(Debug)]
pub enum BlockSequence {
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
pub enum ChannelAssignment {
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

pub struct FrameHeader {
    pub block_sequence: BlockSequence,
    pub block_num_samples: u16,
    pub channel_assignment: ChannelAssignment,
    pub bits_per_sample: Option<u32>,
    pub sample_rate: Option<u32>,
}

pub fn sync_frame<B: ByteStream>(reader: &mut B) -> Result<u16> {
    let mut sync = 0u16;

    // Synchronize stream to Frame Header. FLAC specifies a byte-aligned 14 bit sync code of
    // `0b11_1111_1111_1110`. This would be difficult to find on its own. Expand the search to
    // a 16-bit field of `0b1111_1111_1111_10xx` and search a word at a time.
    while (sync & 0xfffc) != 0xfff8 {
        sync = sync.wrapping_shl(8) | u16::from(reader.read_u8()?);
    }

    Ok(sync)
}

pub fn read_frame_header<B: ByteStream>(reader: &mut B, sync: u16) -> Result<FrameHeader> {
    // The header is checksummed with a CRC8 hash. Include the sync code in this CRC.
    let mut crc8 = Crc8Ccitt::new(0);
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
    let crc8_expected = reader_crc8.into_inner().read_u8()?;

    if crc8_expected != crc8_computed {
        return decode_error("Computed frame header CRC does not match expected CRC.");
    }

    Ok(FrameHeader {
        block_sequence,
        block_num_samples,
        channel_assignment,
        bits_per_sample,
        sample_rate,
    })
}

/// A very quick check if the provided buffer is likely be a FLAC frame header.
pub fn is_likely_frame_header(buf: &[u8]) -> bool {
    // let is_variable = (buf[1] & 0x1) == 1;

    if (buf[0] & 0xf0) == 0x00 {
        return false;
    }

    if (buf[0] & 0x0f) == 0x0f {
        return false;
    }

    if ((buf[1] & 0xf0) >> 4) >= 0xb {
        return false;
    }

    if (buf[1] & 0x0e == 0x6) || (buf[1] & 0x0e == 0x0e) {
        return false;
    }

    if buf[1] & 0x1 == 1 {
        return false;
    }

    true
}

pub struct ParsedPacket {
    /// The timestamp of the first audio frame in the packet.
    pub packet_ts: u64,
    /// The number of audio frames in the packet.
    pub n_frames: u32,
    // The number of bytes of the packet that were consumed while parsing.
    pub parsed_len: usize,
}

pub fn next_frame(reader: &mut MediaSourceStream) -> Result<ParsedPacket> {
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