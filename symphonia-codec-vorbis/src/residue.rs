// Symphonia
// Copyright (c) 2019-2021 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::cmp::min;
use std::io;

use symphonia_core::errors::{Error, Result, decode_error};
use symphonia_core::io::{ReadBitsRtl, BitReaderRtl};

use super::codebook::VorbisCodebook;
use super::common::*;
use super::DspChannel;

#[derive(Debug, Default)]
struct ResidueVqClass {
    books: [u8; 8],
    is_used: u8,
}

impl ResidueVqClass {
    #[inline(always)]
    fn is_used(&self, pass: usize) -> bool {
        debug_assert!(pass < 8);
        self.is_used & (1 << pass) != 0
    }
}

#[derive(Debug)]
struct ResidueSetup {
    /// The residue format.
    residue_type: u16,
    /// The residue's starting offset.
    residue_begin: u32,
    /// The residue's ending offset.
    residue_end: u32,
    /// Residue partition size (max. value 2^24).
    residue_partition_size: u32,
    /// Residue classifications (max. value 64).
    residue_classifications: u8,
    /// Codebook for reading partition classifications.
    residue_classbook: u8,
    /// Codebooks for each partition classification.
    residue_vq_class: Vec<ResidueVqClass>,
}

/// `ResidueScratch` is a working-area that may be reused by many `Residue`s to reduce overall
/// memory consumption.
#[derive(Default)]
pub struct ResidueScratch {
    /// Classifications vector.
    part_classes: Vec<u8>,
    /// Vector to read interleaved format 2 residuals.
    buf: Vec<f32>,
}

impl ResidueScratch {
    /// Ensures the scratch pad has enough storage for `len` partition classes.
    #[inline(always)]
    fn reserve_part_classes(&mut self, len: usize) {
        if self.part_classes.len() < len {
            self.part_classes.resize(len, Default::default());
        }
    }

    /// Ensures the scratch buffer can accomodate `len`.
    #[inline(always)]
    fn reserve_buf(&mut self, len: usize) {
        if self.buf.len() < len {
            self.buf.resize(len, Default::default());
        }
    }
}

#[derive(Debug)]
pub struct Residue {
    setup: ResidueSetup,
}

impl Residue {

    pub fn try_read(
        bs: &mut BitReaderRtl<'_>,
        residue_type: u16,
        max_codebook: u8
    ) -> Result<Self> {
        let setup = Self::read_setup(bs, residue_type, max_codebook)?;

        Ok(Residue { setup })
    }

    fn read_setup(
        bs: &mut BitReaderRtl<'_>,
        residue_type: u16,
        max_codebook: u8
    ) -> Result<ResidueSetup> {
        let residue_begin = bs.read_bits_leq32(24)?;
        let residue_end = bs.read_bits_leq32(24)?;
        let residue_partition_size = bs.read_bits_leq32(24)? + 1;
        let residue_classifications = bs.read_bits_leq32(6)? as u8 + 1;
        let residue_classbook = bs.read_bits_leq32(8)? as u8;

        if residue_end < residue_begin {
            return decode_error("vorbis: invalid residue begin and end");
        }

        let mut residue_vq_books = Vec::<ResidueVqClass>::new();

        for _ in 0..residue_classifications {
            let low_bits = bs.read_bits_leq32(3)? as u8;

            let high_bits = if bs.read_bool()? {
                bs.read_bits_leq32(5)? as u8
            }
            else {
                0
            };

            let is_used = (high_bits << 3) | low_bits;

            residue_vq_books.push(ResidueVqClass {
                is_used,
                books: [0; 8],
            });
        }

        for vq_books in &mut residue_vq_books {
            // For each set of residue codebooks, if the codebook is used, read the codebook
            // number.
            for (j, book) in vq_books.books.iter_mut().enumerate() {
                // Is a codebook used?
                let is_codebook_used = vq_books.is_used & (1 << j) != 0;

                if is_codebook_used {
                    // Read the codebook number.
                    *book = bs.read_bits_leq32(8)? as u8;

                    // The codebook number cannot be 0 or exceed the number of codebooks in this
                    // stream.
                    if *book == 0 || *book >= max_codebook {
                        return decode_error("vorbis: invalid codebook for residue");
                    }
                }
            }

        }

        let residue = ResidueSetup {
            residue_type,
            residue_begin,
            residue_end,
            residue_partition_size,
            residue_classifications,
            residue_classbook,
            residue_vq_class: residue_vq_books,
        };

        Ok(residue)
    }

