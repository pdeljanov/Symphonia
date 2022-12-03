// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// Previous Author: Kostya Shishkov <kostya.shiskov@gmail.com>
//
// This source file includes code originally written for the NihAV
// project. With the author's permission, it has been relicensed for,
// and ported to the Symphonia project.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::vlc::{Codebook, Entry16x16};
use symphonia_core::io::ReadBitsLtr;

use crate::aac::codebooks;
use crate::aac::common::*;
use crate::aac::dsp;
use crate::common::M4AType;

use lazy_static::lazy_static;
use log::debug;

mod gain;
mod ltp;
mod pulse;
mod tns;

const ZERO_HCB: u8 = 0;
const FIRST_PAIR_HCB: u8 = 5;
const ESC_HCB: u8 = 11;
const RESERVED_HCB: u8 = 12;
const NOISE_HCB: u8 = 13;
const INTENSITY_HCB2: u8 = 14;
const INTENSITY_HCB: u8 = 15;

lazy_static! {
    /// Pre-computed table of y = x^(4/3).
    static ref POW43_TABLE: [f32; 8192] = {
        let mut pow43 = [0f32; 8192];
        for (i, pow43) in pow43.iter_mut().enumerate() {
            *pow43 = f32::powf(i as f32, 4.0 / 3.0);
        }
        pow43
    };
}

#[derive(Clone)]
pub struct IcsInfo {
    pub window_sequence: u8,
    pub prev_window_sequence: u8,
    pub window_shape: bool,
    pub prev_window_shape: bool,
    pub scale_factor_grouping: [bool; MAX_WINDOWS],
    pub group_start: [usize; MAX_WINDOWS],
    pub window_groups: usize,
    pub num_windows: usize,
    pub max_sfb: usize,
    pub long_win: bool,
    pub ltp: Option<ltp::LtpData>,
}

impl IcsInfo {
    fn new() -> Self {
        Self {
            window_sequence: 0,
            prev_window_sequence: 0,
            window_shape: false,
            prev_window_shape: false,
            scale_factor_grouping: [false; MAX_WINDOWS],
            group_start: [0; MAX_WINDOWS],
            num_windows: 0,
            window_groups: 0,
            max_sfb: 0,
            ltp: None,
            long_win: true,
        }
    }

    pub fn decode<B: ReadBitsLtr>(&mut self, bs: &mut B) -> Result<()> {
        self.prev_window_sequence = self.window_sequence;
        self.prev_window_shape = self.window_shape;

        if bs.read_bool()? {
            return decode_error("aac: ics reserved bit set");
        }

        self.window_sequence = bs.read_bits_leq32(2)? as u8;

        match self.prev_window_sequence {
            ONLY_LONG_SEQUENCE | LONG_STOP_SEQUENCE => {
                if (self.window_sequence != ONLY_LONG_SEQUENCE)
                    && (self.window_sequence != LONG_START_SEQUENCE)
                {
                    debug!("previous window is invalid");
                }
            }
            LONG_START_SEQUENCE | EIGHT_SHORT_SEQUENCE => {
                if (self.window_sequence != EIGHT_SHORT_SEQUENCE)
                    && (self.window_sequence != LONG_STOP_SEQUENCE)
                {
                    debug!("previous window is invalid");
                }
            }
            _ => {}
        };

        self.window_shape = bs.read_bool()?;
        self.window_groups = 1;

        if self.window_sequence == EIGHT_SHORT_SEQUENCE {
            self.long_win = false;
            self.num_windows = 8;
            self.max_sfb = bs.read_bits_leq32(4)? as usize;

            for i in 0..MAX_WINDOWS - 1 {
                self.scale_factor_grouping[i] = bs.read_bool()?;

                if !self.scale_factor_grouping[i] {
                    self.group_start[self.window_groups] = i + 1;
                    self.window_groups += 1;
                }
            }
        }
        else {
            self.long_win = true;
            self.num_windows = 1;
            self.max_sfb = bs.read_bits_leq32(6)? as usize;
            self.ltp = ltp::LtpData::read(bs)?;
        }
        Ok(())
    }

