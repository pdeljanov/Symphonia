// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SBR bitstream decoding: sbr_extension_data() and its sub-elements (Tables 4.62 to 4.74), the
//! derivation of the frequency band tables (4.6.18.3.2), the time/frequency grid (4.6.18.3.3),
//! envelope and noise floor decoding (4.6.18.3.4), and dequantization (4.6.18.3.5).
//!
//! Only the SBR payloads of the AAC Main, SSR, LC, and LTP audio object types (4.5.2.8.2.2) with
//! 1024-sample frames are supported.

mod crc;
mod grid;

use symphonia_core::errors::{Result, decode_error};
use symphonia_core::io::{BitReaderLtr, FiniteBitStream, ReadBitsLtr};

use crate::aac::ps::PsDecoder;
use crate::aac::sbr::bands::{derive_freq_band_tables, limiter_table, sbr_sampling_frequency};
use crate::aac::sbr::envelope::{
    DeltaCodedData, DeltaHistory, Scalefactors, decode_scalefactors, dequantize, dequantize_coupled,
};
use crate::aac::sbr::huffman::{SbrHuffmanTable, sbr_huff_dec};
use crate::aac::sbr::types::{
    FreqBandTables, MAX_NOISE_BANDS, QMF_BANDS, SbrChannelFrame, SbrFrame, SbrHeader,
};

use grid::{SbrGrid, read_grid};

/// bs_extension_id of parametric stereo data (Table 4.112).
const EXTENSION_ID_PS: u32 = 2;

/// A bit reader that is limited to the exact length of an SBR payload.
///
/// Every read is checked against the length of the payload. Reading beyond the end of the payload
/// is a decode error.
struct SbrBitReader<'a> {
    /// The underlying bit reader. Its buffer may contain up-to 7 bits beyond the payload.
    inner: BitReaderLtr<'a>,
    /// The total number of bits of the buffer of `inner`.
    buf_bits: usize,
    /// The length of the payload in bits.
    num_bits: usize,
}

impl<'a> SbrBitReader<'a> {
    fn new(buf: &'a [u8], num_bits: usize) -> Self {
        Self { inner: BitReaderLtr::new(buf), buf_bits: 8 * buf.len(), num_bits }
    }

    /// The number of bits read.
    fn pos(&self) -> usize {
        self.buf_bits.saturating_sub(self.inner.bits_left() as usize)
    }

    /// The number of bits remaining in the payload.
    fn bits_left(&self) -> usize {
        self.num_bits.saturating_sub(self.pos())
    }

    /// Read `bit_width` <= 32 bits.
    fn read(&mut self, bit_width: u32) -> Result<u32> {
        if bit_width as usize > self.bits_left() {
            return decode_error("sbr: unexpected end of payload");
        }
        match self.inner.read_bits_leq32(bit_width) {
            Ok(value) => Ok(value),
            Err(_) => decode_error("sbr: unexpected end of payload"),
        }
    }

    /// Read one bit.
    fn read_bool(&mut self) -> Result<bool> {
        Ok(self.read(1)? == 1)
    }

    /// Read one Huffman codeword with sbr_huff_dec() (Annex 4.A.6.1).
    fn read_huff(&mut self, table: SbrHuffmanTable) -> Result<i32> {
        let value = match sbr_huff_dec(&mut self.inner, table) {
            Ok(value) => value,
            Err(_) => return decode_error("sbr: unexpected end of payload"),
        };
        if self.pos() > self.num_bits {
            return decode_error("sbr: unexpected end of payload");
        }
        Ok(value)
    }

    /// Skip `num_bits` bits.
    fn skip(&mut self, num_bits: usize) -> Result<()> {
        if num_bits > self.bits_left() {
            return decode_error("sbr: unexpected end of payload");
        }
        let mut remaining = num_bits;
        while remaining > 0 {
            let n = remaining.min(1 << 16);
            if self.inner.ignore_bits(n as u32).is_err() {
                return decode_error("sbr: unexpected end of payload");
            }
            remaining -= n;
        }
        Ok(())
    }
}

/// Read sbr_header() (Table 4.63).
///
/// The data elements of the optional header parts that are not present take their default values
/// (4.5.2.8.1, Tables 4.105 to 4.111).
fn read_header(bs: &mut SbrBitReader<'_>) -> Result<SbrHeader> {
    let bs_amp_res = bs.read_bool()?;
    let bs_start_freq = bs.read(4)? as u8;
    let bs_stop_freq = bs.read(4)? as u8;
    let bs_xover_band = bs.read(3)? as u8;
    let _bs_reserved = bs.read(2)?;
    let bs_header_extra_1 = bs.read_bool()?;
    let bs_header_extra_2 = bs.read_bool()?;

    let mut header = SbrHeader {
        bs_amp_res,
        bs_start_freq,
        bs_stop_freq,
        bs_xover_band,
        bs_freq_scale: 2,
        bs_alter_scale: true,
        bs_noise_bands: 2,
        bs_limiter_bands: 2,
        bs_limiter_gains: 2,
        bs_interpol_freq: true,
        bs_smoothing_mode: true,
    };

    if bs_header_extra_1 {
        header.bs_freq_scale = bs.read(2)? as u8;
        header.bs_alter_scale = bs.read_bool()?;
        header.bs_noise_bands = bs.read(2)? as u8;
    }

    if bs_header_extra_2 {
        header.bs_limiter_bands = bs.read(2)? as u8;
        header.bs_limiter_gains = bs.read(2)? as u8;
        header.bs_interpol_freq = bs.read_bool()?;
        header.bs_smoothing_mode = bs.read_bool()?;
    }

    Ok(header)
}

/// Returns true if the SBR system must be reset (reset = 1) because one of the header data
/// elements listed in 4.6.18.3.1 changed.
fn header_requires_reset(prev: &SbrHeader, next: &SbrHeader) -> bool {
    prev.bs_start_freq != next.bs_start_freq
        || prev.bs_stop_freq != next.bs_stop_freq
        || prev.bs_freq_scale != next.bs_freq_scale
        || prev.bs_alter_scale != next.bs_alter_scale
        || prev.bs_xover_band != next.bs_xover_band
        || prev.bs_noise_bands != next.bs_noise_bands
}

/// The data of one channel of an SBR frame as read from the bitstream.
struct ChannelData {
    /// The time/frequency grid.
    grid: SbrGrid,
    /// The delta coded envelope and noise floor scalefactors.
    delta: DeltaCodedData,
    /// bs_invf_mode.
    invf_mode: [u8; MAX_NOISE_BANDS],
    /// bs_add_harmonic, all false if bs_add_harmonic_flag is 0.
    add_harmonic: [bool; QMF_BANDS],
}

impl Default for ChannelData {
    fn default() -> Self {
        Self {
            grid: Default::default(),
            delta: Default::default(),
            invf_mode: [0; MAX_NOISE_BANDS],
            add_harmonic: [false; QMF_BANDS],
        }
    }
}

impl ChannelData {
    fn set_grid(&mut self, grid: SbrGrid) {
        self.grid = grid;
        self.delta.num_env = grid.num_env;
        self.delta.freq_res = grid.freq_res;
        self.delta.num_noise = grid.num_noise;
    }

    /// Copy the decoded data of the channel to an output channel frame. The dequantized
    /// scalefactors are written separately.
    fn write_to(&self, out: &mut SbrChannelFrame) {
        out.num_env = self.grid.num_env;
        out.t_env = self.grid.t_env;
        out.freq_res = self.grid.freq_res;
        out.num_noise = self.grid.num_noise;
        out.t_noise = self.grid.t_noise;
        out.l_a = self.grid.l_a;
        out.invf_mode = self.invf_mode;
        out.add_harmonic = self.add_harmonic;
    }
}

/// N_Q, the number of noise floor bands.
fn num_noise_bands(tables: &FreqBandTables) -> Result<usize> {
    let n_q = tables.f_table_noise.len().saturating_sub(1);
    if n_q > MAX_NOISE_BANDS {
        return decode_error("sbr: too many noise floor bands");
    }
    Ok(n_q)
}

/// n(r), the number of envelope bands for the frequency resolution r.
fn num_env_bands(tables: &FreqBandTables, high_res: bool) -> Result<usize> {
    let n = tables.envelope_table(high_res).len().saturating_sub(1);
    if n > QMF_BANDS {
        return decode_error("sbr: too many envelope bands");
    }
    Ok(n)
}

