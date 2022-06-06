// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::cmp::min;
use std::collections::VecDeque;

use symphonia_core::checksum::Crc16Ansi;
use symphonia_core::errors::{Error, Result};
use symphonia_core::io::{BufReader, Monitor, ReadBytes};
use symphonia_core::util::bits;
use symphonia_utils_xiph::flac::metadata::StreamInfo;

use log::{error, trace, warn};

use super::frame::{is_likely_frame_header, read_frame_header, BlockSequence, FrameHeader};

#[inline(always)]
fn round_pow2(value: usize, pow2: usize) -> usize {
    (value + (pow2 - 1)) & !(pow2 - 1)
}

struct Fragment {
    pos: usize,
    header: FrameHeader,
}

pub struct ParsedPacket {
    pub buf: Box<[u8]>,
    pub ts: u64,
    pub dur: u64,
}

pub struct PacketParser {
    stream_info: StreamInfo,
    buf: Vec<u8>,
    buf_write: usize,
    buf_read: usize,
    fragments: VecDeque<Fragment>,
    frame_size_hist: [u32; 4],
    n_frames: usize,
    last_seq: u64,
    last_read_err: Option<Error>,
}

impl Default for PacketParser {
    fn default() -> Self {
        PacketParser {
            stream_info: Default::default(),
            buf: vec![0; (2 * PacketParser::FLAC_AVG_FRAME_LEN) as usize],
            buf_write: 0,
            buf_read: 0,
            fragments: Default::default(),
            frame_size_hist: [PacketParser::FLAC_AVG_FRAME_LEN; 4],
            n_frames: 0,
            last_seq: 0,
            last_read_err: None,
        }
    }
}

impl PacketParser {
    /// The size of the largest possible valid FLAC frame header.
    const FLAC_MAX_FRAME_HEADER_LEN: usize = 16;

    /// The size of the largest possible complete and valid FLAC frame.
    const FLAC_MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

    /// The "average" FLAC frame length.
    const FLAC_AVG_FRAME_LEN: u32 = 8 * 1024;

    /// Number of padding bytes in the buffer.
    const BUF_PADDING: usize = 8;

    /// The maximum buffer length possible.
    const MAX_BUF_LEN: usize = PacketParser::FLAC_MAX_FRAME_LEN
        + PacketParser::FLAC_MAX_FRAME_HEADER_LEN
        + PacketParser::BUF_PADDING;

    // Frames:    [                           F0                          |   ..   ]
    //                                                                    :
    // Fragments: [  f0   |  f1   |      f2     |    f3    |    f4   | f5 |   ..   ]
    //
    // Frames are complete FLAC frames. Frames are composed of an integer number of fragments.
    //
    // Data is buffered from the media source stream. Once buffered, the data is scanned for
    // fragments. A fragment shall start with a FLAC frame header preamble, and decode into a valid
    // FLAC frame header. Fragments are then be combined until a valid FLAC frame is found. A set of
    // rules and heuristics are used to limit the number of fragments that can be recombined. This
    // is required to detect errors.

    /// Reset the packet parser for a new stream.
    pub fn hard_reset(&mut self, stream_info: StreamInfo) {
        self.stream_info = stream_info;
        self.soft_reset()
    }

    /// Reset the packet parser after a stream discontinuity.
    pub fn soft_reset(&mut self) {
        self.frame_size_hist = [PacketParser::FLAC_AVG_FRAME_LEN; 4];
        self.n_frames = 0;
        self.last_seq = 0;
        self.last_read_err = None;
        self.buf_write = 0;
        self.buf_read = 0;
        self.fragments.clear();
    }

    fn buffer_data<B: ReadBytes>(&mut self, reader: &mut B) -> Result<()> {
        // Calculate the average frame size.
        let avg_frame_size = ((self.frame_size_hist[0]
            + self.frame_size_hist[1]
            + self.frame_size_hist[2]
            + self.frame_size_hist[3])
            / 4) as usize;

        // Read average frame size bytes.
        let new_buf_write = self.buf_write + round_pow2(avg_frame_size, 4096);

        if new_buf_write >= self.buf.len() - PacketParser::BUF_PADDING {
            // Grow buffer to 1.25x the average frame size, plus padding, rounded to the nearest
            // multiple of 4kB.
            let new_size = round_pow2(((10 * new_buf_write) / 8) + PacketParser::BUF_PADDING, 4096);

            if new_size > PacketParser::MAX_BUF_LEN {
                error!("buffer would exceed maximum size");
                // TODO: This is a hard error.
            }

            // trace!("grow buffer: new_size={}", new_size);

            self.buf.resize(new_size, 0);
        }

        // trace!("fetch data: buf_write={}, new_buf_write={}", self.buf_write, new_buf_write);

        self.buf_write += reader.read_buf(&mut self.buf[self.buf_write..new_buf_write])?;

        Ok(())
    }