    pub fn copy_from_common(&mut self, other: &IcsInfo) {
        // Maintain the previous window sequence and shape.
        let prev_window_sequence = self.window_sequence;
        let prev_window_shape = self.window_shape;

        *self = other.clone();

        self.prev_window_sequence = prev_window_sequence;
        self.prev_window_shape = prev_window_shape;
    }

    fn get_group_start(&self, g: usize) -> usize {
        if g == 0 {
            0
        }
        else if g >= self.window_groups {
            if self.long_win {
                1
            }
            else {
                8
            }
        }
        else {
            self.group_start[g]
        }
    }
}

#[derive(Clone)]
pub struct Ics {
    global_gain: u8,
    pub info: IcsInfo,
    pulse: Option<pulse::Pulse>,
    tns: Option<tns::Tns>,
    gain: Option<gain::GainControl>,
    sect_cb: [[u8; MAX_SFBS]; MAX_WINDOWS],
    sect_len: [[usize; MAX_SFBS]; MAX_WINDOWS],
    sfb_cb: [[u8; MAX_SFBS]; MAX_WINDOWS],
    num_sec: [usize; MAX_WINDOWS],
    pub scales: [[f32; MAX_SFBS]; MAX_WINDOWS],
    sbinfo: GASubbandInfo,
    pub coeffs: [f32; 1024],
    delay: [f32; 1024],
}

const INTENSITY_SCALE_MIN: i16 = -155;
const NOISE_SCALE_MIN: i16 = -100;

#[inline(always)]
fn get_scale(scale: i16) -> f32 {
    2.0f32.powf(0.25 * f32::from(scale - 56))
    // 2.0f32.powf(0.25 * (f32::from(scale) - 100.0 - 56.0))
}

#[inline(always)]
fn get_intensity_scale(scale: i16) -> f32 {
    0.5f32.powf(0.25 * f32::from(scale))
}

impl Ics {
    pub fn new(sbinfo: GASubbandInfo) -> Self {
        Self {
            global_gain: 0,
            info: IcsInfo::new(),
            pulse: None,
            tns: None,
            gain: None,
            sect_cb: [[0; MAX_SFBS]; MAX_WINDOWS],
            sect_len: [[0; MAX_SFBS]; MAX_WINDOWS],
            sfb_cb: [[0; MAX_SFBS]; MAX_WINDOWS],
            scales: [[0.0; MAX_SFBS]; MAX_WINDOWS],
            num_sec: [0; MAX_WINDOWS],
            sbinfo,
            coeffs: [0.0; 1024],
            delay: [0.0; 1024],
        }
    }

    pub fn reset(&mut self) {
        self.info = IcsInfo::new();
        self.delay = [0.0; 1024];
    }

    fn decode_section_data<B: ReadBitsLtr>(&mut self, bs: &mut B) -> Result<()> {
        let sect_bits = if self.info.long_win { 5 } else { 3 };
        let sect_esc_val = (1 << sect_bits) - 1;

        for g in 0..self.info.window_groups {
            let mut k = 0;
            let mut l = 0;

            while k < self.info.max_sfb {
                self.sect_cb[g][l] = bs.read_bits_leq32(4)? as u8;
                self.sect_len[g][l] = 0;

                if self.sect_cb[g][l] == RESERVED_HCB {
                    return decode_error("aac: invalid band type");
                }

                loop {
                    let sect_len_incr = bs.read_bits_leq32(sect_bits)? as usize;

                    self.sect_len[g][l] += sect_len_incr;

                    if sect_len_incr < sect_esc_val {
                        break;
                    }
                }

                validate!(k + self.sect_len[g][l] <= self.info.max_sfb);

                for sfb in k..k + self.sect_len[g][l] {
                    self.sfb_cb[g][sfb] = self.sect_cb[g][l];
                }

                k += self.sect_len[g][l];
                l += 1;
            }

            self.num_sec[g] = l;
        }
        Ok(())
    }

    #[inline(always)]
    pub fn is_zero(&self, g: usize, sfb: usize) -> bool {
        self.sfb_cb[g][sfb] == ZERO_HCB
    }

