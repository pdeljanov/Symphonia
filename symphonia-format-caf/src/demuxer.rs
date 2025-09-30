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
    audio::{Channels, Position},
    codecs::audio::*,
    codecs::CodecParameters,
    errors::{decode_error, seek_error, unsupported_error, Result, SeekErrorKind},
    formats::prelude::*,
    formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable},
    formats::well_known::FORMAT_ID_CAF,
    io::*,
    meta::{Metadata, MetadataLog},
    support_format,
    units::{TimeBase, TimeStamp},
};

const MAX_FRAMES_PER_PACKET: u64 = 1152;

const CAF_FORMAT_INFO: FormatInfo =
    FormatInfo { format: FORMAT_ID_CAF, short_name: "caf", long_name: "Core Audio Format" };

/// Core Audio Format (CAF) format reader.
///
/// `CafReader` implements a demuxer for Core Audio Format containers.
pub struct CafReader<'s> {
    reader: MediaSourceStream<'s>,
    tracks: Vec<Track>,
    chapters: Option<ChapterGroup>,
    metadata: MetadataLog,
    data_start_pos: u64,
    data_len: Option<u64>,
    packet_info: PacketInfo,
}

enum PacketInfo {
    Unknown,
    FixedAudioPacket { bytes_per_packet: u32, frames_per_packet: u32 },
    VariableAudioPacket { packets: Vec<CafPacket>, current_packet_index: usize },
}

impl Scoreable for CafReader<'_> {
    fn score(_src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        Ok(Score::Supported(255))
    }
}

impl ProbeableFormat<'_> for CafReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(CafReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(CAF_FORMAT_INFO, &["caf"], &["audio/x-caf"], &[b"caff"])]
    }
}

