// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{Result, decode_error};
use symphonia_core::io::{MediaSourceStream, MediaSource};

use crate::atoms::{AtomIterator, AtomType};
use crate::atoms::{MoofAtom, MoovAtom, StcoAtom, Co64Atom, MvexAtom};
use crate::atoms::{stsz::SampleSize, stts::SampleDurationEntry, hdlr::TrackType};

use std::io::{Seek, SeekFrom};

#[derive(Debug)]
pub struct SampleRun {
    /// The position of the first byte of the first sample in this run of samples.
    pub base_pos: u64,
    /// The index of the first sample in the run, relative to the track segment.
    pub first_sample: u32,
    /// One plus the index of the last sample in this run, relative to the track segment.
    pub last_sample: u32,
}

#[derive(Debug)]
pub struct TrackSegment {
    /// Sample runs.
    pub runs: Vec<SampleRun>,
    /// The temporal position of the first sample in this segment.
    pub base_pts: u64,
    /// The index of the first sample in this segment, relative to the track.
    pub first_sample: u32,
    /// The size of each sample in this track, stored individually or as a constant.
    pub sample_sizes: SampleSize,
    /// The duration of each sample in this track, stored as runs of samples with the same duration.
    pub sample_durations: Vec<SampleDurationEntry>,
}

impl TrackSegment {

    fn sample_size(&self, sample: usize) -> Result<u32> {
        let sample_size = match self.sample_sizes {
            SampleSize::Constant(size) => size,
            SampleSize::Variable(ref sizes) => {
                if let Some(size) = sizes.get(sample) {
                    *size
                }
                else {
                    return decode_error("missing stsz entry");
                }
            }
        };

        Ok(sample_size)
    }

    /// Get the timestamp for the sample.
    pub fn sample_timestamp(&self, mut sample: u32) -> Option<u64> {
        // If sample is greater than the upper bound of this track segment, return nothing.
        if let Some(last_run) = self.runs.last() {
            if self.first_sample + last_run.last_sample < sample {
                return None;
            }
        }
        
        // Likewise, if sample is less than the lower bound of this track segment, return nothing.
        if let Some(first_run) = self.runs.first() {
            if self.first_sample + first_run.first_sample > sample {
                return None;
            }
            
            // However, if sample is within the bounds of this track segment, make sample relative
            // to the track segment.
            sample -= first_run.first_sample + self.first_sample;
        }

        let mut ts = self.base_pts;
        let mut next_entry_first_sample = 0;

        // The Stts atom compactly encodes a mapping between number of samples and sample duration.
        // Iterate through each entry until the entry containing the next sample is found. The next
        // packet timestamp is then the sum of the product of sample count and sample duration for
        // the n-1 iterated entries, plus the product of the number of consumed samples in the n-th
        // iterated entry and sample duration.
        for entry in &self.sample_durations {
            next_entry_first_sample += entry.sample_count;

            if sample < next_entry_first_sample {
                let entry_samples = sample + entry.sample_count - next_entry_first_sample;
                ts += u64::from(entry.sample_delta) * u64::from(entry_samples);

                return Some(ts);
            }

            ts += u64::from(entry.sample_count) * u64::from(entry.sample_delta);
        }

        // No more samples.
        None
    }

}

fn get_chunk_offset(
    stco: &Option<StcoAtom>,
    co64: &Option<Co64Atom>,
    chunk: usize
) -> Result<Option<u64>> {
    // Get the offset from either the stco or co64 atoms.
    if let Some(stco) = stco.as_ref() {
        // 32-bit offset
        if let Some(offset) = stco.chunk_offsets.get(chunk) {
            Ok(Some(u64::from(*offset)))
        }
        else {
            decode_error("missing stco entry")
        }
    }
    else if let Some(co64) = co64.as_ref() {
        // 64-bit offset
        if let Some(offset) = co64.chunk_offsets.get(chunk) {
            Ok(Some(*offset))
        }
        else {
            decode_error("missing co64 entry")
        }
    }
    else {
        // This should never happen because it is mandatory to have either a stco or co64 atom,
        // but it does happen sometimes so don't actually seek anywhere.
        Ok(None)
    }
}

#[derive(Debug)]
pub struct Segment {
    pub sequence: u32,
    pub tracks: Vec<TrackSegment>,
}

impl Segment {

    /// Instantiate a segment from the provide moov atom.
    pub fn from_moov(moov: MoovAtom) -> Result<Segment> {
        let mut tracks = Vec::new();

        // For each track.
        for trak in moov.traks {

            if trak.mdia.hdlr.track_type != TrackType::Sound {
                continue;
            }

            // Decompose stbl into its constituent tables.
            let stsz = trak.mdia.minf.stbl.stsz;
            let stts = trak.mdia.minf.stbl.stts;
            let stsc = trak.mdia.minf.stbl.stsc;
            let stco = trak.mdia.minf.stbl.stco;
            let co64 = trak.mdia.minf.stbl.co64;

            let mut track = TrackSegment {
                runs: Default::default(),
                base_pts: 0,
                first_sample: 0,
                sample_sizes: stsz.sample_sizes,
                sample_durations: stts.entries,
            };

            // For each chunk group.
            for pair in stsc.entries.windows(2) {
                let (start, end) = (&pair[0], &pair[1]);

                let mut sample = start.first_sample;

                // For each chunk in the group.
                for c in start.first_chunk..end.first_chunk {
                    let base_pos = get_chunk_offset(&stco, &co64, c as usize)?.unwrap();

                    let run = SampleRun {
                        base_pos,
                        first_sample: sample,
                        last_sample: sample + start.samples_per_chunk,
                    };

                    sample += start.samples_per_chunk;

                    track.runs.push(run);
                }
            }

            tracks.push(track);
        }

        Ok(Segment {
            sequence: 0,
            tracks,
        })
    }