    #[inline(always)]
    pub fn is_intensity(&self, g: usize, sfb: usize) -> bool {
        (self.sfb_cb[g][sfb] == INTENSITY_HCB) || (self.sfb_cb[g][sfb] == INTENSITY_HCB2)
    }

    #[inline(always)]
    pub fn is_noise(&self, g: usize, sfb: usize) -> bool {
        self.sfb_cb[g][sfb] == NOISE_HCB
    }

    #[inline(always)]
    pub fn get_intensity_dir(&self, g: usize, sfb: usize) -> bool {
        self.sfb_cb[g][sfb] == INTENSITY_HCB
    }

    fn decode_scale_factor_data<B: ReadBitsLtr>(&mut self, bs: &mut B) -> Result<()> {
        let mut noise_pcm_flag = true;
        let mut scf_intensity = 0i16;
        let mut scf_noise = i16::from(self.global_gain) - 90;
        let mut scf_normal = i16::from(self.global_gain);

        let scf_table = &codebooks::SCF_CODEBOOK;

        for g in 0..self.info.window_groups {
            for sfb in 0..self.info.max_sfb {
                self.scales[g][sfb] = if self.is_zero(g, sfb) {
                    0.0
                }
                else if self.is_intensity(g, sfb) {
                    scf_intensity += i16::from(bs.read_codebook(scf_table)?.0) - 60;

                    validate!(
                        (scf_intensity >= INTENSITY_SCALE_MIN)
                            && (scf_intensity < INTENSITY_SCALE_MIN + 256)
                    );

                    get_intensity_scale(scf_intensity)
                }
                else if self.is_noise(g, sfb) {
                    if noise_pcm_flag {
                        noise_pcm_flag = false;
                        scf_noise += (bs.read_bits_leq32(9)? as i16) - 256;
                    }
                    else {
                        scf_noise += i16::from(bs.read_codebook(scf_table)?.0) - 60;
                    }

                    validate!(
                        (scf_noise >= NOISE_SCALE_MIN) && (scf_noise < NOISE_SCALE_MIN + 256)
                    );

                    get_scale(scf_noise)
                }
                else {
                    scf_normal += i16::from(bs.read_codebook(scf_table)?.0) - 60;
                    validate!((scf_normal >= 0) && (scf_normal < 256));

                    get_scale(scf_normal - 100)
                }
            }
        }
        Ok(())
    }

    pub fn get_bands(&self) -> &'static [usize] {
        if self.info.long_win {
            self.sbinfo.long_bands
        }
        else {
            self.sbinfo.short_bands
        }
    }

    fn decode_spectrum<B: ReadBitsLtr>(&mut self, bs: &mut B, lcg: &mut Lcg) -> Result<()> {
        // Zero all spectral coefficients.
        self.coeffs = [0.0; 1024];

        let bands = self.get_bands();

        for g in 0..self.info.window_groups {
            let cur_w = self.info.get_group_start(g);
            let next_w = self.info.get_group_start(g + 1);
            for sfb in 0..self.info.max_sfb {
                let start = bands[sfb];
                let end = bands[sfb + 1];

                let cb_idx = self.sfb_cb[g][sfb];

                for w in cur_w..next_w {
                    let dst = &mut self.coeffs[start + w * 128..end + w * 128];

                    let scale = self.scales[g][sfb];

                    match cb_idx {
                        ZERO_HCB => (),
                        NOISE_HCB => decode_noise(lcg, scale, dst),
                        INTENSITY_HCB | INTENSITY_HCB2 => (),
                        _ => {
                            let unsigned = AAC_UNSIGNED_CODEBOOK[(cb_idx - 1) as usize];

                            let cb = &codebooks::SPECTRUM_CODEBOOKS[(cb_idx - 1) as usize];

                            if cb_idx < FIRST_PAIR_HCB {
                                decode_quads(bs, cb, unsigned, scale, dst)?;
                            }
                            else {
                                decode_pairs(
                                    bs,
                                    cb,
                                    unsigned,
                                    cb_idx == ESC_HCB,
                                    AAC_CODEBOOK_MODULO[(cb_idx - FIRST_PAIR_HCB) as usize],
                                    scale,
                                    dst,
                                )?;
                            }
                        }
                    };
                }
            }
        }
        Ok(())
    }

    pub fn decode<B: ReadBitsLtr>(
        &mut self,
        bs: &mut B,
        lcg: &mut Lcg,
        m4atype: M4AType,
        common_window: bool,
    ) -> Result<()> {
        self.global_gain = bs.read_bits_leq32(8)? as u8;

        // If a common window is used, a common ICS info was decoded previously.
        if !common_window {
            self.info.decode(bs)?;
        }

        self.decode_section_data(bs)?;

        self.decode_scale_factor_data(bs)?;

        self.pulse = pulse::Pulse::read(bs)?;

        validate!(self.pulse.is_none() || self.info.long_win);

        let is_aac_lc = m4atype == M4AType::Lc;

        self.tns = tns::Tns::read(bs, &self.info, is_aac_lc)?;

        match m4atype {
            M4AType::Ssr => self.gain = gain::GainControl::read(bs)?,
            _ => {
                let gain_control_data_present = bs.read_bool()?;
                validate!(!gain_control_data_present);
            }
        }

        self.decode_spectrum(bs, lcg)?;
        Ok(())
    }

    pub fn synth_channel(&mut self, dsp: &mut dsp::Dsp, rate_idx: usize, dst: &mut [f32]) {
        let bands = self.get_bands();

        if let Some(pulse) = &self.pulse {
            pulse.synth(bands, &self.scales, &mut self.coeffs);
        }

        if let Some(tns) = &self.tns {
            tns.synth(&self.info, bands, rate_idx, &mut self.coeffs);
        }

        dsp.synth(
            &self.coeffs,
            &mut self.delay,
            self.info.window_sequence,
            self.info.window_shape,
            self.info.prev_window_shape,
            dst,
        );
    }
}

