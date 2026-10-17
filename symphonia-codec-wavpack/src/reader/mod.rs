// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{ErrorKind, Seek};
use std::mem::size_of;

use symphonia_core::codecs::audio::well_known::CODEC_ID_WAVPACK;
use symphonia_core::support_format;

use symphonia_core::errors::{
    decode_error, seek_error, unsupported_error, Error, Result, SeekErrorKind,
};
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

use log::{trace, debug, warn};

mod subblocks;

const MAX_FRAMES_PER_PACKET: u64 = 1152;

const MAX_BLOCK_SIZE:u64 = 1000;

const WAVPACK_FORMAT_INFO: FormatInfo = FormatInfo {
    format: FORMAT_ID_WAVPACK,
    short_name: "wavpack",
    long_name: "Wavpack",
};

const STREAM_MARKER: [u8; 4] = *b"wvpk";
const HEADER_SIZE: u32 = 32;
const INITIAL_BLOCK: u32 = 0x800;
const FINAL_BLOCK: u32 = 0x1000;

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
    // Only the first block (block_index 0) provides the file's total sample count.
    // Other blocks reporting this value should be ignored.
    // If the total_samples is unknown reader will run into trailing metadata
    total_samples: Option<u64>,
    next_ts: u64,
    stream_start_pos: u64,
}

impl<'s> WavPackReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        //TODO: should already always be aligned, finding next block isn't necessary
        let block_pos = find_next_block(&mut mss, 100)?;
        let Some(first_block) = read_block(&mut mss)? else {
            return decode_error("wavpack: missing marker");
        };
        let header = first_block.header;
        let block_index = header.get_block_index();
        let total_samples = if block_index == 0 {
            header.get_total_samples()
        } else {
            debug!("First block is not first block after all. Total samples unknown");
            None
        };
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
        
        let metadata = opts.external_data.metadata.unwrap_or_default();

        let mut track = Track::new(0);
        track.with_codec_params(CodecParameters::Audio(codec_params));
        
        mss.seek(std::io::SeekFrom::Start(block_pos))?;
        
        return Ok(WavPackReader {
            reader: mss,
            tracks: vec![track],
            metadata,
            chapters: None,
            total_samples,
            next_ts: block_index,
            stream_start_pos: block_pos,
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

        // Trailing metadata is parsed separately by the format probe.
        if self.total_samples.is_some_and(|total| self.next_ts >= total) {
            return Ok(None);
        }

        let Some(first_block) = read_block(&mut self.reader)? else {
            return Ok(None);
        };

        if !first_block.header.is_initial_block() {
            return decode_error("wavpack: packet does not start with an initial block");
        }

        let block_index = first_block.header.get_block_index();
        let block_samples = first_block.header.block_samples;
        let mut packet_data = Vec::new();
        let mut block = first_block;
        let mut is_first_block = true;

        loop {
            if block.header.get_block_index() != block_index
                || block.header.block_samples != block_samples
            {
                return decode_error("wavpack: multichannel segment sample range changed");
            }
            if !is_first_block && block.header.is_initial_block() {
                return decode_error("wavpack: unexpected initial block in multichannel segment");
            }

            let is_final_block = block.header.is_final_block();
            block.append_to(&mut packet_data);
            if is_final_block {
                break;
            }

            block = match read_block(&mut self.reader)? {
                Some(block) => block,
                None => return decode_error("wavpack: incomplete multichannel segment"),
            };
            is_first_block = false;
        }

        let timestamp = match Timestamp::try_from(block_index) {
            Ok(timestamp) => timestamp,
            Err(_) => {
                warn!("wavpack: timestamp overflow");
                return Ok(None);
            }
        };
        let duration = Duration::new(u64::from(block_samples));
        self.next_ts = block_index.saturating_add(u64::from(block_samples));

        Ok(Some(Packet::new(0, timestamp, duration, packet_data.into_boxed_slice())))
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
        if !self.reader.is_seekable() {
            return seek_error(SeekErrorKind::Unseekable);
        }

        let track = self.tracks.first().ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
        let required_ts = match to {
            SeekTo::TimeStamp { ts, track_id } => {
                if track_id != track.id {
                    return seek_error(SeekErrorKind::InvalidTrack);
                }
                ts
            }
            SeekTo::Time { time, track_id } => {
                if track_id.is_some_and(|track_id| track_id != track.id) {
                    return seek_error(SeekErrorKind::InvalidTrack);
                }
                let time_base =
                    track.time_base.ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
                time_base
                    .calc_timestamp(time)
                    .ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?
            }
        };

        if required_ts.is_negative() {
            return seek_error(SeekErrorKind::OutOfRange);
        }

        if self.total_samples.is_some_and(|total| required_ts.get() as u64 > total) {
            return seek_error(SeekErrorKind::OutOfRange);
        }

        let track_id = track.id;
        let original_pos = self.reader.pos();

        let seek_target = 'scan: {
            // TODO: use seek index instead of scanning from the first block.
            if let Err(err) = self.reader.seek(std::io::SeekFrom::Start(self.stream_start_pos)) {
                break 'scan Err(Error::IoError(err));
            }

            let mut target = None;

            loop {
                let block_pos = self.reader.pos();
                let header = match Header::decode(&mut self.reader) {
                    Ok(header) => header,
                    Err(Error::IoError(err)) if err.kind() == ErrorKind::UnexpectedEof => break,
                    Err(Error::Unsupported("wavpack: missing marker")) => break,
                    Err(err) => break 'scan Err(err),
                };

                if header.block_samples != 0 && header.is_initial_block() {
                    let block_index = header.get_block_index();
                    if block_index > required_ts.get() as u64 {
                        break;
                    }

                    target = Some((block_pos, block_index));
                    let block_end =
                        block_index.saturating_add(u64::from(header.block_samples));
                    if (required_ts.get() as u64) < block_end {
                        break;
                    }
                }

                // skip to next block since already consumed part of it (the header)
                let body_len = match header.body_len() {
                    Ok(len) => len as u64,
                    Err(err) => break 'scan Err(err),
                };
                if let Err(err) = self.reader.ignore_bytes(body_len) {
                    break 'scan Err(Error::IoError(err));
                }
            }

            Ok(target)
        };
        let Some((block_pos, found_ts)) = (match seek_target {
            Ok(target) => target,
            Err(err) => {
                let _ = self.reader.seek(std::io::SeekFrom::Start(original_pos));
                return Err(err);
            }
        }) else {
            let _ = self.reader.seek(std::io::SeekFrom::Start(original_pos));
            return seek_error(SeekErrorKind::OutOfRange);
        };

        self.reader.seek(std::io::SeekFrom::Start(block_pos))?;
        self.next_ts = found_ts;

        let actual_ts = Timestamp::try_from(found_ts).map_err(|_| Error::SeekError(SeekErrorKind::OutOfRange))?;
        Ok(SeekedTo { track_id, required_ts, actual_ts })
    }
    
    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }

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

