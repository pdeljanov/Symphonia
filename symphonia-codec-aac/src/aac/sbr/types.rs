// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Types shared by the SBR bitstream decoder, the SBR signal processing, and the parametric stereo
//! (PS) tool.
//!
//! Clause and table references are to ISO/IEC 14496-3:2009. Two-dimensional QMF-domain arrays are
//! indexed `[l][k]`, where `l` is the QMF subsample (time) index and `k` is the QMF subband
//! (frequency) index. The specification writes these as `X(k, l)`.

use symphonia_core::dsp::complex::Complex;

/// Number of SBR time slots per frame, numTimeSlots, for 1024-sample AAC frames.
pub const NUM_TIME_SLOTS: usize = 16;
/// Number of QMF subsamples per SBR time slot, RATE.
pub const RATE: usize = 2;
/// Number of QMF subsamples per frame, numTimeSlots * RATE.
pub const QMF_SLOTS: usize = NUM_TIME_SLOTS * RATE;
/// Number of subbands of the 64-band QMF domain.
pub const QMF_BANDS: usize = 64;
/// Number of subbands of the 32-band analysis QMF bank.
pub const QMF_ANALYSIS_BANDS: usize = 32;
/// t_HFGen (4.6.18.2.5).
pub const T_HF_GEN: usize = 8;
/// t_HFAdj (4.6.18.2.5).
pub const T_HF_ADJ: usize = 2;
/// Number of look-ahead QMF subsamples of the low band provided to the PS tool (Annex 8.A.3).
pub const PS_LOOKAHEAD: usize = 6;
/// Number of low QMF subbands for which look-ahead subsamples are provided to the PS tool
/// (Annex 8.A.3).
pub const PS_LOOKAHEAD_BANDS: usize = 5;
/// Maximum number of envelopes, L_E, in an SBR frame.
pub const MAX_ENVELOPES: usize = 5;
/// Maximum number of noise floors, L_Q, in an SBR frame.
pub const MAX_NOISE_FLOORS: usize = 2;
/// Maximum number of noise floor bands, N_Q.
pub const MAX_NOISE_BANDS: usize = 5;
/// Maximum number of HF generation patches.
pub const MAX_PATCHES: usize = 5;

/// The 64-band QMF representation of one frame of one channel.
///
/// Rows `0..QMF_SLOTS` hold X(k, l), the output of the SBR tool immediately prior to the 64-band
/// QMF synthesis filterbank. Rows `QMF_SLOTS..QMF_SLOTS + PS_LOOKAHEAD` hold the low band
/// look-ahead X_Low(k, l + t_HFAdj) for `k < PS_LOOKAHEAD_BANDS` and are zero for all other
/// subbands. Together, these form the matrix X_input of Annex 8.A.3.
pub type QmfFrame = [[Complex<f32>; QMF_BANDS]; QMF_SLOTS + PS_LOOKAHEAD];

/// Output of the PS tool for one channel: X(k, l) for `0 <= l < QMF_SLOTS`.
pub type QmfSlots = [[Complex<f32>; QMF_BANDS]; QMF_SLOTS];

/// The fields of sbr_header() (Table 4.63).
///
/// Fields of the optional header parts that are not present in the bitstream take their default
/// values as specified in 4.5.2.8.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SbrHeader {
    pub bs_amp_res: bool,
    pub bs_start_freq: u8,
    pub bs_stop_freq: u8,
    pub bs_xover_band: u8,
    pub bs_freq_scale: u8,
    pub bs_alter_scale: bool,
    pub bs_noise_bands: u8,
    pub bs_limiter_bands: u8,
    pub bs_limiter_gains: u8,
    pub bs_interpol_freq: bool,
    pub bs_smoothing_mode: bool,
}

/// A HF generation patch (4.6.18.6.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Patch {
    /// patchNumSubbands.
    pub num_subbands: usize,
    /// patchStartSubband.
    pub start_subband: usize,
}

