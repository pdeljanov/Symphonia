// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{HashMap, VecDeque};
use std::convert::{TryFrom, TryInto};
use std::io::{Seek, SeekFrom};

use symphonia_core::errors::{
    decode_error, end_of_stream_error, seek_error, unsupported_error, Error, Result, SeekErrorKind,
};
use symphonia_core::formats::{
    Cue, FormatOptions, FormatReader, Packet, SeekMode, SeekTo, SeekedTo, Track,
};
use symphonia_core::io::{MediaSource, MediaSourceStream, ReadBytes};
use symphonia_core::meta::{MetadataLog, MetadataBuilder, Value, Tag, MetadataRevision, Metadata};
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};
use symphonia_core::support_format;
use symphonia_core::units::TimeBase;
use webm_iterable::errors::TagIteratorError;
use webm_iterable::matroska_spec::{MatroskaSpec, Master, SimpleBlock};

use crate::compression::{Compression, decompress};
use crate::tags::block_group::BlockGroup;
use crate::tags::cue_point::CuePoint;
use crate::tags::info::Info;
use crate::tags::track_entry::TrackEntry;

use webm_iterable::WebmIterator;

struct Frame {
    pub(crate) track: u32,
    /// Absolute frame timestamp.
    pub(crate) timestamp: u64,
    pub(crate) duration: u64,
    pub(crate) data: Box<[u8]>,
}

struct TrackData {
    pub default_duration: Option<u64>,
    pub compression: Option<(Compression, Box<[u8]>)>,
}

/// Matroska (MKV) and WebM demultiplexer.
///
/// `MkvReader` implements a demuxer for the Matroska and WebM formats.
pub struct MkvReader {
    source: Option<WebmIterator<MediaSourceStream>>,
    tracks: Vec<Track>,
    tracks_data: HashMap<u64, TrackData>,
    metadata: MetadataLog,
    timestamp_scale: u64,
    current_cluster_timestamp: Option<u64>,
    cues: Vec<Cue>,
    cued_clusters: Vec<(u64,u64)>,
    frames: VecDeque<Frame>,
}

fn extract_tag_metadata(tags: Vec<MatroskaSpec>) -> Result<MetadataRevision> {
    let mut tags_metadata_builder = MetadataBuilder::new();
    for tag in tags {
        if let MatroskaSpec::Tag(Master::Full(simple_tags)) = tag {
            for tag in simple_tags {
                if let MatroskaSpec::SimpleTag(Master::Full(tag_values)) = tag {
                    let mut simple_tag_name = None;
                    let mut simple_tag_value = None;

                    for tag in tag_values {
                        match tag {
                            MatroskaSpec::TagName(val) => { simple_tag_name = Some(val); },
                            MatroskaSpec::TagString(val) => { simple_tag_value = Some(Value::String(val)); },
                            MatroskaSpec::TagBinary(val) => { simple_tag_value = Some(Value::Binary(val.into_boxed_slice())); },
                            other => { log::debug!("ignored element {:?}", other); }
                        }
                    }

                    tags_metadata_builder.add_tag(Tag::new(
                        None, 
                        &simple_tag_name.ok_or(Error::DecodeError("mkv: missing tag name"))?.into_boxed_str(), 
                        simple_tag_value.ok_or(Error::DecodeError("mkv: missing tag value"))?
                    ));
                }
            }
        }
    }
    Ok(tags_metadata_builder.metadata())
}

fn get_tracks(tags: Vec<MatroskaSpec>) -> Result<Vec<TrackEntry>> {
    let mut tracks = vec![];
    for tag in tags {
        if let MatroskaSpec::TrackEntry(Master::Full(data)) = tag {
            tracks.push(TrackEntry::try_from(data)?);
        }
    }
    Ok(tracks)
}

fn try_recover(it: &mut WebmIterator<MediaSourceStream>) -> impl FnMut(TagIteratorError) -> core::result::Result<Option<MatroskaSpec>, TagIteratorError> + '_ {
    move |e| {
        if let TagIteratorError::CorruptedFileData(_) = e {
            log::warn!("mkv: corrupted file data detected. Attempting recovery...");
            let mut next = it.next().transpose();
            while matches!(next, Err(TagIteratorError::CorruptedFileData(_))) {
                it.try_recover()?;
                next = it.next().transpose();
            }
            log::debug!("mkv: resuming file from {}", it.last_emitted_tag_offset());
            next
        } else {
            Err(e)
        }
    }
}

