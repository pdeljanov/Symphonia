// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{HashMap, VecDeque};
use std::convert::TryFrom;
use std::io::{Seek, SeekFrom};

use symphonia_core::audio::Layout;
use symphonia_core::codecs::{CodecParameters, CODEC_TYPE_FLAC, CODEC_TYPE_VORBIS};
use symphonia_core::errors::{
    decode_error, end_of_stream_error, seek_error, unsupported_error, Error, Result, SeekErrorKind,
};
use symphonia_core::formats::{
    Cue, FormatOptions, FormatReader, Packet, SeekMode, SeekTo, SeekedTo, Track,
};
use symphonia_core::io::{BufReader, MediaSource, MediaSourceStream, ReadBytes};
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::probe::Instantiate;
use symphonia_core::probe::{Descriptor, QueryDescriptor};
use symphonia_core::sample::SampleFormat;
use symphonia_core::support_format;
use symphonia_core::units::TimeBase;
use symphonia_utils_xiph::flac::metadata::{MetadataBlockHeader, MetadataBlockType};

use crate::codecs::codec_id_to_type;
use crate::ebml::{EbmlElement, ElementHeader, ElementIterator};
use crate::element_ids::{ElementType, ELEMENTS};
use crate::lacing::{extract_frames, read_xiph_sizes, Frame};
use crate::segment::{
    BlockGroupElement, ClusterElement, CuesElement, InfoElement, SeekHeadElement, TagsElement,
    TracksElement,
};

#[allow(dead_code)]
pub struct TrackState {
    /// Codec parameters.
    pub(crate) codec_params: CodecParameters,
    /// The track number.
    track_num: u32,
    /// Default frame duration in nanoseconds.
    pub(crate) default_frame_duration: Option<u64>,
}

/// Matroska (MKV) and WebM demultiplexer.
///
/// `MkvReader` implements a demuxer for the Matroska and WebM formats.
pub struct MkvReader {
    /// Iterator over EBML element headers
    iter: ElementIterator<MediaSourceStream>,
    tracks: Vec<Track>,
    track_states: HashMap<u32, TrackState>,
    current_cluster: Option<ClusterState>,
    metadata: MetadataLog,
    cues: Vec<Cue>,
    frames: VecDeque<Frame>,
    timestamp_scale: u64,
    clusters: Vec<ClusterElement>,
}

#[derive(Debug)]
struct ClusterState {
    timestamp: Option<u64>,
    end: Option<u64>,
}

fn vorbis_extra_data_from_codec_private(extra: &[u8]) -> Result<Box<[u8]>> {
    const VORBIS_PACKET_TYPE_IDENTIFICATION: u8 = 1;
    const VORBIS_PACKET_TYPE_SETUP: u8 = 5;

    // Private Data for this codec has the following layout:
    // - 1 byte that represents number of packets minus one;
    // - Xiph coded lengths of packets, length of the last packet must be deduced (as in Xiph lacing)
    // - packets in order:
    //    - The Vorbis identification header
    //    - Vorbis comment header
    //    - codec setup header

    let mut reader = BufReader::new(extra);
    let packet_count = reader.read_byte()? as usize;
    let packet_lengths = read_xiph_sizes(&mut reader, packet_count)?;

    let mut packets = Vec::new();
    for length in packet_lengths {
        packets.push(reader.read_boxed_slice_exact(length as usize)?);
    }

    let last_packet_length = extra.len() - reader.pos() as usize;
    packets.push(reader.read_boxed_slice_exact(last_packet_length)?);

    let mut ident_header = None;
    let mut setup_header = None;

    for packet in packets {
        match packet.get(0).copied() {
            Some(VORBIS_PACKET_TYPE_IDENTIFICATION) => {
                ident_header = Some(packet);
            }
            Some(VORBIS_PACKET_TYPE_SETUP) => {
                setup_header = Some(packet);
            }
            _ => {
                log::debug!("unsupported vorbis packet type");
            }
        }
    }

    // This is layout expected currently by Vorbis codec.
    Ok([
        ident_header.ok_or(Error::DecodeError("mkv: missing vorbis identification packet"))?,
        setup_header.ok_or(Error::DecodeError("mkv: missing vorbis setup packet"))?,
    ]
    .concat()
    .into_boxed_slice())
}