/// Read sbr_dtdf() (Table 4.70).
fn read_dtdf(bs: &mut SbrBitReader<'_>, ch: &mut ChannelData) -> Result<()> {
    for env in 0..ch.grid.num_env {
        ch.delta.df_env[env] = bs.read_bool()?;
    }
    for noise in 0..ch.grid.num_noise {
        ch.delta.df_noise[noise] = bs.read_bool()?;
    }
    Ok(())
}

/// Read sbr_invf() (Table 4.71).
fn read_invf(bs: &mut SbrBitReader<'_>, n_q: usize, ch: &mut ChannelData) -> Result<()> {
    for mode in ch.invf_mode.iter_mut().take(n_q) {
        *mode = bs.read(2)? as u8;
    }
    Ok(())
}

/// Read sbr_envelope() (Table 4.72).
///
/// `balance` is true for the second channel of a coupled channel pair (bs_coupling && ch).
fn read_envelope(
    bs: &mut SbrBitReader<'_>,
    tables: &FreqBandTables,
    ch: &mut ChannelData,
    balance: bool,
    bs_amp_res: bool,
) -> Result<()> {
    // The Huffman tables and the length of the start value.
    let (t_huff, f_huff, start_bits) = match (balance, bs_amp_res) {
        (true, true) => (SbrHuffmanTable::TEnvBal3_0dB, SbrHuffmanTable::FEnvBal3_0dB, 5),
        (true, false) => (SbrHuffmanTable::TEnvBal1_5dB, SbrHuffmanTable::FEnvBal1_5dB, 6),
        (false, true) => (SbrHuffmanTable::TEnv3_0dB, SbrHuffmanTable::FEnv3_0dB, 6),
        (false, false) => (SbrHuffmanTable::TEnv1_5dB, SbrHuffmanTable::FEnv1_5dB, 7),
    };

    for env in 0..ch.grid.num_env {
        let n = num_env_bands(tables, ch.grid.freq_res[env])?;
        let data = &mut ch.delta.data_env[env];

        if !ch.delta.df_env[env] {
            // bs_env_start_value_balance or bs_env_start_value_level.
            data[0] = bs.read(start_bits)? as i32;
            for value in data.iter_mut().take(n).skip(1) {
                *value = bs.read_huff(f_huff)?;
            }
        }
        else {
            for value in data.iter_mut().take(n) {
                *value = bs.read_huff(t_huff)?;
            }
        }
    }

    Ok(())
}

/// Read sbr_noise() (Table 4.73).
///
/// `balance` is true for the second channel of a coupled channel pair (bs_coupling && ch).
fn read_noise(
    bs: &mut SbrBitReader<'_>,
    n_q: usize,
    ch: &mut ChannelData,
    balance: bool,
) -> Result<()> {
    // f_huffman_noise_3_0dB and f_huffman_noise_bal_3_0dB are identical to f_huffman_env_3_0dB and
    // f_huffman_env_bal_3_0dB (Table 4.A.78).
    let (t_huff, f_huff) = if balance {
        (SbrHuffmanTable::TNoiseBal3_0dB, SbrHuffmanTable::FEnvBal3_0dB)
    }
    else {
        (SbrHuffmanTable::TNoise3_0dB, SbrHuffmanTable::FEnv3_0dB)
    };

    for noise in 0..ch.grid.num_noise {
        let data = &mut ch.delta.data_noise[noise];

        if !ch.delta.df_noise[noise] {
            // bs_noise_start_value_balance or bs_noise_start_value_level.
            data[0] = bs.read(5)? as i32;
            for value in data.iter_mut().take(n_q).skip(1) {
                *value = bs.read_huff(f_huff)?;
            }
        }
        else {
            for value in data.iter_mut().take(n_q) {
                *value = bs.read_huff(t_huff)?;
            }
        }
    }

    Ok(())
}

/// Read bs_add_harmonic_flag and, if set, sbr_sinusoidal_coding() (Table 4.74).
fn read_sinusoidal_coding(
    bs: &mut SbrBitReader<'_>,
    tables: &FreqBandTables,
    ch: &mut ChannelData,
) -> Result<()> {
    if bs.read_bool()? {
        let n_high = num_env_bands(tables, true)?;
        for value in ch.add_harmonic.iter_mut().take(n_high) {
            *value = bs.read_bool()?;
        }
    }
    Ok(())
}

/// Read bs_extended_data and, if set, the sbr_extension() elements (Tables 4.65, 4.66, and 8.A.1).
///
/// Returns true if ps_data() was read.
fn read_extended_data(bs: &mut SbrBitReader<'_>, mut ps: Option<&mut PsDecoder>) -> Result<bool> {
    let mut ps_data_present = false;

    if !bs.read_bool()? {
        return Ok(ps_data_present);
    }

    let mut cnt = bs.read(4)? as usize;
    if cnt == 15 {
        cnt += bs.read(8)? as usize;
    }

    let mut num_bits_left = 8 * cnt;

    if num_bits_left > bs.bits_left() {
        return decode_error("sbr: extended data exceeds the payload");
    }

    while num_bits_left > 7 {
        let bs_extension_id = bs.read(2)?;
        num_bits_left -= 2;

        match (bs_extension_id, ps.as_deref_mut()) {
            (EXTENSION_ID_PS, Some(ps)) => {
                let start = bs.pos();

                ps.read_ps_data(&mut bs.inner, num_bits_left)?;

                // Account for the bits actually read by ps_data().
                let num_read = bs.pos().saturating_sub(start);

                if num_read > num_bits_left || bs.pos() > bs.num_bits {
                    return decode_error("sbr: ps_data() exceeds the extended data");
                }

                num_bits_left -= num_read;
                ps_data_present = true;
            }
            _ => {
                // Unsupported or unused extension: skip the remaining bits.
                bs.skip(num_bits_left)?;
                num_bits_left = 0;
            }
        }
    }

    // bs_fill_bits.
    bs.skip(num_bits_left)?;

    Ok(ps_data_present)
}

/// Read sbr_single_channel_element() (Table 4.65), and decode and dequantize the scalefactors.
fn decode_sce(
    bs: &mut SbrBitReader<'_>,
    header: &SbrHeader,
    tables: &FreqBandTables,
    history: &mut [DeltaHistory; 2],
    ps: Option<&mut PsDecoder>,
    frame: &mut SbrFrame,
) -> Result<()> {
    let n_q = num_noise_bands(tables)?;

    let mut ch = ChannelData::default();

    // bs_data_extra and bs_reserved.
    if bs.read_bool()? {
        bs.skip(4)?;
    }

    // The amplitude resolution may be changed by sbr_grid() for this frame.
    let mut bs_amp_res = header.bs_amp_res;

    ch.set_grid(read_grid(bs, &mut bs_amp_res)?);
    read_dtdf(bs, &mut ch)?;
    read_invf(bs, n_q, &mut ch)?;
    read_envelope(bs, tables, &mut ch, false, bs_amp_res)?;
    read_noise(bs, n_q, &mut ch, false)?;
    read_sinusoidal_coding(bs, tables, &mut ch)?;

    // Decode and dequantize. This is done before the extended data is read, such that ps_data()
    // is only passed to the PS decoder if the SBR data of the frame is valid.
    let mut sf = Scalefactors::default();
    decode_scalefactors(&ch.delta, tables, 1, &mut history[0], &mut sf)?;

    let out = &mut frame.channels[0];
    ch.write_to(out);
    dequantize(&ch.delta, &sf, tables, bs_amp_res, out)?;

    frame.ps_data_present = read_extended_data(bs, ps)?;

    Ok(())
}

