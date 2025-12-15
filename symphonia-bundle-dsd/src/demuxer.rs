use symphonia_core::codecs::{CodecParameters, CODEC_TYPE_DSD_LSBF};
use symphonia_core::errors::{end_of_stream_error, Result};
use symphonia_core::formats::{
    Cue, FormatOptions, FormatReader, Packet, SeekMode, SeekTo, SeekedTo, Track,
};
use symphonia_core::io::{MediaSourceStream, ReadBytes};
use symphonia_core::meta::{Metadata, MetadataBuilder, MetadataLog};
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};
use symphonia_core::support_format;
use symphonia_core::units::TimeBase;
use symphonia_metadata::id3v2::read_id3v2;

use std::io::Seek;

use crate::dsf::DSFMetadata;

pub struct DsfReader {
    reader: MediaSourceStream,
    tracks: Vec<Track>,
    cues: Vec<Cue>,
    metadata: MetadataLog,
    // State
    data_start: u64,
    #[allow(dead_code)]
    data_end: u64,
    #[allow(dead_code)]
    block_size_per_channel: u32,
    #[allow(dead_code)]
    channel_num: u32,
    total_blocks: u64,
    current_block: u64,
    bytes_per_sample_frame: u32, // block_size_per_channel * channel_num
    samples_per_block: u64,      // block_size_per_channel * 8
}

impl QueryDescriptor for DsfReader {
    fn query() -> &'static [Descriptor] {
        &[support_format!(
            "dsf",
            "DSD Stream File",
            &["dsf"],
            &["audio/dsd", "application/x-dsd", "audio/x-dsf"],
            &[b"DSD "]
        )]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}

impl FormatReader for DsfReader {
    fn try_new(mut source: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {
        let dsf_metadata = DSFMetadata::read(&mut source)?;

        let data_start = source.pos();
        // chunk_size includes the 12 byte header (which we already read)
        let data_len = dsf_metadata.data_chunk.chunk_size.saturating_sub(12);
        let data_end = data_start + data_len;

        let block_size = dsf_metadata.fmt_chunk.block_size_per_channel;
        let channels = dsf_metadata.fmt_chunk.channel_num;
        let sample_rate = dsf_metadata.fmt_chunk.sampling_frequency;

        let total_samples = dsf_metadata.fmt_chunk.sample_count;

        // Codec Params
        let mut params = CodecParameters::new();

        let layout = match dsf_metadata.fmt_chunk.channel_type {
            1 => symphonia_core::audio::Layout::Mono,
            2 => symphonia_core::audio::Layout::Stereo,
            7 => symphonia_core::audio::Layout::FivePointOne,
            // TODO: Map others
            _ => symphonia_core::audio::Layout::Stereo,
        };

        params
            .for_codec(CODEC_TYPE_DSD_LSBF)
            .with_sample_rate(sample_rate)
            .with_time_base(TimeBase::new(1, sample_rate))
            .with_n_frames(total_samples)
            .with_channels(layout.into_channels());

        params.with_channel_layout(layout);

        // We say "frames per packet" is one block of samples.
        // One block is `block_size` bytes per channel.
        // 1 byte = 8 samples.
        // samples_per_block = block_size * 8.
        let samples_per_block = u64::from(block_size) * 8;
        params.with_frames_per_block(samples_per_block);

        let bytes_per_sample_frame = block_size * channels;
        let total_blocks = if bytes_per_sample_frame > 0 {
            data_len / u64::from(bytes_per_sample_frame)
        } else {
            0
        };

        let mut metadata_log = MetadataLog::default();
        let mut metadata_builder = MetadataBuilder::new();

        // Read Metadata if present
        let metadata_offset = dsf_metadata.dsd_chunk.metadata_offset;
        if metadata_offset > 0 {
            if let Ok(pos) = source.seek(std::io::SeekFrom::Start(metadata_offset)) {
                if pos == metadata_offset {
                    if let Err(e) = read_id3v2(&mut source, &mut metadata_builder) {
                        log::warn!("Failed to read ID3v2 metadata: {}", e);
                    }
                }
            }
            // Seek back to data start
            source.seek(std::io::SeekFrom::Start(data_start))?;
        }

        metadata_log.push(metadata_builder.metadata());

        Ok(DsfReader {
            reader: source,
            tracks: vec![Track::new(0, params)],
            cues: Vec::new(),
            metadata: metadata_log,
            data_start,
            data_end,
            block_size_per_channel: block_size,
            channel_num: channels,
            total_blocks,
            current_block: 0,
            bytes_per_sample_frame,
            samples_per_block,
        })
    }

    fn next_packet(&mut self) -> Result<Packet> {
        if self.current_block >= self.total_blocks {
            return end_of_stream_error();
        }

        let pts = self.current_block * self.samples_per_block;
        let duration = self.samples_per_block;

        let packet_size = self.bytes_per_sample_frame as usize;
        let buf = self.reader.read_boxed_slice(packet_size)?;

        self.current_block += 1;

        Ok(Packet::new_from_boxed_slice(0, pts, duration, buf))
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        // Simple seek implementation
        // Calculate target block
        let params = &self.tracks[0].codec_params;
        let sample_rate = params.sample_rate.unwrap_or(2822400);

        let ts = match to {
            SeekTo::TimeStamp { ts, .. } => ts,
            SeekTo::Time { time, .. } => TimeBase::new(1, sample_rate).calc_timestamp(time),
        };

        // Clamp to range
        let ts = ts.min(params.n_frames.unwrap_or(u64::MAX));

        // Find block index
        let block_index = ts / self.samples_per_block;

        // Seek to byte offset
        let byte_offset = block_index * u64::from(self.bytes_per_sample_frame);
        let abs_pos = self.data_start + byte_offset;

        self.reader.seek(std::io::SeekFrom::Start(abs_pos))?;
        self.current_block = block_index;

        let actual_ts = block_index * self.samples_per_block;

        Ok(SeekedTo { track_id: 0, actual_ts, required_ts: ts })
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn cues(&self) -> &[Cue] {
        &self.cues
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.reader
    }
}
