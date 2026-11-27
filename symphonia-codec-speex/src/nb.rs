// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The decoder follows the floating point build of the Xiph.Org Speex reference implementation, see
// NOTICE-xiph.txt.

use symphonia_core::errors::{Result, decode_error};

use crate::bits::Bits;
use crate::dsp::{bw_lpc, highpass, iir_mem, lsp_interpolate, lsp_to_lpc, multicomb, rand, rms};
use crate::modes::{NB_SUBMODES, Pitch, Submode, unquant_innovation};
use crate::stereo::Stereo;

pub(crate) const FRAME_SIZE: usize = 160;
const SUBFRAME_SIZE: usize = 40;
const NB_SUBFRAMES: usize = 4;
const ORDER: usize = 10;
const PITCH_START: i32 = 17;
const PITCH_END: i32 = 144;
const EXC_BUF: usize = FRAME_SIZE + 2 * PITCH_END as usize + SUBFRAME_SIZE + 12;
const EXC_OFFSET: usize = 2 * PITCH_END as usize + SUBFRAME_SIZE + 6;
const LSP_MARGIN: f32 = 0.002;

/// The number of bits of the wideband frame of a submode, which the narrowband decoder skips.
const WB_SKIP: [usize; 8] = [0, 36, 112, 192, 352, 0, 0, 0];

const EXC_GAIN_SCAL3: [f32; 8] =
    [0.061130, 0.163546, 0.310413, 0.428220, 0.555887, 0.719055, 0.938694, 1.326874];
const EXC_GAIN_SCAL1: [f32; 2] = [0.70469, 1.05127];

/// In-band signalling of the stereo state, which only exists in the low band decoder.
pub(crate) const INBAND_STEREO: u32 = 9;

pub(crate) struct NbDecoder {
    exc_buf: [f32; EXC_BUF],
    mem_sp: [f32; ORDER],
    mem_hp: [f32; 2],
    old_qlsp: [f32; ORDER],
    interp_qlpc: [f32; ORDER],
    pub(crate) pi_gain: [f32; NB_SUBFRAMES],
    pub(crate) exc_rms: [f32; NB_SUBFRAMES],
    pub(crate) innov: [f32; FRAME_SIZE],
    voc_m1: f32,
    voc_m2: f32,
    voc_mean: f32,
    voc_offset: i32,
    seed: u32,
    first: bool,
    submode_id: usize,
    pub(crate) wideband: bool,
    /// Whether the encoder stopped transmitting frames for the comfort noise.
    pub(crate) dtx: bool,
    pub(crate) stereo: Option<Stereo>,
}

impl NbDecoder {
    pub(crate) fn new() -> Self {
        NbDecoder {
            exc_buf: [0.0; EXC_BUF],
            mem_sp: [0.0; ORDER],
            mem_hp: [0.0; 2],
            old_qlsp: [0.0; ORDER],
            interp_qlpc: [0.0; ORDER],
            pi_gain: [0.0; NB_SUBFRAMES],
            exc_rms: [0.0; NB_SUBFRAMES],
            innov: [0.0; FRAME_SIZE],
            voc_m1: 0.0,
            voc_m2: 0.0,
            voc_mean: 0.0,
            voc_offset: 0,
            seed: 1000,
            first: true,
            submode_id: 5,
            wideband: false,
            dtx: false,
            stereo: None,
        }
    }

    fn exc(&self, i: isize) -> f32 {
        self.exc_buf[(EXC_OFFSET as isize + i) as usize]
    }

    fn handle_inband(&mut self, bits: &mut Bits<'_>) -> Result<()> {
        let id = bits.read(4)?;
        match (id, self.stereo.as_mut()) {
            (INBAND_STEREO, Some(stereo)) => stereo.request(bits)?,
            _ => {
                let adv = match id {
                    0 | 1 => 1,
                    2..=7 => 4,
                    8 | 9 => 8,
                    10 | 11 => 16,
                    12 | 13 => 32,
                    _ => 64,
                };
                bits.advance(adv);
            }
        }
        Ok(())
    }

