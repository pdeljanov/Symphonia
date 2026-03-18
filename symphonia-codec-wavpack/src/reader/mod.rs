// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::Seek;

use symphonia_core::codecs::audio::well_known::CODEC_ID_WAVPACK;
use symphonia_core::support_format;

use symphonia_core::errors::{decode_error, unsupported_error, Result};
use symphonia_core::formats::prelude::*;
use symphonia_core::io::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_WAVPACK;
use symphonia_core::formats::FormatReader;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::audio::{Channels, layouts};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::audio::sample::SampleFormat;

use log::{trace, debug};

const MAX_FRAMES_PER_PACKET: u64 = 1152;

const MAX_BLOCK_SIZE:u64 = 1000;

const WAVPACK_FORMAT_INFO: FormatInfo = FormatInfo {
    format: FORMAT_ID_WAVPACK,
    short_name: "wavpack",
    long_name: "Wavpack",
};

const STREAM_MARKER: [u8; 4] = *b"wvpk";

const SAMPLE_RATES: [u32; 15] = [
    6000, 8000, 9600, 11025, 12000, 16000, 22050, 24000, 32000, 44100,
    48000, 64000, 88200, 96000, 192000
];

macro_rules! combine_values {
    ($u32_value:expr, $u8_value:expr) => {
        (($u8_value as u64) << 32) | ($u32_value as u64)
    };
}

pub struct WavPackReader<'a> {
    reader: MediaSourceStream<'a>,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    chapters: Option<ChapterGroup>,
    next_packet_ts: u64,
}

impl<'s> WavPackReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let original_pos = mss.pos();
        _ = find_next_block(&mut mss, 100);
        let header = Header::decode(&mut mss)?;
        if header.get_block_index() != 0 {
            debug!("First block is not first block after all. Total samples unknown");
        }
        // TODO: more extensive check
        let channel_layout = match header.is_stereo() {
            true => layouts::CHANNEL_LAYOUT_STEREO,
            false => layouts::CHANNEL_LAYOUT_MONO,
        };

        let mut codec_params = AudioCodecParameters::new();
        codec_params
            .for_codec(CODEC_ID_WAVPACK)
            .with_bits_per_coded_sample(header.get_bytes_per_sample() * 8)
            .with_bits_per_sample(header.get_bytes_per_sample() * 8)
            .with_channels(channel_layout)
        ;

        let sample_format = match header.get_encoding(){
            Encoding::PCM => match header.get_bytes_per_sample(){
                1 => SampleFormat::S8,
                2 => SampleFormat::S16,
                3 => SampleFormat::S24,
                4 => SampleFormat::S32,
                //todo: float
                _ => return decode_error("WavPack: Invalid sample format")
            },
            Encoding::DSD => return unsupported_error("WavPack: DSD unsupported"),
        };

        codec_params.with_sample_format(sample_format);

        let sample_rate = header.get_sample_rate();
        if let Some(sample_rate) = sample_rate {
            codec_params.with_sample_rate(sample_rate);
        }
        
        let metadata: MetadataLog = Default::default();

        let mut track = Track::new(0);
        track.with_codec_params(CodecParameters::Audio(codec_params));
        
        mss.seek(std::io::SeekFrom::Start(original_pos))?;
        
        return Ok(WavPackReader {
            reader: mss,
            tracks: vec![track],
            metadata,
            chapters: None,
            next_packet_ts: 0,
        });

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
        &[
            support_format!(
                WAVPACK_FORMAT_INFO,
                &["wv"],
                &["audio/x-wavpack"],
                &[b"wvpk"]
            ),
        ]
    }
}

impl Scoreable for WavPackReader<'_> {
    fn score(_src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        Ok(Score::Supported(255))
    }
}

impl FormatReader for WavPackReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &WAVPACK_FORMAT_INFO
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        if self.tracks.is_empty() {
            return decode_error("wavpack: no tracks");
        }

        
        _ = find_next_block(&mut self.reader, 10000);
        let header = Header::decode(&mut self.reader)?;
        loop {
            let sub_block = decode_sub_block(&mut self.reader)?;
            match sub_block {
                // Final sub-block is usually audio, although this is not a strict rule.
                // Version 5.0 a checksum of 4-6 bytes was added.
                SubBlock::WvBitStream(data) => {
                    let ts = self.next_packet_ts;
                    let n_samples = header.get_n_samples();
                    //let n_samples = data.len() / 4;
                    let dur =  n_samples;
                    self.next_packet_ts += n_samples as u64;

                    // debug!("Got packet with {} samples, datasize: {}", n_samples, data.len());
                    
                    return Ok(Some(Packet::new_from_boxed_slice(0, ts, dur as u64, data.into_boxed_slice())));
                },
                SubBlock::WvcBitStream(data) => {
                    debug!("wvc stream");
                },
                SubBlock::WvxBitStream(data) => {
                    debug!("wvx stream");
                },
                SubBlock::DsdBlock(data) =>{
                    todo!("DSD audio");
                }
                _ => {
                    debug!("some non-audio block");
                }
            }
        }
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn chapters(&self) -> Option<&ChapterGroup> {
        self.chapters.as_ref()
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        todo!("seek");
    }
    
    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }

}