fn flac_extra_data_from_codec_private(codec_private: &[u8]) -> Result<Box<[u8]>> {
    let mut reader = BufReader::new(codec_private);

    let marker = reader.read_quad_bytes()?;
    if marker != *b"fLaC" {
        return decode_error("mkv (flac): missing flac stream marker");
    }

    let header = MetadataBlockHeader::read(&mut reader)?;

    loop {
        match header.block_type {
            MetadataBlockType::StreamInfo => {
                break Ok(reader.read_boxed_slice_exact(header.block_len as usize)?);
            }
            _ => reader.ignore_bytes(u64::from(header.block_len))?,
        }
    }
}

impl MkvReader {
    fn seek_track_by_ts_forward(&mut self, track_id: u32, ts: u64) -> Result<SeekedTo> {
        let actual_ts = 'out: loop {
            // Skip frames from the buffer until the given timestamp
            while let Some(frame) = self.frames.front() {
                if frame.timestamp + frame.duration >= ts && frame.track == track_id {
                    break 'out frame.timestamp;
                }
                else {
                    self.frames.pop_front();
                }
            }
            self.next_element()?
        };

        Ok(SeekedTo { track_id, required_ts: ts, actual_ts })
    }

    fn seek_track_by_ts(&mut self, track_id: u32, ts: u64) -> Result<SeekedTo> {
        if self.clusters.is_empty() {
            self.seek_track_by_ts_forward(track_id, ts)
        }
        else {
            let mut target_cluster = None;
            for cluster in &self.clusters {
                if cluster.timestamp > ts {
                    break;
                }
                target_cluster = Some(cluster);
            }
            let cluster = target_cluster.ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?;

            let mut target_block = None;
            for block in cluster.blocks.iter() {
                if block.track as u32 != track_id {
                    continue;
                }
                if block.timestamp > ts {
                    break;
                }
                target_block = Some(block);
            }

            let pos = match target_block {
                Some(block) => block.pos,
                None => cluster.pos,
            };
            self.iter.seek(pos)?;

            // Restore cluster's metadata
            self.current_cluster =
                Some(ClusterState { timestamp: Some(cluster.timestamp), end: cluster.end });

            // Seek to a specified block inside the cluster.
            self.seek_track_by_ts_forward(track_id, ts)
        }
    }

    fn next_element(&mut self) -> Result<()> {
        if let Some(ClusterState { end: Some(end), .. }) = &self.current_cluster {
            // Make sure we don't read past the current cluster if its size is known.
            if self.iter.pos() >= *end {
                log::debug!("ended cluster");
                self.current_cluster = None;
            }
        }

        // Each Cluster is being read incrementally so we need to keep track of
        // which cluster we are currently in.

        let header = match self.iter.read_child_header()? {
            Some(header) => header,
            None => {
                // If we reached here, it must be an end of stream.
                return end_of_stream_error();
            }
        };

        match header.etype {
            ElementType::Cluster => {
                self.current_cluster = Some(ClusterState { timestamp: None, end: header.end() });
            }
            ElementType::Timestamp => match self.current_cluster.as_mut() {
                Some(cluster) => {
                    cluster.timestamp = Some(self.iter.read_u64()?);
                }
                None => {
                    self.iter.ignore_data()?;
                    log::warn!("timestamp element outside of a cluster");
                    return Ok(());
                }
            },
            ElementType::SimpleBlock => {
                let cluster_ts = match self.current_cluster.as_ref() {
                    Some(ClusterState { timestamp: Some(ts), .. }) => *ts,
                    Some(_) => {
                        self.iter.ignore_data()?;
                        log::warn!("missing cluster timestamp");
                        return Ok(());
                    }
                    None => {
                        self.iter.ignore_data()?;
                        log::warn!("simple block element outside of a cluster");
                        return Ok(());
                    }
                };

                let data = self.iter.read_boxed_slice()?;
                extract_frames(
                    &data,
                    None,
                    &self.track_states,
                    cluster_ts,
                    self.timestamp_scale,
                    &mut self.frames,
                )?;
            }
            ElementType::BlockGroup => {
                let cluster_ts = match self.current_cluster.as_ref() {
                    Some(ClusterState { timestamp: Some(ts), .. }) => *ts,
                    Some(_) => {
                        self.iter.ignore_data()?;
                        log::warn!("missing cluster timestamp");
                        return Ok(());
                    }
                    None => {
                        self.iter.ignore_data()?;
                        log::warn!("block group element outside of a cluster");
                        return Ok(());
                    }
                };

                let group = self.iter.read_element_data::<BlockGroupElement>()?;
                extract_frames(
                    &group.data,
                    group.duration,
                    &self.track_states,
                    cluster_ts,
                    self.timestamp_scale,
                    &mut self.frames,
                )?;
            }
            ElementType::Tags => {
                let tags = self.iter.read_element_data::<TagsElement>()?;
                self.metadata.push(tags.to_metadata());
                self.current_cluster = None;
            }
            _ if header.etype.is_top_level() => {
                self.current_cluster = None;
            }
            other => {
                log::debug!("ignored element {:?}", other);
                self.iter.ignore_data()?;
            }
        }

        Ok(())
    }
}

