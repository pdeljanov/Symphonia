// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Parametric stereo bitstream syntax: ps_data() (ISO/IEC 14496-3:2009, 8.4.2, Tables 8.9 to 8.14)
//! and the decoding of the stereo parameter indices (8.5.2).

use symphonia_core::errors::{Result, decode_error};
use symphonia_core::io::ReadBitsLtr;

use super::huffman::{self, HuffTree};
use super::tables::{
    ICC_MAX_INDEX, IID_MAX_INDEX_DEFAULT, IID_MAX_INDEX_FINE, MAX_NR_PAR_BANDS, MAX_NUM_ENV,
    NR_ICC_PAR_TAB, NR_IID_PAR_TAB, NR_IPDOPD_PAR_TAB, NUM_ENV_TAB, NUM_QMF_SLOTS, StereoBands,
};

/// Reads bits from a bitstream without ever exceeding a budget of bits.
pub(super) struct BitBudget<'a, B: ReadBitsLtr> {
    bs: &'a mut B,
    used: usize,
    limit: usize,
}

impl<'a, B: ReadBitsLtr> BitBudget<'a, B> {
    pub fn new(bs: &'a mut B, limit: usize) -> Self {
        BitBudget { bs, used: 0, limit }
    }

    /// The number of bits read so far.
    pub fn used(&self) -> usize {
        self.used
    }

    /// Account for `num_bits` bits that are about to be read, failing if this would exceed the
    /// budget.
    fn consume(&mut self, num_bits: usize) -> Result<()> {
        match self.used.checked_add(num_bits) {
            Some(used) if used <= self.limit => {
                self.used = used;
                Ok(())
            }
            _ => decode_error("ps: ps_data() exceeds the available bits"),
        }
    }

    pub fn read_bit(&mut self) -> Result<bool> {
        self.consume(1)?;
        Ok(self.bs.read_bit()? != 0)
    }

    /// Read an unsigned integer of `num_bits <= 32` bits.
    pub fn read_bits(&mut self, num_bits: u32) -> Result<u32> {
        self.consume(num_bits as usize)?;
        Ok(self.bs.read_bits_leq32(num_bits)?)
    }

    pub fn skip_bits(&mut self, num_bits: usize) -> Result<()> {
        let Ok(num_bits_u32) = u32::try_from(num_bits)
        else {
            return decode_error("ps: ps_data() exceeds the available bits");
        };
        self.consume(num_bits)?;
        Ok(self.bs.ignore_bits(num_bits_u32)?)
    }

    /// Read one Huffman codeword, ssc_huff_dec() (Annex 8.B.1).
    pub fn read_huffman(&mut self, tree: &HuffTree) -> Result<i32> {
        tree.decode(|| self.read_bit())
    }
}

/// The stereo parameter indices of one parameter position (envelope).
///
/// Each parameter type remembers the mode it was decoded with, which determines the number of
/// parameters and the quantization grid (Table 8.24, Table 8.27). A mode of `None` means that the
/// parameters were not transmitted and have their default value (index 0) (8.5.2, 8.6.4.6.5).
/// Entries beyond the number of transmitted parameters are 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ParamSet {
    /// iid_mode of the IID parameters.
    pub iid_mode: Option<u8>,
    /// iid_par[e][b].
    pub iid_par: [i8; MAX_NR_PAR_BANDS],
    /// icc_mode of the ICC parameters.
    pub icc_mode: Option<u8>,
    /// icc_par[e][b].
    pub icc_par: [i8; MAX_NR_PAR_BANDS],
    /// iid_mode of the IPD and OPD parameters (Table 8.24).
    pub ipdopd_mode: Option<u8>,
    /// ipd_par[e][b], 0..=7.
    pub ipd_par: [i8; MAX_NR_PAR_BANDS],
    /// opd_par[e][b], 0..=7.
    pub opd_par: [i8; MAX_NR_PAR_BANDS],
}

impl ParamSet {
    /// All parameters at their default value (IID = 0 dB, ICC = 1, IPD = OPD = 0).
    pub const DEFAULT: ParamSet = ParamSet {
        iid_mode: None,
        iid_par: [0; MAX_NR_PAR_BANDS],
        icc_mode: None,
        icc_par: [0; MAX_NR_PAR_BANDS],
        ipdopd_mode: None,
        ipd_par: [0; MAX_NR_PAR_BANDS],
        opd_par: [0; MAX_NR_PAR_BANDS],
    };
}

/// The PS header configuration (Table 8.9, enable_ps_header).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct PsHeader {
    pub enable_iid: bool,
    pub iid_mode: u8,
    pub enable_icc: bool,
    pub icc_mode: u8,
    pub enable_ext: bool,
}

impl PsHeader {
    /// The stereo band configuration given the number of IID and ICC parameters (Table 8.44,
    /// 8.6.4.6.1), or `None` if neither IID nor ICC parameters are enabled, in which case the
    /// configuration of the previous frame is kept.
    pub fn stereo_bands(&self) -> Option<StereoBands> {
        if !self.enable_iid && !self.enable_icc {
            return None;
        }
        // If IID or ICC parameters are not transmitted, their number is assumed to be 20.
        let nr_iid_par =
            if self.enable_iid { NR_IID_PAR_TAB[usize::from(self.iid_mode)] } else { 20 };
        let nr_icc_par =
            if self.enable_icc { NR_ICC_PAR_TAB[usize::from(self.icc_mode)] } else { 20 };
        Some(StereoBands::from_num_params(nr_iid_par, nr_icc_par))
    }