fn map_iterator_error(e: TagIteratorError) -> Error {
    log::debug!("mkv decode error: {}", e.to_string());
    Error::DecodeError("mkv decode error; see debug log for details")
}

impl MkvReader {

    // To be honest, I'm not a huge fan of this function living here.  It's too similar to the above `try_recover` function, only
    // it's tailored to work in the middle of a stream by seeking a cue point.  This seems like higher-level error handling that
    // should maybe be handled outside of the demuxing library.
    fn recover_self(&mut self) -> impl FnMut(TagIteratorError) -> core::result::Result<Option<MatroskaSpec>, TagIteratorError> + '_ {
        move |e| {
            if let TagIteratorError::CorruptedFileData(_) = e {
                let it = self.source.as_mut().expect("mkv: cannot recover without iterator");
                log::warn!("mkv: corrupted file data detected. Attempting recovery...");
                let mut next = it.next().transpose();
                let original_offset = it.last_emitted_tag_offset();
                while matches!(next, Err(TagIteratorError::CorruptedFileData(_))) {
                    it.try_recover()?;
                    next = it.next().transpose();
    
                    if it.last_emitted_tag_offset() > original_offset + 15_000 {
                        // If we've passed 15k bytes and are still getting errors, just try seeking from a cued point
                        // Continuing to try and read tags is slow and also likely to fail
                        let current_ts = self.current_cluster_timestamp.unwrap_or(0);
                        let next_cluster = self.cued_clusters.iter().find(|c| c.0 > current_ts);
                        if let Some((ts, _)) = next_cluster {
                            self.seek_track_by_ts(self.tracks[0].id, *ts).map_err(|_| TagIteratorError::UnexpectedEOF { tag_start: 0, tag_id: None, tag_size: None, partial_data: None })?;
                            break;
                        }
                    }
                }
                next
            } else {
                Err(e)
            }
        }
    }

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
        if self.cued_clusters.is_empty() {
            self.seek_track_by_ts_forward(track_id, ts)
        } else {
            let mut target_cluster = None;
            for cluster in &self.cued_clusters {
                if cluster.0 > ts {
                    break;
                }
                target_cluster = Some(cluster);
            }
            let cluster = target_cluster.ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?;
            let pos = cluster.1;

            let mut source = self.source.take().ok_or(Error::DecodeError("mkv: iterator not available"))?.into_inner();
            let current_pos = source.pos();
            if source.is_seekable() {
                source.seek(SeekFrom::Start(pos))?;
            } else if pos < current_pos {
                return seek_error(SeekErrorKind::ForwardOnly);
            } else {
                source.ignore_bytes(pos - current_pos)?;
            }
            self.source = Some(get_iterator(source));

            // Restore cluster's metadata
            self.current_cluster_timestamp = Some(cluster.0);

            // Seek to a specified block inside the cluster.
            self.seek_track_by_ts_forward(track_id, ts)
        }
    }

    fn next_element(&mut self) -> Result<()> {
        let source = self.source.as_mut().ok_or(Error::DecodeError("mkv: iterator not available"))?;
        if let Some(tag) = source.next().transpose().or_else(self.recover_self()).map_err(map_iterator_error)? {
            match tag {
                MatroskaSpec::Cluster(Master::End) => {
                    self.current_cluster_timestamp = None;
                },
                MatroskaSpec::Timestamp(val) => {
                    self.current_cluster_timestamp = Some(val);
                },
                MatroskaSpec::SimpleBlock(data) => {
                    let simple_block: core::result::Result<SimpleBlock<'_>,_> = data.as_slice().try_into();
                    if let Ok(simple_block) = simple_block {
                        if let Ok(frame_data) = simple_block.read_frame_data() {
                            self.append_block_frames(simple_block.timestamp, None, simple_block.track, frame_data);
                        } else {
                            log::warn!("mkv: unable to read corrupted frame data in SimpleBlock element");
                        }
                    } else {
                        log::warn!("mkv: unable to read corrupted SimpleBlock element");
                    }
                },
                MatroskaSpec::BlockGroup(Master::Full(tags)) => {            
                    if let Ok(group) = BlockGroup::try_from(&tags) {
                        if let Ok(frame_data) = group.block.read_frame_data() {
                            self.append_block_frames(group.block.timestamp, group.duration, group.block.track, frame_data);
                        } else {
                            log::warn!("mkv: unable to read corrupted frame data in BlockGroup element");
                        }
                    }
                }
                MatroskaSpec::Tags(Master::Full(tags)) => {
                    self.metadata.push(extract_tag_metadata(tags)?);
                },
                other => {
                    log::debug!("ignored element {:?}", other);
                }
            }
            Ok(())
        } else {
            end_of_stream_error()
        }
    }

    fn append_block_frames(&mut self, rel_ts: i16, block_duration: Option<u64>, track: u64, frame_data: Vec<webm_iterable::matroska_spec::Frame<'_>>) {
        if self.current_cluster_timestamp.is_none() {
            log::warn!("block element with unknown cluster timestamp");
            return;
        }

        let mut timestamp = self.current_cluster_timestamp.map(|cluster_ts| {
            if rel_ts < 0 {
                cluster_ts.saturating_sub((-rel_ts) as u64)
            } else {
                cluster_ts + rel_ts as u64
            }
        }).unwrap();
        
        let track_data = self.tracks_data.get(&track);
        let default_frame_duration = track_data.and_then(|it| it.default_duration.as_ref()).map(|it| it / self.timestamp_scale);
        let frame_duration = block_duration.map(|d| d/(frame_data.len() as u64)).or(default_frame_duration).unwrap_or(0);
        let compression = track_data.and_then(|t| t.compression.as_ref());
        let decompress_frame = |data: &[u8]| {
            if let Some(compression) = compression {
                decompress(data, &compression.0, &compression.1)
            } else {
                data.to_vec().into_boxed_slice()
            }
        };

        for frame in frame_data {
            self.frames.push_back(Frame { track: track as u32, timestamp, data: decompress_frame(frame.data), duration: frame_duration });
            timestamp += frame_duration;
        }
    }
}

