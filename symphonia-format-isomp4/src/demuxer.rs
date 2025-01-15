// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::support_format;

use symphonia_core::errors::{
    decode_error, seek_error, unsupported_error, Error, Result, SeekErrorKind,
};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_ISOMP4;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::units::Time;

use std::io::{Seek, SeekFrom};
use std::sync::Arc;

use crate::atoms::{AtomIterator, AtomType};
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
    pub fn make(track_num: usize, trak: &TrakAtom) -> (Self, Track) {
        let mut track = Track::new(trak.tkhd.id);

        // Create the codec parameters using the sample description atom.
        if let Some(codec_params) = trak.mdia.minf.stbl.stsd.make_codec_params() {
            track.with_codec_params(codec_params);
        }

        track
            .with_time_base(TimeBase::new(1, trak.mdia.mdhd.timescale))
            .with_num_frames(trak.mdia.mdhd.duration);

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
    ts: u64,
    /// The timestamp expressed in seconds.
    time: Time,
    /// The duration of the next sample.
    dur: u32,
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

/// ISO Base Media File Format (MP4, M4A, MOV, etc.) demultiplexer.
///
/// `IsoMp4Reader` implements a demuxer for the ISO Base Media File Format.
pub struct IsoMp4Reader<'s> {
    iter: AtomIterator<MediaSourceStream<'s>>,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    /// Segments of the movie. Sorted in ascending order by sequence number.
    segs: Vec<Box<dyn StreamSegment>>,
    /// State tracker for each track.
    track_states: Vec<TrackState>,
    /// Optional, movie extends atom used for fragmented streams.
    moov: Arc<MoovAtom>,
}

