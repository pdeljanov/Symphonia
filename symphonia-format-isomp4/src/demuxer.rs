// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.


use symphonia_core::{errors::end_of_stream_error, support_format};

use symphonia_core::codecs::{CodecParameters, CODEC_TYPE_AAC};
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::formats::prelude::*;
use symphonia_core::io::{ByteStream, MediaSource, MediaSourceStream};
use symphonia_core::meta::MetadataQueue;
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};
use symphonia_core::units::Time;

use std::io::{Seek, SeekFrom};
use std::sync::Arc;

use crate::atoms::{AtomIterator, AtomType};
use crate::atoms::{FtypAtom, MoovAtom, MoofAtom, SidxAtom, TrakAtom, MetaAtom, MvexAtom};
use crate::atoms::stsd::SampleDescription;
use crate::stream::*;

use log::{info, trace, warn};

pub struct TrackState {
    codec_params: CodecParameters,
    /// The track number.
    track_num: u32,
    /// The current segment.
    cur_seg: usize,
    /// The current sample index relative to the stream.
    next_sample: u32,
    /// The current sample byte position relative to the start of the stream.
    next_sample_pos: u64,
}

impl TrackState {

    pub fn new(track_num: u32, trak: &TrakAtom) -> Self {

        let mut codec_params = CodecParameters::new();

        codec_params.with_time_base(TimeBase::new(1, trak.mdia.mdhd.timescale));

        // Add a stream for the respective codec.
        match trak.mdia.minf.stbl.stsd.sample_desc {
            // MP4 audio (generally AAC)
            SampleDescription::Mp4a(ref mp4a) => {
                codec_params
                    .for_codec(CODEC_TYPE_AAC)
                    .with_sample_rate(
                        mp4a.sound_desc.sample_rate as u32)
                    .with_extra_data(
                        mp4a.esds.descriptor.dec_config.dec_specific_config.extra_data.clone());
            }
            _ => ()
        }

        Self {
            codec_params,
            track_num,
            cur_seg: 0,
            next_sample: 0,
            next_sample_pos: 0,
        }
    }

    pub fn codec_params(&self) -> CodecParameters {
        self.codec_params.clone()
    }
}

/// Information regarding the next sample.
#[derive(Debug)]
struct NextSampleInfo {
    /// The track number of the next sample.
    track_num: u32,
    /// The timestamp of the next sample.
    ts: u64,
    /// The timestamp expressed in seconds.
    time: Time,
    /// The segment containing the next sample.
    seg_idx: usize,
}

/// Information regarding a sample.
#[derive(Debug)]
struct SampleDataInfo {
    /// The position of the sample in the stream.
    pos: u64,
    /// The length of the sample.
    len: u32,
}

/// ISO Base Media File Format (MP4, M4A, MOV, etc.) demultiplexer.
///
/// `IsoMp4Reader` implements a demuxer for the ISO Base Media File Format.
pub struct IsoMp4Reader {
    iter: AtomIterator<MediaSourceStream>,
    streams: Vec<Stream>,
    cues: Vec<Cue>,
    metadata: MetadataQueue,
    /// Segments of the movie. Sorted in ascending order by sequence number.
    segs: Vec<Box<dyn StreamSegment>>,
    /// State tracker for each track.
    tracks: Vec<TrackState>,
    /// Optional, movie extends atom used for fragmented streams.
    mvex: Option<Arc<MvexAtom>>,
}

