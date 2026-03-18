// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SBR bitstream element parsing (ISO/IEC 14496-3, 4.6.18).
//!
//! Reads the SBR extension payload: header, time-frequency grid,
//! envelope and noise floor scalefactors, inverse filtering modes,
//! sinusoidal coding flags, and optional PS extension data.

use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::vlc::*;
use symphonia_core::io::ReadBitsLtr;

use lazy_static::lazy_static;

use super::ps::PsCommonContext;
use super::tables;
use super::{FrameClass, QuantMode, SbrChannel, SbrHeader, SbrState, NUM_ENVELOPES, SBR_BANDS};

// ---------------------------------------------------------------------------
// Huffman codebooks for SBR scalefactor decoding
// ---------------------------------------------------------------------------

/// Construct a VLC codebook from paired code/length tables.
fn make_vlc(codes: &[u32], lengths: &[u8]) -> Codebook<Entry16x16> {
    let symbols: Vec<u16> = (0..codes.len() as u16).collect();
    let mut b = CodebookBuilder::new(BitOrder::Verbatim);
    b.bits_per_read(8);
    b.make(codes, lengths, &symbols).unwrap()
}

/// All Huffman codebooks used by the SBR envelope and noise floor decoder.
///
/// Codebooks are organized by quantization step size (1.5 dB or 3.0 dB),
/// coding direction (frequency or time), and coupling mode (normal or balance).
struct HuffmanTables {
    /// Envelope 1.5 dB, frequency direction.
    env_15_freq: Codebook<Entry16x16>,
    /// Envelope 1.5 dB, time direction.
    env_15_time: Codebook<Entry16x16>,
    /// Envelope balance 1.5 dB, frequency direction.
    env_bal_15_freq: Codebook<Entry16x16>,
    /// Envelope balance 1.5 dB, time direction.
    env_bal_15_time: Codebook<Entry16x16>,
    /// Envelope 3.0 dB, frequency direction.
    env_30_freq: Codebook<Entry16x16>,
    /// Envelope 3.0 dB, time direction.
    env_30_time: Codebook<Entry16x16>,
    /// Envelope balance 3.0 dB, frequency direction.
    env_bal_30_freq: Codebook<Entry16x16>,
    /// Envelope balance 3.0 dB, time direction.
    env_bal_30_time: Codebook<Entry16x16>,
    /// Noise floor 3.0 dB, time direction.
    noise_30_time: Codebook<Entry16x16>,
    /// Noise floor balance 3.0 dB, time direction.
    noise_bal_30_time: Codebook<Entry16x16>,
}

impl HuffmanTables {
    fn create() -> Self {
        Self {
            env_15_freq: make_vlc(
                &tables::ENVELOPE_1_5DB_FREQ_CODES,
                &tables::ENVELOPE_1_5DB_FREQ_LENGTHS,
            ),
            env_15_time: make_vlc(
                &tables::ENVELOPE_1_5DB_TIME_CODES,
                &tables::ENVELOPE_1_5DB_TIME_LENGTHS,
            ),
            env_bal_15_freq: make_vlc(
                &tables::ENVELOPE_BAL_1_5DB_FREQ_CODES,
                &tables::ENVELOPE_BAL_1_5DB_FREQ_LENGTHS,
            ),
            env_bal_15_time: make_vlc(
                &tables::ENVELOPE_BAL_1_5DB_TIME_CODES,
                &tables::ENVELOPE_BAL_1_5DB_TIME_LENGTHS,
            ),
            env_30_freq: make_vlc(
                &tables::ENVELOPE_3_0DB_FREQ_CODES,
                &tables::ENVELOPE_3_0DB_FREQ_LENGTHS,
            ),
            env_30_time: make_vlc(
                &tables::ENVELOPE_3_0DB_TIME_CODES,
                &tables::ENVELOPE_3_0DB_TIME_LENGTHS,
            ),
            env_bal_30_freq: make_vlc(
                &tables::ENVELOPE_BAL_3_0DB_FREQ_CODES,
                &tables::ENVELOPE_BAL_3_0DB_FREQ_LENGTHS,
            ),
            env_bal_30_time: make_vlc(
                &tables::ENVELOPE_BAL_3_0DB_TIME_CODES,
                &tables::ENVELOPE_BAL_3_0DB_TIME_LENGTHS,
            ),
            noise_30_time: make_vlc(
                &tables::NOISE_3_0DB_TIME_CODES,
                &tables::NOISE_3_0DB_TIME_LENGTHS,
            ),
            noise_bal_30_time: make_vlc(
                &tables::NOISE_BAL_3_0DB_TIME_CODES,
                &tables::NOISE_BAL_3_0DB_TIME_LENGTHS,
            ),
        }
    }
}