    /// Returns true if mixing procedure R_b is used, false for R_a (Table 8.27).
    pub fn use_mixing_rb(&self) -> bool {
        self.icc_mode >= 3
    }
}

/// The decoded content of one ps_data() element.
#[derive(Clone, Copy, Debug)]
pub(super) struct PsFrame {
    /// The header configuration in effect for this ps_data() element.
    pub header: PsHeader,
    /// num_env (Table 8.29).
    pub num_env: usize,
    /// The parameter positions n_e (8.6.4.6.2), `num_env` entries are valid.
    pub border_position: [usize; MAX_NUM_ENV],
    /// The parameters of each parameter position, `num_env` entries are valid. If `num_env` is 0,
    /// the first entry holds the parameters that apply to this frame according to 8.6.4.6.5.
    pub params: [ParamSet; MAX_NUM_ENV],
    /// enable_ipdopd (8.5.2).
    pub enable_ipdopd: bool,
}

impl PsFrame {
    pub const DEFAULT: PsFrame = PsFrame {
        header: PsHeader {
            enable_iid: false,
            iid_mode: 0,
            enable_icc: false,
            icc_mode: 0,
            enable_ext: false,
        },
        num_env: 0,
        border_position: [0; MAX_NUM_ENV],
        params: [ParamSet::DEFAULT; MAX_NUM_ENV],
        enable_ipdopd: false,
    };

    /// The parameters of the last parameter position, which are the reference for time
    /// differential coding in the next ps_data() element.
    pub fn last_params(&self) -> &ParamSet {
        &self.params[self.num_env.clamp(1, MAX_NUM_ENV) - 1]
    }
}

/// The state of ps_data() decoding that persists from one ps_data() element to the next.
#[derive(Clone, Copy, Debug)]
pub(super) struct PsSyntaxState {
    /// The latest header configuration ("the latest configuration persists", 8.5.2).
    pub header: PsHeader,
    /// True if `header` was received in a ps_data() element (enable_ps_header = 1), rather than
    /// being the initial configuration.
    pub header_received: bool,
    /// The parameters of the last parameter position of the previous ps_data() element.
    pub prev: ParamSet,
    /// True once a ps_data() element was received from which all parameters can be decoded
    /// correctly (8.6.5.1), and no error occurred since.
    pub synced: bool,
}

impl PsSyntaxState {
    pub const DEFAULT: PsSyntaxState = PsSyntaxState {
        header: PsFrame::DEFAULT.header,
        header_received: false,
        prev: ParamSet::DEFAULT,
        synced: false,
    };

    /// Discard the references to previous ps_data() elements (the reference for time differential
    /// coding and the held parameters), keeping the header configuration.
    ///
    /// Decoding is synchronized again by the first ps_data() element that can be decoded without
    /// previous elements. If a header was received before, that element need not carry a header.
    pub fn flush(&mut self) {
        self.prev = ParamSet::DEFAULT;
        self.synced = false;
    }
}

/// The value range constraints of a parameter type.
#[derive(Clone, Copy)]
enum ParamRange {
    /// Indices must be within `min..=max` (Table 8.24, Table 8.27).
    Bounded { min: i32, max: i32 },
    /// Indices are coded modulo 8 (IPD and OPD, 8.5.2).
    Modulo8,
}

/// Decode the parameter indices of one parameter position, iid_data(), icc_data(), ipd_data(), or
/// opd_data() (Tables 8.11 to 8.14), using the differential decoding of 8.5.2.
///
/// `time_base` holds the indices of the previous parameter position for time differential coding,
/// or is `None` for frequency differential coding. If `trusted` is false, the time differential
/// reference is not known to be correct and out-of-range indices are clamped instead of being
/// reported as an error.
fn read_param_indices<B: ReadBitsLtr>(
    r: &mut BitBudget<'_, B>,
    tree: &HuffTree,
    nr_par: usize,
    time_base: Option<&[i8; MAX_NR_PAR_BANDS]>,
    range: ParamRange,
    trusted: bool,
    out: &mut [i8; MAX_NR_PAR_BANDS],
) -> Result<()> {
    let mut prev = 0;
    for (b, par) in out.iter_mut().enumerate() {
        if b >= nr_par {
            *par = 0;
            continue;
        }
        let delta = r.read_huffman(tree)?;
        let mut value = match time_base {
            Some(base) => i32::from(base[b]) + delta,
            None => prev + delta,
        };
        match range {
            ParamRange::Modulo8 => value = value.rem_euclid(8),
            ParamRange::Bounded { min, max } => {
                if value < min || value > max {
                    if trusted {
                        return decode_error("ps: stereo parameter index out of range");
                    }
                    value = value.clamp(min, max);
                }
            }
        }
        // All ranges fit in an i8.
        *par = value as i8;
        prev = value;
    }
    Ok(())
}

