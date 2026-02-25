// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::chunks::*;
use alloc::{boxed::Box, vec::Vec};
use log::{debug, error, info, warn};
use core::num::NonZero;
use symphonia_core::{
    audio::{Channels, Position},
    codecs::{
        CodecParameters,
        audio::{well_known::CODEC_ID_AAC, *},
    },
    errors::{Error, Result, SeekErrorKind, decode_error, seek_error, unsupported_error},
    formats::{
        prelude::*,
        probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable},
        well_known::FORMAT_ID_CAF,
    },
    io::*,
    meta::{Metadata, MetadataLog},
    support_format,
    units::{TimeBase, Timestamp},
};

use symphonia_common::mpeg::formats::*;

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
    FixedAudioPacket {
        bytes_per_packet: NonZero<u32>,
        frames_per_packet: NonZero<u32>,
        start_pts: Timestamp,
    },
    VariableAudioPacket {
        packets: Vec<CafPacket>,
        current_packet_index: usize,
    },
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
            PacketInfo::FixedAudioPacket { bytes_per_packet, frames_per_packet, start_pts } => {
                let pos = self.reader.pos();
                let data_pos = pos - self.data_start_pos;

                let bytes_per_packet = u64::from(bytes_per_packet.get());
                let frames_per_packet = u64::from(frames_per_packet.get());

                // frames_per_packet == 1 means uncompressed data which we want to chunk into
                // MAX_FRAMES_PER_PACKET
                let max_bytes_to_read = if frames_per_packet == 1 {
                    bytes_per_packet * MAX_FRAMES_PER_PACKET
                } else {
                    bytes_per_packet
                };

                let bytes_remaining = if let Some(data_len) = self.data_len {
                    data_len.saturating_sub(data_pos)
                } else {
                    max_bytes_to_read
                };

                if bytes_remaining == 0 {
                    return Ok(None);
                }

                let bytes_to_read = max_bytes_to_read.min(bytes_remaining);

                // Calculate the packet duration.
                let Some(dur) = (bytes_to_read / bytes_per_packet)
                    .checked_mul(frames_per_packet)
                    .map(Duration::new)
                else {
                    warn!("packet duration exceeds maximum representable duration");
                    return Ok(None);
                };

                // Calculate the packet PTS by offsetting the duration read so far from the start
                // PTS.
                let Some(pts) = (data_pos / bytes_per_packet)
                    .checked_mul(frames_per_packet)
                    .and_then(|offset| start_pts.checked_add(Duration::from(offset)))
                else {
                    warn!("media exceeds maximum representable duration");
                    return Ok(None);
                };

                let buf = self.reader.read_boxed_slice(bytes_to_read as usize)?;

                Ok(Some(Packet::new(0, pts, dur, buf)))
            }
            PacketInfo::VariableAudioPacket { packets, current_packet_index } => {
                if let Some(packet) = packets.get(*current_packet_index) {
                    *current_packet_index += 1;
                    let buffer = self.reader.read_boxed_slice(packet.size as usize)?;
                    Ok(Some(Packet::new(0, packet.start_frame, packet.frames, buffer)))
                } else if *current_packet_index == packets.len() {
                    Ok(None)
                } else {
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
                // The timebase is required to calculate the timestamp.
                let tb = self.time_base().ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
                // If the timestamp overflows, the seek if out-of-range.
                tb.calc_timestamp(time).ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?
            }
        };

        // Range check.
        let (min_ts, max_ts) = self.seek_bounds()?;

        if required_ts < min_ts {
            return seek_error(SeekErrorKind::OutOfRange);
        } else if let Some(max_ts) = max_ts {
            if required_ts > max_ts {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        }

        match &mut self.packet_info {
            PacketInfo::FixedAudioPacket { bytes_per_packet, frames_per_packet, start_pts } => {
                let is_uncompressed = frames_per_packet.get() == 1;

                let frames_per_packet = if is_uncompressed {
                    // Packetization for uncompressed data is performed by chunking the stream into
                    // packets of MAX_FRAMES_PER_PACKET frames each.
                    MAX_FRAMES_PER_PACKET
                } else {
                    u64::from(frames_per_packet.get())
                };

                // Calculate the duration from the start PTS. This is equal to the number of frames
                // to the requested PTS. The seek is out-of-range if this is negative. To maintain
                // determinstic packet timestamps, the seek must jump to a packet boundary before
                // the requested seek time, so align down to a packet boundary.
                let dur_from_start = required_ts
                    .duration_from(*start_pts)
                    .and_then(|d| d.align_down(Duration::from(frames_per_packet)))
                    .ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?;

                // Calculate the actual PTS that will be seeked to after aligning down.
                let actual_ts = start_pts
                    .checked_add(dur_from_start)
                    .ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?;

                // Calculate the position to seek to. If compressed, there may be more than one
                // frame per packet so the duration from start must be converted from frames to
                // packets by dividing by frames-per-packet. This is lossless since we're aligned
                // to a multiple of frames-per-packet in all cases.
                let seek_pos = self.data_start_pos
                    + u64::from(bytes_per_packet.get()) * dur_from_start.get()
                        / if is_uncompressed { 1 } else { frames_per_packet };

                if self.reader.is_seekable() {
                    self.reader.seek(SeekFrom::Start(seek_pos))?;
                } else {
                    let current_pos = self.reader.pos();
                    if seek_pos >= current_pos {
                        self.reader.ignore_bytes(seek_pos - current_pos)?;
                    } else {
                        return seek_error(SeekErrorKind::ForwardOnly);
                    }
                }

                debug!(
                    "seek required_ts: {}, actual_ts: {}, (difference: {})",
                    required_ts,
                    actual_ts,
                    actual_ts.saturating_delta(required_ts),
                );

                Ok(SeekedTo { track_id: 0, required_ts, actual_ts })
            }
            PacketInfo::VariableAudioPacket { packets, current_packet_index } => {
                let current_ts = if let Some(packet) = packets.get(*current_packet_index) {
                    packet.start_frame
                } else {
                    error!("invalid packet index: {current_packet_index}");
                    return decode_error("caf: invalid packet index");
                };

                let search_range = if current_ts < required_ts {
                    *current_packet_index..packets.len()
                } else {
                    0..*current_packet_index
                };

                let packet_after_ts = packets[search_range]
                    .partition_point(|packet| packet.start_frame < required_ts);
                let seek_packet_index = packet_after_ts.saturating_sub(1);
                let seek_packet = &packets[seek_packet_index];

                let seek_pos = self.data_start_pos + seek_packet.data_offset;

                if self.reader.is_seekable() {
                    self.reader.seek(SeekFrom::Start(seek_pos))?;
                } else {
                    let current_pos = self.reader.pos();
                    if seek_pos >= current_pos {
                        self.reader.ignore_bytes(seek_pos - current_pos)?;
                    } else {
                        return seek_error(SeekErrorKind::ForwardOnly);
                    }
                }

                *current_packet_index = seek_packet_index;
                let actual_ts = seek_packet.start_frame;

                debug!(
                    "seek required_ts: {}, actual_ts: {}, (difference: {}, packet: {})",
                    required_ts,
                    actual_ts,
                    actual_ts.saturating_delta(required_ts),
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

    fn seek_bounds(&self) -> Result<(Timestamp, Option<Timestamp>)> {
        let (dur, delay, padding) = self
            .tracks
            .first()
            .map(|track| (track.num_frames, track.delay.unwrap_or(0), track.padding.unwrap_or(0)))
            .unwrap_or((None, 0, 0));

        let dur = dur.map(Duration::from);

        let min_ts = Timestamp::from(-i64::from(delay));
        let max_ts = dur
            .and_then(|dur| min_ts.checked_add(dur))
            .and_then(|ts| ts.checked_add(Duration::from(padding)));

        Ok((min_ts, max_ts))
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

        // TODO: Bits per sample and bits per coded sample are wrong for compressed.

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

        // Need to set max_frames_per_packet in all cases in case it is needed (e.g. ADPCM).
        if desc.frames_per_packet == 1 {
            // If uncompressed data, chunk the stream into MAX_FRAMES_PER_PACKET blocks.
            codec_params
                .with_max_frames_per_packet(MAX_FRAMES_PER_PACKET)
                .with_frames_per_block(MAX_FRAMES_PER_PACKET);
        } else {
            // TODO: Handle variable frames per packet (frames per packet == 0).
            codec_params
                .with_max_frames_per_packet(u64::from(desc.frames_per_packet))
                .with_frames_per_block(u64::from(desc.frames_per_packet));
        }

        self.packet_info = if desc.is_variable_packet_format() {
            PacketInfo::VariableAudioPacket { packets: Vec::new(), current_packet_index: 0 }
        } else {
            PacketInfo::FixedAudioPacket {
                // UNWRAP: Cannot reach this else block if either bytes/packet or frames/packet are
                // zero.
                bytes_per_packet: NonZero::new(desc.bytes_per_packet).unwrap(),
                frames_per_packet: NonZero::new(desc.frames_per_packet).unwrap(),
                start_pts: Timestamp::new(0),
            }
        };

        Ok(())
    }

    fn read_chunks(&mut self) -> Result<Track> {
        use Chunk::*;

        let mut track = Track::new(0);
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
                            ..
                        } = &self.packet_info
                        {
                            num_frames = Some(
                                (data_len / u64::from(bytes_per_packet.get()))
                                    * u64::from(frames_per_packet.get()),
                            );
                        }
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
                Some(PacketTable(table)) => {
                    if table.priming_frames > 0 {
                        track.with_delay(table.priming_frames as u32);
                    }
                    if table.remainder_frames > 0 {
                        track.with_padding(table.remainder_frames as u32);
                    }

                    match &mut self.packet_info {
                        PacketInfo::FixedAudioPacket { start_pts, .. } => {
                            if table.priming_frames > 0 {
                                *start_pts = Timestamp::from(-i64::from(table.priming_frames));
                            }
                        }
                        PacketInfo::VariableAudioPacket { packets, .. } => {
                            num_frames = Some(table.valid_frames as u64);
                            *packets = table.packets;
                        }
                        _ => (),
                    }
                }
                Some(MagicCookie(data)) => {
                    match codec_params.codec {
                        CODEC_ID_AAC => {
                            // For AAC, the magic cookie is an ES Descriptor. However, the extra
                            // data format for AAC decoders is solely the content of the
                            // decoder-specific information descriptor.
                            let mut reader = BufReader::new(&data);

                            let (desc_tag, desc_len) = read_object_descriptor_header(&mut reader)?;

                            if desc_tag == ClassTag::EsDescriptor {
                                // Parse the ES Descriptor.
                                let desc = ESDescriptor::read(&mut reader, desc_len)?;

                                // Attach the extra data stored in the decoder-specific
                                // configuration.
                                if let Some(info) = desc.dec_config.dec_specific_info {
                                    codec_params.with_extra_data(info.extra_data);
                                }
                            }
                        }
                        _ => {
                            // For all other formats attach the entire magic cookie.
                            codec_params.with_extra_data(data);
                        }
                    }
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

        track.with_codec_params(CodecParameters::Audio(codec_params));

        if let Some(num_frames) = num_frames {
            track.with_num_frames(num_frames);
        }

        Ok(track)
    }
}