    pub fn read_residue(
        &mut self,
        bs: &mut BitReaderRtl<'_>,
        bs_exp: u8,
        codebooks: &[VorbisCodebook],
        residue_channels: &BitSet256,
        scratch: &mut ResidueScratch,
        channels: &mut [DspChannel],
    ) -> Result<()> {

        // Read the residue, and ignore end-of-bitstream errors which are legal.
        match self.read_residue_inner(bs, bs_exp, codebooks, residue_channels, scratch, channels) {
            Ok(_) => (),
            // An end-of-bitstream error is classified under ErrorKind::Other. This condition
            // should not be treated as an error.
            Err(Error::IoError(ref e)) if e.kind() == io::ErrorKind::Other => (),
            Err(e) => return Err(e),
        };

        if self.setup.residue_type == 2 {
            // For format 2, the residue vectors for all channels are interleaved together into one
            // large vector. This vector is in the scratch-pad buffer and can now be de-interleaved
            // into the channel buffers.
            let stride = residue_channels.count() as usize;

            for (i, channel_idx) in residue_channels.iter().enumerate() {
                let channel = &mut channels[channel_idx];

                let iter = scratch.buf.chunks_exact(stride).map(|c| c[i]);

                for (o, i) in channel.residue.iter_mut().zip(iter) { *o = i; }
            }
        }

        Ok(())
    }

