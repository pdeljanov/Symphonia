// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use log::debug;

use symphonia_core::errors::Result;
use symphonia_core::io::*;

pub enum SubBlock {
    Unknown(Vec<u8>),
    // Padding
    Dummy(Vec<u8>),
    // Decorrelation terms & deltas (fixed)
    DecorrelationTerms(Vec<u8>),
    // Initial decorrelation weights
    DecorrelationWeights(Vec<u8>),
    // Decorrelation sample history
    DecorrelationSamples(Vec<u8>),
    // Initial entropy variables
    EntropyVariables(Vec<u8>),
    // Entropy variables specific to hybrid mode
    HybridProfile(Vec<u8>),
    // Info needed for hybrid lossless (wvc) mode
    ShapingWeights(Vec<u8>),
    // Specific info for floating point decode
    FloatInfo(Vec<u8>),
    // Specific info for decoding integers > 24  bits, or data requiring shift after decode
    Int32Info(Vec<u8>),
    // Normal compressed audio bitstream (wv file)
    WvBitStream(Vec<u8>),
    // Correction file bitstream (wvc file)
    WvcBitStream(Vec<u8>),
    // special extended bitstream for floating point data or integers > 24 bit
    //(can be in either wv or wvc file, depending.....)
    WvxBitStream(Vec<u8>),
    // Contains channel count and channel_mask
    ChannelInfo(Vec<u8>),
    // contains compressed DSD audio (ver 5.0+)
    DsdBlock(Vec<u8>),
    // RIFF header for .wav files (before audio)
    RiffHeader(Vec<u8>),
    // RIFF trailer for .wav files (after audio)
    RiffTrailer(Vec<u8>),
    // some encoding details for info purposes
    ConfigChecksum(Vec<u8>),
    // 16-byte MD5 sum of raw audio data
    Md5Checksum(Vec<u8>),
    // non-standard sampling rate info
    SampleRate(Vec<u8>),
    // header for non-wav files (ver 5.0+)
    AltHeader(Vec<u8>),
    // trailer for non-wav files (ver 5.0+)
    AltTrailer(Vec<u8>),
    // target filename extension
    AltExtension(Vec<u8>),
    // 16-byte MD5 sum of raw audio data with non-wav standard (e.g., big-endian)
    AltMd5Checksum(Vec<u8>),
    // new file configuration stuff including file type, non-wav formats (e.g., big endian), and CAF channel layouts and reordering
    NewConfigBlock(Vec<u8>),
    // identities of non-MS channels
    ChannelIdentities(Vec<u8>),
    // 2- or 4-byte checksum of entire block
    BlockChecksum(Vec<u8>),
}

pub fn decode(source: &mut impl ReadBytes) -> Result<SubBlock> {
    let id = source.read_u8()?;
    if id & 0x3f == 0x3f {
        debug!("unique metadata function ID");
    }

    let size_in_words;
    if id & 0x80 == 0x80 {
        // debug!("large block, > 255 words");
        let b = source.read_triple_bytes()?;
        size_in_words = (b[0] as u32)
            | ((b[1] as u32) << 8)
            | ((b[2] as u32) << 16);
    } else {
        size_in_words = source.read_byte()? as u32;
    }
    let datasize = size_in_words * 2;

    if datasize % 2 != 0 {
        debug!("wavpack: Weird data size");
    }

    if id & 0x40 == 0x40 {
        debug!("actual data byte len is 1 less");
    }

    // debug!("subblock data size {}", datasize);
    let mut data: Vec<u8> = vec![0; datasize as usize];
    source.read_buf_exact(&mut data)?;

    if id & 0x20 == 0x20 {
        debug!("wavpack: ignore subblock");
        return Ok(SubBlock::Unknown(data));
    }

    //todo: use consts
    let sub_block = match id {
        id if id & 0x1F == 0x0 => {
            SubBlock::Dummy(data)
        },
        id if id & 0x1F == 0x2 => {
            SubBlock::DecorrelationTerms(data)
        },
        id if id & 0x1F == 0x3 => {
            SubBlock::DecorrelationWeights(data)
        },
        id if id & 0x1F == 0x4 => {
            SubBlock::DecorrelationSamples(data)
        },
        id if id & 0x1F == 0x5 => {
            SubBlock::EntropyVariables(data)
        },
        id if id & 0x1F == 0x6 => {
            SubBlock::HybridProfile(data)
        },
        id if id & 0x1F == 0x7 => {
            SubBlock::ShapingWeights(data)
        },
        id if id & 0x1F == 0x8 => {
            SubBlock::FloatInfo(data)
        },
        id if id & 0x1F == 0x9 => {
            SubBlock::Int32Info(data)
        },
        id if id & 0x1F == 0xA => {
            SubBlock::WvBitStream(data)
        },
        id if id & 0x1F == 0xB => {
            SubBlock::WvcBitStream(data)
        },
        id if id & 0x1F == 0xC => {
            SubBlock::WvxBitStream(data)
        },
        id if id & 0x1F == 0xD => {
            SubBlock::ChannelInfo(data)
        },
        id if id & 0x1F == 0xE => {
            SubBlock::DsdBlock(data)
        },
        id if id & 0x1F == 0x21 => {
            SubBlock::RiffHeader(data)
        },
        id if id & 0x1F == 0x22 => {
            SubBlock::RiffTrailer(data)
        },
        id if id & 0x1F == 0x25 => {
            SubBlock::ConfigChecksum(data)
        },
        id if id & 0x1F == 0x26 => {
            SubBlock::Md5Checksum(data)
        },
        id if id & 0x1F == 0x27 => {
            SubBlock::SampleRate(data)
        },
        id if id & 0x1F == 0x23 => {
            SubBlock::AltHeader(data)
        },
        id if id & 0x1F == 0x24 => {
            SubBlock::AltTrailer(data)
        },
        id if id & 0x1F == 0x28 => {
            SubBlock::AltExtension(data)
        },
        id if id & 0x1F == 0x29 => {
            SubBlock::AltMd5Checksum(data)
        },
        id if id & 0x1F == 0x2A => {
            SubBlock::NewConfigBlock(data)
        },
        id if id & 0x1F == 0x2B => {
            SubBlock::ChannelIdentities(data)
        },
        id if id & 0x1F == 0x2F => {
            SubBlock::BlockChecksum(data)
        },
        _ => {
            debug!("WavPack: Unknown subblock id: {}", id);
            SubBlock::Unknown(data)
        },
    };
    return Ok(sub_block);
}
