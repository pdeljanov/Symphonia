// DSF (DSD Stream File) Format Parser
// Based on DSF specification v1.01

use std::io::{Seek, SeekFrom};

use symphonia_core::codecs::CodecParameters;
use symphonia_core::errors::{decode_error, end_of_stream_error, seek_error, unsupported_error};
use symphonia_core::errors::{Result, SeekErrorKind};
use symphonia_core::formats::prelude::*;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};
use symphonia_core::support_format;

use log::{debug, info};

use crate::CODEC_TYPE_DSD;

/// DSF magic number "DSD " (0x44534420)
const DSF_MAGIC: [u8; 4] = *b"DSD ";

/// Format chunk ID "fmt " (0x666d7420)
const DSF_FMT_MAGIC: [u8; 4] = *b"fmt ";

/// Data chunk ID "data" (0x64617461)
const DSF_DATA_MAGIC: [u8; 4] = *b"data";

/// DSF Header (28 bytes)
#[derive(Debug)]
struct DsfHeader {
    /// Total file size in bytes
    file_size: u64,
    /// Pointer to metadata chunk (0 if no metadata)
    _metadata_pointer: u64,
}

impl DsfHeader {
    fn read(reader: &mut MediaSourceStream) -> Result<Self> {
        // Read and verify magic
        let magic = reader.read_quad_bytes()?;
        if magic != DSF_MAGIC {
            return unsupported_error("dsf: invalid magic number");
        }

        // Chunk size (should be 28)
        let chunk_size = reader.read_u64()?;
        if chunk_size != 28 {
            return decode_error("dsf: invalid header chunk size");
        }

        let file_size = reader.read_u64()?;
        let _metadata_pointer = reader.read_u64()?;

        Ok(DsfHeader { file_size, _metadata_pointer })
    }
}

/// DSF Format Chunk (52 bytes)
#[derive(Debug)]
struct DsfFormatChunk {
    /// Format version (should be 1)
    format_version: u32,
    /// Format ID (0 = DSD Raw)
    format_id: u32,
    /// Channel type (1=mono, 2=stereo, 3=3ch, 4=quad, 5=4ch, 6=5ch, 7=5.1ch)
    _channel_type: u32,
    /// Number of channels
    channel_num: u32,
    /// Sampling frequency (2822400, 5644800, 11289600, 22579200)
    sampling_frequency: u32,
    /// Bits per sample (1 or 8)
    bits_per_sample: u32,
    /// Sample count (number of samples per channel)
    sample_count: u64,
    /// Block size per channel
    block_size_per_channel: u32,
}

impl DsfFormatChunk {
    fn read(reader: &mut MediaSourceStream) -> Result<Self> {
        // Read and verify chunk ID
        let chunk_id = reader.read_quad_bytes()?;
        if chunk_id != DSF_FMT_MAGIC {
            return decode_error("dsf: invalid format chunk ID");
        }

        // Chunk size (should be 52)
        let chunk_size = reader.read_u64()?;
        if chunk_size != 52 {
            return decode_error("dsf: invalid format chunk size");
        }

        let format_version = reader.read_u32()?;
        let format_id = reader.read_u32()?;
        let _channel_type = reader.read_u32()?;
        let channel_num = reader.read_u32()?;
        let sampling_frequency = reader.read_u32()?;
        let bits_per_sample = reader.read_u32()?;
        let sample_count = reader.read_u64()?;
        let block_size_per_channel = reader.read_u32()?;

        // Reserved (4 bytes)
        reader.read_u32()?;

        debug!(
            "DSF Format: version={}, channels={}, rate={}, bps={}, samples={}",
            format_version, channel_num, sampling_frequency, bits_per_sample, sample_count
        );

        Ok(DsfFormatChunk {
            format_version,
            format_id,
            _channel_type,
            channel_num,
            sampling_frequency,
            bits_per_sample,
            sample_count,
            block_size_per_channel,
        })
    }

    fn validate(&self) -> Result<()> {
        if self.format_version != 1 {
            return unsupported_error("dsf: unsupported format version");
        }

        if self.format_id != 0 {
            return unsupported_error("dsf: only DSD Raw format supported");
        }

        if self.bits_per_sample != 1 && self.bits_per_sample != 8 {
            return decode_error("dsf: invalid bits per sample");
        }

        if self.channel_num == 0 || self.channel_num > 6 {
            return unsupported_error("dsf: unsupported channel count");
        }

        Ok(())
    }
}

/// DSF Data Chunk Header (12 bytes)
#[derive(Debug)]
struct DsfDataChunk {
    /// Size of data in bytes
    data_size: u64,
}

impl DsfDataChunk {
    fn read(reader: &mut MediaSourceStream) -> Result<Self> {
        // Read and verify chunk ID
        let chunk_id = reader.read_quad_bytes()?;
        if chunk_id != DSF_DATA_MAGIC {
            return decode_error("dsf: invalid data chunk ID");
        }

        // Chunk size (12 + data size)
        let chunk_size = reader.read_u64()?;
        let data_size = chunk_size - 12;

        Ok(DsfDataChunk { data_size })
    }
}

