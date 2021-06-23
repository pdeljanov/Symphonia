// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
use symphonia_core::errors::{Error, Result, decode_error};

use crate::atoms::{MoofAtom, MoovAtom, StcoAtom, Co64Atom, MvexAtom, stsz::SampleSize};

use std::sync::Arc;

pub struct SampleDataDesc {
    pub base_pos: u64,
    pub offset: Option<u64>,
    pub size: u32,
}

pub struct SampleTiming {
    pub ts: u64,
    pub dur: u32,
}

pub trait StreamSegment: Send {
    /// Gets the sequence number of this segment.
    fn sequence_num(&self) -> u32;

    /// Gets the first and last sample numbers for the track `track_num`.
    fn track_sample_range(&self, track_num: u32) -> (u32, u32);

    /// Gets the first and last sample timestamps for the track `track_num`.
    fn track_ts_range(&self, track_num: u32) -> (u64, u64);

    /// Get the timestamp and duration for the sample indicated by `sample_num` for the track
    /// `track_num`.
    fn sample_timing(&self, track_num: u32, sample_num: u32) -> Result<Option<SampleTiming>>;

    /// Get the sample number of the sample containing the timestamp indicated by `ts` for track
    // `track_num`.
    fn ts_sample(&self, track_num: u32, ts: u64) -> Result<Option<u32>>;

    /// Get the byte position and length of the sample indicated by `sample_num` for track
    /// `track_num`.
    fn sample_data(&self, track_num: u32, sample_num: u32, get_offset: bool) -> Result<SampleDataDesc>;
}

/// Track-to-stream sequencing information.
#[derive(Debug)]
struct SequenceInfo {
    /// The sample number of the first sample of a track in a fragment.
    first_sample: u32,
    /// The timestamp of the first sample of a track in a fragment.
    first_ts: u64,
    /// The total duration of all samples of a track in a fragment.
    total_sample_duration: u64,
}

pub struct MoofSegment {
    moof: MoofAtom,
    mvex: Arc<MvexAtom>,
    seq: Vec<SequenceInfo>,
}

impl MoofSegment {
    /// Instantiate a new segment from a `MoofAtom`.
    pub fn new(moof: MoofAtom, mvex: Arc<MvexAtom>, last: &Box<dyn StreamSegment>) -> MoofSegment {
        let mut seq = Vec::new();

        // Calculate the sequence information for each track of this segment.
        for (track_num, traf) in moof.trafs.iter().enumerate() {
            // Calculate the total duration of all runs in the fragment for the track.
            let mut total_sample_duration = 0;

            for trun in traf.truns.iter() {
                total_sample_duration += if trun.is_sample_duration_present() {
                    trun.total_sample_duration
                }
                else {
                    let duration = traf.tfhd.default_sample_duration.unwrap_or(
                        mvex.trexs[track_num].default_sample_duration);

                    u64::from(trun.sample_count) * u64::from(duration)
                }
            }

            let (_, first_sample) = last.track_sample_range(track_num as u32);
            let (_, first_ts) = last.track_ts_range(track_num as u32);

            seq.push(SequenceInfo { first_sample, first_ts, total_sample_duration });
        }

        MoofSegment { moof, mvex, seq }
    }
}

impl StreamSegment for MoofSegment {
    fn sequence_num(&self) -> u32 {
        self.moof.mfhd.sequence_number
    }

    fn sample_timing(&self, track_num: u32, sample_num: u32) -> Result<Option<SampleTiming>> {
        // Get the track fragment associated with track_num.
        let traf = self.moof.trafs.get(track_num as usize)
            .ok_or(Error::DecodeError("invalid track index"))?;

        let mut sample_num_rel = sample_num - self.seq[track_num as usize].first_sample;
        let mut trun_ts_offset = self.seq[track_num as usize].first_ts;

        for trun in traf.truns.iter() {
            // If the sample is contained within the this track fragment run, calculate and return
            // the exact sample timestamp.
            if sample_num_rel < trun.sample_count {

                let timing = if trun.is_sample_duration_present() {
                    // The size of the entire track fragment run is known.
                    let ts = trun.sample_duration[..1 + sample_num_rel as usize].iter()
                                                                       .map(|&s| u64::from(s))
                                                                       .sum::<u64>();

                    let dur = trun.sample_duration[sample_num_rel as usize];

                    SampleTiming { ts: trun_ts_offset + ts, dur }
                }
                else {
                    let dur = traf.tfhd.default_sample_duration.unwrap_or(
                        self.mvex.trexs[track_num as usize].default_sample_duration);

                    let ts = u64::from(sample_num_rel) * u64::from(dur);

                    SampleTiming { ts: trun_ts_offset + ts, dur }
                };

                return Ok(Some(timing));
            }

            let trun_duration = if trun.is_sample_duration_present() {
                // The size of the entire track fragment run is known.
                trun.total_sample_duration
            }
            else {
                let duration = traf.tfhd.default_sample_duration.unwrap_or(
                    self.mvex.trexs[track_num as usize].default_sample_duration);

                u64::from(trun.sample_count) * u64::from(duration)
            };

            sample_num_rel -= trun.sample_count;
            trun_ts_offset += trun_duration;
        }

        Ok(None)
    }