/// Determine the reference of time differential coding for the first parameter position. Returns
/// whether the reference is trusted, or an error if time differential coding is used across a mode
/// change (8.5.2 iid_dt, icc_dt, ipd_dt, opd_dt).
fn first_time_base_trusted(
    synced: bool,
    prev_mode: Option<u8>,
    mode: u8,
    dt: bool,
) -> Result<bool> {
    // A reference without transmitted parameters is all-zero, which is valid in any mode.
    let compatible = prev_mode.is_none_or(|m| m == mode);
    if dt && synced && !compatible {
        return decode_error("ps: time differential coding after a mode change");
    }
    Ok(synced && compatible)
}

/// Read ps_data() (Table 8.9) using the persistent state `state`, without modifying it. Returns the
/// decoded element and whether it allows all parameters to be decoded correctly (8.6.5.1).
///
/// `header_present` is set to the value of enable_ps_header as soon as it is read, so that it is
/// known even if an error occurs later.
fn parse_ps_data<B: ReadBitsLtr>(
    state: &PsSyntaxState,
    r: &mut BitBudget<'_, B>,
    header_present: &mut bool,
) -> Result<(PsFrame, bool)> {
    let mut frame = PsFrame::DEFAULT;
    let mut header = state.header;

    let enable_ps_header = r.read_bit()?;
    *header_present = enable_ps_header;
    if enable_ps_header {
        header.enable_iid = r.read_bit()?;
        if header.enable_iid {
            header.iid_mode = r.read_bits(3)? as u8;
            if header.iid_mode > 5 {
                return decode_error("ps: reserved iid_mode");
            }
        }
        header.enable_icc = r.read_bit()?;
        if header.enable_icc {
            header.icc_mode = r.read_bits(3)? as u8;
            if header.icc_mode > 5 {
                return decode_error("ps: reserved icc_mode");
            }
        }
        header.enable_ext = r.read_bit()?;
    }
    frame.header = header;

    let frame_class = r.read_bit()?;
    let num_env_idx = r.read_bits(2)? as usize;
    let num_env = NUM_ENV_TAB[usize::from(frame_class)][num_env_idx];
    frame.num_env = num_env;

    if frame_class {
        // VAR_BORDERS: the parameter positions must be strictly increasing.
        for e in 0..num_env {
            let n_e = r.read_bits(5)? as usize;
            if e > 0 && n_e <= frame.border_position[e - 1] {
                return decode_error("ps: border positions are not increasing");
            }
            frame.border_position[e] = n_e;
        }
    }
    else {
        // FIX_BORDERS (8.6.4.6.2).
        for e in 0..num_env {
            frame.border_position[e] = NUM_QMF_SLOTS * (e + 1) / num_env - 1;
        }
    }

    // The element allows correct decoding if the header configuration is known, and all
    // parameters of the first parameter position are frequency differentially coded (8.6.5.1).
    // The header configuration is known if this element carries a header, or if a header was
    // received before (and was not lost in an erroneous element).
    let mut sync = (enable_ps_header || state.header_received) && num_env > 0;

    if header.enable_iid {
        let mode = header.iid_mode;
        let nr_par = NR_IID_PAR_TAB[usize::from(mode)];
        let fine = mode >= 3;
        let max = if fine { IID_MAX_INDEX_FINE } else { IID_MAX_INDEX_DEFAULT };
        let range = ParamRange::Bounded { min: -max, max };

        let mut trusted = false;
        for e in 0..num_env {
            let iid_dt = r.read_bit()?;
            let tree = match (fine, iid_dt) {
                (false, false) => &huffman::IID_DF_DEFAULT,
                (false, true) => &huffman::IID_DT_DEFAULT,
                (true, false) => &huffman::IID_DF_FINE,
                (true, true) => &huffman::IID_DT_FINE,
            };
            let base = if e == 0 { state.prev.iid_par } else { frame.params[e - 1].iid_par };
            if e == 0 {
                trusted = first_time_base_trusted(state.synced, state.prev.iid_mode, mode, iid_dt)?;
                sync &= !iid_dt;
            }
            if !iid_dt {
                trusted = true;
            }
            let params = &mut frame.params[e];
            let time_base = if iid_dt { Some(&base) } else { None };
            read_param_indices(r, tree, nr_par, time_base, range, trusted, &mut params.iid_par)?;
            params.iid_mode = Some(mode);
        }
    }

    if header.enable_icc {
        let mode = header.icc_mode;
        let nr_par = NR_ICC_PAR_TAB[usize::from(mode)];
        let range = ParamRange::Bounded { min: 0, max: ICC_MAX_INDEX };

        let mut trusted = false;
        for e in 0..num_env {
            let icc_dt = r.read_bit()?;
            let tree = if icc_dt { &huffman::ICC_DT } else { &huffman::ICC_DF };
            let base = if e == 0 { state.prev.icc_par } else { frame.params[e - 1].icc_par };
            if e == 0 {
                trusted = first_time_base_trusted(state.synced, state.prev.icc_mode, mode, icc_dt)?;
                sync &= !icc_dt;
            }
            if !icc_dt {
                trusted = true;
            }
            let params = &mut frame.params[e];
            let time_base = if icc_dt { Some(&base) } else { None };
            read_param_indices(r, tree, nr_par, time_base, range, trusted, &mut params.icc_par)?;
            params.icc_mode = Some(mode);
        }
    }

    if header.enable_ext {
        let mut cnt = r.read_bits(4)? as usize;
        if cnt == 15 {
            cnt += r.read_bits(8)? as usize;
        }

        let mut num_bits_left = 8 * cnt;
        while num_bits_left > 7 {
            let ps_extension_id = r.read_bits(2)?;
            num_bits_left -= 2;

            if ps_extension_id != 0 {
                // A reserved extension (Table 8.30). Its content cannot be parsed and is skipped
                // along with the fill bits.
                break;
            }

            let start = r.used();
            let ipdopd_sync = read_ps_extension_v0(state, r, &mut frame)?;
            let ext_bits = r.used() - start;

            let Some(left) = num_bits_left.checked_sub(ext_bits)
            else {
                return decode_error("ps: ps_extension() exceeds the extension size");
            };
            num_bits_left = left;

            sync &= ipdopd_sync;
        }

        // fill_bits.
        r.skip_bits(num_bits_left)?;
    }

    if num_env == 0 {
        // No new parameters are transmitted. Enabled parameters are held, and the others are set
        // to their default value (Tables 8.50 to 8.52).
        let prev = &state.prev;
        let held = &mut frame.params[0];
        if header.enable_iid {
            held.iid_mode = prev.iid_mode;
            held.iid_par = prev.iid_par;
        }
        if header.enable_icc {
            held.icc_mode = prev.icc_mode;
            held.icc_par = prev.icc_par;
        }
        if frame.enable_ipdopd {
            held.ipdopd_mode = prev.ipdopd_mode;
            held.ipd_par = prev.ipd_par;
            held.opd_par = prev.opd_par;
        }
    }

    Ok((frame, sync))
}