    /// Reads the headers in front of the narrowband block and returns its submode identifier.
    fn find_block(&mut self, bits: &mut Bits<'_>) -> Result<usize> {
        loop {
            if bits.remaining() < 5 {
                return decode_error("speex: packet is truncated");
            }

            let mut wideband = bits.read(1)?;
            for _ in 0..2 {
                if wideband == 0 {
                    break;
                }
                let submode = bits.read(3)? as usize;
                let advance = WB_SKIP[submode];
                if advance == 0 {
                    return decode_error("speex: invalid wideband submode");
                }
                bits.advance(advance - 4);
                if bits.remaining() < 5 {
                    return decode_error("speex: packet is truncated");
                }
                wideband = bits.read(1)?;
            }
            if wideband != 0 {
                return decode_error("speex: too many wideband layers");
            }

            if bits.remaining() < 4 {
                return decode_error("speex: packet is truncated");
            }
            let m = bits.read(4)?;
            match m {
                15 => return decode_error("speex: end of the packet"),
                14 => self.handle_inband(bits)?,
                13 => {
                    let size = bits.read(4)? as usize;
                    bits.advance(5 + 8 * size);
                }
                9..=12 => return decode_error("speex: invalid mode"),
                _ => return Ok(m as usize),
            }
        }
    }

