// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]
// The following lints are allowed in all Symphonia crates. Please see clippy.toml for their
// justification.
#![allow(clippy::comparison_chain)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_range_contains)]

use std::io::{ErrorKind, Seek, SeekFrom};

use log::{debug, info};

use symphonia_core::audio::sample::{SampleFormat, i24};
use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioMut, AudioSpec, Channels, GenericAudioBuffer,
};
use symphonia_core::codecs::audio::well_known::CODEC_ID_WAVPACK;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::codecs::{CodecInfo, CodecParameters};
use symphonia_core::errors::{
    Error, Result, SeekErrorKind, decode_error, seek_error, unsupported_error,
};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_WAVPACK;
use symphonia_core::io::{BufReader, MediaSource, MediaSourceStream, ReadBytes, ScopedStream};
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::packet::PacketRef;
use symphonia_core::support_audio_codec;
use symphonia_core::support_format;
use symphonia_core::units::{Duration, Timestamp};

const WAVPACK_MARKER: [u8; 4] = *b"wvpk";
const WAVPACK_HEADER_LEN: usize = 32;
const WAVPACK_HEADER_REST_LEN: usize = WAVPACK_HEADER_LEN - WAVPACK_MARKER.len();
const WAVPACK_MIN_CK_SIZE: u32 = 24;

const WAVPACK_MIN_STREAM_VERSION: u16 = 0x402;
const WAVPACK_MAX_STREAM_VERSION: u16 = 0x410;

const BYTES_STORED: u32 = 0x3;
const MONO_FLAG: u32 = 0x4;
const HYBRID_FLAG: u32 = 0x8;
const JOINT_STEREO: u32 = 0x10;
const CROSS_DECORR: u32 = 0x20;
const HYBRID_SHAPE: u32 = 0x40;
const FLOAT_DATA: u32 = 0x80;
const INT32_DATA: u32 = 0x100;
const HYBRID_BITRATE: u32 = 0x200;
const HYBRID_BALANCE: u32 = 0x400;
const INITIAL_BLOCK: u32 = 0x800;
const FINAL_BLOCK: u32 = 0x1000;
const SHIFT_LSB: u32 = 13;
const SHIFT_MASK: u32 = 0x1f << SHIFT_LSB;
const SRATE_LSB: u32 = 23;
const SRATE_MASK: u32 = 0xf << SRATE_LSB;
const NEW_SHAPING: u32 = 0x2000_0000;
const FALSE_STEREO: u32 = 0x4000_0000;
const DSD_FLAG: u32 = 0x8000_0000;

const ID_UNIQUE: u8 = 0x3f;
const ID_OPTIONAL_DATA: u8 = 0x20;
const ID_ODD_SIZE: u8 = 0x40;
const ID_LARGE: u8 = 0x80;
const ID_DECORR_TERMS: u8 = 0x02;
const ID_DECORR_WEIGHTS: u8 = 0x03;
const ID_DECORR_SAMPLES: u8 = 0x04;
const ID_ENTROPY_VARS: u8 = 0x05;
const ID_HYBRID_PROFILE: u8 = 0x06;
const ID_SHAPING_WEIGHTS: u8 = 0x07;
const ID_FLOAT_INFO: u8 = 0x08;
const ID_INT32_INFO: u8 = 0x09;
const ID_WV_BITSTREAM: u8 = 0x0a;
const ID_WVC_BITSTREAM: u8 = 0x0b;
const ID_WVX_BITSTREAM: u8 = 0x0c;
const ID_WVX_NEW_BITSTREAM: u8 = ID_OPTIONAL_DATA | ID_WVX_BITSTREAM;
const ID_CHANNEL_INFO: u8 = 0x0d;
const ID_DSD_BLOCK: u8 = 0x0e;
const ID_SAMPLE_RATE: u8 = 0x27;
const ID_MD5_CHECKSUM: u8 = 0x26;

const SAMPLE_RATES: [u32; 15] = [
    6000, 8000, 9600, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000,
    192000,
];

const MAX_TERM: i8 = 8;
const MAX_TERM_USIZE: usize = MAX_TERM as usize;
const MAX_NTERMS: usize = 16;
const LIMIT_ONES: u32 = 16;

const DIV0: u32 = 128;
const DIV1: u32 = 64;
const DIV2: u32 = 32;
const SLS: u32 = 8;
const SLO: u32 = 1 << (SLS - 1);

const FLOAT_SHIFT_ONES: u8 = 0x01;
const FLOAT_SHIFT_SAME: u8 = 0x02;
const FLOAT_SHIFT_SENT: u8 = 0x04;
const FLOAT_ZEROS_SENT: u8 = 0x08;
const FLOAT_NEG_ZEROS: u8 = 0x10;

const LOG2_TABLE: [u8; 256] = [
    0x00, 0x01, 0x03, 0x04, 0x06, 0x07, 0x09, 0x0a, 0x0b, 0x0d, 0x0e, 0x10, 0x11, 0x12, 0x14, 0x15,
    0x16, 0x18, 0x19, 0x1a, 0x1c, 0x1d, 0x1e, 0x20, 0x21, 0x22, 0x24, 0x25, 0x26, 0x28, 0x29, 0x2a,
    0x2c, 0x2d, 0x2e, 0x2f, 0x31, 0x32, 0x33, 0x34, 0x36, 0x37, 0x38, 0x39, 0x3b, 0x3c, 0x3d, 0x3e,
    0x3f, 0x41, 0x42, 0x43, 0x44, 0x45, 0x47, 0x48, 0x49, 0x4a, 0x4b, 0x4d, 0x4e, 0x4f, 0x50, 0x51,
    0x52, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x5c, 0x5d, 0x5e, 0x5f, 0x60, 0x61, 0x62, 0x63,
    0x64, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x6b, 0x6c, 0x6d, 0x6e, 0x6f, 0x70, 0x71, 0x72, 0x74, 0x75,
    0x76, 0x77, 0x78, 0x79, 0x7a, 0x7b, 0x7c, 0x7d, 0x7e, 0x7f, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85,
    0x86, 0x87, 0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8d, 0x8e, 0x8f, 0x90, 0x91, 0x92, 0x93, 0x94, 0x95,
    0x96, 0x97, 0x98, 0x99, 0x9a, 0x9b, 0x9b, 0x9c, 0x9d, 0x9e, 0x9f, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4,
    0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xa9, 0xaa, 0xab, 0xac, 0xad, 0xae, 0xaf, 0xb0, 0xb1, 0xb2, 0xb2,
    0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xb9, 0xba, 0xbb, 0xbc, 0xbd, 0xbe, 0xbf, 0xc0, 0xc0,
    0xc1, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xcb, 0xcb, 0xcc, 0xcd, 0xce,
    0xcf, 0xd0, 0xd0, 0xd1, 0xd2, 0xd3, 0xd4, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd8, 0xd9, 0xda, 0xdb,
    0xdc, 0xdc, 0xdd, 0xde, 0xdf, 0xe0, 0xe0, 0xe1, 0xe2, 0xe3, 0xe4, 0xe4, 0xe5, 0xe6, 0xe7, 0xe7,
    0xe8, 0xe9, 0xea, 0xea, 0xeb, 0xec, 0xed, 0xee, 0xee, 0xef, 0xf0, 0xf1, 0xf1, 0xf2, 0xf3, 0xf4,
    0xf4, 0xf5, 0xf6, 0xf7, 0xf7, 0xf8, 0xf9, 0xf9, 0xfa, 0xfb, 0xfc, 0xfc, 0xfd, 0xfe, 0xff, 0xff,
];

const EXP2_TABLE: [u8; 256] = [
    0x00, 0x01, 0x01, 0x02, 0x03, 0x03, 0x04, 0x05, 0x06, 0x06, 0x07, 0x08, 0x08, 0x09, 0x0a, 0x0b,
    0x0b, 0x0c, 0x0d, 0x0e, 0x0e, 0x0f, 0x10, 0x10, 0x11, 0x12, 0x13, 0x13, 0x14, 0x15, 0x16, 0x16,
    0x17, 0x18, 0x19, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1d, 0x1e, 0x1f, 0x20, 0x20, 0x21, 0x22, 0x23,
    0x24, 0x24, 0x25, 0x26, 0x27, 0x28, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2c, 0x2d, 0x2e, 0x2f, 0x30,
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3a, 0x3b, 0x3c, 0x3d,
    0x3e, 0x3f, 0x40, 0x41, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x48, 0x49, 0x4a, 0x4b,
    0x4c, 0x4d, 0x4e, 0x4f, 0x50, 0x51, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a,
    0x5b, 0x5c, 0x5d, 0x5e, 0x5e, 0x5f, 0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x6b, 0x6c, 0x6d, 0x6e, 0x6f, 0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79,
    0x7a, 0x7b, 0x7c, 0x7d, 0x7e, 0x7f, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x87, 0x88, 0x89, 0x8a,
    0x8b, 0x8c, 0x8d, 0x8e, 0x8f, 0x90, 0x91, 0x92, 0x93, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0x9b,
    0x9c, 0x9d, 0x9f, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa8, 0xa9, 0xaa, 0xab, 0xac, 0xad,
    0xaf, 0xb0, 0xb1, 0xb2, 0xb3, 0xb4, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xbc, 0xbd, 0xbe, 0xbf, 0xc0,
    0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc8, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf, 0xd0, 0xd2, 0xd3, 0xd4,
    0xd6, 0xd7, 0xd8, 0xd9, 0xdb, 0xdc, 0xdd, 0xde, 0xe0, 0xe1, 0xe2, 0xe4, 0xe5, 0xe6, 0xe8, 0xe9,
    0xea, 0xec, 0xed, 0xee, 0xf0, 0xf1, 0xf2, 0xf4, 0xf5, 0xf6, 0xf8, 0xf9, 0xfa, 0xfc, 0xfd, 0xff,
];

const WAVPACK_CODEC_INFO: CodecInfo =
    CodecInfo { short_name: "wavpack", long_name: "WavPack", profiles: &[] };

const WAVPACK_FORMAT_INFO: FormatInfo =
    FormatInfo { format: FORMAT_ID_WAVPACK, short_name: "wavpack", long_name: "WavPack" };

#[derive(Clone, Debug)]
struct WavPackBlockHeader {
    ck_size: u32,
    _version: u16,
    block_index: u64,
    total_samples: Option<u64>,
    block_samples: u32,
    flags: u32,
    _crc: u32,
}

impl WavPackBlockHeader {
    fn parse(src: &[u8]) -> Result<Self> {
        if src.len() < WAVPACK_HEADER_LEN || src[..4] != WAVPACK_MARKER {
            return decode_error("wavpack: invalid block header");
        }

        let mut reader = BufReader::new(&src[4..WAVPACK_HEADER_LEN]);
        let ck_size = reader.read_u32()?;

        if ck_size < WAVPACK_MIN_CK_SIZE {
            return decode_error("wavpack: invalid block size");
        }

        let version = reader.read_u16()?;

        if !(WAVPACK_MIN_STREAM_VERSION..=WAVPACK_MAX_STREAM_VERSION).contains(&version) {
            return unsupported_error("wavpack: unsupported stream version");
        }

        let block_index_hi = u64::from(reader.read_byte()?);
        let total_samples_hi = u64::from(reader.read_byte()?);
        let total_samples_lo = reader.read_u32()?;
        let block_index_lo = reader.read_u32()?;
        let block_samples = reader.read_u32()?;
        let flags = reader.read_u32()?;
        let crc = reader.read_u32()?;

        let total_samples = if total_samples_lo == u32::MAX {
            None
        } else {
            Some(u64::from(total_samples_lo) + (total_samples_hi << 32) - total_samples_hi)
        };

        Ok(WavPackBlockHeader {
            ck_size,
            _version: version,
            block_index: u64::from(block_index_lo) + (block_index_hi << 32),
            total_samples,
            block_samples,
            flags,
            _crc: crc,
        })
    }

    fn byte_len(&self) -> u64 {
        u64::from(self.ck_size) + 8
    }

    fn body_len(&self) -> usize {
        (self.ck_size - WAVPACK_MIN_CK_SIZE) as usize
    }

    fn channels_in_block(&self) -> u16 {
        if self.flags & MONO_FLAG != 0 { 1 } else { 2 }
    }

    fn bytes_per_sample(&self) -> u32 {
        (self.flags & BYTES_STORED) + 1
    }

    fn shifted_bits(&self) -> u32 {
        (self.flags & SHIFT_MASK) >> SHIFT_LSB
    }

    fn bits_per_sample(&self) -> u32 {
        self.bytes_per_sample() * 8 - self.shifted_bits()
    }

    fn sample_rate_from_flags(&self) -> Option<u32> {
        let idx = ((self.flags & SRATE_MASK) >> SRATE_LSB) as usize;
        SAMPLE_RATES.get(idx).copied()
    }

    fn output_channels_in_block(&self) -> usize {
        if self.flags & FALSE_STEREO != 0 {
            2
        } else if self.flags & MONO_FLAG != 0 {
            1
        } else {
            2
        }
    }

    fn sample_format(&self) -> Result<SampleFormat> {
        if self.flags & DSD_FLAG != 0 {
            return Ok(SampleFormat::U8);
        }

        if self.flags & FLOAT_DATA != 0 {
            Ok(SampleFormat::F32)
        } else {
            match self.bytes_per_sample() {
                1 => Ok(SampleFormat::S8),
                2 => Ok(SampleFormat::S16),
                3 => Ok(SampleFormat::S24),
                4 => Ok(SampleFormat::S32),
                _ => decode_error("wavpack: invalid sample width"),
            }
        }
    }
}

#[derive(Clone, Debug)]
struct WavPackBlock {
    header: WavPackBlockHeader,
    data: Box<[u8]>,
}

type WavPackPacketBlocks = (Timestamp, Duration, Box<[u8]>);

fn read_block_with_marker(
    reader: &mut MediaSourceStream<'_>,
    marker: [u8; 4],
) -> Result<WavPackBlock> {
    if marker != WAVPACK_MARKER {
        return decode_error("wavpack: invalid block marker");
    }

    let mut data = Vec::with_capacity(WAVPACK_HEADER_LEN);
    data.extend_from_slice(&marker);
    data.extend_from_slice(&reader.read_boxed_slice_exact(WAVPACK_HEADER_REST_LEN)?);

    let header = WavPackBlockHeader::parse(&data)?;
    let body = reader.read_boxed_slice_exact(header.body_len())?;
    data.extend_from_slice(&body);

    Ok(WavPackBlock { header, data: data.into_boxed_slice() })
}

fn read_block_header_with_marker(
    reader: &mut MediaSourceStream<'_>,
    marker: [u8; 4],
) -> Result<WavPackBlockHeader> {
    if marker != WAVPACK_MARKER {
        return decode_error("wavpack: invalid block marker");
    }

    let mut data = [0u8; WAVPACK_HEADER_LEN];
    data[..4].copy_from_slice(&marker);
    reader.read_buf_exact(&mut data[4..])?;
    WavPackBlockHeader::parse(&data)
}

fn read_block(reader: &mut MediaSourceStream<'_>) -> Result<Option<WavPackBlock>> {
    let marker = match reader.read_quad_bytes() {
        Ok(marker) => marker,
        Err(err) if err.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err.into()),
    };

    if is_trailing_metadata_marker(marker) {
        return Ok(None);
    }

    read_block_with_marker(reader, marker).map(Some)
}

fn is_trailing_metadata_marker(marker: [u8; 4]) -> bool {
    marker == *b"APET" || marker[..3] == *b"TAG" || marker[..3] == *b"ID3"
}

#[derive(Debug)]
struct WavPackMetadata<'a> {
    id: u8,
    data: &'a [u8],
}

struct WavPackMetadataIter<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> WavPackMetadataIter<'a> {
    fn new(block: &'a WavPackBlock) -> Self {
        Self::new_data(&block.data[WAVPACK_HEADER_LEN..])
    }

    fn new_data(data: &'a [u8]) -> Self {
        WavPackMetadataIter { data, pos: 0 }
    }
}

impl<'a> Iterator for WavPackMetadataIter<'a> {
    type Item = Result<WavPackMetadata<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.data.len().saturating_sub(self.pos) < 2 {
            return None;
        }

