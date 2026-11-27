// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::Result;

use crate::bits::Bits;
use crate::tables::*;

#[derive(Clone, Copy)]
pub(crate) enum LspCodebook {
    /// The 30 bit narrowband code book.
    Narrow,
    /// The 18 bit narrowband code book of the low bit rates.
    LowRate,
    /// The 12 bit code book of the high band.
    High,
}

#[derive(Clone, Copy)]
pub(crate) enum Pitch {
    /// The pitch is not coded, the gain is derived from the open loop pitch coefficient.
    Forced,
    ThreeTap {
        gain_codebook: &'static [i8],
        gain_bits: u32,
        pitch_bits: u32,
    },
}

#[derive(Clone, Copy)]
pub(crate) enum Innovation {
    Noise,
    Split {
        subvect_size: usize,
        nb_subvect: usize,
        codebook: &'static [i8],
        bits: u32,
        signs: bool,
    },
    /// The high band folds the innovation of the low band.
    None,
}

#[derive(Clone, Copy)]
pub(crate) struct Submode {
    /// The margin of the coded open loop pitch, or -1 if the pitch is coded in full.
    pub(crate) lbr_pitch: i32,
    pub(crate) forced_pitch_gain: bool,
    pub(crate) have_subframe_gain: u32,
    pub(crate) double_codebook: bool,
    pub(crate) lsp: LspCodebook,
    pub(crate) pitch: Pitch,
    pub(crate) innovation: Innovation,
    pub(crate) comb_gain: f32,
}

const fn split(
    subvect_size: usize,
    nb_subvect: usize,
    codebook: &'static [i8],
    bits: u32,
    signs: bool,
) -> Innovation {
    Innovation::Split { subvect_size, nb_subvect, codebook, bits, signs }
}

const fn nb(
    lbr_pitch: i32,
    forced_pitch_gain: bool,
    have_subframe_gain: u32,
    double_codebook: bool,
    lsp: LspCodebook,
    pitch: Pitch,
    innovation: Innovation,
    comb_gain: f32,
) -> Option<Submode> {
    Some(Submode {
        lbr_pitch,
        forced_pitch_gain,
        have_subframe_gain,
        double_codebook,
        lsp,
        pitch,
        innovation,
        comb_gain,
    })
}

const LTP_VLBR: Pitch =
    Pitch::ThreeTap { gain_codebook: &GAIN_CDBK_LBR, gain_bits: 5, pitch_bits: 0 };
const LTP_LBR: Pitch =
    Pitch::ThreeTap { gain_codebook: &GAIN_CDBK_LBR, gain_bits: 5, pitch_bits: 7 };
const LTP_NB: Pitch = Pitch::ThreeTap { gain_codebook: &GAIN_CDBK_NB, gain_bits: 7, pitch_bits: 7 };

use LspCodebook::{High, LowRate, Narrow};

/// The submodes of the narrowband mode by their identifier.
pub(crate) static NB_SUBMODES: [Option<Submode>; 9] = [
    None,
    nb(0, true, 0, false, LowRate, Pitch::Forced, Innovation::Noise, -1.0),
    nb(0, false, 0, false, LowRate, LTP_VLBR, split(10, 4, &EXC_10_16_TABLE, 4, false), 0.6),
    nb(-1, false, 1, false, LowRate, LTP_LBR, split(10, 4, &EXC_10_32_TABLE, 5, false), 0.55),
    nb(-1, false, 1, false, LowRate, LTP_LBR, split(8, 5, &EXC_8_128_TABLE, 7, false), 0.45),
    nb(-1, false, 3, false, Narrow, LTP_NB, split(5, 8, &EXC_5_64_TABLE, 6, false), 0.25),
    nb(-1, false, 3, false, Narrow, LTP_NB, split(5, 8, &EXC_5_256_TABLE, 8, false), 0.15),
    nb(-1, false, 3, true, Narrow, LTP_NB, split(5, 8, &EXC_5_64_TABLE, 6, false), 0.05),
    nb(0, true, 0, false, LowRate, Pitch::Forced, split(20, 2, &EXC_20_32_TABLE, 5, false), 0.5),
];

