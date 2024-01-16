// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::chunks::*;
use log::{debug, error, info};
use std::io::{Seek, SeekFrom};
use symphonia_core::{
    audio::Channels,
    codecs::*,
    errors::{
        decode_error, end_of_stream_error, seek_error, unsupported_error, Result, SeekErrorKind,
    },
    formats::{Cue, FormatOptions, FormatReader, Packet, SeekMode, SeekTo, SeekedTo, Track},
    io::{MediaSource, MediaSourceStream, ReadBytes},
    meta::{Metadata, MetadataLog},
    probe::{Descriptor, Instantiate, QueryDescriptor},
    support_format,
    units::{TimeBase, TimeStamp},
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
    packet_info: PacketInfo,
}

enum PacketInfo {
    Unknown,
    Uncompressed { bytes_per_frame: u32 },
    Compressed { packets: Vec<CafPacket>, current_packet_index: usize },
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
            packet_info: PacketInfo::Unknown,
        };

        reader.check_file_header()?;
        let codec_params = reader.read_chunks()?;

        reader.tracks.push(Track::new(0, codec_params));

        Ok(reader)
    }

    fn next_packet(&mut self) -> Result<Packet> {
        match &mut self.packet_info {
            PacketInfo::Uncompressed { bytes_per_frame } => {
                let pos = self.reader.pos();
                let data_pos = pos - self.data_start_pos;

                let bytes_per_frame = *bytes_per_frame as u64;
                let max_bytes_to_read = bytes_per_frame * MAX_FRAMES_PER_PACKET;

                let bytes_remaining = if let Some(data_len) = self.data_len {
                    data_len - data_pos
                }
                else {
                    max_bytes_to_read
                };

                if bytes_remaining == 0 {
                    return end_of_stream_error();
                }

                let bytes_to_read = max_bytes_to_read.min(bytes_remaining);
                let packet_duration = bytes_to_read / bytes_per_frame;
                let packet_timestamp = data_pos / bytes_per_frame;
                let buffer = self.reader.read_boxed_slice(bytes_to_read as usize)?;
                Ok(Packet::new_from_boxed_slice(0, packet_timestamp, packet_duration, buffer))
            }
            PacketInfo::Compressed { packets, ref mut current_packet_index } => {
                if let Some(packet) = packets.get(*current_packet_index) {
                    *current_packet_index += 1;
                    let buffer = self.reader.read_boxed_slice(packet.size as usize)?;
                    Ok(Packet::new_from_boxed_slice(0, packet.start_frame, packet.frames, buffer))
                }
                else if *current_packet_index == packets.len() {
                    end_of_stream_error()
                }
                else {
                    decode_error("caf: invalid packet index")
                }
            }
            PacketInfo::Unknown => decode_error("caf: missing packet info"),
        }
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
        let required_ts = match to {
            SeekTo::TimeStamp { ts, .. } => ts,
            SeekTo::Time { time, .. } => {
                if let Some(time_base) = self.time_base() {
                    time_base.calc_timestamp(time)
                }
                else {
                    return seek_error(SeekErrorKind::Unseekable);
                }
            }
        };

        match &mut self.packet_info {
            PacketInfo::Uncompressed { bytes_per_frame } => {
                // Packetization for PCM data is performed by chunking the stream into
                // packets of MAX_FRAMES_PER_PACKET frames each.
                // To allow for determinstic packet timestamps, we want the seek to jump to the
                // packet boundary before the requested seek time.
                let actual_ts = (required_ts / MAX_FRAMES_PER_PACKET) * MAX_FRAMES_PER_PACKET;
                let seek_pos = self.data_start_pos + actual_ts * (*bytes_per_frame as u64);

                if self.reader.is_seekable() {
                    self.reader.seek(SeekFrom::Start(seek_pos))?;
                }
                else {
                    let current_pos = self.reader.pos();
                    if seek_pos >= current_pos {
                        self.reader.ignore_bytes(seek_pos - current_pos)?;
                    }
                    else {
                        return seek_error(SeekErrorKind::ForwardOnly);
                    }
                }

                debug!(
                    "seek required_ts: {}, actual_ts: {}, (difference: {})",
                    actual_ts,
                    required_ts,
                    actual_ts as i64 - required_ts as i64,
                );

                Ok(SeekedTo { track_id: 0, actual_ts, required_ts })
            }
            PacketInfo::Compressed { packets, current_packet_index } => {
                let current_ts = if let Some(packet) = packets.get(*current_packet_index) {
                    TimeStamp::from(packet.start_frame)
                }
                else {
                    error!("invalid packet index: {}", current_packet_index);
                    return decode_error("caf: invalid packet index");
                };

                let search_range = if current_ts < required_ts {
                    *current_packet_index..packets.len()
                }
                else {
                    0..*current_packet_index
                };

                let packet_after_ts = packets[search_range]
                    .partition_point(|packet| packet.start_frame < required_ts);
                let seek_packet_index = packet_after_ts.saturating_sub(1);
                let seek_packet = &packets[seek_packet_index];

                let seek_pos = self.data_start_pos + seek_packet.data_offset;

                if self.reader.is_seekable() {
                    self.reader.seek(SeekFrom::Start(seek_pos))?;
                }
                else {
                    let current_pos = self.reader.pos();
                    if seek_pos >= current_pos {
                        self.reader.ignore_bytes(seek_pos - current_pos)?;
                    }
                    else {
                        return seek_error(SeekErrorKind::ForwardOnly);
                    }
                }

                *current_packet_index = seek_packet_index;
                let actual_ts = TimeStamp::from(seek_packet.start_frame);

                debug!(
                    "seek required_ts: {}, actual_ts: {}, (difference: {}, packet: {})",
                    required_ts,
                    actual_ts,
                    actual_ts as i64 - required_ts as i64,
                    seek_packet_index,
                );

                Ok(SeekedTo { track_id: 0, actual_ts, required_ts })
            }
            PacketInfo::Unknown => decode_error("caf: missing packet info"),
        }
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.reader
    }
}