        let mut id = self.data[self.pos];
        self.pos += 1;

        let mut byte_len = usize::from(self.data[self.pos]) << 1;
        self.pos += 1;

        if id & ID_LARGE != 0 {
            id &= !ID_LARGE;

            if self.data.len().saturating_sub(self.pos) < 2 {
                return Some(decode_error("wavpack: invalid large metadata size"));
            }

            byte_len += usize::from(self.data[self.pos]) << 9;
            self.pos += 1;
            byte_len += usize::from(self.data[self.pos]) << 17;
            self.pos += 1;
        }

        if id & ID_ODD_SIZE != 0 {
            if byte_len == 0 {
                return Some(decode_error("wavpack: invalid odd metadata size"));
            }

            id &= !ID_ODD_SIZE;
            byte_len -= 1;
        }

        let padded_len = byte_len + (byte_len & 1);

        if self.data.len().saturating_sub(self.pos) < padded_len {
            return Some(decode_error("wavpack: metadata overruns block"));
        }

        let data = &self.data[self.pos..self.pos + byte_len];
        self.pos += padded_len;

        Some(Ok(WavPackMetadata { id: id & ID_UNIQUE, data }))
    }
}

#[derive(Copy, Clone, Debug, Default)]
struct StreamInfo {
    sample_rate: Option<u32>,
    channels: Option<u16>,
    channel_mask: Option<u32>,
    md5: Option<[u8; 16]>,
    dsd_multiplier: Option<u32>,
}

impl StreamInfo {
    fn from_block(block: &WavPackBlock) -> Result<Self> {
        let mut info = StreamInfo {
            sample_rate: block.header.sample_rate_from_flags(),
            channels: Some(block.header.output_channels_in_block() as u16),
            channel_mask: None,
            md5: None,
            dsd_multiplier: None,
        };

        for metadata in WavPackMetadataIter::new(block) {
            let metadata = metadata?;

            match metadata.id {
                ID_SAMPLE_RATE => {
                    if matches!(metadata.data.len(), 3 | 4) {
                        let mut sample_rate = u32::from(metadata.data[0])
                            | (u32::from(metadata.data[1]) << 8)
                            | (u32::from(metadata.data[2]) << 16);

                        if metadata.data.len() == 4 {
                            sample_rate |= (u32::from(metadata.data[3]) & 0x7f) << 24;
                        }

                        info.sample_rate = Some(sample_rate);
                    }
                }
                ID_CHANNEL_INFO => {
                    if let Some((channels, mask)) = parse_channel_info(metadata.data)? {
                        info.channels = Some(channels);
                        info.channel_mask = mask;
                    }
                }
                ID_MD5_CHECKSUM if metadata.data.len() == 16 => {
                    let mut md5 = [0; 16];
                    md5.copy_from_slice(metadata.data);
                    info.md5 = Some(md5);
                }
                ID_DSD_BLOCK => {
                    let dsd = parse_dsd_info(metadata.data)?;
                    info.dsd_multiplier =
                        Some(1u32.checked_shl(u32::from(dsd.multiplier_pow)).unwrap_or(0));
                }
                _ => (),
            }
        }

        Ok(info)
    }
}

fn parse_channel_info(data: &[u8]) -> Result<Option<(u16, Option<u32>)>> {
    if data.is_empty() || data.len() > 7 {
        return decode_error("wavpack: invalid channel info");
    }

    let (channels, mask_data) = if data.len() >= 6 {
        let channels = u16::from(data[0]) | (u16::from(data[2] & 0x0f) << 8);
        (channels + 1, &data[3..])
    } else {
        (u16::from(data[0]), &data[1..])
    };

    let mut mask = 0u32;

    for (i, &byte) in mask_data.iter().enumerate() {
        mask |= u32::from(byte) << (i * 8);
    }

    Ok(Some((channels, if mask == 0 { None } else { Some(mask) })))
}

fn make_channels(count: u16, mask: Option<u32>) -> Channels {
    if let Some(mask) = mask.and_then(symphonia_core::audio::Position::from_wave_channel_mask) {
        Channels::Positioned(mask)
    } else {
        Channels::Discrete(count)
    }
}

fn make_codec_params(block: &WavPackBlock) -> Result<AudioCodecParameters> {
    let stream_info = StreamInfo::from_block(block)?;
    let channels = stream_info.channels.unwrap_or_else(|| block.header.channels_in_block());
    let mut sample_rate =
        stream_info.sample_rate.ok_or(Error::Unsupported("wavpack: sample rate is required"))?;
    if block.header.flags & DSD_FLAG != 0 {
        let multiplier = stream_info.dsd_multiplier.unwrap_or(1);
        sample_rate = sample_rate
            .checked_mul(multiplier)
            .and_then(|rate| rate.checked_mul(8))
            .ok_or(Error::DecodeError("wavpack: invalid dsd sample rate"))?;
    }
    let sample_format = block.header.sample_format()?;
    let bits_per_sample = block.header.bits_per_sample();

    let mut params = AudioCodecParameters::new();
    params
        .for_codec(CODEC_ID_WAVPACK)
        .with_sample_rate(sample_rate)
        .with_channels(make_channels(channels, stream_info.channel_mask))
        .with_sample_format(sample_format)
        .with_bits_per_sample(bits_per_sample)
        .with_bits_per_coded_sample(bits_per_sample);

    if block.header.flags & DSD_FLAG != 0 {
        params.with_bits_per_sample(8).with_bits_per_coded_sample(1);
    }

    if block.header.block_samples != 0 {
        params.with_max_frames_per_packet(u64::from(block.header.block_samples));
    }

    if let Some(md5) = stream_info.md5 {
        params.with_verification_code(symphonia_core::codecs::audio::VerificationCheck::Md5(md5));
    }

    Ok(params)
}

#[derive(Copy, Clone, Debug, Default)]
struct DecorrPass {
    term: i8,
    delta: u8,
    weight_a: i32,
    weight_b: i32,
    samples_a: [i32; MAX_TERM_USIZE],
    samples_b: [i32; MAX_TERM_USIZE],
}

#[derive(Copy, Clone, Debug, Default)]
struct EntropyVars {
    median: [[u32; 3]; 2],
}

#[derive(Copy, Clone, Debug, Default)]
struct HybridProfile {
    slow_level: [u32; 2],
    bitrate_acc: [u32; 2],
    bitrate_delta: [u32; 2],
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
struct ShapingInfo {
    error: [i32; 2],
    shaping_acc: [i32; 2],
    shaping_delta: [i32; 2],
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
struct DecodedWord {
    value: i32,
    correction: i32,
}

#[derive(Copy, Clone, Debug, Default)]
struct Int32Info {
    sent_bits: u32,
    zeros: u32,
    ones: u32,
    dups: u32,
}

#[derive(Copy, Clone, Debug, Default)]
struct FloatInfo {
    flags: u8,
    shift: u8,
    max_exp: u8,
    _norm_exp: u8,
}

#[derive(Copy, Clone, Debug)]
struct DsdInfo<'a> {
    multiplier_pow: u8,
    mode: u8,
    payload: &'a [u8],
}

const MAX_HISTORY_BITS: u8 = 5;
const MAX_BYTES_PER_BIN: usize = 1280;
const PTABLE_BITS: usize = 8;
const PTABLE_BINS: usize = 1 << PTABLE_BITS;
const PTABLE_MASK: usize = PTABLE_BINS - 1;
const UP: i32 = 0x0100_00fe;
const DOWN: i32 = 0x0001_0000;
const DECAY: i32 = 8;
const PRECISION: i32 = 20;
const VALUE_ONE: i32 = 1 << PRECISION;
const PRECISION_USE: i32 = 12;
const RATE_S: u8 = 20;

#[derive(Copy, Clone, Debug, Default)]
struct DsdFilterState {
    byte: u8,
    factor: i32,
    filter1: i32,
    filter2: i32,
    filter3: i32,
    filter4: i32,
    filter5: i32,
    filter6: i32,
    value: i32,
}

#[derive(Debug)]
struct DsdScratch {
    probabilities: Vec<u8>,
    summed: Vec<u16>,
    offsets: Vec<usize>,
    lookup: Vec<u8>,
    ptable: [i32; PTABLE_BINS],
}

impl Default for DsdScratch {
    fn default() -> Self {
        DsdScratch {
            probabilities: Vec::new(),
            summed: Vec::new(),
            offsets: Vec::new(),
            lookup: Vec::new(),
            ptable: [0; PTABLE_BINS],
        }
    }
}

#[derive(Debug)]
struct DecodeBlock<'a> {
    header: WavPackBlockHeader,
    decorr_passes: [DecorrPass; MAX_NTERMS],
    decorr_pass_count: usize,
    entropy: Option<EntropyVars>,
    decorr_samples: Option<&'a [u8]>,
    hybrid_profile: Option<HybridProfile>,
    shaping_info: Option<ShapingInfo>,
    float_info: Option<FloatInfo>,
    int32_info: Option<Int32Info>,
    dsd: Option<DsdInfo<'a>>,
    wv_bitstream: Option<&'a [u8]>,
    wvc_bitstream: Option<&'a [u8]>,
    wvx_bitstream: Option<&'a [u8]>,
    wvx_new_bitstream: bool,
}

impl<'a> DecodeBlock<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        let header = WavPackBlockHeader::parse(data)?;

        if data.len() < header.byte_len() as usize {
            return decode_error("wavpack: block overruns packet");
        }

        let mut parsed = DecodeBlock {
            header,
            decorr_passes: [DecorrPass::default(); MAX_NTERMS],
            decorr_pass_count: 0,
            entropy: None,
            decorr_samples: None,
            hybrid_profile: None,
            shaping_info: None,
            float_info: None,
            int32_info: None,
            dsd: None,
            wv_bitstream: None,
            wvc_bitstream: None,
            wvx_bitstream: None,
            wvx_new_bitstream: false,
        };

        let body = &data[WAVPACK_HEADER_LEN..parsed.header.byte_len() as usize];

        for metadata in WavPackMetadataIter::new_data(body) {
            let metadata = metadata?;

            match metadata.id {
                ID_DECORR_TERMS => {
                    parsed.decorr_pass_count = parse_decorr_terms(
                        metadata.data,
                        parsed.header.flags,
                        &mut parsed.decorr_passes,
                    )?;
                }
                ID_DECORR_WEIGHTS => {
                    parse_decorr_weights(
                        metadata.data,
                        parsed.header.flags,
                        parsed.decorr_passes_mut(),
                    )?;
                }
                ID_DECORR_SAMPLES => parsed.decorr_samples = Some(metadata.data),
                ID_ENTROPY_VARS => {
                    parsed.entropy = Some(parse_entropy_vars(metadata.data, parsed.header.flags)?);
                }
                ID_HYBRID_PROFILE => {
                    parsed.hybrid_profile =
                        Some(parse_hybrid_profile(metadata.data, parsed.header.flags)?);
                }
                ID_SHAPING_WEIGHTS => {
                    parsed.shaping_info =
                        Some(parse_shaping_info(metadata.data, parsed.header.flags)?);
                }
                ID_FLOAT_INFO => parsed.float_info = Some(parse_float_info(metadata.data)?),
                ID_INT32_INFO => parsed.int32_info = Some(parse_int32_info(metadata.data)?),
                ID_DSD_BLOCK => parsed.dsd = Some(parse_dsd_info(metadata.data)?),
                ID_WV_BITSTREAM => parsed.wv_bitstream = Some(metadata.data),
                ID_WVC_BITSTREAM => parsed.wvc_bitstream = Some(metadata.data),
                ID_WVX_BITSTREAM => {
                    parsed.wvx_bitstream = Some(metadata.data);
                    parsed.wvx_new_bitstream = false;
                }
                ID_WVX_NEW_BITSTREAM => {
                    parsed.wvx_bitstream = Some(metadata.data);
                    parsed.wvx_new_bitstream = true;
                }
                _ => (),
            }
        }

        if parsed.header.block_samples != 0 && parsed.wv_bitstream.is_none() && parsed.dsd.is_none()
        {
            return decode_error("wavpack: missing audio bitstream");
        }

        if let Some(decorr_samples) = parsed.decorr_samples {
            parse_decorr_samples(
                decorr_samples,
                parsed.header._version,
                parsed.header.flags,
                parsed.decorr_passes_mut(),
            )?;
        }

        Ok(parsed)
    }

    fn metadata_summary(&self) -> (usize, bool, bool, bool, bool) {
        (
            self.decorr_pass_count,
            self.entropy.is_some(),
            self.decorr_samples.is_some()
                || self.hybrid_profile.is_some()
                || self.shaping_info.is_some()
                || self.wv_bitstream.is_some(),
            self.wvc_bitstream.is_some() || self.wvx_bitstream.is_some(),
            self.float_info.is_some() || self.int32_info.is_some(),
        )
    }

    fn decorr_passes(&self) -> &[DecorrPass] {
        &self.decorr_passes[..self.decorr_pass_count]
    }

    fn decorr_passes_mut(&mut self) -> &mut [DecorrPass] {
        &mut self.decorr_passes[..self.decorr_pass_count]
    }

    fn read_decorrelated_samples_into(
        &self,
        residuals: &mut Vec<DecodedWord>,
        samples: &mut Vec<i32>,
        decorr_passes: &mut Vec<DecorrPass>,
        dsd_scratch: &mut DsdScratch,
    ) -> Result<bool> {
        if self.header.block_samples == 0 {
            samples.clear();
            return Ok(false);
        }

        if let Some(dsd) = self.dsd {
            decode_dsd_into(
                samples,
                dsd,
                self.header.flags,
                self.header.block_samples as usize,
                dsd_scratch,
            )?;
            let crc = dsd_crc(samples);
            if crc != self.header._crc {
                return decode_error("wavpack: dsd crc mismatch");
            }
            expand_false_stereo(samples, self.header.flags);
            return Ok(true);
        }

        let entropy =
            self.entropy.ok_or(Error::DecodeError("wavpack: missing entropy variables"))?;

        let bitstream =
            self.wv_bitstream.ok_or(Error::DecodeError("wavpack: missing audio bitstream"))?;

        let mut words = WordsDecoder::new(entropy, self.hybrid_profile, self.header.flags);
        let mut bits = WavPackBitReader::new(bitstream);
        let mut correction_bits = self.wvc_bitstream.map(WavPackBitReader::new);

        words.read_words_into(
            &mut bits,
            correction_bits.as_mut(),
            self.header.block_samples as usize,
            residuals,
        )?;

        if self.wvc_bitstream.is_some() {
            apply_hybrid_correction_into(
                residuals,
                self.header.flags,
                self.header.block_samples as usize,
                self.decorr_passes(),
                self.shaping_info,
                samples,
                decorr_passes,
            )?;
        } else {
            samples.clear();
            samples.reserve(residuals.len());
            samples.extend(residuals.iter().map(|word| word.value));
            apply_decorr_passes(
                samples,
                self.header.flags,
                self.header.block_samples as usize,
                self.decorr_passes(),
            )?;
        }

        fixup_samples(
            samples,
            self.header.flags,
            self.float_info,
            self.int32_info,
            self.wvx_bitstream,
            self.wvx_new_bitstream,
            self.wvc_bitstream.is_some(),
        )?;
        expand_false_stereo(samples, self.header.flags);

        Ok(true)
    }
}

fn is_mono(flags: u32) -> bool {
    flags & (MONO_FLAG | FALSE_STEREO) != 0
}

fn parse_decorr_terms(data: &[u8], flags: u32, passes: &mut [DecorrPass]) -> Result<usize> {
    if data.len() > MAX_NTERMS {
        return decode_error("wavpack: too many decorrelation terms");
    }

    if data.len() > passes.len() {
        return decode_error("wavpack: too many decorrelation terms");
    }

    for pass in passes.iter_mut() {
        *pass = DecorrPass::default();
    }

    for (&byte, pass) in data.iter().zip(passes[..data.len()].iter_mut().rev()) {
        let term = (byte & 0x1f) as i8 - 5;
        let delta = (byte >> 5) & 0x7;

        if term == 0
            || term < -3
            || (term > MAX_TERM && term < 17)
            || term > 18
            || (is_mono(flags) && term < 0)
        {
            return decode_error("wavpack: invalid decorrelation term");
        }

        pass.term = term;
        pass.delta = delta;
    }

    Ok(data.len())
}

