// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Parametric Stereo bitstream parser.
//!
//! Reads PS extension data from the SBR bitstream as defined in
//! ISO/IEC 14496-3:2009, Subpart 4, Section 8.6.4.3--8.6.4.5.
//! Decodes IID, ICC, and optional IPD/OPD parameters per envelope
//! using delta-coded Huffman encoding.

use std::convert::TryInto;

use symphonia_core::errors::{Result, decode_error};
use symphonia_core::io::ReadBitsLtr;
use symphonia_core::io::vlc::*;

use lazy_static::lazy_static;

use super::PsCommonContext;
use super::tables::*;

/// Identifies which Huffman codebook to use for a given parameter type and
/// coding direction (ISO/IEC 14496-3:2009, Tables 8.54--8.57).
#[derive(Clone, Copy)]
enum PsCodebookId {
    /// IID delta-frequency, fine quantization (61 symbols).
    IidDeltaFreqFine = 0,
    /// IID delta-time, fine quantization (61 symbols).
    IidDeltaTimeFine = 1,
    /// IID delta-frequency, coarse quantization (29 symbols).
    IidDeltaFreqCoarse = 2,
    /// IID delta-time, coarse quantization (29 symbols).
    IidDeltaTimeCoarse = 3,
    /// ICC delta-frequency (15 symbols).
    IccDeltaFreq = 4,
    /// ICC delta-time (15 symbols).
    IccDeltaTime = 5,
    /// IPD delta-frequency (8 symbols).
    IpdDeltaFreq = 6,
    /// IPD delta-time (8 symbols).
    IpdDeltaTime = 7,
    /// OPD delta-frequency (8 symbols).
    OpdDeltaFreq = 8,
    /// OPD delta-time (8 symbols).
    OpdDeltaTime = 9,
}

/// Collection of all 10 Huffman codebooks used for PS parameter decoding
/// (ISO/IEC 14496-3:2009, Section 8.6.4.5).
struct PsHuffmanSet {
    /// The 10 codebooks indexed by `PsCodebookId`.
    tables: [Codebook<Entry16x16>; 10],
}

/// Construct a single Huffman codebook from the concatenated table data.
///
/// Generates canonical codewords from (symbol, code_length) pairs using the
/// same algorithm as FFmpeg's `ff_vlc_init_from_lengths`: entries are processed
/// in order and codes are assigned canonically based on code lengths.
fn construct_codebook(table_start: usize, num_entries: usize) -> Codebook<Entry16x16> {
    let raw_entries = &AACPS_HUFF_TABS[table_start..table_start + num_entries];

    let mut codewords = Vec::with_capacity(num_entries);
    let mut lengths = Vec::with_capacity(num_entries);
    let mut symbols = Vec::with_capacity(num_entries);

    // Assign canonical codewords. The running code is maintained left-justified
    // in a 32-bit space so that incrementing always advances correctly.
    let mut running_code: u64 = 0;
    for &(sym, bit_len) in raw_entries {
        if bit_len > 0 {
            let word = (running_code >> (32 - bit_len as u64)) as u32;
            codewords.push(word);
            lengths.push(bit_len);
            symbols.push(sym as u16);
            running_code += 1u64 << (32 - bit_len as u64);
        }
    }

    let mut builder = CodebookBuilder::new(BitOrder::Verbatim);
    builder.bits_per_read(8);
    builder.make(&codewords, &lengths, &symbols).unwrap()
}

impl PsHuffmanSet {
    fn build() -> Self {
        let mut cursor = 0;
        let mut codebook_vec = Vec::with_capacity(10);
        for &table_len in &HUFF_SIZES {
            codebook_vec.push(construct_codebook(cursor, table_len));
            cursor += table_len;
        }
        let tables: [Codebook<Entry16x16>; 10] =
            codebook_vec.try_into().unwrap_or_else(|_| panic!("expected 10 codebooks"));
        Self { tables }
    }
}

lazy_static! {
    static ref PS_HUFFMAN: PsHuffmanSet = PsHuffmanSet::build();
}

/// Decode a single Huffman-coded value from the bitstream.
///
/// Reads one VLC symbol using the codebook identified by `which`, applies
/// the corresponding signed offset, and accumulates the number of bits
/// consumed into `bits_used`.
#[inline]
fn huffman_decode<B: ReadBitsLtr>(
    bs: &mut B,
    which: PsCodebookId,
    bits_used: &mut usize,
) -> Result<i8> {
    let idx = which as usize;
    let (sym, num_bits) = bs.read_codebook(&PS_HUFFMAN.tables[idx])?;
    *bits_used += num_bits as usize;
    Ok(sym as i8 + HUFF_OFFSETS[idx])
}

