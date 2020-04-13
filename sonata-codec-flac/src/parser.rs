// Sonata
// Copyright (c) 2020 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use sonata_core::checksum::Crc16;
use sonata_core::errors::Result;
use sonata_core::io::{BufStream, ByteStream, Monitor};
use sonata_core::util::bits;

use super::metadata::StreamInfo;
use super::frame::{BlockSequence, FrameHeader, is_likely_frame_header, read_frame_header};

use std::cmp::min;
use std::collections::VecDeque;

#[inline(always)]
fn round_pow2(value: usize, pow2: usize) -> usize {
    (value + (pow2 - 1)) & !(pow2 - 1)
}

struct Fragment {
    pos: usize,
    header: FrameHeader,
}

pub struct PacketParser {
    stream_info: StreamInfo,
    last_seq: u64,

    buf: Vec<u8>,
    block_write: usize,

    fragments: VecDeque<Fragment>,
    fragment_read: usize,

    frame_size_hist: [u32; 4],
    n_frames: usize,
}

impl Default for PacketParser {
    fn default() -> Self {
        PacketParser {
            buf: vec![0; (2 * PacketParser::BLOCK_LEN) + 8],
            block_write: 0,
            fragments: Default::default(),
            fragment_read: 0,
            frame_size_hist: [PacketParser::FLAC_AVG_FRAME_LEN; 4],
            n_frames: 0,
            stream_info: Default::default(),
            last_seq: 0,
        }
    }
}

impl PacketParser {
    /// The size of the largest possible valid FLAC frame header.
    const FLAC_MAX_FRAME_HEADER_LEN: usize = 16;
    /// The size of the largest possible complete and valid FLAC frame.
    const FLAC_MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

    /// The average FLAC frame length.
    const FLAC_AVG_FRAME_LEN: u32 = 8 * 1024;

    const BLOCK_LEN: usize = 4 * 1024;
    const MAX_BUFFER_LEN: usize = PacketParser::FLAC_MAX_FRAME_LEN + PacketParser::BLOCK_LEN;

    // Frames:    [                           F0                          |   ..   ]
    //                                                                    :
    // Fragments: [  f0   |  f1   |      f2     |    f3    |    f4   | f5 |   ..   ]
    //                    :       :             :          :         :    :
    // Blocks:    [           b0            |     b1     |     b2     |     b3     ]
    //
    // Frames are complete FLAC frames. Frames are composed of an integer number of fragments.
    // Fragments are variable size, but always start with the frame header preamble.
    // Blocks are variable size, generally equal to the average frame size rounded up to the next
    // multiple of 4kB.
    //
    // Blocks are read from the media source stream. Once buffered, the block is scanned for
    // fragments. Fragments shall start with the frame header preamble that decodes to a valid
    // FLAC header. Fragments are then be combined until a valid FLAC frame is found. A set of
    // rules and heuristics are used to limit the recombination process to handle correupt frames.

    pub fn reset(&mut self, stream_info: StreamInfo) {
        self.stream_info = stream_info;
        self.frame_size_hist = [PacketParser::FLAC_AVG_FRAME_LEN; 4];
        self.n_frames = 0;
        self.last_seq = 0;
    }

    fn fetch_block<B: ByteStream>(&mut self, reader: &mut B) -> Result<()> {
        // Calculate the average frame size.
        let avg_frame_size = (self.frame_size_hist[0]
                                + self.frame_size_hist[1]
                                + self.frame_size_hist[2]
                                + self.frame_size_hist[3]) / 4;

        // Round up to the nearest block size multiple.
        // TODO: FIX possibly truncating conversion!
        let block_read_len = round_pow2(avg_frame_size as usize, PacketParser::BLOCK_LEN);

        let new_block_write = self.block_write + block_read_len;

        if new_block_write >= self.buf.len() - 8 {
            // Grow buffer to to twice it's existing length, rounded up to the nearest block size.
            let new_size = round_pow2(2 * (self.buf.len() - 8), PacketParser::BLOCK_LEN);

            if new_size > PacketParser::MAX_BUFFER_LEN {
                eprintln!("flac: buffer would exceed maximum size");
            }

            // eprintln!("flac: grow buffer, new_size={}", new_size);
            self.buf.resize(new_size + 8, 0);
        }

        // eprintln!("flac: fetch block, len={}", block_read_len);

        reader.read_buf_bytes(&mut self.buf[self.block_write..new_block_write])?;
        self.block_write = new_block_write;

        Ok(())
    }

