use crate::chunks::*;
use log::{error, info};
use std::io::{Seek, SeekFrom};
use symphonia_core::{
    audio::Channels,
    codecs::*,
    errors::{decode_error, end_of_stream_error, unsupported_error, Result},
    formats::{Cue, FormatOptions, FormatReader, Packet, SeekMode, SeekTo, SeekedTo, Track},
    io::{MediaSource, MediaSourceStream, ReadBytes},
    meta::{Metadata, MetadataLog},
    probe::{Descriptor, Instantiate, QueryDescriptor},
    support_format,
    units::TimeBase,
};

const MAX_FRAMES_PER_PACKET: u64 = 1152;

/// Core Audio Format (CAF) format reader.
///
/// `CafReader` implements a demuxer for Core Audio Format containers.
pub struct CafReader {
    reader: MediaSourceStream,
    tracks: Vec<Track>,
    cues: Vec<Cue>,
    metadata: MetadataLog,
    data_start_pos: u64,
    data_len: Option<u64>,
    bytes_per_caf_packet: u64,
    max_frames_per_packet: u64,
}

impl QueryDescriptor for CafReader {
    fn query() -> &'static [Descriptor] {
        &[support_format!("caf", "Core Audio Format", &["caf"], &["audio/x-caf"], &[b"caff"])]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}

impl FormatReader for CafReader {
    fn try_new(source: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {
        let mut reader = Self {
            reader: source,
            tracks: vec![],
            cues: vec![],
            metadata: MetadataLog::default(),
            data_start_pos: 0,
            data_len: None,
            bytes_per_caf_packet: 0,
            max_frames_per_packet: 0,
        };

        reader.check_file_header()?;
        let mut codec_params = reader.read_audio_description_chunk()?;
        reader.read_chunks(&mut codec_params)?;

        reader.tracks.push(Track::new(0, codec_params));

        Ok(reader)
    }

    fn next_packet(&mut self) -> Result<Packet> {
        if self.bytes_per_caf_packet == 0 {
            return decode_error("missing packet info");
        }

        let pos = self.reader.pos();
        let data_pos = pos - self.data_start_pos;

        let max_bytes_to_read = self.bytes_per_caf_packet * self.max_frames_per_packet;

        let bytes_remaining = if let Some(data_len) = self.data_len {
            data_len - data_pos
        } else {
            max_bytes_to_read
        };

        if bytes_remaining == 0 {
            return end_of_stream_error();
        }

        let bytes_to_read = max_bytes_to_read.min(bytes_remaining);
        let packet_duration = bytes_to_read / self.bytes_per_caf_packet;
        let packet_timestamp = data_pos / self.bytes_per_caf_packet;
        let buffer = self.reader.read_boxed_slice(bytes_to_read as usize)?;
        Ok(Packet::new_from_boxed_slice(0, packet_timestamp, packet_duration, buffer))
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

    fn seek(&mut self, _mode: SeekMode, _to: SeekTo) -> Result<SeekedTo> {
        unimplemented!();
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.reader
    }
}

impl CafReader {
    fn check_file_header(&mut self) -> Result<()> {
        let file_type = self.reader.read_quad_bytes()?;
        if file_type != *b"caff" {
            return unsupported_error("missing 'caff' stream marker");
        }

        let file_version = self.reader.read_be_u16()?;
        if file_version != 1 {
            error!("unsupported file version ({file_version})");
            return unsupported_error("unsupported file version");
        }

        // Ignored in CAF v1
        let _file_flags = self.reader.read_be_u16()?;

        Ok(())
    }

    fn read_audio_description_chunk(&mut self) -> Result<CodecParameters> {
        let chunk = Chunk::read(&mut self.reader)?;
        if let Some(Chunk::AudioDescription(desc)) = chunk {
            let mut codec_params = CodecParameters::new();
            codec_params
                .for_codec(desc.codec_type()?)
                .with_sample_rate(desc.sample_rate as u32)
                .with_time_base(TimeBase::new(1, desc.sample_rate as u32))
                .with_bits_per_sample(desc.bits_per_channel)
                .with_bits_per_coded_sample((desc.bytes_per_packet * 8) / desc.channels_per_frame);

            match desc.channels_per_frame {
                0 => {
                    return decode_error("channel count is zero");
                }
                1 => {
                    codec_params.with_channels(Channels::FRONT_LEFT);
                }
                2 => {
                    codec_params.with_channels(Channels::FRONT_LEFT | Channels::FRONT_RIGHT);
                }
                n => {
                    // When the channel count is >2 then enable the first N channels.
                    // This can/should be overridden when parsing the channel layout chunk.
                    match Channels::from_bits(((1u64 << n as u64) - 1) as u32) {
                        Some(channels) => {
                            codec_params.with_channels(channels);
                        }
                        None => {
                            return unsupported_error("unsupported channel count");
                        }
                    }
                }
            }

            if desc.frames_per_packet > 0 && !desc.format_is_compressed() {
                self.max_frames_per_packet = MAX_FRAMES_PER_PACKET;
                codec_params
                    .with_max_frames_per_packet(self.max_frames_per_packet)
                    .with_frames_per_block(desc.frames_per_packet as u64);
            } else {
                return unsupported_error("compressed formats are currently unsupported");
            }

            self.bytes_per_caf_packet = desc.bytes_per_packet as u64;

            Ok(codec_params)
        } else {
            error!("expected audio description chunk, found: {:?}", chunk);
            decode_error("expected audio description chunk")
        }
    }

    fn read_chunks(&mut self, codec_params: &mut CodecParameters) -> Result<()> {
        use Chunk::*;

        loop {
            match Chunk::read(&mut self.reader)? {
                Some(AudioDescription(_)) => {
                    return decode_error("additional Audio Description chunk")
                }
                Some(AudioData(data)) => {
                    self.data_start_pos = data.start_pos;
                    self.data_len = data.data_len;
                    if let Some(data_len) = self.data_len {
                        codec_params.with_n_frames(data_len / self.bytes_per_caf_packet);
                    }
                }
                Some(ChannelLayout(layout)) => {
                    if let Some(channels) = layout.channels() {
                        codec_params.channels = Some(channels);
                    } else {
                        // Don't error if the layout doesn't correspond directly to a Symphonia
                        // layout, the channels bitmap was set after the audio description was read
                        // to match the number of channels, and that's probably OK.
                        info!("couldn't convert the channel layout into a channel bitmap");
                    }
                }
                Some(Free) | None => {}
            }

            if let Some(byte_len) = self.reader.byte_len() {
                if self.reader.pos() == byte_len {
                    // If we've reached the end of the file, then the Audio Data chunk should have
                    // had a defined size, and we should seek to the start of the audio data.
                    if self.data_len.is_some() {
                        self.reader.seek(SeekFrom::Start(self.data_start_pos))?;
                    }
                    break;
                }
            }
        }

        Ok(())
    }
}