/// Read ps_extension() with ps_extension_id = 0 (Table 8.10) after ps_extension_id. Returns false
/// if IPD or OPD parameters of the first parameter position are time differentially coded.
fn read_ps_extension_v0<B: ReadBitsLtr>(
    state: &PsSyntaxState,
    r: &mut BitBudget<'_, B>,
    frame: &mut PsFrame,
) -> Result<bool> {
    // Discard IPD/OPD parameters of a previous extension in the same ps_data() element.
    for params in frame.params.iter_mut() {
        params.ipdopd_mode = None;
        params.ipd_par = [0; MAX_NR_PAR_BANDS];
        params.opd_par = [0; MAX_NR_PAR_BANDS];
    }

    let mut sync = true;

    frame.enable_ipdopd = r.read_bit()?;
    if frame.enable_ipdopd {
        if !frame.header.enable_iid {
            return decode_error("ps: enable_ipdopd is set while enable_iid is 0");
        }

        let mode = frame.header.iid_mode;
        let nr_par = NR_IPDOPD_PAR_TAB[usize::from(mode)];
        let range = ParamRange::Modulo8;

        for e in 0..frame.num_env {
            let (ipd_base, opd_base) = if e == 0 {
                (state.prev.ipd_par, state.prev.opd_par)
            }
            else {
                (frame.params[e - 1].ipd_par, frame.params[e - 1].opd_par)
            };

            let ipd_dt = r.read_bit()?;
            let tree = if ipd_dt { &huffman::IPD_DT } else { &huffman::IPD_DF };
            let time_base = if ipd_dt { Some(&ipd_base) } else { None };
            if e == 0 {
                first_time_base_trusted(state.synced, state.prev.ipdopd_mode, mode, ipd_dt)?;
                sync &= !ipd_dt;
            }
            let params = &mut frame.params[e];
            read_param_indices(r, tree, nr_par, time_base, range, true, &mut params.ipd_par)?;

            let opd_dt = r.read_bit()?;
            let tree = if opd_dt { &huffman::OPD_DT } else { &huffman::OPD_DF };
            let time_base = if opd_dt { Some(&opd_base) } else { None };
            if e == 0 {
                first_time_base_trusted(state.synced, state.prev.ipdopd_mode, mode, opd_dt)?;
                sync &= !opd_dt;
            }
            let params = &mut frame.params[e];
            read_param_indices(r, tree, nr_par, time_base, range, true, &mut params.opd_par)?;

            params.ipdopd_mode = Some(mode);
        }
    }

    // reserved_ps.
    r.read_bit()?;

    Ok(sync)
}

/// Read ps_data() (Table 8.9), reading at most `num_bits_left` bits, and return the number of bits
/// read.
///
/// On success, the decoded element is written to `frame` and `state` is updated. On error, `state`
/// is unchanged except that it is marked as not synchronized, since the reference for time
/// differential coding of the following elements was lost. If the erroneous element carried a
/// header, the header configuration is no longer known either, and only an element with a header
/// can synchronize decoding again.
pub(super) fn read_ps_data<B: ReadBitsLtr>(
    state: &mut PsSyntaxState,
    bs: &mut B,
    num_bits_left: usize,
    frame: &mut PsFrame,
) -> Result<usize> {
    let mut r = BitBudget::new(bs, num_bits_left);
    let mut header_present = false;
    match parse_ps_data(state, &mut r, &mut header_present) {
        Ok((parsed, sync)) => {
            state.header = parsed.header;
            state.header_received |= header_present;
            state.prev = *parsed.last_params();
            state.synced |= sync;
            *frame = parsed;
            Ok(r.used())
        }
        Err(err) => {
            state.synced = false;
            if header_present {
                state.header_received = false;
            }
            Err(err)
        }
    }
}