impl FormatReader for CafReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &CAF_FORMAT_INFO
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        match &mut self.packet_info {
            PacketInfo::FixedAudioPacket { bytes_per_packet, frames_per_packet } => {
                let pos = self.reader.pos();
                let data_pos = pos - self.data_start_pos;

                let bytes_per_packet = *bytes_per_packet as u64;
                let frames_per_packet = *frames_per_packet as u64;

                // frames_per_packet == 1 means uncompressed data which we want to chunk into MAX_FRAMES_PER_PACKET
                let max_bytes_to_read = if frames_per_packet == 1 {
                    bytes_per_packet * MAX_FRAMES_PER_PACKET
                }
                else {
                    bytes_per_packet
                };

                let bytes_remaining = if let Some(data_len) = self.data_len {
                    data_len.saturating_sub(data_pos)
                }
                else {
                    max_bytes_to_read
                };

                if bytes_remaining == 0 {
                    return Ok(None);
                }

                let bytes_to_read = max_bytes_to_read.min(bytes_remaining);
                let packet_duration = bytes_to_read / bytes_per_packet * frames_per_packet;
                let packet_timestamp = (data_pos / bytes_per_packet) * frames_per_packet;
                let buffer = self.reader.read_boxed_slice(bytes_to_read as usize)?;
                Ok(Some(Packet::new_from_boxed_slice(0, packet_timestamp, packet_duration, buffer)))
            }
            PacketInfo::VariableAudioPacket { packets, ref mut current_packet_index } => {
                if let Some(packet) = packets.get(*current_packet_index) {
                    *current_packet_index += 1;
                    let buffer = self.reader.read_boxed_slice(packet.size as usize)?;
                    Ok(Some(Packet::new_from_boxed_slice(
                        0,
                        packet.start_frame,
                        packet.frames,
                        buffer,
                    )))
                }
                else if *current_packet_index == packets.len() {
                    Ok(None)
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

    fn chapters(&self) -> Option<&ChapterGroup> {
        self.chapters.as_ref()
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

        if let Some(duration) = self.duration() {
            if duration < required_ts {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        }

        match &mut self.packet_info {
            PacketInfo::FixedAudioPacket {
                bytes_per_packet: bytes_per_frame,
                frames_per_packet,
            } => {
                let frames_per_packet = if *frames_per_packet == 1 {
                    // Packetization for uncompressed data is performed by chunking the stream into
                    // packets of MAX_FRAMES_PER_PACKET frames each.
                    MAX_FRAMES_PER_PACKET
                }
                else {
                    *frames_per_packet as u64
                };

                // To allow for determinstic packet timestamps, we want the seek to jump to the
                // packet boundary before the requested seek time.
                let actual_ts = (required_ts / frames_per_packet) * frames_per_packet;
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
            PacketInfo::VariableAudioPacket { packets, current_packet_index } => {
                let current_ts = if let Some(packet) = packets.get(*current_packet_index) {
                    TimeStamp::from(packet.start_frame)
                }
                else {
                    error!("invalid packet index: {current_packet_index}");
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

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}

impl<'s> CafReader<'s> {
    pub fn try_new(mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let mut reader = Self {
            reader: mss,
            tracks: vec![],
            chapters: opts.external_data.chapters,
            metadata: opts.external_data.metadata.unwrap_or_default(),
            data_start_pos: 0,
            data_len: None,
            packet_info: PacketInfo::Unknown,
        };

        reader.check_file_header()?;
        let track = reader.read_chunks()?;

        reader.tracks.push(track);

        Ok(reader)
    }

    fn time_base(&self) -> Option<TimeBase> {
        self.tracks.first().and_then(|track| track.time_base)
    }

    fn duration(&self) -> Option<u64> {
        self.tracks.first().and_then(|track| track.num_frames)
    }

    fn check_file_header(&mut self) -> Result<()> {
        let file_type = self.reader.read_quad_bytes()?;
        if file_type != *b"caff" {
            return unsupported_error("caf: missing 'caff' stream marker");
        }

        let file_version = self.reader.read_be_u16()?;
        if file_version != 1 {
            error!("unsupported file version ({file_version})");
            return unsupported_error("caf: unsupported file version");
        }

        // Ignored in CAF v1
        let _file_flags = self.reader.read_be_u16()?;

        Ok(())
    }

    fn read_audio_description_chunk(
        &mut self,
        desc: &AudioDescription,
        codec_params: &mut AudioCodecParameters,
    ) -> Result<()> {
        codec_params
            .for_codec(desc.codec_id()?)
            .with_sample_rate(desc.sample_rate as u32)
            .with_bits_per_sample(desc.bits_per_channel)
            .with_bits_per_coded_sample((desc.bytes_per_packet * 8) / desc.channels_per_frame);

        match desc.channels_per_frame {
            0 => {
                // A channel count of zero should have been rejected by the AudioDescription parser
                unreachable!("Invalid channel count");
            }
            1 => {
                codec_params.with_channels(Channels::Positioned(Position::FRONT_LEFT));
            }
            2 => {
                codec_params.with_channels(Channels::Positioned(
                    Position::FRONT_LEFT | Position::FRONT_RIGHT,
                ));
            }
            n => {
                // When the channel count is >2 then enable the first N channels.
                // This can/should be overridden when parsing the channel layout chunk.
                match Position::from_count(n) {
                    Some(positions) => {
                        codec_params.with_channels(Channels::Positioned(positions));
                    }
                    None => {
                        return unsupported_error("caf: unsupported channel count");
                    }
                }
            }
        }

        // Need to set max_frames_per_packet in all cases in case it is needed (eg. adpcm)
        if desc.frames_per_packet == 1 {
            // If uncompressed data, chunk the stream into MAX_FRAMES_PER_PACKET sizes
            codec_params
                .with_max_frames_per_packet(MAX_FRAMES_PER_PACKET)
                .with_frames_per_block(MAX_FRAMES_PER_PACKET);
        }
        else {
            codec_params
                .with_max_frames_per_packet(desc.frames_per_packet as u64)
                .with_frames_per_block(desc.frames_per_packet as u64);
        }

        if desc.is_variable_packet_format() {
            self.packet_info =
                PacketInfo::VariableAudioPacket { packets: Vec::new(), current_packet_index: 0 };
        }
        else {
            self.packet_info = PacketInfo::FixedAudioPacket {
                bytes_per_packet: desc.bytes_per_packet,
                frames_per_packet: desc.frames_per_packet,
            }
        };

        Ok(())
    }

    fn read_chunks(&mut self) -> Result<Track> {
        use Chunk::*;

        let mut codec_params = AudioCodecParameters::new();
        let mut audio_desc = None;
        let mut num_frames = None;

        loop {
            match Chunk::read(&mut self.reader, &audio_desc)? {
                Some(AudioDescription(desc)) => {
                    if audio_desc.is_some() {
                        return decode_error("caf: additional Audio Description chunk");
                    }
                    self.read_audio_description_chunk(&desc, &mut codec_params)?;
                    audio_desc = Some(desc);
                }
                Some(AudioData(data)) => {
                    self.data_start_pos = data.start_pos;
                    self.data_len = data.data_len;

                    if let Some(data_len) = self.data_len {
                        if let PacketInfo::FixedAudioPacket {
                            bytes_per_packet,
                            frames_per_packet,
                        } = &self.packet_info
                        {
                            num_frames = Some(
                                (data_len / *bytes_per_packet as u64) * *frames_per_packet as u64,
                            );
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
                    if let PacketInfo::VariableAudioPacket { ref mut packets, .. } =
                        &mut self.packet_info
                    {
                        num_frames = Some(table.valid_frames as u64);
                        *packets = table.packets;
                    }
                }
                Some(MagicCookie(data)) => {
                    codec_params.with_extra_data(data);
                }
                Some(Free) | None => {}
            }

            if audio_desc.is_none() {
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

        let mut track = Track::new(0);

        track.with_codec_params(CodecParameters::Audio(codec_params));

        if let Some(num_frames) = num_frames {
            track.with_num_frames(num_frames);
        }

        Ok(track)
    }
}