impl FormatReader for MkvReader {
    fn try_new(mut reader: MediaSourceStream, _options: &FormatOptions) -> Result<Self>
    where
        Self: Sized,
    {
        let is_seekable = reader.is_seekable();

        // Get the total length of the stream, if possible.
        let total_len = if is_seekable {
            let pos = reader.pos();
            let len = reader.seek(SeekFrom::End(0))?;
            reader.seek(SeekFrom::Start(pos))?;
            log::info!("stream is seekable with len={} bytes.", len);
            Some(len)
        }
        else {
            None
        };

        let mut it = ElementIterator::new(reader, total_len);
        let ebml = it.read_element::<EbmlElement>()?;

        if !matches!(ebml.header.doc_type.as_str(), "matroska" | "webm") {
            return unsupported_error("mkv: not a matroska / webm file");
        }

        let segment_pos = match it.read_child_header()? {
            Some(ElementHeader { etype: ElementType::Segment, data_pos, .. }) => data_pos,
            _ => return unsupported_error("mkv: missing segment element"),
        };

        let mut segment_tracks = None;
        let mut info = None;
        let mut clusters = Vec::new();
        let mut metadata = MetadataLog::default();
        let mut current_cluster = None;

        let mut seek_positions = Vec::new();
        while let Ok(Some(header)) = it.read_child_header() {
            match header.etype {
                ElementType::SeekHead => {
                    let seek_head = it.read_element_data::<SeekHeadElement>()?;
                    for element in seek_head.seeks.into_vec() {
                        let tag = element.id as u32;
                        let etype = match ELEMENTS.get(&tag) {
                            Some((_, etype)) => *etype,
                            None => continue,
                        };
                        seek_positions.push((etype, segment_pos + element.position));
                    }
                }
                ElementType::Tracks => {
                    segment_tracks = Some(it.read_element_data::<TracksElement>()?);
                }
                ElementType::Info => {
                    info = Some(it.read_element_data::<InfoElement>()?);
                }
                ElementType::Cues => {
                    let cues = it.read_element_data::<CuesElement>()?;
                    for cue in cues.points.into_vec() {
                        clusters.push(ClusterElement {
                            timestamp: cue.time,
                            pos: segment_pos + cue.positions.cluster_position,
                            end: None,
                            blocks: Box::new([]),
                        });
                    }
                }
                ElementType::Tags => {
                    let tags = it.read_element_data::<TagsElement>()?;
                    metadata.push(tags.to_metadata());
                }
                ElementType::Cluster => {
                    // Set state for current cluster for the first call of `next_element`.
                    current_cluster = Some(ClusterState { timestamp: None, end: header.end() });

                    // Don't look forward into the stream since
                    // we can't be sure that we'll find anything useful.
                    break;
                }
                other => {
                    it.ignore_data()?;
                    log::debug!("ignored element {:?}", other);
                }
            }
        }

        if is_seekable {
            // Make sure we don't jump backwards unnecessarily.
            seek_positions.sort_by_key(|sp| sp.1);

            for (etype, pos) in seek_positions {
                it.seek(pos)?;
                match etype {
                    ElementType::Tracks => {
                        segment_tracks = Some(it.read_element::<TracksElement>()?);
                    }
                    ElementType::Info => {
                        info = Some(it.read_element::<InfoElement>()?);
                    }
                    ElementType::Tags => {
                        let tags = it.read_element::<TagsElement>()?;
                        metadata.push(tags.to_metadata());
                    }
                    ElementType::Cues => {
                        let cues = it.read_element::<CuesElement>()?;
                        for cue in cues.points.into_vec() {
                            clusters.push(ClusterElement {
                                timestamp: cue.time,
                                pos: segment_pos + cue.positions.cluster_position,
                                end: None,
                                blocks: Box::new([]),
                            });
                        }
                    }
                    _ => (),
                }
            }
        }

        let segment_tracks =
            segment_tracks.ok_or(Error::DecodeError("mkv: missing Tracks element"))?;

        if is_seekable {
            let mut reader = it.into_inner();
            reader.seek(SeekFrom::Start(segment_pos))?;
            it = ElementIterator::new(reader, total_len);
        }

        let info = info.ok_or(Error::DecodeError("mkv: missing Info element"))?;

        // TODO: remove this unwrap?
        let time_base = TimeBase::new(u32::try_from(info.timestamp_scale).unwrap(), 1_000_000_000);

        let mut tracks = Vec::new();
        let mut states = HashMap::new();
        for track in segment_tracks.tracks.into_vec() {
            let codec_type = codec_id_to_type(&track);

            let mut codec_params = CodecParameters::new();
            codec_params.with_time_base(time_base);

            if let Some(duration) = info.duration {
                codec_params.with_n_frames(duration as u64);
            }

            if let Some(audio) = track.audio {
                codec_params.with_sample_rate(audio.sampling_frequency.round() as u32);

                let format = audio.bit_depth.and_then(|bits| match bits {
                    8 => Some(SampleFormat::S8),
                    16 => Some(SampleFormat::S16),
                    24 => Some(SampleFormat::S24),
                    32 => Some(SampleFormat::S32),
                    _ => None,
                });

                if let Some(format) = format {
                    codec_params.with_sample_format(format);
                }

                if let Some(bits) = audio.bit_depth {
                    codec_params.with_bits_per_sample(bits as u32);
                }

                let layout = match audio.channels {
                    1 => Some(Layout::Mono),
                    2 => Some(Layout::Stereo),
                    3 => Some(Layout::TwoPointOne),
                    6 => Some(Layout::FivePointOne),
                    other => {
                        log::warn!(
                            "track #{} has custom number of channels: {}",
                            track.number,
                            other
                        );
                        None
                    }
                };

                if let Some(layout) = layout {
                    codec_params.with_channel_layout(layout);
                }

                if let Some(codec_type) = codec_type {
                    codec_params.for_codec(codec_type);
                    if let Some(codec_private) = track.codec_private {
                        let extra_data = match codec_type {
                            CODEC_TYPE_VORBIS => {
                                vorbis_extra_data_from_codec_private(&codec_private)?
                            }
                            CODEC_TYPE_FLAC => flac_extra_data_from_codec_private(&codec_private)?,
                            _ => codec_private,
                        };
                        codec_params.with_extra_data(extra_data);
                    }
                }
            }

            let track_id = track.number as u32;
            tracks.push(Track {
                id: track_id,
                codec_params: codec_params.clone(),
                language: track.language,
            });

            states.insert(
                track_id,
                TrackState {
                    codec_params,
                    track_num: track_id,
                    default_frame_duration: track.default_duration,
                },
            );
        }

        Ok(Self {
            iter: it,
            tracks,
            track_states: states,
            current_cluster,
            metadata,
            cues: Vec::new(),
            frames: VecDeque::new(),
            timestamp_scale: info.timestamp_scale,
            clusters,
        })
    }