    fn ts_sample(&self, track_num: u32, ts: u64) -> Result<Option<u32>> {
        // Get the track fragment associated with track_num.
        let traf = self.moof.trafs.get(track_num as usize)
            .ok_or(Error::DecodeError("invalid track index"))?;

        let mut sample_num = self.seq[track_num as usize].first_sample;
        let mut ts_accum = self.seq[track_num as usize].first_ts;

        for trun in traf.truns.iter() {
            // Get the total duration of this track run.
            let trun_duration = if trun.is_sample_duration_present() {
                trun.total_sample_duration
            }
            else {
                let duration = traf.tfhd.default_sample_duration.unwrap_or(
                    self.mvex.trexs[track_num as usize].default_sample_duration);

                u64::from(trun.sample_count) * u64::from(duration)
            };

            if ts_accum + trun_duration > ts {
                // If the sample durations are present, then each sample duration is independently
                // stored. Sum sample durations until the delta is reached.
                if trun.is_sample_duration_present() {
                    let mut ts_delta = ts - ts_accum;

                    for &duration in &trun.sample_duration {
                        if u64::from(duration) > ts_delta {
                            break;
                        }

                        ts_delta -= u64::from(duration);
                        sample_num += 1;
                    }
                }
                else {
                    // If the sample durations are not present, then get the sample duration from
                    // the track fragment header or track extends atom. Then, calculate the number
                    // of samples are needed to reach the desired timestamp.
                    let duration = traf.tfhd.default_sample_duration.unwrap_or(
                        self.mvex.trexs[track_num as usize].default_sample_duration);

                    sample_num += ((ts - ts_accum) / u64::from(duration)) as u32;
                }

                return Ok(Some(sample_num));
            }

            sample_num += trun.sample_count;
            ts_accum += trun_duration;
        }

        Ok(None)
    }

    fn sample_data(&self, track_num: u32, sample_num: u32, get_offset: bool) -> Result<SampleDataDesc> {
        // Get the track fragment associated with track_num.
        let traf = self.moof.trafs.get(track_num as usize)
                                  .ok_or(Error::DecodeError("invalid track index"))?;

        // If an explicit anchor-point is set, then use that for the position, otherwise use the
        // first-byte of the enclosing moof atom.
        let traf_base_pos = match traf.tfhd.base_data_offset {
            Some(pos) => pos,
            _ => self.moof.moof_base_pos,
        };

        let mut sample_num_rel = sample_num - self.seq[track_num as usize].first_sample;
        let mut trun_offset = traf_base_pos;

        for trun in traf.truns.iter() {
            // If a data offset is present for this track fragment run, then calculate the new base
            // position for the run. When a data offset is not present, do nothing because this run
            // follows the previous run.
            if let Some(offset) = trun.data_offset {
                // The offset for the run is relative to the anchor-point defined in the track
                // fragment header.
                trun_offset = if offset.is_negative() {
                    traf_base_pos - u64::from(offset.wrapping_abs() as u32)
                }
                else {
                    traf_base_pos + offset as u64
                };
            }

            if sample_num_rel < trun.sample_count {
                // Get the size of the sample.
                let size = if trun.is_sample_size_present() {
                    trun.sample_size[sample_num_rel as usize]
                }
                else {
                    traf.tfhd.default_sample_size.unwrap_or(
                        self.mvex.trexs[track_num as usize].default_sample_size)
                };

                let offset = if get_offset {
                    if trun.is_sample_size_present() {
                        let sample_sizes = &trun.sample_size[..sample_num_rel as usize];
                        Some(sample_sizes.iter().map(|&s| u64::from(s)).sum::<u64>())
                    }
                    else {
                        Some(u64::from(sample_num_rel) * u64::from(size))
                    }
                }
                else {
                    None
                };

                return Ok(SampleDataDesc { base_pos: trun_offset, size, offset });
            }

            // Get or calculate the total size of the track fragment run.
            let trun_size = if trun.is_sample_size_present() {
                // The size of the entire track fragment run is known.
                u64::from(trun.total_sample_size)
            }
            else {
                let size = traf.tfhd.default_sample_size.unwrap_or(
                    self.mvex.trexs[track_num as usize].default_sample_size);

                u64::from(trun.sample_count) * u64::from(size)
            };

            sample_num_rel -= trun.sample_count;
            trun_offset += trun_size;
        }

        decode_error("invalid sample index")
    }

    fn track_sample_range(&self, track_num: u32) -> (u32, u32) {
        let first = self.seq[track_num as usize].first_sample;
        (first, first + self.moof.trafs[track_num as usize].total_sample_count)
    }

