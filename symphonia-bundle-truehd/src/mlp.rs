// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The Meridian Lossless Packing (MLP) and Dolby TrueHD bitstream decoder core.
//!
//! Only the first substream of an access unit is decoded, which is the stereo presentation of a
//! TrueHD stream and the whole of an MLP stream. The syntax follows the open Apache-2.0
//! `truehd` crate (https://github.com/truehdd/truehdd).

use crate::bitreader::{BitReader, BitResult};

pub(crate) const SYNC_FBA: u32 = 0xf872_6fba;
pub(crate) const SYNC_FBB: u32 = 0xf872_6fbb;

/// The size of the header that precedes every access unit.
pub(crate) const AU_HEADER_LEN: usize = 4;

const MAX_AU_FRAMES: usize = 160;
const MAX_CHANNELS: usize = 16;
const RESTART_SYNC: u32 = 0x31ea;
const SAMPLE_LIMIT: i64 = 1 << 23;

const FLAG_HEAVY_DRC: u16 = 0x2000;

/// Guard bits, which tell whether the header of a block may change a field.
const GUARD_GUARDS: u8 = 0x01;
const GUARD_COEFFS_A: u8 = 0x08;
const GUARD_COEFFS_B: u8 = 0x04;
const GUARD_HUFF_OFFSET: u8 = 0x02;
const GUARD_QUANT_STEP: u8 = 0x10;
const GUARD_OUTPUT_SHIFT: u8 = 0x20;
const GUARD_MATRIXING: u8 = 0x40;
const GUARD_BLOCK_SIZE: u8 = 0x80;

type Row = [i32; MAX_CHANNELS];

/// Returns the length in bytes of the access unit that starts at the beginning of `data`.
pub(crate) fn access_unit_len(header: &[u8]) -> Option<usize> {
    let word = u16::from_be_bytes([*header.first()?, *header.get(1)?]);
    Some(usize::from(word & 0xfff) * 2)
}