    fn cues(&self) -> &[Cue] {
        &self.cues
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        if self.tracks.is_empty() {
            return seek_error(SeekErrorKind::Unseekable);
        }

        match to {
            SeekTo::Time { time, track_id } => {
                let track = match track_id {
                    Some(id) => self.tracks.iter().find(|track| track.id == id),
                    None => self.tracks.first(),
                };
                let track = track.ok_or(Error::SeekError(SeekErrorKind::InvalidTrack))?;
                let tb = track.codec_params.time_base.unwrap();
                let ts = tb.calc_timestamp(time);
                let track_id = track.id;
                self.seek_track_by_ts(track_id, ts)
            }
            SeekTo::TimeStamp { ts, track_id } => {
                match self.tracks.iter().find(|t| t.id == track_id) {
                    Some(_) => self.seek_track_by_ts(track_id, ts),
                    None => seek_error(SeekErrorKind::InvalidTrack),
                }
            }
        }
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> Result<Packet> {
        loop {
            if let Some(frame) = self.frames.pop_front() {
                return Ok(Packet::new_from_boxed_slice(
                    frame.track as u32,
                    frame.timestamp,
                    frame.duration,
                    frame.data,
                ));
            }
            self.next_element()?;
        }
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.iter.into_inner()
    }
}

impl QueryDescriptor for MkvReader {
    fn query() -> &'static [Descriptor] {
        &[support_format!(
            "matroska",
            "Matroska / WebM",
            &["webm", "mkv"],
            &["video/webm", "video/x-matroska"],
            &[b"\x1A\x45\xDF\xA3"] // Top-level element Ebml element
        )]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}