/// Read sbr_channel_pair_element() (Table 4.66), and decode, dequantize, and stereo decode the
/// scalefactors.
fn decode_cpe(
    bs: &mut SbrBitReader<'_>,
    header: &SbrHeader,
    tables: &FreqBandTables,
    history: &mut [DeltaHistory; 2],
    frame: &mut SbrFrame,
) -> Result<()> {
    let n_q = num_noise_bands(tables)?;

    let mut ch0 = ChannelData::default();
    let mut ch1 = ChannelData::default();

    // bs_data_extra and 2 * bs_reserved.
    if bs.read_bool()? {
        bs.skip(8)?;
    }

    let bs_coupling = bs.read_bool()?;

    // The amplitude resolution may be changed by sbr_grid() for this frame (Table 4.69). Without
    // coupling, the channels are treated as single channel elements (4.6.18.3.5), so the amplitude
    // resolution of each channel is changed by its own sbr_grid() only. With coupling, both
    // channels share sbr_grid(0).
    let mut bs_amp_res = header.bs_amp_res;
    let mut bs_amp_res1 = header.bs_amp_res;

    if bs_coupling {
        // Both channels share the time/frequency grid and the inverse filtering modes.
        let grid = read_grid(bs, &mut bs_amp_res)?;
        ch0.set_grid(grid);
        ch1.set_grid(grid);

        read_dtdf(bs, &mut ch0)?;
        read_dtdf(bs, &mut ch1)?;
        read_invf(bs, n_q, &mut ch0)?;
        ch1.invf_mode = ch0.invf_mode;

        read_envelope(bs, tables, &mut ch0, false, bs_amp_res)?;
        read_noise(bs, n_q, &mut ch0, false)?;
        read_envelope(bs, tables, &mut ch1, true, bs_amp_res)?;
        read_noise(bs, n_q, &mut ch1, true)?;
    }
    else {
        ch0.set_grid(read_grid(bs, &mut bs_amp_res)?);
        ch1.set_grid(read_grid(bs, &mut bs_amp_res1)?);

        read_dtdf(bs, &mut ch0)?;
        read_dtdf(bs, &mut ch1)?;
        read_invf(bs, n_q, &mut ch0)?;
        read_invf(bs, n_q, &mut ch1)?;

        read_envelope(bs, tables, &mut ch0, false, bs_amp_res)?;
        read_envelope(bs, tables, &mut ch1, false, bs_amp_res1)?;
        read_noise(bs, n_q, &mut ch0, false)?;
        read_noise(bs, n_q, &mut ch1, false)?;
    }

    read_sinusoidal_coding(bs, tables, &mut ch0)?;
    read_sinusoidal_coding(bs, tables, &mut ch1)?;

    // Parametric stereo is only defined for single channel elements: skip any ps_data().
    frame.ps_data_present = read_extended_data(bs, None)?;

    // Decode and dequantize.
    let [history0, history1] = history;

    let mut sf0 = Scalefactors::default();
    let mut sf1 = Scalefactors::default();

    let [out0, out1] = &mut frame.channels;

    ch0.write_to(out0);
    ch1.write_to(out1);

    if bs_coupling {
        // δ = 2 for the second channel of a coupled channel pair.
        decode_scalefactors(&ch0.delta, tables, 1, history0, &mut sf0)?;
        decode_scalefactors(&ch1.delta, tables, 2, history1, &mut sf1)?;
        dequantize_coupled(&ch0.delta, &sf0, &sf1, tables, bs_amp_res, out0, out1)?;
    }
    else {
        decode_scalefactors(&ch0.delta, tables, 1, history0, &mut sf0)?;
        decode_scalefactors(&ch1.delta, tables, 1, history1, &mut sf1)?;
        dequantize(&ch0.delta, &sf0, tables, bs_amp_res, out0)?;
        dequantize(&ch1.delta, &sf1, tables, bs_amp_res1, out1)?;
    }

    Ok(())
}

/// An SBR header together with the frequency band tables derived from it.
struct SbrConfig {
    header: SbrHeader,
    /// Fs_SBR used to derive the tables.
    fs_sbr: u32,
    tables: FreqBandTables,
}

impl SbrConfig {
    fn new(header: SbrHeader, fs_sbr: u32) -> Result<Self> {
        let tables = derive_freq_band_tables(&header, fs_sbr)?;
        Ok(Self { header, fs_sbr, tables })
    }
}

/// The change of the SBR configuration requested by an SBR frame.
enum ConfigUpdate {
    /// Keep the current configuration.
    Keep,
    /// Replace the header only. The frequency band tables are unaffected.
    Header(SbrHeader),
    /// Replace the configuration without a reset (only the limiter band table changed).
    Replace(SbrConfig),
    /// Replace the configuration and reset the SBR system (reset = 1).
    Reset(SbrConfig),
}

/// The persistent SBR bitstream decoding state of one syntactic element (SCE or CPE).
pub struct SbrBitstreamDecoder {
    /// True for a channel pair element.
    is_cpe: bool,
    /// The active SBR header and frequency band tables, if a header was received.
    config: Option<SbrConfig>,
    /// The history for delta decoding in the time direction of each channel.
    history: [DeltaHistory; 2],
}

impl SbrBitstreamDecoder {
    /// Create a decoder for a single channel element (`is_cpe == false`) or a channel pair element.
    pub fn new(is_cpe: bool) -> Self {
        Self { is_cpe, config: None, history: Default::default() }
    }

    /// Reset all state, as if no SBR data was ever decoded.
    #[cfg(test)]
    pub fn reset(&mut self) {
        self.config = None;
        self.history = Default::default();
    }

    /// Discard the state that depends on previously decoded frames (e.g., after a seek), but keep
    /// the most recently received SBR header and the frequency band tables derived from it, so that
    /// decoding can resume without waiting for the next SBR header.
    pub fn flush(&mut self) {
        self.history = Default::default();
    }

    /// The most recently received SBR header, if any.
    pub fn header(&self) -> Option<&SbrHeader> {
        self.config.as_ref().map(|config| &config.header)
    }

    /// The frequency band tables derived from the most recently received SBR header, if any.
    pub fn freq_bands(&self) -> Option<&FreqBandTables> {
        self.config.as_ref().map(|config| &config.tables)
    }

    /// Decode one sbr_extension_data() (Table 4.62).
    ///
    /// `payload` holds the extension payload following the 4-bit extension_type of the fill
    /// element, and `num_bits` is its exact length in bits. `crc` is true for EXT_SBR_DATA_CRC.
    /// `sbr_rate` is the SBR sampling frequency. If `ps` is provided, ps_data() carried in
    /// sbr_extension() is passed to it.
    ///
    /// Returns `Ok(None)` until the first SBR header is received. On error, the SBR header and
    /// frequency band tables are kept, but the history for delta decoding in the time direction is
    /// discarded.
    pub fn decode(
        &mut self,
        payload: &[u8],
        num_bits: usize,
        crc: bool,
        sbr_rate: u32,
        ps: Option<&mut PsDecoder>,
    ) -> Result<Option<SbrFrame>> {
        let result = self.decode_frame(payload, num_bits, crc, sbr_rate, ps);

        if result.is_err() {
            for history in self.history.iter_mut() {
                history.invalidate();
            }
        }

        result
    }