    // Instantiate a segment from the provided moof atom.
    pub fn from_moof(mut moof: MoofAtom, mvex: &MvexAtom) -> Segment {

        let mut tracks = Vec::new();

        for traf in moof.trafs.iter_mut() {
            // TODO: Map to actual track index.
            let t = traf.tfhd.track_id as usize;

            let mut runs = Vec::new();
            let mut sizes = Vec::new();
            let mut durations = Vec::new();

            // If an explicit anchor-point is set, then use that for the position, otherwise use the
            // first-byte of the enclosing moof atom.
            let traf_base_pos = match traf.tfhd.base_data_offset {
                Some(pos) => pos,
                _ => moof.moof_base_pos,
            };

            let mut sample = 0;
            let mut traf_run_pos = traf_base_pos;

            for trun in traf.truns.iter_mut() {

                let base_pos = match trun.data_offset {
                    Some(offset) => {
                        // If a data offset is present, it is relative to the anchor-point defined
                        // above.
                        if offset.is_negative() {
                            traf_base_pos - u64::from(offset.wrapping_abs() as u32)
                        }
                        else {
                            traf_base_pos + offset as u64
                        }
                    }
                    _ => {
                        // If data offset is not provided, then this run starts immediately after
                        // the previous run.
                        traf_run_pos
                    }
                };

                // Add a new sample run.
                runs.push(SampleRun {
                    base_pos,
                    first_sample: sample,
                    last_sample: sample + trun.sample_count,
                });

                // Populate sample lengths.
                if trun.is_sample_duration_present() {
                    if trun.sample_count == trun.sample_duration.len() as u32 {
                        // TODO: BAD, INEFFICIENT.
                        let iter = trun.sample_duration.iter().map(|&sample_delta| SampleDurationEntry { sample_count: 1, sample_delta });
                        durations.extend(iter);
                    }
                    else {
                        todo!();
                    }
                }
                else {
                    let default_duration = match traf.tfhd.default_sample_duration {
                        Some(duration) => duration,
                        _ => mvex.trexs[t].default_sample_duration,
                    };

                    durations.push(SampleDurationEntry {
                        sample_count: trun.sample_count,
                        sample_delta: default_duration,
                    });
                }
                
                // Populate sample sizes.
                traf_run_pos += if trun.is_sample_size_present() {
                    // The track segment run provides explicitly recorded sample sizes.
                    if trun.sample_count == trun.sample_size.len() as u32 {
                        sizes.append(&mut trun.sample_size);
                    }
                    else {
                        todo!();
                    }

                    sizes.iter().rev().take(trun.sample_count as usize).map(|&s| u64::from(s)).sum()
                }
                else {
                    let default_size = match traf.tfhd.default_sample_size {
                        Some(size) => size,
                        _ => mvex.trexs[t].default_sample_size,
                    };
                    
                    sizes.extend(std::iter::repeat(default_size).take(trun.sample_count as usize));

                    u64::from(default_size) * u64::from(trun.sample_count)
                };

                sample += trun.sample_count;
            }

            let track = TrackSegment {
                runs,
                base_pts: 0,
                first_sample: 0,
                sample_sizes: SampleSize::Variable(sizes),
                sample_durations: durations,
            };

            tracks.push(track);
        }

        Segment {
            sequence: moof.mfhd.sequence_number,
            tracks,
        }
    }
}

#[derive(Debug)]
pub struct DeferredSegment {
    pub earliest_pts: u64, 
    pub segment_pos: u64,
    pub segment_size: u64,
}

impl DeferredSegment {
    /// Consumes the `DeferredSegment` and returns a `Segment`.
    pub fn load(self, mvex: &MvexAtom, reader: &mut MediaSourceStream) -> Result<Segment> {
        // If the stream is seekable, seek normally to the segment's position.
        if reader.is_seekable() {
            reader.seek(SeekFrom::Start(self.segment_pos))?;
        }
        else {
            // Stream is not seekable, seek within buffer.
            reader.seek_buffered(self.segment_pos);
        }
    
        // Iterate through the atoms in the segment and find the moof atom.
        let mut iter = AtomIterator::new_root(reader, Some(self.segment_size));

        while let Some(header) = iter.next()? {
            match header.atype {
                AtomType::MovieFragment => {
                    // Found the Movie Fragment (moof) atom, generate a segment.
                    let moof = iter.read_atom::<MoofAtom>()?;
                    let segment = Segment::from_moof(moof, mvex);
                    return Ok(segment);
                }
                _ => ()
            }
        }

        decode_error("segment reference did not contain moof")
    }
}