fn get_iterator(source: MediaSourceStream) -> WebmIterator<MediaSourceStream> {
    WebmIterator::new(source, &[
        MatroskaSpec::Ebml(Master::Start),
        MatroskaSpec::Seek(Master::Start),
        MatroskaSpec::Tracks(Master::Start),
        MatroskaSpec::Info(Master::Start),
        MatroskaSpec::CuePoint(Master::Start),
        MatroskaSpec::Tags(Master::Start),
        MatroskaSpec::BlockGroup(Master::Start),
    ])
}

impl FormatReader for MkvReader {
    fn try_new(reader: MediaSourceStream, _options: &FormatOptions) -> Result<Self>
    where
        Self: Sized,
    {
        let mut source = reader;
        let is_seekable = source.is_seekable();

        let mut it = get_iterator(source);

        let ebml = it.next().map(|t| {
            t.or_else(|_| decode_error("mkv: unable to read file"))
        }).unwrap_or_else(|| {
            unsupported_error("mkv: not an ebml file")
        })?;

        if !matches!(ebml, MatroskaSpec::Ebml(Master::Full(ebml_data)) if ebml_data.iter().any(|d| matches!(d, MatroskaSpec::DocType(d_type) if matches!(d_type.as_str(), "matroska" | "webm"))) ) {
            return unsupported_error("mkv: not a matroska / webm file");
        }

        if !matches!(it.next(), Some(tag) if matches!(tag, Ok(MatroskaSpec::Segment(Master::Start)))) {
            return unsupported_error("mkv: missing segment element");
        }

        let mut segment_tracks: Option<Vec<TrackEntry>> = None;
        let mut info = None;
        let mut cued_clusters = Vec::new();
        let mut metadata = MetadataLog::default();
        let mut seeks = Vec::new();

        let mut next_tag: Option<MatroskaSpec> = it.next().transpose().or_else(try_recover(&mut it)).map_err(map_iterator_error)?;
        let segment_data_start = it.last_emitted_tag_offset() as u64;

        while let Some(tag) = next_tag {
            match tag {
                MatroskaSpec::Seek(Master::Full(tags)) => {
                    let mut tag_id = None;
                    let mut tag_pos = None;
                    for child in tags {
                        match child {
                            MatroskaSpec::SeekID(val) => { tag_id = Some(val.into_iter().fold(0u64, |a, c| (a << 8) + c as u64)); },
                            MatroskaSpec::SeekPosition(val) => { tag_pos = Some(val); },
                            other => {
                                log::debug!("ignored element {:?}", other);
                            }
                        }
                    }
                    // TracksId = 0x1654AE6B, InfoId = 0x1549A966, TagsId = 0x1254C367, CuesId = 0x1C53BB6B
                    if matches!(tag_id, Some(val) if matches!(val, 0x1654AE6B | 0x1549A966 | 0x1254C367 | 0x1C53BB6B)) {
                        if let Some(pos) = tag_pos {
                            seeks.push(segment_data_start + pos);
                        }
                    }
                },
                MatroskaSpec::Tracks(Master::Full(tags)) => {
                    segment_tracks = Some(get_tracks(tags)?);
                },
                MatroskaSpec::Info(Master::Full(tags)) => {
                    info = Some(Info::try_from(tags)?);
                },
                MatroskaSpec::CuePoint(Master::Full(tags)) => {
                    let cue = CuePoint::try_from(tags)?;
                    cued_clusters.push((cue.time, segment_data_start + cue.positions.cluster_position));
                },
                MatroskaSpec::Tags(Master::Full(tags)) => {
                    metadata.push(extract_tag_metadata(tags)?);
                },
                MatroskaSpec::Cluster(Master::Start) => {
                    // Don't look forward into the stream since
                    // we can't be sure that we'll find anything useful.
                    break;
                }
                other => {
                    log::debug!("ignored element {:?}", other);
                }
            }
            next_tag = it.next().transpose().or_else(try_recover(&mut it)).map_err(map_iterator_error)?;
        }

        
        source = it.into_inner();
        if is_seekable {
            seeks.sort_unstable();
            for pos in seeks {
                if source.seek(SeekFrom::Start(pos)).is_err() {
                    // If we seek beyond the end of the file here it means the seeks are wrong, but we should still try to play what we can
                    break;
                }
                let mut it = get_iterator(source);

                // Ignore errors here since seeks aren't strictly necessary
                if let Some(Ok(tag)) = it.next() {
                    match tag {
                        MatroskaSpec::Tracks(Master::Full(tags)) => {
                            segment_tracks = Some(get_tracks(tags)?);
                        },
                        MatroskaSpec::Info(Master::Full(tags)) => {
                            info = Some(Info::try_from(tags)?);
                        },
                        MatroskaSpec::Tags(Master::Full(tags)) => {
                            metadata.push(extract_tag_metadata(tags)?);
                        },
                        MatroskaSpec::CuePoint(Master::Full(tags)) => {
                            let cue = CuePoint::try_from(tags)?;
                            cued_clusters.push((cue.time, segment_data_start + cue.positions.cluster_position));
                        },
                        other => {
                            log::debug!("seek ignored element {:?}", other); 
                        }
                    }
                }
                source = it.into_inner();
            }
            source.seek(SeekFrom::Start(segment_data_start))?;
        }

        let segment_tracks =
            segment_tracks.ok_or(Error::DecodeError("mkv: missing Tracks element"))?;

        let info = info.ok_or(Error::DecodeError("mkv: missing Info element"))?;

        // TODO: remove this unwrap?
        let time_base = TimeBase::new(u32::try_from(info.timestamp_scale).unwrap(), 1_000_000_000);

        let mut tracks = Vec::new();
        let mut track_data = HashMap::new();
        for track in segment_tracks {
            tracks.push(track.to_core_track(time_base, info.duration.map(|d| d as u64))?);
            track_data.insert(
                track.number,
                TrackData { default_duration: track.default_duration, compression: track.compression }
            );
        }

        Ok(Self {
            source: Some(get_iterator(source)),
            tracks,
            tracks_data: track_data,
            current_cluster_timestamp: None,
            metadata,
            cues: Vec::new(),
            frames: VecDeque::new(),
            timestamp_scale: info.timestamp_scale,
            cued_clusters,
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
                    frame.track,
                    frame.timestamp,
                    frame.duration,
                    frame.data,
                ));
            }
            self.next_element()?;
        }
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.source.expect("mkv: source not available").into_inner()
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