    fn decode_frame(
        &mut self,
        payload: &[u8],
        num_bits: usize,
        crc: bool,
        sbr_rate: u32,
        ps: Option<&mut PsDecoder>,
    ) -> Result<Option<SbrFrame>> {
        let Some(buf) = payload.get(..num_bits.div_ceil(8))
        else {
            return decode_error("sbr: payload is shorter than its length");
        };

        let mut bs = SbrBitReader::new(buf, num_bits);

        // bs_sbr_crc_bits covers all bits following it (4.5.2.8.1).
        if crc {
            let bs_sbr_crc_bits = bs.read(10)?;

            if u32::from(crc::sbr_crc(buf, 10, num_bits)) != bs_sbr_crc_bits {
                return decode_error("sbr: crc check failed");
            }
        }

        // bs_header_flag.
        let new_header = if bs.read_bool()? { Some(read_header(&mut bs)?) } else { None };

        let fs_sbr = sbr_sampling_frequency(sbr_rate);

        let update = match (&self.config, new_header) {
            // No SBR header was received yet: the frame cannot be decoded.
            (None, None) => return Ok(None),
            (None, Some(header)) => ConfigUpdate::Reset(SbrConfig::new(header, fs_sbr)?),
            (Some(config), Some(header)) => {
                if header_requires_reset(&config.header, &header) || config.fs_sbr != fs_sbr {
                    ConfigUpdate::Reset(SbrConfig::new(header, fs_sbr)?)
                }
                else if header.bs_limiter_bands != config.header.bs_limiter_bands {
                    let mut tables = config.tables.clone();
                    tables.f_table_lim = limiter_table(
                        header.bs_limiter_bands,
                        &tables.f_table_low,
                        tables.kx,
                        &tables.patches,
                    )?;
                    ConfigUpdate::Replace(SbrConfig { header, fs_sbr, tables })
                }
                else if header != config.header {
                    ConfigUpdate::Header(header)
                }
                else {
                    ConfigUpdate::Keep
                }
            }
            (Some(config), None) => {
                if config.fs_sbr != fs_sbr {
                    ConfigUpdate::Reset(SbrConfig::new(config.header, fs_sbr)?)
                }
                else {
                    ConfigUpdate::Keep
                }
            }
        };

        // The header and frequency band tables to decode this frame with.
        let (header, tables, reset) = match (&update, &self.config) {
            (ConfigUpdate::Reset(config), _) => (&config.header, &config.tables, true),
            (ConfigUpdate::Replace(config), _) => (&config.header, &config.tables, false),
            (ConfigUpdate::Header(header), Some(config)) => (header, &config.tables, false),
            (ConfigUpdate::Keep, Some(config)) => (&config.header, &config.tables, false),
            _ => return decode_error("sbr: no sbr header"),
        };

        // Decode using a copy of the history, such that it is only updated on success.
        let mut history = self.history;

        // Delta coding in the time direction across a reset is not possible.
        if reset {
            for history in history.iter_mut() {
                history.invalidate();
            }
        }

        let mut frame = SbrFrame { reset, ..Default::default() };

        if self.is_cpe {
            decode_cpe(&mut bs, header, tables, &mut history, &mut frame)?;
        }
        else {
            decode_sce(&mut bs, header, tables, &mut history, ps, &mut frame)?;
        }

        // The frame was decoded successfully: commit the new state.
        match update {
            ConfigUpdate::Keep => (),
            ConfigUpdate::Header(header) => {
                if let Some(config) = self.config.as_mut() {
                    config.header = header;
                }
            }
            ConfigUpdate::Replace(config) | ConfigUpdate::Reset(config) => {
                self.config = Some(config);
            }
        }

        self.history = history;

        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::aac::sbr::types::MAX_ENVELOPES;

    use SbrHuffmanTable::*;

    /// Writes bits, most-significant bit first.
    #[derive(Default)]
    struct BitWriter {
        buf: Vec<u8>,
        len: usize,
    }

    impl BitWriter {
        fn put(&mut self, value: u32, bits: u32) {
            for i in (0..bits).rev() {
                if self.len % 8 == 0 {
                    self.buf.push(0);
                }
                if (value >> i) & 1 != 0 {
                    *self.buf.last_mut().unwrap() |= 0x80 >> (self.len % 8);
                }
                self.len += 1;
            }
        }

        fn put_bool(&mut self, value: bool) {
            self.put(u32::from(value), 1);
        }

        fn put_huff(&mut self, table: SbrHuffmanTable, value: i32) {
            let (len, code) = table.codes()[(value + table.lav()) as usize];
            self.put(code, u32::from(len));
        }

        fn put_huffs(&mut self, table: SbrHuffmanTable, values: &[i32]) {
            for &value in values {
                self.put_huff(table, value);
            }
        }

        /// Start a payload. If `crc` is true, space for bs_sbr_crc_bits is reserved.
        fn new_payload(crc: bool) -> Self {
            let mut w = BitWriter::default();
            if crc {
                w.put(0, 10);
            }
            w
        }

        /// Complete an sbr_extension_data() payload by appending bs_fill_bits such that the payload
        /// and the 4-bit extension_type occupy an integer number of bytes. If `crc` is true,
        /// bs_sbr_crc_bits is computed and inserted.
        fn finish(mut self, crc: bool) -> (Vec<u8>, usize) {
            while (self.len + 4) % 8 != 0 {
                self.put(0, 1);
            }
            if crc {
                let value = crc::sbr_crc(&self.buf, 10, self.len);
                self.buf[0] = (value >> 2) as u8;
                self.buf[1] = (self.buf[1] & 0x3f) | (((value & 3) as u8) << 6);
            }
            (self.buf, self.len)
        }
    }

    const SBR_RATE: u32 = 44100;

    /// Write bs_header_flag = 1 and sbr_header() with bs_amp_res = 1, bs_start_freq = 5,
    /// bs_stop_freq = 8, the given bs_xover_band, and without the optional header parts.
    ///
    /// At Fs_SBR = 44100, the header with bs_xover_band = 0 results in
    /// f_TableHigh = [14, 15, 16, 17, 18, 19, 20, 22, 24, 26, 28, 31, 35, 39, 43] (N_High = 14),
    /// f_TableLow = [14, 16, 18, 20, 24, 28, 35, 43] (N_Low = 7), and
    /// f_TableNoise = [14, 18, 24, 43] (N_Q = 3). See the tests of the bands module.
    fn put_header(w: &mut BitWriter, xover: u32) {
        w.put_bool(true); // bs_header_flag
        w.put_bool(true); // bs_amp_res
        w.put(5, 4); // bs_start_freq
        w.put(8, 4); // bs_stop_freq
        w.put(xover, 3); // bs_xover_band
        w.put(0, 2); // bs_reserved
        w.put_bool(false); // bs_header_extra_1
        w.put_bool(false); // bs_header_extra_2
    }

    /// The expected E(k, 0) of `frame1`.
    const FRAME1_E0: [i32; 14] = [20, 21, 20, 20, 22, 19, 19, 19, 20, 21, 19, 19, 19, 23];
    /// The expected E(k, 1) of `frame1`.
    const FRAME1_E1: [i32; 14] = [19, 21, 21, 20, 22, 19, 19, 19, 20, 21, 19, 19, 19, 25];
    /// The expected Q(k, 0) and Q(k, 1) of `frame1`.
    const FRAME1_Q: [[i32; 3]; 2] = [[10, 12, 11], [10, 10, 12]];
    /// The bs_add_harmonic of `frame1`.
    const FRAME1_HARMONICS: [bool; 14] = [
        false, true, false, false, true, true, false, false, false, false, false, false, true,
        false,
    ];

    /// A single channel element frame with a header, and a FIXFIX grid with two high frequency
    /// resolution envelopes.
    fn frame1(crc: bool) -> (Vec<u8>, usize) {
        let mut w = BitWriter::new_payload(crc);
        put_header(&mut w, 0);

        w.put_bool(false); // bs_data_extra

        // sbr_grid(): FIXFIX, tmp = 1 (2 envelopes), bs_freq_res = 1.
        w.put(0, 2);
        w.put(1, 2);
        w.put_bool(true);

        // sbr_dtdf(): bs_df_env = [0, 1], bs_df_noise = [0, 1].
        w.put_bool(false);
        w.put_bool(true);
        w.put_bool(false);
        w.put_bool(true);

        // sbr_invf(): N_Q = 3.
        w.put(0, 2);
        w.put(1, 2);
        w.put(3, 2);

        // sbr_envelope(): bs_amp_res = 1. Envelope 0 is delta coded in frequency with a 6-bit
        // start value, envelope 1 in time.
        w.put(20, 6);
        w.put_huffs(FEnv3_0dB, &[1, -1, 0, 2, -3, 0, 0, 1, 1, -2, 0, 0, 4]);
        w.put_huffs(TEnv3_0dB, &[-1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);

        // sbr_noise(): noise floor 0 delta coded in frequency, noise floor 1 in time.
        w.put(10, 5);
        w.put_huffs(FEnv3_0dB, &[2, -1]);
        w.put_huffs(TNoise3_0dB, &[0, -2, 1]);

        // bs_add_harmonic_flag = 1, sbr_sinusoidal_coding().
        w.put_bool(true);
        for h in FRAME1_HARMONICS {
            w.put_bool(h);
        }

        // bs_extended_data = 0.
        w.put_bool(false);

        w.finish(crc)
    }

    /// The expected E(k, 0) of `frame2`.
    const FRAME2_E: [i32; 7] = [20, 21, 21, 21, 20, 19, 16];
    /// The expected Q(k, 0) of `frame2`.
    const FRAME2_Q: [i32; 3] = [11, 11, 11];

    /// A single channel element frame without header following `frame1`, with a single low
    /// frequency resolution FIXFIX envelope delta coded in time.
    fn frame2() -> (Vec<u8>, usize) {
        let mut w = BitWriter::default();
        w.put_bool(false); // bs_header_flag
        w.put_bool(false); // bs_data_extra

        // sbr_grid(): FIXFIX, tmp = 0 (1 envelope, thus bs_amp_res = 0), bs_freq_res = 0.
        w.put(0, 2);
        w.put(0, 2);
        w.put_bool(false);

        // sbr_dtdf(): bs_df_env = [1], bs_df_noise = [1].
        w.put_bool(true);
        w.put_bool(true);

        // sbr_invf().
        w.put(2, 2);
        w.put(2, 2);
        w.put(2, 2);

        // sbr_envelope(): bs_amp_res = 0, delta coded in time. The previous envelope has high
        // frequency resolution: i(k) = [0, 2, 4, 6, 8, 10, 12], E'(i(k)) = [19, 21, 22, 19, 20,
        // 19, 19].
        w.put_huffs(TEnv1_5dB, &[1, 0, -1, 2, 0, 0, -3]);

        // sbr_noise(): delta coded in time with respect to Q'(k) = [10, 10, 12].
        w.put_huffs(TNoise3_0dB, &[1, 1, -1]);

        w.put_bool(false); // bs_add_harmonic_flag
        w.put_bool(false); // bs_extended_data

        w.finish(false)
    }

    fn assert_close(actual: f32, expected: f64) {
        let tolerance = 1e-5 * expected.abs().max(1e-9);
        assert!((f64::from(actual) - expected).abs() <= tolerance, "{} != {}", actual, expected);
    }

    fn check_frame1(frame: &SbrFrame) {
        let ch = &frame.channels[0];

        assert_eq!(ch.num_env, 2);
        assert_eq!(ch.t_env[..3], [0, 8, 16]);
        assert_eq!(ch.freq_res[..2], [true, true]);
        assert_eq!(ch.num_noise, 2);
        assert_eq!(ch.t_noise[..3], [0, 8, 16]);
        assert_eq!(ch.l_a, None);
        assert_eq!(ch.invf_mode[..3], [0, 1, 3]);
        assert_eq!(ch.add_harmonic[..14], FRAME1_HARMONICS);
        assert!(ch.add_harmonic[14..].iter().all(|&h| !h));

        // bs_amp_res = 1: E_Orig = 64 * 2^E.
        for (l, e) in [FRAME1_E0, FRAME1_E1].iter().enumerate() {
            for k in 0..14 {
                assert_close(ch.e_orig[l][k], 64.0 * f64::from(e[k]).exp2());
            }
        }

        // Q_Orig = 2^(6 - Q).
        for l in 0..2 {
            for k in 0..3 {
                assert_close(ch.q_orig[l][k], f64::from(6 - FRAME1_Q[l][k]).exp2());
            }
        }

        assert!(!frame.ps_data_present);
        assert_eq!(frame.channels[1], SbrChannelFrame::default());
    }

    fn check_frame2(frame: &SbrFrame) {
        let ch = &frame.channels[0];

        assert!(!frame.reset);
        assert_eq!(ch.num_env, 1);
        assert_eq!(ch.t_env[..2], [0, 16]);
        assert!(!ch.freq_res[0]);
        assert_eq!(ch.num_noise, 1);
        assert_eq!(ch.t_noise[..2], [0, 16]);
        assert_eq!(ch.invf_mode[..3], [2, 2, 2]);
        assert!(ch.add_harmonic.iter().all(|&h| !h));

        // bs_amp_res = 0: E_Orig = 64 * 2^(E / 2).
        for k in 0..7 {
            assert_close(ch.e_orig[0][k], 64.0 * (f64::from(FRAME2_E[k]) / 2.0).exp2());
        }
        for k in 0..3 {
            assert_close(ch.q_orig[0][k], f64::from(6 - FRAME2_Q[k]).exp2());
        }
    }

    #[test]
    fn verify_sce_frames() {
        let mut dec = SbrBitstreamDecoder::new(false);
        assert!(dec.header().is_none() && dec.freq_bands().is_none());

        let (buf, len) = frame1(false);
        let frame = dec.decode(&buf, len, false, SBR_RATE, None).unwrap().unwrap();
        assert!(frame.reset);
        check_frame1(&frame);

        let tables = dec.freq_bands().unwrap();
        assert_eq!(tables.f_table_low, [14, 16, 18, 20, 24, 28, 35, 43]);
        assert_eq!(tables.f_table_noise, [14, 18, 24, 43]);

        // The second frame uses delta coding in the time direction across the frame boundary.
        let (buf, len) = frame2();
        let frame = dec.decode(&buf, len, false, SBR_RATE, None).unwrap().unwrap();
        check_frame2(&frame);

        // Repeating the first frame with an identical header does not reset.
        let (buf, len) = frame1(false);
        let frame = dec.decode(&buf, len, false, SBR_RATE, None).unwrap().unwrap();
        assert!(!frame.reset);
        check_frame1(&frame);
    }

    #[test]
    fn verify_sce_frames_with_crc() {
        let mut dec = SbrBitstreamDecoder::new(false);

        let (buf, len) = frame1(true);
        let frame = dec.decode(&buf, len, true, SBR_RATE, None).unwrap().unwrap();
        check_frame1(&frame);

        // Any bit error is detected.
        for bit in [0, 9, 10, 40, len - 1] {
            let mut corrupt = buf.clone();
            corrupt[bit / 8] ^= 0x80 >> (bit % 8);
            assert!(dec.decode(&corrupt, len, true, SBR_RATE, None).is_err());
        }

        // The error discards the history: the second frame cannot be decoded.
        let (buf2, len2) = frame2();
        assert!(dec.decode(&buf2, len2, false, SBR_RATE, None).is_err());

        // The header and tables are intact, and decoding resumes.
        assert!(dec.header().is_some());
        let frame = dec.decode(&buf, len, true, SBR_RATE, None).unwrap().unwrap();
        assert!(!frame.reset);
        check_frame1(&frame);
        let frame = dec.decode(&buf2, len2, false, SBR_RATE, None).unwrap().unwrap();
        check_frame2(&frame);
    }

    #[test]
    fn verify_no_header() {
        let mut dec = SbrBitstreamDecoder::new(false);

        // Without a header, the frame cannot be decoded.
        let (buf, len) = frame2();
        assert!(dec.decode(&buf, len, false, SBR_RATE, None).unwrap().is_none());
        assert!(dec.header().is_none());

        // After a reset, a header is required again.
        let (buf1, len1) = frame1(false);
        assert!(dec.decode(&buf1, len1, false, SBR_RATE, None).unwrap().is_some());
        dec.reset();
        assert!(dec.header().is_none() && dec.freq_bands().is_none());
        assert!(dec.decode(&buf, len, false, SBR_RATE, None).unwrap().is_none());
    }

    #[test]
    fn verify_header_defaults() {
        let mut dec = SbrBitstreamDecoder::new(false);

        let (buf, len) = frame1(false);
        dec.decode(&buf, len, false, SBR_RATE, None).unwrap();

        let expected = SbrHeader {
            bs_amp_res: true,
            bs_start_freq: 5,
            bs_stop_freq: 8,
            bs_xover_band: 0,
            bs_freq_scale: 2,
            bs_alter_scale: true,
            bs_noise_bands: 2,
            bs_limiter_bands: 2,
            bs_limiter_gains: 2,
            bs_interpol_freq: true,
            bs_smoothing_mode: true,
        };
        assert_eq!(dec.header(), Some(&expected));

        // A header with both optional parts.
        let mut w = BitWriter::default();
        w.put_bool(false); // bs_amp_res
        w.put(3, 4); // bs_start_freq
        w.put(10, 4); // bs_stop_freq
        w.put(2, 3); // bs_xover_band
        w.put(0, 2); // bs_reserved
        w.put_bool(true); // bs_header_extra_1
        w.put_bool(true); // bs_header_extra_2
        w.put(1, 2); // bs_freq_scale
        w.put_bool(false); // bs_alter_scale
        w.put(1, 2); // bs_noise_bands
        w.put(0, 2); // bs_limiter_bands
        w.put(3, 2); // bs_limiter_gains
        w.put_bool(false); // bs_interpol_freq
        w.put_bool(false); // bs_smoothing_mode
        let (buf, len) = w.finish(false);
        let mut bs = SbrBitReader::new(&buf, len);
        let header = read_header(&mut bs).unwrap();
        assert_eq!(
            header,
            SbrHeader {
                bs_amp_res: false,
                bs_start_freq: 3,
                bs_stop_freq: 10,
                bs_xover_band: 2,
                bs_freq_scale: 1,
                bs_alter_scale: false,
                bs_noise_bands: 1,
                bs_limiter_bands: 0,
                bs_limiter_gains: 3,
                bs_interpol_freq: false,
                bs_smoothing_mode: false,
            }
        );
        assert_eq!(bs.pos(), 27);
    }

    #[test]
    fn verify_reset_on_header_change() {
        let mut dec = SbrBitstreamDecoder::new(false);

        let (buf, len) = frame1(false);
        dec.decode(&buf, len, false, SBR_RATE, None).unwrap().unwrap();

        // A header with a different bs_xover_band followed by a time delta coded frame: the reset
        // discards the history, thus the frame cannot be decoded.
        let mut w = BitWriter::default();
        put_header(&mut w, 1);
        w.put_bool(false); // bs_data_extra
        w.put(0, 2); // FIXFIX
        w.put(0, 2);
        w.put_bool(false);
        w.put_bool(true); // bs_df_env
        w.put_bool(false); // bs_df_noise
        w.put(0, 6); // sbr_invf()
        // N_High = 13, N_Low = 7.
        w.put_huffs(TEnv1_5dB, &[0; 7]);
        w.put(10, 5);
        w.put_huffs(FEnv3_0dB, &[0, 0]);
        w.put_bool(false);
        w.put_bool(false);
        let (bad, bad_len) = w.finish(false);

        assert!(dec.decode(&bad, bad_len, false, SBR_RATE, None).is_err());

        // The failed frame did not change the header.
        assert_eq!(dec.header().unwrap().bs_xover_band, 0);
        assert_eq!(dec.freq_bands().unwrap().kx, 14);

        // The same header with delta coding in the frequency direction is accepted.
        let mut w = BitWriter::default();
        put_header(&mut w, 1);
        w.put_bool(false); // bs_data_extra
        w.put(0, 2); // FIXFIX
        w.put(0, 2);
        w.put_bool(false);
        w.put_bool(false); // bs_df_env
        w.put_bool(false); // bs_df_noise
        w.put(0, 6); // sbr_invf()
        w.put(30, 7);
        w.put_huffs(FEnv1_5dB, &[0; 6]);
        w.put(10, 5);
        w.put_huffs(FEnv3_0dB, &[0, 0]);
        w.put_bool(false);
        w.put_bool(false);
        let (good, good_len) = w.finish(false);

        let frame = dec.decode(&good, good_len, false, SBR_RATE, None).unwrap().unwrap();
        assert!(frame.reset);
        assert_eq!(dec.header().unwrap().bs_xover_band, 1);
        let tables = dec.freq_bands().unwrap();
        assert_eq!((tables.kx, tables.m), (15, 28));
        // N_High = 13 is odd, i(k) = 2k - 1.
        assert_eq!(tables.f_table_low, [15, 16, 18, 20, 24, 28, 35, 43]);
        assert_close(frame.channels[0].e_orig[0][6], 64.0 * 2f64.powi(15));

        // A change of the sampling rate also resets. Build a frame for the tables of the same
        // header at Fs_SBR = 48000.
        let tables = derive_freq_band_tables(dec.header().unwrap(), 48000).unwrap();
        let n_low = tables.f_table_low.len() - 1;
        let n_q = tables.f_table_noise.len() - 1;

        let mut w = BitWriter::default();
        w.put_bool(false); // bs_header_flag
        w.put_bool(false); // bs_data_extra
        w.put(0, 2); // FIXFIX
        w.put(0, 2);
        w.put_bool(false);
        w.put_bool(false); // bs_df_env
        w.put_bool(false); // bs_df_noise
        w.put(0, 2 * n_q as u32); // sbr_invf()
        w.put(30, 7);
        w.put_huffs(FEnv1_5dB, &vec![0; n_low - 1]);
        w.put(10, 5);
        w.put_huffs(FEnv3_0dB, &vec![0; n_q - 1]);
        w.put_bool(false);
        w.put_bool(false);
        let (buf, len) = w.finish(false);

        let frame = dec.decode(&buf, len, false, 48000, None).unwrap().unwrap();
        assert!(frame.reset);
        assert_eq!(dec.freq_bands(), Some(&tables));
        let frame = dec.decode(&buf, len, false, 48000, None).unwrap().unwrap();
        assert!(!frame.reset);
    }

    #[test]
    fn verify_limiter_bands_change() {
        let mut dec = SbrBitstreamDecoder::new(false);

        let (buf, len) = frame1(false);
        dec.decode(&buf, len, false, SBR_RATE, None).unwrap().unwrap();
        assert_eq!(dec.freq_bands().unwrap().f_table_lim, [14, 18, 26, 35, 43]);

        // The same header with bs_limiter_bands = 0 and bs_interpol_freq = 0.
        let mut w = BitWriter::default();
        w.put_bool(true); // bs_header_flag
        w.put_bool(true);
        w.put(5, 4);
        w.put(8, 4);
        w.put(0, 3);
        w.put(0, 2);
        w.put_bool(false); // bs_header_extra_1
        w.put_bool(true); // bs_header_extra_2
        w.put(0, 2); // bs_limiter_bands
        w.put(2, 2); // bs_limiter_gains
        w.put_bool(false); // bs_interpol_freq
        w.put_bool(true); // bs_smoothing_mode
        w.put_bool(false); // bs_data_extra
        w.put(0, 2); // FIXFIX
        w.put(0, 2);
        w.put_bool(true);
        w.put_bool(true); // bs_df_env
        w.put_bool(true); // bs_df_noise
        w.put(0, 6); // sbr_invf()
        w.put_huffs(TEnv1_5dB, &[0; 14]);
        w.put_huffs(TNoise3_0dB, &[0; 3]);
        w.put_bool(false);
        w.put_bool(false);
        let (buf, len) = w.finish(false);

        // Not a reset: delta coding in the time direction remains possible.
        let frame = dec.decode(&buf, len, false, SBR_RATE, None).unwrap().unwrap();
        assert!(!frame.reset);
        assert_eq!(dec.freq_bands().unwrap().f_table_lim, [14, 43]);
        assert!(!dec.header().unwrap().bs_interpol_freq);

        // E(k, 0) = E'(k) (the last envelope of frame 1).
        for k in 0..14 {
            assert_close(
                frame.channels[0].e_orig[0][k],
                64.0 * (f64::from(FRAME1_E1[k]) / 2.0).exp2(),
            );
        }
    }

    /// A channel pair element frame with coupling and a header.
    fn cpe_coupled_frame() -> (Vec<u8>, usize) {
        let mut w = BitWriter::default();
        put_header(&mut w, 0);
        w.put_bool(false); // bs_data_extra
        w.put_bool(true); // bs_coupling

        // sbr_grid(0): FIXFIX, 1 envelope (bs_amp_res = 0), low frequency resolution.
        w.put(0, 2);
        w.put(0, 2);
        w.put_bool(false);

        // sbr_dtdf(0), sbr_dtdf(1).
        w.put(0, 2);
        w.put(0, 2);

        // sbr_invf(0).
        w.put(1, 2);
        w.put(2, 2);
        w.put(3, 2);

        // sbr_envelope(0, 1): E_0 = [40, 42, 40, 40, 40, 41, 40].
        w.put(40, 7);
        w.put_huffs(FEnv1_5dB, &[2, -2, 0, 0, 1, -1]);
        // sbr_noise(0, 1): Q_0 = [8, 9, 9].
        w.put(8, 5);
        w.put_huffs(FEnv3_0dB, &[1, 0]);
        // sbr_envelope(1, 1): balance, 6-bit start value, E_1 = 2 * [12, 13, 12, 12, 14, 14, 12].
        w.put(12, 6);
        w.put_huffs(FEnvBal1_5dB, &[1, -1, 0, 2, 0, -2]);
        // sbr_noise(1, 1): balance, Q_1 = 2 * [6, 7, 5].
        w.put(6, 5);
        w.put_huffs(FEnvBal3_0dB, &[1, -2]);

        w.put_bool(false); // bs_add_harmonic_flag[0]
        w.put_bool(true); // bs_add_harmonic_flag[1]
        w.put(0b10_0000_0000_0001, 14);
        w.put_bool(false); // bs_extended_data
        w.finish(false)
    }

    #[test]
    fn verify_cpe_coupled() {
        let (buf, len) = cpe_coupled_frame();

        let mut dec = SbrBitstreamDecoder::new(true);
        let frame = dec.decode(&buf, len, false, SBR_RATE, None).unwrap().unwrap();

        let e0 = [40, 42, 40, 40, 40, 41, 40];
        let e1 = [24, 26, 24, 24, 28, 28, 24];
        let q0 = [8, 9, 9];
        let q1 = [12, 14, 10];

        let [left, right] = &frame.channels;

        for ch in [left, right] {
            assert_eq!(ch.num_env, 1);
            assert_eq!(ch.t_env[..2], [0, 16]);
            assert_eq!(ch.num_noise, 1);
            assert_eq!(ch.invf_mode[..3], [1, 2, 3]);
        }

        assert!(left.add_harmonic.iter().all(|&h| !h));
        assert!(right.add_harmonic[0] && right.add_harmonic[13]);
        assert_eq!(right.add_harmonic.iter().filter(|&&h| h).count(), 2);

        // a = 2, panOffset(0) = 24.
        for k in 0..7 {
            let level = 64.0 * (f64::from(e0[k]) / 2.0 + 1.0).exp2();
            let ratio = f64::from(24 - e1[k]) / 2.0;
            assert_close(left.e_orig[0][k], level / (1.0 + ratio.exp2()));
            assert_close(right.e_orig[0][k], level / (1.0 + (-ratio).exp2()));
        }
        // E_0 = 40, E_1 = panOffset: both channels get 64 * 2^21 / 2.
        assert_eq!(left.e_orig[0][0], 67108864.0);
        assert_eq!(right.e_orig[0][0], 67108864.0);

        // panOffset(1) = 12.
        for k in 0..3 {
            let level = f64::from(6 - q0[k] + 1).exp2();
            let ratio = f64::from(12 - q1[k]);
            assert_close(left.q_orig[0][k], level / (1.0 + ratio.exp2()));
            assert_close(right.q_orig[0][k], level / (1.0 + (-ratio).exp2()));
        }
        assert_eq!((left.q_orig[0][0], right.q_orig[0][0]), (0.25, 0.25));
    }

    /// A channel pair element frame without coupling, where the second grid sets bs_amp_res = 0 for
    /// the second channel only.
    fn cpe_uncoupled_frame() -> (Vec<u8>, usize) {
        let mut w = BitWriter::default();
        put_header(&mut w, 0);
        w.put_bool(false); // bs_data_extra
        w.put_bool(false); // bs_coupling

        // sbr_grid(0): FIXFIX, 2 envelopes, low frequency resolution.
        w.put(0, 2);
        w.put(1, 2);
        w.put_bool(false);
        // sbr_grid(1): FIXFIX, 1 envelope (bs_amp_res = 0 for this frame), high resolution.
        w.put(0, 2);
        w.put(0, 2);
        w.put_bool(true);

        // sbr_dtdf(0), sbr_dtdf(1).
        w.put(0, 4);
        w.put(0, 2);

        // sbr_invf(0), sbr_invf(1).
        w.put(0, 6);
        w.put(0b11_10_01, 6);

        // sbr_envelope(0, 0): 2 low resolution envelopes with bs_amp_res = 1 of the header, and
        // thus 6-bit start values.
        w.put(10, 6);
        w.put_huffs(FEnv3_0dB, &[1; 6]);
        w.put(11, 6);
        w.put_huffs(FEnv3_0dB, &[-1; 6]);
        // sbr_envelope(1, 0): 1 high resolution envelope with a 7-bit start value.
        w.put(100, 7);
        w.put_huffs(FEnv1_5dB, &[-2; 13]);

        // sbr_noise(0, 0) with 2 noise floors, sbr_noise(1, 0).
        w.put(1, 5);
        w.put_huffs(FEnv3_0dB, &[1, 1]);
        w.put(2, 5);
        w.put_huffs(FEnv3_0dB, &[2, 2]);
        w.put(30, 5);
        w.put_huffs(FEnv3_0dB, &[-10, -10]);

        w.put_bool(false); // bs_add_harmonic_flag[0]
        w.put_bool(false); // bs_add_harmonic_flag[1]
        w.put_bool(false); // bs_extended_data
        w.finish(false)
    }

    #[test]
    fn verify_cpe_uncoupled_amp_res() {
        let (buf, len) = cpe_uncoupled_frame();

        let mut dec = SbrBitstreamDecoder::new(true);
        let frame = dec.decode(&buf, len, false, SBR_RATE, None).unwrap().unwrap();

        let [ch0, ch1] = &frame.channels;

        assert_eq!((ch0.num_env, ch0.num_noise), (2, 2));
        assert_eq!(ch0.t_env[..3], [0, 8, 16]);
        assert_eq!((ch1.num_env, ch1.num_noise), (1, 1));
        assert_eq!(ch1.invf_mode[..3], [3, 2, 1]);

        // a = 1 for the first channel, a = 2 for the second channel.
        for k in 0..7 {
            assert_close(ch0.e_orig[0][k], 64.0 * f64::from(10 + k as i32).exp2());
            assert_close(ch0.e_orig[1][k], 64.0 * f64::from(11 - k as i32).exp2());
        }
        for k in 0..14 {
            assert_close(ch1.e_orig[0][k], 64.0 * (f64::from(100 - 2 * k as i32) / 2.0).exp2());
        }

        assert_eq!(ch0.q_orig[0][..3], [32.0, 16.0, 8.0]);
        assert_eq!(ch0.q_orig[1][..3], [16.0, 4.0, 1.0]);
        assert_eq!(ch1.q_orig[0][..3], [2f32.powi(-24), 2f32.powi(-14), 2f32.powi(-4)]);
    }

    /// Write a single envelope, frequency delta coded, SCE frame without header and with the
    /// given extended data writer.
    fn sce_frame_with_extension(ext: impl Fn(&mut BitWriter)) -> (Vec<u8>, usize) {
        let mut w = BitWriter::default();
        w.put_bool(false); // bs_header_flag
        w.put_bool(true); // bs_data_extra
        w.put(0, 4); // bs_reserved
        w.put(0, 2); // FIXFIX
        w.put(0, 2);
        w.put_bool(false);
        w.put_bool(false); // bs_df_env
        w.put_bool(false); // bs_df_noise
        w.put(0, 6); // sbr_invf()
        w.put(30, 7);
        w.put_huffs(FEnv1_5dB, &[0; 6]);
        w.put(10, 5);
        w.put_huffs(FEnv3_0dB, &[0, 0]);
        w.put_bool(false);
        ext(&mut w);
        w.finish(false)
    }

    #[test]
    fn verify_extended_data() {
        let mut dec = SbrBitstreamDecoder::new(false);
        let (buf, len) = frame1(false);
        dec.decode(&buf, len, false, SBR_RATE, None).unwrap();

        // No extended data.
        let (buf, len) = sce_frame_with_extension(|w| w.put_bool(false));
        let frame = dec.decode(&buf, len, false, SBR_RATE, None).unwrap().unwrap();
        assert!(!frame.ps_data_present);
        assert_close(frame.channels[0].e_orig[0][0], 64.0 * 2f64.powi(15));

        // A PS extension of 2 bytes, skipped since there is no PS decoder.
        let (buf, len) = sce_frame_with_extension(|w| {
            w.put_bool(true);
            w.put(2, 4);
            w.put(EXTENSION_ID_PS, 2);
            w.put(0x1234, 14);
        });
        let frame = dec.decode(&buf, len, false, SBR_RATE, None).unwrap().unwrap();
        assert!(!frame.ps_data_present);

        // An escaped extension size: 15 + 3 bytes, with a reserved extension identifier.
        let (buf, len) = sce_frame_with_extension(|w| {
            w.put_bool(true);
            w.put(15, 4);
            w.put(3, 8);
            w.put(1, 2);
            for _ in 0..18 * 8 - 2 {
                w.put(0, 1);
            }
        });
        assert!(dec.decode(&buf, len, false, SBR_RATE, None).unwrap().is_some());

        // Extended data that exceeds the payload.
        let (buf, len) = sce_frame_with_extension(|w| {
            w.put_bool(true);
            w.put(2, 4);
            w.put(0, 8);
        });
        assert!(dec.decode(&buf, len, false, SBR_RATE, None).is_err());

        // Trailing bits after the SBR data are ignored.
        let (mut buf, len) = sce_frame_with_extension(|w| w.put_bool(false));
        buf.extend_from_slice(&[0xff, 0xff]);
        assert!(dec.decode(&buf, len + 16, false, SBR_RATE, None).unwrap().is_some());
    }

    #[test]
    fn verify_constraint_violations() {
        let (header_buf, header_len) = frame1(false);

        let new_decoder = || {
            let mut dec = SbrBitstreamDecoder::new(false);
            dec.decode(&header_buf, header_len, false, SBR_RATE, None).unwrap();
            dec
        };

        // A payload shorter than its length in bits.
        let mut dec = new_decoder();
        assert!(dec.decode(&header_buf, header_len + 9, false, SBR_RATE, None).is_err());

        // A truncated payload.
        for len in [0, 1, 10, 40, header_len - 20] {
            let mut dec = SbrBitstreamDecoder::new(false);
            assert!(dec.decode(&header_buf, len, false, SBR_RATE, None).is_err());
        }

        // A CRC payload too short for the CRC.
        let mut dec = new_decoder();
        assert!(dec.decode(&[0], 5, true, SBR_RATE, None).is_err());

        let frame = |grid: &dyn Fn(&mut BitWriter), noise_start: u32| {
            let mut w = BitWriter::default();
            w.put_bool(false); // bs_header_flag
            w.put_bool(false); // bs_data_extra
            grid(&mut w);
            w.put_bool(false); // bs_df_env
            w.put_bool(false); // bs_df_noise
            w.put(0, 6);
            w.put(30, 7);
            w.put_huffs(FEnv1_5dB, &[0; 6]);
            w.put(noise_start, 5);
            w.put_huffs(FEnv3_0dB, &[0, 0]);
            w.put_bool(false);
            w.put_bool(false);
            w.finish(false)
        };

        let fixfix_1 = |w: &mut BitWriter| {
            w.put(0, 2);
            w.put(0, 2);
            w.put_bool(false);
        };

        // The reference frame is valid.
        let mut dec = new_decoder();
        let (buf, len) = frame(&fixfix_1, 10);
        assert!(dec.decode(&buf, len, false, SBR_RATE, None).unwrap().is_some());

        // Noise floor scalefactors out of range: Q = 31 > 30.
        let (buf, len) = frame(&fixfix_1, 31);
        assert!(dec.decode(&buf, len, false, SBR_RATE, None).is_err());

        // FIXFIX with 8 envelopes.
        let (buf, len) = frame(
            &|w: &mut BitWriter| {
                w.put(0, 2);
                w.put(3, 2);
                w.put_bool(false);
            },
            10,
        );
        assert!(dec.decode(&buf, len, false, SBR_RATE, None).is_err());

        // VARVAR with 7 envelopes.
        let (buf, len) = frame(
            &|w: &mut BitWriter| {
                w.put(3, 2);
                w.put(0, 4);
                w.put(3, 2);
                w.put(3, 2);
                w.put(0, 12);
                w.put(0, 3);
                w.put(0, 7);
            },
            10,
        );
        assert!(dec.decode(&buf, len, false, SBR_RATE, None).is_err());

        // Invalid bs_pointer: VARFIX with 1 envelope... cannot exceed. VARFIX with 2 envelopes
        // (ptr_bits = 2) and bs_pointer = 3 > L_E.
        let (buf, len) = frame(
            &|w: &mut BitWriter| {
                w.put(2, 2);
                w.put(0, 2);
                w.put(1, 2);
                w.put(0, 2);
                w.put(3, 2);
                w.put(0, 1);
            },
            10,
        );
        assert!(dec.decode(&buf, len, false, SBR_RATE, None).is_err());

        // A header violating bs_xover_band < N_Master (N_Master = 14) on a fresh decoder: the
        // header is not accepted.
        let mut w = BitWriter::default();
        put_header(&mut w, 7);
        w.put(0, 32);
        let mut dec = SbrBitstreamDecoder::new(false);
        let (buf, len) = w.finish(false);
        assert!(dec.decode(&buf, len, false, SBR_RATE, None).is_ok());
        assert_eq!(dec.freq_bands().unwrap().kx, 22);

        let mut w = BitWriter::default();
        w.put_bool(true); // bs_header_flag
        w.put_bool(true);
        w.put(15, 4); // bs_start_freq: k0 = 32
        w.put(0, 4); // bs_stop_freq: k2 = 23 < k0
        w.put(0, 3);
        w.put(0, 4);
        w.put(0, 32);
        let (buf, len) = w.finish(false);
        let mut dec = SbrBitstreamDecoder::new(false);
        assert!(dec.decode(&buf, len, false, SBR_RATE, None).is_err());
        assert!(dec.header().is_none());

        // Delta coding in the time direction in the first frame.
        let mut dec = SbrBitstreamDecoder::new(false);
        let (buf, len) = frame2();
        let mut w = BitWriter::default();
        put_header(&mut w, 0);
        let mut payload = w;
        // Append frame2 without its bs_header_flag.
        for bit in 1..len {
            payload.put(u32::from((buf[bit / 8] >> (7 - bit % 8)) & 1), 1);
        }
        let (buf, len) = payload.finish(false);
        assert!(dec.decode(&buf, len, false, SBR_RATE, None).is_err());
        // The header was not committed.
        assert!(dec.header().is_none());
    }

    /// A simple linear congruential generator.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) as u32
        }
    }

