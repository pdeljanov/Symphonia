// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Parametric Stereo bitstream parser.
//!
//! Reads PS extension data from the SBR bitstream as defined in
//! ISO/IEC 14496-3:2009, Subpart 4, Section 8.6.4.3–8.6.4.5.
//! Decodes IID, ICC, and optional IPD/OPD parameters per envelope
//! using delta-coded Huffman encoding.

use std::convert::TryInto;

use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::vlc::*;
use symphonia_core::io::ReadBitsLtr;

use lazy_static::lazy_static;

use super::tables::*;
use super::PsCommonContext;

/// SBR Huffman codebooks for PS parameter decoding (ISO/IEC 14496-3:2009, 8.6.4.5).
struct PsCodebooks {
    /// 10 codebooks: [iid_df1, iid_dt1, iid_df0, iid_dt0, icc_df, icc_dt,
    ///                ipd_df, ipd_dt, opd_df, opd_dt]
    books: [Codebook<Entry16x16>; 10],
}

/// Generate canonical Huffman codewords from (symbol, code_length) pairs.
///
/// Uses the same algorithm as FFmpeg's `ff_vlc_init_from_lengths`:
/// process entries in order, assigning codes canonically based on the
/// code lengths. Shorter codes consume more of the code space.
fn build_ps_codebook(table_offset: usize, size: usize) -> Codebook<Entry16x16> {
    let entries = &AACPS_HUFF_TABS[table_offset..table_offset + size];

    let mut codes = Vec::with_capacity(size);
    let mut lens = Vec::with_capacity(size);
    let mut values = Vec::with_capacity(size);

    // Generate canonical codewords from (symbol, length) pairs.
    // Code is maintained left-justified in a 32-bit space.
    let mut code: u64 = 0;
    for &(symbol, len) in entries {
        if len > 0 {
            let codeword = (code >> (32 - len as u64)) as u32;
            codes.push(codeword);
            lens.push(len);
            values.push(symbol as u16);
            code += 1u64 << (32 - len as u64);
        }
    }

    let mut builder = CodebookBuilder::new(BitOrder::Verbatim);
    builder.bits_per_read(8);
    builder.make(&codes, &lens, &values).unwrap()
}

impl PsCodebooks {
    fn new() -> Self {
        let mut offset = 0;
        let mut books_vec = Vec::with_capacity(10);
        for &sz in &HUFF_SIZES {
            books_vec.push(build_ps_codebook(offset, sz));
            offset += sz;
        }
        let books: [Codebook<Entry16x16>; 10] =
            books_vec.try_into().unwrap_or_else(|_| panic!("expected 10 codebooks"));
        Self { books }
    }
}

lazy_static! {
    static ref PS_CODEBOOKS: PsCodebooks = PsCodebooks::new();
}

/// Codebook indices for the different PS parameter types
/// (ISO/IEC 14496-3:2009, Tables 8.54–8.57).
const HUFF_IID_DF1: usize = 0;
const HUFF_IID_DT1: usize = 1;
const HUFF_IID_DF0: usize = 2;
const HUFF_IID_DT0: usize = 3;
const HUFF_ICC_DF: usize = 4;
const HUFF_ICC_DT: usize = 5;
const HUFF_IPD_DF: usize = 6;
const HUFF_IPD_DT: usize = 7;
const HUFF_OPD_DF: usize = 8;
const HUFF_OPD_DT: usize = 9;

/// Read a single Huffman-coded value from the bitstream.
/// Returns the decoded signed value and adds bits consumed to `cnt`.
#[inline]
fn read_ps_huff<B: ReadBitsLtr>(bs: &mut B, cb_idx: usize, cnt: &mut usize) -> Result<i8> {
    let cbs = &PS_CODEBOOKS;
    let (sym, bits) = bs.read_codebook(&cbs.books[cb_idx])?;
    *cnt += bits as usize;
    Ok(sym as i8 + HUFF_OFFSETS[cb_idx])
}