lazy_static! {
    static ref HUFF: HuffmanTables = HuffmanTables::create();
}

/// Decode one Huffman symbol and return as signed delta value.
///
/// The codebook produces an unsigned index; subtracting `mid` centres it
/// around zero to give the signed delta.
#[inline]
fn decode_delta<B: ReadBitsLtr>(bs: &mut B, cb: &Codebook<Entry16x16>, mid: i8) -> Result<i8> {
    let (sym, _) = bs.read_codebook(cb)?;
    Ok(sym as i8 - mid)
}

// ---------------------------------------------------------------------------
// Public bitstream entry points
// ---------------------------------------------------------------------------

/// Parse SBR header element (ISO/IEC 14496-3, 4.6.18.2.2, Table 4.56).
pub fn sbr_read_header<B: ReadBitsLtr>(bs: &mut B) -> Result<SbrHeader> {
    let mut hdr = SbrHeader::new();

    hdr.amp_res = bs.read_bool()?;
    hdr.start_freq = bs.read_bits_leq32(4)? as usize;
    hdr.stop_freq = bs.read_bits_leq32(4)? as usize;
    hdr.xover_band = bs.read_bits_leq32(3)? as usize;
    let _reserved = bs.read_bits_leq32(2)?;

    let extra_header_1 = bs.read_bool()?;
    let extra_header_2 = bs.read_bool()?;

    if extra_header_1 {
        hdr.freq_scale = bs.read_bits_leq32(2)? as u8;
        hdr.alter_scale = bs.read_bool()?;
        hdr.noise_bands = bs.read_bits_leq32(2)? as u8;
    }
    if extra_header_2 {
        hdr.limiter_bands = bs.read_bits_leq32(2)? as u8;
        hdr.limiter_gains = bs.read_bits_leq32(2)? as u8;
        hdr.interpol_freq = bs.read_bool()?;
        hdr.smoothing_mode = bs.read_bool()?;
    }

    Ok(hdr)
}

/// Parse SBR single channel element (SCE) data (ISO/IEC 14496-3, Table 4.57).
pub fn sbr_read_sce<B: ReadBitsLtr>(
    bs: &mut B,
    orig_amp_res: bool,
    state: &SbrState,
    ch: &mut SbrChannel,
    ps: Option<&mut PsCommonContext>,
    num_time_slots: usize,
) -> Result<()> {
    let huff = &*HUFF;
    ch.qmode = QuantMode::Single;

    // bs_data_extra: reserved extension bits.
    if bs.read_bool()? {
        bs.ignore_bits(4)?;
    }

    parse_time_freq_grid(bs, ch, num_time_slots)?;
    parse_dtdf_flags(bs, ch)?;
    parse_invf_mode(bs, ch, state)?;
    ch.set_amp_res(orig_amp_res);
    parse_envelope_data(bs, ch, false, huff, state)?;
    parse_noise_data(bs, ch, false, huff, state)?;
    parse_sinusoidal_flags(bs, ch, state)?;
    parse_extension_data(bs, ps, num_time_slots)?;

    Ok(())
}