/// Perceptual noise substitution decode step. Section 4.6.13.3.
fn decode_noise(lcg: &mut Lcg, sf: f32, dst: &mut [f32]) {
    let mut energy = 0.0;

    for spec in dst.iter_mut() {
        // The random number generator outputs i32, but the largest signed
        // integer that can convert to f32 is i16.
        *spec = f32::from((lcg.next() >> 16) as i16);
        energy += *spec * *spec;
    }

    let scale = sf / energy.sqrt();

    for spec in dst.iter_mut() {
        *spec *= scale;
    }
}

fn decode_quads<B: ReadBitsLtr>(
    bs: &mut B,
    cb: &Codebook<Entry16x16>,
    unsigned: bool,
    scale: f32,
    dst: &mut [f32],
) -> Result<()> {
    let pow43_table: &[f32; 8192] = &POW43_TABLE;

    for out in dst.chunks_mut(4) {
        let cw = bs.read_codebook(cb)?.0 as usize;
        if unsigned {
            for (out, &quad) in out.iter_mut().zip(&AAC_QUADS[cw]) {
                if quad != 0 {
                    *out = if bs.read_bool()? {
                        -pow43_table[quad as usize] * scale
                    }
                    else {
                        pow43_table[quad as usize] * scale
                    }
                }
            }
        }
        else {
            for (out, &quad) in out.iter_mut().zip(&AAC_QUADS[cw]) {
                let val = quad - 1;

                *out = if val < 0 {
                    -pow43_table[-val as usize] * scale
                }
                else {
                    pow43_table[val as usize] * scale
                }
            }
        }
    }
    Ok(())
}

fn decode_pairs<B: ReadBitsLtr>(
    bs: &mut B,
    cb: &Codebook<Entry16x16>,
    unsigned: bool,
    escape: bool,
    modulo: u16,
    scale: f32,
    dst: &mut [f32],
) -> Result<()> {
    let pow43_table: &[f32; 8192] = &POW43_TABLE;

    for out in dst.chunks_mut(2) {
        let cw = bs.read_codebook(cb)?.0;

        let mut x = (cw / modulo) as i16;
        let mut y = (cw % modulo) as i16;

        if unsigned {
            if x != 0 && bs.read_bool()? {
                x = -x;
            }
            if y != 0 && bs.read_bool()? {
                y = -y;
            }
        }
        else {
            x -= (modulo >> 1) as i16;
            y -= (modulo >> 1) as i16;
        }

        if escape {
            if (x == 16) || (x == -16) {
                x = read_escape(bs, x.is_positive())?;
            }
            if (y == 16) || (y == -16) {
                y = read_escape(bs, y.is_positive())?;
            }
        }

        out[0] = if x < 0 { -pow43_table[-x as usize] } else { pow43_table[x as usize] } * scale;
        out[1] = if y < 0 { -pow43_table[-y as usize] } else { pow43_table[y as usize] } * scale;
    }
    Ok(())
}