    fn check_frame_invariants(frame: &SbrFrame, tables: &FreqBandTables, is_cpe: bool) {
        let num_ch = if is_cpe { 2 } else { 1 };
        for ch in &frame.channels[..num_ch] {
            assert!((1..=MAX_ENVELOPES).contains(&ch.num_env));
            assert!(ch.t_env[..=ch.num_env].windows(2).all(|w| w[0] < w[1]));
            assert!((1..=2).contains(&ch.num_noise));
            assert!(ch.t_noise[..=ch.num_noise].windows(2).all(|w| w[0] < w[1]));
            assert_eq!(ch.t_noise[0], ch.t_env[0]);
            assert_eq!(ch.t_noise[ch.num_noise], ch.t_env[ch.num_env]);
            assert!(ch.l_a.is_none_or(|l_a| l_a <= ch.num_env));
            for l in 0..ch.num_env {
                let n = tables.envelope_table(ch.freq_res[l]).len() - 1;
                assert!(ch.e_orig[l][..n].iter().all(|e| e.is_finite() && *e >= 0.0));
            }
            let n_q = tables.f_table_noise.len() - 1;
            for l in 0..ch.num_noise {
                assert!(ch.q_orig[l][..n_q].iter().all(|q| q.is_finite() && *q > 0.0));
            }
            assert!(ch.invf_mode.iter().all(|&m| m < 4));
        }
    }