impl<'s> IsoMp4Reader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        // To get to beginning of the atom.
        mss.seek_buffered_rel(-4);

        let is_seekable = mss.is_seekable();

        let mut ftyp = None;
        let mut moov = None;
        let mut sidx = None;

        // Get the total length of the stream, if possible.
        let total_len = if is_seekable {
            let pos = mss.pos();
            let len = mss.seek(SeekFrom::End(0))?;
            mss.seek(SeekFrom::Start(pos))?;
            info!("stream is seekable with len={} bytes.", len);
            Some(len)
        }
        else {
            None
        };

        let mut metadata = opts.external_data.metadata.unwrap_or_default();

        // Parse all atoms if the stream is seekable, otherwise parse all atoms up-to the mdat atom.
        let mut iter = AtomIterator::new_root(mss, total_len);

        while let Some(header) = iter.next()? {
            // Top-level atoms.
            match header.atom_type() {
                AtomType::FileType => {
                    ftyp = Some(iter.read_atom::<FtypAtom>()?);
                }
                AtomType::Movie => {
                    moov = Some(iter.read_atom::<MoovAtom>()?);
                }
                AtomType::SegmentIndex => {
                    // If the stream is not seekable, then it can only be assumed that the first
                    // segment index atom is indeed the first segment index because the format
                    // reader cannot practically skip past this point.
                    if !is_seekable {
                        sidx = Some(iter.read_atom::<SidxAtom>()?);
                        break;
                    }
                    else {
                        // If the stream is seekable, examine all segment indexes and select the
                        // index with the earliest presentation timestamp to be the first.
                        let new_sidx = iter.read_atom::<SidxAtom>()?;

                        let is_earlier = match &sidx {
                            Some(sidx) => new_sidx.earliest_pts < sidx.earliest_pts,
                            _ => true,
                        };

                        if is_earlier {
                            sidx = Some(new_sidx);
                        }
                    }
                }
                AtomType::MediaData | AtomType::MovieFragment => {
                    // The mdat atom contains the codec bitstream data. For segmented streams, a
                    // moof + mdat pair is required for playback. If the source is unseekable then
                    // the format reader cannot skip past these atoms without dropping samples.
                    if !is_seekable {
                        // If the moov atom hasn't been seen before the moof and/or mdat atom, and
                        // the stream is not seekable, then the mp4 is not streamable.
                        if moov.is_none() || ftyp.is_none() {
                            warn!("mp4 is not streamable.");
                        }

                        // The remainder of the stream will be read incrementally.
                        break;
                    }
                }
                AtomType::Meta => {
                    // Read the metadata atom and append it to the log.
                    let mut meta = iter.read_atom::<MetaAtom>()?;

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

        // If the stream was seekable, then all atoms in the media source stream were scanned. Seek
        // back to the first mdat atom for playback. If the stream is not seekable, then the atom
        // iterator is currently positioned at the first mdat atom.
        if is_seekable {
            let mut mss = iter.into_inner();
            mss.seek(SeekFrom::Start(0))?;

            iter = AtomIterator::new_root(mss, total_len);

            while let Some(header) = iter.next_no_consume()? {
                match header.atom_type() {
                    AtomType::MediaData | AtomType::MovieFragment => break,
                    _ => (),
                }
                iter.consume_atom();
            }
        }

        let mut moov = moov.unwrap();

        if moov.is_fragmented() {
            // If a Segment Index (sidx) atom was found, add the segments contained within.
            if sidx.is_some() {
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
            let (track_state, track) = TrackState::make(t, trak);

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

        Ok(IsoMp4Reader { iter, tracks, metadata, track_states, segs, moov })
    }

    /// Idempotently gets information regarding the next sample of the media stream. This function
    /// selects the next sample with the lowest timestamp of all tracks.
    fn next_sample_info(&self) -> Result<Option<NextSampleInfo>> {
        let mut earliest = None;

        // TODO: Consider returning samples based on lowest byte position in the track instead of
        // timestamp. This may be important if video tracks are ever decoded (i.e., DTS vs. PTS).

        for (state, track) in self.track_states.iter().zip(&self.tracks) {
            // Get the timebase of the track used to calculate the presentation time.
            let tb = track.time_base.unwrap();

            // Get the next timestamp for the next sample of the current track. The next sample may
            // be in a future segment.
            for (seg_idx_delta, seg) in self.segs[state.cur_seg..].iter().enumerate() {
                // Try to get the timestamp for the next sample of the track from the segment.
                if let Some(timing) = seg.sample_timing(state.track_num, state.next_sample)? {
                    // Calculate the presentation time using the timestamp.
                    let sample_time = tb.calc_time(timing.ts);

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
                                ts: timing.ts,
                                time: sample_time,
                                dur: timing.dur,
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

    fn try_read_more_segments(&mut self) -> Result<bool> {
        // If all tracks ended in the last segment, then do not try to read anymore segments.
        //
        // Note, there will always be one segment because the moov atom was converted into a segment
        // when the reader was instantiated.
        if self.segs.last().unwrap().all_tracks_ended() {
            return Ok(false);
        }

        // Continue iterating over atoms until a segment (a moof + mdat atom pair) is found. All
        // other atoms will be ignored.
        while let Some(header) = self.iter.next_no_consume()? {
            match header.atom_type() {
                AtomType::MediaData => {
                    // Consume the atom from the iterator so that on the next iteration a new atom
                    // will be read.
                    self.iter.consume_atom();

                    return Ok(true);
                }
                AtomType::MovieFragment => {
                    let moof = self.iter.read_atom::<MoofAtom>()?;

                    // A moof segment can only be created if the media is fragmented.
                    if self.moov.is_fragmented() {
                        // Get the last segment.
                        let last_seg = self.segs.last().unwrap();

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
                        // TODO: This is a fatal error.
                        return decode_error("isomp4: moof atom present without mvex atom");
                    }
                }
                _ => {
                    trace!("skipping atom: {:?}.", header.atom_type());
                    self.iter.consume_atom();
                }
            }
        }

        // If no atoms were returned above, then the end-of-stream has been reached.
        Ok(false)
    }

    fn seek_track_by_time(&mut self, track_num: usize, time: Time) -> Result<SeekedTo> {
        // Convert time to timestamp for the track.
        if let Some(track) = self.tracks.get(track_num) {
            let tb = track.time_base.unwrap();
            self.seek_track_by_ts(track_num, tb.calc_timestamp(time))
        }
        else {
            seek_error(SeekErrorKind::Unseekable)
        }
    }

    fn seek_track_by_ts(&mut self, track_num: usize, ts: u64) -> Result<SeekedTo> {
        debug!("seeking track_num={} to frame_ts={}", track_num, ts);

        struct SeekLocation {
            seg_idx: usize,
            sample_num: u32,
        }

        let mut seek_loc = None;
        let mut seg_skip = 0;

        loop {
            // Iterate over all segments and attempt to find the segment and sample number that
            // contains the desired timestamp. Skip segments already examined.
            for (seg_idx, seg) in self.segs.iter().enumerate().skip(seg_skip) {
                if let Some(sample_num) = seg.ts_sample(track_num, ts)? {
                    seek_loc = Some(SeekLocation { seg_idx, sample_num });
                    break;
                }

                // Mark the segment as examined.
                seg_skip = seg_idx + 1;
            }

            // If a seek location is found, break.
            if seek_loc.is_some() {
                break;
            }

            // Otherwise, try to read more segments from the stream.
            if !self.try_read_more_segments()? {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        }

        if let Some(seek_loc) = seek_loc {
            let seg = &self.segs[seek_loc.seg_idx];

            // Get the sample information.
            let data_desc = seg.sample_data(track_num, seek_loc.sample_num, true)?;

            // Update the track's next sample information to point to the seeked sample.
            let track = &mut self.track_states[track_num];

            track.cur_seg = seek_loc.seg_idx;
            track.next_sample = seek_loc.sample_num;
            track.next_sample_pos = data_desc.base_pos + data_desc.offset.unwrap();

            // Get the actual timestamp for this sample.
            let timing = seg.sample_timing(track_num, seek_loc.sample_num)?.unwrap();

            debug!(
                "seeked track_num={} (track_id={}) to packet_ts={} (delta={})",
                track_num,
                track.track_id,
                timing.ts,
                timing.ts as i64 - ts as i64
            );

            Ok(SeekedTo { track_id: track.track_id, required_ts: ts, actual_ts: timing.ts })
        }
        else {
            // Timestamp was not found.
            seek_error(SeekErrorKind::OutOfRange)
        }
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

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        // Get the index of the track with the next-nearest (minimum) timestamp.
        let next_sample_info = loop {
            // Using the current set of segments, try to get the next sample info.
            if let Some(info) = self.next_sample_info()? {
                break info;
            }
            else {
                // No more segments. If the stream is unseekable, it may be the case that there are
                // more segments coming. If the stream is seekable it might be fragmented and no segments are found in
                // the moov atom. Iterate atoms until a new segment is found or the
                // end-of-stream is reached
                if !self.try_read_more_segments()? {
                    return Ok(None);
                }
            }
        };

        // Get the position and length information of the next sample.
        let sample_info = self.consume_next_sample(&next_sample_info)?.unwrap();

        let reader = self.iter.inner_mut();

        // Attempt a fast seek within the buffer cache.
        if reader.seek_buffered(sample_info.pos) != sample_info.pos {
            if reader.is_seekable() {
                // Fallback to a slow seek if the stream is seekable.
                reader.seek(SeekFrom::Start(sample_info.pos))?;
            }
            else if sample_info.pos > reader.pos() {
                // The stream is not seekable but the desired seek position is ahead of the reader's
                // current position, thus the seek can be emulated by ignoring the bytes up to the
                // the desired seek position.
                reader.ignore_bytes(sample_info.pos - reader.pos())?;
            }
            else {
                // The stream is not seekable and the desired seek position falls outside the lower
                // bound of the buffer cache. This sample cannot be read.
                return decode_error("isomp4: packet out-of-bounds for a non-seekable stream");
            }
        }

        Ok(Some(Packet::new_from_boxed_slice(
            next_sample_info.track_id,
            next_sample_info.ts,
            u64::from(next_sample_info.dur),
            reader.read_boxed_slice_exact(sample_info.len as usize)?,
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

        match to {
            SeekTo::TimeStamp { ts, track_id } => {
                // The seek timestamp is in timebase units specific to the selected track. Get the
                // selected track and use the timebase to convert the timestamp into time units so
                // that the other tracks can be seeked.
                if let Some((track_num, track)) =
                    self.tracks.iter().enumerate().find(|(_, track)| track.id == track_id)
                {
                    // Convert to time units.
                    let time = track.time_base.unwrap().calc_time(ts);

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
        }
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.iter.into_inner()
    }
}