fn parse_decorr_weights(data: &[u8], flags: u32, passes: &mut [DecorrPass]) -> Result<()> {
    let term_count = if is_mono(flags) { data.len() } else { data.len() / 2 };

    if term_count > passes.len() || (!is_mono(flags) && data.len() & 1 != 0) {
        return decode_error("wavpack: invalid decorrelation weights");
    }

    for pass in passes.iter_mut() {
        pass.weight_a = 0;
        pass.weight_b = 0;
    }

    let mut weights = data.iter().copied();

    for pass in passes.iter_mut().rev().take(term_count) {
        pass.weight_a = restore_weight(weights.next().unwrap() as i8);

        if !is_mono(flags) {
            pass.weight_b = restore_weight(weights.next().unwrap() as i8);
        }
    }

    Ok(())
}

fn parse_entropy_vars(data: &[u8], flags: u32) -> Result<EntropyVars> {
    let expected = if is_mono(flags) { 6 } else { 12 };

    if data.len() != expected {
        return decode_error("wavpack: invalid entropy variables");
    }

    let mut entropy = EntropyVars::default();

    for ch in 0..if is_mono(flags) { 1 } else { 2 } {
        for median in 0..3 {
            let pos = ch * 6 + median * 2;
            let log = u16::from_le_bytes([data[pos], data[pos + 1]]);
            entropy.median[ch][median] = wp_exp2s(i32::from(log)) as u32;
        }
    }

    Ok(entropy)
}

fn parse_decorr_samples(
    data: &[u8],
    version: u16,
    flags: u32,
    passes: &mut [DecorrPass],
) -> Result<()> {
    let mono = is_mono(flags);
    let mut reader = BufReader::new(data);

    for pass in passes.iter_mut() {
        pass.samples_a = [0; MAX_TERM_USIZE];
        pass.samples_b = [0; MAX_TERM_USIZE];
    }

    if version == 0x402 && flags & HYBRID_FLAG != 0 {
        reader.read_i16()?;

        if !mono {
            reader.read_i16()?;
        }
    }

    for pass in passes.iter_mut().rev() {
        if reader.pos() == data.len() as u64 {
            break;
        }

        if pass.term > MAX_TERM {
            pass.samples_a[0] = read_wp_signed_log_value(&mut reader)?;
            pass.samples_a[1] = read_wp_signed_log_value(&mut reader)?;

            if !mono {
                pass.samples_b[0] = read_wp_signed_log_value(&mut reader)?;
                pass.samples_b[1] = read_wp_signed_log_value(&mut reader)?;
            }
        } else if pass.term < 0 {
            pass.samples_a[0] = read_wp_signed_log_value(&mut reader)?;
            pass.samples_b[0] = read_wp_signed_log_value(&mut reader)?;
        } else {
            for sample in 0..pass.term as usize {
                pass.samples_a[sample] = read_wp_signed_log_value(&mut reader)?;

                if !mono {
                    pass.samples_b[sample] = read_wp_signed_log_value(&mut reader)?;
                }
            }
        }
    }

    if reader.pos() != data.len() as u64 {
        return decode_error("wavpack: invalid decorrelation samples");
    }

    Ok(())
}

fn parse_hybrid_profile(data: &[u8], flags: u32) -> Result<HybridProfile> {
    let mono = is_mono(flags);
    let mut reader = BufReader::new(data);
    let mut profile = HybridProfile::default();

    if flags & HYBRID_BITRATE != 0 {
        profile.slow_level[0] = read_wp_log_value(&mut reader)? as u32;

        if !mono {
            profile.slow_level[1] = read_wp_log_value(&mut reader)? as u32;
        }
    }

    profile.bitrate_acc[0] = u32::from(reader.read_u16()?) << 16;

    if !mono {
        profile.bitrate_acc[1] = u32::from(reader.read_u16()?) << 16;
    }

    if reader.pos() < data.len() as u64 {
        profile.bitrate_delta[0] = read_wp_signed_log_value(&mut reader)? as u32;

        if !mono {
            profile.bitrate_delta[1] = read_wp_signed_log_value(&mut reader)? as u32;
        }
    }

    if reader.pos() != data.len() as u64 {
        return decode_error("wavpack: invalid hybrid profile");
    }

    Ok(profile)
}

fn parse_shaping_info(data: &[u8], flags: u32) -> Result<ShapingInfo> {
    let mono = is_mono(flags);
    let mut info = ShapingInfo::default();

    if data.len() == 2 {
        info.shaping_acc[0] = restore_weight(data[0] as i8) << 16;
        info.shaping_acc[1] = restore_weight(data[1] as i8) << 16;
        return Ok(info);
    }

    let expected = if mono { 4 } else { 8 };
    let expected_with_delta = if mono { 6 } else { 12 };

    if data.len() < expected {
        return decode_error("wavpack: invalid shaping info");
    }

    let mut reader = BufReader::new(data);
    info.error[0] = read_wp_signed_log_value(&mut reader)?;
    info.shaping_acc[0] = read_wp_signed_log_value(&mut reader)?;

    if !mono {
        info.error[1] = read_wp_signed_log_value(&mut reader)?;
        info.shaping_acc[1] = read_wp_signed_log_value(&mut reader)?;
    }

    if data.len() == expected_with_delta {
        info.shaping_delta[0] = read_wp_signed_log_value(&mut reader)?;

        if !mono {
            info.shaping_delta[1] = read_wp_signed_log_value(&mut reader)?;
        }
    }

    Ok(info)
}

fn read_wp_log_value(reader: &mut BufReader<'_>) -> Result<i32> {
    Ok(wp_exp2s(i32::from(reader.read_u16()?)))
}

fn read_wp_signed_log_value(reader: &mut BufReader<'_>) -> Result<i32> {
    Ok(wp_exp2s(i32::from(reader.read_i16()?)))
}

fn parse_four_byte_info(data: &[u8], kind: &'static str) -> Result<[u8; 4]> {
    if data.len() != 4 {
        return match kind {
            "float" => decode_error("wavpack: invalid float info"),
            "int32" => decode_error("wavpack: invalid int32 info"),
            _ => decode_error("wavpack: invalid metadata info"),
        };
    }

    let mut info = [0; 4];
    info.copy_from_slice(data);
    Ok(info)
}

fn parse_int32_info(data: &[u8]) -> Result<Int32Info> {
    let info = parse_four_byte_info(data, "int32")?;

    Ok(Int32Info {
        sent_bits: u32::from(info[0]) & 0x1f,
        zeros: u32::from(info[1]) & 0x1f,
        ones: u32::from(info[2]) & 0x1f,
        dups: u32::from(info[3]) & 0x1f,
    })
}

fn parse_float_info(data: &[u8]) -> Result<FloatInfo> {
    let info = parse_four_byte_info(data, "float")?;

    Ok(FloatInfo { flags: info[0], shift: info[1], max_exp: info[2], _norm_exp: info[3] })
}

fn parse_dsd_info(data: &[u8]) -> Result<DsdInfo<'_>> {
    if data.len() < 2 {
        return decode_error("wavpack: invalid dsd block");
    }

    let multiplier_pow = data[0];
    if multiplier_pow > 31 {
        return decode_error("wavpack: invalid dsd multiplier");
    }

    Ok(DsdInfo { multiplier_pow, mode: data[1], payload: &data[2..] })
}

fn restore_weight(weight: i8) -> i32 {
    let mut result = i32::from(weight) * 8;

    if result > 0 {
        result += (result + 64) >> 7;
    }

    result
}

fn apply_weight(weight: i32, sample: i32) -> i32 {
    if sample == i32::from(sample as i16) {
        weight.wrapping_mul(sample).wrapping_add(512) >> 10
    } else {
        let low = ((sample & 0xffff).wrapping_mul(weight)) >> 9;
        let high = ((sample & !0xffff) >> 9).wrapping_mul(weight);
        low.wrapping_add(high).wrapping_add(1) >> 1
    }
}

fn predict_term_17(prev: i32, older: i32) -> i32 {
    prev.wrapping_mul(2).wrapping_sub(older)
}

fn predict_term_18(prev: i32, older: i32) -> i32 {
    prev.wrapping_mul(3).wrapping_sub(older) >> 1
}

fn predict_term_18_stereo(prev: i32, older: i32) -> i32 {
    prev.wrapping_add(prev.wrapping_sub(older) >> 1)
}

fn update_weight(weight: &mut i32, delta: u8, source: i32, result: i32) {
    if source != 0 && result != 0 {
        let s = (source ^ result) >> 31;
        *weight = (i32::from(delta) ^ s) + (*weight - s);
    }
}

fn update_weight_clip(weight: &mut i32, delta: u8, source: i32, result: i32) {
    if source != 0 && result != 0 {
        let s = (source ^ result) >> 31;
        let mut updated = (*weight ^ s) + (i32::from(delta) - s);

        if updated > 1024 {
            updated = 1024;
        }

        *weight = (updated ^ s) - s;
    }
}

fn apply_decorr_passes(
    samples: &mut [i32],
    flags: u32,
    sample_count: usize,
    passes: &[DecorrPass],
) -> Result<()> {
    let channels = if is_mono(flags) { 1 } else { 2 };

    if samples.len() != sample_count * channels {
        return decode_error("wavpack: invalid decorrelation buffer length");
    }

    for pass in passes {
        let mut pass = *pass;

        if channels == 1 {
            decorr_mono_pass(&mut pass, samples);
        } else {
            decorr_stereo_pass(&mut pass, samples);
        }
    }

    if channels == 2 && flags & JOINT_STEREO != 0 {
        undo_joint_stereo(samples);
    }

    Ok(())
}

fn undo_joint_stereo(samples: &mut [i32]) {
    for frame in samples.chunks_exact_mut(2) {
        frame[1] -= frame[0] >> 1;
        frame[0] += frame[1];
    }
}

fn apply_hybrid_correction_into(
    residuals: &[DecodedWord],
    flags: u32,
    sample_count: usize,
    passes: &[DecorrPass],
    shaping_info: Option<ShapingInfo>,
    samples: &mut Vec<i32>,
    decorr_passes: &mut Vec<DecorrPass>,
) -> Result<()> {
    if flags & HYBRID_FLAG == 0 {
        return decode_error("wavpack: correction bitstream without hybrid coding");
    }

    let channels = if is_mono(flags) { 1 } else { 2 };

    if residuals.len() != sample_count * channels {
        return decode_error("wavpack: invalid correction buffer length");
    }

    decorr_passes.clear();
    decorr_passes.extend_from_slice(passes);
    samples.clear();
    samples.reserve(residuals.len());

    let mut shaping = shaping_info.unwrap_or_default();
    let mut m = 0;

    if channels == 1 {
        for word in residuals {
            let mut sample = word.value;

            for pass in decorr_passes.iter_mut() {
                sample = decorr_mono_sample(pass, sample, m);
            }

            m = (m + 1) & (MAX_TERM_USIZE - 1);
            samples.push(shape_hybrid_sample(flags, &mut shaping, 0, sample, word.correction));
        }
    } else {
        for frame in residuals.chunks_exact(2) {
            let mut left = frame[0].value;
            let mut right = frame[1].value;
            let corrections = [frame[0].correction, frame[1].correction];
            let mut left_c = 0;
            let mut right_c = 0;

            if flags & CROSS_DECORR != 0 {
                left_c = left.wrapping_add(corrections[0]);
                right_c = right.wrapping_add(corrections[1]);

                for pass in decorr_passes.iter() {
                    (left_c, right_c) = preview_corrected_stereo_sample(pass, left_c, right_c, m);
                }

                if flags & JOINT_STEREO != 0 {
                    right_c = right_c.wrapping_sub(left_c >> 1);
                    left_c = left_c.wrapping_add(right_c);
                }
            }

            for pass in decorr_passes.iter_mut() {
                (left, right) = decorr_stereo_sample(pass, left, right, m);
            }

            m = (m + 1) & (MAX_TERM_USIZE - 1);

            if flags & CROSS_DECORR == 0 {
                left_c = left.wrapping_add(corrections[0]);
                right_c = right.wrapping_add(corrections[1]);

                if flags & JOINT_STEREO != 0 {
                    right_c = right_c.wrapping_sub(left_c >> 1);
                    left_c = left_c.wrapping_add(right_c);
                }
            }

            if flags & JOINT_STEREO != 0 {
                right = right.wrapping_sub(left >> 1);
                left = left.wrapping_add(right);
            }

            if flags & HYBRID_SHAPE != 0 {
                left = shape_corrected_output(flags, &mut shaping, 0, left, left_c);
                right = shape_corrected_output(flags, &mut shaping, 1, right, right_c);
            } else {
                left = left_c;
                right = right_c;
            }

            samples.push(left);
            samples.push(right);
        }
    }

    Ok(())
}

#[cfg(test)]
fn apply_hybrid_correction(
    residuals: &[DecodedWord],
    flags: u32,
    sample_count: usize,
    passes: &[DecorrPass],
    shaping_info: Option<ShapingInfo>,
) -> Result<Vec<i32>> {
    let mut samples = Vec::new();
    let mut decorr_passes = Vec::new();
    apply_hybrid_correction_into(
        residuals,
        flags,
        sample_count,
        passes,
        shaping_info,
        &mut samples,
        &mut decorr_passes,
    )?;
    Ok(samples)
}

fn decode_dsd_into(
    samples: &mut Vec<i32>,
    dsd: DsdInfo<'_>,
    flags: u32,
    block_samples: usize,
    scratch: &mut DsdScratch,
) -> Result<()> {
    let channels = if flags & MONO_FLAG != 0 { 1 } else { 2 };
    let total = block_samples.saturating_mul(channels);
    samples.clear();
    samples.reserve(total);

    match dsd.mode {
        0 => {
            if dsd.payload.len() < total {
                return decode_error("wavpack: truncated dsd payload");
            }
            samples.extend(dsd.payload[..total].iter().copied().map(i32::from));
        }
        1 => decode_dsd_mode_fast_into(samples, dsd.payload, total, channels, scratch)?,
        3 => decode_dsd_mode_high_into(samples, dsd.payload, total, channels, scratch)?,
        _ => return unsupported_error("wavpack: unknown compressed dsd mode"),
    }

    Ok(())
}

