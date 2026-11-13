// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::codecs::CodecParameters;
use symphonia_core::support_format;

use symphonia_core::errors::{
    Error, Result, SeekErrorKind, decode_error, seek_error, unsupported_error,
};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_ISOMP4;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::units::Time;

use std::collections::HashMap;
use std::io::{Seek, SeekFrom};
use std::num::NonZero;
use std::sync::Arc;

use crate::atoms::{AtomError, AtomIterator, AtomType, ReadAtom};
use crate::atoms::{FtypAtom, MetaAtom, MoofAtom, MoovAtom, SidxAtom, TrakAtom};
use crate::stream::*;

use log::{debug, info, trace, warn};

const ISOMP4_FORMAT_INFO: FormatInfo = FormatInfo {
    format: FORMAT_ID_ISOMP4,
    short_name: "isomp4",
    long_name: "ISO Base Media File Format",
};

pub struct TrackState {
    /// The track number.
    track_num: usize,
    /// The track ID.
    track_id: u32,
    /// The current segment.
    cur_seg: usize,
    /// The current sample index relative to the track.
    next_sample: u32,
    /// The current sample byte position relative to the start of the track.
    next_sample_pos: u64,
}

impl TrackState {
    pub fn make(track_num: usize, trak: &TrakAtom, timespan: &TimeSpan) -> (Self, Track) {
        let mut track = Track::new(trak.tkhd.id);

        // Create the codec parameters using the sample description atom.
        if let Some(codec_params) = trak.mdia.minf.stbl.stsd.make_codec_params() {
            track.with_codec_params(codec_params);
        }

        // Populate timing information.
        track
            .with_time_base(TimeBase::from_recip(timespan.timescale))
            .with_duration(timespan.duration);

        // If the track is an audio track, and the timescale is equal to the sample rate, then the
        // number of frames is equal to the duration. This is the case for almost all audio tracks.
        // If not, there is no generic, low overhead, & precise way to determine the number of
        // frames.
        if let Some(CodecParameters::Audio(audio)) = &track.codec_params {
            if let Some(sample_rate) = audio.sample_rate {
                if sample_rate == timespan.timescale.get() {
                    track.with_num_frames(timespan.duration.get());
                }
            }
        }

        let state = Self {
            track_num,
            track_id: trak.tkhd.id,
            cur_seg: 0,
            next_sample: 0,
            next_sample_pos: 0,
        };

        (state, track)
    }
}

/// Information regarding the next sample.
#[derive(Debug)]
struct NextSampleInfo {
    /// The track number of the next sample.
    track_num: usize,
    /// The track id.
    track_id: u32,
    /// The timestamp of the next sample.
    ts: Timestamp,
    /// The timestamp expressed in seconds.
    time: Time,
    /// The duration of the next sample.
    dur: Duration,
    /// The segment containing the next sample.
    seg_idx: usize,
}

/// Information regarding a sample.
#[derive(Debug)]
struct SampleDataInfo {
    /// The position of the sample in the track.
    pos: u64,
    /// The length of the sample.
    len: u32,
}

/// A representation of time, defining a duration relative to a specific frequency
#[derive(Debug)]
pub struct TimeSpan {
    pub timescale: NonZero<u32>,
    pub duration: Duration,
}

impl Default for TimeSpan {
    fn default() -> Self {
        Self { timescale: NonZero::new(1).expect("1 is non-zero"), duration: Duration::ZERO }
    }
}

impl TimeSpan {
    pub fn new(timescale: NonZero<u32>, duration: Duration) -> Self {
        TimeSpan { timescale, duration }
    }
}

/// ISO Base Media File Format (MP4, M4A, MOV, etc.) demultiplexer.
///
/// `IsoMp4Reader` implements a demuxer for the ISO Base Media File Format.
pub struct IsoMp4Reader<'s> {
    iter: AtomIterator<MediaSourceStream<'s>>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    /// Segments of the movie. Sorted in ascending order by sequence number.
    segs: Vec<Box<dyn StreamSegment>>,
    /// State tracker for each track.
    track_states: Vec<TrackState>,
    /// Optional, movie extends atom used for fragmented streams.
    moov: Arc<MoovAtom>,
    /// Seekability of the source at construction. Consumed fragments are only dropped when this
    /// is false: a backward seek finds its timestamp by scanning `segs`.
    source_is_seekable: bool,
    /// Total length of a seekable source, in bytes.
    source_len: Option<u64>,
}