impl CafReader {
    fn time_base(&self) -> Option<TimeBase> {
        self.tracks.first().and_then(|track| {
            track.codec_params.sample_rate.map(|sample_rate| TimeBase::new(1, sample_rate))
        })
    }

    fn check_file_header(&mut self) -> Result<()> {
        let file_type = self.reader.read_quad_bytes()?;
        if file_type != *b"caff" {
            return unsupported_error("caf: missing 'caff' stream marker");
        }

        let file_version = self.reader.read_be_u16()?;
        if file_version != 1 {
            error!("unsupported file version ({})", file_version);
            return unsupported_error("caf: unsupported file version");
        }

        // Ignored in CAF v1
        let _file_flags = self.reader.read_be_u16()?;

        Ok(())
    }

    fn read_audio_description_chunk(
        &mut self,
        desc: &AudioDescription,
        codec_params: &mut CodecParameters,
    ) -> Result<()> {
        codec_params
            .for_codec(desc.codec_type()?)
            .with_sample_rate(desc.sample_rate as u32)
            .with_time_base(TimeBase::new(1, desc.sample_rate as u32))
            .with_bits_per_sample(desc.bits_per_channel)
            .with_bits_per_coded_sample((desc.bytes_per_packet * 8) / desc.channels_per_frame);

        match desc.channels_per_frame {
            0 => {
                // A channel count of zero should have been rejected by the AudioDescription parser
                unreachable!("Invalid channel count");
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
                        return unsupported_error("caf: unsupported channel count");
                    }
                }
            }
        }

        if desc.format_is_compressed() {
            self.packet_info =
                PacketInfo::Compressed { packets: Vec::new(), current_packet_index: 0 };
        }
        else {
            codec_params.with_max_frames_per_packet(MAX_FRAMES_PER_PACKET).with_frames_per_block(1);
            self.packet_info = PacketInfo::Uncompressed { bytes_per_frame: desc.bytes_per_packet }
        };

        Ok(())
    }

    fn read_chunks(&mut self) -> Result<CodecParameters> {
        use Chunk::*;

        let mut codec_params = CodecParameters::new();
        let mut audio_description = None;

        loop {
            match Chunk::read(&mut self.reader, &audio_description)? {
                Some(AudioDescription(desc)) => {
                    if audio_description.is_some() {
                        return decode_error("caf: additional Audio Description chunk");
                    }
                    self.read_audio_description_chunk(&desc, &mut codec_params)?;
                    audio_description = Some(desc);
                }
                Some(AudioData(data)) => {
                    self.data_start_pos = data.start_pos;
                    self.data_len = data.data_len;
                    if let Some(data_len) = self.data_len {
                        if let PacketInfo::Uncompressed { bytes_per_frame } = &self.packet_info {
                            codec_params.with_n_frames(data_len / *bytes_per_frame as u64);
                        }
                    }
                }
                Some(ChannelLayout(layout)) => {
                    if let Some(channels) = layout.channels() {
                        codec_params.channels = Some(channels);
                    }
                    else {
                        // Don't error if the layout doesn't correspond directly to a Symphonia
                        // layout, the channels bitmap was set after the audio description was read
                        // to match the number of channels, and that's probably OK.
                        info!("couldn't convert the channel layout into a channel bitmap");
                    }
                }
                Some(PacketTable(table)) => {
                    if let PacketInfo::Compressed { ref mut packets, .. } = &mut self.packet_info {
                        codec_params.with_n_frames(table.valid_frames as u64);
                        *packets = table.packets;
                    }
                }
                Some(MagicCookie(data)) => {
                    codec_params.with_extra_data(data);
                }
                Some(Free) | None => {}
            }

            if audio_description.is_none() {
                error!("missing audio description chunk");
                return decode_error("caf: missing audio description chunk");
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

        Ok(codec_params)
    }
}
