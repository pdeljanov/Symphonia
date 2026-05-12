// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! WavPack v4/v5 sub-block parsing.

use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::{MediaSourceStream, ReadBytes};

use log::debug;

/// A decoded WavPack v4/v5 sub-block.
pub(super) enum SubBlock {
    Unknown(Vec<u8>),
    Dummy(Vec<u8>),
    DecorrelationTerms(Vec<u8>),
    DecorrelationWeights(Vec<u8>),
    DecorrelationSamples(Vec<u8>),
    EntropyVariables(Vec<u8>),
    HybridProfile(Vec<u8>),
    ShapingWeights(Vec<u8>),
    FloatInfo(Vec<u8>),
    Int32Info(Vec<u8>),
    WvBitStream(Vec<u8>),
    WvcBitStream(Vec<u8>),
    WvxBitStream(Vec<u8>),
    ChannelInfo(Vec<u8>),
    DsdBlock(Vec<u8>),
    RiffHeader(Vec<u8>),
    RiffTrailer(Vec<u8>),
    ConfigChecksum(Vec<u8>),
    Md5Checksum(Vec<u8>),
    SampleRate(Vec<u8>),
    AltHeader(Vec<u8>),
    AltTrailer(Vec<u8>),
    AltExtension(Vec<u8>),
    AltMd5Checksum(Vec<u8>),
    NewConfigBlock(Vec<u8>),
    ChannelIdentities(Vec<u8>),
    BlockChecksum(Vec<u8>),
}

/// Audio encoding type carried by a WavPack block.
pub(super) enum Encoding {
    PCM,
    DSD,
}

/// Parse one sub-block from the stream. Called repeatedly after the block header.
pub(super) fn decode_sub_block(source: &mut MediaSourceStream<'_>) -> Result<SubBlock> {
    let id = source.read_u8()?;
    if id & 0x3f == 0x3f {
        debug!("unique metadata function ID");
    }

    let size_in_words = if id & 0x80 == 0x80 {
        let b = source.read_triple_bytes()?;
        (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16)
    } else {
        source.read_byte()? as u32
    };
    let datasize = size_in_words * 2;
    if id & 0x40 == 0x40 {
        debug!("actual data byte len is 1 less");
    }

    let mut data = vec![0u8; datasize as usize];
    source.read_buf_exact(&mut data)?;

    if id & 0x20 == 0x20 {
        return Ok(SubBlock::Unknown(data));
    }

    Ok(match id & 0x1F {
        0x00 => SubBlock::Dummy(data),
        0x02 => SubBlock::DecorrelationTerms(data),
        0x03 => SubBlock::DecorrelationWeights(data),
        0x04 => SubBlock::DecorrelationSamples(data),
        0x05 => SubBlock::EntropyVariables(data),
        0x06 => SubBlock::HybridProfile(data),
        0x07 => SubBlock::ShapingWeights(data),
        0x08 => SubBlock::FloatInfo(data),
        0x09 => SubBlock::Int32Info(data),
        0x0A => SubBlock::WvBitStream(data),
        0x0B => SubBlock::WvcBitStream(data),
        0x0C => SubBlock::WvxBitStream(data),
        0x0D => SubBlock::ChannelInfo(data),
        0x0E => SubBlock::DsdBlock(data),
        0x21 => SubBlock::RiffHeader(data),
        0x22 => SubBlock::RiffTrailer(data),
        0x25 => SubBlock::ConfigChecksum(data),
        0x26 => SubBlock::Md5Checksum(data),
        0x27 => SubBlock::SampleRate(data),
        0x23 => SubBlock::AltHeader(data),
        0x24 => SubBlock::AltTrailer(data),
        0x28 => SubBlock::AltExtension(data),
        0x29 => SubBlock::AltMd5Checksum(data),
        0x2A => SubBlock::NewConfigBlock(data),
        0x2B => SubBlock::ChannelIdentities(data),
        0x2F => SubBlock::BlockChecksum(data),
        id => {
            debug!("WavPack: unknown sub-block id: {:#x}", id);
            return decode_error("wavpack: unknown sub-block");
        }
    })
}