enum SubBlock {
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

enum Encoding {
    PCM,
    DSD,
}

struct Header {
    ck_size: u32,
    version: u16,
    block_index_u8: u8,
    total_samples_u8: u8,
    total_samples_u32:u32,
    block_index_u32:u32,
    block_samples: u32,
    flags: u32,
    crc: u32,
}

impl Header {
    fn decode(reader: &mut MediaSourceStream<'_>) -> Result<Header>{
        let marker = reader.read_quad_bytes()?;
        if marker != STREAM_MARKER{
            return unsupported_error("wavpack: missing marker");
        }
        // Entire block size = ck_size + 8
        let ck_size = reader.read_u32()?;
        let version = reader.read_u16()?;
        let block_index_u8 = reader.read_u8()?;
        let total_samples_u8 = reader.read_u8()?;
        let total_samples_u32 = reader.read_u32()?;
        let block_index_u32 = reader.read_u32()?;
        let block_samples = reader.read_u32()?;
        let flags = reader.read_u32()?;
        let crc = reader.read_u32()?;
        return Ok(Header {
            ck_size,
            version,
            block_index_u8,
            total_samples_u8,
            total_samples_u32,
            block_index_u32,
            block_samples,
            flags,
            crc,
        });

    }
}

impl Header {
    const SIZE: usize = 4 * 8;

    // Index of the first sample in the block relative to file start
    fn get_block_index(&self) -> u64 {
        combine_values!(self.block_index_u32, self.block_index_u8)
    }

    fn get_total_samples(&self)->Option<u64>{
        if self.total_samples_u32 == 0xFFFFFFFF{
            // Indicates unknown
            return None;
        }
        return Some(combine_values!(self.total_samples_u32, self.total_samples_u8));
    }

    fn get_bytes_per_sample(&self)->u32{
        // TODO: use constants for flag values
        return (self.flags & 3) + 1;
    }

    fn get_encoding(&self) -> Encoding {
        match (self.flags >> 31) & 0b1 {
            0 => Encoding::PCM,
            _=> Encoding::DSD,
        }
    }

    // TODO: better way to get n channels
    fn get_n_channels(&self) -> u32{
        if self.is_stereo() {
            2
        }else {
            1
        }
    }

    fn get_n_samples(&self) -> u32{
        // In wavpack a "sample" is what we usually refer to as frame
        self.block_samples / self.get_n_channels()
    }

    fn is_stereo(&self) -> bool {
        ((self.flags >> 2) & 0b1) == 0
    }

    fn get_sample_rate(&self) -> Option<u32> {
        let shifted_number = self.flags >> 23;
        let sample_rate = shifted_number & 0b0000_1111;
        if sample_rate == 0b1111{
            debug!("wavpack: unknown/custom samplerate");
            return None;
        } 

        if sample_rate < SAMPLE_RATES.len() as u32 {
            Some(SAMPLE_RATES[sample_rate as usize])
        } else {
            debug!("wavpack: invalid samplerate index {}", sample_rate);
            None
        }
    }
}

fn find_next_block(source: &mut MediaSourceStream<'_>, max_bytes: usize) -> Result<u64>{
    let mut bytes_read = 0;
    source.ensure_seekback_buffer(max_bytes);
    loop {
        if bytes_read + 4 >= max_bytes {
            return decode_error("no block found")
        }
        let b = source.read_u8()?;
        bytes_read += 1;
        
        if b == b'w' {
            let b = source.read_triple_bytes()?;
            bytes_read += 3;
    
            if b == *b"vpk" {
                source.seek_buffered_rev(4);
                return Ok(bytes_read as u64);
            }
        }
    };
}

fn decode_sub_block(source: &mut MediaSourceStream)-> Result<SubBlock>{
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