    fn track_ts_range(&self, track_num: u32) -> (u64, u64) {
        let first = self.seq[track_num as usize].first_ts;
        (first, first + self.seq[track_num as usize].total_sample_duration)
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
        // This should never happen because it is mandatory to have either a stco or co64 atom.
        decode_error("missing stco or co64 atom")
    }
}

pub struct MoovSegment {
    moov: MoovAtom,
}

impl MoovSegment {
    /// Instantiate a segment from the provide moov atom.
    pub fn new(moov: MoovAtom) -> MoovSegment {
        MoovSegment { moov }
    }
}

impl StreamSegment for MoovSegment {
    fn sequence_num(&self) -> u32 {
        // The segment defined by the moov atom is always 0.
        0
    }

    fn sample_timing(&self, track_num: u32, sample_num: u32) -> Result<Option<SampleTiming>> {
        // Get the trak atom associated with track_num.
        let trak = self.moov.traks.get(track_num as usize)
                                  .ok_or(Error::DecodeError("invalid track index"))?;

        // Find the sample timing. Note, complexity of O(N).
        let timing = trak.mdia.minf.stbl.stts.find_timing_for_sample(sample_num);

        if let Some((ts, dur)) = timing {
            Ok(Some(SampleTiming { ts, dur }))
        }
        else {
            Ok(None)
        }
    }

    fn ts_sample(&self, track_num: u32, ts: u64) -> Result<Option<u32>> {
        // Get the trak atom associated with track_num.
        let trak = self.moov.traks.get(track_num as usize)
                                  .ok_or(Error::DecodeError("invalid track index"))?;

        // Find the sample timestamp. Note, complexity of O(N).
        Ok(trak.mdia.minf.stbl.stts.find_sample_for_timestamp(ts))
    }

    fn sample_data(&self, track_num: u32, sample_num: u32, get_offset: bool) -> Result<SampleDataDesc> {
        // Get the trak atom associated with track_num.
        let trak = self.moov.traks.get(track_num as usize)
                                  .ok_or(Error::DecodeError("invalid trak index"))?;

        // Get the constituent tables.
        let stsz = &trak.mdia.minf.stbl.stsz;
        let stsc = &trak.mdia.minf.stbl.stsc;
        let stco = &trak.mdia.minf.stbl.stco;
        let co64 = &trak.mdia.minf.stbl.co64;

        // Find the sample-to-chunk mapping. Note, complexity of O(log N).
        let group = stsc.find_entry_for_sample(sample_num)
                        .ok_or(Error::DecodeError("invalid sample index"))?;

        // Index of the sample relative to the chunk group.
        let sample_in_group = sample_num - group.first_sample;

        // Index of the chunk containing the sample relative to the chunk group.
        let chunk_in_group = sample_in_group / group.samples_per_chunk;

        // Index of the chunk containing the sample relative to the entire stream.
        let chunk_in_stream = group.first_chunk + chunk_in_group;

        // Get the byte position of the first sample of the chunk containing the sample.
        let base_pos = get_chunk_offset(&stco, &co64, chunk_in_stream as usize)?.unwrap();

        // Determine the absolute sample byte position if requested by calculating the offset of
        // the sample from the base position of the chunk.
        let offset = if get_offset {
            // Index of the sample relative to the chunk containing the sample.
            let sample_in_chunk = sample_in_group - (chunk_in_group * group.samples_per_chunk);

            // Calculat the byte offset of the sample relative to the chunk containing it.
            let offset = match stsz.sample_sizes {
                SampleSize::Constant(size) => {
                    // Constant size samples can be calculated directly.
                    u64::from(sample_in_chunk) * u64::from(size)
                }
                SampleSize::Variable(ref entries) => {
                    // For variable size samples, sum the sizes of all the samples preceeding the
                    // desired sample in the chunk.
                    let chunk_first_sample = (sample_num - sample_in_chunk) as usize;

                    if let Some(samples) = entries.get(chunk_first_sample..sample_num as usize) {
                        samples.iter().map(|&size| u64::from(size)).sum()
                    }
                    else {
                        return decode_error("missing one or more stsz entries");
                    }
                }
            };

            Some(offset)
        }
        else {
            None
        };

        // Get the size in bytes of the sample.
        let size = match stsz.sample_sizes {
            SampleSize::Constant(size) => size,
            SampleSize::Variable(ref entries) => {
                if let Some(size) = entries.get(sample_num as usize) {
                    *size
                }
                else {
                    return decode_error("missing stsz entry");
                }
            }
        };

        Ok(SampleDataDesc { base_pos, size, offset })
    }

    fn track_sample_range(&self, track_num: u32) -> (u32, u32) {
        (0, self.moov.traks[track_num as usize].mdia.minf.stbl.stsz.sample_count)
    }

    fn track_ts_range(&self, track_num: u32) -> (u64, u64) {
        (0, self.moov.traks[track_num as usize].mdia.minf.stbl.stts.total_duration)
    }
}