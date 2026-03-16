// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SBR bitstream parsing.
//!
//! Reads SBR extension data from the AAC bitstream, including the time-frequency
//! grid, envelope/noise floor scalefactors, inverse filtering modes, and
//! sinusoidal coding flags.

use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::vlc::*;
use symphonia_core::io::ReadBitsLtr;

use lazy_static::lazy_static;

use super::ps::PsCommonContext;
use super::tables;
use super::{FrameClass, QuantMode, SbrChannel, SbrHeader, SbrState, NUM_ENVELOPES, SBR_BANDS};

/// SBR Huffman codebooks for envelope and noise scalefactor decoding.
pub struct SbrCodebooks {
    env_1_5db_f: Codebook<Entry16x16>,
    env_1_5db_t: Codebook<Entry16x16>,
    env_bal_1_5db_f: Codebook<Entry16x16>,
    env_bal_1_5db_t: Codebook<Entry16x16>,
    env_3_0db_f: Codebook<Entry16x16>,
    env_3_0db_t: Codebook<Entry16x16>,
    env_bal_3_0db_f: Codebook<Entry16x16>,
    env_bal_3_0db_t: Codebook<Entry16x16>,
    noise_3_0db_t: Codebook<Entry16x16>,
    noise_bal_3_0db_t: Codebook<Entry16x16>,
}

fn build_codebook(codes: &[u32], lens: &[u8]) -> Codebook<Entry16x16> {
    assert_eq!(codes.len(), lens.len());
    let n = codes.len() as u16;
    let indices: Vec<u16> = (0..n).collect();
    let mut builder = CodebookBuilder::new(BitOrder::Verbatim);
    builder.bits_per_read(8);
    builder.make(codes, lens, &indices).unwrap()
}

impl SbrCodebooks {
    fn new() -> Self {
        Self {
            env_1_5db_f: build_codebook(&tables::ENV_1_5DB_F_BITS, &tables::ENV_1_5DB_F_LENS),
            env_1_5db_t: build_codebook(&tables::ENV_1_5DB_T_BITS, &tables::ENV_1_5DB_T_LENS),
            env_bal_1_5db_f: build_codebook(
                &tables::ENV_BAL_1_5DB_F_BITS,
                &tables::ENV_BAL_1_5DB_F_LENS,
            ),
            env_bal_1_5db_t: build_codebook(
                &tables::ENV_BAL_1_5DB_T_BITS,
                &tables::ENV_BAL_1_5DB_T_LENS,
            ),
            env_3_0db_f: build_codebook(&tables::ENV_3_0DB_F_BITS, &tables::ENV_3_0DB_F_LENS),
            env_3_0db_t: build_codebook(&tables::ENV_3_0DB_T_BITS, &tables::ENV_3_0DB_T_LENS),
            env_bal_3_0db_f: build_codebook(
                &tables::ENV_BAL_3_0DB_F_BITS,
                &tables::ENV_BAL_3_0DB_F_LENS,
            ),
            env_bal_3_0db_t: build_codebook(
                &tables::ENV_BAL_3_0DB_T_BITS,
                &tables::ENV_BAL_3_0DB_T_LENS,
            ),
            noise_3_0db_t: build_codebook(&tables::NOISE_3_0DB_T_BITS, &tables::NOISE_3_0DB_T_LENS),
            noise_bal_3_0db_t: build_codebook(
                &tables::NOISE_BAL_3_0DB_T_BITS,
                &tables::NOISE_BAL_3_0DB_T_LENS,
            ),
        }
    }
}

lazy_static! {
    pub static ref SBR_CODEBOOKS: SbrCodebooks = SbrCodebooks::new();
}

/// Read an SBR header from the bitstream.
///
/// Returns `Some(SbrHeader)` on success, `None` if parsing fails.
pub fn sbr_read_header<B: ReadBitsLtr>(bs: &mut B) -> Result<SbrHeader> {
    let mut hdr = SbrHeader::new();
    hdr.amp_res = bs.read_bool()?;
    hdr.start_freq = bs.read_bits_leq32(4)? as usize;
    hdr.stop_freq = bs.read_bits_leq32(4)? as usize;
    hdr.xover_band = bs.read_bits_leq32(3)? as usize;
    // Reserved bits
    bs.ignore_bits(2)?;
    let header_extra_1 = bs.read_bool()?;
    let header_extra_2 = bs.read_bool()?;
    if header_extra_1 {
        hdr.freq_scale = bs.read_bits_leq32(2)? as u8;
        hdr.alter_scale = bs.read_bool()?;
        hdr.noise_bands = bs.read_bits_leq32(2)? as u8;
    }
    if header_extra_2 {
        hdr.limiter_bands = bs.read_bits_leq32(2)? as u8;
        hdr.limiter_gains = bs.read_bits_leq32(2)? as u8;
        hdr.interpol_freq = bs.read_bool()?;
        hdr.smoothing_mode = bs.read_bool()?;
    }
    Ok(hdr)
}