/// The frequency band tables (4.6.18.3.2) and HF generation patches (4.6.18.6.3) derived from an
/// SBR header and the SBR sampling frequency.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FreqBandTables {
    /// k0, the first QMF subband of the master frequency band table.
    pub k0: usize,
    /// k2, the last QMF subband (exclusive) of the master frequency band table.
    pub k2: usize,
    /// k_x, the first QMF subband of the SBR range.
    pub kx: usize,
    /// M, the number of QMF subbands of the SBR range.
    pub m: usize,
    /// f_master, N_master + 1 entries.
    pub f_master: Vec<usize>,
    /// f_TableHigh, N_high + 1 entries.
    pub f_table_high: Vec<usize>,
    /// f_TableLow, N_low + 1 entries.
    pub f_table_low: Vec<usize>,
    /// f_TableNoise, N_Q + 1 entries.
    pub f_table_noise: Vec<usize>,
    /// f_TableLim, N_L + 1 entries.
    pub f_table_lim: Vec<usize>,
    /// The HF generation patches, numPatches entries.
    pub patches: Vec<Patch>,
}

impl FreqBandTables {
    /// Get the envelope frequency band table for the frequency resolution r: f_TableHigh if
    /// `high_res` is true, otherwise f_TableLow.
    pub fn envelope_table(&self, high_res: bool) -> &[usize] {
        if high_res { &self.f_table_high } else { &self.f_table_low }
    }
}

/// The SBR data of one channel for one SBR frame after decoding of the time/frequency grid
/// (4.6.18.3.3), envelope and noise floor decoding (4.6.18.3.4), and dequantization and stereo
/// decoding (4.6.18.3.5).
#[derive(Clone, Debug, PartialEq)]
pub struct SbrChannelFrame {
    /// L_E, the number of envelopes (1..=MAX_ENVELOPES).
    pub num_env: usize,
    /// t_E, the envelope time borders in SBR time slots, L_E + 1 entries are valid.
    pub t_env: [usize; MAX_ENVELOPES + 1],
    /// r(l), the frequency resolution of each envelope (true = high), L_E entries are valid.
    pub freq_res: [bool; MAX_ENVELOPES],
    /// L_Q, the number of noise floors (1..=MAX_NOISE_FLOORS).
    pub num_noise: usize,
    /// t_Q, the noise floor time borders in SBR time slots, L_Q + 1 entries are valid.
    pub t_noise: [usize; MAX_NOISE_FLOORS + 1],
    /// l_A, or None if l_A = -1.
    pub l_a: Option<usize>,
    /// bs_invf_mode for each noise floor band, N_Q entries are valid.
    pub invf_mode: [u8; MAX_NOISE_BANDS],
    /// E_Orig(k, l) indexed `[l][k]`, for `l < L_E` and `k < n(r(l))`.
    pub e_orig: [[f32; QMF_BANDS]; MAX_ENVELOPES],
    /// Q_Orig(k, l) indexed `[l][k]`, for `l < L_Q` and `k < N_Q`.
    pub q_orig: [[f32; MAX_NOISE_BANDS]; MAX_NOISE_FLOORS],
    /// bs_add_harmonic(k) for each high frequency resolution band `k < N_high`. All entries are
    /// false if bs_add_harmonic_flag is 0.
    pub add_harmonic: [bool; QMF_BANDS],
}

impl Default for SbrChannelFrame {
    fn default() -> Self {
        Self {
            num_env: 0,
            t_env: [0; MAX_ENVELOPES + 1],
            freq_res: [false; MAX_ENVELOPES],
            num_noise: 0,
            t_noise: [0; MAX_NOISE_FLOORS + 1],
            l_a: None,
            invf_mode: [0; MAX_NOISE_BANDS],
            e_orig: [[0.0; QMF_BANDS]; MAX_ENVELOPES],
            q_orig: [[0.0; MAX_NOISE_BANDS]; MAX_NOISE_FLOORS],
            add_harmonic: [false; QMF_BANDS],
        }
    }
}

/// The decoded data of one SBR frame of a syntactic element.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SbrFrame {
    /// The channel data. Only the first entry is valid for a single channel element.
    pub channels: [SbrChannelFrame; 2],
    /// True if the frequency band tables were (re-)derived for this frame because a new or changed
    /// SBR header was received.
    pub reset: bool,
    /// True if ps_data() was read for this frame.
    pub ps_data_present: bool,
}