/// Read IID or ICC delta-coded parameters for one envelope (8.6.4.4).
#[allow(clippy::too_many_arguments)]
fn read_par_data<B: ReadBitsLtr>(
    bs: &mut B,
    par: &mut [[i8; PS_MAX_NR_IIDICC]],
    cb_idx: usize,
    nr_par: usize,
    env: usize,
    dt: bool,
    stride: usize,
    cnt: &mut usize,
) -> Result<()> {
    if dt {
        // Delta time: decode relative to previous envelope.
        let prev = if env > 0 { par[env - 1] } else { [0i8; PS_MAX_NR_IIDICC] };
        for b in 0..nr_par {
            let delta = read_ps_huff(bs, cb_idx, cnt)?;
            par[env][b] = prev[b].wrapping_add(delta);
        }
    }
    else {
        // Delta frequency: first value absolute, rest relative to previous band.
        par[env][0] = read_ps_huff(bs, cb_idx, cnt)?;
        for b in 1..nr_par {
            let delta = read_ps_huff(bs, cb_idx, cnt)?;
            par[env][b] = par[env][b - 1].wrapping_add(delta);
        }
    }

    // Apply stride (scale values for coarse quantization).
    if stride > 1 {
        for b in 0..nr_par {
            par[env][b] = par[env][b].wrapping_mul(stride as i8);
        }
    }

    Ok(())
}

/// Read IPD or OPD delta-coded parameters for one envelope (8.6.4.4).
fn read_ipdopd_data<B: ReadBitsLtr>(
    bs: &mut B,
    par: &mut [[i8; PS_MAX_NR_IIDICC]],
    cb_idx: usize,
    nr_par: usize,
    env: usize,
    dt: bool,
    cnt: &mut usize,
) -> Result<()> {
    if dt {
        let prev = if env > 0 { par[env - 1] } else { [0i8; PS_MAX_NR_IIDICC] };
        for b in 0..nr_par {
            let delta = read_ps_huff(bs, cb_idx, cnt)?;
            par[env][b] = (prev[b].wrapping_add(delta)) & 0x07; // mod 8
        }
    }
    else {
        par[env][0] = read_ps_huff(bs, cb_idx, cnt)? & 0x07;
        for b in 1..nr_par {
            let delta = read_ps_huff(bs, cb_idx, cnt)?;
            par[env][b] = (par[env][b - 1].wrapping_add(delta)) & 0x07;
        }
    }
    Ok(())
}

/// Read PS extension data containing IPD/OPD parameters.
/// Returns the number of bits consumed (excluding the 2-bit extension ID).
fn ps_read_extension_data<B: ReadBitsLtr>(
    bs: &mut B,
    ps: &mut PsCommonContext,
    ps_extension_id: u32,
    cnt: &mut usize,
) -> Result<()> {
    if ps_extension_id != 0 {
        return Ok(());
    }

    ps.enable_ipdopd = bs.read_bool()?;
    *cnt += 1;

    if ps.enable_ipdopd {
        for e in 0..ps.num_env {
            let dt = bs.read_bool()?;
            *cnt += 1;
            let cb = if dt { HUFF_IPD_DT } else { HUFF_IPD_DF };
            read_ipdopd_data(bs, &mut ps.ipd_par, cb, ps.nr_ipdopd_par, e, dt, cnt)?;

            let dt = bs.read_bool()?;
            *cnt += 1;
            let cb = if dt { HUFF_OPD_DT } else { HUFF_OPD_DF };
            read_ipdopd_data(bs, &mut ps.opd_par, cb, ps.nr_ipdopd_par, e, dt, cnt)?;
        }
    }

    // reserved_ps
    let _ = bs.read_bool()?;
    *cnt += 1;

    Ok(())
}