/// Read a Huffman-coded delta value from the bitstream.
/// The codebook returns an index; subtracting `offset` gives the signed delta.
#[inline]
fn read_sbr_huff<B: ReadBitsLtr>(bs: &mut B, cb: &Codebook<Entry16x16>, offset: i8) -> Result<i8> {
    let (idx, _) = bs.read_codebook(cb)?;
    Ok(idx as i8 - offset)
}

/// Read SBR single channel element data.
pub fn sbr_read_sce<B: ReadBitsLtr>(
    bs: &mut B,
    orig_amp_res: bool,
    state: &SbrState,
    ch: &mut SbrChannel,
    ps: Option<&mut PsCommonContext>,
    num_time_slots: usize,
) -> Result<()> {
    let cbs = &*SBR_CODEBOOKS;
    ch.qmode = QuantMode::Single;

    // Reserved bits
    if bs.read_bool()? {
        bs.ignore_bits(4)?;
    }

    read_grid(bs, ch, num_time_slots)?;
    read_dtdf(bs, ch)?;
    read_invf(bs, ch, state)?;
    ch.set_amp_res(orig_amp_res);
    read_envelope(bs, ch, false, cbs, state)?;
    read_noise(bs, ch, false, cbs, state)?;
    read_sinusoidal_coding(bs, ch, state)?;
    read_extensions(bs, ps, num_time_slots)?;

    Ok(())
}

/// Read SBR channel pair element data.
pub fn sbr_read_cpe<B: ReadBitsLtr>(
    bs: &mut B,
    orig_amp_res: bool,
    state: &SbrState,
    ch: &mut [SbrChannel; 2],
    ps: Option<&mut PsCommonContext>,
    num_time_slots: usize,
) -> Result<()> {
    let cbs = &*SBR_CODEBOOKS;

    // Reserved bits
    if bs.read_bool()? {
        bs.ignore_bits(4)?;
        bs.ignore_bits(4)?;
    }

    let coupling = bs.read_bool()?;

    if coupling {
        ch[0].qmode = QuantMode::Left;
        ch[1].qmode = QuantMode::Right;

        read_grid(bs, &mut ch[0], num_time_slots)?;

        // Copy grid from ch[0] to ch[1].
        ch[1].fclass = ch[0].fclass;
        ch[1].num_env = ch[0].num_env;
        ch[1].env_border = ch[0].env_border;
        ch[1].freq_res = ch[0].freq_res;
        ch[1].pointer = ch[0].pointer;
        ch[1].num_noise = ch[0].num_noise;
        ch[1].noise_env_border = ch[0].noise_env_border;

        read_dtdf(bs, &mut ch[0])?;
        read_dtdf(bs, &mut ch[1])?;
        read_invf(bs, &mut ch[0], state)?;
        ch[1].invf_mode = ch[0].invf_mode;

        ch[0].set_amp_res(orig_amp_res);
        read_envelope(bs, &mut ch[0], false, cbs, state)?;
        read_noise(bs, &mut ch[0], false, cbs, state)?;
        ch[1].set_amp_res(orig_amp_res);
        read_envelope(bs, &mut ch[1], true, cbs, state)?;
        read_noise(bs, &mut ch[1], true, cbs, state)?;

        // Exchange envelope/noise data for coupling.
        ch[0].data_env2 = ch[1].data_env;
        ch[0].data_noise2 = ch[1].data_noise;
        ch[1].data_env2 = ch[0].data_env;
        ch[1].data_noise2 = ch[0].data_noise;
    }
    else {
        ch[0].qmode = QuantMode::Single;
        ch[1].qmode = QuantMode::Single;

        read_grid(bs, &mut ch[0], num_time_slots)?;
        read_grid(bs, &mut ch[1], num_time_slots)?;
        read_dtdf(bs, &mut ch[0])?;
        read_dtdf(bs, &mut ch[1])?;
        read_invf(bs, &mut ch[0], state)?;
        read_invf(bs, &mut ch[1], state)?;

        ch[0].set_amp_res(orig_amp_res);
        read_envelope(bs, &mut ch[0], false, cbs, state)?;
        ch[1].set_amp_res(orig_amp_res);
        read_envelope(bs, &mut ch[1], false, cbs, state)?;
        read_noise(bs, &mut ch[0], false, cbs, state)?;
        read_noise(bs, &mut ch[1], false, cbs, state)?;
    }

    read_sinusoidal_coding(bs, &mut ch[0], state)?;
    read_sinusoidal_coding(bs, &mut ch[1], state)?;
    read_extensions(bs, ps, num_time_slots)?;

    Ok(())
}