fn decode_dsd_mode_fast_into(
    output: &mut Vec<i32>,
    payload: &[u8],
    total: usize,
    channels: usize,
    scratch: &mut DsdScratch,
) -> Result<()> {
    if payload.len() < 2 {
        return decode_error("wavpack: invalid dsd fast payload");
    }

    let mut cursor = 0usize;
    let history_bits = payload[cursor];
    cursor += 1;
    if history_bits > MAX_HISTORY_BITS {
        return decode_error("wavpack: invalid dsd history bits");
    }
    let history_bins = 1usize << history_bits;

    let max_probability = payload[cursor];
    cursor += 1;

    let table_len = history_bins * 256;
    scratch.probabilities.clear();
    scratch.probabilities.resize(table_len, 0);
    let probabilities = &mut scratch.probabilities;

    if max_probability < 0xff {
        let mut out_idx = 0usize;
        while out_idx < table_len && cursor < payload.len() {
            let code = payload[cursor];
            cursor += 1;
            if code > max_probability {
                let zcount = usize::from(code - max_probability);
                let fill = zcount.min(table_len - out_idx);
                out_idx += fill;
            } else if code != 0 {
                probabilities[out_idx] = code;
                out_idx += 1;
            } else {
                break;
            }
        }
        if out_idx < table_len {
            return decode_error("wavpack: invalid dsd probability table");
        }
        if cursor < payload.len() {
            if payload[cursor] != 0 {
                return decode_error("wavpack: invalid dsd probability terminator");
            }
            cursor += 1;
        }
    } else {
        if payload.len().saturating_sub(cursor) < table_len {
            return decode_error("wavpack: invalid dsd probability table size");
        }
        probabilities.copy_from_slice(&payload[cursor..cursor + table_len]);
        cursor += table_len;
    }

    scratch.summed.clear();
    scratch.summed.resize(table_len, 0);
    scratch.offsets.clear();
    scratch.offsets.resize(history_bins, usize::MAX);
    scratch.lookup.clear();
    scratch.lookup.reserve(history_bins * MAX_BYTES_PER_BIN);
    let summed = &mut scratch.summed;
    let offsets = &mut scratch.offsets;
    let lookup = &mut scratch.lookup;

    for bi in 0..history_bins {
        let base = bi * 256;
        let mut sum = 0u32;
        for i in 0..256 {
            sum = sum.wrapping_add(u32::from(probabilities[base + i]));
            summed[base + i] = sum as u16;
        }
        if sum != 0 {
            if sum as usize > MAX_BYTES_PER_BIN {
                return decode_error("wavpack: invalid dsd summed probabilities");
            }
            offsets[bi] = lookup.len();
            for i in 0..256 {
                let c = probabilities[base + i];
                if c != 0 {
                    lookup.extend(std::iter::repeat_n(i as u8, usize::from(c)));
                }
            }
        }
    }

    if payload.len().saturating_sub(cursor) < 4 {
        return decode_error("wavpack: truncated dsd fast range state");
    }

    let mut value = 0u32;
    for _ in 0..4 {
        value = (value << 8) | u32::from(payload[cursor]);
        cursor += 1;
    }
    let mut low = 0u32;
    let mut high = u32::MAX;
    let mut p0 = 0usize;
    let mut p1 = 0usize;
    let mono = channels == 1;

    for _ in 0..total {
        let base = p0 * 256;
        let sum_total = u32::from(summed[base + 255]);
        if sum_total == 0 {
            return decode_error("wavpack: invalid dsd probability state");
        }

        let mut mult = (high.wrapping_sub(low)) / sum_total;
        if mult == 0 {
            if payload.len().saturating_sub(cursor) < 4 {
                return decode_error("wavpack: truncated dsd fast range state");
            }
            value = 0;
            for _ in 0..4 {
                value = (value << 8) | u32::from(payload[cursor]);
                cursor += 1;
            }
            low = 0;
            high = u32::MAX;
            mult = high / sum_total;
            if mult == 0 {
                return decode_error("wavpack: invalid dsd range multiplier");
            }
        }

        let index = value.wrapping_sub(low) / mult;
        if index >= sum_total {
            return decode_error("wavpack: invalid dsd range index");
        }

        let off = offsets[p0];
        if off == usize::MAX {
            return decode_error("wavpack: missing dsd lookup bin");
        }
        let code = lookup[off + index as usize];
        output.push(i32::from(code));

        if code != 0 {
            low = low
                .wrapping_add(u32::from(summed[base + usize::from(code) - 1]).wrapping_mul(mult));
        }
        high = low
            .wrapping_add(u32::from(probabilities[base + usize::from(code)]).wrapping_mul(mult))
            .wrapping_sub(1);

        if mono {
            p0 = usize::from(code) & (history_bins - 1);
        } else {
            p0 = p1;
            p1 = usize::from(code) & (history_bins - 1);
        }

        while ((high ^ low) & 0xff00_0000) == 0 {
            if cursor >= payload.len() {
                return decode_error("wavpack: truncated dsd fast bitstream");
            }
            value = (value << 8) | u32::from(payload[cursor]);
            cursor += 1;
            high = (high << 8) | 0xff;
            low <<= 8;
        }
    }

    Ok(())
}

fn init_dsd_ptable(table: &mut [i32; PTABLE_BINS], rate_i: u8) {
    let mut value = 0x808000i32;
    let mut rate = i32::from(rate_i) << 8;
    let mut c = (rate + 128) >> 8;
    while c > 0 {
        value += (DOWN - value) >> DECAY;
        c -= 1;
    }
    for i in 0..(PTABLE_BINS / 2) {
        table[i] = value;
        table[PTABLE_BINS - 1 - i] = 0x100ffff - value;
        if value > 0x010000 {
            rate += (rate * i32::from(RATE_S) + 128) >> 8;
            c = (rate + 64) >> 7;
            while c > 0 {
                value += (DOWN - value) >> DECAY;
                c -= 1;
            }
        }
    }
}

fn decode_dsd_mode_high_into(
    output: &mut Vec<i32>,
    payload: &[u8],
    total: usize,
    channels: usize,
    scratch: &mut DsdScratch,
) -> Result<()> {
    let header_len = 2 + channels * 7 + 4;
    if payload.len() < header_len {
        return decode_error("wavpack: invalid dsd high payload");
    }
    let mut cursor = 0usize;
    let rate_i = payload[cursor];
    cursor += 1;
    let rate_s = payload[cursor];
    cursor += 1;
    if rate_s != RATE_S {
        return decode_error("wavpack: unsupported dsd high slope");
    }

    let mut filters = [DsdFilterState::default(), DsdFilterState::default()];
    for state in filters.iter_mut().take(channels) {
        state.filter1 = i32::from(payload[cursor]) << (PRECISION - 8);
        cursor += 1;
        state.filter2 = i32::from(payload[cursor]) << (PRECISION - 8);
        cursor += 1;
        state.filter3 = i32::from(payload[cursor]) << (PRECISION - 8);
        cursor += 1;
        state.filter4 = i32::from(payload[cursor]) << (PRECISION - 8);
        cursor += 1;
        state.filter5 = i32::from(payload[cursor]) << (PRECISION - 8);
        cursor += 1;
        state.filter6 = 0;
        let factor = i16::from_le_bytes([payload[cursor], payload[cursor + 1]]);
        cursor += 2;
        state.factor = i32::from(factor);
    }

    let mut value = 0u32;
    for _ in 0..4 {
        value = (value << 8) | u32::from(payload[cursor]);
        cursor += 1;
    }
    let mut low = 0u32;
    let mut high = u32::MAX;
    init_dsd_ptable(&mut scratch.ptable, rate_i);
    let ptable = &mut scratch.ptable;

    let frames = total / channels;
    for _ in 0..frames {
        for state in filters.iter_mut().take(channels) {
            state.value = state.filter1 - state.filter5 + ((state.filter6 * state.factor) >> 2);
        }

        for _ in 0..8 {
            for state in filters.iter_mut().take(channels) {
                let pidx = ((state.value >> (PRECISION - PRECISION_USE)) as usize) & PTABLE_MASK;
                let pp = &mut ptable[pidx];
                let split = low
                    .wrapping_add(((high.wrapping_sub(low)) >> 8).wrapping_mul((*pp >> 16) as u32));
                let filter0 = if value <= split {
                    high = split;
                    *pp += (UP - *pp) >> DECAY;
                    -1
                } else {
                    low = split.wrapping_add(1);
                    *pp += (DOWN - *pp) >> DECAY;
                    0
                };

                while ((high ^ low) & 0xff00_0000) == 0 {
                    if cursor >= payload.len() {
                        return decode_error("wavpack: truncated dsd high bitstream");
                    }
                    value = (value << 8) | u32::from(payload[cursor]);
                    cursor += 1;
                    high = (high << 8) | 0xff;
                    low <<= 8;
                }

                state.value += state.filter6 * 8;
                state.byte = (state.byte << 1) | ((filter0 & 1) as u8);
                state.factor += (((state.value ^ filter0) >> 31) | 1)
                    & ((state.value ^ (state.value - (state.filter6 * 16))) >> 31);
                state.filter1 += ((filter0 & VALUE_ONE) - state.filter1) >> 6;
                state.filter2 += ((filter0 & VALUE_ONE) - state.filter2) >> 4;
                state.filter3 += (state.filter2 - state.filter3) >> 4;
                state.filter4 += (state.filter3 - state.filter4) >> 4;
                state.value = (state.filter4 - state.filter5) >> 4;
                state.filter5 += state.value;
                state.filter6 += (state.value - state.filter6) >> 3;
                state.value = state.filter1 - state.filter5 + ((state.filter6 * state.factor) >> 2);
            }
        }

        for state in filters.iter_mut().take(channels) {
            output.push(i32::from(state.byte));
            state.factor -= (state.factor + 512) >> 10;
        }
    }

    Ok(())
}

fn dsd_crc(samples: &[i32]) -> u32 {
    let mut crc = 0u32;
    for &sample in samples {
        crc = crc.wrapping_add((crc << 1).wrapping_add((sample as u32) & 0xff));
    }
    crc
}

fn shape_hybrid_sample(
    flags: u32,
    shaping: &mut ShapingInfo,
    channel: usize,
    lossy: i32,
    correction: i32,
) -> i32 {
    if flags & HYBRID_SHAPE == 0 {
        return lossy.wrapping_add(correction);
    }

    let corrected = lossy.wrapping_add(correction);
    shape_corrected_output(flags, shaping, channel, lossy, corrected)
}

fn shape_corrected_output(
    flags: u32,
    shaping: &mut ShapingInfo,
    channel: usize,
    _lossy: i32,
    corrected: i32,
) -> i32 {
    let correction = corrected.wrapping_sub(_lossy);
    let shaping_acc = shaping.shaping_acc[channel].wrapping_add(shaping.shaping_delta[channel]);
    shaping.shaping_acc[channel] = shaping_acc;

    let shaping_weight = shaping_acc >> 16;
    let mut temp = apply_weight(shaping_weight, shaping.error[channel]).wrapping_neg();

    if flags & NEW_SHAPING != 0 && shaping_weight < 0 && temp != 0 {
        if temp == shaping.error[channel] {
            temp = if temp < 0 { temp.wrapping_add(1) } else { temp.wrapping_sub(1) };
        }

        shaping.error[channel] = temp.wrapping_sub(correction);
    } else {
        shaping.error[channel] = correction.wrapping_neg();
    }

    corrected.wrapping_sub(temp)
}

fn decorr_mono_sample(pass: &mut DecorrPass, residual: i32, m: usize) -> i32 {
    let (sam, k) = if pass.term > MAX_TERM {
        let sam = if pass.term & 1 != 0 {
            predict_term_17(pass.samples_a[0], pass.samples_a[1])
        } else {
            predict_term_18(pass.samples_a[0], pass.samples_a[1])
        };

        pass.samples_a[1] = pass.samples_a[0];
        (sam, 0)
    } else {
        (pass.samples_a[m], (m + pass.term as usize) & (MAX_TERM_USIZE - 1))
    };

    let sample = apply_weight(pass.weight_a, sam).wrapping_add(residual);
    update_weight(&mut pass.weight_a, pass.delta, sam, residual);
    pass.samples_a[k] = sample;
    sample
}

fn preview_corrected_stereo_sample(
    pass: &DecorrPass,
    mut left: i32,
    mut right: i32,
    m: usize,
) -> (i32, i32) {
    if pass.term > 0 {
        let (sam_a, sam_b) = if pass.term > MAX_TERM {
            if pass.term & 1 != 0 {
                (
                    predict_term_17(pass.samples_a[0], pass.samples_a[1]),
                    predict_term_17(pass.samples_b[0], pass.samples_b[1]),
                )
            } else {
                (
                    predict_term_18(pass.samples_a[0], pass.samples_a[1]),
                    predict_term_18(pass.samples_b[0], pass.samples_b[1]),
                )
            }
        } else {
            (pass.samples_a[m], pass.samples_b[m])
        };

        left = left.wrapping_add(apply_weight(pass.weight_a, sam_a));
        right = right.wrapping_add(apply_weight(pass.weight_b, sam_b));
    } else if pass.term == -1 {
        left = left.wrapping_add(apply_weight(pass.weight_a, pass.samples_a[0]));
        right = right.wrapping_add(apply_weight(pass.weight_b, left));
    } else {
        right = right.wrapping_add(apply_weight(pass.weight_b, pass.samples_b[0]));

        if pass.term == -3 {
            left = left.wrapping_add(apply_weight(pass.weight_a, pass.samples_a[0]));
        } else {
            left = left.wrapping_add(apply_weight(pass.weight_a, right));
        }
    }

    (left, right)
}

fn decorr_stereo_sample(
    pass: &mut DecorrPass,
    mut left: i32,
    mut right: i32,
    m: usize,
) -> (i32, i32) {
    if pass.term > 0 {
        let (sam_a, sam_b, k) = if pass.term > MAX_TERM {
            let (sam_a, sam_b) = if pass.term & 1 != 0 {
                (
                    predict_term_17(pass.samples_a[0], pass.samples_a[1]),
                    predict_term_17(pass.samples_b[0], pass.samples_b[1]),
                )
            } else {
                (
                    predict_term_18(pass.samples_a[0], pass.samples_a[1]),
                    predict_term_18(pass.samples_b[0], pass.samples_b[1]),
                )
            };

            pass.samples_a[1] = pass.samples_a[0];
            pass.samples_b[1] = pass.samples_b[0];
            (sam_a, sam_b, 0)
        } else {
            (pass.samples_a[m], pass.samples_b[m], (m + pass.term as usize) & (MAX_TERM_USIZE - 1))
        };

        let left2 = apply_weight(pass.weight_a, sam_a).wrapping_add(left);
        let right2 = apply_weight(pass.weight_b, sam_b).wrapping_add(right);

        update_weight(&mut pass.weight_a, pass.delta, sam_a, left);
        update_weight(&mut pass.weight_b, pass.delta, sam_b, right);

        pass.samples_a[k] = left2;
        pass.samples_b[k] = right2;
        left = left2;
        right = right2;
    } else if pass.term == -1 {
        let left2 = left.wrapping_add(apply_weight(pass.weight_a, pass.samples_a[0]));
        update_weight_clip(&mut pass.weight_a, pass.delta, pass.samples_a[0], left);
        left = left2;

        let right2 = right.wrapping_add(apply_weight(pass.weight_b, left2));
        update_weight_clip(&mut pass.weight_b, pass.delta, left2, right);
        pass.samples_a[0] = right2;
        right = right2;
    } else {
        let mut right2 = right.wrapping_add(apply_weight(pass.weight_b, pass.samples_b[0]));
        update_weight_clip(&mut pass.weight_b, pass.delta, pass.samples_b[0], right);
        right = right2;

        if pass.term == -3 {
            right2 = pass.samples_a[0];
            pass.samples_a[0] = right;
        }

        let left2 = left.wrapping_add(apply_weight(pass.weight_a, right2));
        update_weight_clip(&mut pass.weight_a, pass.delta, right2, left);
        pass.samples_b[0] = left2;
        left = left2;
    }

    (left, right)
}

fn fixup_samples(
    samples: &mut [i32],
    flags: u32,
    float_info: Option<FloatInfo>,
    int32_info: Option<Int32Info>,
    wvx_bitstream: Option<&[u8]>,
    wvx_new_bitstream: bool,
    has_correction: bool,
) -> Result<()> {
    if flags & FLOAT_DATA != 0 {
        let info = float_info.ok_or(Error::DecodeError("wavpack: missing float info"))?;
        restore_float_values(samples, info, wvx_bitstream, wvx_new_bitstream)?;
        return Ok(());
    }

    let lossy = flags & HYBRID_FLAG != 0 && !has_correction;
    let mut shift = (flags & SHIFT_MASK) >> SHIFT_LSB;

    if flags & INT32_DATA != 0 {
        let info = int32_info.ok_or(Error::DecodeError("wavpack: missing int32 info"))?;

        if let Some(wvx_bitstream) = wvx_bitstream {
            restore_int32_low_bits(samples, info, wvx_bitstream, wvx_new_bitstream)?;
        } else if info.sent_bits == 0 && (info.zeros + info.ones + info.dups) != 0 {
            let mut zeros = info.zeros;
            let mut ones = info.ones;
            let mut dups = info.dups;

            while lossy && flags & BYTES_STORED == 3 && shift < 8 {
                if zeros != 0 {
                    zeros -= 1;
                } else if ones != 0 {
                    ones -= 1;
                } else if dups != 0 {
                    dups -= 1;
                } else {
                    break;
                }

                shift += 1;
            }

            for sample in samples.iter_mut() {
                apply_int32_redundancy(sample, zeros, ones, dups);
            }
        } else {
            shift += info.zeros + info.sent_bits + info.ones + info.dups;
        }
    }

    shift &= 0x1f;

    if lossy {
        let (min_value, max_value) = match flags & BYTES_STORED {
            0 => (-128 >> shift, 127 >> shift),
            1 => (-32768 >> shift, 32767 >> shift),
            2 => (-8388608 >> shift, 8388607 >> shift),
            _ => (i32::MIN >> shift, i32::MAX >> shift),
        };
        let min_shifted = ((min_value as u32) << shift) as i32;
        let max_shifted = ((max_value as u32) << shift) as i32;

        for sample in samples {
            if *sample < min_value {
                *sample = min_shifted;
            } else if *sample > max_value {
                *sample = max_shifted;
            } else {
                *sample = ((*sample as u32) << shift) as i32;
            }
        }
    } else if shift != 0 {
        for sample in samples {
            *sample = ((*sample as u32) << shift) as i32;
        }
    }

    Ok(())
}