const fn hb(
    have_subframe_gain: u32,
    double_codebook: bool,
    innovation: Innovation,
) -> Option<Submode> {
    Some(Submode {
        lbr_pitch: 0,
        forced_pitch_gain: false,
        have_subframe_gain,
        double_codebook,
        lsp: High,
        pitch: Pitch::Forced,
        innovation,
        comb_gain: -1.0,
    })
}

/// The submodes of the high band by their identifier.
pub(crate) static HB_SUBMODES: [Option<Submode>; 5] = [
    None,
    hb(1, false, Innovation::None),
    hb(1, false, split(10, 4, &HEXC_10_32_TABLE, 5, false)),
    hb(1, false, split(8, 5, &HEXC_TABLE, 7, true)),
    hb(1, true, split(8, 5, &HEXC_TABLE, 7, true)),
];

/// The submodes of the 32 kHz high band by their identifier.
pub(crate) static UWB_SUBMODES: [Option<Submode>; 2] = [None, HB_SUBMODES[1]];

impl LspCodebook {
    pub(crate) fn unquant(self, order: usize, bits: &mut Bits<'_>) -> Result<Vec<f32>> {
        let mut lsp = vec![0f32; order];

        match self {
            Narrow | LowRate => {
                for (i, l) in lsp.iter_mut().enumerate() {
                    *l = 0.25 * i as f32 + 0.25;
                }
                let id = bits.read(6)? as usize;
                for i in 0..10 {
                    lsp[i] += 0.0039062 * f32::from(CDBK_NB[id * 10 + i]);
                }
                let id = bits.read(6)? as usize;
                for i in 0..5 {
                    lsp[i] += 0.0019531 * f32::from(CDBK_NB_LOW1[id * 5 + i]);
                }
                if matches!(self, Narrow) {
                    let id = bits.read(6)? as usize;
                    for i in 0..5 {
                        lsp[i] += 0.00097656 * f32::from(CDBK_NB_LOW2[id * 5 + i]);
                    }
                }
                let id = bits.read(6)? as usize;
                for i in 0..5 {
                    lsp[i + 5] += 0.0019531 * f32::from(CDBK_NB_HIGH1[id * 5 + i]);
                }
                if matches!(self, Narrow) {
                    let id = bits.read(6)? as usize;
                    for i in 0..5 {
                        lsp[i + 5] += 0.00097656 * f32::from(CDBK_NB_HIGH2[id * 5 + i]);
                    }
                }
            }
            High => {
                for (i, l) in lsp.iter_mut().enumerate() {
                    *l = 0.3125 * i as f32 + 0.75;
                }
                let id = bits.read(6)? as usize;
                for i in 0..order {
                    lsp[i] += 0.0039062 * f32::from(HIGH_LSP_CDBK[id * order + i]);
                }
                let id = bits.read(6)? as usize;
                for i in 0..order {
                    lsp[i] += 0.0019531 * f32::from(HIGH_LSP_CDBK2[id * order + i]);
                }
            }
        }

        Ok(lsp)
    }
}

/// Decodes the innovation of a subframe by adding the code book vectors to `exc`.
pub(crate) fn unquant_innovation(
    innovation: Innovation,
    exc: &mut [f32],
    bits: &mut Bits<'_>,
    seed: &mut u32,
) -> Result<()> {
    match innovation {
        Innovation::Noise => {
            for e in exc.iter_mut() {
                *e = crate::dsp::rand(1.0, seed);
            }
        }
        Innovation::Split { subvect_size, nb_subvect, codebook, bits: shape_bits, signs } => {
            let mut ind = [0usize; 8];
            let mut neg = [false; 8];
            for i in 0..nb_subvect {
                if signs {
                    neg[i] = bits.read(1)? != 0;
                }
                ind[i] = bits.read(shape_bits)? as usize;
            }
            for i in 0..nb_subvect {
                let s = if neg[i] { -1.0 } else { 1.0 };
                for j in 0..subvect_size {
                    exc[subvect_size * i + j] +=
                        s * 0.03125 * f32::from(codebook[ind[i] * subvect_size + j]);
                }
            }
        }
        Innovation::None => {}
    }
    Ok(())
}
