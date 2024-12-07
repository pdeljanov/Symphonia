// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{HashMap, VecDeque};
use std::convert::TryFrom;
use std::io::{Seek, SeekFrom};

use symphonia_core::errors::{seek_error, unsupported_error, Error, Result, SeekErrorKind};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_MKV;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::support_format;
use symphonia_core::units::TimeBase;

use log::warn;

use crate::codecs::make_track_codec_params;
use crate::ebml::{EbmlElement, ElementHeader, ElementIterator};
use crate::element_ids::{ElementType, ELEMENTS};
use crate::lacing::{extract_frames, Frame};
use crate::segment::{
    BlockGroupElement, ClusterElement, CuesElement, InfoElement, SeekHeadElement, TagsElement,
    TracksElement,
};

const MKV_FORMAT_INFO: FormatInfo =
    FormatInfo { format: FORMAT_ID_MKV, short_name: "matroska", long_name: "Matroska / WebM" };

#[allow(dead_code)]
pub struct TrackState {
    /// The track number.
    track_num: u32,
    /// Default frame duration in nanoseconds.
    pub(crate) default_frame_duration: Option<u64>,
}

/// Matroska (MKV) and WebM demultiplexer.
///
/// `MkvReader` implements a demuxer for the Matroska and WebM formats.
pub struct MkvReader<'s> {
    /// Iterator over EBML element headers
    iter: ElementIterator<MediaSourceStream<'s>>,
    tracks: Vec<Track>,
    track_states: HashMap<u32, TrackState>,
    current_cluster: Option<ClusterState>,
    metadata: MetadataLog,
    chapters: Option<ChapterGroup>,
    frames: VecDeque<Frame>,
    timestamp_scale: u64,
    clusters: Vec<ClusterElement>,
}

#[derive(Debug)]
struct ClusterState {
    timestamp: Option<u64>,
    end: Option<u64>,
}

impl<'s> MkvReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let is_seekable = mss.is_seekable();

        // Get the total length of the stream, if possible.
        let total_len = if is_seekable {
            let pos = mss.pos();
            let len = mss.seek(SeekFrom::End(0))?;
            mss.seek(SeekFrom::Start(pos))?;
            log::info!("stream is seekable with len={} bytes.", len);
            Some(len)
        }
        else {
            None
        };

        let mut it = ElementIterator::new(mss, total_len);
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
        let mut metadata = opts.external_data.metadata.unwrap_or_default();
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

                // Safety: The element type or position may be incorrect. The element iterator will
                // validate the type (as declared in the header) of the element at the seeked
                // position against the element type asked to be read.
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

        for track in segment_tracks.tracks {
            // Create the track state.
            let state = TrackState {
                track_num: track.number as u32,
                default_frame_duration: track.default_duration,
            };

            // Create the track.
            let mut tr = Track::new(state.track_num);

            tr.with_time_base(time_base);

            if let Some(duration) = info.duration {
                tr.with_num_frames(duration as u64);
            }

            if let Some(language) = &track.language {
                tr.with_language(language);
            }

            tr.with_flags(track.flags);

            if let Some(codec_params) = make_track_codec_params(track)? {
                tr.with_codec_params(codec_params);
            }

            tracks.push(tr);
            states.insert(state.track_num, state);
        }

        Ok(Self {
            iter: it,
            tracks,
            track_states: states,
            current_cluster,
            metadata,
            chapters: opts.external_data.chapters,
            frames: VecDeque::new(),
            timestamp_scale: info.timestamp_scale,
            clusters,
        })
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

            if !self.next_element()? {
                // There are no more elements.
                return Err(Error::SeekError(SeekErrorKind::OutOfRange));
            }
        };

        Ok(SeekedTo { track_id, required_ts: ts, actual_ts })
    }

    fn seek_track_by_ts(&mut self, track_id: u32, ts: u64) -> Result<SeekedTo> {
        let original_pos = self.iter.pos();

        let result = if self.clusters.is_empty() {
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
        };

        // On error, attempt to rollback to the original position.
        if result.is_err() {
            if let Err(err) = self.iter.seek(original_pos) {
                warn!("seek rollback failed due to {}", err)
            }
        }

        result
    }

    /// Process the next element. Returns `true` if an element was processed, `false` otherwise.
    fn next_element(&mut self) -> Result<bool> {
        if let Some(ClusterState { end: Some(end), .. }) = &self.current_cluster {
            // Make sure we don't read past the current cluster if its size is known.
            if self.iter.pos() >= *end {
                // log::debug!("ended cluster");
                self.current_cluster = None;
            }
        }

        // Each Cluster is being read incrementally so we need to keep track of
        // which cluster we are currently in.

        let header = match self.iter.read_child_header()? {
            Some(header) => header,
            None => {
                // If we reached here, it must be an end of stream.
                return Ok(false);
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
                    return Ok(true);
                }
            },
            ElementType::SimpleBlock => {
                let cluster_ts = match self.current_cluster.as_ref() {
                    Some(ClusterState { timestamp: Some(ts), .. }) => *ts,
                    Some(_) => {
                        self.iter.ignore_data()?;
                        log::warn!("missing cluster timestamp");
                        return Ok(true);
                    }
                    None => {
                        self.iter.ignore_data()?;
                        log::warn!("simple block element outside of a cluster");
                        return Ok(true);
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
                        return Ok(true);
                    }
                    None => {
                        self.iter.ignore_data()?;
                        log::warn!("block group element outside of a cluster");
                        return Ok(true);
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

        Ok(true)
    }
}

impl ProbeableFormat<'_> for MkvReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>>
    where
        Self: Sized,
    {
        Ok(Box::new(MkvReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(
            MKV_FORMAT_INFO,
            &["webm", "mkv"],
            &["video/webm", "video/x-matroska"],
            &[b"\x1A\x45\xDF\xA3"] // Top-level element Ebml element
        )]
    }
}

impl FormatReader for MkvReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &MKV_FORMAT_INFO
    }

    fn chapters(&self) -> Option<&ChapterGroup> {
        self.chapters.as_ref()
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
                let tb = track.time_base.unwrap();
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

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        loop {
            if let Some(frame) = self.frames.pop_front() {
                return Ok(Some(Packet::new_from_boxed_slice(
                    frame.track,
                    frame.timestamp,
                    frame.duration,
                    frame.data,
                )));
            }

            if !self.next_element()? {
                // Reached the end of stream.
                return Ok(None);
            }
        }
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.iter.into_inner()
    }
}

impl Scoreable for MkvReader<'_> {
    fn score(_src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        Ok(Score::Supported(255))
    }
}