impl<'s> IsoMp4Reader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        // To get to beginning of the atom.
        mss.seek_buffered_rel(-4);

        let is_seekable = mss.is_seekable();

        let mut ftyp = None;
        let mut moov = None;

        // Get the total length of the stream, if possible.
        let total_len = if is_seekable {
            let pos = mss.pos();
            let len = mss.seek(SeekFrom::End(0))?;
            mss.seek(SeekFrom::Start(pos))?;
            info!("stream is seekable with len={len} bytes.");
            Some(len)
        }
        else {
            None
        };

        let mut metadata = opts.external_data.metadata.unwrap_or_default();

        // Parse all atoms if the stream is seekable, otherwise parse all atoms up-to the mdat atom.
        let mut it = AtomIterator::new(mss, total_len);
        // Maps each track id to its cumulative duration (TimeSpan) as parsed from the segment
        // index.
        let mut sidx_timespans: HashMap<u32, TimeSpan> = HashMap::new();

        while let Some(header) = it.next_header()? {
            // Top-level atoms.
            match header.atom_type() {
                AtomType::FileType => {
                    ftyp = Some(it.read_atom::<FtypAtom>()?);
                }
                AtomType::Movie => {
                    moov = Some(it.read_atom::<MoovAtom>()?);
                }
                AtomType::SegmentIndex => {
                    let sidx = it.read_atom::<SidxAtom>()?;

                    // Calculate the total duration, per track, from the segment index atoms.
                    let sidx_timespan = sidx_timespans
                        .entry(sidx.reference_id)
                        .or_insert(TimeSpan::new(sidx.timescale, Duration::ZERO));

                    if sidx_timespan.timescale != sidx.timescale {
                        return unsupported_error(
                            "isomp4: different sidx timescale for the same track",
                        );
                    }

                    // Don't overflow.
                    // TODO: Duration should maybe be None since SIDX is non-authoritative.
                    sidx_timespan.duration = sidx_timespan
                        .duration
                        .checked_add(Duration::new(sidx.total_duration))
                        .ok_or(Error::DecodeError("isomp4: sidx total duration overflow"))?
                }
                AtomType::MediaData | AtomType::MovieFragment => {
                    // The mdat atom contains the codec bitstream data. For fragmented streams, a
                    // moof + mdat pair is required. If the ftyp and moov atoms have been read, then
                    // the top-level atom scan can exit here and begin playback immediately as an
                    // optimization. If not, then the scan must continue.
                    //
                    // The scan must also exit if the source is unseekable because in that case
                    // the format reader cannot skip past these atoms without dropping packets.
                    let is_playable = moov.is_some() && ftyp.is_some();

                    if is_playable || !is_seekable {
                        if !is_playable {
                            warn!("mp4 is not streamable.");
                        }
                        break;
                    }
                }
                AtomType::Meta => {
                    // Read the metadata atom and append it to the log.
                    let mut meta = it.read_atom::<MetaAtom>()?;

                    if let Some(rev) = meta.take_metadata() {
                        metadata.push(rev);
                    }
                }
                AtomType::Free => (),
                AtomType::Skip => (),
                _ => {
                    info!("skipping top-level atom: {:?}.", header.atom_type());
                }
            }
        }

        if ftyp.is_none() {
            return unsupported_error("isomp4: missing ftyp atom");
        }

        if moov.is_none() {
            return unsupported_error("isomp4: missing moov atom");
        }

        // If the top-level atom scan iterated across the entire source (e.g., if moov was the last
        // atom), then the iterator must return to the first moof or mdat atom. This is only
        // possible if the source is seekable. If it's not, then the media will be effectively
        // unplayable.
        if is_seekable && it.pending().is_none() {
            let mut mss = it.into_inner();
            mss.seek(SeekFrom::Start(0))?;

            it = AtomIterator::new(mss, total_len);

            while let Some(header) = it.next_header()? {
                if let AtomType::MovieFragment | AtomType::MediaData = header.atom_type() {
                    break;
                }
            }
        }

        // Fragments (moof + mdat pairs) are streamed. So if the pending atom is a moof, seek the
        // iterator to the start of the moof atom.
        if let Some(atom) = it.pending() {
            if atom.atom_type() == AtomType::MovieFragment {
                it.seek_atom_start()?;
            }
        }

        let mut moov = moov.expect("moov is checked for None above");

        if moov.is_fragmented() {
            if !sidx_timespans.is_empty() {
                info!("stream is segmented with a segment index.");
            }
            else {
                info!("stream is segmented without a segment index.");
            }
        }

        if let Some(rev) = moov.take_metadata() {
            metadata.push(rev);
        }

        // Create a track and track state for each Track (trak) atom.
        let mut tracks = Vec::with_capacity(moov.traks.len());
        let mut track_states = Vec::with_capacity(moov.traks.len());

        for (t, trak) in moov.traks.iter().enumerate() {
            // Determine the timespan of the track.
            let timespan = if moov.is_fragmented() {
                // If fragmented, prefer the duration from the sidx, if it is provided. Otherwise,
                // fallback to the mdhd.
                sidx_timespans
                    .get(&trak.tkhd.id)
                    .map(|sidx_tspan| TimeSpan::new(sidx_tspan.timescale, sidx_tspan.duration))
                    .unwrap_or_else(|| {
                        TimeSpan::new(trak.mdia.mdhd.timescale, trak.mdia.mdhd.duration.into())
                    })
            }
            else {
                // If non-fragmented, use the total duration (media timescale) from the track's
                // stts atom. Since edits are not currently supported, this is the duration of all
                // samples that will be yielded.
                //
                // TODO: Support edits. Once supported, prefer the tkhd duration.
                let duration = Duration::from(trak.mdia.minf.stbl.stts.total_duration);

                TimeSpan::new(trak.mdia.mdhd.timescale, duration)
            };

            let (track_state, track) = TrackState::make(t, trak, &timespan);

            tracks.push(track);
            track_states.push(track_state);
        }

        // The number of tracks specified in the moov atom must match the number in the mvex atom.
        if let Some(mvex) = &moov.mvex {
            if mvex.trexs.len() != moov.traks.len() {
                return decode_error("isomp4: mvex and moov track number mismatch");
            }
        }

        // The moov atom will be shared among all segments and the demuxer using an Arc.
        let moov = Arc::new(moov);

        let segs: Vec<Box<dyn StreamSegment>> = vec![Box::new(MoovSegment::new(moov.clone()))];

        // Populate media information.
        let mut media_info = MediaInfo::new();
        media_info.with_time_base(TimeBase::from_recip(moov.mvhd.timescale));
        media_info.with_duration(Duration::new(moov.mvhd.duration));

        Ok(IsoMp4Reader {
            iter: it,
            media_info,
            tracks,
            metadata,
            track_states,
            segs,
            moov,
            source_is_seekable: is_seekable,
            source_len: total_len,
        })
    }

    /// `true` when the pending atom runs through the end of a seekable source.
    ///
    /// Nothing can follow that atom. Leaving it pending lets `read_raw_boxed_slice_exact` seek
    /// back to an indexed sample after playback has reached EOF.
    fn pending_atom_reaches_eof(&self) -> bool {
        match (self.source_len, self.iter.pending().and_then(|header| header.end())) {
            (Some(len), Some(end)) => end >= len,
            _ => false,
        }
    }

    /// Idempotently gets information regarding the next sample of the media stream. This function
    /// selects the next sample with the lowest timestamp of all tracks.
    fn next_sample_info(&self) -> Result<Option<NextSampleInfo>> {
        let mut earliest = None;

        // TODO: Consider returning samples based on lowest byte position in the track instead of
        // timestamp. This may be important if video tracks are ever decoded (i.e., DTS vs. PTS).

        for (state, track) in self.track_states.iter().zip(&self.tracks) {
            // Get the timebase of the track used to calculate the presentation time.
            let tb = track.time_base.expect("track always created with a timebase");

            // Get the next timestamp for the next sample of the current track. The next sample may
            // be in a future segment.
            for (seg_idx_delta, seg) in self.segs[state.cur_seg..].iter().enumerate() {
                // Try to get the timestamp for the next sample of the track from the segment.
                if let Some(timing) = seg.sample_timing(state.track_num, state.next_sample)? {
                    // Calculate the presentation time using the timestamp.
                    let Some(ts) = timing.ts.try_into().ok()
                    else {
                        return Ok(None);
                    };

                    let Some(sample_time) = tb.calc_time(ts)
                    else {
                        return Ok(None);
                    };

                    // Compare the presentation time of the sample from this track to other tracks,
                    // and select the track with the earliest presentation time.
                    match earliest {
                        Some(NextSampleInfo { time, .. }) if time <= sample_time => {
                            // Earliest is less than or equal to the track's next sample
                            // presentation time. No need to update earliest.
                        }
                        _ => {
                            // Earliest was either None, or greater than the track's next sample
                            // presentation time. Update earliest.
                            earliest = Some(NextSampleInfo {
                                track_num: state.track_num,
                                track_id: state.track_id,
                                ts,
                                time: sample_time,
                                dur: Duration::from(timing.dur),
                                seg_idx: seg_idx_delta + state.cur_seg,
                            });
                        }
                    }

                    // Either the next sample of the track had the earliest presentation time seen
                    // thus far, or it was greater than those from other tracks, but there is no
                    // reason to check samples in future segments.
                    break;
                }
            }
        }

        Ok(earliest)
    }

    fn consume_next_sample(&mut self, info: &NextSampleInfo) -> Result<Option<SampleDataInfo>> {
        // Get the track state.
        let track = &mut self.track_states[info.track_num];

        // Get the segment associated with the sample.
        let seg = &self.segs[info.seg_idx];

        // Get the sample data descriptor.
        let sample_data_desc = seg.sample_data(track.track_num, track.next_sample, false)?;

        // The sample base position in the sample data descriptor remains constant if the sample
        // followed immediately after the previous sample. In this case, the track state's
        // next_sample_pos is the position of the current sample. If the base position has jumped,
        // then the base position is the position of current the sample.
        let pos = if sample_data_desc.base_pos > track.next_sample_pos {
            sample_data_desc.base_pos
        }
        else {
            track.next_sample_pos
        };

        // Advance the track's current segment to the next sample's segment.
        track.cur_seg = info.seg_idx;

        // Advance the track's next sample number and position.
        track.next_sample += 1;
        track.next_sample_pos = pos + u64::from(sample_data_desc.size);

        Ok(Some(SampleDataInfo { pos, len: sample_data_desc.size }))
    }

    /// Drop fragments that every track has moved past.
    ///
    /// Returns the number of leading segments removed. Seekable sources keep every fragment so a
    /// backward seek can scan `segs`. The last segment stays: it is the sample-index predecessor
    /// of the next fragment. Callers must not be holding an absolute `segs` index.
    fn reclaim_consumed_segments(&mut self) -> usize {
        if self.source_is_seekable || self.track_states.is_empty() || self.segs.len() <= 1 {
            return 0;
        }

        // The last segment stands in for a sample that belongs to a fragment not yet read.
        let last_idx = self.segs.len() - 1;
        let mut first_live = last_idx;

        for state in &self.track_states {
            let mut live = last_idx;

            // The first segment at or after `cur_seg` that still contains the next sample. An
            // idle track's `cur_seg` can point at a fragment that no longer contains that sample.
            for (idx, seg) in self.segs.iter().enumerate().skip(state.cur_seg) {
                if seg.track_sample_range(state.track_num).contains(&state.next_sample) {
                    live = idx;
                    break;
                }
            }

            first_live = first_live.min(live);
        }

        if first_live == 0 {
            return 0;
        }

        self.segs.drain(..first_live);

        for state in &mut self.track_states {
            // `cur_seg` may sit in the drained prefix when that fragment did not contain the
            // track's next sample.
            state.cur_seg = state.cur_seg.saturating_sub(first_live);
        }

        first_live
    }

    fn try_read_more_segments(&mut self) -> Result<bool> {
        // Do this before `segs.last()`, and only when no `NextSampleInfo.seg_idx` is in hand.
        // Those indices are absolute into `segs`.
        self.reclaim_consumed_segments();

        // If all tracks ended in the last segment, then do not try to read anymore segments.
        //
        // Note, there will always be one segment because the moov atom was converted into a segment
        // when the reader was instantiated.
        let last_seg = self.segs.last().expect("always at least one segment from moov atom");
        if last_seg.all_tracks_ended() {
            return Ok(false);
        }

        // Continue iterating over atoms until a segment (a moof + mdat atom pair) is found. All
        // other atoms will be ignored.
        loop {
            let header = match self.iter.next_header() {
                Ok(Some(header)) => header,
                Ok(None) => break,
                // If fragmented, an EOF is the only way to truly detect the end of stream.
                Err(AtomError::Other(Error::IoError(err)))
                    if self.moov.is_fragmented()
                        && err.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                // Passthrough other errors.
                Err(err) => return Err(err.into()),
            };

            match header.atom_type() {
                AtomType::MediaData => {
                    return Ok(true);
                }
                AtomType::MovieFragment => {
                    let moof = self.iter.read_atom::<MoofAtom>()?;

                    // A moof segment can only be created if the media is fragmented.
                    if self.moov.is_fragmented() {
                        // Get the last segment.
                        let last_seg =
                            self.segs.last().expect("always at least one segment from moov atom");

                        // Create a new segment for the moof atom.
                        let seg = MoofSegment::new(moof, self.moov.clone(), last_seg.as_ref());

                        // Segments should have a monotonic sequence number.
                        if seg.sequence_num() <= last_seg.sequence_num() {
                            warn!("moof fragment has a non-monotonic sequence number.");
                        }

                        // Push the segment.
                        self.segs.push(Box::new(seg));
                    }
                    else {
                        return decode_error("isomp4: moof atom present without mvex atom");
                    }
                }
                _ => {
                    trace!("skipping atom: {:?}.", header.atom_type());
                }
            }
        }

        // If no atoms were returned above, then the end-of-stream has been reached.
        Ok(false)
    }

    fn seek_track_by_time(&mut self, track_num: usize, time: Time) -> Result<SeekedTo> {
        // Convert time to timestamp for the track.
        if let Some(track) = self.tracks.get(track_num) {
            let tb = track.time_base.expect("track always created with a timebase");
            let ts = tb.calc_timestamp(time).ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?;
            self.seek_track_by_ts(track_num, ts)
        }
        else {
            seek_error(SeekErrorKind::Unseekable)
        }
    }

    fn seek_track_by_ts(&mut self, track_num: usize, ts: Timestamp) -> Result<SeekedTo> {
        debug!("seeking track_num={track_num} to frame_ts={ts}");

        struct SeekLocation {
            seg_idx: usize,
            sample_num: u32,
        }

        // Can only seek to 0 or positive timestamps.
        if ts.is_negative() {
            return seek_error(SeekErrorKind::OutOfRange);
        }

        let mut seg_skip = 0;

        let seek_loc = 'locate: loop {
            // Iterate over all segments and attempt to find the segment and sample number that
            // contains the desired timestamp. Skip segments already examined.
            for (seg_idx, seg) in self.segs.iter().enumerate().skip(seg_skip) {
                // Earlier fragments may have been reclaimed on an unseekable source.
                if (ts.get() as u64) < seg.track_ts_range(track_num).start {
                    return seek_error(SeekErrorKind::OutOfRange);
                }

                if let Some(sample_num) = seg.ts_sample(track_num, ts.get() as u64)? {
                    break 'locate SeekLocation { seg_idx, sample_num };
                }

                // Mark the segment as examined.
                seg_skip = seg_idx + 1;
            }

            // Otherwise, try to read more segments from the stream. Reclaim first so `seg_skip`,
            // an absolute index, moves with the drained prefix. The reclaim inside
            // `try_read_more_segments` is then a no-op.
            let reclaimed = self.reclaim_consumed_segments();
            seg_skip = seg_skip.saturating_sub(reclaimed);

            if !self.try_read_more_segments()? {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        };

        let seg = &self.segs[seek_loc.seg_idx];

        // Get the sample timing.
        let timing = match seg.sample_timing(track_num, seek_loc.sample_num)? {
            Some(t) => t,
            None => return seek_error(SeekErrorKind::OutOfRange),
        };

        // Try to convert the sample timing to a timestamp.
        let actual_ts = match Timestamp::try_from(timing.ts) {
            Ok(ts) => ts,
            _ => return seek_error(SeekErrorKind::OutOfRange),
        };

        // Get the sample information.
        let data_desc = seg.sample_data(track_num, seek_loc.sample_num, true)?;

        // Update the track's next sample information to point to the seeked sample.
        let track = &mut self.track_states[track_num];

        track.cur_seg = seek_loc.seg_idx;
        track.next_sample = seek_loc.sample_num;
        track.next_sample_pos = data_desc.base_pos
            + match data_desc.offset {
                Some(o) => o,
                None => return seek_error(SeekErrorKind::OutOfRange),
            };

        debug!(
            "seeked track_num={} (track_id={}) to packet_ts={} (delta={})",
            track_num,
            track.track_id,
            actual_ts,
            actual_ts.saturating_delta(ts),
        );

        Ok(SeekedTo { track_id: track.track_id, required_ts: ts, actual_ts })
    }
}

