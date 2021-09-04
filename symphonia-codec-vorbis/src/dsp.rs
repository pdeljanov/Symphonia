// Symphonia
// Copyright (c) 2019-2021 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::cmp::min;

/// As defined in section 9.2.1 of the Vorbis I specification.
///
/// The `ilog` function returns the position number (1 through n) of the highest set bit in the two’s
/// complement integer value `x`. Values of `x` less than zero are defined to return zero.

use symphonia_core::dsp::mdct::Imdct;

use super::residue::ResidueScratch;
use super::window::{Windows, Window, WindowHalf};

pub struct LappingState {
    pub prev_block_size: usize,
    pub prev_win_right: WindowHalf,
}

pub struct Dsp {
    /// DSP channels (max. 256 per-spec, but actually limited to 32 by Symphonia).
    pub channels: Vec<DspChannel>,
    /// Residue scratch-pad.
    pub residue_scratch: ResidueScratch,
    /// IMDCT for short-blocks.
    pub imdct_short: Imdct,
    /// IMDCT for long-blocks.
    pub imdct_long: Imdct,
    /// Windows for overlap-add.
    pub windows: Windows,
    /// Lapping state.
    pub lapping_state: Option<LappingState>,
}

impl Dsp {
    pub fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.reset();
        }

        self.lapping_state = None;
    }
}

pub struct DspChannel {
    /// The channel floor buffer.
    pub floor: Vec<f32>,
    /// The channel residue buffer.
    pub residue: Vec<f32>,
    /// Do not decode!
    pub do_not_decode: bool,
    /// The output buffer for the IMDCT, containing the samples for overlap-add.
    overlap: Vec<f32>,
}

impl DspChannel {
    pub fn new(bs1_exp: u8) -> Self {
        DspChannel {
            floor: vec![0.0; (1 << bs1_exp) >> 1],
            residue: vec![0.0; (1 << bs1_exp) >> 1],
            overlap: vec![0.0; 1 << bs1_exp],
            do_not_decode: false,
        }
    }

    pub fn synth(
        &mut self,
        blk_len: usize,
        lap_state: &Option<LappingState>,
        win: &Window,
        imdct: &mut Imdct,
        buf: &mut [f32]
    ) {
         let buf_len = buf.len();

        // Step 1
        //
        // Copy the right-hand side of overlap buffer (previously windowed) into the output buffer.
        // Ignore the all-zero region.
        let overlap_end = if let Some(lap_state) = &lap_state {
            let prev_rhs_start = lap_state.prev_block_size >> 1;
            let rhs = &self.overlap[prev_rhs_start..lap_state.prev_win_right.end];
            buf[..rhs.len()].copy_from_slice(rhs);

            // Samples after this are not overlapped.
            rhs.len()
        }
        else {
            0
        };

        // Step 2
        //
        // Perform the inverse MDCT on the audio spectrum and overwriting the previous overlap
        // buffer.
        imdct.imdct(&self.floor[..blk_len >> 1], &mut self.overlap[..blk_len], 1.0);

        // Step 3
        //
        // Apply windowing to the samples produced by the IMDCT. Only the samples overlapping the
        // non-zero or non-unity portions of the window needs to be multiplied.
        let l_start = win.left.start;
        let l_end = win.left.end;

        for (s, &w) in self.overlap[l_start..l_end].iter_mut().zip(&win.window[l_start..l_end]) {
            *s *= w;
        }

        let r_start = win.right.start;
        let r_end = win.right.end;

        for (s, &w) in self.overlap[r_start..r_end].iter_mut().zip(&win.window[r_start..r_end]) {
            *s *= w;
        }

        // Step 4
        //
        // Overlap-add the windowed left-hand side of the overlap buffer with the output buffer.
        // Ignore the all-zero region.
        if lap_state.is_some() {
            let lhs_start = win.left.start;
            let lhs = &self.overlap[lhs_start..blk_len >> 1];

            let overlap_start = buf_len - lhs.len();
            let overlap_end = min(buf.len(), overlap_end);

            // The left-hand side overlaps the right-hand side in this region. The output buffer
            // contains the left-hand side samples, so add the right-hand side samples.
            for (o, &s) in buf[overlap_start..overlap_end].iter_mut().zip(lhs) {
                *o += s;
            }

            // The left-hand side has ended, so simply copy the right-hand side samples to the output.
            for (o, &s) in buf[overlap_end..].iter_mut().zip(&lhs[overlap_end - overlap_start..]) {
                *o = s;
            }
        }

        // Step 5
        //
        // Clamp the output samples.
        for s in buf.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
    }

    pub fn reset(&mut self) {
        // Clear the overlap buffer. Nothing else is used across packets.
        self.overlap.fill(0.0);
    }
}