    /// Decodes a frame of 160 samples.
    pub(crate) fn decode(&mut self, bits: &mut Bits<'_>, out: &mut [f32]) -> Result<()> {
        let id = self.find_block(bits)?;
        self.submode_id = id;

        self.exc_buf
            .copy_within(FRAME_SIZE..FRAME_SIZE + 2 * PITCH_END as usize + SUBFRAME_SIZE + 12, 0);

        let Some(mode) = NB_SUBMODES[id]
        else {
            let mut lpc = [0f32; ORDER];
            bw_lpc(0.93, &self.interp_qlpc, &mut lpc);
            let gain = rms(&self.exc_buf[EXC_OFFSET..EXC_OFFSET + FRAME_SIZE]);
            for i in 0..FRAME_SIZE {
                self.exc_buf[EXC_OFFSET + i] = rand(gain, &mut self.seed);
            }
            self.first = true;
            let exc: Vec<f32> = self.exc_buf[EXC_OFFSET..EXC_OFFSET + FRAME_SIZE].to_vec();
            iir_mem(&exc, &lpc, out, &mut self.mem_sp);
            self.innov = [0.0; FRAME_SIZE];
            self.pi_gain = [1.0; NB_SUBFRAMES];
            self.exc_rms = [0.0; NB_SUBFRAMES];
            return Ok(());
        };

        let qlsp = mode.lsp.unquant(ORDER, bits)?;
        if self.first {
            self.old_qlsp.copy_from_slice(&qlsp);
        }

        let mut ol_pitch = 0;
        if mode.lbr_pitch != -1 {
            ol_pitch = PITCH_START + bits.read(7)? as i32;
        }

        let mut ol_pitch_coef = 0.0;
        if mode.forced_pitch_gain {
            ol_pitch_coef = 0.066667 * bits.read(4)? as f32;
        }

        let ol_gain = (bits.read(5)? as f32 / 3.5).exp();

        if id == 1 {
            self.dtx = bits.read(4)? == 15;
        }
        else if id > 1 {
            self.dtx = false;
        }

        let mut best_pitch = 40;
        let mut best_pitch_gain = 0.0;

        for sub in 0..NB_SUBFRAMES {
            let offset = SUBFRAME_SIZE * sub;
            let base = EXC_OFFSET + offset;
            self.exc_buf[base..base + SUBFRAME_SIZE].fill(0.0);

            let pit_min = if mode.lbr_pitch != -1 {
                let margin = mode.lbr_pitch;
                if margin != 0 { (ol_pitch - margin + 1).max(PITCH_START) } else { ol_pitch }
            }
            else {
                PITCH_START
            };

            let (pitch, gains, exc32) =
                self.unquant_pitch(&mode, pit_min, ol_pitch_coef, base, bits)?;

            let tap = gains[1].abs()
                + if gains[0] > 0.0 { gains[0] } else { -0.5 * gains[0] }
                + if gains[2] > 0.0 { gains[2] } else { -0.5 * gains[2] };

            let (p, bp) = (pitch, best_pitch);
            let cond1 = tap > best_pitch_gain
                && (2 * bp - p).abs() >= 3
                && (3 * bp - p).abs() >= 4
                && (4 * bp - p).abs() >= 5;
            let cond2 = tap > 0.6 * best_pitch_gain
                && ((bp - 2 * p).abs() < 3 || (bp - 3 * p).abs() < 4 || (bp - 4 * p).abs() < 5);
            let cond3 = 0.67 * tap > best_pitch_gain
                && ((2 * bp - p).abs() < 3 || (3 * bp - p).abs() < 4 || (4 * bp - p).abs() < 5);
            if cond1 || cond2 || cond3 {
                best_pitch = pitch;
                if tap > best_pitch_gain {
                    best_pitch_gain = tap;
                }
            }

            // Fixed codebook contribution.
            let ener = match mode.have_subframe_gain {
                3 => EXC_GAIN_SCAL3[bits.read(3)? as usize] * ol_gain,
                1 => EXC_GAIN_SCAL1[bits.read(1)? as usize] * ol_gain,
                _ => ol_gain,
            };

            let mut innov = [0f32; SUBFRAME_SIZE];
            unquant_innovation(mode.innovation, &mut innov, bits, &mut self.seed)?;
            for v in innov.iter_mut() {
                *v *= ener;
            }
            if mode.double_codebook {
                let mut innov2 = [0f32; SUBFRAME_SIZE];
                unquant_innovation(mode.innovation, &mut innov2, bits, &mut self.seed)?;
                for (v, v2) in innov.iter_mut().zip(&innov2) {
                    *v += 0.454545 * ener * v2;
                }
            }

            for i in 0..SUBFRAME_SIZE {
                self.exc_buf[base + i] = exc32[i] + innov[i];
            }
            self.innov[offset..offset + SUBFRAME_SIZE].copy_from_slice(&innov);

            if id == 1 {
                self.vocoder(base, &innov, ol_pitch, ol_pitch_coef, ol_gain);
            }
        }

        for sub in 0..NB_SUBFRAMES {
            self.exc_rms[sub] = rms(&self.exc_buf
                [EXC_OFFSET + sub * SUBFRAME_SIZE..EXC_OFFSET + (sub + 1) * SUBFRAME_SIZE]);
        }

        if mode.comb_gain > 0.0 {
            let pos = EXC_OFFSET as isize - SUBFRAME_SIZE as isize;
            let (a, b) = out.split_at_mut(2 * SUBFRAME_SIZE);
            multicomb(
                &self.exc_buf,
                pos,
                a,
                2 * SUBFRAME_SIZE,
                best_pitch as isize,
                40,
                mode.comb_gain,
            );
            multicomb(
                &self.exc_buf,
                pos + 2 * SUBFRAME_SIZE as isize,
                b,
                2 * SUBFRAME_SIZE,
                best_pitch as isize,
                40,
                mode.comb_gain,
            );
        }
        else {
            for i in 0..FRAME_SIZE as isize {
                out[i as usize] = self.exc(i - SUBFRAME_SIZE as isize);
            }
        }

        let mut interp = [0f32; ORDER];
        let mut ak = [0f32; ORDER];
        for sub in 0..NB_SUBFRAMES {
            let offset = SUBFRAME_SIZE * sub;
            lsp_interpolate(&self.old_qlsp, &qlsp, &mut interp, sub, NB_SUBFRAMES, LSP_MARGIN);
            lsp_to_lpc(&interp, &mut ak);

            let mut pi_g = 1.0;
            for i in (0..ORDER).step_by(2) {
                pi_g += ak[i + 1] - ak[i];
            }
            self.pi_gain[sub] = pi_g;

            let sp = &mut out[offset..offset + SUBFRAME_SIZE];
            let input: [f32; SUBFRAME_SIZE] = sp.try_into().unwrap_or([0.0; SUBFRAME_SIZE]);
            iir_mem(&input, &self.interp_qlpc, sp, &mut self.mem_sp);

            self.interp_qlpc = ak;
        }

        highpass(out, self.wideband, &mut self.mem_hp);

        self.old_qlsp.copy_from_slice(&qlsp);
        self.first = false;

        Ok(())
    }