/// Read PS extension data from the SBR bitstream.
///
/// Parses the PS header (if present), envelope structure, and delta-coded
/// IID/ICC/IPD/OPD parameters per ISO/IEC 14496-3:2009, 8.6.4.3–8.6.4.4.
/// Returns the number of bits consumed.
pub fn ps_read_data<B: ReadBitsLtr>(
    bs: &mut B,
    ps: &mut PsCommonContext,
    bits_left: usize,
    num_qmf_slots: usize,
) -> Result<usize> {
    // Track bits consumed manually since ReadBitsLtr has no bits_read() method.
    let mut cnt: usize = 0;

    // PS header (8.6.4.3).
    let header = bs.read_bool()?;
    cnt += 1;
    if header {
        ps.enable_iid = bs.read_bool()?;
        cnt += 1;
        if ps.enable_iid {
            let iid_mode = bs.read_bits_leq32(3)? as usize;
            cnt += 3;
            if iid_mode > 5 {
                return decode_error("ps: iid_mode reserved");
            }
            ps.nr_iid_par = NR_IIDICC_PAR_TAB[iid_mode];
            ps.iid_quant = iid_mode > 2;
            ps.nr_ipdopd_par = NR_IIDOPD_PAR_TAB[iid_mode];
        }

        ps.enable_icc = bs.read_bool()?;
        cnt += 1;
        if ps.enable_icc {
            let icc_mode = bs.read_bits_leq32(3)? as usize;
            cnt += 3;
            if icc_mode > 5 {
                return decode_error("ps: icc_mode reserved");
            }
            ps.icc_mode = icc_mode;
            ps.nr_icc_par = NR_IIDICC_PAR_TAB[icc_mode];
        }

        ps.enable_ext = bs.read_bool()?;
        cnt += 1;
    }

    // Frame class and envelope count (8.6.4.4).
    ps.frame_class = bs.read_bool()?;
    cnt += 1;
    ps.num_env_old = ps.num_env;
    let num_env_idx = bs.read_bits_leq32(2)? as usize;
    cnt += 2;
    ps.num_env = NUM_ENV_TAB[ps.frame_class as usize][num_env_idx];

    // Border positions
    ps.border_position[0] = -1;
    if ps.frame_class {
        // Variable frame borders
        for e in 1..=ps.num_env {
            ps.border_position[e] = bs.read_bits_leq32(5)? as i32;
            cnt += 5;
            if ps.border_position[e] < ps.border_position[e - 1] {
                return err_and_clear(bs, ps, cnt, bits_left);
            }
        }
    }
    else {
        // Fixed frame borders
        for e in 1..=ps.num_env {
            ps.border_position[e] =
                ((e as i32 * num_qmf_slots as i32) >> FF_LOG2_TAB[ps.num_env]) - 1;
        }
    }

    // IID parameters
    if ps.enable_iid {
        for e in 0..ps.num_env {
            let dt = bs.read_bool()?;
            cnt += 1;
            let cb = if ps.iid_quant {
                if dt {
                    HUFF_IID_DT1
                }
                else {
                    HUFF_IID_DF1
                }
            }
            else {
                if dt {
                    HUFF_IID_DT0
                }
                else {
                    HUFF_IID_DF0
                }
            };
            if read_par_data(bs, &mut ps.iid_par, cb, ps.nr_iid_par, e, dt, 1, &mut cnt).is_err() {
                return err_and_clear(bs, ps, cnt, bits_left);
            }
        }
    }
    else {
        ps.iid_par = [[0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
    }

    // ICC parameters
    if ps.enable_icc {
        for e in 0..ps.num_env {
            let dt = bs.read_bool()?;
            cnt += 1;
            let cb = if dt { HUFF_ICC_DT } else { HUFF_ICC_DF };
            if read_par_data(bs, &mut ps.icc_par, cb, ps.nr_icc_par, e, dt, 1, &mut cnt).is_err() {
                return err_and_clear(bs, ps, cnt, bits_left);
            }
        }
    }
    else {
        ps.icc_par = [[0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
    }

    // Extension data (IPD/OPD)
    if ps.enable_ext {
        let mut ext_cnt = bs.read_bits_leq32(4)? as i32;
        cnt += 4;
        if ext_cnt == 15 {
            ext_cnt += bs.read_bits_leq32(8)? as i32;
            cnt += 8;
        }
        ext_cnt *= 8;
        while ext_cnt > 7 {
            let ps_extension_id = bs.read_bits_leq32(2)?;
            cnt += 2;
            ext_cnt -= 2;
            let cnt_before = cnt;
            ps_read_extension_data(bs, ps, ps_extension_id, &mut cnt)?;
            ext_cnt -= (cnt - cnt_before) as i32;
        }
        if ext_cnt < 0 {
            return err_and_clear(bs, ps, cnt, bits_left);
        }
        if ext_cnt > 0 {
            bs.ignore_bits(ext_cnt as u32)?;
            cnt += ext_cnt as usize;
        }
    }

    // Disable IPD/OPD for baseline profile (always disabled in this implementation).
    // ps.enable_ipdopd is kept as parsed.

    // Fix up envelopes: ensure the last border covers all QMF slots.
    if ps.num_env == 0 || ps.border_position[ps.num_env] < (num_qmf_slots as i32 - 1) {
        // Create a fake envelope by copying the last valid one.
        let source = if ps.num_env > 0 {
            ps.num_env - 1
        }
        else if ps.num_env_old > 0 {
            // Use from previous frame — but data is already in arrays, just reference last.
            ps.num_env_old - 1
        }
        else {
            // No previous data either; leave zeros.
            ps.num_env
        };

        if source < ps.num_env {
            if ps.enable_iid {
                ps.iid_par[ps.num_env] = ps.iid_par[source];
            }
            if ps.enable_icc {
                ps.icc_par[ps.num_env] = ps.icc_par[source];
            }
            if ps.enable_ipdopd {
                ps.ipd_par[ps.num_env] = ps.ipd_par[source];
                ps.opd_par[ps.num_env] = ps.opd_par[source];
            }
        }

        // Validate copied parameters.
        if ps.enable_iid {
            let limit = 7 + 8 * (ps.iid_quant as i8);
            for b in 0..ps.nr_iid_par {
                if ps.iid_par[ps.num_env][b].abs() > limit {
                    return err_and_clear(bs, ps, cnt, bits_left);
                }
            }
        }
        if ps.enable_icc {
            for b in 0..ps.nr_icc_par {
                if ps.icc_par[ps.num_env][b] < 0 || ps.icc_par[ps.num_env][b] > 7 {
                    return err_and_clear(bs, ps, cnt, bits_left);
                }
            }
        }

        ps.num_env += 1;
        ps.border_position[ps.num_env] = num_qmf_slots as i32 - 1;
    }

    // Determine if 34-band mode.
    ps.is34bands_old = ps.is34bands;
    if ps.enable_iid || ps.enable_icc {
        ps.is34bands =
            (ps.enable_iid && ps.nr_iid_par == 34) || (ps.enable_icc && ps.nr_icc_par == 34);
    }

    // Clear IPD/OPD if not enabled.
    if !ps.enable_ipdopd {
        ps.ipd_par = [[0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        ps.opd_par = [[0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
    }

    if header {
        ps.start = true;
    }

    if cnt <= bits_left {
        Ok(cnt)
    }
    else {
        err_and_clear(bs, ps, cnt, bits_left)
    }
}

/// Error handler: clear PS state and skip remaining bits.
fn err_and_clear<B: ReadBitsLtr>(
    bs: &mut B,
    ps: &mut PsCommonContext,
    bits_consumed: usize,
    bits_left: usize,
) -> Result<usize> {
    ps.start = false;
    ps.iid_par = [[0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
    ps.icc_par = [[0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
    ps.ipd_par = [[0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
    ps.opd_par = [[0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];

    if bits_consumed < bits_left {
        let _ = bs.ignore_bits((bits_left - bits_consumed) as u32);
    }
    Ok(bits_left)
}

#[cfg(test)]
mod tests {
    use super::*;
    use symphonia_core::io::BitReaderLtr;

    #[test]
    fn verify_all_codebooks_build() {
        // Force lazy_static initialization of all 10 codebooks.
        let cbs = &*PS_CODEBOOKS;
        // If we get here without panic, all codebooks built successfully.
        assert_eq!(cbs.books.len(), 10);
    }

    #[test]
    fn verify_canonical_codewords_prefix_free() {
        // Build each codebook and verify no codeword is a prefix of another.
        let mut offset = 0;
        for (tab_idx, &sz) in HUFF_SIZES.iter().enumerate() {
            let entries = &AACPS_HUFF_TABS[offset..offset + sz];

            let mut codes: Vec<(u32, u8)> = Vec::new();
            let mut code: u64 = 0;
            for &(_, len) in entries {
                if len > 0 {
                    let codeword = (code >> (32 - len as u64)) as u32;
                    codes.push((codeword, len));
                    code += 1u64 << (32 - len as u64);
                }
            }

            // Verify the prefix-free property: no codeword is a prefix of another.
            for i in 0..codes.len() {
                for j in 0..codes.len() {
                    if i == j {
                        continue;
                    }
                    let (ci, li) = codes[i];
                    let (cj, lj) = codes[j];
                    if li <= lj {
                        // Check if ci is a prefix of cj.
                        let cj_shifted = cj >> (lj - li);
                        assert_ne!(
                            ci, cj_shifted,
                            "Table {}: code {} (len {}) is prefix of code {} (len {})",
                            tab_idx, ci, li, cj, lj
                        );
                    }
                }
            }
            offset += sz;
        }
    }

    #[test]
    fn verify_codebook_code_space_complete() {
        // Verify the Kraft inequality: sum(2^-len) should = 1.0 for a complete code.
        let mut offset = 0;
        for (tab_idx, &sz) in HUFF_SIZES.iter().enumerate() {
            let entries = &AACPS_HUFF_TABS[offset..offset + sz];

            let mut kraft_sum: f64 = 0.0;
            for &(_, len) in entries {
                if len > 0 {
                    kraft_sum += 2.0f64.powi(-(len as i32));
                }
            }

            assert!(
                (kraft_sum - 1.0).abs() < 1e-10,
                "Table {}: Kraft sum {} != 1.0 (code space not complete)",
                tab_idx,
                kraft_sum
            );
            offset += sz;
        }
    }

    #[test]
    fn verify_codebook_all_symbols_present() {
        // Verify each table has all symbols 0..sz-1 exactly once.
        let mut offset = 0;
        for (tab_idx, &sz) in HUFF_SIZES.iter().enumerate() {
            let entries = &AACPS_HUFF_TABS[offset..offset + sz];
            let mut seen = vec![false; sz];
            for &(sym, _) in entries {
                assert!(!seen[sym as usize], "Table {}: duplicate symbol {}", tab_idx, sym);
                seen[sym as usize] = true;
            }
            for (sym, &present) in seen.iter().enumerate() {
                assert!(present, "Table {}: missing symbol {}", tab_idx, sym);
            }
            offset += sz;
        }
    }

    #[test]
    fn verify_read_iid_df0_zero() {
        // Encode a known IID delta-frequency value using huff_iid_df0.
        // The codebook for huff_iid_df0 (table index 2) should decode symbol 14
        // (which becomes 14 + offset(-14) = 0) from a 1-bit codeword '0'.
        let data = [0x00]; // Bit '0' followed by zeros.
        let mut bs = BitReaderLtr::new(&data);
        let mut cnt = 0;
        let val = read_ps_huff(&mut bs, HUFF_IID_DF0, &mut cnt).unwrap();
        assert_eq!(val, 0, "IID df0 symbol 14 should decode to 0");
        assert_eq!(cnt, 1, "Should consume 1 bit");
    }

    #[test]
    fn verify_read_icc_df_zero() {
        // huff_icc_df (table index 4): symbol 7 at code length 1 → value 7 + (-7) = 0.
        let data = [0x00]; // Bit '0' followed by zeros.
        let mut bs = BitReaderLtr::new(&data);
        let mut cnt = 0;
        let val = read_ps_huff(&mut bs, HUFF_ICC_DF, &mut cnt).unwrap();
        assert_eq!(val, 0, "ICC df symbol 7 should decode to 0");
        assert_eq!(cnt, 1, "Should consume 1 bit");
    }

    #[test]
    fn verify_read_ipd_df_zero() {
        // huff_ipd_df (table index 6): symbol 0 is the last entry with code length 1.
        // Canonical code assignment gives it codeword '1' (the only 1-bit code).
        // So we need MSB=1 → 0x80.
        let data = [0x80];
        let mut bs = BitReaderLtr::new(&data);
        let mut cnt = 0;
        let val = read_ps_huff(&mut bs, HUFF_IPD_DF, &mut cnt).unwrap();
        assert_eq!(val, 0, "IPD df symbol 0 should decode to 0");
        assert_eq!(cnt, 1, "Should consume 1 bit");
    }

    #[test]
    fn verify_read_par_data_delta_freq() {
        // Test delta-frequency mode: first value absolute, rest relative.
        // Encode 3 zero values using huff_icc_df (shortest code = 1 bit for zero).
        // Three '0' bits → three zero values.
        let data = [0x00];
        let mut bs = BitReaderLtr::new(&data);
        let mut par = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        let mut cnt = 0;

        read_par_data(&mut bs, &mut par, HUFF_ICC_DF, 3, 0, false, 1, &mut cnt).unwrap();

        assert_eq!(par[0][0], 0);
        assert_eq!(par[0][1], 0);
        assert_eq!(par[0][2], 0);
        assert_eq!(cnt, 3, "Should consume 3 bits for 3 zero deltas");
    }

    #[test]
    fn verify_read_par_data_delta_time() {
        // Test delta-time mode: decode relative to previous envelope.
        // Previous envelope all zeros, decode zero deltas → result should be all zeros.
        let data = [0x00, 0x00];
        let mut bs = BitReaderLtr::new(&data);
        let mut par = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        let mut cnt = 0;

        // env=1 with dt=true should reference env=0 (all zeros).
        read_par_data(&mut bs, &mut par, HUFF_ICC_DF, 3, 1, true, 1, &mut cnt).unwrap();

        assert_eq!(par[1][0], 0);
        assert_eq!(par[1][1], 0);
        assert_eq!(par[1][2], 0);
    }

    #[test]
    fn verify_read_par_data_stride() {
        // Test stride multiplication: values should be multiplied by stride.
        let data = [0x00];
        let mut bs = BitReaderLtr::new(&data);
        let mut par = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        let mut cnt = 0;

        // With stride=2, decoded zero * 2 = 0 (trivial but verifies no crash).
        read_par_data(&mut bs, &mut par, HUFF_ICC_DF, 2, 0, false, 2, &mut cnt).unwrap();

        assert_eq!(par[0][0], 0);
        assert_eq!(par[0][1], 0);
    }

    #[test]
    fn verify_read_ipdopd_data_mod8() {
        // IPD/OPD values should be masked to 3 bits (& 0x07 = mod 8).
        let data = [0x00, 0x00];
        let mut bs = BitReaderLtr::new(&data);
        let mut par = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        let mut cnt = 0;

        read_ipdopd_data(&mut bs, &mut par, HUFF_IPD_DF, 3, 0, false, &mut cnt).unwrap();

        // All decoded as zero, mod 8 should still be zero.
        for b in 0..3 {
            assert_eq!(par[0][b] & 0x07, par[0][b], "IPD value should be in range 0..7");
        }
    }

    #[test]
    fn verify_ps_read_data_minimal_header() {
        // Construct a minimal PS frame with:
        // - header=1 (1 bit)
        // - enable_iid=0 (1 bit)
        // - enable_icc=0 (1 bit)
        // - enable_ext=0 (1 bit)
        // - frame_class=0 (1 bit)
        // - num_env_idx=1 → 1 envelope (2 bits)
        // Total header: 7 bits = 0b1000_010 + padding
        //
        // With enable_iid=0 and enable_icc=0, no parameter data is needed.
        // border_position computed as fixed: [e*32/1]-1 = 31 for e=1.
        // Envelope fix-up: border[1]=31 == QMF_SLOTS-1, so no fake envelope.
        //
        // Bit layout: 1_000_0_01_0 = 0x82
        let data = [0x82, 0x00];
        let mut bs = BitReaderLtr::new(&data);
        let mut ps = PsCommonContext::new();

        let bits = ps_read_data(&mut bs, &mut ps, 16, 32).unwrap();

        assert!(!ps.enable_iid);
        assert!(!ps.enable_icc);
        assert!(!ps.enable_ext);
        assert!(!ps.frame_class);
        assert_eq!(ps.num_env, 1);
        assert!(ps.start, "PS start should be true after header");
        assert!(bits <= 16, "Should not consume more than bits_left");
    }

    #[test]
    fn verify_ps_read_data_no_header() {
        // PS frame without header:
        // - header=0 (1 bit)
        // - frame_class=0 (1 bit)
        // - num_env_idx=0 → 0 envelopes (2 bits)
        // Total: 4 bits = 0b0_0_00_xxxx
        //
        // With 0 envelopes and no data, a fake envelope should be generated.
        // Bit layout: 0_0_00_0000 = 0x00
        let data = [0x00, 0x00, 0x00, 0x00];
        let mut bs = BitReaderLtr::new(&data);
        let mut ps = PsCommonContext::new();

        let bits = ps_read_data(&mut bs, &mut ps, 32, 32).unwrap();

        // Without header, start stays false.
        assert!(!ps.start);
        // With 0 envelopes, a fake envelope should be created → num_env becomes 1.
        assert_eq!(ps.num_env, 1);
        assert!(bits <= 32);
    }

    #[test]
    fn verify_err_and_clear_resets_state() {
        let data = [0xFF, 0xFF];
        let mut bs = BitReaderLtr::new(&data);
        let mut ps = PsCommonContext::new();

        // Set some state.
        ps.start = true;
        ps.iid_par[0][0] = 5;
        ps.icc_par[0][0] = 3;

        let result = err_and_clear(&mut bs, &mut ps, 0, 16).unwrap();

        assert_eq!(result, 16);
        assert!(!ps.start);
        assert_eq!(ps.iid_par[0][0], 0);
        assert_eq!(ps.icc_par[0][0], 0);
    }

    #[test]
    fn verify_huff_offsets() {
        // IID tables (indices 0-3) have offset -30 or -14.
        assert_eq!(HUFF_OFFSETS[0], -30); // iid_df1 (61 entries, center at 30)
        assert_eq!(HUFF_OFFSETS[1], -30); // iid_dt1
        assert_eq!(HUFF_OFFSETS[2], -14); // iid_df0 (29 entries, center at 14)
        assert_eq!(HUFF_OFFSETS[3], -14); // iid_dt0
                                          // ICC tables (indices 4-5) have offset -7.
        assert_eq!(HUFF_OFFSETS[4], -7);
        assert_eq!(HUFF_OFFSETS[5], -7);
        // IPD/OPD tables (indices 6-9) have offset 0.
        assert_eq!(HUFF_OFFSETS[6], 0);
        assert_eq!(HUFF_OFFSETS[7], 0);
        assert_eq!(HUFF_OFFSETS[8], 0);
        assert_eq!(HUFF_OFFSETS[9], 0);
    }
}