fn expand_false_stereo(samples: &mut Vec<i32>, flags: u32) {
    if flags & FALSE_STEREO == 0 {
        return;
    }

    let old_len = samples.len();
    samples.resize(old_len * 2, 0);

    for i in (0..old_len).rev() {
        let sample = samples[i];
        samples[2 * i] = sample;
        samples[2 * i + 1] = sample;
    }
}

fn restore_int32_low_bits(
    samples: &mut [i32],
    info: Int32Info,
    wvx_bitstream: &[u8],
    wvx_new_bitstream: bool,
) -> Result<()> {
    let mut bits = WavPackBitReader::new(wvx_bitstream);
    let max_width = if wvx_new_bitstream { bits.read_bits(5)? & 0x1f } else { 0 };
    let mask = (1u32 << info.sent_bits) - 1;

    for sample in samples {
        if info.sent_bits != 0 {
            if max_width != 0 {
                let pvalue = if *sample < 0 { !*sample } else { *sample } as u32;
                let width = if pvalue == 0 { 0 } else { 32 - pvalue.leading_zeros() };
                let mut bits_to_read = info.sent_bits as i32;
                bits_to_read -= (width + info.sent_bits).saturating_sub(max_width) as i32;

                if bits_to_read > 0 {
                    let bits_to_read = bits_to_read as u32;
                    let data = bits.read_bits(bits_to_read)? & ((1u32 << bits_to_read) - 1);
                    *sample = (((*sample as u32) << bits_to_read) | data)
                        .wrapping_shl(info.sent_bits - bits_to_read)
                        as i32;
                } else {
                    *sample = ((*sample as u32) << info.sent_bits) as i32;
                }
            } else {
                let data = bits.read_bits(info.sent_bits)? & mask;
                *sample = (((*sample as u32) << info.sent_bits) | data) as i32;
            }
        }

        apply_int32_redundancy(sample, info.zeros, info.ones, info.dups);
    }

    Ok(())
}

fn apply_int32_redundancy(sample: &mut i32, zeros: u32, ones: u32, dups: u32) {
    if zeros != 0 {
        *sample = ((*sample as u32) << zeros) as i32;
    } else if ones != 0 {
        *sample = (((*sample).wrapping_add(1) as u32) << ones).wrapping_sub(1) as i32;
    } else if dups != 0 {
        let low_bit = *sample & 1;
        *sample =
            (((*sample).wrapping_add(low_bit) as u32) << dups).wrapping_sub(low_bit as u32) as i32;
    }
}

fn restore_float_values(
    samples: &mut [i32],
    info: FloatInfo,
    wvx_bitstream: Option<&[u8]>,
    wvx_new_bitstream: bool,
) -> Result<()> {
    if let Some(wvx_bitstream) = wvx_bitstream {
        restore_float_values_wvx(samples, info, wvx_bitstream, wvx_new_bitstream)
    } else {
        restore_float_values_nowvx(samples, info);
        Ok(())
    }
}

fn restore_float_values_nowvx(samples: &mut [i32], info: FloatInfo) {
    for sample in samples {
        let mut value = *sample;
        let mut out = 0u32;

        if value != 0 {
            let mut shift_count = 0;
            let mut exp = info.max_exp;
            value = ((value as u32) << (u32::from(info.shift) & 0x1f)) as i32;

            if value < 0 {
                value = value.wrapping_neg();
                out |= 1 << 31;
            }

            let mut mag = value as u32;

            if mag >= 0x1000000 {
                while mag & 0x0f00_0000 != 0 {
                    mag >>= 1;
                    exp = exp.wrapping_add(1);
                }
            } else if exp != 0 {
                while mag & 0x800000 == 0 && exp != 0 {
                    exp = exp.wrapping_sub(1);
                    if exp == 0 {
                        break;
                    }
                    shift_count += 1;
                    mag <<= 1;
                }

                shift_count &= 0x1f;
                if shift_count != 0 && info.flags & FLOAT_SHIFT_ONES != 0 {
                    mag |= (1u32 << shift_count) - 1;
                }
            }

            out |= mag & 0x7fffff;
            out |= u32::from(exp) << 23;
        }

        *sample = out as i32;
    }
}

fn restore_float_values_wvx(
    samples: &mut [i32],
    info: FloatInfo,
    wvx_bitstream: &[u8],
    wvx_new_bitstream: bool,
) -> Result<()> {
    let mut bits = WavPackBitReader::new(wvx_bitstream);
    let (min_shifted_zeros, max_shifted_ones) = if wvx_new_bitstream {
        (bits.read_bits(5)? & 0x1f, bits.read_bits(5)? & 0x1f)
    } else {
        (0, 0)
    };

    for sample in samples {
        let mut value = *sample;
        let mut out = 0u32;
        let mut exp = u32::from(info.max_exp);

        if value == 0 {
            if info.flags & FLOAT_ZEROS_SENT != 0 {
                if bits.read_bit()? != 0 {
                    let mantissa = bits.read_bits(23)?;
                    out |= mantissa & 0x7fffff;

                    if exp >= 25 {
                        exp = bits.read_bits(8)?;
                    }

                    out |= (bits.read_bit()? & 1) << 31;
                } else if info.flags & FLOAT_NEG_ZEROS != 0 {
                    out |= (bits.read_bit()? & 1) << 31;
                }
            }
        } else {
            value = ((value as u32) << (u32::from(info.shift) & 0x1f)) as i32;

            if value < 0 {
                value = value.wrapping_neg();
                out |= 1 << 31;
            }

            let mut mag = value as u32;

            if mag == 0x1000000 {
                if bits.read_bit()? != 0 {
                    out |= bits.read_bits(23)? & 0x7fffff;
                }

                exp = 255;
            } else {
                let mut shift_count = 0;

                if exp != 0 {
                    while mag & 0x800000 == 0 && exp != 0 {
                        exp -= 1;
                        if exp == 0 {
                            break;
                        }
                        shift_count += 1;
                        mag <<= 1;
                    }
                }

                shift_count &= 0x1f;

                if shift_count != 0 {
                    if info.flags & FLOAT_SHIFT_ONES != 0
                        || (info.flags & FLOAT_SHIFT_SAME != 0 && bits.read_bit()? != 0)
                    {
                        mag |= (1u32 << shift_count) - 1;
                    } else if info.flags & FLOAT_SHIFT_SENT != 0 {
                        let mask = (1u32 << shift_count) - 1;
                        let mut num_zeros = 0;

                        if max_shifted_ones != 0 && shift_count > max_shifted_ones {
                            num_zeros = shift_count - max_shifted_ones;
                        }

                        if min_shifted_zeros > num_zeros {
                            num_zeros = if min_shifted_zeros > shift_count {
                                shift_count
                            } else {
                                min_shifted_zeros
                            };
                        }

                        shift_count -= num_zeros;

                        if shift_count > 0 {
                            let data = bits.read_bits(shift_count)?;
                            mag |= (data << num_zeros) & mask;
                        }
                    }
                }

                out |= mag & 0x7fffff;
            }

            out |= (exp & 0xff) << 23;
        }

        *sample = out as i32;
    }

    Ok(())
}

fn prepare_output_buffer(buf: &mut GenericAudioBuffer, frames: usize) -> Result<()> {
    if buf.is_empty() {
        buf.grow_capacity(frames);
        buf.resize_uninit(frames);
    } else if buf.frames() != frames {
        return decode_error("wavpack: packet block duration mismatch");
    }

    Ok(())
}

fn write_samples_to_buffer(
    buf: &mut GenericAudioBuffer,
    samples: &[i32],
    frames: usize,
    channels: usize,
    channel_offset: usize,
) -> Result<()> {
    if samples.len() != frames * channels {
        return decode_error("wavpack: invalid output sample count");
    }

    prepare_output_buffer(buf, frames)?;

    if channel_offset + channels > buf.spec().channels().count() {
        return decode_error("wavpack: packet has too many channels");
    }

    match buf {
        GenericAudioBuffer::U8(buf) => {
            write_integer_samples(buf, samples, frames, channels, channel_offset, |sample| {
                sample as u8
            })
        }
        GenericAudioBuffer::S8(buf) => {
            write_integer_samples(buf, samples, frames, channels, channel_offset, |sample| {
                sample as i8
            })
        }
        GenericAudioBuffer::S16(buf) => {
            write_integer_samples(buf, samples, frames, channels, channel_offset, |sample| {
                sample as i16
            })
        }
        GenericAudioBuffer::S24(buf) => {
            write_integer_samples(buf, samples, frames, channels, channel_offset, i24::from)
        }
        GenericAudioBuffer::S32(buf) => {
            write_integer_samples(buf, samples, frames, channels, channel_offset, |sample| sample)
        }
        GenericAudioBuffer::F32(buf) => {
            write_integer_samples(buf, samples, frames, channels, channel_offset, |sample| {
                f32::from_bits(sample as u32)
            })
        }
        _ => unsupported_error("wavpack: unsupported output sample format"),
    }
}

fn write_integer_samples<S, F>(
    buf: &mut symphonia_core::audio::AudioBuffer<S>,
    samples: &[i32],
    frames: usize,
    channels: usize,
    channel_offset: usize,
    mut convert: F,
) -> Result<()>
where
    S: symphonia_core::audio::sample::Sample,
    F: FnMut(i32) -> S,
{
    match channels {
        1 => {
            let plane = buf
                .plane_mut(channel_offset)
                .ok_or(Error::DecodeError("wavpack: invalid output channel"))?;

            for (dst, &sample) in plane.iter_mut().zip(samples.iter()).take(frames) {
                *dst = convert(sample);
            }
        }
        2 => {
            let (left, right) = buf
                .plane_pair_mut(channel_offset, channel_offset + 1)
                .ok_or(Error::DecodeError("wavpack: invalid output channels"))?;

            for (frame, lr) in samples.chunks_exact(2).enumerate().take(frames) {
                left[frame] = convert(lr[0]);
                right[frame] = convert(lr[1]);
            }
        }
        _ => return decode_error("wavpack: invalid output channel count"),
    }

    Ok(())
}

fn generic_buffer_format(buf: &GenericAudioBuffer) -> SampleFormat {
    match buf {
        GenericAudioBuffer::U8(_) => SampleFormat::U8,
        GenericAudioBuffer::U16(_) => SampleFormat::U16,
        GenericAudioBuffer::U24(_) => SampleFormat::U24,
        GenericAudioBuffer::U32(_) => SampleFormat::U32,
        GenericAudioBuffer::S8(_) => SampleFormat::S8,
        GenericAudioBuffer::S16(_) => SampleFormat::S16,
        GenericAudioBuffer::S24(_) => SampleFormat::S24,
        GenericAudioBuffer::S32(_) => SampleFormat::S32,
        GenericAudioBuffer::F32(_) => SampleFormat::F32,
        GenericAudioBuffer::F64(_) => SampleFormat::F64,
    }
}

fn decorr_mono_pass(pass: &mut DecorrPass, samples: &mut [i32]) {
    match pass.term {
        17 => {
            for sample in samples {
                let sam_a = predict_term_17(pass.samples_a[0], pass.samples_a[1]);
                pass.samples_a[1] = pass.samples_a[0];
                let residual = *sample;
                pass.samples_a[0] = apply_weight(pass.weight_a, sam_a).wrapping_add(residual);
                update_weight(&mut pass.weight_a, pass.delta, sam_a, residual);
                *sample = pass.samples_a[0];
            }
        }
        18 => {
            for sample in samples {
                let sam_a = predict_term_18(pass.samples_a[0], pass.samples_a[1]);
                pass.samples_a[1] = pass.samples_a[0];
                let residual = *sample;
                pass.samples_a[0] = apply_weight(pass.weight_a, sam_a).wrapping_add(residual);
                update_weight(&mut pass.weight_a, pass.delta, sam_a, residual);
                *sample = pass.samples_a[0];
            }
        }
        _ => {
            let mut m = 0;
            let mut k = pass.term as usize & (MAX_TERM_USIZE - 1);

            for sample in samples {
                let sam_a = pass.samples_a[m];
                let residual = *sample;
                pass.samples_a[k] = apply_weight(pass.weight_a, sam_a).wrapping_add(residual);
                update_weight(&mut pass.weight_a, pass.delta, sam_a, residual);
                *sample = pass.samples_a[k];
                m = (m + 1) & (MAX_TERM_USIZE - 1);
                k = (k + 1) & (MAX_TERM_USIZE - 1);
            }

            if m != 0 {
                let temp_samples = pass.samples_a;

                for (k, sample) in pass.samples_a.iter_mut().enumerate() {
                    *sample = temp_samples[(m + k) & (MAX_TERM_USIZE - 1)];
                }
            }
        }
    }
}

fn decorr_stereo_pass(pass: &mut DecorrPass, samples: &mut [i32]) {
    match pass.term {
        17 => {
            for frame in samples.chunks_exact_mut(2) {
                let sam = predict_term_17(pass.samples_a[0], pass.samples_a[1]);
                pass.samples_a[1] = pass.samples_a[0];
                let residual = frame[0];
                pass.samples_a[0] = apply_weight(pass.weight_a, sam).wrapping_add(residual);
                update_weight(&mut pass.weight_a, pass.delta, sam, residual);
                frame[0] = pass.samples_a[0];

                let sam = predict_term_17(pass.samples_b[0], pass.samples_b[1]);
                pass.samples_b[1] = pass.samples_b[0];
                let residual = frame[1];
                pass.samples_b[0] = apply_weight(pass.weight_b, sam).wrapping_add(residual);
                update_weight(&mut pass.weight_b, pass.delta, sam, residual);
                frame[1] = pass.samples_b[0];
            }
        }
        18 => {
            for frame in samples.chunks_exact_mut(2) {
                let sam = predict_term_18_stereo(pass.samples_a[0], pass.samples_a[1]);
                pass.samples_a[1] = pass.samples_a[0];
                let residual = frame[0];
                pass.samples_a[0] = apply_weight(pass.weight_a, sam).wrapping_add(residual);
                update_weight(&mut pass.weight_a, pass.delta, sam, residual);
                frame[0] = pass.samples_a[0];

                let sam = predict_term_18_stereo(pass.samples_b[0], pass.samples_b[1]);
                pass.samples_b[1] = pass.samples_b[0];
                let residual = frame[1];
                pass.samples_b[0] = apply_weight(pass.weight_b, sam).wrapping_add(residual);
                update_weight(&mut pass.weight_b, pass.delta, sam, residual);
                frame[1] = pass.samples_b[0];
            }
        }
        -1 => {
            for frame in samples.chunks_exact_mut(2) {
                let sam = frame[0].wrapping_add(apply_weight(pass.weight_a, pass.samples_a[0]));
                update_weight_clip(&mut pass.weight_a, pass.delta, pass.samples_a[0], frame[0]);
                frame[0] = sam;
                pass.samples_a[0] = frame[1].wrapping_add(apply_weight(pass.weight_b, sam));
                update_weight_clip(&mut pass.weight_b, pass.delta, sam, frame[1]);
                frame[1] = pass.samples_a[0];
            }
        }
        -2 => {
            for frame in samples.chunks_exact_mut(2) {
                let sam = frame[1].wrapping_add(apply_weight(pass.weight_b, pass.samples_b[0]));
                update_weight_clip(&mut pass.weight_b, pass.delta, pass.samples_b[0], frame[1]);
                frame[1] = sam;
                pass.samples_b[0] = frame[0].wrapping_add(apply_weight(pass.weight_a, sam));
                update_weight_clip(&mut pass.weight_a, pass.delta, sam, frame[0]);
                frame[0] = pass.samples_b[0];
            }
        }
        -3 => {
            for frame in samples.chunks_exact_mut(2) {
                let sam_a = frame[0].wrapping_add(apply_weight(pass.weight_a, pass.samples_a[0]));
                update_weight_clip(&mut pass.weight_a, pass.delta, pass.samples_a[0], frame[0]);
                let sam_b = frame[1].wrapping_add(apply_weight(pass.weight_b, pass.samples_b[0]));
                update_weight_clip(&mut pass.weight_b, pass.delta, pass.samples_b[0], frame[1]);
                pass.samples_b[0] = sam_a;
                pass.samples_a[0] = sam_b;
                frame[0] = sam_a;
                frame[1] = sam_b;
            }
        }
        _ => {
            let mut m = 0;
            let mut k = pass.term as usize & (MAX_TERM_USIZE - 1);

            for frame in samples.chunks_exact_mut(2) {
                let sam = pass.samples_a[m];
                let residual = frame[0];
                pass.samples_a[k] = apply_weight(pass.weight_a, sam).wrapping_add(residual);
                update_weight(&mut pass.weight_a, pass.delta, sam, residual);
                frame[0] = pass.samples_a[k];

                let sam = pass.samples_b[m];
                let residual = frame[1];
                pass.samples_b[k] = apply_weight(pass.weight_b, sam).wrapping_add(residual);
                update_weight(&mut pass.weight_b, pass.delta, sam, residual);
                frame[1] = pass.samples_b[k];

                m = (m + 1) & (MAX_TERM_USIZE - 1);
                k = (k + 1) & (MAX_TERM_USIZE - 1);
            }
        }
    }
}