/// Decode IID or ICC delta-coded parameters for a single envelope
/// (ISO/IEC 14496-3:2009, Section 8.6.4.4).
///
/// When `is_delta_time` is true, values are decoded relative to the previous
/// envelope. Otherwise, delta-frequency coding is used where the first band
/// is absolute and subsequent bands are relative to the previous band.
/// A `scale_factor` > 1 multiplies each decoded value (used for coarse
/// quantization modes).
#[allow(clippy::too_many_arguments)]
fn decode_parameter_band<B: ReadBitsLtr>(
    bs: &mut B,
    params: &mut [[i8; PS_MAX_NR_IIDICC]],
    codebook: PsCodebookId,
    num_bands: usize,
    envelope: usize,
    is_delta_time: bool,
    scale_factor: usize,
    bits_used: &mut usize,
) -> Result<()> {
    if is_delta_time {
        // Delta-time: each band is relative to the same band in the prior envelope.
        let reference = if envelope > 0 { params[envelope - 1] } else { [0i8; PS_MAX_NR_IIDICC] };
        for band in 0..num_bands {
            let delta = huffman_decode(bs, codebook, bits_used)?;
            params[envelope][band] = reference[band].wrapping_add(delta);
        }
    }
    else {
        // Delta-frequency: first band is absolute, each subsequent band is
        // relative to the preceding one.
        params[envelope][0] = huffman_decode(bs, codebook, bits_used)?;
        for band in 1..num_bands {
            let delta = huffman_decode(bs, codebook, bits_used)?;
            params[envelope][band] = params[envelope][band - 1].wrapping_add(delta);
        }
    }

    // Scale values when using coarse quantization.
    if scale_factor > 1 {
        for band in 0..num_bands {
            params[envelope][band] = params[envelope][band].wrapping_mul(scale_factor as i8);
        }
    }

    Ok(())
}

/// Decode IPD or OPD delta-coded phase parameters for a single envelope
/// (ISO/IEC 14496-3:2009, Section 8.6.4.4).
///
/// Phase parameters are wrapped to 3 bits (modulo 8) after accumulation.
fn decode_phase_parameters<B: ReadBitsLtr>(
    bs: &mut B,
    params: &mut [[i8; PS_MAX_NR_IIDICC]],
    codebook: PsCodebookId,
    num_bands: usize,
    envelope: usize,
    is_delta_time: bool,
    bits_used: &mut usize,
) -> Result<()> {
    if is_delta_time {
        let reference = if envelope > 0 { params[envelope - 1] } else { [0i8; PS_MAX_NR_IIDICC] };
        for band in 0..num_bands {
            let delta = huffman_decode(bs, codebook, bits_used)?;
            params[envelope][band] = (reference[band].wrapping_add(delta)) & 0x07;
            // mod 8
        }
    }
    else {
        params[envelope][0] = huffman_decode(bs, codebook, bits_used)? & 0x07;
        for band in 1..num_bands {
            let delta = huffman_decode(bs, codebook, bits_used)?;
            params[envelope][band] = (params[envelope][band - 1].wrapping_add(delta)) & 0x07;
        }
    }
    Ok(())
}

/// Parse PS extension payload containing IPD/OPD phase parameters
/// (ISO/IEC 14496-3:2009, Section 8.6.4.3, ps_extension).
///
/// Only extension ID 0 is defined; other IDs are silently ignored.
fn parse_extension_payload<B: ReadBitsLtr>(
    bs: &mut B,
    ps: &mut PsCommonContext,
    ext_id: u32,
    bits_used: &mut usize,
) -> Result<()> {
    // Only extension ID 0 (IPD/OPD data) is specified.
    if ext_id != 0 {
        return Ok(());
    }

    ps.enable_ipdopd = bs.read_bool()?;
    *bits_used += 1;

    if ps.enable_ipdopd {
        for env_idx in 0..ps.num_env {
            // IPD parameters.
            let dt_ipd = bs.read_bool()?;
            *bits_used += 1;
            let ipd_cb =
                if dt_ipd { PsCodebookId::IpdDeltaTime } else { PsCodebookId::IpdDeltaFreq };
            decode_phase_parameters(
                bs,
                &mut ps.ipd_par,
                ipd_cb,
                ps.nr_ipdopd_par,
                env_idx,
                dt_ipd,
                bits_used,
            )?;

            // OPD parameters.
            let dt_opd = bs.read_bool()?;
            *bits_used += 1;
            let opd_cb =
                if dt_opd { PsCodebookId::OpdDeltaTime } else { PsCodebookId::OpdDeltaFreq };
            decode_phase_parameters(
                bs,
                &mut ps.opd_par,
                opd_cb,
                ps.nr_ipdopd_par,
                env_idx,
                dt_opd,
                bits_used,
            )?;
        }
    }

    // reserved_ps (1 bit, ISO/IEC 14496-3:2009, 8.6.4.3).
    let _ = bs.read_bool()?;
    *bits_used += 1;

    Ok(())
}