struct Block {
    header: Header,
    body: Box<[u8]>,
}

impl Block {
    fn append_to(self, packet_data: &mut Vec<u8>) {
        packet_data.reserve(HEADER_SIZE as usize + self.body.len());
        packet_data.extend_from_slice(&STREAM_MARKER);
        packet_data.extend_from_slice(&self.header.ck_size.to_le_bytes());
        packet_data.extend_from_slice(&self.header.version.to_le_bytes());
        packet_data.push(self.header.block_index_u8);
        packet_data.push(self.header.total_samples_u8);
        packet_data.extend_from_slice(&self.header.total_samples_u32.to_le_bytes());
        packet_data.extend_from_slice(&self.header.block_index_u32.to_le_bytes());
        packet_data.extend_from_slice(&self.header.block_samples.to_le_bytes());
        packet_data.extend_from_slice(&self.header.flags.to_le_bytes());
        packet_data.extend_from_slice(&self.header.crc.to_le_bytes());
        packet_data.extend_from_slice(&self.body);
    }
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

    fn body_len(&self) -> Result<usize> {
        match self.ck_size.checked_sub(HEADER_SIZE - STREAM_MARKER.len() as u32 - size_of::<u32>() as u32) {
            Some(len) => Ok(len as usize),
            None => decode_error("wavpack: invalid block size"),
        }
    }
}

impl Header {
    const SIZE: usize = HEADER_SIZE as usize;

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

    fn is_initial_block(&self) -> bool {
        self.flags & INITIAL_BLOCK != 0
    }

    fn is_final_block(&self) -> bool {
        self.flags & FINAL_BLOCK != 0
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

fn find_next_block(source: &mut MediaSourceStream<'_>, max_bytes: usize) -> Result<u64> {
    let mut bytes_read = 0;
    source.ensure_seekback_buffer(max_bytes);
    loop {
        if bytes_read + 4 >= max_bytes {
            return decode_error("wavpack: no block found");
        }
        let b = source.read_u8()?;
        bytes_read += 1;

        if b == b'w' {
            let b = source.read_triple_bytes()?;
            bytes_read += 3;

            if b == *b"vpk" {
                source.seek_buffered_rev(4);
                return Ok(source.pos());
            }
        }
    }
}

fn read_block(reader: &mut MediaSourceStream<'_>) -> Result<Option<Block>> {
    let header = match Header::decode(reader) {
        Ok(header) => header,
        Err(Error::IoError(err)) if err.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    };

    let body = reader.read_boxed_slice_exact(header.body_len()?)?;
    Ok(Some(Block { header, body }))
}