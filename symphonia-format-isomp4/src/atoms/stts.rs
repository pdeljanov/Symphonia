// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::atoms::limits::*;
use crate::atoms::{Atom, AtomHeader, AtomIterator, ReadAtom, Result, decode_error};

#[derive(Debug)]
pub struct SampleDurationEntry {
    pub sample_count: u32,
    pub sample_delta: u32,
}

/// Time-to-sample atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct SttsAtom {
    pub entries: Vec<SampleDurationEntry>,
    pub total_duration: u64,
}

impl SttsAtom {
    /// Get the timestamp and duration for the sample indicated by `sample_num`. Note, `sample_num`
    /// is indexed relative to the `SttsAtom`. Complexity of this function in O(N).
    pub fn find_timing_for_sample(&self, sample_num: u32) -> Option<(u64, u32)> {
        let mut ts = 0;
        let mut next_entry_first_sample: u64 = 0;

        // The Stts atom compactly encodes a mapping between number of samples and sample duration.
        // Iterate through each entry until the entry containing the next sample is found. The next
        // packet timestamp is then the sum of the product of sample count and sample duration for
        // the n-1 iterated entries, plus the product of the number of consumed samples in the n-th
        // iterated entry and sample duration.
        for entry in &self.entries {
            let entry_first_sample = next_entry_first_sample;
            next_entry_first_sample += u64::from(entry.sample_count);

            if u64::from(sample_num) < next_entry_first_sample {
                let entry_sample_offset = u64::from(sample_num) - entry_first_sample;
                ts += u64::from(entry.sample_delta) * entry_sample_offset;

                return Some((ts, entry.sample_delta));
            }

            ts += u64::from(entry.sample_count) * u64::from(entry.sample_delta);
        }

        None
    }

    /// Get the sample that contains the timestamp indicated by `ts`. Note, the returned `sample_num`
    /// is indexed relative to the `SttsAtom`. Complexity of this function in O(N).
    pub fn find_sample_for_timestamp(&self, ts: u64) -> Option<u32> {
        let mut ts_accum = 0;
        let mut sample_num: u64 = 0;

        for entry in &self.entries {
            let delta = u64::from(entry.sample_delta) * u64::from(entry.sample_count);

            if ts_accum + delta > ts {
                sample_num += (ts - ts_accum) / u64::from(entry.sample_delta);
                return u32::try_from(sample_num).ok();
            }

            ts_accum += delta;
            sample_num += u64::from(entry.sample_count);
        }

        None
    }
}

impl Atom for SttsAtom {
    fn read<R: ReadAtom>(it: &mut AtomIterator<R>, _header: &AtomHeader) -> Result<Self> {
        let (_, _) = it.read_extended_header()?;

        let entry_count = it.read_u32()?;

        // Limit the maximum initial capacity to prevent malicious files from using all the
        // available memory.
        let mut entries = Vec::with_capacity(MAX_TABLE_INITIAL_CAPACITY.min(entry_count as usize));

        let mut total_duration: u64 = 0;

        for _ in 0..entry_count {
            let sample_count = it.read_u32()?;
            let sample_delta = it.read_u32()?;

            let Some(next_duration) =
                total_duration.checked_add(u64::from(sample_count) * u64::from(sample_delta))
            else {
                return decode_error("isomp4 (stts): total duration overflow");
            };
            total_duration = next_duration;

            entries.push(SampleDurationEntry { sample_count, sample_delta });
        }

        Ok(SttsAtom { entries, total_duration })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stts(entries: &[(u32, u32)]) -> SttsAtom {
        SttsAtom {
            entries: entries
                .iter()
                .map(|&(sample_count, sample_delta)| SampleDurationEntry {
                    sample_count,
                    sample_delta,
                })
                .collect(),
            total_duration: 0,
        }
    }

    #[test]
    fn find_timing_for_sample_does_not_overflow() {
        let atom = stts(&[(1, 1024), (u32::MAX, 1024)]);
        assert_eq!(atom.find_timing_for_sample(0), Some((0, 1024)));
        assert_eq!(atom.find_timing_for_sample(1), Some((1024, 1024)));
        assert_eq!(atom.find_timing_for_sample(u32::MAX), Some((u64::from(u32::MAX) * 1024, 1024)));

        let atom = stts(&[(u32::MAX, 1024)]);
        assert_eq!(
            atom.find_timing_for_sample(u32::MAX - 1),
            Some((u64::from(u32::MAX - 1) * 1024, 1024))
        );
        assert_eq!(atom.find_timing_for_sample(u32::MAX), None);
    }

    #[test]
    fn find_sample_for_timestamp_does_not_overflow() {
        let atom = stts(&[(u32::MAX, 1), (u32::MAX, 1)]);
        assert_eq!(atom.find_sample_for_timestamp(10), Some(10));
        assert_eq!(atom.find_sample_for_timestamp(u64::from(u32::MAX)), Some(u32::MAX));
        assert_eq!(atom.find_sample_for_timestamp(u64::from(u32::MAX) + 5), None);
    }
}
