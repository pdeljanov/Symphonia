// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::ReadBytes;

use crate::atoms::{Atom, AtomHeader, Co64Atom, StcoAtom};

#[derive(Debug)]
pub struct StscEntry {
    pub first_chunk: u32,
    pub first_sample: u32,
    pub samples_per_chunk: u32,
    #[allow(dead_code)]
    pub sample_desc_index: u32,
}

/// Sample to Chunk Atom
#[allow(dead_code)]
#[derive(Debug)]
pub struct StscAtom {
    /// Entries.
    pub entries: Vec<StscEntry>,
}

impl StscAtom {
    /// Finds the `StscEntry` for the sample indicated by `sample_num`. Note, `sample_num` is indexed
    /// relative to the `StscAtom`. Complexity is O(log2 N).
    pub fn find_entry_for_sample(&self, sample_num: u32) -> Option<&StscEntry> {
        let mut left = 1;
        let mut right = self.entries.len();

        while left < right {
            let mid = left + (right - left) / 2;

            let entry = self.entries.get(mid).unwrap();

            if entry.first_sample < sample_num {
                left = mid + 1;
            }
            else {
                right = mid;
            }
        }

        // The index found above (left) is the exclusive upper bound of all entries where
        // first_sample < sample_num. Therefore, the entry to return has an index of left-1. The
        // index will never equal 0 so this is safe. If the table were empty, left == 1, thus calling
        // get with an index of 0, and safely returning None.
        self.entries.get(left - 1)
    }

    pub fn post_processing(
        &mut self,
        stco: &Option<StcoAtom>,
        co64: &Option<Co64Atom>,
    ) -> Result<()> {
        // Post-process entries to check for errors and calculate the file sample.
        if !self.entries.is_empty() {
            // stco and co64 can both be absent, this is a spec. violation, but some m4a files appear to lack these atoms.
            // set maximum possible total_chunks in this case
            let mut total_chunks = u32::MAX;

            if let Some(stco) = stco {
                total_chunks = stco.chunk_offsets.len() as u32;
            }
            else if let Some(co64) = co64 {
                total_chunks = co64.chunk_offsets.len() as u32;
            }

            for i in 0..self.entries.len() - 1 {
                // Validate that first_chunk is within the total number of chunks
                // first_chunk index was transformed to be starting from 0
                if self.entries[i].first_chunk >= total_chunks
                    || self.entries[i + 1].first_chunk >= total_chunks
                {
                    return decode_error(
                        "isomp4 (stsc): entry's first chunk is bigger than total chunk number",
                    );
                }

                // Validate that first_chunk is monotonic across all entries.
                if self.entries[i + 1].first_chunk <= self.entries[i].first_chunk {
                    return decode_error("isomp4 (stsc): entry's first chunk not monotonic");
                }

                let n = self.entries[i + 1].first_chunk - self.entries[i].first_chunk;

                self.entries[i + 1].first_sample =
                    self.entries[i].first_sample + (n * self.entries[i].samples_per_chunk);
            }
        }

        Ok(())
    }
}

impl Atom for StscAtom {
    fn read<B: ReadBytes>(reader: &mut B, mut header: AtomHeader) -> Result<Self> {
        let (_, _) = header.read_extended_header(reader)?;

        // minimum data size is 4 bytes
        let len = match header.data_len() {
            Some(len) if len >= 4 => len as u32,
            Some(_) => return decode_error("isomp4 (stsc): atom size is less than 16 bytes"),
            None => return decode_error("isomp4 (stsc): expected atom size to be known"),
        };

        let entry_count = reader.read_be_u32()?;
        if entry_count != (len - 4) / 12 {
            return decode_error("isomp4 (stsc): invalid entry count");
        }

        // TODO: Apply a limit.
        let mut entries = Vec::with_capacity(entry_count as usize);

        for _ in 0..entry_count {
            let first_chunk = reader.read_be_u32()?;

            // Validate that first chunk is indexed from 1
            if first_chunk == 0 {
                return decode_error("isomp4 (stsc):  entry's first chunk undex should be from 1");
            }

            let samples_per_chunk = reader.read_be_u32()?;

            // Validate that samples per chunk is > 0. Could the entry be ignored?
            if samples_per_chunk == 0 {
                return decode_error("isomp4 (stsc): entry has 0 samples per chunk");
            }

            entries.push(StscEntry {
                first_chunk: first_chunk - 1,
                first_sample: 0,
                samples_per_chunk,
                sample_desc_index: reader.read_be_u32()?,
            });
        }

        Ok(StscAtom { entries })
    }
}