/// Returns the format sync word if the access unit carries a major sync.
pub(crate) fn major_sync(au: &[u8]) -> Option<u32> {
    let word = u32::from_be_bytes(au.get(AU_HEADER_LEN..AU_HEADER_LEN + 4)?.try_into().ok()?);
    (word == SYNC_FBA || word == SYNC_FBB).then_some(word)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct StreamInfo {
    pub(crate) is_fba: bool,
    pub(crate) sample_rate: u32,
    pub(crate) frames_per_au: usize,
}

struct Substream {
    min_chan: usize,
    max_chan: usize,
    max_matrix_chan: usize,
    dither_shift: u32,
    dither_seed: u32,
    error_protect: bool,
    ch_assign: [usize; MAX_CHANNELS],

    guards: u8,
    block_size: usize,
    primitive_matrices: usize,
    matrix_ch: [usize; MAX_CHANNELS],
    frac_bits: [u32; MAX_CHANNELS],
    lsb_bypass: [bool; MAX_CHANNELS],
    m_coeff: [[i32; MAX_CHANNELS]; MAX_CHANNELS],
    huff_offset: [i32; MAX_CHANNELS],
    huff_type: [u32; MAX_CHANNELS],
    huff_lsbs: [u32; MAX_CHANNELS],
    quant_step: [u32; MAX_CHANNELS],
    output_shift: [i32; MAX_CHANNELS],
    order: [[usize; MAX_CHANNELS]; 2],
    coeff_q: [[u32; MAX_CHANNELS]; 2],
    coeff: [[[i32; 8]; MAX_CHANNELS]; 2],
    state: [[[i32; 8]; MAX_CHANNELS]; 2],
}

impl Substream {
    fn new(
        min_chan: usize,
        max_chan: usize,
        max_matrix_chan: usize,
        dither_shift: u32,
        dither_seed: u32,
        error_protect: bool,
        ch_assign: [usize; MAX_CHANNELS],
    ) -> Self {
        Substream {
            min_chan,
            max_chan,
            max_matrix_chan,
            dither_shift,
            dither_seed,
            error_protect,
            ch_assign,
            guards: 0xff,
            block_size: 8,
            primitive_matrices: 0,
            matrix_ch: [0; MAX_CHANNELS],
            frac_bits: [0; MAX_CHANNELS],
            lsb_bypass: [false; MAX_CHANNELS],
            m_coeff: [[0; MAX_CHANNELS]; MAX_CHANNELS],
            huff_offset: [0; MAX_CHANNELS],
            huff_type: [0; MAX_CHANNELS],
            huff_lsbs: [0; MAX_CHANNELS],
            quant_step: [0; MAX_CHANNELS],
            output_shift: [0; MAX_CHANNELS],
            order: [[0; MAX_CHANNELS]; 2],
            coeff_q: [[0; MAX_CHANNELS]; 2],
            coeff: [[[0; 8]; MAX_CHANNELS]; 2],
            state: [[[0; 8]; MAX_CHANNELS]; 2],
        }
    }
}

pub(crate) struct MlpCore {
    info: Option<StreamInfo>,
    flags: u16,
    substreams: usize,
    channels: usize,
    ss: Option<Substream>,
    rematrix: Box<[Row; MAX_AU_FRAMES]>,
}

impl Default for MlpCore {
    fn default() -> Self {
        MlpCore {
            info: None,
            flags: 0,
            substreams: 0,
            channels: 0,
            ss: None,
            rematrix: Box::new([[0; MAX_CHANNELS]; MAX_AU_FRAMES]),
        }
    }
}

impl MlpCore {
    pub(crate) fn reset(&mut self) {
        *self = MlpCore::default();
    }

    pub(crate) fn info(&self) -> Option<StreamInfo> {
        self.info
    }

    /// The number of channels that the last restart header declared.
    pub(crate) fn channels(&self) -> usize {
        self.channels
    }

    /// Decodes an access unit, including its header, and appends its frames to `out`.
    pub(crate) fn decode(&mut self, au: &[u8], out: &mut Vec<Row>) -> BitResult<()> {
        let result = self.decode_inner(au, out);
        if result.is_err() {
            self.ss = None;
        }
        result
    }

    fn decode_inner(&mut self, au: &[u8], out: &mut Vec<Row>) -> BitResult<()> {
        let mut r = BitReader::new(au);
        r.skip(AU_HEADER_LEN * 8)?;

        if major_sync(au).is_some() {
            self.read_major_sync(&mut r)?;
        }

        let Some(info) = self.info
        else {
            return Err("truehd: no major sync yet");
        };

        for _ in 0..self.substreams {
            let extra_word = r.bit()?;
            r.skip(2 + 1 + 12)?;
            if extra_word {
                r.skip(16)?;
            }
        }

        let mut decoded = 0;
        loop {
            self.read_block(&mut r, &info, decoded, out)?;
            decoded += self.ss.as_ref().map_or(0, |s| s.block_size);
            if r.bit()? {
                break;
            }
        }

        Ok(())
    }

    fn read_major_sync(&mut self, r: &mut BitReader<'_>) -> BitResult<()> {
        let sync = r.read(32)?;
        let (is_fba, rate_code) = if sync == SYNC_FBA {
            let code = r.read(4)?;
            r.skip(28)?;
            (true, code)
        }
        else {
            r.skip(8)?;
            let code = r.read(4)?;
            r.skip(20)?;
            (false, code)
        };

        let sample_rate = match rate_code {
            0..=2 => 48000 << rate_code,
            8..=10 => 44100 << (rate_code - 8),
            _ => return Err("truehd: invalid sampling frequency"),
        };

        r.skip(16)?;
        let flags = r.read(16)? as u16;
        r.skip(32)?;
        let substreams = r.read(4)? as usize;
        if !(1..=4).contains(&substreams) {
            return Err("truehd: invalid substream count");
        }
        r.skip(12)?;

        r.skip(63)?;
        let extra_channel_meaning = r.bit()?;
        if is_fba && extra_channel_meaning {
            let start = r.pos();
            let len = r.read(4)? as usize;
            r.seek(start + (len + 1) * 16)?;
            r.align16();
        }
        r.skip(16)?;

        let frames_per_au = (sample_rate / 44100) as usize * 40;
        let info = StreamInfo { is_fba, sample_rate, frames_per_au };

        let changed = self.info.is_some_and(|i| i.sample_rate != sample_rate || i.is_fba != is_fba);
        if changed {
            self.ss = None;
        }

        self.info = Some(info);
        self.flags = flags;
        self.substreams = substreams;
        Ok(())
    }

    fn read_block(
        &mut self,
        r: &mut BitReader<'_>,
        info: &StreamInfo,
        decoded: usize,
        out: &mut Vec<Row>,
    ) -> BitResult<()> {
        if r.bit()? {
            if r.bit()? {
                self.read_restart_header(r, info)?;
            }
            let ss = self.ss.as_mut().ok_or("truehd: block without a restart header")?;
            read_block_header(r, ss, info)?;
        }

        let ss = self.ss.as_mut().ok_or("truehd: block without a restart header")?;

        if ss.error_protect {
            r.skip(16)?;
        }

        let block_size = ss.block_size;
        if decoded + block_size > info.frames_per_au {
            return Err("truehd: block exceeds the access unit");
        }

        for ch in ss.min_chan..=ss.max_chan {
            if ss.huff_lsbs[ch] < ss.quant_step[ch] {
                return Err("truehd: quantiser step is larger than the lsb count");
            }
        }

        let mut rows = [[0i32; MAX_CHANNELS]; MAX_AU_FRAMES];
        let mut bypass = [[0i32; MAX_CHANNELS]; MAX_AU_FRAMES];

        for blki in 0..block_size {
            for pmi in 0..ss.primitive_matrices {
                if ss.lsb_bypass[pmi] {
                    bypass[blki][pmi] = r.read(1)? as i32;
                }
            }
            for ch in ss.min_chan..=ss.max_chan {
                rows[blki][ch] = read_sample(r, ss, ch)?;
            }
        }

        if ss.error_protect {
            r.skip(8)?;
        }

        self.decode_block(&rows, &bypass, decoded, out)
    }

    fn read_restart_header(&mut self, r: &mut BitReader<'_>, info: &StreamInfo) -> BitResult<()> {
        if r.read(14)? != RESTART_SYNC {
            return Err("truehd: unsupported restart header");
        }
        r.skip(16)?;
        let min_chan = r.read(4)? as usize;
        let max_chan = r.read(4)? as usize;
        let max_matrix_chan = r.read(4)? as usize;
        let dither_shift = r.read(4)?;
        let dither_seed = r.read(23)?;
        r.skip(4 + 5 + 5 + 5)?;
        let error_protect = r.bit()?;
        r.skip(8)?;

        if min_chan > max_chan + 1 {
            return Err("truehd: reversed channel range");
        }
        if max_matrix_chan > 13 {
            return Err("truehd: too many matrix channels");
        }

        r.skip(1 + 2)?;
        let heavy_drc = if self.flags & FLAG_HEAVY_DRC != 0 {
            r.bit()?
        }
        else {
            r.skip(1)?;
            false
        };
        if !heavy_drc || info.is_fba {
            r.skip(12)?;
        }

        let mut ch_assign = [0; MAX_CHANNELS];
        for slot in ch_assign.iter_mut().take(max_matrix_chan + 1) {
            let value = r.read(6)? as usize;
            if value > max_matrix_chan {
                return Err("truehd: channel assignment out of range");
            }
            *slot = value;
        }
        r.skip(8)?;

        self.channels = max_matrix_chan + 1;
        self.ss = Some(Substream::new(
            min_chan,
            max_chan,
            max_matrix_chan,
            dither_shift,
            dither_seed,
            error_protect,
            ch_assign,
        ));
        Ok(())
    }

    fn decode_block(
        &mut self,
        rows: &[Row; MAX_AU_FRAMES],
        bypass: &[Row; MAX_AU_FRAMES],
        base: usize,
        out: &mut Vec<Row>,
    ) -> BitResult<()> {
        let ss = self.ss.as_mut().ok_or("truehd: missing substream state")?;
        let block_size = ss.block_size;

        let mut recorrelated = [[0i32; MAX_CHANNELS]; MAX_AU_FRAMES];

        for ch in ss.min_chan..=ss.max_chan {
            let mut hist = [[0i32; 168]; 2];
            hist[0][160..].copy_from_slice(&ss.state[0][ch]);
            hist[1][160..].copy_from_slice(&ss.state[1][ch]);

            let fir_order = ss.order[0][ch];
            let iir_order = ss.order[1][ch];
            let shift = ss.coeff_q[0][ch];
            let mask = !((1i64 << ss.quant_step[ch]) - 1);

            for blki in 0..block_size {
                let top = 160 - blki;
                let mut acc = 0i64;
                for o in 0..fir_order {
                    acc += i64::from(ss.coeff[0][ch][o]) * i64::from(hist[0][top + o]);
                }
                for o in 0..iir_order {
                    acc += i64::from(ss.coeff[1][ch][o]) * i64::from(hist[1][top + o]);
                }

                let pred = acc >> shift;
                let fir_state = i64::from(rows[blki][ch]) + (pred & mask);
                let iir_state = fir_state - pred;

                if !(-SAMPLE_LIMIT..SAMPLE_LIMIT).contains(&fir_state)
                    || !(-SAMPLE_LIMIT..SAMPLE_LIMIT).contains(&iir_state)
                {
                    return Err("truehd: recorrelator output out of range");
                }

                hist[0][159 - blki] = fir_state as i32;
                hist[1][159 - blki] = iir_state as i32;
                recorrelated[blki][ch] = fir_state as i32;
            }

            let from = 160 - block_size;
            ss.state[0][ch].copy_from_slice(&hist[0][from..from + 8]);
            ss.state[1][ch].copy_from_slice(&hist[1][from..from + 8]);
        }

        let mmc = ss.max_matrix_chan;

        for blki in 0..block_size {
            let row = &mut self.rematrix[base + blki];
            row[ss.min_chan..=ss.max_chan]
                .copy_from_slice(&recorrelated[blki][ss.min_chan..=ss.max_chan]);

            let seed = ss.dither_seed;
            let seed_shr7 = seed >> 7;
            row[mmc + 1] = i32::from((seed >> 15) as u8 as i8) << ss.dither_shift;
            row[mmc + 2] = i32::from(seed_shr7 as u8 as i8) << ss.dither_shift;
            ss.dither_seed = (seed_shr7 ^ (seed_shr7 << 5) ^ (seed << 16)) & 0x7f_ffff;

            for pmi in 0..ss.primitive_matrices {
                let mut acc = 0i64;
                for ch in 0..=mmc + 2 {
                    acc += i64::from(row[ch]) * i64::from(ss.m_coeff[pmi][ch]);
                }
                let target = ss.matrix_ch[pmi];
                let keep = !((1i32 << ss.quant_step[target]) - 1);
                row[target] = (((acc >> 18) as i32) & keep).wrapping_add(bypass[blki][pmi]);
            }

            let mut frame = [0i32; MAX_CHANNELS];
            for ch in 0..=mmc {
                let shift = ss.output_shift[ch];
                let value = row[ch];
                frame[ss.ch_assign[ch]] =
                    if shift < 0 { value >> -shift } else { value.wrapping_shl(shift as u32) };
            }
            out.push(frame);
        }

        Ok(())
    }
}

fn read_block_header(
    r: &mut BitReader<'_>,
    ss: &mut Substream,
    info: &StreamInfo,
) -> BitResult<()> {
    if ss.guards & GUARD_GUARDS != 0 && r.bit()? {
        ss.guards = r.read(8)? as u8;
    }
    let guards = ss.guards;

    if guards & GUARD_BLOCK_SIZE != 0 && r.bit()? {
        let size = r.read(9)? as usize;
        if !(8..=MAX_AU_FRAMES).contains(&size) || size > info.frames_per_au {
            return Err("truehd: invalid block size");
        }
        ss.block_size = size;
    }

    if guards & GUARD_MATRIXING != 0 && r.bit()? {
        read_matrixing(r, ss)?;
    }

    if guards & GUARD_OUTPUT_SHIFT != 0 && r.bit()? {
        for ch in 0..=ss.max_matrix_chan {
            ss.output_shift[ch] = r.read_signed(4)?;
        }
    }

    if guards & GUARD_QUANT_STEP != 0 && r.bit()? {
        for ch in 0..=ss.max_chan {
            ss.quant_step[ch] = r.read(4)?;
        }
    }

    for ch in ss.min_chan..=ss.max_chan {
        if r.bit()? {
            read_channel_params(r, ss, ch, guards)?;
        }
    }

    Ok(())
}

fn read_matrixing(r: &mut BitReader<'_>, ss: &mut Substream) -> BitResult<()> {
    ss.primitive_matrices = r.read(4)? as usize;

    for pmi in 0..ss.primitive_matrices {
        ss.matrix_ch[pmi] = r.read(4)? as usize;
        let frac_bits = r.read(4)?;
        if frac_bits > 14 {
            return Err("truehd: too many fractional bits in a matrix");
        }
        ss.frac_bits[pmi] = frac_bits;
        ss.lsb_bypass[pmi] = r.bit()?;

        for ch in 0..=ss.max_matrix_chan + 2 {
            ss.m_coeff[pmi][ch] =
                if r.bit()? { r.read_signed(frac_bits + 2)? << (18 - frac_bits) } else { 0 };
        }
    }

    Ok(())
}

fn read_channel_params(
    r: &mut BitReader<'_>,
    ss: &mut Substream,
    ch: usize,
    guards: u8,
) -> BitResult<()> {
    if guards & GUARD_COEFFS_A != 0 && r.bit()? {
        read_filter(r, ss, 0, ch)?;
    }
    if guards & GUARD_COEFFS_B != 0 && r.bit()? {
        read_filter(r, ss, 1, ch)?;
    }
    if guards & GUARD_HUFF_OFFSET != 0 && r.bit()? {
        ss.huff_offset[ch] = r.read_signed(15)?;
    }

    ss.huff_type[ch] = r.read(2)?;
    ss.huff_lsbs[ch] = r.read(5)?;
    if ss.huff_lsbs[ch] > 24 {
        return Err("truehd: too many lsbs");
    }

    if ss.order[0][ch] == 0 && ss.order[1][ch] != 0 {
        ss.coeff_q[0][ch] = ss.coeff_q[1][ch];
    }

    Ok(())
}

fn read_filter(r: &mut BitReader<'_>, ss: &mut Substream, kind: usize, ch: usize) -> BitResult<()> {
    let order = r.read(4)? as usize;
    if order > if kind == 0 { 8 } else { 4 } {
        return Err("truehd: filter order too high");
    }
    ss.order[kind][ch] = order;
    if order == 0 {
        return Ok(());
    }

    let coeff_q = r.read(4)?;
    let coeff_bits = r.read(5)?;
    let coeff_shift = r.read(3)?;
    if coeff_q < 8 || coeff_bits == 0 || coeff_bits + coeff_shift > 16 {
        return Err("truehd: invalid filter coefficient format");
    }
    ss.coeff_q[kind][ch] = coeff_q;

    for i in 0..order {
        let coeff = r.read_signed(coeff_bits)? << coeff_shift;
        if coeff == -32768 {
            return Err("truehd: invalid filter coefficient");
        }
        ss.coeff[kind][ch][i] = coeff;
    }

    if r.bit()? {
        if kind == 0 {
            return Err("truehd: new states are not allowed in the FIR filter");
        }
        let state_bits = r.read(4)?;
        let state_shift = r.read(4)?;
        for i in 0..order {
            ss.state[kind][ch][i] = r.read_signed(state_bits)?.wrapping_shl(state_shift);
        }
    }

    Ok(())
}

fn read_huffman(r: &mut BitReader<'_>, tree: u32) -> BitResult<i32> {
    if r.bit()? {
        return Ok(match tree {
            1 => r.read(2)? as i32,
            2 => r.read(1)? as i32,
            _ => 0,
        });
    }

    let negative = !r.bit()?;
    let mut zeros = 0;
    let mut terminated = false;
    while zeros < 6 {
        if r.bit()? {
            terminated = true;
            break;
        }
        zeros += 1;
    }

    if !terminated {
        r.skip(1)?;
        return Ok(if negative { -7 } else { [10, 8, 7][tree as usize - 1] });
    }

    let magnitude = zeros + 1;
    Ok(if negative { -magnitude } else { [3, 1, 0][tree as usize - 1] + magnitude })
}

fn read_sample(r: &mut BitReader<'_>, ss: &Substream, ch: usize) -> BitResult<i32> {
    let tree = ss.huff_type[ch];
    let lsb_bits = ss.huff_lsbs[ch] - ss.quant_step[ch];

    let value = if tree != 0 {
        let code = i64::from(read_huffman(r, tree)?);
        let lsbs = i64::from(r.read(lsb_bits)?);
        let shift = lsb_bits as i32 + 2 - tree as i32;
        lsbs + (code << lsb_bits) - if shift < 0 { 0 } else { 1 << shift }
    }
    else {
        let lsbs = i64::from(r.read(lsb_bits)?);
        lsbs - if lsb_bits > 0 { 1 << (lsb_bits - 1) } else { 0 }
    };

    let value = (value + i64::from(ss.huff_offset[ch])) << ss.quant_step[ch];
    if !(-SAMPLE_LIMIT..SAMPLE_LIMIT).contains(&value) {
        return Err("truehd: sample out of range");
    }

    Ok(value as i32)
}