/// DSF Format Reader
pub struct DsfReader {
    reader: MediaSourceStream,
    tracks: Vec<Track>,
    cues: Vec<Cue>,
    metadata: MetadataLog,
    data_start_pos: u64,
    data_end_pos: u64,
    block_size: u64,
    current_block: u64,
    total_blocks: u64,
}

impl QueryDescriptor for DsfReader {
    fn query() -> &'static [Descriptor] {
        &[support_format!("dsf", "DSD Stream File", &["dsf"], &["audio/dsd"], &[b"DSD "])]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}

impl FormatReader for DsfReader {
    fn try_new(mut source: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {
        // Read DSF header
        let header = DsfHeader::read(&mut source)?;

        info!("DSF file size: {} bytes", header.file_size);

        // Read format chunk
        let format = DsfFormatChunk::read(&mut source)?;
        format.validate()?;

        // Read data chunk
        let data = DsfDataChunk::read(&mut source)?;

        let data_start_pos = source.pos();
        let data_end_pos = data_start_pos + data.data_size;

        // Calculate blocks
        let block_size = format.block_size_per_channel * format.channel_num;
        let total_blocks = if block_size > 0 { data.data_size / block_size as u64 } else { 0 };

        debug!(
            "DSF data: start={}, end={}, block_size={}, total_blocks={}",
            data_start_pos, data_end_pos, block_size, total_blocks
        );

        // Build codec parameters
        let mut codec_params = CodecParameters::new();

        // Determine channel layout
        let channels = match format.channel_num {
            1 => symphonia_core::audio::Layout::Mono.into_channels(),
            2 => symphonia_core::audio::Layout::Stereo.into_channels(),
            3 => symphonia_core::audio::Layout::TwoPointOne.into_channels(),
            6 => symphonia_core::audio::Layout::FivePointOne.into_channels(),
            // For other channel counts, create a custom channel map
            n => {
                use symphonia_core::audio::Channels;
                // Just use the first n channels
                let mut ch = Channels::empty();
                if n >= 1 {
                    ch |= Channels::FRONT_LEFT;
                }
                if n >= 2 {
                    ch |= Channels::FRONT_RIGHT;
                }
                if n >= 3 {
                    ch |= Channels::FRONT_CENTRE;
                }
                if n >= 4 {
                    ch |= Channels::LFE1;
                }
                if n >= 5 {
                    ch |= Channels::REAR_LEFT;
                }
                if n >= 6 {
                    ch |= Channels::REAR_RIGHT;
                }
                ch
            }
        };

        codec_params
            .for_codec(CODEC_TYPE_DSD)
            .with_sample_rate(format.sampling_frequency)
            .with_bits_per_sample(format.bits_per_sample)
            .with_channels(channels)
            .with_channel_data_layout(symphonia_core::codecs::ChannelDataLayout::Planar)
            .with_bit_order(symphonia_core::codecs::BitOrder::LsbFirst)
            .with_max_frames_per_packet(format.block_size_per_channel as u64)
            .with_frames_per_block(format.block_size_per_channel as u64);

        // If we have sample count, calculate duration
        if format.sample_count > 0 {
            let frames = format.sample_count;
            let tb = TimeBase::new(1, format.sampling_frequency);
            codec_params.with_time_base(tb).with_n_frames(frames);
        }

        // Create track
        let track = Track::new(0, codec_params);

        Ok(DsfReader {
            reader: source,
            tracks: vec![track],
            cues: Vec::new(),
            metadata: MetadataLog::default(),
            data_start_pos,
            data_end_pos,
            block_size: block_size as u64,
            current_block: 0,
            total_blocks,
        })
    }

    fn next_packet(&mut self) -> Result<Packet> {
        // Check if we've reached the end
        if self.reader.pos() >= self.data_end_pos {
            return end_of_stream_error();
        }

        if self.current_block >= self.total_blocks {
            return end_of_stream_error();
        }

        // Read one block of DSD data
        let to_read = self.block_size.min(self.data_end_pos - self.reader.pos());

        let buf = self.reader.read_boxed_slice_exact(to_read as usize)?;

        let ts = self.current_block * self.block_size;
        self.current_block += 1;

        Ok(Packet::new_from_boxed_slice(0, ts, to_read, buf))
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn cues(&self) -> &[Cue] {
        &self.cues
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        // Seeking in DSF is block-aligned
        let required_block = match to {
            SeekTo::TimeStamp { ts, .. } => ts / self.block_size,
            SeekTo::Time { time, .. } => {
                let track = &self.tracks[0];
                let ts = track.codec_params.time_base.unwrap().calc_timestamp(time);
                ts / self.block_size
            }
        };

        if required_block >= self.total_blocks {
            return seek_error(SeekErrorKind::OutOfRange);
        }

        let seek_pos = self.data_start_pos + (required_block * self.block_size);

        self.reader.seek(SeekFrom::Start(seek_pos))?;
        self.current_block = required_block;

        let actual_ts = required_block * self.block_size;

        Ok(SeekedTo { track_id: 0, required_ts: actual_ts, actual_ts })
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.reader
    }
}