    fn fetch_fragments<B: ByteStream>(&mut self, reader: &mut B) -> Result<()> {
        if self.fragment_read + PacketParser::FLAC_MAX_FRAME_HEADER_LEN >= self.block_write {
            self.fetch_block(reader)?;
        }

        // Scan for fragments, 8 bytes at a time.
        for pos in (self.fragment_read..(self.block_write - 8)).step_by(8) {
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&self.buf[pos..pos + 8]);

            self.fragment_read = pos + 8;

            // If, within the current 8 byte window, no single byte is 0xff then there cannot be a
            // frame synchronization preamble present.
            if !bits::contains_ones_byte_u64(u64::from_ne_bytes(buf)) {
                continue;
            }

            // Otherwise, there *may* be a frame synchronization preamble, scan for it byte-by-byte.
            let mut sync = 0u16;

            for (i, byte) in self.buf[pos..pos + 8 + 1].iter().enumerate() {
                sync = (sync << 8) | u16::from(*byte);

                // If the frame synchronization preamble was found, then interrogate the buffer for
                // a valid FLAC frame header.
                if (sync & 0xfffc) == 0xfff8 {
                    // If there are not enough bytes in the buffer to attempt parsing a frame then
                    // no more fragments can be fetched.
                    if pos + i - 1 + PacketParser::FLAC_MAX_FRAME_HEADER_LEN >= self.block_write {
                        // eprintln!(
                        //     "flac: found preamble, but not enough data is buffered, pos={}",
                        //     pos + i + 1
                        // );

                        self.fragment_read = pos + i - 1;
                        return Ok(());
                    }

                    let buf = &self.buf[pos + i + 1..pos + i + PacketParser::FLAC_MAX_FRAME_HEADER_LEN + 1];

                    // If the header buffer passes a quick sanity check, then attempt to parse the
                    // frame header in its entirety.
                    if is_likely_frame_header(buf) {
                        if let Ok(header) = read_frame_header(&mut BufStream::new(buf), sync) {
                            // eprintln!(
                            //     "flac: new fragment, ts={:?}, pos={}",
                            //     header.block_sequence,
                            //     pos + i - 1
                            // );

                            self.fragments.push_back(Fragment {
                                pos: pos + i - 1,
                                header,
                            });
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn remove_fragments(&mut self, to: usize) {
        while let Some(fragment) = self.fragments.front() {
            if fragment.pos < to {
                self.fragments.pop_front();
            }
            else {
                break;
            }
        }

        self.compact(to);
    }

    fn compact(&mut self, len: usize) {
        for fragment in self.fragments.iter_mut() {
            fragment.pos -= len;
        }

        // eprintln!("flac: compact, len={}", self.block_write - len);

        self.buf.copy_within(len.., 0);
        self.block_write -= len;
        self.fragment_read -= len;
    }

    /// Scores up-to 8 fragments, and returns the score, the number of fragments scored, and an
    /// array of bounds for those fragments.
    fn score(&self) -> (u8, usize, [usize; 9]) {
        let n_fragments = min(self.fragments.len(), 8);

        // Buffer position indicies of each fragment.
        let mut indicies = [0; 8 + 1];

        // Do the frame stream parameters match the global stream parameters (sample rate, and
        // bits per sample)?
        let mut score_par = 0u8;

        // Is the number of samples in the decoded frame consistent with the stream parameters?
        let mut score_len = 0u8;

        // Is the frame or sample sequence number monotonic? Is the blocking strategy
        // consistent with the stream parameters?
        let mut score_seq = 0u8;

        let iter = self.fragments.iter().zip(&mut indicies[0..n_fragments]).enumerate();

        let is_fixed = self.stream_info.block_sample_len.0 == self.stream_info.block_sample_len.1;

        for (i, (fragment, index)) in iter {
            // Stream parameter scoring: The optional parameters of the stream information block
            // match that of the fragment.
            if let Some(sample_rate) = fragment.header.sample_rate {
                if let Some(bps) = fragment.header.bits_per_sample {
                    if sample_rate == self.stream_info.sample_rate
                        && bps == self.stream_info.bits_per_sample
                    {
                        score_par |= 1 << i;
                    }
                }
            }

            // Fragment length scoring: The fragment's sample length is within the range provided in
            // the stream information block.
            if fragment.header.block_num_samples >= self.stream_info.block_sample_len.0
                && fragment.header.block_num_samples <= self.stream_info.block_sample_len.1
            {
                score_len |= 1 << i;
            }

            // Sequence scoring: The fragment's blocking strategy is consistent with the stream
            // information block, and the sequence number (frame number or sample number) is
            // monotonic given the current state.
            let is_monotonic = match fragment.header.block_sequence {
                BlockSequence::BySample(sample) => {
                    !is_fixed && (sample > self.last_seq || sample == 0)
                }
                BlockSequence::ByFrame(frame) => {
                    is_fixed && (u64::from(frame) > self.last_seq || frame == 0)
                }
            };

            if is_monotonic {
                score_seq |= 1 << i;
            }

            *index = fragment.pos;
        }

        indicies[n_fragments] = match self.fragments.get(n_fragments) {
            Some(fragment) => fragment.pos,
            None           => self.block_write,
        };

        // First, second, and third represent three descending tiers of confidence. A set bit
        // indicates that the fragment at the index of the set bit is likely to be a valid
        // fragment. If absolutely no bits are set then the stream is either very corrupt,
        // malicious, or not FLAC.
        let first  = (score_par & score_len) & score_seq;
        let second = (score_par | score_len) & score_seq;
        let third  = (score_par | score_len) | score_seq;

        let best = match (first, second, third) {
            (0, 0, _) => third,
            (0, _, _) => second,
            (_, _, _) => first,
        };

        // eprintln!("flac: indicies={:?}", &indicies);

        // eprintln!(
        //     "flac: score_par={:#04x}, score_len={:#04x}, score_seq={:#04x}, best={:#04x}",
        //     score_par,
        //     score_len,
        //     score_seq,
        //     best,
        // );

        (best, n_fragments, indicies)
    }

    pub fn parse<B: ByteStream>(&mut self, reader: &mut B) -> Result<Box<[u8]>> {
        // Given the current set of fragments, which may or may not contain whole and valid frames,
        // determine which fragment is the best-pick for the next frame. Starting from that
        // fragment, attempt to build a complete and valid frame.
        loop {
            let (mut best, n_scored, indicies) = self.score();

            let mut limit_hit = false;

            if n_scored > 0 {
                let mut iter = indicies[0..n_scored].iter().zip(&indicies[1..n_scored]);

                // Discard any fragments that preceed the best-pick fragment.
                while (best & 1) == 0 {
                    if let Some(_) = iter.next() {
                        self.fragments.pop_front();
                        eprintln!("flac: discard fragment");
                    }
                    else {
                        break;
                    }
                    best >>= 1;
                }

                let mut frame_len = 0;
                let mut crc = Crc16::new(0);

                // Attempt to merge fragments starting with the best-pick fragment. A frame is
                // considered complete and valid when the last two bytes of a fragment (a frame's
                // potential footer) equals the CRC16 of all fragments preceeding the current
                // fragment, excluding the final two bytes.
                for (count, (&start, &end)) in iter.enumerate() {
                    // Calculate the CRC16 up-to the (potential) footer.
                    crc.process_buf_bytes(&self.buf[start..end - 2]);

                    let mut footer_buf = [0; 2];
                    footer_buf[0..2].copy_from_slice(&self.buf[end - 2..end]);

                    frame_len += end - start;

                    // If the CRC16 matches then a frame was found!
                    if crc.crc() == u16::from_be_bytes(footer_buf) {
                        // Copy the frame data into a new buffer.
                        let frame = Box::<[u8]>::from(&self.buf[end - frame_len..end]);

                        // Update the frame size history.
                        self.frame_size_hist[self.n_frames % 4] = frame.len() as u32;
                        self.n_frames += 1;

                        // Update the last sequence.

                        self.remove_fragments(end);

                        return Ok(frame);
                    }

                    crc.process_buf_bytes(&footer_buf);

                    // Choosing when to "give-up" on a fragment is a difficult problem. We rely on
                    // the frame's CRC to know if a set of fragments successfully combine into a
                    // complete and valid frame, but what if that CRC is, itself, corrupt? Likewise,
                    // what if the frame is actually corrupt? This is a difficult problem that can
                    // only be solved heuristically since the FLAC container does not provide use
                    // enough information.
                    //
                    // The heuristics, and justifications, are as follows:
                    //
                    // 1) A frame may never exceed 16MB per the specification.
                    //
                    // 2) If there is a second-best fragment, then discard all predecessors to it
                    //    and try again. A second-best fragment met all the same criteria as the
                    //    best-pick fragment except that it is sequentially after the best-pick
                    //    fragment. In other words, we have as much confidence in the second-best
                    //    fragment being the start of a frame as we do the best-pick fragment. Thus,
                    //    if a second-best fragment exists, it must be the exclusive upper bound of
                    //    the frame that starting with the best-pick fragment. Therefore, if the
                    //    best-pick fragment failed to form a complete and valid frame when it
                    //    reaches the second-best pick fragment, the frame itself must be corrupt
                    //    and can be discarded.
                    //
                    // 3a) If there is no second-best fragment to bound the frame starting from the
                    //     best-pick fragment, then if the stream information block defines a
                    //     maximum frame size, use that limit to bound the frame.
                    //
                    // 3b) If the stream information block does not define a maximum frame size, and
                    //     if average frame length moving-average filter is filled, then use 2x the
                    //     average frame size as the limit.
                    //
                    // 3c) If the average frame length moving-average filter has not been filled,
                    //     then use 4 fragments as the limit.
                    //
                    // If none of these heuristics are met, then it is reasonable to continue to
                    // fetch more fragments and see if a complete and valid frame can be formed with
                    // the extra fragments.

                    // Heuristic 1.
                    if frame_len >= PacketParser::FLAC_MAX_FRAME_LEN {
                        eprintln!("flac: rebuild failure; frame exceeds 16MB");
                        limit_hit = true;
                    }
                    // Heuristic 2.
                    else if best & (2 << count) != 0 {
                        eprintln!(
                            "flac: rebuild failure; \
                             frame exceeds lower-bound of next-best fragment"
                        );
                        limit_hit = true;
                    }
                    // Heuristic 3a.
                    else if self.stream_info.frame_byte_len.1 > 0 {
                        if (frame_len as u32) > self.stream_info.frame_byte_len.1 {
                            eprintln!(
                                "flac: rebuild failure; \
                                 frame exceeds stream's frame length limit"
                            );
                            limit_hit = true;
                        }
                    }
                    // Heuristic 3b.
                    else if self.n_frames >= 4 {
                        let avg_frame_size = (self.frame_size_hist[0]
                                                + self.frame_size_hist[1]
                                                + self.frame_size_hist[2]
                                                + self.frame_size_hist[3]) / 4;

                        if (frame_len as u32) > 2 * avg_frame_size {
                            eprintln!(
                                "flac: rebuild failure; \
                                 frame exceeds 2x average historical length"
                            );
                            limit_hit = true;
                        }
                    }
                    // Heuristic 3c.
                    else if count >= 4 {
                        eprintln!("flac: rebuild failure; frame exceeds fragment limit");
                        limit_hit = true;
                    }

                    // If a limit was hit, break out of the rebuild loop.
                    if limit_hit {
                        break;
                    }
                }

                // If a limit was hit, the current fragment, and possibly other fragments must be
                // discarded to make progress.
                if limit_hit {
                    if best & 0xfe != 0 {
                        // Zero out the current best-pick fragment.
                        best &= !1;

                        // Pop all fragments preceeding the second-best pick fragment.
                        while self.fragments.len() > 0 && (best & 0x1) == 0 {
                            eprintln!("flac: discard fragment");

                            self.fragments.pop_front();
                            best >>= 1;
                        }
                    }
                    else {
                        self.fragments.pop_front();
                    }
                }
            }

            // Nom nom, need more fragments to continue!
            self.fetch_fragments(reader)?;
        }
    }

}