/// A bit writer for assembling test bitstreams.
#[cfg(test)]
pub(super) mod test_util {
    use super::super::huffman;

    #[derive(Default)]
    pub struct BitWriter {
        bytes: Vec<u8>,
        len: usize,
    }

    impl BitWriter {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn put(&mut self, value: u32, num_bits: u32) -> &mut Self {
            for i in (0..num_bits).rev() {
                if self.len % 8 == 0 {
                    self.bytes.push(0);
                }
                let bit = ((value >> i) & 1) as u8;
                let last = self.bytes.len() - 1;
                self.bytes[last] |= bit << (7 - (self.len % 8));
                self.len += 1;
            }
            self
        }

        pub fn bit(&mut self, value: bool) -> &mut Self {
            self.put(u32::from(value), 1)
        }

        /// Write the codeword of `value` from a code table with values starting at `first`.
        pub fn code(&mut self, table: &[(u32, u8)], first: i32, value: i32) -> &mut Self {
            let (code, len) = table[(value - first) as usize];
            self.put(code, u32::from(len))
        }

        pub fn iid_df(&mut self, fine: bool, value: i32) -> &mut Self {
            if fine {
                self.code(&huffman::HUFF_IID_DF_FINE_CODES, -30, value)
            }
            else {
                self.code(&huffman::HUFF_IID_DF_DEFAULT_CODES, -14, value)
            }
        }

        pub fn iid_dt(&mut self, fine: bool, value: i32) -> &mut Self {
            if fine {
                self.code(&huffman::HUFF_IID_DT_FINE_CODES, -30, value)
            }
            else {
                self.code(&huffman::HUFF_IID_DT_DEFAULT_CODES, -14, value)
            }
        }

        pub fn icc_df(&mut self, value: i32) -> &mut Self {
            self.code(&huffman::HUFF_ICC_DF_CODES, -7, value)
        }

        pub fn icc_dt(&mut self, value: i32) -> &mut Self {
            self.code(&huffman::HUFF_ICC_DT_CODES, -7, value)
        }

        pub fn ipd_df(&mut self, value: i32) -> &mut Self {
            self.code(&huffman::HUFF_IPD_DF_CODES, 0, value)
        }

        pub fn ipd_dt(&mut self, value: i32) -> &mut Self {
            self.code(&huffman::HUFF_IPD_DT_CODES, 0, value)
        }

        pub fn opd_df(&mut self, value: i32) -> &mut Self {
            self.code(&huffman::HUFF_OPD_DF_CODES, 0, value)
        }

        pub fn opd_dt(&mut self, value: i32) -> &mut Self {
            self.code(&huffman::HUFF_OPD_DT_CODES, 0, value)
        }

        /// The number of bits written.
        pub fn len(&self) -> usize {
            self.len
        }