/// Parse SBR channel pair element (CPE) data (ISO/IEC 14496-3, Table 4.58).
pub fn sbr_read_cpe<B: ReadBitsLtr>(
    bs: &mut B,
    orig_amp_res: bool,
    state: &SbrState,
    ch: &mut [SbrChannel; 2],
    ps: Option<&mut PsCommonContext>,
    num_time_slots: usize,
) -> Result<()> {
    let huff = &*HUFF;

    // bs_data_extra: reserved bits for both channels.
    if bs.read_bool()? {
        bs.ignore_bits(4)?;
        bs.ignore_bits(4)?;
    }

    let bs_coupling = bs.read_bool()?;

    if bs_coupling {
        // Coupled mode: shared grid, balance-coded second channel.
        ch[0].qmode = QuantMode::Left;
        ch[1].qmode = QuantMode::Right;

        parse_time_freq_grid(bs, &mut ch[0], num_time_slots)?;
        // Duplicate grid to second channel.
        ch[1].fclass = ch[0].fclass;
        ch[1].num_env = ch[0].num_env;
        ch[1].env_border = ch[0].env_border;
        ch[1].freq_res = ch[0].freq_res;
        ch[1].pointer = ch[0].pointer;
        ch[1].num_noise = ch[0].num_noise;
        ch[1].noise_env_border = ch[0].noise_env_border;

        parse_dtdf_flags(bs, &mut ch[0])?;
        parse_dtdf_flags(bs, &mut ch[1])?;
        parse_invf_mode(bs, &mut ch[0], state)?;
        // Shared inverse filtering in coupled mode.
        ch[1].invf_mode = ch[0].invf_mode;

        ch[0].set_amp_res(orig_amp_res);
        parse_envelope_data(bs, &mut ch[0], false, huff, state)?;
        parse_noise_data(bs, &mut ch[0], false, huff, state)?;

        ch[1].set_amp_res(orig_amp_res);
        parse_envelope_data(bs, &mut ch[1], true, huff, state)?;
        parse_noise_data(bs, &mut ch[1], true, huff, state)?;

        // Cross-store coupled data for de-coupling in synthesis.
        ch[0].data_env2 = ch[1].data_env;
        ch[0].data_noise2 = ch[1].data_noise;
        ch[1].data_env2 = ch[0].data_env;
        ch[1].data_noise2 = ch[0].data_noise;
    }
    else {
        // Independent mode: separate grids and data.
        ch[0].qmode = QuantMode::Single;
        ch[1].qmode = QuantMode::Single;

        parse_time_freq_grid(bs, &mut ch[0], num_time_slots)?;
        parse_time_freq_grid(bs, &mut ch[1], num_time_slots)?;
        parse_dtdf_flags(bs, &mut ch[0])?;
        parse_dtdf_flags(bs, &mut ch[1])?;
        parse_invf_mode(bs, &mut ch[0], state)?;
        parse_invf_mode(bs, &mut ch[1], state)?;

        ch[0].set_amp_res(orig_amp_res);
        parse_envelope_data(bs, &mut ch[0], false, huff, state)?;
        ch[1].set_amp_res(orig_amp_res);
        parse_envelope_data(bs, &mut ch[1], false, huff, state)?;
        parse_noise_data(bs, &mut ch[0], false, huff, state)?;
        parse_noise_data(bs, &mut ch[1], false, huff, state)?;
    }

    parse_sinusoidal_flags(bs, &mut ch[0], state)?;
    parse_sinusoidal_flags(bs, &mut ch[1], state)?;
    parse_extension_data(bs, ps, num_time_slots)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Internal parsing functions
// ---------------------------------------------------------------------------

/// Parse the SBR time-frequency grid (ISO/IEC 14496-3, 4.6.18.3.3).
///
/// Determines envelope time borders, frequency resolution per envelope,
/// and noise floor time borders for the frame.
fn parse_time_freq_grid<B: ReadBitsLtr>(
    bs: &mut B,
    ch: &mut SbrChannel,
    num_time_slots: usize,
) -> Result<()> {
    let frame_class = bs.read_bits_leq32(2)?;
    ch.fclass = match frame_class {
        0 => FrameClass::FixFix,
        1 => FrameClass::FixVar,
        2 => FrameClass::VarFix,
        _ => FrameClass::VarVar,
    };

    match ch.fclass {
        FrameClass::FixFix => parse_grid_fixfix(bs, ch, num_time_slots)?,
        FrameClass::FixVar => parse_grid_fixvar(bs, ch, num_time_slots)?,
        FrameClass::VarFix => parse_grid_varfix(bs, ch, num_time_slots)?,
        FrameClass::VarVar => parse_grid_varvar(bs, ch, num_time_slots)?,
    }

    // Validate: envelope borders must be strictly increasing.
    for i in 0..ch.num_env {
        if ch.env_border[i] >= ch.env_border[i + 1] {
            return decode_error("sbr: envelope time borders are not strictly increasing");
        }
    }

    // Derive noise floor envelope borders from the time-frequency grid.
    derive_noise_borders(ch);

    Ok(())
}

/// FIXFIX grid: uniformly spaced envelopes with constant frequency resolution.
fn parse_grid_fixfix<B: ReadBitsLtr>(
    bs: &mut B,
    ch: &mut SbrChannel,
    num_time_slots: usize,
) -> Result<()> {
    let num_env_exp = bs.read_bits_leq32(2)? as usize;
    ch.num_env = 1 << num_env_exp;
    let resolution = bs.read_bool()?;

    for fr in ch.freq_res[..ch.num_env].iter_mut() {
        *fr = resolution;
    }

    // Evenly spaced borders: t_E[0] = 0, t_E[L_E] = numTimeSlots.
    ch.env_border[0] = 0;
    if ch.num_env > 1 {
        let spacing = (num_time_slots + ch.num_env / 2) / ch.num_env;
        for i in 1..ch.num_env {
            ch.env_border[i] = ch.env_border[i - 1] + spacing;
        }
    }
    ch.env_border[ch.num_env] = num_time_slots;

    Ok(())
}

/// FIXVAR grid: fixed start, variable end with relative borders from the right.
fn parse_grid_fixvar<B: ReadBitsLtr>(
    bs: &mut B,
    ch: &mut SbrChannel,
    num_time_slots: usize,
) -> Result<()> {
    let abs_bord_trail = bs.read_bits_leq32(2)? as usize;
    let num_rel = bs.read_bits_leq32(2)? as usize;
    ch.num_env = num_rel + 1;

    // Relative border widths (read for trailing borders).
    let mut rel_widths = [0usize; NUM_ENVELOPES];
    for w in rel_widths[..num_rel].iter_mut() {
        *w = 2 * bs.read_bits_leq32(2)? as usize + 2;
    }

    // Pointer and frequency resolutions (read in reverse order per spec).
    let ptr_bits = ceil_log2(ch.num_env as u32 + 1);
    ch.pointer = bs.read_bits_leq32(ptr_bits)? as u8;
    for fr in ch.freq_res[..ch.num_env].iter_mut().rev() {
        *fr = bs.read_bool()?;
    }

    // Build borders: first = 0, last = numTimeSlots + abs_bord_trail.
    ch.env_border[0] = 0;
    ch.env_border[ch.num_env] = num_time_slots + abs_bord_trail;
    // Fill inward from the right.
    for r in 0..num_rel {
        let env_idx = ch.num_env - 1 - r;
        ch.env_border[env_idx] = ch.env_border[env_idx + 1] - rel_widths[r];
    }

    Ok(())
}

/// VARFIX grid: variable start, fixed end with relative borders from the left.
fn parse_grid_varfix<B: ReadBitsLtr>(
    bs: &mut B,
    ch: &mut SbrChannel,
    num_time_slots: usize,
) -> Result<()> {
    let abs_bord_lead = bs.read_bits_leq32(2)? as usize;
    let num_rel = bs.read_bits_leq32(2)? as usize;
    ch.num_env = num_rel + 1;

    let mut rel_widths = [0usize; NUM_ENVELOPES];
    for w in rel_widths[..num_rel].iter_mut() {
        *w = 2 * bs.read_bits_leq32(2)? as usize + 2;
    }

    let ptr_bits = ceil_log2(ch.num_env as u32 + 1);
    ch.pointer = bs.read_bits_leq32(ptr_bits)? as u8;
    for fr in ch.freq_res[..ch.num_env].iter_mut() {
        *fr = bs.read_bool()?;
    }

    // Build borders: first = abs_bord_lead, last = numTimeSlots.
    ch.env_border[0] = abs_bord_lead;
    for r in 0..num_rel {
        ch.env_border[r + 1] = ch.env_border[r] + rel_widths[r];
    }
    ch.env_border[ch.num_env] = num_time_slots;

    Ok(())
}

/// VARVAR grid: variable start and end with relative borders from both sides.
fn parse_grid_varvar<B: ReadBitsLtr>(
    bs: &mut B,
    ch: &mut SbrChannel,
    num_time_slots: usize,
) -> Result<()> {
    let abs_bord_lead = bs.read_bits_leq32(2)? as usize;
    let abs_bord_trail = bs.read_bits_leq32(2)? as usize;
    let num_rel_lead = bs.read_bits_leq32(2)? as usize;
    let num_rel_trail = bs.read_bits_leq32(2)? as usize;
    ch.num_env = num_rel_lead + num_rel_trail + 1;

    let mut rel_lead = [0usize; NUM_ENVELOPES];
    let mut rel_trail = [0usize; NUM_ENVELOPES];
    for w in rel_lead[..num_rel_lead].iter_mut() {
        *w = 2 * bs.read_bits_leq32(2)? as usize + 2;
    }
    for w in rel_trail[..num_rel_trail].iter_mut() {
        *w = 2 * bs.read_bits_leq32(2)? as usize + 2;
    }

    let ptr_bits = ceil_log2(ch.num_env as u32 + 1);
    ch.pointer = bs.read_bits_leq32(ptr_bits)? as u8;
    for fr in ch.freq_res[..ch.num_env].iter_mut() {
        *fr = bs.read_bool()?;
    }

    // Leading borders.
    ch.env_border[0] = abs_bord_lead;
    for r in 0..num_rel_lead {
        ch.env_border[r + 1] = ch.env_border[r] + rel_lead[r];
    }
    // Trailing border.
    ch.env_border[ch.num_env] = num_time_slots + abs_bord_trail;
    // Fill inward from the right for trailing relative borders.
    for r in 0..num_rel_trail {
        let env_idx = ch.num_env - 1 - r;
        ch.env_border[env_idx] = ch.env_border[env_idx + 1] - rel_trail[r];
    }

    Ok(())
}

/// Derive noise floor time borders from the envelope grid.
///
/// With multiple envelopes, two noise floors are used with the split point
/// determined by the pointer field and frame class. With a single envelope,
/// one noise floor spans the entire frame.
fn derive_noise_borders(ch: &mut SbrChannel) {
    if ch.num_env > 1 {
        ch.num_noise = 2;
        // Determine the envelope index that splits the two noise floors.
        let split = match (ch.fclass, ch.pointer) {
            (FrameClass::FixFix, _) => ch.num_env / 2,
            (FrameClass::VarFix, 0) => 1,
            (FrameClass::VarFix, 1) => ch.num_env - 1,
            (FrameClass::VarFix, p) => (p as usize) - 1,
            (_, 0) | (_, 1) => ch.num_env - 1,
            (_, p) => ch.num_env + 1 - (p as usize),
        };
        ch.noise_env_border[0] = ch.env_border[0];
        ch.noise_env_border[1] = ch.env_border[split];
        ch.noise_env_border[2] = ch.env_border[ch.num_env];
    }
    else {
        ch.num_noise = 1;
        ch.noise_env_border[0] = ch.env_border[0];
        ch.noise_env_border[1] = ch.env_border[1];
    }
}

/// Parse delta-time / delta-frequency direction flags (ISO/IEC 14496-3, 4.6.18.3.4).
fn parse_dtdf_flags<B: ReadBitsLtr>(bs: &mut B, ch: &mut SbrChannel) -> Result<()> {
    for flag in ch.df_env[..ch.num_env].iter_mut() {
        *flag = bs.read_bool()?;
    }
    for flag in ch.df_noise[..ch.num_noise].iter_mut() {
        *flag = bs.read_bool()?;
    }
    Ok(())
}

/// Parse inverse filtering mode for each noise band (ISO/IEC 14496-3, 4.6.18.6.3).
fn parse_invf_mode<B: ReadBitsLtr>(
    bs: &mut B,
    ch: &mut SbrChannel,
    state: &SbrState,
) -> Result<()> {
    for mode in ch.invf_mode[..state.num_noise_bands].iter_mut() {
        *mode = bs.read_bits_leq32(2)? as u8;
    }
    Ok(())
}

/// Parse envelope scalefactor data (ISO/IEC 14496-3, 4.6.18.4).
///
/// When `balance` is true, the balance (coupled) codebooks are used.
#[allow(clippy::collapsible_else_if)]
fn parse_envelope_data<B: ReadBitsLtr>(
    bs: &mut B,
    ch: &mut SbrChannel,
    balance: bool,
    huff: &HuffmanTables,
    state: &SbrState,
) -> Result<()> {
    // Select codebook pair and centre offset based on resolution and coupling.
    let (freq_cb, time_cb, centre) = if balance {
        if ch.amp_res {
            (&huff.env_bal_30_freq, &huff.env_bal_30_time, 12i8)
        }
        else {
            (&huff.env_bal_15_freq, &huff.env_bal_15_time, 24i8)
        }
    }
    else {
        if ch.amp_res {
            (&huff.env_30_freq, &huff.env_30_time, 31i8)
        }
        else {
            (&huff.env_15_freq, &huff.env_15_time, 60i8)
        }
    };

    let step: i8 = if balance { 2 } else { 1 };

    for l in 0..ch.num_env {
        let res = ch.freq_res[l];
        let n_bands = state.num_env_bands[res as usize];

        if !ch.df_env[l] {
            // Delta-frequency: first band is PCM-coded, rest are Huffman deltas.
            let pcm_bits =
                if balance { 5 + u32::from(!ch.amp_res) } else { 6 + u32::from(!ch.amp_res) };
            ch.data_env[l][0] = (bs.read_bits_leq32(pcm_bits)? as i8) * step;

            let mut prev = ch.data_env[l][0];
            for k in 1..n_bands {
                let d = decode_delta(bs, freq_cb, centre)?;
                ch.data_env[l][k] = prev + d * step;
                prev = ch.data_env[l][k];
            }
        }
        else {
            // Delta-time: Huffman delta from same band in previous envelope.
            for k in 0..n_bands {
                let d = decode_delta(bs, time_cb, centre)?;
                let ref_val = match (res, ch.last_freq_res) {
                    (false, true) => ch.last_envelope[state.high_to_low_res[k]],
                    (true, false) => ch.last_envelope[state.low_to_high_res[k]],
                    _ => ch.last_envelope[k],
                };
                ch.data_env[l][k] = ref_val + d * step;
            }
        }

        ch.last_envelope = ch.data_env[l];
        ch.last_freq_res = res;
    }

    Ok(())
}

/// Parse noise floor scalefactor data (ISO/IEC 14496-3, 4.6.18.5).
fn parse_noise_data<B: ReadBitsLtr>(
    bs: &mut B,
    ch: &mut SbrChannel,
    balance: bool,
    huff: &HuffmanTables,
    state: &SbrState,
) -> Result<()> {
    // Noise frequency uses the envelope 3.0 dB codebook; noise time has its own.
    let (freq_cb, time_cb, centre) = if balance {
        (&huff.env_bal_30_freq, &huff.noise_bal_30_time, 12i8)
    }
    else {
        (&huff.env_30_freq, &huff.noise_30_time, 31i8)
    };

    let step: i8 = if balance { 2 } else { 1 };

    for q in 0..ch.num_noise {
        if !ch.df_noise[q] {
            // Delta-frequency direction.
            ch.data_noise[q][0] = (bs.read_bits_leq32(5)? as i8) * step;
            let mut prev = ch.data_noise[q][0];
            for k in 1..state.num_noise_bands {
                let d = decode_delta(bs, freq_cb, centre)?;
                ch.data_noise[q][k] = prev + step * d;
                prev = ch.data_noise[q][k];
            }
        }
        else {
            // Delta-time direction.
            for k in 0..state.num_noise_bands {
                let d = decode_delta(bs, time_cb, centre)?;
                ch.data_noise[q][k] = ch.last_noise_env[k] + d * step;
            }
        }

        ch.last_noise_env = ch.data_noise[q];
    }

    Ok(())
}

/// Parse bs_add_harmonic flags (ISO/IEC 14496-3, 4.6.18.7).
fn parse_sinusoidal_flags<B: ReadBitsLtr>(
    bs: &mut B,
    ch: &mut SbrChannel,
    state: &SbrState,
) -> Result<()> {
    let present = bs.read_bool()?;
    if !present {
        ch.add_harmonic = [false; SBR_BANDS];
        return Ok(());
    }
    for flag in ch.add_harmonic[..state.num_env_bands[1]].iter_mut() {
        *flag = bs.read_bool()?;
    }
    Ok(())
}

/// Parse SBR extension data, including PS if a context is available.
fn parse_extension_data<B: ReadBitsLtr>(
    bs: &mut B,
    mut ps: Option<&mut PsCommonContext>,
    num_time_slots: usize,
) -> Result<()> {
    let has_ext = bs.read_bool()?;
    if !has_ext {
        return Ok(());
    }

    let mut byte_count = bs.read_bits_leq32(4)? as usize;
    if byte_count == 15 {
        byte_count += bs.read_bits_leq32(8)? as usize;
    }
    let total_bits = byte_count * 8;
    let mut consumed: usize = 0;

    while consumed + 7 < total_bits {
        let ext_type = bs.read_bits_leq32(2)? as u8;
        consumed += 2;

        match ext_type {
            // PS extension (EXTENSION_ID_PS = 2).
            2 if ps.is_some() => {
                let avail = total_bits - consumed;
                let used = super::ps::bs::ps_read_data(
                    bs,
                    ps.as_deref_mut().unwrap(),
                    avail,
                    num_time_slots * 2,
                )?;
                consumed += used;
            }
            _ => {
                // Unknown or unavailable extension — skip remainder.
                let skip = total_bits - consumed;
                bs.ignore_bits(skip as u32)?;
                consumed = total_bits;
            }
        }
    }

    // Consume any leftover bits.
    if consumed < total_bits {
        bs.ignore_bits((total_bits - consumed) as u32)?;
    }

    Ok(())
}

/// Minimum number of bits needed to represent `n` (ceiling of log2).
#[inline]
fn ceil_log2(n: u32) -> u32 {
    if n == 0 {
        0
    }
    else {
        32 - (n - 1).leading_zeros()
    }
}