impl Scoreable for IsoMp4Reader<'_> {
    fn score(_src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        Ok(Score::Supported(255))
    }
}

impl ProbeableFormat<'_> for IsoMp4Reader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(IsoMp4Reader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(
            ISOMP4_FORMAT_INFO,
            &["mp4", "m4a", "m4p", "m4b", "m4r", "m4v", "mov"],
            &["video/mp4", "audio/m4a"],
            &[b"ftyp"] // Top-level atoms
        )]
    }
}

impl FormatReader for IsoMp4Reader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &ISOMP4_FORMAT_INFO
    }

    fn media_info(&self) -> &MediaInfo {
        &self.media_info
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        // Get the index of the track with the next-nearest (minimum) timestamp.
        let next_sample_info = loop {
            // Using the current set of segments, try to get the next sample info.
            if let Some(info) = self.next_sample_info()? {
                break info;
            }
            else if self.pending_atom_reaches_eof() {
                // `seek_atom_end` would drop the pending atom. A later seek still has to raw-read
                // samples that are already indexed, which requires some atom to be pending.
                return Ok(None);
            }
            else {
                // The inner reader of the atom iterator has been used/seeked around to read
                // packets, so resync the reader and iterator by seeking to the end of the current
                // pending atom. Under regular circumstances, no actual expensive seek operation is
                // performed since the reader should be at the end of the last iterated atom if we
                // are trying to read another.
                match self.iter.seek_atom_end() {
                    Ok(_) | Err(AtomError::NoPendingAtom) => (),
                    Err(_) => return decode_error("sync lost"),
                };

                // No more segments. If the stream is unseekable, it may be the case that there are
                // more segments coming. If the stream is seekable it might be fragmented and no
                // segments are found in the moov atom. Iterate atoms until a new segment is found
                // or the end-of-stream is reached
                if !self.try_read_more_segments()? {
                    return Ok(None);
                }
            }
        };

        // Get the position and length information of the next sample.
        let sample_info = match self.consume_next_sample(&next_sample_info)? {
            Some(s) => s,
            None => return decode_error("isomp4: failed to get next sample data"),
        };

        let data =
            self.iter.read_raw_boxed_slice_exact(sample_info.pos, sample_info.len as usize)?;

        Ok(Some(Packet::new(
            next_sample_info.track_id,
            next_sample_info.ts,
            next_sample_info.dur,
            data,
        )))
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        if self.tracks.is_empty() {
            return seek_error(SeekErrorKind::Unseekable);
        }

        let seeked = match to {
            SeekTo::Timestamp { ts, track_id } => {
                // The seek timestamp is in timebase units specific to the selected track. Get the
                // selected track and use the timebase to convert the timestamp into time units so
                // that the other tracks can be seeked.
                if let Some((track_num, track)) =
                    self.tracks.iter().enumerate().find(|(_, track)| track.id == track_id)
                {
                    // Convert to time units.
                    let tb = match track.time_base {
                        Some(tb) => tb,
                        None => return seek_error(SeekErrorKind::Unseekable),
                    };
                    let time =
                        tb.calc_time(ts).ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;

                    // Seek all tracks excluding the primary track to the desired time.
                    for t in 0..self.track_states.len() {
                        if t != track_num {
                            self.seek_track_by_time(t, time)?;
                        }
                    }

                    // Seek the primary track and return the result.
                    self.seek_track_by_ts(track_num, ts)
                }
                else {
                    seek_error(SeekErrorKind::InvalidTrack)
                }
            }
            SeekTo::Time { time, track_id } => {
                // If provided, find the track number of the track with the desired track_id, or
                // default to the first track.
                let track_num = match track_id {
                    Some(id) => self
                        .tracks
                        .iter()
                        .position(|track| track.id == id)
                        .ok_or(Error::SeekError(SeekErrorKind::InvalidTrack))?,
                    None => 0,
                };

                // Seek all tracks excluding the selected track and discard the result.
                for t in 0..self.track_states.len() {
                    if t != track_num {
                        self.seek_track_by_time(t, time)?;
                    }
                }

                // Seek the primary track and return the result.
                self.seek_track_by_time(track_num, time)
            }
        };

        // Track state now points at the seeked samples, so fragments before that position are
        // consumed. No `NextSampleInfo.seg_idx` is held here.
        if seeked.is_ok() {
            self.reclaim_consumed_segments();
        }

        seeked
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.iter.into_inner()
    }
}