    /// Feed many pseudo-random and mutated payloads to the decoders, which must never panic.
    #[test]
    fn verify_random_payloads_do_not_panic() {
        let mut rng = Lcg(0x5eed);

        let rates = [16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000, 7, 1 << 31];

        // Valid payloads to mutate.
        let (frame1_buf, frame1_len) = frame1(false);
        let (frame1_crc_buf, frame1_crc_len) = frame1(true);
        let (frame2_buf, frame2_len) = frame2();

        let (coupled_buf, coupled_len) = cpe_coupled_frame();
        let (uncoupled_buf, uncoupled_len) = cpe_uncoupled_frame();

        let sce_seeds: [(&[u8], usize, bool); 3] = [
            (&frame1_buf, frame1_len, false),
            (&frame1_crc_buf, frame1_crc_len, true),
            (&frame2_buf, frame2_len, false),
        ];

        let cpe_seeds: [(&[u8], usize, bool); 2] =
            [(&coupled_buf, coupled_len, false), (&uncoupled_buf, uncoupled_len, false)];

        let mut decoders = [SbrBitstreamDecoder::new(false), SbrBitstreamDecoder::new(true)];

        let mut num_decoded = [0; 2];

        for iter in 0..60000 {
            let is_cpe = iter % 2 == 1;
            let dec = &mut decoders[iter % 2];
            let seeds: &[(&[u8], usize, bool)] = if is_cpe { &cpe_seeds } else { &sce_seeds };

            let sbr_rate = if rng.next() % 8 == 0 {
                rates[rng.next() as usize % rates.len()]
            }
            else {
                SBR_RATE
            };

            let (buf, len, crc) = match rng.next() % 4 {
                0 => {
                    // Fully random payload.
                    let len = rng.next() as usize % 96;
                    let buf: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
                    let num_bits = rng.next() as usize % (8 * len + 9);
                    (buf, num_bits, rng.next() % 4 == 0)
                }
                1 => {
                    // A valid header followed by random data.
                    let mut w = BitWriter::default();
                    put_header(&mut w, rng.next() % 8);
                    for _ in 0..rng.next() % 600 {
                        w.put(rng.next() & 1, 1);
                    }
                    let (buf, len) = w.finish(false);
                    (buf, len, false)
                }
                _ => {
                    // A mutated valid payload.
                    let (seed, len, crc) = seeds[rng.next() as usize % seeds.len()];
                    let mut buf = seed.to_vec();
                    for _ in 0..1 + rng.next() % 4 {
                        let bit = rng.next() as usize % len;
                        buf[bit / 8] ^= 0x80 >> (bit % 8);
                    }
                    (buf, len, crc)
                }
            };

            if let Ok(Some(frame)) = dec.decode(&buf, len, crc, sbr_rate, None) {
                let tables = dec.freq_bands().unwrap();
                check_frame_invariants(&frame, tables, is_cpe);
                num_decoded[iter % 2] += 1;
            }

            if rng.next() % 1000 == 0 {
                dec.reset();
            }
        }

        assert!(num_decoded.iter().all(|&n| n > 1000), "{:?}", num_decoded);
    }
}