    fn read_residue_inner(
        &mut self,
        bs: &mut BitReaderRtl<'_>,
        bs_exp: u8,
        codebooks: &[VorbisCodebook],
        residue_channels: &BitSet256,
        scratch: &mut ResidueScratch,
        channels: &mut [DspChannel],
    ) -> Result<()> {

        let class_book = &codebooks[self.setup.residue_classbook as usize];

        // The actual length of the entire residue vector for a channel (formats 0 and 1), or all
        // interleaved channels (format 2).
        let actual_size = match self.setup.residue_type {
            2 => ((1 << bs_exp) >> 1) * residue_channels.count() as usize,
            _ => (1 << bs_exp) >> 1,
        };

        // The range of the residue vector being encoded.
        let limit_residue_begin = min(self.setup.residue_begin as usize, actual_size);
        let limit_residue_end = min(self.setup.residue_end as usize, actual_size);

        // Length of the coded (non-zero) part of the residue vector.
        let residue_len = limit_residue_end - limit_residue_begin;

        // Partitions per classword.
        let parts_per_classword = class_book.dimensions();

        // Partitions to read.
        let parts_to_read = residue_len / self.setup.residue_partition_size as usize;

        let is_fmt2 = self.setup.residue_type == 2;

        // Setup the scratch-pad.
        if is_fmt2 {
            // Reserve partition classification space in the scratch-pad.
            scratch.reserve_part_classes(parts_to_read);

            // Reserve interleave buffer storage in the scratch-pad.
            scratch.reserve_buf(actual_size);

            // Zero the interleaving buffer.
            scratch.buf[..actual_size].fill(0.0);
        }
        else {
            scratch.reserve_part_classes(parts_to_read * residue_channels.count() as usize);
        }

        let mut has_channel_to_decode = false;

        // Zero unused residue channels.
        for j in residue_channels.iter() {
            let ch = &mut channels[j];

            // Zero the channel residue if not type 2.
            if !is_fmt2 {
                ch.residue[..actual_size].fill(0.0);
            }

            if !ch.do_not_decode {
                has_channel_to_decode = true;
            }
        }

        // If all channels are marked do-not-decode then immediately exit.
        if !has_channel_to_decode {
            return Ok(());
        }

        // Residues may be encoded in up-to 8 passes. Fewer passes may be encoded by prematurely
        // "ending" the packet. This means that an end-of-bitstream error is actually NOT an error.
        for pass in 0..8 {
            // The number of partitions that can be read at once is limited by the number of
            // partitions per classword. Therefore, read partitions in batches of size
            // parts_per_classword.
            for p_start in (0..parts_to_read).step_by(parts_per_classword as usize) {
                // The classifications for each partition are only encoded in the first pass.
                // Ultimately, this encoding strategy is what forces us to process in batches.
                if pass == 0 {
                    // If using format 2, there is only a single classification list.
                    if is_fmt2 {
                        let code = class_book.read_scalar(bs)?.0;

                        decode_classes(
                            code,
                            parts_per_classword,
                            self.setup.residue_classifications as u32,
                            &mut scratch.part_classes[p_start..],
                        );
                    }
                    else {
                        // For formats 0 and 1, each channel has its own classification list.
                        for (i, channel_idx) in residue_channels.iter().enumerate() {
                            let ch = &channels[channel_idx];

                            // If the channel is marked do-not-decode then advance to the next
                            // channel.
                            if ch.do_not_decode {
                                continue;
                            }

                            let code = class_book.read_scalar(bs)?.0;

                            decode_classes(
                                code,
                                parts_per_classword,
                                self.setup.residue_classifications as u32,
                                &mut scratch.part_classes[p_start + i * parts_to_read as usize..],
                            );
                        }
                    }
                }

                // The last partition in this batch of partitions, being careful not to exceed the
                // total number of partitions.
                let p_end = min(parts_to_read, p_start + parts_per_classword as usize);

                // Read each partitions for all the channels that are part of this residue.
                for p in p_start..p_end {

                    for (i, channel_idx) in residue_channels.iter().enumerate() {
                        let ch = &mut channels[channel_idx];

                        let vq_class = if !is_fmt2 {
                            // If the channel is marked do-no-decode, then advance to the next
                            // channels.
                            if ch.do_not_decode {
                                continue;
                            }

                            let class_idx = scratch.part_classes[p + parts_to_read * i] as usize;
                            &self.setup.residue_vq_class[class_idx]
                        }
                        else {
                            &self.setup.residue_vq_class[scratch.part_classes[p] as usize]
                        };

                        if vq_class.is_used(pass) {
                            let vq_book = &codebooks[vq_class.books[pass] as usize];

                            let part_size = self.setup.residue_partition_size as usize;
                            let offset = limit_residue_begin as usize + part_size * p;

                            match self.setup.residue_type {
                                0 => {
                                    read_residue_partition_format0(
                                        bs,
                                        vq_book,
                                        &mut ch.residue[offset..offset + part_size]
                                    )
                                }
                                1 => {
                                    read_residue_partition_format1(
                                        bs,
                                        vq_book,
                                        &mut ch.residue[offset..offset + part_size]
                                    )
                                }
                                2 => {
                                    // Residue type 2 is implemented in term of type 1.
                                    read_residue_partition_format1(
                                        bs,
                                        vq_book,
                                        &mut scratch.buf[offset..offset + part_size]
                                    )
                                }
                                _ => unreachable!(),
                            }?;
                        }

                        if is_fmt2 {
                            break;
                        }
                    }
                }
                // End of partition batch iteration.
            }
            // End of pass iteration.
        }

        Ok(())
    }
}

fn decode_classes(mut val: u32, class_words: u16, classifications: u32, out: &mut [u8]) {
    for (_, out) in (0..class_words as usize).zip(out).rev() {
        *out = (val % classifications) as u8;
        val /= classifications;
    }
}

fn read_residue_partition_format0(
    bs: &mut BitReaderRtl<'_>,
    codebook: &VorbisCodebook,
    out: &mut [f32],
) -> Result<()> {

    let step = out.len() / codebook.dimensions() as usize;

    for i in 0..step {
        let vq = codebook.read_vq(bs)?;

        for (o, &v) in out[i..].iter_mut().step_by(step).zip(vq) {
            *o += v;
        }
    }

    Ok(())
}

#[inline(always)]
fn read_residue_partition_format1(
    bs: &mut BitReaderRtl<'_>,
    codebook: &VorbisCodebook,
    out: &mut [f32],
) -> Result<()> {

    let dimensions = codebook.dimensions() as usize;

    for out in out.chunks_exact_mut(dimensions) {
        let vq = codebook.read_vq(bs)?;

        for (o, &v) in out.iter_mut().zip(vq) {
            *o += v;
        }
    }

    Ok(())
}