fn read_escape<B: ReadBitsLtr>(bs: &mut B, is_pos: bool) -> Result<i16> {
    let n = bs.read_unary_ones()?;

    validate!(n < 9);

    // The escape word is added to 2^(n + 4) to yield the unsigned value.
    let word = (1 << (n + 4)) + bs.read_bits_leq32(n + 4)? as i16;

    if is_pos {
        Ok(word)
    }
    else {
        Ok(-word)
    }
}

const AAC_UNSIGNED_CODEBOOK: [bool; 11] =
    [false, false, true, true, false, false, true, true, true, true, true];

const AAC_CODEBOOK_MODULO: [u16; 7] = [9, 9, 8, 8, 13, 13, 17];

const AAC_QUADS: [[i8; 4]; 81] = [
    [0, 0, 0, 0],
    [0, 0, 0, 1],
    [0, 0, 0, 2],
    [0, 0, 1, 0],
    [0, 0, 1, 1],
    [0, 0, 1, 2],
    [0, 0, 2, 0],
    [0, 0, 2, 1],
    [0, 0, 2, 2],
    [0, 1, 0, 0],
    [0, 1, 0, 1],
    [0, 1, 0, 2],
    [0, 1, 1, 0],
    [0, 1, 1, 1],
    [0, 1, 1, 2],
    [0, 1, 2, 0],
    [0, 1, 2, 1],
    [0, 1, 2, 2],
    [0, 2, 0, 0],
    [0, 2, 0, 1],
    [0, 2, 0, 2],
    [0, 2, 1, 0],
    [0, 2, 1, 1],
    [0, 2, 1, 2],
    [0, 2, 2, 0],
    [0, 2, 2, 1],
    [0, 2, 2, 2],
    [1, 0, 0, 0],
    [1, 0, 0, 1],
    [1, 0, 0, 2],
    [1, 0, 1, 0],
    [1, 0, 1, 1],
    [1, 0, 1, 2],
    [1, 0, 2, 0],
    [1, 0, 2, 1],
    [1, 0, 2, 2],
    [1, 1, 0, 0],
    [1, 1, 0, 1],
    [1, 1, 0, 2],
    [1, 1, 1, 0],
    [1, 1, 1, 1],
    [1, 1, 1, 2],
    [1, 1, 2, 0],
    [1, 1, 2, 1],
    [1, 1, 2, 2],
    [1, 2, 0, 0],
    [1, 2, 0, 1],
    [1, 2, 0, 2],
    [1, 2, 1, 0],
    [1, 2, 1, 1],
    [1, 2, 1, 2],
    [1, 2, 2, 0],
    [1, 2, 2, 1],
    [1, 2, 2, 2],
    [2, 0, 0, 0],
    [2, 0, 0, 1],
    [2, 0, 0, 2],
    [2, 0, 1, 0],
    [2, 0, 1, 1],
    [2, 0, 1, 2],
    [2, 0, 2, 0],
    [2, 0, 2, 1],
    [2, 0, 2, 2],
    [2, 1, 0, 0],
    [2, 1, 0, 1],
    [2, 1, 0, 2],
    [2, 1, 1, 0],
    [2, 1, 1, 1],
    [2, 1, 1, 2],
    [2, 1, 2, 0],
    [2, 1, 2, 1],
    [2, 1, 2, 2],
    [2, 2, 0, 0],
    [2, 2, 0, 1],
    [2, 2, 0, 2],
    [2, 2, 1, 0],
    [2, 2, 1, 1],
    [2, 2, 1, 2],
    [2, 2, 2, 0],
    [2, 2, 2, 1],
    [2, 2, 2, 2],
];