fn wp_exp2s(log: i32) -> i32 {
    if log < 0 {
        return !wp_exp2s(-log).wrapping_sub(1);
    }

    let value = u32::from(EXP2_TABLE[(log & 0xff) as usize]) | 0x100;
    let log = log >> 8;

    if log <= 9 { (value >> (9 - log)) as i32 } else { (value << ((log - 9) & 0x1f)) as i32 }
}

fn wp_log2(mut value: u32) -> i32 {
    value = value.wrapping_add(value >> 9);

    if value < (1 << 8) {
        let dbits = if value == 0 { 0 } else { 32 - value.leading_zeros() };
        (dbits << 8) as i32 + i32::from(LOG2_TABLE[((value << (9 - dbits)) & 0xff) as usize])
    } else {
        let dbits = 32 - value.leading_zeros();
        (dbits << 8) as i32 + i32::from(LOG2_TABLE[((value >> (dbits - 9)) & 0xff) as usize])
    }
}

struct WavPackBitReader<'a> {
    data: &'a [u8],
    pos: usize,
    sr: u32,
    bc: u32,
}

impl<'a> WavPackBitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        WavPackBitReader { data, pos: 0, sr: 0, bc: 0 }
    }

    fn read_bit(&mut self) -> Result<u32> {
        if self.bc == 0 {
            let byte =
                self.data.get(self.pos).ok_or(Error::DecodeError("wavpack: bitstream eof"))?;
            self.pos += 1;
            self.sr = u32::from(*byte);
            self.bc = 8;
        }

        let bit = self.sr & 1;
        self.sr >>= 1;
        self.bc -= 1;
        Ok(bit)
    }

    fn ensure_bits(&mut self, nbits: u32) -> Result<()> {
        while self.bc < nbits {
            let byte =
                self.data.get(self.pos).ok_or(Error::DecodeError("wavpack: bitstream eof"))?;
            self.pos += 1;
            self.sr |= u32::from(*byte) << self.bc;
            self.bc += 8;
        }

        Ok(())
    }

    fn read_bits(&mut self, nbits: u32) -> Result<u32> {
        if nbits == 0 {
            return Ok(0);
        }

        self.ensure_bits(nbits)?;

        let value = self.sr & ((1u32 << nbits) - 1);
        self.sr >>= nbits;
        self.bc -= nbits;
        Ok(value)
    }

    fn read_code(&mut self, maxcode: u32) -> Result<u32> {
        if maxcode < 2 {
            return if maxcode == 0 { Ok(0) } else { self.read_bit() };
        }

        let mut bitcount = 32 - maxcode.leading_zeros();
        let extras = (1 << bitcount) - maxcode - 1;
        let low_bits = self.read_bits(bitcount - 1)?;

        if low_bits >= extras {
            let high_bit = self.read_bit()?;
            Ok((low_bits << 1) - extras + high_bit)
        } else {
            bitcount -= 1;
            debug_assert_eq!(bitcount, 32 - maxcode.leading_zeros() - 1);
            Ok(low_bits)
        }
    }

    fn read_limited_ones(&mut self) -> Result<u32> {
        let mut ones = 0;

        while ones < LIMIT_ONES + 1 && self.read_bit()? != 0 {
            ones += 1;
        }

        if ones == LIMIT_ONES + 1 {
            return decode_error("wavpack: invalid ones count");
        }

        if ones == LIMIT_ONES {
            let cbits = self.read_limited_ones()?;

            ones = if cbits < 2 {
                cbits
            } else {
                let mut mask = 1;
                let mut value = 0;

                for _ in 1..cbits {
                    if self.read_bit()? != 0 {
                        value |= mask;
                    }

                    mask <<= 1;
                }

                value | mask
            };

            ones += LIMIT_ONES;
        }

        Ok(ones)
    }
}

#[derive(Clone, Debug)]
struct WordsDecoder {
    entropy: EntropyVars,
    hybrid: Option<HybridProfile>,
    error_limit: [u32; 2],
    flags: u32,
    holding_one: u32,
    holding_zero: bool,
    zeros_acc: u32,
    mono: bool,
}

impl WordsDecoder {
    fn new(entropy: EntropyVars, hybrid: Option<HybridProfile>, flags: u32) -> Self {
        WordsDecoder {
            entropy,
            hybrid,
            error_limit: [0; 2],
            flags,
            holding_one: 0,
            holding_zero: false,
            zeros_acc: 0,
            mono: is_mono(flags),
        }
    }

    fn get_med(&self, chan: usize, med: usize) -> u32 {
        (self.entropy.median[chan][med] >> 4) + 1
    }

    fn inc_med(&mut self, chan: usize, med: usize) {
        let div = match med {
            0 => DIV0,
            1 => DIV1,
            _ => DIV2,
        };

        self.entropy.median[chan][med] += ((self.entropy.median[chan][med] + div) / div) * 5;
    }

    fn dec_med(&mut self, chan: usize, med: usize) {
        let div = match med {
            0 => DIV0,
            1 => DIV1,
            _ => DIV2,
        };

        self.entropy.median[chan][med] -= ((self.entropy.median[chan][med] + (div - 2)) / div) * 2;
    }

    fn medians_are_low(&self) -> bool {
        self.entropy.median[0][0] < 2 && self.entropy.median[1][0] < 2
    }

    fn update_slow_level_for_zero(&mut self, chan: usize) {
        if let Some(hybrid) = self.hybrid.as_mut() {
            hybrid.slow_level[chan] =
                hybrid.slow_level[chan].wrapping_sub((hybrid.slow_level[chan] + SLO) >> SLS);
        }
    }

    fn read_zero_run(&mut self, bs: &mut WavPackBitReader<'_>) -> Result<bool> {
        if self.zeros_acc != 0 {
            self.zeros_acc -= 1;

            return Ok(self.zeros_acc != 0);
        }

        let cbits = bs.read_limited_ones()?;

        self.zeros_acc = if cbits < 2 {
            cbits
        } else {
            let mut mask = 1;
            let mut value = 0;

            for _ in 1..cbits {
                if bs.read_bit()? != 0 {
                    value |= mask;
                }

                mask <<= 1;
            }

            value | mask
        };

        if self.zeros_acc != 0 {
            self.entropy.median = [[0; 3]; 2];
            return Ok(true);
        }

        Ok(false)
    }

    fn update_error_limit(&mut self) {
        let Some(hybrid) = self.hybrid.as_mut() else {
            return;
        };

        hybrid.bitrate_acc[0] = hybrid.bitrate_acc[0].wrapping_add(hybrid.bitrate_delta[0]);
        let mut bitrate_0 = (hybrid.bitrate_acc[0] >> 16) as i32;

        if self.mono {
            self.error_limit[0] = if self.flags & HYBRID_BITRATE != 0 {
                let slow_log_0 = ((hybrid.slow_level[0] + SLO) >> SLS) as i32;
                if slow_log_0 - bitrate_0 > -0x100 {
                    wp_exp2s(slow_log_0 - bitrate_0 + 0x100) as u32
                } else {
                    0
                }
            } else {
                wp_exp2s(bitrate_0) as u32
            };
        } else {
            hybrid.bitrate_acc[1] = hybrid.bitrate_acc[1].wrapping_add(hybrid.bitrate_delta[1]);
            let mut bitrate_1 = (hybrid.bitrate_acc[1] >> 16) as i32;

            if self.flags & HYBRID_BITRATE != 0 {
                let slow_log_0 = ((hybrid.slow_level[0] + SLO) >> SLS) as i32;
                let slow_log_1 = ((hybrid.slow_level[1] + SLO) >> SLS) as i32;

                if self.flags & HYBRID_BALANCE != 0 {
                    let balance = (slow_log_1 - slow_log_0 + bitrate_1 + 1) >> 1;

                    if balance > bitrate_0 {
                        bitrate_1 = bitrate_0 * 2;
                        bitrate_0 = 0;
                    } else if -balance > bitrate_0 {
                        bitrate_0 *= 2;
                        bitrate_1 = 0;
                    } else {
                        bitrate_1 = bitrate_0 + balance;
                        bitrate_0 -= balance;
                    }
                }

                self.error_limit[0] = if slow_log_0 - bitrate_0 > -0x100 {
                    wp_exp2s(slow_log_0 - bitrate_0 + 0x100) as u32
                } else {
                    0
                };

                self.error_limit[1] = if slow_log_1 - bitrate_1 > -0x100 {
                    wp_exp2s(slow_log_1 - bitrate_1 + 0x100) as u32
                } else {
                    0
                };
            } else {
                self.error_limit[0] = wp_exp2s(bitrate_0) as u32;
                self.error_limit[1] = wp_exp2s(bitrate_1) as u32;
            }
        }
    }

    fn read_word(
        &mut self,
        bs: &mut WavPackBitReader<'_>,
        mut correction_bits: Option<&mut WavPackBitReader<'_>>,
        chan: usize,
    ) -> Result<DecodedWord> {
        if self.holding_zero {
            self.holding_zero = false;
            let mut low = 0;
            let mut high = self.get_med(chan, 0) - 1;
            self.dec_med(chan, 0);

            if self.flags & HYBRID_FLAG != 0 && chan == 0 {
                self.update_error_limit();
            }

            if self.error_limit[chan] == 0 {
                low += bs.read_code(high - low)?;
            } else {
                let mut mid = (high + low + 1) >> 1;

                while high - low > self.error_limit[chan] {
                    if bs.read_bit()? != 0 {
                        low = mid;
                    } else {
                        high = mid - 1;
                    }

                    mid = (high + low + 1) >> 1;
                }

                low = mid;
            }

            let sign = bs.read_bit()?;

            let correction = if let Some(correction_bits) = correction_bits.as_mut() {
                if self.error_limit[chan] != 0 {
                    let value = correction_bits.read_code(high - low)? + low;
                    if sign != 0 { low as i32 - value as i32 } else { value as i32 - low as i32 }
                } else {
                    0
                }
            } else {
                0
            };

            if self.flags & HYBRID_BITRATE != 0 {
                if let Some(hybrid) = self.hybrid.as_mut() {
                    hybrid.slow_level[chan] = hybrid.slow_level[chan]
                        .wrapping_sub((hybrid.slow_level[chan] + SLO) >> SLS);
                    hybrid.slow_level[chan] =
                        hybrid.slow_level[chan].wrapping_add(wp_log2(low) as u32);
                }
            }

            return Ok(DecodedWord {
                value: if sign != 0 { !(low as i32) } else { low as i32 },
                correction,
            });
        }

        if self.medians_are_low() && self.holding_one == 0 && self.read_zero_run(bs)? {
            self.update_slow_level_for_zero(chan);
            return Ok(DecodedWord { value: 0, correction: 0 });
        }

        let raw_ones = bs.read_limited_ones()?;
        let mut ones_count = (raw_ones >> 1) + self.holding_one;
        self.holding_one = raw_ones & 1;
        self.holding_zero = (raw_ones & 1) == 0;

        if self.flags & HYBRID_FLAG != 0 && chan == 0 {
            self.update_error_limit();
        }

        let (mut low, high) = if ones_count == 0 {
            let high = self.get_med(chan, 0) - 1;
            self.dec_med(chan, 0);
            (0, high)
        } else {
            let mut low = self.get_med(chan, 0);
            self.inc_med(chan, 0);

            if ones_count == 1 {
                let high = low + self.get_med(chan, 1) - 1;
                self.dec_med(chan, 1);
                (low, high)
            } else {
                low += self.get_med(chan, 1);
                self.inc_med(chan, 1);
                ones_count -= 2;

                if ones_count == 0 {
                    let high = low + self.get_med(chan, 2) - 1;
                    self.dec_med(chan, 2);
                    (low, high)
                } else {
                    low += ones_count * self.get_med(chan, 2);
                    let high = low + self.get_med(chan, 2) - 1;
                    self.inc_med(chan, 2);
                    (low, high)
                }
            }
        };

        let mut high = high;
        low &= 0x7fff_ffff;
        high &= 0x7fff_ffff;

        if low > high {
            high = low;
        }

        let mut mid = (high + low + 1) >> 1;

        if self.error_limit[chan] == 0 {
            mid = bs.read_code(high - low)? + low;
        } else {
            while high - low > self.error_limit[chan] {
                if bs.read_bit()? != 0 {
                    low = mid;
                } else {
                    high = mid - 1;
                }

                mid = (high + low + 1) >> 1;
            }
        }

        let sign = bs.read_bit()?;
        let correction = if let Some(correction_bits) = correction_bits.as_mut() {
            if self.error_limit[chan] != 0 {
                let value = correction_bits.read_code(high - low)? + low;
                if sign != 0 { mid as i32 - value as i32 } else { value as i32 - mid as i32 }
            } else {
                0
            }
        } else {
            0
        };

        if self.flags & HYBRID_BITRATE != 0 {
            if let Some(hybrid) = self.hybrid.as_mut() {
                hybrid.slow_level[chan] =
                    hybrid.slow_level[chan].wrapping_sub((hybrid.slow_level[chan] + SLO) >> SLS);
                hybrid.slow_level[chan] = hybrid.slow_level[chan].wrapping_add(wp_log2(mid) as u32);
            }
        }

        Ok(DecodedWord { value: if sign != 0 { !(mid as i32) } else { mid as i32 }, correction })
    }

    fn read_words_into(
        &mut self,
        bs: &mut WavPackBitReader<'_>,
        mut correction_bits: Option<&mut WavPackBitReader<'_>>,
        samples: usize,
        out: &mut Vec<DecodedWord>,
    ) -> Result<()> {
        let channels = if self.mono { 1 } else { 2 };
        out.clear();
        out.reserve(samples * channels);

        for i in 0..samples * channels {
            let chan = if self.mono { 0 } else { i & 1 };
            out.push(self.read_word(bs, correction_bits.as_deref_mut(), chan)?);
        }

        Ok(())
    }

    #[cfg(test)]
    fn read_words(
        &mut self,
        bs: &mut WavPackBitReader<'_>,
        correction_bits: Option<&mut WavPackBitReader<'_>>,
        samples: usize,
    ) -> Result<Vec<DecodedWord>> {
        let mut out = Vec::new();
        self.read_words_into(bs, correction_bits, samples, &mut out)?;
        Ok(out)
    }
}

/// WavPack native block reader.
pub struct WavPackReader<'s> {
    reader: MediaSourceStream<'s>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    chapters: Option<ChapterGroup>,
    pending: Option<WavPackBlock>,
    total_samples: Option<u64>,
    next_ts: Timestamp,
}