/// Read the SBR time-frequency grid.
///
/// `num_time_slots` is 15 for 960-sample core, 16 for 1024-sample core.
fn read_grid<B: ReadBitsLtr>(bs: &mut B, ch: &mut SbrChannel, num_time_slots: usize) -> Result<()> {
    ch.fclass = match bs.read_bits_leq32(2)? {
        0 => FrameClass::FixFix,
        1 => FrameClass::FixVar,
        2 => FrameClass::VarFix,
        _ => FrameClass::VarVar,
    };

    match ch.fclass {
        FrameClass::FixFix => {
            ch.num_env = 1 << bs.read_bits_leq32(2)?;
            let freq_res = bs.read_bool()?;
            for el in ch.freq_res[..ch.num_env].iter_mut() {
                *el = freq_res;
            }
            ch.env_border[0] = 0;
            if ch.num_env > 1 {
                let delta = (num_time_slots + ch.num_env / 2) / ch.num_env;
                for i in 1..ch.num_env {
                    ch.env_border[i] = ch.env_border[i - 1] + delta;
                }
            }
            ch.env_border[ch.num_env] = num_time_slots;
        }
        FrameClass::FixVar => {
            let var_bord_1 = bs.read_bits_leq32(2)? as usize;
            ch.num_env = bs.read_bits_leq32(2)? as usize + 1;
            let mut rel_bord_1 = [0usize; NUM_ENVELOPES];
            for el in rel_bord_1[..ch.num_env - 1].iter_mut() {
                *el = 2 * (bs.read_bits_leq32(2)? as usize) + 2;
            }
            let ptr_bits = 8 - (ch.num_env as u8).leading_zeros();
            ch.pointer = bs.read_bits_leq32(ptr_bits)? as u8;
            for el in ch.freq_res[..ch.num_env].iter_mut().rev() {
                *el = bs.read_bool()?;
            }
            ch.env_border[0] = 0;
            ch.env_border[ch.num_env] = num_time_slots + var_bord_1;
            for (i, &delta) in (1..ch.num_env).rev().zip(rel_bord_1.iter()) {
                ch.env_border[i] = ch.env_border[i + 1] - delta;
            }
        }
        FrameClass::VarFix => {
            let var_bord_0 = bs.read_bits_leq32(2)? as usize;
            ch.num_env = bs.read_bits_leq32(2)? as usize + 1;
            let mut rel_bord_0 = [0usize; NUM_ENVELOPES];
            for el in rel_bord_0[..ch.num_env - 1].iter_mut() {
                *el = 2 * (bs.read_bits_leq32(2)? as usize) + 2;
            }
            let ptr_bits = 8 - (ch.num_env as u8).leading_zeros();
            ch.pointer = bs.read_bits_leq32(ptr_bits)? as u8;
            for el in ch.freq_res[..ch.num_env].iter_mut() {
                *el = bs.read_bool()?;
            }
            ch.env_border[0] = var_bord_0;
            for i in 0..ch.num_env - 1 {
                ch.env_border[i + 1] = ch.env_border[i] + rel_bord_0[i];
            }
            ch.env_border[ch.num_env] = num_time_slots;
        }
        FrameClass::VarVar => {
            let var_bord_0 = bs.read_bits_leq32(2)? as usize;
            let var_bord_1 = bs.read_bits_leq32(2)? as usize;
            let num_rel_0 = bs.read_bits_leq32(2)? as usize;
            let num_rel_1 = bs.read_bits_leq32(2)? as usize;
            ch.num_env = num_rel_0 + num_rel_1 + 1;

            let mut rel_bord_0 = [0usize; NUM_ENVELOPES];
            let mut rel_bord_1 = [0usize; NUM_ENVELOPES];
            for el in rel_bord_0[..num_rel_0].iter_mut() {
                *el = 2 * (bs.read_bits_leq32(2)? as usize) + 2;
            }
            for el in rel_bord_1[..num_rel_1].iter_mut() {
                *el = 2 * (bs.read_bits_leq32(2)? as usize) + 2;
            }
            let ptr_bits = 8 - (ch.num_env as u8).leading_zeros();
            ch.pointer = bs.read_bits_leq32(ptr_bits)? as u8;
            for el in ch.freq_res[..ch.num_env].iter_mut() {
                *el = bs.read_bool()?;
            }

            ch.env_border[0] = var_bord_0;
            for i in 0..num_rel_0 {
                ch.env_border[i + 1] = ch.env_border[i] + rel_bord_0[i];
            }
            ch.env_border[ch.num_env] = num_time_slots + var_bord_1;
            for i in 0..num_rel_1 {
                ch.env_border[ch.num_env - 1 - i] = ch.env_border[ch.num_env - i] - rel_bord_1[i];
            }
        }
    }

    // Validate envelope borders are strictly increasing.
    for i in 0..ch.num_env {
        if ch.env_border[i] >= ch.env_border[i + 1] {
            return decode_error("sbr: invalid envelope borders");
        }
    }

    // Derive noise floor envelope borders.
    if ch.num_env > 1 {
        ch.num_noise = 2;
        let mid = match (ch.fclass, ch.pointer) {
            (FrameClass::FixFix, _) => ch.num_env / 2,
            (FrameClass::VarFix, 0) => 1,
            (FrameClass::VarFix, 1) => ch.num_env - 1,
            (FrameClass::VarFix, _) => ch.pointer as usize - 1,
            (_, 0) | (_, 1) => ch.num_env - 1,
            (_, _) => ch.num_env + 1 - (ch.pointer as usize),
        };
        ch.noise_env_border[0] = ch.env_border[0];
        ch.noise_env_border[1] = ch.env_border[mid];
        ch.noise_env_border[2] = ch.env_border[ch.num_env];
    }
    else {
        ch.num_noise = 1;
        ch.noise_env_border[0] = ch.env_border[0];
        ch.noise_env_border[1] = ch.env_border[1];
    }

    Ok(())
}