impl ReadAtom for MediaSourceStream<'_> {}

impl From<AtomError> for Error {
    fn from(value: AtomError) -> Self {
        // Map all atom iteration errors to decode errors.
        let msg = match value {
            AtomError::InvalidAtomSize => "isomp4: invalid atom size",
            AtomError::InvalidUtf8 => "isomp4: invalid utf-8 string",
            AtomError::MaximumDepthReached => "isomp4: maximum recursion depth reached",
            AtomError::NoParentAtom => "isomp4: no parent atom",
            AtomError::NoPendingAtom => "isomp4: no atom pending read",
            AtomError::Overrun => "isomp4: overrun while reading atom",
            AtomError::SeekOutOfRange => "isomp4: out-of-bounds seek for a non-seekable stream",
            AtomError::UnexpectedEndOfAtom => "isomp4: unexpected end of atom",
            AtomError::UnexpectedPosition => "isomp4: unexpected position",
            AtomError::UnexpectedUnknownSizeAtom => "isomp4: unknown size atom has sized parent",
            AtomError::UnexpectedReadOperation => "isomp4: unexpected read operation",
            AtomError::UnknownAtomSize => "isomp4: unknown atom size",
            AtomError::Other(err) => return err,
        };
        Error::DecodeError(msg)
    }
}