impl<'s> WavPackReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let marker = mss.read_quad_bytes()?;
        let first = read_block_with_marker(&mut mss, marker)?;

        let codec_params = make_codec_params(&first)?;
        let total_samples = first.header.total_samples;

        let mut track = Track::new(0);

        if let Some(total_samples) = total_samples {
            track.with_num_frames(total_samples).with_duration(Duration::from(total_samples));
        }

        track.with_codec_params(CodecParameters::Audio(codec_params));

        let media_info = MediaInfo::from_track(&track);

        Ok(WavPackReader {
            reader: mss,
            media_info,
            tracks: vec![track],
            metadata: opts.external_data.metadata.unwrap_or_default(),
            chapters: opts.external_data.chapters,
            pending: Some(first),
            total_samples,
            next_ts: Timestamp::new(0),
        })
    }

    fn next_block(&mut self) -> Result<Option<WavPackBlock>> {
        if let Some(block) = self.pending.take() {
            Ok(Some(block))
        } else {
            read_block(&mut self.reader)
        }
    }

    fn next_audio_block(&mut self) -> Result<Option<WavPackBlock>> {
        while let Some(block) = self.next_block()? {
            if block.header.block_samples != 0 {
                return Ok(Some(block));
            }

            debug!("skipping non-audio wavpack block");
        }

        Ok(None)
    }

    fn read_packet_blocks(&mut self) -> Result<Option<WavPackPacketBlocks>> {
        if let Some(total_samples) = self.total_samples {
            if self.next_ts.get() >= 0 && self.next_ts.get() as u64 >= total_samples {
                return Ok(None);
            }
        }

        let Some(first) = self.next_audio_block()? else {
            return Ok(None);
        };

        let ts = Timestamp::new(first.header.block_index as i64);
        let dur = Duration::from(u64::from(first.header.block_samples));
        let mut packet = Vec::from(first.data.as_ref());
        let mut saw_final = first.header.flags & FINAL_BLOCK != 0;

        if first.header.flags & INITIAL_BLOCK == 0 {
            info!("wavpack packet starts on a non-initial block");
        }

        while !saw_final {
            let Some(block) = self.next_audio_block()? else {
                return decode_error("wavpack: unterminated multichannel packet");
            };

            if block.header.block_index != ts.get() as u64 {
                return decode_error("wavpack: multichannel block index mismatch");
            }

            if block.header.block_samples != dur.get() as u32 {
                return decode_error("wavpack: multichannel block duration mismatch");
            }

            saw_final = block.header.flags & FINAL_BLOCK != 0;
            packet.extend_from_slice(&block.data);
        }

        self.next_ts = Timestamp::new(ts.get().saturating_add(dur.get() as i64));

        Ok(Some((ts, dur, packet.into_boxed_slice())))
    }

    fn scan_seek_target(
        &mut self,
        required_ts: Timestamp,
        mode: SeekMode,
    ) -> Result<Option<(u64, Timestamp)>> {
        self.reader.seek(SeekFrom::Start(0))?;

        let mut best_before: Option<(u64, Timestamp)> = None;
        let mut first_after: Option<(u64, Timestamp)> = None;

        loop {
            let block_offset = self.reader.pos();
            let marker = match self.reader.read_quad_bytes() {
                Ok(marker) => marker,
                Err(err) if err.kind() == ErrorKind::UnexpectedEof => break,
                Err(err) => return Err(err.into()),
            };

            if is_trailing_metadata_marker(marker) {
                break;
            }

            let header = read_block_header_with_marker(&mut self.reader, marker)?;
            let packet_ts = Timestamp::new(header.block_index as i64);

            if header.block_samples != 0 && header.flags & INITIAL_BLOCK != 0 {
                if packet_ts <= required_ts {
                    best_before = Some((block_offset, packet_ts));
                } else if first_after.is_none() {
                    first_after = Some((block_offset, packet_ts));
                    if mode == SeekMode::Coarse {
                        break;
                    }
                }
            }

            let remaining = header.byte_len().saturating_sub(WAVPACK_HEADER_LEN as u64);
            self.reader.ignore_bytes(remaining)?;
        }

        if mode == SeekMode::Coarse { Ok(first_after.or(best_before)) } else { Ok(best_before) }
    }
}

impl Scoreable for WavPackReader<'_> {
    fn score(_src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        Ok(Score::Supported(255))
    }
}

impl ProbeableFormat<'_> for WavPackReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(WavPackReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(
            WAVPACK_FORMAT_INFO,
            &["wv", "wvp", "wavpack"],
            &["audio/wavpack", "audio/x-wavpack"],
            &[b"wvpk"]
        )]
    }
}

impl FormatReader for WavPackReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &WAVPACK_FORMAT_INFO
    }

    fn media_info(&self) -> &MediaInfo {
        &self.media_info
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn chapters(&self) -> Option<&ChapterGroup> {
        self.chapters.as_ref()
    }

    fn seek(&mut self, mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        if !self.reader.is_seekable() {
            return seek_error(SeekErrorKind::Unseekable);
        }

        let track = self.tracks.first().ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
        let required_ts = match to {
            SeekTo::Timestamp { ts, .. } => ts,
            SeekTo::Time { time, .. } => {
                let tb = track.time_base.ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
                tb.calc_timestamp(time).ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?
            }
        };

        if required_ts.get() < 0 {
            return seek_error(SeekErrorKind::OutOfRange);
        }

        if let Some(total_samples) = self.total_samples {
            if required_ts.get() as u64 > total_samples {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        }

        let (offset, actual_ts) = if required_ts == Timestamp::new(0) {
            (0, Timestamp::new(0))
        } else {
            self.scan_seek_target(required_ts, mode)?
                .ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?
        };

        self.reader.seek(SeekFrom::Start(offset))?;
        self.pending = read_block(&mut self.reader)?;
        self.next_ts = actual_ts;

        Ok(SeekedTo { track_id: 0, actual_ts, required_ts })
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        match self.read_packet_blocks()? {
            Some((ts, dur, data)) => Ok(Some(Packet::new(0, ts, dur, data))),
            None => Ok(None),
        }
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}

fn expand_matroska_packet(data: &[u8], version: u16) -> Result<Vec<u8>> {
    if !(WAVPACK_MIN_STREAM_VERSION..=WAVPACK_MAX_STREAM_VERSION).contains(&version) {
        return unsupported_error("wavpack: unsupported matroska stream version");
    }

    let mut reader = BufReader::new(data);
    let mut out = Vec::new();
    let mut block_samples = None;
    let mut multiple_blocks = false;

    while reader.pos() < data.len() as u64 {
        if data.len().saturating_sub(reader.pos() as usize) < 8 {
            return decode_error("wavpack: truncated matroska block");
        }

        if block_samples.is_none() {
            let samples = reader.read_u32()?;
            block_samples = Some(samples);
        }

        let flags = reader.read_u32()?;

        if out.is_empty() && flags & FINAL_BLOCK == 0 {
            multiple_blocks = true;
        }

        let crc = reader.read_u32()?;

        let block_size = if multiple_blocks {
            if data.len().saturating_sub(reader.pos() as usize) < 4 {
                return decode_error("wavpack: missing matroska block size");
            }

            reader.read_u32()? as usize
        } else {
            data.len() - reader.pos() as usize
        };

        if data.len().saturating_sub(reader.pos() as usize) < block_size {
            return decode_error("wavpack: matroska block overruns packet");
        }

        let block_samples = block_samples.unwrap();
        let ck_size = WAVPACK_MIN_CK_SIZE + block_size as u32;

        out.extend_from_slice(&WAVPACK_MARKER);
        out.extend_from_slice(&ck_size.to_le_bytes());
        out.extend_from_slice(&version.to_le_bytes());
        out.push(0);
        out.push(0);
        out.extend_from_slice(&block_samples.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&block_samples.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&data[reader.pos() as usize..reader.pos() as usize + block_size]);

        reader.ignore_bytes(block_size as u64)?;
    }

    if out.is_empty() {
        return decode_error("wavpack: empty matroska packet");
    }

    Ok(out)
}

/// WavPack decoder.
pub struct WavPackDecoder {
    params: AudioCodecParameters,
    matroska_version: Option<u16>,
    buf: GenericAudioBuffer,
    residuals: Vec<DecodedWord>,
    samples: Vec<i32>,
    decorr_passes: Vec<DecorrPass>,
    dsd_scratch: DsdScratch,
}

impl WavPackDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_WAVPACK {
            return unsupported_error("wavpack: invalid codec");
        }

        let sample_rate =
            params.sample_rate.ok_or(Error::Unsupported("wavpack: sample rate is required"))?;
        let channels =
            params.channels.clone().ok_or(Error::Unsupported("wavpack: channels are required"))?;
        let sample_format = params.sample_format.unwrap_or(SampleFormat::S32);
        let capacity = params.max_frames_per_packet.unwrap_or(0) as usize;
        let matroska_version = params
            .extra_data
            .as_deref()
            .and_then(|extra| extra.get(..2))
            .map(|version| u16::from_le_bytes([version[0], version[1]]));

        let buf =
            GenericAudioBuffer::new(sample_format, AudioSpec::new(sample_rate, channels), capacity);

        Ok(WavPackDecoder {
            params: params.clone(),
            matroska_version,
            buf,
            residuals: Vec::new(),
            samples: Vec::new(),
            decorr_passes: Vec::new(),
            dsd_scratch: DsdScratch::default(),
        })
    }

    fn ensure_output_format(&mut self, sample_format: SampleFormat) -> Result<()> {
        if std::mem::discriminant(&generic_buffer_format(&self.buf))
            == std::mem::discriminant(&sample_format)
        {
            return Ok(());
        }

        if !self.buf.is_empty() {
            return decode_error("wavpack: packet changes sample format between blocks");
        }

        let spec = self.buf.spec().clone();
        let capacity = self.buf.capacity();
        self.buf = GenericAudioBuffer::new(sample_format, spec, capacity);
        self.params.sample_format = Some(sample_format);

        Ok(())
    }

    fn decode_inner(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        self.buf.clear();

        let expanded;
        let data = if packet.data.starts_with(&WAVPACK_MARKER) {
            packet.data
        } else if let Some(version) = self.matroska_version {
            expanded = expand_matroska_packet(packet.data, version)?;
            &expanded
        } else {
            return decode_error("wavpack: headerless packet without version");
        };

        let mut reader = BufReader::new(data);
        let mut channel_offset = 0;
        let output_channels = self.buf.spec().channels().count();

        while reader.pos() < data.len() as u64 {
            let pos = reader.pos() as usize;
            if data.len().saturating_sub(pos) < WAVPACK_HEADER_LEN {
                return decode_error("wavpack: truncated packet block");
            }

            let block = DecodeBlock::parse(&data[pos..])?;
            let (num_terms, has_entropy, has_primary_data, has_correction, has_extended_samples) =
                block.metadata_summary();
            reader.ignore_bytes(block.header.byte_len())?;

            if block.header.flags & INT32_DATA != 0 {
                debug!("wavpack packet contains extended int32 data");
            }

            self.ensure_output_format(block.header.sample_format()?)?;

            if !block.read_decorrelated_samples_into(
                &mut self.residuals,
                &mut self.samples,
                &mut self.decorr_passes,
                &mut self.dsd_scratch,
            )? {
                continue;
            }
            let block_channels = block.header.output_channels_in_block();

            write_samples_to_buffer(
                &mut self.buf,
                &self.samples,
                block.header.block_samples as usize,
                block_channels,
                channel_offset,
            )?;
            channel_offset += block_channels;

            debug!(
                "parsed wavpack block: terms={num_terms}, entropy={has_entropy}, primary={has_primary_data}, correction={has_correction}, extended={has_extended_samples}, samples={}",
                self.samples.len()
            );
        }

        if channel_offset == 0 {
            return decode_error("wavpack: packet contained no audio samples");
        }

        if channel_offset != output_channels {
            return decode_error("wavpack: packet channel count mismatch");
        }

        Ok(())
    }
}

impl AudioDecoder for WavPackDecoder {
    fn reset(&mut self) {
        self.buf.clear();
        self.residuals.clear();
        self.samples.clear();
        self.decorr_passes.clear();
        self.dsd_scratch.probabilities.clear();
        self.dsd_scratch.summed.clear();
        self.dsd_scratch.offsets.clear();
        self.dsd_scratch.lookup.clear();
    }

    fn codec_info(&self) -> &CodecInfo {
        &WAVPACK_CODEC_INFO
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode_ref(
        &mut self,
        packet: &PacketRef<'_>,
    ) -> Result<symphonia_core::audio::GenericAudioBufferRef<'_>> {
        if let Err(e) = self.decode_inner(packet) {
            self.buf.clear();
            Err(e)
        } else {
            Ok(self.buf.as_generic_audio_buffer_ref())
        }
    }

    fn finalize(&mut self) -> FinalizeResult {
        Default::default()
    }