/// Read delta-time / delta-frequency flags.
fn read_dtdf<B: ReadBitsLtr>(bs: &mut B, ch: &mut SbrChannel) -> Result<()> {
    for el in ch.df_env[..ch.num_env].iter_mut() {
        *el = bs.read_bool()?;
    }
    for el in ch.df_noise[..ch.num_noise].iter_mut() {
        *el = bs.read_bool()?;
    }
    Ok(())
}

/// Read inverse filtering mode per noise band.
fn read_invf<B: ReadBitsLtr>(bs: &mut B, ch: &mut SbrChannel, state: &SbrState) -> Result<()> {
    for el in ch.invf_mode[..state.num_noise_bands].iter_mut() {
        *el = bs.read_bits_leq32(2)? as u8;
    }
    Ok(())
}

/// Read SBR envelope scalefactors.
#[allow(clippy::collapsible_else_if)]
fn read_envelope<B: ReadBitsLtr>(
    bs: &mut B,
    ch: &mut SbrChannel,
    coupled: bool,
    cbs: &SbrCodebooks,
    state: &SbrState,
) -> Result<()> {
    // Select codebooks and offset based on amplitude resolution and coupling.
    let (f_cb, t_cb, f_offset, t_offset) = if coupled {
        if ch.amp_res {
            (&cbs.env_bal_3_0db_f, &cbs.env_bal_3_0db_t, 12i8, 12i8)
        }
        else {
            (&cbs.env_bal_1_5db_f, &cbs.env_bal_1_5db_t, 24i8, 24i8)
        }
    }
    else {
        if ch.amp_res {
            (&cbs.env_3_0db_f, &cbs.env_3_0db_t, 31i8, 31i8)
        }
        else {
            (&cbs.env_1_5db_f, &cbs.env_1_5db_t, 60i8, 60i8)
        }
    };

    let scale: i8 = if coupled { 2 } else { 1 };

    for env_idx in 0..ch.num_env {
        let freq_res = ch.freq_res[env_idx];
        let df_env = ch.df_env[env_idx];
        let num_env_bands = state.num_env_bands[freq_res as usize];

        if !df_env {
            // Frequency direction: first band is absolute, rest are delta-coded.
            let bits = if coupled { 5 + (!ch.amp_res as u32) } else { 6 + (!ch.amp_res as u32) };
            ch.data_env[env_idx][0] = (bs.read_bits_leq32(bits)? as i8) * scale;
            let mut last = ch.data_env[env_idx][0];
            for band in 1..num_env_bands {
                let delta = read_sbr_huff(bs, f_cb, f_offset)?;
                ch.data_env[env_idx][band] = last + delta * scale;
                last = ch.data_env[env_idx][band];
            }
        }
        else {
            // Time direction: delta from previous envelope.
            for band in 0..num_env_bands {
                let delta = read_sbr_huff(bs, t_cb, t_offset)?;
                let last = match (freq_res, ch.last_freq_res) {
                    (false, true) => ch.last_envelope[state.high_to_low_res[band]],
                    (true, false) => ch.last_envelope[state.low_to_high_res[band]],
                    _ => ch.last_envelope[band],
                };
                ch.data_env[env_idx][band] = last + delta * scale;
            }
        }

        ch.last_envelope = ch.data_env[env_idx];
        ch.last_freq_res = freq_res;
    }

    Ok(())
}