/// Reset PS state to safe defaults and skip any remaining bits in the PS
/// payload. Used when a bitstream error is detected to avoid propagating
/// corrupt parameters to the synthesis stage.
fn reset_on_error<B: ReadBitsLtr>(
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

/// Read PS extension data from the SBR bitstream.
///
/// Parses the PS header (if present), envelope structure, and delta-coded
/// IID/ICC/IPD/OPD parameters per ISO/IEC 14496-3:2009, Sections 8.6.4.3
/// and 8.6.4.4. Returns the total number of bits consumed.
pub fn ps_read_data<B: ReadBitsLtr>(
    bs: &mut B,
    ps: &mut PsCommonContext,
    bits_left: usize,
    num_qmf_slots: usize,
) -> Result<usize> {
    // Manual bit accounting since ReadBitsLtr provides no bits_read() accessor.
    let mut consumed: usize = 0;

    // --- PS Header (ISO/IEC 14496-3:2009, Section 8.6.4.3) ---
    let has_header = bs.read_bool()?;
    consumed += 1;

    if has_header {
        // IID configuration.
        ps.enable_iid = bs.read_bool()?;
        consumed += 1;
        if ps.enable_iid {
            let iid_mode = bs.read_bits_leq32(3)? as usize;
            consumed += 3;
            if iid_mode > 5 {
                return decode_error("ps: iid_mode reserved");
            }
            ps.nr_iid_par = NR_IIDICC_PAR_TAB[iid_mode];
            ps.iid_quant = iid_mode > 2;
            ps.nr_ipdopd_par = NR_IIDOPD_PAR_TAB[iid_mode];
        }

        // ICC configuration.
        ps.enable_icc = bs.read_bool()?;
        consumed += 1;
        if ps.enable_icc {
            let icc_mode = bs.read_bits_leq32(3)? as usize;
            consumed += 3;
            if icc_mode > 5 {
                return decode_error("ps: icc_mode reserved");
            }
            ps.icc_mode = icc_mode;
            ps.nr_icc_par = NR_IIDICC_PAR_TAB[icc_mode];
        }

        // Extension flag.
        ps.enable_ext = bs.read_bool()?;
        consumed += 1;
    }

    // --- Envelope structure (ISO/IEC 14496-3:2009, Section 8.6.4.4) ---
    ps.frame_class = bs.read_bool()?;
    consumed += 1;

    ps.num_env_old = ps.num_env;
    let env_count_index = bs.read_bits_leq32(2)? as usize;
    consumed += 2;
    ps.num_env = NUM_ENV_TAB[ps.frame_class as usize][env_count_index];

    // --- Border positions ---
    ps.border_position[0] = -1;
    if ps.frame_class {
        // Variable-length borders (frame_class == 1).
        for e in 1..=ps.num_env {
            ps.border_position[e] = bs.read_bits_leq32(5)? as i32;
            consumed += 5;
            if ps.border_position[e] < ps.border_position[e - 1] {
                return reset_on_error(bs, ps, consumed, bits_left);
            }
        }
    }
    else {
        // Fixed equally-spaced borders (frame_class == 0).
        for e in 1..=ps.num_env {
            ps.border_position[e] =
                ((e as i32 * num_qmf_slots as i32) >> FF_LOG2_TAB[ps.num_env]) - 1;
        }
    }

    // --- IID parameters (ISO/IEC 14496-3:2009, Section 8.6.4.4) ---
    if ps.enable_iid {
        for env_idx in 0..ps.num_env {
            let dt = bs.read_bool()?;
            consumed += 1;

            let codebook = if ps.iid_quant {
                if dt { PsCodebookId::IidDeltaTimeFine } else { PsCodebookId::IidDeltaFreqFine }
            }
            else if dt {
                PsCodebookId::IidDeltaTimeCoarse
            }
            else {
                PsCodebookId::IidDeltaFreqCoarse
            };

            if decode_parameter_band(
                bs,
                &mut ps.iid_par,
                codebook,
                ps.nr_iid_par,
                env_idx,
                dt,
                1,
                &mut consumed,
            )
            .is_err()
            {
                return reset_on_error(bs, ps, consumed, bits_left);
            }
        }
    }
    else {
        ps.iid_par = [[0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
    }

    // --- ICC parameters (ISO/IEC 14496-3:2009, Section 8.6.4.4) ---
    if ps.enable_icc {
        for env_idx in 0..ps.num_env {
            let dt = bs.read_bool()?;
            consumed += 1;

            let codebook = if dt { PsCodebookId::IccDeltaTime } else { PsCodebookId::IccDeltaFreq };

            if decode_parameter_band(
                bs,
                &mut ps.icc_par,
                codebook,
                ps.nr_icc_par,
                env_idx,
                dt,
                1,
                &mut consumed,
            )
            .is_err()
            {
                return reset_on_error(bs, ps, consumed, bits_left);
            }
        }
    }
    else {
        ps.icc_par = [[0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
    }

    // --- Extension data / IPD/OPD (ISO/IEC 14496-3:2009, Section 8.6.4.3) ---
    if ps.enable_ext {
        let mut ext_bits_remaining = bs.read_bits_leq32(4)? as i32;
        consumed += 4;
        if ext_bits_remaining == 15 {
            ext_bits_remaining += bs.read_bits_leq32(8)? as i32;
            consumed += 8;
        }
        ext_bits_remaining *= 8;

        while ext_bits_remaining > 7 {
            let ext_id = bs.read_bits_leq32(2)?;
            consumed += 2;
            ext_bits_remaining -= 2;

            let before = consumed;
            parse_extension_payload(bs, ps, ext_id, &mut consumed)?;
            ext_bits_remaining -= (consumed - before) as i32;
        }

        if ext_bits_remaining < 0 {
            return reset_on_error(bs, ps, consumed, bits_left);
        }
        if ext_bits_remaining > 0 {
            bs.ignore_bits(ext_bits_remaining as u32)?;
            consumed += ext_bits_remaining as usize;
        }
    }

    // --- Envelope fixup (ISO/IEC 14496-3:2009, Section 8.6.4.6.1) ---
    //
    // If no envelopes were decoded, or the last border does not reach the
    // end of the QMF frame, append a synthetic envelope by copying the last
    // valid parameter set.
    if ps.num_env == 0 || ps.border_position[ps.num_env] < (num_qmf_slots as i32 - 1) {
        let copy_src = if ps.num_env > 0 {
            ps.num_env - 1
        }
        else if ps.num_env_old > 0 {
            // Fall back to the last envelope from the previous frame.
            ps.num_env_old - 1
        }
        else {
            // No prior data; zeros will remain.
            ps.num_env
        };

        if copy_src < ps.num_env {
            if ps.enable_iid {
                ps.iid_par[ps.num_env] = ps.iid_par[copy_src];
            }
            if ps.enable_icc {
                ps.icc_par[ps.num_env] = ps.icc_par[copy_src];
            }
            if ps.enable_ipdopd {
                ps.ipd_par[ps.num_env] = ps.ipd_par[copy_src];
                ps.opd_par[ps.num_env] = ps.opd_par[copy_src];
            }
        }

        // Validate the copied/synthesized parameter values.
        if ps.enable_iid {
            let iid_limit = 7 + 8 * (ps.iid_quant as i8);
            for band in 0..ps.nr_iid_par {
                if ps.iid_par[ps.num_env][band].abs() > iid_limit {
                    return reset_on_error(bs, ps, consumed, bits_left);
                }
            }
        }
        if ps.enable_icc {
            for band in 0..ps.nr_icc_par {
                if ps.icc_par[ps.num_env][band] < 0 || ps.icc_par[ps.num_env][band] > 7 {
                    return reset_on_error(bs, ps, consumed, bits_left);
                }
            }
        }

        ps.num_env += 1;
        ps.border_position[ps.num_env] = num_qmf_slots as i32 - 1;
    }

    // --- Determine band resolution ---
    ps.is34bands_old = ps.is34bands;
    if ps.enable_iid || ps.enable_icc {
        ps.is34bands =
            (ps.enable_iid && ps.nr_iid_par == 34) || (ps.enable_icc && ps.nr_icc_par == 34);
    }

    // --- Zero out phase parameters when IPD/OPD is disabled ---
    if !ps.enable_ipdopd {
        ps.ipd_par = [[0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        ps.opd_par = [[0; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
    }

    if has_header {
        ps.start = true;
    }

    if consumed <= bits_left { Ok(consumed) } else { reset_on_error(bs, ps, consumed, bits_left) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symphonia_core::io::BitReaderLtr;

    #[test]
    fn verify_all_codebooks_build() {
        // Force lazy_static initialization of all 10 codebooks.
        let hset = &*PS_HUFFMAN;
        // If we get here without panic, all codebooks built successfully.
        assert_eq!(hset.tables.len(), 10);
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
        let mut bits_used = 0;
        let val =
            huffman_decode(&mut bs, PsCodebookId::IidDeltaFreqCoarse, &mut bits_used).unwrap();
        assert_eq!(val, 0, "IID df0 symbol 14 should decode to 0");
        assert_eq!(bits_used, 1, "Should consume 1 bit");
    }

    #[test]
    fn verify_read_icc_df_zero() {
        // huff_icc_df (table index 4): symbol 7 at code length 1 -> value 7 + (-7) = 0.
        let data = [0x00]; // Bit '0' followed by zeros.
        let mut bs = BitReaderLtr::new(&data);
        let mut bits_used = 0;
        let val = huffman_decode(&mut bs, PsCodebookId::IccDeltaFreq, &mut bits_used).unwrap();
        assert_eq!(val, 0, "ICC df symbol 7 should decode to 0");
        assert_eq!(bits_used, 1, "Should consume 1 bit");
    }

    #[test]
    fn verify_read_ipd_df_zero() {
        // huff_ipd_df (table index 6): symbol 0 is the last entry with code length 1.
        // Canonical code assignment gives it codeword '1' (the only 1-bit code).
        // So we need MSB=1 -> 0x80.
        let data = [0x80];
        let mut bs = BitReaderLtr::new(&data);
        let mut bits_used = 0;
        let val = huffman_decode(&mut bs, PsCodebookId::IpdDeltaFreq, &mut bits_used).unwrap();
        assert_eq!(val, 0, "IPD df symbol 0 should decode to 0");
        assert_eq!(bits_used, 1, "Should consume 1 bit");
    }

    #[test]
    fn verify_read_par_data_delta_freq() {
        // Test delta-frequency mode: first value absolute, rest relative.
        // Encode 3 zero values using huff_icc_df (shortest code = 1 bit for zero).
        // Three '0' bits -> three zero values.
        let data = [0x00];
        let mut bs = BitReaderLtr::new(&data);
        let mut par = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        let mut bits_used = 0;

        decode_parameter_band(
            &mut bs,
            &mut par,
            PsCodebookId::IccDeltaFreq,
            3,
            0,
            false,
            1,
            &mut bits_used,
        )
        .unwrap();

        assert_eq!(par[0][0], 0);
        assert_eq!(par[0][1], 0);
        assert_eq!(par[0][2], 0);
        assert_eq!(bits_used, 3, "Should consume 3 bits for 3 zero deltas");
    }

    #[test]
    fn verify_read_par_data_delta_time() {
        // Test delta-time mode: decode relative to previous envelope.
        // Previous envelope all zeros, decode zero deltas -> result should be all zeros.
        let data = [0x00, 0x00];
        let mut bs = BitReaderLtr::new(&data);
        let mut par = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        let mut bits_used = 0;

        // env=1 with dt=true should reference env=0 (all zeros).
        decode_parameter_band(
            &mut bs,
            &mut par,
            PsCodebookId::IccDeltaFreq,
            3,
            1,
            true,
            1,
            &mut bits_used,
        )
        .unwrap();

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
        let mut bits_used = 0;

        // With stride=2, decoded zero * 2 = 0 (trivial but verifies no crash).
        decode_parameter_band(
            &mut bs,
            &mut par,
            PsCodebookId::IccDeltaFreq,
            2,
            0,
            false,
            2,
            &mut bits_used,
        )
        .unwrap();

        assert_eq!(par[0][0], 0);
        assert_eq!(par[0][1], 0);
    }

    #[test]
    fn verify_read_ipdopd_data_mod8() {
        // IPD/OPD values should be masked to 3 bits (& 0x07 = mod 8).
        let data = [0x00, 0x00];
        let mut bs = BitReaderLtr::new(&data);
        let mut par = [[0i8; PS_MAX_NR_IIDICC]; PS_MAX_NUM_ENV];
        let mut bits_used = 0;

        decode_phase_parameters(
            &mut bs,
            &mut par,
            PsCodebookId::IpdDeltaFreq,
            3,
            0,
            false,
            &mut bits_used,
        )
        .unwrap();

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
        // - num_env_idx=1 -> 1 envelope (2 bits)
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
        // - num_env_idx=0 -> 0 envelopes (2 bits)
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
        // With 0 envelopes, a fake envelope should be created -> num_env becomes 1.
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

        let result = reset_on_error(&mut bs, &mut ps, 0, 16).unwrap();

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