    fn last_decoded(&self) -> symphonia_core::audio::GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for WavPackDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(WavPackDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(CODEC_ID_WAVPACK, "wavpack", "WavPack")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn pack_bits(bits: &[u32]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut byte = 0u8;

        for (i, &bit) in bits.iter().enumerate() {
            if bit != 0 {
                byte |= 1 << (i & 7);
            }

            if i & 7 == 7 {
                out.push(byte);
                byte = 0;
            }
        }

        if bits.len() & 7 != 0 {
            out.push(byte);
        }

        out
    }

    fn make_test_block(
        flags: u32,
        total_samples: Option<u32>,
        block_samples: u32,
        body: &[u8],
    ) -> Vec<u8> {
        let ck_size = WAVPACK_MIN_CK_SIZE + body.len() as u32;
        let mut data = Vec::with_capacity(WAVPACK_HEADER_LEN + body.len());

        data.extend_from_slice(&WAVPACK_MARKER);
        data.extend_from_slice(&ck_size.to_le_bytes());
        data.extend_from_slice(&WAVPACK_MAX_STREAM_VERSION.to_le_bytes());
        data.push(0);
        data.push(0);
        data.extend_from_slice(&total_samples.unwrap_or(u32::MAX).to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&block_samples.to_le_bytes());
        data.extend_from_slice(&flags.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(body);
        data
    }

    fn make_test_block_with_crc(
        flags: u32,
        total_samples: Option<u32>,
        block_samples: u32,
        crc: u32,
        body: &[u8],
    ) -> Vec<u8> {
        let mut data = make_test_block(flags, total_samples, block_samples, body);
        data[28..32].copy_from_slice(&crc.to_le_bytes());
        data
    }

    fn parse_test_block(data: &[u8]) -> WavPackBlock {
        let header = WavPackBlockHeader::parse(data).unwrap();
        WavPackBlock { header, data: Vec::from(data).into_boxed_slice() }
    }

    fn make_test_block_at_index(
        flags: u32,
        total_samples: Option<u32>,
        block_index: u32,
        block_samples: u32,
        body: &[u8],
    ) -> Vec<u8> {
        let mut data = make_test_block(flags, total_samples, block_samples, body);
        data[16..20].copy_from_slice(&block_index.to_le_bytes());
        data
    }

    #[test]
    fn wavpack_bitreader_reads_lsb_first_codes() {
        let data = pack_bits(&[
            1, // maxcode=1.
            0, 1, 0, // maxcode=5, code=2.
            1, 1, 1, // maxcode=5, code=5.
        ]);
        let mut bits = WavPackBitReader::new(&data);

        assert_eq!(bits.read_code(0).unwrap(), 0);
        assert_eq!(bits.read_code(1).unwrap(), 1);
        assert_eq!(bits.read_code(5).unwrap(), 2);
        assert_eq!(bits.read_code(5).unwrap(), 5);
    }

    #[test]
    fn wavpack_words_decoder_reads_zero_run() {
        let data = pack_bits(&[
            1, 1, 0, // zero-run unary length code: cbits=2.
            0, // zero-run payload -> two zeros.
        ]);
        let mut bits = WavPackBitReader::new(&data);
        let entropy = EntropyVars { median: [[0; 3]; 2] };
        let mut words = WordsDecoder::new(entropy, None, MONO_FLAG);

        assert_eq!(
            words.read_words(&mut bits, None, 2).unwrap(),
            vec![DecodedWord { value: 0, correction: 0 }, DecodedWord { value: 0, correction: 0 },]
        );
    }

    #[test]
    fn wavpack_words_decoder_uses_only_channel_zero_for_mono() {
        let data = pack_bits(&[
            0, // ones_count = 0.
            1, 0, // magnitude 1 with channel 0 median.
            0, // positive sign.
            0, // ones_count = 0.
            1, 0, // magnitude 1 with channel 0 median again.
            0, // positive sign.
        ]);
        let mut bits = WavPackBitReader::new(&data);
        let entropy = EntropyVars { median: [[32, 0, 0], [0, 0, 0]] };
        let mut words = WordsDecoder::new(entropy, None, MONO_FLAG);

        assert_eq!(
            words.read_words(&mut bits, None, 2).unwrap(),
            vec![DecodedWord { value: 1, correction: 0 }, DecodedWord { value: -1, correction: 0 },]
        );
    }

    #[test]
    fn wavpack_exp2s_matches_reference_values() {
        assert_eq!(wp_exp2s(0), 0);
        assert_eq!(wp_exp2s(0x100), 1);
        assert_eq!(wp_exp2s(0x200), 2);
        assert_eq!(wp_exp2s(-0x200), -2);
        assert_eq!(restore_weight(64), 516);
    }

    #[test]
    fn wavpack_decorr_samples_parse_mono_history() {
        let mut passes = vec![DecorrPass { term: 2, ..Default::default() }];

        parse_decorr_samples(&[0, 1, 0, 0xfe], 0x410, MONO_FLAG, &mut passes).unwrap();

        assert_eq!(passes[0].samples_a[0], 1);
        assert_eq!(passes[0].samples_a[1], -2);
    }

    #[test]
    fn wavpack_decorr_mono_pass_reconstructs_predictive_samples() {
        let pass = DecorrPass {
            term: 1,
            weight_a: 512,
            samples_a: [10, 0, 0, 0, 0, 0, 0, 0],
            ..Default::default()
        };
        let mut samples = vec![1, 2, 3];

        apply_decorr_passes(&mut samples, MONO_FLAG, 3, &[pass]).unwrap();

        assert_eq!(samples, vec![6, 5, 6]);
    }

    #[test]
    fn wavpack_decorr_stereo_negative_term_reconstructs_cross_channel_samples() {
        let pass = DecorrPass {
            term: -1,
            weight_a: 512,
            weight_b: 512,
            samples_a: [10, 0, 0, 0, 0, 0, 0, 0],
            ..Default::default()
        };
        let mut samples = vec![2, 4];

        apply_decorr_passes(&mut samples, 0, 1, &[pass]).unwrap();

        assert_eq!(samples, vec![7, 8]);
    }

    #[test]
    fn wavpack_joint_stereo_reconstructs_left_right() {
        let mut samples = vec![10, 3, -9, 4];

        apply_decorr_passes(&mut samples, JOINT_STEREO, 2, &[]).unwrap();

        assert_eq!(samples, vec![8, -2, 0, 9]);
    }

    #[test]
    fn wavpack_fixup_applies_final_shift() {
        let mut samples = vec![1, -2];

        fixup_samples(&mut samples, 2 << SHIFT_LSB, None, None, None, false, false).unwrap();

        assert_eq!(samples, vec![4, -8]);
    }

    #[test]
    fn wavpack_fixup_restores_int32_redundant_bits() {
        let mut zeros = vec![3, -3];
        let mut ones = vec![3, -3];
        let mut dups = vec![2, 3];

        fixup_samples(
            &mut zeros,
            INT32_DATA,
            None,
            Some(Int32Info { zeros: 1, ..Default::default() }),
            None,
            false,
            false,
        )
        .unwrap();
        fixup_samples(
            &mut ones,
            INT32_DATA,
            None,
            Some(Int32Info { ones: 2, ..Default::default() }),
            None,
            false,
            false,
        )
        .unwrap();
        fixup_samples(
            &mut dups,
            INT32_DATA,
            None,
            Some(Int32Info { dups: 3, ..Default::default() }),
            None,
            false,
            false,
        )
        .unwrap();

        assert_eq!(zeros, vec![6, -6]);
        assert_eq!(ones, vec![15, -9]);
        assert_eq!(dups, vec![16, 31]);
    }

    #[test]
    fn wavpack_fixup_restores_wvx_low_bits() {
        let mut samples = vec![2];
        let int32 = Int32Info { sent_bits: 2, ..Default::default() };
        let wvx = pack_bits(&[1, 1]);

        fixup_samples(&mut samples, INT32_DATA, None, Some(int32), Some(&wvx), false, false)
            .unwrap();

        assert_eq!(samples, vec![11]);
    }

    #[test]
    fn wavpack_float_fixup_restores_ieee_bits_without_wvx() {
        let mut samples = vec![0x800000, -0x800000, 0x1000000];
        let float = FloatInfo { max_exp: 127, ..Default::default() };

        fixup_samples(&mut samples, FLOAT_DATA, Some(float), None, None, false, false).unwrap();

        assert_eq!(samples[0] as u32, 1.0f32.to_bits());
        assert_eq!(samples[1] as u32, (-1.0f32).to_bits());
        assert_eq!(samples[2] as u32, 2.0f32.to_bits());
    }

    #[test]
    fn wavpack_false_stereo_duplicates_mono_samples() {
        let mut samples = vec![1, -2, 3];

        expand_false_stereo(&mut samples, FALSE_STEREO);

        assert_eq!(samples, vec![1, 1, -2, -2, 3, 3]);
    }

    #[test]
    fn wavpack_output_writer_fills_planar_integer_buffer() {
        let mut buf = GenericAudioBuffer::new(
            SampleFormat::S16,
            AudioSpec::new(44100, Channels::Discrete(2)),
            0,
        );

        write_samples_to_buffer(&mut buf, &[1, 2], 2, 1, 0).unwrap();
        write_samples_to_buffer(&mut buf, &[3, 4], 2, 1, 1).unwrap();

        let mut interleaved = Vec::<i16>::new();
        buf.copy_to_vec_interleaved(&mut interleaved);

        assert_eq!(interleaved, vec![1, 3, 2, 4]);
    }

    #[test]
    fn wavpack_reader_stops_at_total_samples_before_trailing_tag() {
        let flags = 1 | INITIAL_BLOCK | FINAL_BLOCK | (9 << SRATE_LSB);
        let mut data = make_test_block(flags, Some(1), 1, &[]);
        data.extend_from_slice(b"APETAGEX");

        let cursor = Cursor::new(data);
        let mss = MediaSourceStream::new(Box::new(cursor), Default::default());
        let mut reader = WavPackReader::try_new(mss, Default::default()).unwrap();

        assert!(reader.next_packet().unwrap().is_some());
        assert!(reader.next_packet().unwrap().is_none());
    }

    #[test]
    fn wavpack_reader_treats_trailing_metadata_marker_as_eos() {
        let flags = 1 | INITIAL_BLOCK | FINAL_BLOCK | (9 << SRATE_LSB);
        let mut data = make_test_block(flags, None, 1, &[]);
        data.extend_from_slice(b"APETAGEX");

        let cursor = Cursor::new(data);
        let mss = MediaSourceStream::new(Box::new(cursor), Default::default());
        let mut reader = WavPackReader::try_new(mss, Default::default()).unwrap();

        assert!(reader.next_packet().unwrap().is_some());
        assert!(reader.next_packet().unwrap().is_none());
    }

    #[test]
    fn wavpack_stream_info_accepts_ten_channel_float_blocks() {
        let flags = FLOAT_DATA | 3 | (8 << SRATE_LSB);
        let block = parse_test_block(&make_test_block(
            flags,
            Some(1),
            1,
            &[ID_CHANNEL_INFO | ID_ODD_SIZE, 1, 10, 0],
        ));
        let params = make_codec_params(&block).unwrap();

        assert!(matches!(params.sample_format, Some(SampleFormat::F32)));
        assert_eq!(params.sample_rate, Some(32000));
        assert_eq!(params.channels.unwrap().count(), 10);
    }

    #[test]
    fn wavpack_dsd_mode0_decodes_packed_bytes() {
        let dsd = parse_dsd_info(&[0, 0, 0xAA, 0x55]).unwrap();
        let mut out = Vec::new();
        let mut scratch = DsdScratch::default();

        decode_dsd_into(&mut out, dsd, MONO_FLAG | DSD_FLAG, 2, &mut scratch).unwrap();

        assert_eq!(out, vec![0xAA, 0x55]);
    }

    #[test]
    fn wavpack_dsd_mode1_decodes_minimal_fixture() {
        let mut payload = Vec::new();
        payload.push(0); // history bits.
        payload.push(0xff); // raw probability table follows.
        payload.push(1); // bin0 value 0 probability.
        payload.extend(std::iter::repeat_n(0u8, 255));
        payload.extend_from_slice(&[0, 0, 0, 0]); // initial range value.
        let mut md = vec![0, 1];
        md.extend_from_slice(&payload);

        let dsd = parse_dsd_info(&md).unwrap();
        let mut out = Vec::new();
        let mut scratch = DsdScratch::default();
        decode_dsd_into(&mut out, dsd, MONO_FLAG | DSD_FLAG, 1, &mut scratch).unwrap();

        assert_eq!(out, vec![0]);
    }

    #[test]
    fn wavpack_dsd_mode3_decodes_minimal_fixture() {
        let mut payload = Vec::new();
        payload.push(0); // rate_i
        payload.push(RATE_S); // rate_s
        payload.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0]); // filter state + factor
        payload.extend_from_slice(&[0, 0, 0, 0]); // initial range value
        payload.extend(std::iter::repeat_n(0u8, 16)); // extra bytes for renorm
        let mut md = vec![0, 3];
        md.extend_from_slice(&payload);

        let dsd = parse_dsd_info(&md).unwrap();
        let mut out = Vec::new();
        let mut scratch = DsdScratch::default();
        decode_dsd_into(&mut out, dsd, MONO_FLAG | DSD_FLAG, 1, &mut scratch).unwrap();

        assert_eq!(out, vec![0xFF]);
    }

    #[test]
    fn wavpack_dsd_codec_params_scale_sample_rate() {
        let flags = DSD_FLAG | MONO_FLAG | (9 << SRATE_LSB);
        let block = parse_test_block(&make_test_block(
            flags | INITIAL_BLOCK | FINAL_BLOCK,
            Some(1),
            1,
            &[ID_DSD_BLOCK | ID_ODD_SIZE, 2, 1, 0, 0xAA, 0],
        ));
        let params = make_codec_params(&block).unwrap();

        assert!(matches!(params.sample_format, Some(SampleFormat::U8)));
        assert_eq!(params.sample_rate, Some(44100 * 2 * 8));
        assert_eq!(params.bits_per_coded_sample, Some(1));
    }

    #[test]
    fn wavpack_decoder_rejects_dsd_crc_mismatch() {
        let flags = DSD_FLAG | MONO_FLAG | INITIAL_BLOCK | FINAL_BLOCK | (9 << SRATE_LSB);
        let body = [ID_DSD_BLOCK | ID_ODD_SIZE, 2, 1, 0, 0xAA, 0];
        let data = make_test_block_with_crc(flags, Some(1), 1, 0, &body);
        let block = parse_test_block(&data);
        let params = make_codec_params(&block).unwrap();
        let mut decoder = WavPackDecoder::try_new(&params, &Default::default()).unwrap();
        let packet =
            Packet::new(0, Timestamp::new(0), Duration::from(1u64), data.into_boxed_slice());

        assert!(matches!(decoder.decode_ref(&packet.as_packet_ref()), Err(Error::DecodeError(_))));
    }

    #[test]
    fn wavpack_decoder_accepts_dsd_crc_match() {
        let flags = DSD_FLAG | MONO_FLAG | INITIAL_BLOCK | FINAL_BLOCK | (9 << SRATE_LSB);
        let body = [ID_DSD_BLOCK | ID_ODD_SIZE, 2, 1, 0, 0xAA, 0];
        let data = make_test_block_with_crc(flags, Some(1), 1, dsd_crc(&[0xAA]), &body);
        let block = parse_test_block(&data);
        let params = make_codec_params(&block).unwrap();
        let mut decoder = WavPackDecoder::try_new(&params, &Default::default()).unwrap();
        let packet =
            Packet::new(0, Timestamp::new(0), Duration::from(1u64), data.into_boxed_slice());

        assert!(decoder.decode_ref(&packet.as_packet_ref()).is_ok());
    }

    #[test]
    fn wavpack_reader_seeks_to_packet_boundary() {
        let flags = 1 | INITIAL_BLOCK | FINAL_BLOCK | (9 << SRATE_LSB);
        let mut data = make_test_block_at_index(flags, Some(11), 0, 1, &[]);
        data.extend_from_slice(&make_test_block_at_index(flags, Some(11), 10, 1, &[]));

        let cursor = Cursor::new(data);
        let mss = MediaSourceStream::new(Box::new(cursor), Default::default());
        let mut reader = WavPackReader::try_new(mss, Default::default()).unwrap();

        let seeked = reader
            .seek(SeekMode::Accurate, SeekTo::Timestamp { ts: Timestamp::new(10), track_id: 0 })
            .unwrap();

        assert_eq!(seeked.actual_ts, Timestamp::new(10));
        let packet = reader.next_packet().unwrap().unwrap();
        assert_eq!(packet.pts, Timestamp::new(10));
    }

    #[test]
    fn wavpack_hybrid_profile_parses_mono_bitrate_state() {
        let profile =
            parse_hybrid_profile(&[0, 1, 2, 0, 0, 2], MONO_FLAG | HYBRID_FLAG | HYBRID_BITRATE)
                .unwrap();

        assert_eq!(profile.slow_level[0], 1);
        assert_eq!(profile.bitrate_acc[0], 2 << 16);
        assert_eq!(profile.bitrate_delta[0], 2);
    }

    #[test]
    fn wavpack_shaping_info_parses_accumulators_and_deltas() {
        let shaping = parse_shaping_info(
            &[
                0, 1, // error[0].
                0, 2, // shaping_acc[0].
                0, 3, // shaping_delta[0].
            ],
            MONO_FLAG | HYBRID_FLAG | HYBRID_SHAPE,
        )
        .unwrap();

        assert_eq!(shaping.error[0], 1);
        assert_eq!(shaping.shaping_acc[0], 2);
        assert_eq!(shaping.shaping_delta[0], 4);

        let shaping = parse_shaping_info(&[64, 0], HYBRID_FLAG | HYBRID_SHAPE).unwrap();

        assert_eq!(shaping.shaping_acc[0], 516 << 16);
        assert_eq!(shaping.shaping_acc[1], 0);
    }

    #[test]
    fn wavpack_words_decoder_reads_hybrid_correction() {
        let entropy = EntropyVars { median: [[16, 0, 0], [16, 0, 0]] };
        let hybrid = HybridProfile { bitrate_acc: [0x0200 << 16, 0], ..Default::default() };
        let mut words = WordsDecoder::new(entropy, Some(hybrid), MONO_FLAG | HYBRID_FLAG);
        let main_data = pack_bits(&[
            0, // ones_count = 0.
            0, // positive sign.
        ]);
        let correction_data = pack_bits(&[
            0, // exact value below mid by one.
        ]);
        let mut main_bits = WavPackBitReader::new(&main_data);
        let mut correction_bits = WavPackBitReader::new(&correction_data);

        assert_eq!(
            words.read_words(&mut main_bits, Some(&mut correction_bits), 1).unwrap(),
            vec![DecodedWord { value: 1, correction: -1 }]
        );
    }

    #[test]
    fn wavpack_hybrid_correction_reconstructs_mono_samples() {
        let residuals = vec![
            DecodedWord { value: 10, correction: 3 },
            DecodedWord { value: -4, correction: -2 },
        ];

        assert_eq!(
            apply_hybrid_correction(&residuals, MONO_FLAG | HYBRID_FLAG, 2, &[], None).unwrap(),
            vec![13, -6]
        );
    }

    #[test]
    fn wavpack_hybrid_correction_reconstructs_joint_stereo() {
        let residuals = vec![
            DecodedWord { value: 10, correction: 2 },
            DecodedWord { value: 4, correction: -1 },
        ];

        assert_eq!(
            apply_hybrid_correction(&residuals, HYBRID_FLAG | JOINT_STEREO, 1, &[], None).unwrap(),
            vec![9, -3]
        );
    }
}