impl IsoMp4Reader {
    /// Idempotently gets information regarding the next sample of the media stream. This function
    /// selects the next sample with the lowest timestamp of all tracks.
    fn next_sample_info(&self) -> Result<Option<NextSampleInfo>> {
        let mut earliest = None;

        // TODO: Consider returning samples based on lowest byte position in the stream instead of
        // timestamp. This may be important if video streams are ever decoded (i.e., DTS vs. PTS).

        for (track, stream) in self.tracks.iter().zip(&self.streams) {

            // Get the timebase of the track used to calculate the presentation time.
            let tb = stream.codec_params.time_base.unwrap();

            // Get the next timestamp for the next sample of the current track. The next sample may
            // be in a future segment.
            for (seg_idx_delta, seg) in self.segs[track.cur_seg as usize..].iter().enumerate() {

                // Try to get the timestamp for the next sample of the track from the segment.
                if let Some(sample_ts) = seg.sample_ts(track.track_num, track.next_sample)? {

                    // Calculate the presentation time using the timestamp.
                    let sample_time = tb.calc_time(sample_ts);

                    // Compare the presentation time of the sample from this track to other tracks,
                    // and select the track with the earliest presentation time.
                    match earliest {
                        Some(NextSampleInfo { track_num: _, ts: _, time, seg_idx: _ })
                            if time <= sample_time =>
                        {
                            // Earliest is less than or equal to the track's next sample presentation
                            // time. No need to update earliest.
                            ()
                        }
                        _ => {
                            // Earliest was either None, or greater than the track's next sample
                            // presentation time. Update earliest.
                            earliest = Some(NextSampleInfo{
                                track_num: track.track_num,
                                ts: sample_ts,
                                time: sample_time,
                                seg_idx: seg_idx_delta + track.cur_seg,
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
        let track = &mut self.tracks[info.track_num as usize];

        // Get the segment associated with the sample.
        let seg = &self.segs[info.seg_idx];

        // Get the sample data descriptor.
        let sample_data_desc = seg.sample_data(track.track_num, track.next_sample)?;

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

    fn try_read_more_segments(&mut self) -> Result<()> {
        // Continue iterating over atoms until a segment (a moof + mdat atom pair) is found. All
        // other atoms will be ignored.
        while let Some(header) = self.iter.next_no_consume()? {
            match header.atype {
                AtomType::MediaData => {
                    // Consume the atom from the iterator so that on the next iteration a new atom
                    // will be read.
                    self.iter.consume_atom();

                    return Ok(());
                }
                AtomType::MovieFragment => {
                    let moof = self.iter.read_atom::<MoofAtom>()?;

                    // A moof segment can only be created if the mvex atom is present.
                    if let Some(mvex) = &self.mvex {
                        // Get the last segment. Note, there will always be one segment because the
                        // moov atom is converted into a segment when the reader is instantiated.
                        let last_seg = self.segs.last().unwrap();

                        // Create a new segment for the moof atom.
                        let seg = MoofSegment::new(moof, mvex.clone(), last_seg);

                        // Segments should have a monotonic sequence number.
                        if seg.sequence_num() <= last_seg.sequence_num() {
                            warn!("moof fragment has a non-monotonic sequence number.");
                        }

                        // Push the segment.
                        self.segs.push(Box::new(seg));
                    }
                    else {
                        // TODO: This is a fatal error.
                        return decode_error("moof atom present without mvex atom");
                    }
                },
                _ => {
                    trace!("skipping atom: {:?}.", header.atype);
                    self.iter.consume_atom();
                }
            }
        }

        // If no atoms were returned above, then the end-of-stream has been reached.
        end_of_stream_error()
    }

}

impl QueryDescriptor for IsoMp4Reader {
    fn query() -> &'static [Descriptor] {
        &[
            support_format!(
                "isomp4",
                "ISO Base Media File Format",
                &[ "mp4", "m4a", "m4p", "m4b", "m4r", "m4v", "mov" ],
                &[ "video/mp4", "audio/m4a" ],
                &[ b"ftyp" ] // Top-level atoms
            ),
        ]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}

impl FormatReader for IsoMp4Reader {

    fn try_new(mut mss: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {

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

        let mut metadata = MetadataQueue::default();

        // Parse all atoms if the stream is seekable, otherwise parse all atoms up-to the mdat atom.
        let mut iter = AtomIterator::new_root(mss, total_len);

        while let Some(header) = iter.next()? {
            // Top-level atoms.
            match header.atype {
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
                AtomType::MediaData |
                AtomType::MovieFragment => {
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
                    // Read the metadata atom and queue it.
                    let meta = iter.read_atom::<MetaAtom>()?;
                    meta.take_metadata(&mut metadata);
                }
                AtomType::Free => (),
                AtomType::Skip => (),
                _ => {
                    info!("skipping top-level atom: {:?}.", header.atype);
                }
            }
        }

        if ftyp.is_none() {
            return unsupported_error("missing ftyp atom");
        }

        if moov.is_none() {
            return unsupported_error("missing moov atom");
        }

        // If the stream was seekable, then all atoms in the stream were scanned. Seek back to the
        // beginning of the stream.
        if is_seekable {
            let mut mss = iter.into_inner();
            mss.seek(SeekFrom::Start(0))?;

            iter = AtomIterator::new_root(mss, total_len);
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

        moov.take_metadata(&mut metadata);

        // Filter all media trak atoms for supported audio tracks and instantiate a track state
        // for each.
        let tracks = moov.traks.iter()
                            //    .filter(|trak| trak.mdia.hdlr.track_type == TrackType::Sound)
                               .enumerate()
                               .map(|(t, trak)| TrackState::new(t as u32, trak))
                               .collect::<Vec<TrackState>>();

        // Instantiate Stream(s) for all tracks selected above.
        let streams = tracks.iter()
                            .map(|track| Stream::new(track.track_num, track.codec_params()))
                            .collect();

        // A Movie Extends (mvex) atom is required to support segmented streams. If the mvex atom is
        // present, wrap it in an Arc so it can be shared amongst all segments.
        let mvex = moov.mvex.take().map(|m| Arc::new(m));

        let segs: Vec<Box<dyn StreamSegment>> = vec![ Box::new(MoovSegment::new(moov)) ];

        Ok(IsoMp4Reader {
            iter,
            streams,
            cues: Default::default(),
            metadata,
            tracks,
            segs,
            mvex,
        })
    }

    fn next_packet(&mut self) -> Result<Packet> {
        // Get the index of the track with the next-nearest (minimum) timestamp.
        let next_sample_info = loop {
            // Using the current set of segments, try to get the next sample info.
            if let Some(info) = self.next_sample_info()? {
                break info;
            }
            else {
                // No more segments. If the stream is unseekable, it may be the case that there are
                // more segments coming. Iterate atoms until a new segment is found or the
                // end-of-stream is reached.
                self.try_read_more_segments()?;
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
                return decode_error("packet out-of-bounds for a non-seekable stream");
            }
        }

        Ok(Packet::new_from_boxed_slice(
            next_sample_info.track_num,
            next_sample_info.ts,
            0,
            reader.read_boxed_slice_exact(sample_info.len as usize)?
        ))
    }

    fn metadata(&self) -> &MetadataQueue {
        &self.metadata
    }

    fn cues(&self) -> &[Cue] {
        &self.cues
    }

    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn seek(&mut self, _to: SeekTo) -> Result<SeekedTo> {
        unsupported_error("seeking unsupported")
    }

}