    fn read_fragments<B: ReadBytes>(&mut self, reader: &mut B) -> Result<()> {
        // Buffer more data if there is not enough to scan for fragments.
        if self.buf_write - self.buf_read <= PacketParser::FLAC_MAX_FRAME_HEADER_LEN {
            self.buffer_data(reader)?;
        }

        // Scan for fragments, 8 bytes at a time.
        for pos in (self.buf_read..(self.buf_write - 8)).step_by(8) {
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&self.buf[pos..pos + 8]);

            self.buf_read = pos + 8;

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
                    if pos + i - 1 + PacketParser::FLAC_MAX_FRAME_HEADER_LEN >= self.buf_write {
                        // trace!(
                        //     "found preamble, but not enough data is buffered, pos={}",
                        //     pos + i + 1
                        // );

                        self.buf_read = pos + i - 1;
                        return Ok(());
                    }

                    let buf = &self.buf
                        [pos + i + 1..pos + i + PacketParser::FLAC_MAX_FRAME_HEADER_LEN + 1];

                    // If the header buffer passes a quick sanity check, then attempt to parse the
                    // frame header in its entirety.
                    if is_likely_frame_header(buf) {
                        if let Ok(header) = read_frame_header(&mut BufReader::new(buf), sync) {
                            // trace!(
                            //     "new fragment, ts={:?}, pos={}",
                            //     header.block_sequence,
                            //     pos + i - 1
                            // );

                            self.fragments.push_back(Fragment { pos: pos + i - 1, header });
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn remove_fragments(&mut self, to: usize) {
        // Pop fragments starting before to.
        while let Some(fragment) = self.fragments.front() {
            if fragment.pos < to {
                self.fragments.pop_front();
            }
            else {
                break;
            }
        }

        // Shift the position of all fragments after to by to.
        for fragment in self.fragments.iter_mut() {
            fragment.pos -= to;
        }

        // trace!("compact, len={}", self.buf_write - to);

        // Compact the data buffer.
        self.buf.copy_within(to..self.buf_write, 0);
        self.buf_write -= to;
        self.buf_read -= min(to, self.buf_read);
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

        let is_fixed = self.stream_info.block_len_min == self.stream_info.block_len_max;

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
            if fragment.header.block_num_samples >= self.stream_info.block_len_min
                && fragment.header.block_num_samples <= self.stream_info.block_len_max
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
            None => self.buf_write,
        };

        // First, second, and third represent three descending tiers of confidence. A set bit
        // indicates that the fragment at the index of the set bit is likely to be a valid
        // fragment. If absolutely no bits are set then the stream is either very corrupt,
        // malicious, or not FLAC.
        let first = (score_par & score_len) & score_seq;
        let second = (score_par | score_len) & score_seq;
        let third = (score_par | score_len) | score_seq;

        let best = match (first, second, third) {
            (0, 0, _) => third,
            (0, _, _) => second,
            (_, _, _) => first,
        };

        // trace!("indicies={:?}", &indicies);

        // trace!(
        //     "score_par={:#04x}, score_len={:#04x}, score_seq={:#04x}, best={:#04x}",
        //     score_par,
        //     score_len,
        //     score_seq,
        //     best,
        // );

        (best, n_fragments, indicies)
    }

    pub fn parse<B: ReadBytes>(&mut self, reader: &mut B) -> Result<ParsedPacket> {
        // Given the current set of fragments, which may or may not contain whole and valid frames,
        // determine which fragment is the best-pick for the next frame. Starting from that
        // fragment, attempt to build a complete and valid frame.
        loop {
            let (mut best, n_scored, indicies) = self.score();

            let mut limit_hit = false;

            // Scoring works best if there is more than one fragment buffered. Always try to have
            // atleast 1 fragment buffered at a time unless there is an IO error.
            if n_scored > 1 || self.last_read_err.is_some() {
                let mut iter = indicies[0..n_scored].iter().zip(&indicies[1..n_scored + 1]);

                // Discard any fragments that preceed the best-pick fragment.
                while (best & 1) == 0 {
                    if iter.next().is_some() {
                        self.fragments.pop_front();
                        trace!("discard fragment");
                    }
                    else {
                        break;
                    }
                    best >>= 1;
                }

                let mut frame_len = 0;
                let mut crc = Crc16Ansi::new(0);

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
                        let buf = Box::<[u8]>::from(&self.buf[end - frame_len..end]);

                        // Update the frame size history.
                        self.frame_size_hist[self.n_frames % 4] = buf.len() as u32;
                        self.n_frames += 1;

                        // Calculate the timestamp and duration of the complete frame.
                        let frag = self.fragments.front().unwrap();

                        let dur = u64::from(frag.header.block_num_samples);

                        let ts = match &frag.header.block_sequence {
                            BlockSequence::BySample(sample) => *sample,
                            BlockSequence::ByFrame(frame) => u64::from(*frame) * dur,
                        };

                        // Remove the fragments that have been consumed to parse this frame.
                        self.remove_fragments(end);

                        return Ok(ParsedPacket { buf, ts, dur });
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
                    // 2a) If the stream information block defines a maximum frame size, use that
                    //     limit to bound the frame.
                    //
                    // 2b) If the stream information block does not define a maximum frame size, and
                    //     if average frame length moving-average filter is filled, then use 2x the
                    //     average frame size as the limit.
                    //
                    // 2c) If the average frame length moving-average filter has not been filled,
                    //     then use 4 fragments as the limit.
                    //
                    // If none of these heuristics are met, then it is reasonable to continue to
                    // fetch more fragments and see if a complete and valid frame can be formed with
                    // the extra fragments.
                    //
                    // Heuristics only apply if there is more than 1 fragment. If there is only one
                    // fragment there are three possibilities for the state of the fragment:
                    //
                    //  i)   a complete and valid frame
                    //  ii)  a complete and valid frame + the start of another frame
                    //  iii) a partial or corrupt frame, or random data
                    //
                    // In the first case, these heuristics won't be reached. In the second and third
                    // cases, more data may be needed to properly partition the fragment into a
                    // frame.
                    if n_scored > 1 {
                        // Heuristic 1.
                        if frame_len >= PacketParser::FLAC_MAX_FRAME_LEN {
                            warn!("rebuild failure; frame exceeds 16MB");
                            limit_hit = true;
                        }
                        // Heuristic 2a.
                        else if self.stream_info.frame_byte_len_max > 0 {
                            if (frame_len as u32) > self.stream_info.frame_byte_len_max {
                                warn!(
                                    "rebuild failure; \
                                    frame exceeds stream's frame length limit ({} > {})",
                                    frame_len, self.stream_info.frame_byte_len_max
                                );
                                limit_hit = true;
                            }
                        }
                        // Heuristic 2b.
                        else if self.n_frames >= 4 {
                            let avg_frame_size = (self.frame_size_hist[0]
                                + self.frame_size_hist[1]
                                + self.frame_size_hist[2]
                                + self.frame_size_hist[3])
                                / 4;

                            if (frame_len as u32) > 2 * avg_frame_size {
                                warn!(
                                    "rebuild failure; \
                                    frame exceeds 2x average historical length"
                                );
                                limit_hit = true;
                            }
                        }
                        // Heuristic 2c.
                        else if count >= 4 {
                            warn!("rebuild failure; frame exceeds fragment limit");
                            limit_hit = true;
                        }

                        // If a limit was hit, break out of the rebuild loop.
                        if limit_hit {
                            break;
                        }
                    }
                }

                // If a limit was hit, the current fragment, and possibly other fragments must be
                // discarded to make progress.
                if limit_hit {
                    if best & 0xfe != 0 {
                        // Zero out the current best-pick fragment.
                        best &= !1;

                        // Pop all fragments preceeding the second-best pick fragment.
                        while !self.fragments.is_empty() && (best & 0x1) == 0 {
                            trace!("discard fragment");

                            self.fragments.pop_front();
                            best >>= 1;
                        }
                    }
                    else {
                        self.fragments.pop_front();
                    }
                }
            }

            // Read more fragments if there was no pending error. Otherwise, return the error.
            self.last_read_err = match self.last_read_err.take() {
                None => self.read_fragments(reader).err(),
                Some(err) => return Err(err),
            }
        }
    }
}
