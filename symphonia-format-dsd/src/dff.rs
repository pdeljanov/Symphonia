// DFF (DSDIFF) Format Parser
// Based on DSDIFF specification v1.5

use std::io::{Seek, SeekFrom};

use symphonia_core::codecs::CodecParameters;
use symphonia_core::errors::{decode_error, end_of_stream_error, seek_error, unsupported_error};
use symphonia_core::errors::{Result, SeekErrorKind};
use symphonia_core::formats::prelude::*;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};
use symphonia_core::support_format;

use log::{debug, warn};

use crate::CODEC_TYPE_DSD;

/// FRM8 magic number (IFF container)
const DFF_FRM8_MAGIC: [u8; 4] = *b"FRM8";

/// DSD form type
const DFF_DSD_FORM: [u8; 4] = *b"DSD ";

/// Format version chunk ID
const DFF_FVER_ID: [u8; 4] = *b"FVER";

/// Property chunk ID
const DFF_PROP_ID: [u8; 4] = *b"PROP";

/// Sound property form
const DFF_SND_FORM: [u8; 4] = *b"SND ";

/// DSD uncompressed format
const DFF_CMPR_DSD: [u8; 4] = *b"DSD ";

/// DFF Header
#[derive(Debug)]
struct DffHeader {
    /// Total file size (excluding FRM8 chunk header)
    _file_size: u64,
}

impl DffHeader {
    fn read(reader: &mut MediaSourceStream) -> Result<Self> {
        // Read and verify FRM8 magic
        let magic = reader.read_quad_bytes()?;
        if magic != DFF_FRM8_MAGIC {
            return unsupported_error("dff: invalid FRM8 magic");
        }

        // Read chunk size (8 bytes, big-endian)
        let _file_size = reader.read_be_u64()?;

        // Read and verify form type
        let form_type = reader.read_quad_bytes()?;
        if form_type != DFF_DSD_FORM {
            return unsupported_error("dff: not a DSD form");
        }

        Ok(DffHeader { _file_size })
    }
}

/// DFF Format Version Chunk
#[derive(Debug)]
struct DffFormatVersion {
    major: u8,
}

impl DffFormatVersion {
    fn read(reader: &mut MediaSourceStream) -> Result<Self> {
        // Read chunk ID
        let chunk_id = reader.read_quad_bytes()?;
        if chunk_id != DFF_FVER_ID {
            return decode_error("dff: expected FVER chunk");
        }

        // Read chunk size (should be 4)
        let chunk_size = reader.read_be_u64()?;
        if chunk_size != 4 {
            return decode_error("dff: invalid FVER chunk size");
        }

        // Read version bytes (big-endian)
        let major = reader.read_u8()?;
        let _minor = reader.read_u8()?;
        let _revision = reader.read_u8()?;
        let _build = reader.read_u8()?;

        debug!("DFF version: {}.{}.{}.{}", major, _minor, _revision, _build);

        Ok(DffFormatVersion { major })
    }
}

/// DFF Sound Properties
#[derive(Debug)]
struct DffSoundProperties {
    sample_rate: u32,
    channel_count: u16,
    _channels: Vec<[u8; 4]>,
    compression: [u8; 4],
}

impl DffSoundProperties {
    fn read(reader: &mut MediaSourceStream) -> Result<Self> {
        // Read PROP chunk ID
        let chunk_id = reader.read_quad_bytes()?;
        if chunk_id != DFF_PROP_ID {
            return decode_error("dff: expected PROP chunk");
        }

        // Read PROP chunk size
        let chunk_size = reader.read_be_u64()?;
        let prop_end = reader.pos() + chunk_size;

        // Read property form type
        let form_type = reader.read_quad_bytes()?;
        if form_type != DFF_SND_FORM {
            return unsupported_error("dff: expected SND property form");
        }

        let mut sample_rate = None;
        let mut channel_count = None;
        let mut channels = None;
        let mut compression = DFF_CMPR_DSD;

        // Parse property chunks
        while reader.pos() < prop_end {
            let id = reader.read_quad_bytes()?;
            let size = reader.read_be_u64()?;

            match &id {
                b"FS  " => {
                    // Sample rate
                    sample_rate = Some(reader.read_be_u32()?);
                }
                b"CHNL" => {
                    // Channel configuration
                    let count = reader.read_be_u16()?;
                    channel_count = Some(count);

                    let mut ch_ids = Vec::new();
                    for _ in 0..count {
                        let ch_id = reader.read_quad_bytes()?;
                        ch_ids.push(ch_id);
                    }
                    channels = Some(ch_ids);
                }
                b"CMPR" => {
                    // Compression type
                    compression = reader.read_quad_bytes()?;
                    // Skip compression name if present
                    if size > 4 {
                        reader.ignore_bytes(size - 4)?;
                    }
                }
                _ => {
                    // Skip unknown chunks
                    warn!("DFF: Skipping unknown PROP chunk: {}", String::from_utf8_lossy(&id));
                    reader.ignore_bytes(size)?;
                }
            }

            // Handle padding
            if size % 2 == 1 {
                reader.ignore_bytes(1)?;
            }
        }

        let sample_rate = match sample_rate {
            Some(sr) => sr,
            None => return decode_error("dff: missing sample rate in PROP chunk"),
        };

        let channel_count = match channel_count {
            Some(cc) => cc,
            None => return decode_error("dff: missing channel count in PROP chunk"),
        };

        let _channels = match channels {
            Some(ch) => ch,
            None => return decode_error("dff: missing channel IDs in PROP chunk"),
        };

        debug!(
            "DFF properties: rate={}, channels={}, compression={:?}",
            sample_rate, channel_count, compression
        );

        Ok(DffSoundProperties { sample_rate, channel_count, _channels, compression })
    }