/// Read SBR noise floor scalefactors.
fn read_noise<B: ReadBitsLtr>(
    bs: &mut B,
    ch: &mut SbrChannel,
    coupled: bool,
    cbs: &SbrCodebooks,
    state: &SbrState,
) -> Result<()> {
    // Noise uses 3.0dB codebooks: frequency uses env_3.0db_f (or bal), time uses noise_3.0db_t (or bal).
    let (f_cb, t_cb, f_offset, t_offset) = if coupled {
        (&cbs.env_bal_3_0db_f, &cbs.noise_bal_3_0db_t, 12i8, 12i8)
    }
    else {
        (&cbs.env_3_0db_f, &cbs.noise_3_0db_t, 31i8, 31i8)
    };

    let scale: i8 = if coupled { 2 } else { 1 };

    for noise_idx in 0..ch.num_noise {
        let df_noise = ch.df_noise[noise_idx];

        if !df_noise {
            // Frequency direction.
            ch.data_noise[noise_idx][0] = (bs.read_bits_leq32(5)? as i8) * scale;
            let mut last = ch.data_noise[noise_idx][0];
            for band in 1..state.num_noise_bands {
                let delta = read_sbr_huff(bs, f_cb, f_offset)?;
                ch.data_noise[noise_idx][band] = last + scale * delta;
                last = ch.data_noise[noise_idx][band];
            }
        }
        else {
            // Time direction.
            for band in 0..state.num_noise_bands {
                let delta = read_sbr_huff(bs, t_cb, t_offset)?;
                ch.data_noise[noise_idx][band] = ch.last_noise_env[band] + delta * scale;
            }
        }

        ch.last_noise_env = ch.data_noise[noise_idx];
    }

    Ok(())
}

/// Read sinusoidal coding (additional harmonic) flags.
fn read_sinusoidal_coding<B: ReadBitsLtr>(
    bs: &mut B,
    ch: &mut SbrChannel,
    state: &SbrState,
) -> Result<()> {
    if !bs.read_bool()? {
        ch.add_harmonic = [false; SBR_BANDS];
        return Ok(());
    }
    for el in ch.add_harmonic[..state.num_env_bands[1]].iter_mut() {
        *el = bs.read_bool()?;
    }
    Ok(())
}

/// Read SBR extension data. Parses PS extensions if a PS context is provided;
/// otherwise skips all extension data.
fn read_extensions<B: ReadBitsLtr>(
    bs: &mut B,
    mut ps: Option<&mut PsCommonContext>,
    num_time_slots: usize,
) -> Result<()> {
    if bs.read_bool()? {
        let mut size = bs.read_bits_leq32(4)? as usize;
        if size == 15 {
            size += bs.read_bits_leq32(8)? as usize;
        }
        let total_bits = size * 8;
        let mut bits_consumed: usize = 0;

        while bits_consumed + 7 < total_bits {
            let ext_id = bs.read_bits_leq32(2)? as usize;
            bits_consumed += 2;

            if ext_id == 2 {
                if let Some(ref mut ps_ctx) = ps {
                    // PS extension: parse Parametric Stereo data.
                    let remaining = total_bits - bits_consumed;
                    let used =
                        super::ps::bs::ps_read_data(bs, ps_ctx, remaining, num_time_slots * 2)?;
                    bits_consumed += used;
                }
                else {
                    // No PS context — skip remaining.
                    let remaining = total_bits - bits_consumed;
                    bs.ignore_bits(remaining as u32)?;
                    bits_consumed = total_bits;
                }
            }
            else {
                // Unknown extension — skip remaining.
                let remaining = total_bits - bits_consumed;
                bs.ignore_bits(remaining as u32)?;
                bits_consumed = total_bits;
            }
        }

        // Skip any remaining fractional bits.
        if bits_consumed < total_bits {
            bs.ignore_bits((total_bits - bits_consumed) as u32)?;
        }
    }
    Ok(())
}