    /// Reads the pitch of a subframe and returns it with the three gains and the contribution
    /// of the adaptive codebook.
    fn unquant_pitch(
        &mut self,
        mode: &Submode,
        pit_min: i32,
        ol_pitch_coef: f32,
        base: usize,
        bits: &mut Bits<'_>,
    ) -> Result<(i32, [f32; 3], [f32; SUBFRAME_SIZE])> {
        let mut out = [0f32; SUBFRAME_SIZE];

        match mode.pitch {
            Pitch::Forced => {
                let coef = ol_pitch_coef.min(0.99);
                for i in 0..SUBFRAME_SIZE {
                    out[i] = self.exc_buf[base + i - pit_min as usize] * coef;
                    self.exc_buf[base + i] = out[i];
                }
                Ok((pit_min, [0.0, coef, 0.0], clamp(out)))
            }
            Pitch::ThreeTap { gain_codebook, gain_bits, pitch_bits } => {
                let pitch = bits.read(pitch_bits)? as i32 + pit_min;
                let index = bits.read(gain_bits)? as usize;
                let gain: [f32; 3] = std::array::from_fn(|k| {
                    0.015625 * f32::from(gain_codebook[index * 4 + k]) + 0.5
                });

                for i in 0..3 {
                    let pp = (pitch + 1 - i as i32) as usize;
                    let tmp1 = SUBFRAME_SIZE.min(pp);
                    for j in 0..tmp1 {
                        out[j] += gain[2 - i] * self.exc_buf[base + j - pp];
                    }
                    let tmp3 = SUBFRAME_SIZE.min(pp + pitch as usize);
                    for j in tmp1..tmp3 {
                        out[j] += gain[2 - i] * self.exc_buf[base + j - pp - pitch as usize];
                    }
                }
                Ok((pitch, gain, clamp(out)))
            }
        }
    }

    /// The excitation of the comfort noise mode.
    fn vocoder(&mut self, base: usize, innov: &[f32], ol_pitch: i32, coef: f32, ol_gain: f32) {
        let g = (1.5 * (coef - 0.2)).clamp(0.0, 1.0);

        self.exc_buf[base..base + SUBFRAME_SIZE].fill(0.0);
        while self.voc_offset < SUBFRAME_SIZE as i32 {
            if self.voc_offset >= 0 {
                self.exc_buf[base + self.voc_offset as usize] =
                    (2.0 * ol_pitch as f32).sqrt() * (g * ol_gain);
            }
            self.voc_offset += ol_pitch;
        }
        self.voc_offset -= SUBFRAME_SIZE as i32;

        for i in 0..SUBFRAME_SIZE {
            let exci = self.exc_buf[base + i];
            let v = 0.7 * exci
                + 0.3 * self.voc_m1
                + ((1.0 - 0.85 * g) * innov[i] - 0.15 * g * self.voc_m2);
            self.voc_m1 = exci;
            self.voc_m2 = innov[i];
            self.voc_mean = 0.8 * self.voc_mean + 0.2 * v;
            self.exc_buf[base + i] = v - self.voc_mean;
        }
    }
}

/// Keeps a crafted stream from producing values that blow up the filters.
fn clamp(mut v: [f32; SUBFRAME_SIZE]) -> [f32; SUBFRAME_SIZE] {
    for x in v.iter_mut() {
        *x = if x.is_nan() { 0.0 } else { x.clamp(-32000.0, 32000.0) };
    }
    v
}