// fn convert_timescale(
//     duration: u64,
//     src_timescale: NonZero<u32>,
//     dst_timescale: NonZero<u32>,
// ) -> Duration {
//     if src_timescale == dst_timescale {
//         return Duration::from(duration);
//     }
//     Duration::from(
//         ((duration as u128 * dst_timescale.get() as u128) / src_timescale.get() as u128) as u64,
//     )
// }

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use symphonia_core::errors::{Error, SeekErrorKind};
    use symphonia_core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
    use symphonia_core::io::{
        MediaSource, MediaSourceStream, MediaSourceStreamOptions, ReadBytes, ReadOnlySource,
    };
    use symphonia_core::units::{Time, Timestamp};

    use super::IsoMp4Reader;

    const FRAGMENTS: usize = 32;

    fn push_u16(buf: &mut Vec<u8>, value: u16) {
        buf.extend_from_slice(&value.to_be_bytes());
    }

    fn push_u32(buf: &mut Vec<u8>, value: u32) {
        buf.extend_from_slice(&value.to_be_bytes());
    }

    fn with_atom(buf: &mut Vec<u8>, tag: &[u8; 4], write_body: impl FnOnce(&mut Vec<u8>)) {
        let start = buf.len();
        buf.extend_from_slice(&[0, 0, 0, 0]);
        buf.extend_from_slice(tag);
        write_body(buf);
        let size = u32::try_from(buf.len() - start).unwrap();
        buf[start..start + 4].copy_from_slice(&size.to_be_bytes());
    }

    fn full_box(buf: &mut Vec<u8>, flags: u32) {
        buf.push(0);
        buf.push(((flags >> 16) & 0xff) as u8);
        buf.push(((flags >> 8) & 0xff) as u8);
        buf.push((flags & 0xff) as u8);
    }

    fn write_trak(buf: &mut Vec<u8>, track_id: u32) {
        with_atom(buf, b"trak", |buf| {
            with_atom(buf, b"tkhd", |buf| {
                full_box(buf, 0);
                push_u32(buf, 0);
                push_u32(buf, 0);
                push_u32(buf, track_id);
                push_u32(buf, 0);
                push_u32(buf, 0);
                push_u32(buf, 0);
                push_u32(buf, 0);
                push_u16(buf, 0);
                push_u16(buf, 0);
                push_u16(buf, 0);
            });

            with_atom(buf, b"mdia", |buf| {
                with_atom(buf, b"mdhd", |buf| {
                    full_box(buf, 0);
                    push_u32(buf, 0);
                    push_u32(buf, 0);
                    push_u32(buf, 1000);
                    push_u32(buf, 0);
                    push_u16(buf, 0);
                    push_u16(buf, 0);
                });

                with_atom(buf, b"hdlr", |buf| {
                    full_box(buf, 0);
                    push_u32(buf, 0);
                    buf.extend_from_slice(b"soun");
                    buf.extend_from_slice(&[0; 12]);
                });

                with_atom(buf, b"minf", |buf| {
                    with_atom(buf, b"stbl", |buf| {
                        with_atom(buf, b"stsd", |buf| {
                            full_box(buf, 0);
                            push_u32(buf, 1);
                            with_atom(buf, b"raw ", |buf| {
                                buf.extend_from_slice(&[0; 6]);
                                push_u16(buf, 1);
                                push_u16(buf, 0);
                                push_u16(buf, 0);
                                push_u32(buf, 0);
                                push_u16(buf, 1);
                                push_u16(buf, 8);
                                push_u16(buf, 0);
                                push_u16(buf, 0);
                                push_u32(buf, 0);
                            });
                        });

                        // Empty sample tables: the moov sample range is 0..0.
                        for tag in [b"stts", b"stsc"] {
                            with_atom(buf, tag, |buf| {
                                full_box(buf, 0);
                                push_u32(buf, 0);
                            });
                        }

                        with_atom(buf, b"stsz", |buf| {
                            full_box(buf, 0);
                            push_u32(buf, 0);
                            push_u32(buf, 0);
                        });
                    });
                });
            });
        });
    }

    fn write_trex(buf: &mut Vec<u8>, track_id: u32) {
        with_atom(buf, b"trex", |buf| {
            full_box(buf, 0);
            push_u32(buf, track_id);
            push_u32(buf, 1);
            push_u32(buf, 1);
            push_u32(buf, 1);
            push_u32(buf, 0);
        });
    }

    /// `ftyp` + `moov` + `FRAGMENTS` `moof`/`mdat` pairs.
    ///
    /// Each fragment carries one sample for track 1. When `idle_track` is set, track 2 has a
    /// single sample in fragment 0 and no samples after that.
    fn fragmented_movie(idle_track: bool) -> Vec<u8> {
        let mut buf = Vec::new();

        with_atom(&mut buf, b"ftyp", |buf| {
            buf.extend_from_slice(b"isom");
            push_u32(buf, 0);
        });

        with_atom(&mut buf, b"moov", |buf| {
            with_atom(buf, b"mvhd", |buf| {
                full_box(buf, 0);
                push_u32(buf, 0);
                push_u32(buf, 0);
                push_u32(buf, 1000);
                push_u32(buf, 0);
                push_u32(buf, 0);
                push_u16(buf, 0);
            });

            write_trak(buf, 1);
            if idle_track {
                write_trak(buf, 2);
            }

            with_atom(buf, b"mvex", |buf| {
                write_trex(buf, 1);
                if idle_track {
                    write_trex(buf, 2);
                }
            });
        });

        for index in 0..FRAGMENTS {
            let tracks: &[u32] = if idle_track && index == 0 { &[1, 2] } else { &[1] };
            // Track 1's byte is the fragment index. Track 2, when present, follows it.
            let payload = [index as u8, 0];
            let payload = if tracks.len() == 2 { &payload[..] } else { &payload[..1] };

            let moof_start = buf.len();
            let mut data_offsets = Vec::with_capacity(tracks.len());

            with_atom(&mut buf, b"moof", |buf| {
                with_atom(buf, b"mfhd", |buf| {
                    full_box(buf, 0);
                    push_u32(buf, (index + 1) as u32);
                });

                for &track_id in tracks {
                    with_atom(buf, b"traf", |buf| {
                        with_atom(buf, b"tfhd", |buf| {
                            full_box(buf, 0);
                            push_u32(buf, track_id);
                        });
                        with_atom(buf, b"trun", |buf| {
                            full_box(buf, 0x1);
                            push_u32(buf, 1);
                            data_offsets.push(buf.len());
                            push_u32(buf, 0);
                        });
                    });
                }
            });

            let moof_len = buf.len() - moof_start;
            for (sample, pos) in data_offsets.iter().enumerate() {
                let data_offset = u32::try_from(moof_len + 8 + sample).unwrap();
                buf[*pos..*pos + 4].copy_from_slice(&data_offset.to_be_bytes());
            }

            with_atom(&mut buf, b"mdat", |buf| {
                buf.extend_from_slice(payload);
            });
        }

        buf
    }

    /// `try_new` rewinds 4 bytes, matching a probe that has already read the atom size.
    fn open_fragmented(bytes: Vec<u8>, seekable: bool) -> IsoMp4Reader<'static> {
        let source: Box<dyn MediaSource> = if seekable {
            Box::new(Cursor::new(bytes))
        }
        else {
            Box::new(ReadOnlySource::new(Cursor::new(bytes)))
        };

        let mut mss = MediaSourceStream::new(source, MediaSourceStreamOptions::default());
        mss.read_buf_exact(&mut [0u8; 4]).expect("probe bytes");
        IsoMp4Reader::try_new(mss, FormatOptions::default()).expect("isomp4 reader")
    }

    #[test]
    fn fragmented_unseekable_drops_consumed_moof_segments() {
        let mut reader = open_fragmented(fragmented_movie(false), false);
        let mut count = 0usize;

        loop {
            match reader.next_packet().expect("next packet") {
                Some(packet) => {
                    assert_eq!(packet.pts.get(), count as i64);
                    assert_eq!(&*packet.data, &[count as u8]);
                    assert!(reader.segs.len() <= 2);
                    count += 1;
                }
                None => {
                    assert_eq!(reader.segs.len(), 1);
                    break;
                }
            }
        }

        assert_eq!(count, FRAGMENTS);
    }

    #[test]
    fn fragmented_unseekable_rejects_seek_before_retained_history() {
        let mut reader = open_fragmented(fragmented_movie(false), false);

        for _ in 0..3 {
            reader.next_packet().expect("next packet").expect("some packet");
        }

        let first_ts = reader.segs[0].track_ts_range(0).start;
        assert_eq!(first_ts, 1);

        for to in [
            SeekTo::Time { time: Time::ZERO, track_id: None },
            SeekTo::Timestamp { ts: Timestamp::from(0), track_id: 1 },
        ] {
            assert!(matches!(
                reader.seek(SeekMode::Accurate, to),
                Err(Error::SeekError(SeekErrorKind::OutOfRange))
            ));
        }

        let packet = reader.next_packet().expect("packet after failed seek").expect("some packet");
        assert_eq!(packet.pts.get(), 3);
        assert_eq!(&*packet.data, &[3]);

        let first_ts = reader.segs[0].track_ts_range(0).start;
        let ts = Timestamp::try_from(first_ts).expect("valid retained timestamp");
        let seeked = reader
            .seek(SeekMode::Accurate, SeekTo::Timestamp { ts, track_id: 1 })
            .expect("seek to retained history boundary");
        assert_eq!(seeked.actual_ts.get(), first_ts as i64);

        let packet = reader.next_packet().expect("packet after seek").expect("some packet");
        assert_eq!(packet.pts.get(), first_ts as i64);
        assert_eq!(&*packet.data, &[first_ts as u8]);
    }

    #[test]
    fn fragmented_seekable_keeps_moof_segments_for_seek() {
        let mut reader = open_fragmented(fragmented_movie(false), true);

        loop {
            if reader.next_packet().expect("next packet").is_none() {
                break;
            }
        }

        // moov plus one segment per fragment.
        assert_eq!(reader.segs.len(), FRAGMENTS + 1);

        reader
            .seek(SeekMode::Accurate, SeekTo::Time { time: Time::ZERO, track_id: None })
            .expect("seek to start");

        let packet = reader.next_packet().expect("packet after seek").expect("some packet");
        assert_eq!(&*packet.data, &[0]);
    }

    #[test]
    fn fragmented_unseekable_idle_track_does_not_pin_segments() {
        let mut reader = open_fragmented(fragmented_movie(true), false);
        let mut count = 0usize;
        let mut track_two = 0usize;

        loop {
            match reader.next_packet().expect("next packet") {
                Some(packet) => {
                    if packet.track_id == 2 {
                        track_two += 1;
                        assert_eq!(packet.pts.get(), 0);
                    }
                    count += 1;
                    // Both fragment-0 samples are the two timestamp-0 packets.
                    if count >= 2 {
                        assert!(reader.segs.len() <= 2);
                    }
                }
                None => {
                    assert_eq!(reader.segs.len(), 1);
                    break;
                }
            }
        }

        assert_eq!(count, FRAGMENTS + 1);
        assert_eq!(track_two, 1);
    }
}
