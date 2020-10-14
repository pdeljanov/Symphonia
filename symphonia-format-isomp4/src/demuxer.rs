// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.


use symphonia_core::support_format;

use symphonia_core::codecs::{CodecParameters, CODEC_TYPE_AAC};
use symphonia_core::errors::{Result, end_of_stream_error, unsupported_error};
use symphonia_core::formats::prelude::*;
use symphonia_core::io::{ByteStream, MediaSource, MediaSourceStream};
use symphonia_core::meta::MetadataQueue;
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};

use std::collections::VecDeque;
use std::io::{Seek, SeekFrom};

use crate::atoms::{AtomIterator, AtomType};
use crate::atoms::{FtypAtom, MoovAtom, MoofAtom, SidxAtom, TrakAtom, MvexAtom};
use crate::atoms::{stsz::SampleSize, stsd::SampleDescription, hdlr::TrackType};
use crate::segments::*;

use log::{info, warn};

pub struct Track {
    codec_params: CodecParameters,
    /// The current segment index.
    cur_seg: u32,
    /// The current sample run index.
    cur_sample_run: u32,
    /// The current sample index.
    cur_sample: u32,
    /// The current sample position in the stream.
    cur_sample_pos: u64,
}

impl Track {

    pub fn new(trak: &TrakAtom) -> Self {

        let mut codec_params = CodecParameters::new();

        // Add a stream for the respective codec.
        match trak.mdia.minf.stbl.stsd.sample_desc {
            // MP4 audio (generally AAC)
            SampleDescription::Mp4a(ref mp4a) => {
                codec_params
                    .for_codec(CODEC_TYPE_AAC)
                    .with_sample_rate(mp4a.sound_desc.sample_rate as u32)
                    .with_extra_data(mp4a.esds.descriptor.dec_config.dec_specific_config.extra_data.clone());
            }
            _ => (),
        }

        Self {
            codec_params,
            cur_seg: 0,
            cur_sample: 0,
            cur_sample_run: 0,
            cur_sample_pos: 0,
        }
    }

    pub fn codec_params(&self) -> CodecParameters {
        self.codec_params.clone()
    }
}

/// ISO Base Media File Format (MP4, M4A, MOV, etc.) demultiplexer.
///
/// `IsoMp4Reader` implements a demuxer for the ISO Base Media File Format.
pub struct IsoMp4Reader {
    reader: MediaSourceStream,
    streams: Vec<Stream>,
    cues: Vec<Cue>,
    metadata: MetadataQueue,
    /// Segments of the movie. Sorted in ascending order by presentation timestamp.
    segs: Vec<Segment>,
    /// Deferred segments are segments of the movie that are to be loaded on-demand. Sorted in
    /// ascending order by presentation timestamp.
    deferred_segs: VecDeque<DeferredSegment>,
    /// Tracks in the movie.
    tracks: Vec<Track>,
    /// Optional, movie extends atom used for loading segments.
    mvex: Option<MvexAtom>,
}

impl IsoMp4Reader {
    /// Gets a tuple containing the track index and timestamp for the next packet. This function
    /// selects the track based with the smallest timestamp for the next sample.
    fn next_track_for_packet(&self) -> Option<(usize, u64)> {
        let mut nearest = None;

        for (i, track) in self.tracks.iter().enumerate() {
            // Get the next timestamp for the current track, which may be in some future segment.
            for seg in &self.segs[track.cur_seg as usize..] {
                // Got a timestamp in this segment.
                if let Some(ts) = seg.tracks[i].sample_timestamp(track.cur_sample) {
                    // Choose the smallest timestamp from all tracks.
                    nearest = match nearest {
                        Some((_, min_ts)) if ts >= min_ts => continue,
                        _                                 => Some((i, ts)),
                    };

                    break;
                }
            }
        }

        nearest
    }