    fn validate(&self) -> Result<()> {
        if self.compression != DFF_CMPR_DSD {
            return unsupported_error(
                "dff: only uncompressed DSD supported (DST compression not implemented)",
            );
        }

        if self.channel_count == 0 || self.channel_count > 6 {
            return unsupported_error("dff: unsupported channel count");
        }

        Ok(())
    }
}

/// DFF Format Reader
pub struct DffReader {
    reader: MediaSourceStream,
    tracks: Vec<Track>,
    cues: Vec<Cue>,
    metadata: MetadataLog,
    data_start_pos: u64,
    data_end_pos: u64,
    current_pos: u64,
}

impl QueryDescriptor for DffReader {
    fn query() -> &'static [Descriptor] {
        &[support_format!("dff", "DSDIFF", &["dff"], &["audio/dsd"], &[b"FRM8"])]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}

impl FormatReader for DffReader {
    fn try_new(mut source: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {
        // Read DFF header
        let _header = DffHeader::read(&mut source)?;

        // Read format version
        let version = DffFormatVersion::read(&mut source)?;

        if version.major != 1 {
            return unsupported_error("dff: unsupported format version");
        }

        // Read sound properties
        let props = DffSoundProperties::read(&mut source)?;
        props.validate()?;

        // Find DSD audio data chunk
        let mut data_start_pos = None;
        let mut data_size = None;

        while let Ok(chunk_id) = source.read_quad_bytes() {
            let chunk_size = source.read_be_u64()?;

            if &chunk_id == b"DSD " {
                data_start_pos = Some(source.pos());
                data_size = Some(chunk_size);
                break;
            }
            else {
                // Skip this chunk
                debug!("DFF: Skipping chunk: {}", String::from_utf8_lossy(&chunk_id));
                source.ignore_bytes(chunk_size)?;

                // Handle padding
                if chunk_size % 2 == 1 {
                    source.ignore_bytes(1)?;
                }
            }
        }

        let data_start_pos = match data_start_pos {
            Some(pos) => pos,
            None => return decode_error("dff: no DSD audio data chunk found"),
        };

        let data_size = match data_size {
            Some(size) => size,
            None => return decode_error("dff: no DSD audio data size"),
        };

        let data_end_pos = data_start_pos + data_size;

        debug!("DFF data: start={}, end={}, size={}", data_start_pos, data_end_pos, data_size);

        // Build codec parameters
        let mut codec_params = CodecParameters::new();

        // Determine channel layout
        let channels = match props.channel_count {
            1 => symphonia_core::audio::Layout::Mono.into_channels(),
            2 => symphonia_core::audio::Layout::Stereo.into_channels(),
            3 => symphonia_core::audio::Layout::TwoPointOne.into_channels(),
            6 => symphonia_core::audio::Layout::FivePointOne.into_channels(),
            n => {
                use symphonia_core::audio::Channels;
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
            .with_sample_rate(props.sample_rate)
            .with_bits_per_sample(1) // DSD is 1-bit
            .with_channels(channels)
            .with_channel_data_layout(symphonia_core::codecs::ChannelDataLayout::Interleaved)
            .with_bit_order(symphonia_core::codecs::BitOrder::MsbFirst);

        // Calculate total frames (each byte = 8 samples per channel)
        let total_bytes = data_size;
        let samples_per_channel = (total_bytes * 8) / props.channel_count as u64;
        let tb = TimeBase::new(1, props.sample_rate);
        codec_params.with_time_base(tb).with_n_frames(samples_per_channel);

        // DSD data is typically read in blocks
        let block_size = 4096; // 4KB blocks
        codec_params
            .with_max_frames_per_packet(block_size * 8) // bytes * 8 bits
            .with_frames_per_block(block_size * 8);

        // Create track
        let track = Track::new(0, codec_params);

        Ok(DffReader {
            reader: source,
            tracks: vec![track],
            cues: Vec::new(),
            metadata: MetadataLog::default(),
            data_start_pos,
            data_end_pos,
            current_pos: data_start_pos,
        })
    }

    fn next_packet(&mut self) -> Result<Packet> {
        // Check if we've reached the end
        if self.current_pos >= self.data_end_pos {
            return end_of_stream_error();
        }

        // Read a block of DSD data (4KB)
        let block_size = 4096;
        let remaining = self.data_end_pos - self.current_pos;
        let to_read = block_size.min(remaining);

        let buf = self.reader.read_boxed_slice_exact(to_read as usize)?;

        let ts = (self.current_pos - self.data_start_pos) * 8; // Convert bytes to bits
        self.current_pos += to_read;

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
        // Seeking in DFF
        let required_byte = match to {
            SeekTo::TimeStamp { ts, .. } => ts / 8, // Convert bits to bytes
            SeekTo::Time { time, .. } => {
                let track = &self.tracks[0];
                let ts = track.codec_params.time_base.unwrap().calc_timestamp(time);
                ts / 8
            }
        };

        let seek_pos = self.data_start_pos + required_byte;

        if seek_pos >= self.data_end_pos {
            return seek_error(SeekErrorKind::OutOfRange);
        }

        self.reader.seek(SeekFrom::Start(seek_pos))?;
        self.current_pos = seek_pos;

        let actual_ts = (seek_pos - self.data_start_pos) * 8;

        Ok(SeekedTo { track_id: 0, required_ts: actual_ts, actual_ts })
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.reader
    }
}