        /// The written bytes, padded with zero bits (plus a few guard bytes).
        pub fn bytes(&self) -> Vec<u8> {
            let mut bytes = self.bytes.clone();
            bytes.extend_from_slice(&[0; 8]);
            bytes
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::BitWriter;
    use super::*;
    use symphonia_core::io::{BitReaderLtr, FiniteBitStream};

    fn parse(state: &mut PsSyntaxState, w: &BitWriter, budget: usize) -> Result<(PsFrame, usize)> {
        let bytes = w.bytes();
        let mut bs = BitReaderLtr::new(&bytes);
        let mut frame = PsFrame::DEFAULT;
        let used = read_ps_data(state, &mut bs, budget, &mut frame)?;
        // The bit reader position must match the number of bits reported.
        assert_eq!(bytes.len() * 8 - bs.bits_left() as usize, used);
        Ok((frame, used))
    }

    /// Write a header (enable_ps_header = 1).
    fn header(w: &mut BitWriter, iid: Option<u32>, icc: Option<u32>, ext: bool) {
        w.bit(true);
        w.bit(iid.is_some());
        if let Some(mode) = iid {
            w.put(mode, 3);
        }
        w.bit(icc.is_some());
        if let Some(mode) = icc {
            w.put(mode, 3);
        }
        w.bit(ext);
    }

    #[test]
    fn verify_header_fix_borders_df() {
        // Header: IID mode 0 (10 parameters, default grid), ICC mode 3 (10 parameters, R_b).
        let mut w = BitWriter::new();
        header(&mut w, Some(0), Some(3), false);
        // frame_class = 0, num_env_idx = 2 -> num_env = 2.
        w.bit(false).put(2, 2);
        let iid: [[i32; 10]; 2] =
            [[0, 1, 2, 3, 4, 5, 6, 7, -7, -1], [-3, -3, -3, -3, -3, -3, -3, -3, -3, -3]];
        for env in iid.iter() {
            w.bit(false);
            let mut prev = 0;
            for &v in env.iter() {
                w.iid_df(false, v - prev);
                prev = v;
            }
        }
        let icc: [i32; 10] = [0, 1, 2, 3, 4, 5, 6, 7, 7, 0];
        for _ in 0..2 {
            w.bit(false);
            let mut prev = 0;
            for &v in icc.iter() {
                w.icc_df(v - prev);
                prev = v;
            }
        }
        let total = w.len();

        let mut state = PsSyntaxState::DEFAULT;
        let (frame, used) = parse(&mut state, &w, total).expect("valid ps_data");
        assert_eq!(used, total);
        assert!(state.synced);
        assert_eq!(frame.num_env, 2);
        assert_eq!(&frame.border_position[..2], &[15, 31]);
        assert_eq!(frame.header.stereo_bands(), Some(StereoBands::Bands20));
        assert!(frame.header.use_mixing_rb());
        for e in 0..2 {
            let p = &frame.params[e];
            assert_eq!(p.iid_mode, Some(0));
            assert_eq!(p.icc_mode, Some(3));
            for b in 0..34 {
                let iid = if b < 10 { iid[e][b] } else { 0 };
                let icc = if b < 10 { icc[b] } else { 0 };
                assert_eq!(i32::from(p.iid_par[b]), iid);
                assert_eq!(i32::from(p.icc_par[b]), icc);
            }
            assert_eq!(p.ipdopd_mode, None);
        }
        assert_eq!(state.prev, frame.params[1]);

        // One bit less than needed is an error, and the state is no longer synchronized.
        let mut state2 = PsSyntaxState::DEFAULT;
        assert!(parse(&mut state2, &w, total - 1).is_err());
        assert!(!state2.synced);
    }

    #[test]
    fn verify_var_borders_and_time_differential() {
        let mut state = PsSyntaxState::DEFAULT;

        // Frame 1: header, IID mode 4 (20 parameters, fine grid), no ICC, VAR_BORDERS, 3 envelopes.
        let mut w = BitWriter::new();
        header(&mut w, Some(4), None, false);
        w.bit(true).put(2, 2);
        w.put(3, 5).put(17, 5).put(30, 5);
        // Envelope 0: frequency differential, 15 (maximum of the fine grid) in band 0, then -15.
        w.bit(false).iid_df(true, 15).iid_df(true, -30);
        for _ in 2..20 {
            w.iid_df(true, 0);
        }
        // Envelope 1: time differential, +1 for every band except band 0.
        w.bit(true).iid_dt(true, 0);
        for _ in 1..20 {
            w.iid_dt(true, 1);
        }
        // Envelope 2: time differential, no change.
        w.bit(true);
        for _ in 0..20 {
            w.iid_dt(true, 0);
        }
        let total = w.len();
        let (frame, used) = parse(&mut state, &w, total + 13).expect("valid ps_data");
        assert_eq!(used, total);
        assert!(state.synced);
        assert_eq!(frame.num_env, 3);
        assert_eq!(&frame.border_position[..3], &[3, 17, 30]);
        assert_eq!(frame.header.stereo_bands(), Some(StereoBands::Bands20));
        assert_eq!(frame.params[0].iid_par[0], 15);
        assert_eq!(frame.params[0].iid_par[1], -15);
        assert_eq!(frame.params[0].iid_par[19], -15);
        assert_eq!(frame.params[1].iid_par[0], 15);
        assert_eq!(frame.params[1].iid_par[1], -14);
        assert_eq!(frame.params[2].iid_par[19], -14);
        assert_eq!(frame.params[2].iid_par[20], 0);
        assert_eq!(frame.params[2].icc_mode, None);

        // Frame 2: no header (configuration persists), FIX_BORDERS with 1 envelope, time
        // differential relative to the last envelope of frame 1.
        let mut w = BitWriter::new();
        w.bit(false);
        w.bit(false).put(1, 2);
        w.bit(true).iid_dt(true, -30);
        for _ in 1..20 {
            w.iid_dt(true, 2);
        }
        let total = w.len();
        let (frame, used) = parse(&mut state, &w, total).expect("valid ps_data");
        assert_eq!(used, total);
        assert_eq!(frame.num_env, 1);
        assert_eq!(frame.border_position[0], 31);
        assert_eq!(frame.params[0].iid_par[0], -15);
        assert_eq!(frame.params[0].iid_par[1], -12);

        // Frame 3: time differential coding leaving the valid range is an error.
        let mut w = BitWriter::new();
        w.bit(false);
        w.bit(false).put(1, 2);
        w.bit(true).iid_dt(true, -1);
        let bytes_len = w.len() + 64;
        assert!(parse(&mut state, &w, bytes_len).is_err());
        assert!(!state.synced);
    }

    #[test]
    fn verify_num_env_zero_holds_parameters() {
        let mut state = PsSyntaxState::DEFAULT;

        let mut w = BitWriter::new();
        header(&mut w, Some(1), Some(1), false);
        w.bit(false).put(1, 2);
        w.bit(false);
        for b in 0..20 {
            w.iid_df(false, if b < 6 { 1 } else { 0 });
        }
        // The fourth band reaches 4 then stays.
        w.bit(false);
        for b in 0..20 {
            w.icc_df(if b < 4 { 1 } else { 0 });
        }
        let total = w.len();
        let (frame, _) = parse(&mut state, &w, total).expect("valid ps_data");
        let first = frame.params[0];
        assert_eq!(first.iid_par[5], 6);
        assert_eq!(first.icc_par[19], 4);

        // num_env = 0 with a header disabling ICC: IID is held, ICC is set to default.
        let mut w = BitWriter::new();
        header(&mut w, Some(1), None, false);
        w.bit(false).put(0, 2);
        let total = w.len();
        let (frame, used) = parse(&mut state, &w, total).expect("valid ps_data");
        assert_eq!(used, total);
        assert_eq!(frame.num_env, 0);
        assert_eq!(frame.params[0].iid_par, first.iid_par);
        assert_eq!(frame.params[0].iid_mode, Some(1));
        assert_eq!(frame.params[0].icc_mode, None);
        assert_eq!(frame.params[0].icc_par, [0; 34]);
        assert_eq!(state.prev, frame.params[0]);
        assert_eq!(frame.header.stereo_bands(), Some(StereoBands::Bands20));
    }

    #[test]
    fn verify_extension_ipd_opd() {
        let mut state = PsSyntaxState::DEFAULT;

        // IID mode 2 (34 parameters, 17 IPD/OPD parameters), ICC mode 0, extension.
        let mut w = BitWriter::new();
        header(&mut w, Some(2), Some(0), true);
        w.bit(false).put(1, 2);
        w.bit(false);
        for _ in 0..34 {
            w.iid_df(false, 0);
        }
        w.bit(false);
        for _ in 0..10 {
            w.icc_df(0);
        }
        // The extension: 6 bytes (48 bits).
        let mut ext = BitWriter::new();
        ext.put(0, 2); // ps_extension_id
        ext.bit(true); // enable_ipdopd
        ext.bit(false); // ipd_dt
        for b in 0..17 {
            ext.ipd_df(if b == 0 { 7 } else { 1 });
        }
        ext.bit(false); // opd_dt
        for b in 0..17 {
            ext.opd_df(if b == 0 { 3 } else { 0 });
        }
        ext.bit(false); // reserved_ps
        let ext_bits = ext.len();
        let cnt = ext_bits.div_ceil(8);
        assert!(cnt < 15);
        w.put(cnt as u32, 4);
        let ext_bytes = ext.bytes();
        for i in 0..ext_bits {
            w.bit(ext_bytes[i / 8] & (0x80 >> (i % 8)) != 0);
        }
        // Fill bits.
        for _ in ext_bits..8 * cnt {
            w.bit(true);
        }
        let total = w.len();
        let (frame, used) = parse(&mut state, &w, total + 7).expect("valid ps_data");
        assert_eq!(used, total);
        assert!(frame.enable_ipdopd);
        assert!(state.synced);
        assert_eq!(frame.header.stereo_bands(), Some(StereoBands::Bands34));
        let p = &frame.params[0];
        assert_eq!(p.ipdopd_mode, Some(2));
        assert_eq!(p.ipd_par[0], 7);
        assert_eq!(p.ipd_par[1], 0); // (7 + 1) mod 8
        assert_eq!(p.ipd_par[2], 1);
        assert_eq!(p.ipd_par[16], 7);
        assert_eq!(p.ipd_par[17], 0);
        assert_eq!(p.opd_par[0], 3);
        assert_eq!(p.opd_par[16], 3);

        // Time differential IPD/OPD in the next frame, using the escape coded extension size.
        let mut w = BitWriter::new();
        w.bit(false);
        w.bit(false).put(1, 2);
        w.bit(true);
        for _ in 0..34 {
            w.iid_dt(false, 0);
        }
        w.bit(true);
        for _ in 0..10 {
            w.icc_dt(0);
        }
        w.put(15, 4).put(4, 8); // cnt = 19 bytes = 152 bits.
        let start = w.len();
        w.put(0, 2).bit(true);
        w.bit(true);
        for _ in 0..17 {
            w.ipd_dt(2);
        }
        w.bit(true);
        for _ in 0..17 {
            w.opd_dt(5);
        }
        w.bit(false);
        // A reserved extension follows, which is skipped.
        w.put(3, 2);
        while w.len() < start + 152 {
            w.bit(true);
        }
        let total = w.len();
        let (frame, used) = parse(&mut state, &w, total).expect("valid ps_data");
        assert_eq!(used, total);
        let q = &frame.params[0];
        assert_eq!(q.ipd_par[0], 1); // (7 + 2) mod 8
        assert_eq!(q.ipd_par[2], 3);
        assert_eq!(q.opd_par[0], 0); // (3 + 5) mod 8
        assert_eq!(q.opd_par[1], 0);
        assert_eq!(q.ipd_par[17], 0);
    }

    #[test]
    fn verify_extension_overflow_is_error() {
        // An extension whose content exceeds its declared size.
        let mut w = BitWriter::new();
        header(&mut w, Some(0), None, true);
        w.bit(false).put(1, 2);
        w.bit(false);
        for _ in 0..10 {
            w.iid_df(false, 0);
        }
        w.put(1, 4); // 8 bits.
        w.put(0, 2).bit(true);
        w.bit(false);
        for _ in 0..5 {
            w.ipd_df(1);
        }
        w.bit(false);
        for _ in 0..5 {
            w.opd_df(1);
        }
        w.bit(false);
        let total = w.len();
        let mut state = PsSyntaxState::DEFAULT;
        assert!(parse(&mut state, &w, total + 100).is_err());
    }

    #[test]
    fn verify_constraint_violations() {
        // Reserved iid_mode.
        let mut w = BitWriter::new();
        w.bit(true).bit(true).put(6, 3);
        w.put(0, 32);
        let mut state = PsSyntaxState::DEFAULT;
        assert!(parse(&mut state, &w, 64).is_err());

        // Reserved icc_mode.
        let mut w = BitWriter::new();
        w.bit(true).bit(false).bit(true).put(7, 3);
        w.put(0, 32);
        assert!(parse(&mut state, &w, 64).is_err());

        // Border positions not increasing.
        let mut w = BitWriter::new();
        header(&mut w, None, None, false);
        w.bit(true).put(1, 2).put(10, 5).put(10, 5);
        assert!(parse(&mut state, &w, 64).is_err());

        // enable_ipdopd without IID.
        let mut w = BitWriter::new();
        header(&mut w, None, Some(0), true);
        w.bit(false).put(1, 2);
        w.bit(false);
        for _ in 0..10 {
            w.icc_df(0);
        }
        w.put(1, 4).put(0, 2).bit(true).put(0, 5);
        assert!(parse(&mut state, &w, 128).is_err());

        // Time differential coding of the first envelope after a change of iid_mode once
        // synchronized.
        let mut state = PsSyntaxState::DEFAULT;
        let mut w = BitWriter::new();
        header(&mut w, Some(0), None, false);
        w.bit(false).put(1, 2).bit(false);
        for _ in 0..10 {
            w.iid_df(false, 0);
        }
        parse(&mut state, &w, w.len()).expect("valid ps_data");
        assert!(state.synced);
        let mut w = BitWriter::new();
        header(&mut w, Some(1), None, false);
        w.bit(false).put(1, 2).bit(true);
        for _ in 0..20 {
            w.iid_dt(false, 0);
        }
        assert!(parse(&mut state, &w, w.len()).is_err());
        assert!(!state.synced);
    }

    #[test]
    fn verify_resync_after_error_and_flush() {
        let mut state = PsSyntaxState::DEFAULT;

        // Frequency differentially coded IID (mode 0), without a header.
        let df_frame = || {
            let mut w = BitWriter::new();
            w.bit(false).bit(false).put(1, 2).bit(false);
            for b in 0..10 {
                w.iid_df(false, if b < 5 { 1 } else { 0 });
            }
            w
        };

        // A header synchronizes.
        let mut w = BitWriter::new();
        header(&mut w, Some(0), None, false);
        w.bit(false).put(1, 2).bit(false);
        for _ in 0..10 {
            w.iid_df(false, 0);
        }
        parse(&mut state, &w, w.len()).expect("valid ps_data");
        assert!(state.synced && state.header_received);

        // An erroneous element without a header loses synchronization but not the header, so a
        // frequency differentially coded element without a header synchronizes again.
        let w = df_frame();
        assert!(parse(&mut state, &w, w.len() - 1).is_err());
        assert!(!state.synced && state.header_received);
        parse(&mut state, &w, w.len()).expect("valid ps_data");
        assert!(state.synced);

        // A flush keeps the header but discards the references.
        state.flush();
        assert!(!state.synced && state.header_received);
        assert_eq!(state.prev, ParamSet::DEFAULT);
        let (frame, _) = parse(&mut state, &w, w.len()).expect("valid ps_data");
        assert!(state.synced);
        assert_eq!(frame.params[0].iid_par[9], 5);

        // An erroneous element with a header loses the header configuration: an element without
        // a header no longer synchronizes.
        let mut w = BitWriter::new();
        header(&mut w, Some(1), None, false);
        w.bit(false).put(1, 2).bit(false);
        assert!(parse(&mut state, &w, w.len() + 4).is_err());
        assert!(!state.synced && !state.header_received);
        let w = df_frame();
        parse(&mut state, &w, w.len()).expect("valid ps_data");
        assert!(!state.synced);
    }

    #[test]
    fn verify_sync_rules() {
        // Without a header, an element does not synchronize.
        let mut state = PsSyntaxState::DEFAULT;
        let mut w = BitWriter::new();
        w.bit(false).bit(false).put(1, 2);
        let (frame, used) = parse(&mut state, &w, 64).expect("valid ps_data");
        assert_eq!(used, 4);
        assert_eq!(frame.header, PsHeader::default());
        assert!(!state.synced);

        // With a header but time differential coding of the first envelope, before
        // synchronization, out-of-range values are clamped rather than reported.
        let mut w = BitWriter::new();
        header(&mut w, Some(0), None, false);
        w.bit(false).put(1, 2).bit(true);
        for _ in 0..10 {
            w.iid_dt(false, 14);
        }
        let (frame, _) = parse(&mut state, &w, w.len()).expect("valid ps_data");
        assert!(!state.synced);
        assert_eq!(frame.params[0].iid_par[0], 7);

        // With num_env = 0, an element does not synchronize.
        let mut w = BitWriter::new();
        header(&mut w, Some(0), None, false);
        w.bit(false).put(0, 2);
        parse(&mut state, &w, w.len()).expect("valid ps_data");
        assert!(!state.synced);
    }
}