    /// Gets the next sample for the track with index `t`.
    fn next_sample_for_track(&mut self, t: usize) -> Option<(u64, u32)> {
        let track = &mut self.tracks[t];

        let start_seg = track.cur_seg as usize;
        let start_sample_run = track.cur_sample_run as usize;

        // The next sample may not be in the current segment or sample run. Find the appropriate
        // segment and sample run for the next sample. These loops should almost never iterate
        // more than once to advance the track.
        for (seg_skip, seg) in self.segs[start_seg..].iter().enumerate() {

            let track_seg = &seg.tracks[t];

            for (run_skip, run) in track_seg.runs[start_sample_run..].iter().enumerate() {

                // If the next sample is within the current run of samples, calculate the sample
                // position and length.
                if track.cur_sample < run.last_sample + track_seg.first_sample {

                    // If the segment was advanced, increment the current segment by the amount
                    // advanced, and reset the current sample run to 0.
                    if seg_skip > 0 || track.cur_sample == 0 {
                        track.cur_seg += seg_skip as u32;
                        track.cur_sample_run = 0;
                    }

                    // If the run was advanced, increment the current sample run by the amount
                    // advanced.
                    if run_skip > 0 || track.cur_sample == 0 {
                        track.cur_sample_run += run_skip as u32;
                    }

                    // If the segment or run was advanced, reset the current sample pos to the
                    // base position of the current run.
                    if seg_skip > 0 || run_skip > 0 || track.cur_sample == 0 {
                        track.cur_sample_pos = run.base_pos;
                    }

                    // Get the length of the sample.
                    let sample_len = match track_seg.sample_sizes {
                        SampleSize::Constant(size) => size,
                        SampleSize::Variable(ref table) => {
                            // The current sample index is relative to the track, calculate the
                            // sample index relative to the track segment.
                            let offset = track.cur_sample - track_seg.first_sample;
                            table[offset as usize]
                        }
                    };

                    // dbg!(track.cur_seg);
                    // dbg!(track.cur_sample_run);
                    // dbg!(track.cur_sample_pos);

                    // Calculate the position of the sample.
                    let sample_pos = track.cur_sample_pos;
                    track.cur_sample_pos += u64::from(sample_len);

                    // dbg!(sample_pos);
                    // dbg!(sample_len);

                    return Some((sample_pos, sample_len));
                }
            }
        }

        None
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

        let mut segs = Vec::<Segment>::new();
        let mut deferred_segs = VecDeque::new();

        // Get the total length of the stream, if possible.
        let total_len = if is_seekable {
            let pos = mss.pos();
            let len = mss.seek(SeekFrom::End(0))?;
            mss.seek(SeekFrom::Start(pos))?;
            info!("stream is seekable with len={} bytes", len);
            Some(len)
        }
        else {
            None
        };

        let mut metadata = MetadataQueue::default();

        // Parse all atoms if the stream is seekable, otherwise parse all atoms up-to the mdat atom.
        let mut iter = AtomIterator::new_root(&mut mss, total_len);

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
                    // segment index atom is indeed the first segment index because the format reader
                    // cannot practically skip past this point.
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
                AtomType::MediaData => {
                    // The mdat atom contains the codec bitstream data. If the source is unseekable
                    // then the format reader cannot skip past this atom.
                    if !is_seekable {
                        // If the moov atom hasn't been found before the mdat atom, and the stream is
                        // not seekable, then the mp4 is not streamable.
                        if moov.is_none() || ftyp.is_none() {
                            warn!("mp4 is not streamable");
                        }

                        break;
                    }
                }
                AtomType::UserData => {
                    // let udta = iter.read_atom::<UdtaAtom>()?;
                }
                AtomType::Meta => {

                }
                AtomType::MovieFragment => {

                    let moof = iter.read_atom::<MoofAtom>()?;

                    if let Some(moov) = &moov {
                        if let Some(mvex) = &moov.mvex {

                            let mut seg = Segment::from_moof(moof, &mvex);


                            if let Some(prev) = segs.last() {
                                if let Some(last_run) = prev.tracks[0].runs.last() {
                                    seg.tracks[0].first_sample += prev.tracks[0].first_sample + last_run.last_sample;
                                }
                            }

                            segs.push(seg);

                        }
                    }
                },
                AtomType::Free => (),
                AtomType::Skip => (),
                _ => {
                    info!("skipping over atom: {:?}", header.atype);
                }
            }
        }

        if ftyp.is_none() {
            return unsupported_error("missing ftyp atom");
        }

        if moov.is_none() {
            return unsupported_error("missing moov atom");
        }

        let mut moov = moov.unwrap();

        moov.push_metadata(&mut metadata);

        // Filter all media trak atoms for audio tracks and instantiate a Track for each.
        let tracks = moov.traks.iter()
                               .filter(|trak| trak.mdia.hdlr.track_type == TrackType::Sound)
                               .map(|trak| Track::new(&trak))
                               .collect::<Vec<Track>>();

        // Instantiate Stream(s) for all Track(s).
        let streams = tracks.iter()
                            .enumerate()
                            .map(|(i, track)| Stream::new(i as u32, track.codec_params()))
                            .collect();


        let mvex = moov.mvex.take();

        // Non-segmented files are represented as one big segment.
        segs.push(Segment::from_moov(moov)?);

        // A Movie Extends (mvex) atom is required to support segmented streams. If a mvex atom is
        // present, treat the media as segmented.
        if mvex.is_some() {
            info!("stream is segmented");

            // If a Segment Index (sidx) atom was found, add the segments contained within.
            if let Some(sidx) = &sidx {
                let mut segment_pos = sidx.first_offset;
                let mut earliest_pts = sidx.earliest_pts;

                // For each reference in the segment index...
                for reference in &sidx.references {
                    // The size of the segment (moof + mdat).
                    let segment_size = u64::from(reference.reference_size);

                    deferred_segs.push_back(DeferredSegment {
                        earliest_pts,
                        segment_pos,
                        segment_size,
                    });

                    earliest_pts += u64::from(reference.subsegment_duration);
                    segment_pos += segment_size;
                }
            }
        }

        Ok(IsoMp4Reader {
            reader: mss,
            streams,
            cues: Default::default(),
            metadata,
            tracks,
            segs,
            deferred_segs,
            mvex,
        })
    }

    fn next_packet(&mut self) -> Result<Packet> {
        // Get the index of the track with the next-nearest (minimum) timestamp.
        let (track, _ts) = loop {
            // Try to search within the current set of segments.
            if let Some((t, ts)) = self.next_track_for_packet() {
                break (t, ts);
            }
            else if let Some(deferred_seg) = self.deferred_segs.pop_front() {
                // The search failed within the current set of segments, but there is a deferred
                // segment. Load the deferred segment and add it to the current set of segments.
                let mut seg = deferred_seg.load(self.mvex.as_ref().unwrap(), &mut self.reader)?;

                if let Some(prev) = self.segs.last() {
                    if let Some(last_run) = prev.tracks[0].runs.last() {
                        seg.tracks[0].first_sample += prev.tracks[0].first_sample + last_run.last_sample;
                    }
                }

                self.segs.push(seg);
            }
            else {
                // End-of-stream has been reached.
                return end_of_stream_error();
            }
        };

        // Get the next sample position and length for the selected track.
        let (sample_pos, sample_len) = self.next_sample_for_track(track).unwrap();

        // Attempt a fast seek within the buffer cache.
        if self.reader.seek_buffered(sample_pos) != sample_pos {
            if self.reader.is_seekable() {
                // Fallback to a slow seek if the stream is seekable.
                self.reader.seek(SeekFrom::Start(sample_pos))?;
            }
            else if sample_pos > self.reader.pos() {
                // The stream is not seekable but the desired seek position is ahead of the reader's
                // current position, thus the seek can be emulated by ignoring the bytes up to the
                // the desired seek position.
                self.reader.ignore_bytes(sample_pos - self.reader.pos())?;
            }
            else {
                // The stream is not seekable, and the desired seek position falls outside the
                // buffer cache lower bound. This sample cannot be read.
                todo!();
            }
        }

        // Advance the current sample for the track.
        self.tracks[track].cur_sample += 1;

        Ok(Packet::new_from_boxed_slice(
            0,
            0,
            0,
            self.reader.read_boxed_slice_exact(sample_len as usize)?
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