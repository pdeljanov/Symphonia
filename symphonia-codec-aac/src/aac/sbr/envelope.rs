// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SBR envelope and noise floor decoding (ISO/IEC 14496-3:2009, 4.6.18.3.4), and dequantization
//! and stereo decoding (4.6.18.3.5).

use symphonia_core::errors::{Result, decode_error};

use crate::aac::sbr::types::{
    FreqBandTables, MAX_ENVELOPES, MAX_NOISE_BANDS, MAX_NOISE_FLOORS, QMF_BANDS, SbrChannelFrame,
};

/// NOISE_FLOOR_OFFSET (4.6.18.2.5).
const NOISE_FLOOR_OFFSET: i32 = 6;

/// panOffset (4.6.18.2.6), indexed by bs_amp_res.
const PAN_OFFSET: [i32; 2] = [24, 12];

/// The maximum value of a quantized noise floor scalefactor of a single channel, or of the first
/// channel of a coupled channel pair (4.6.18.3.6).
const MAX_NOISE_LEVEL: i32 = 30;

/// The delta coded envelope and noise floor scalefactors of one channel of one SBR frame as read
/// by sbr_envelope() and sbr_noise() (Tables 4.72 and 4.73), together with the side information
/// required to decode them.
#[derive(Clone, Debug)]
pub struct DeltaCodedData {
    /// L_E, the number of envelopes.
    pub num_env: usize,
    /// r(l), the frequency resolution of each envelope (true = high).
    pub freq_res: [bool; MAX_ENVELOPES],
    /// bs_df_env(l), true for delta coding in the time direction.
    pub df_env: [bool; MAX_ENVELOPES],
    /// bs_data_env, E_Delta(k, l) indexed `[l][k]`.
    pub data_env: [[i32; QMF_BANDS]; MAX_ENVELOPES],
    /// L_Q, the number of noise floors.
    pub num_noise: usize,
    /// bs_df_noise(l), true for delta coding in the time direction.
    pub df_noise: [bool; MAX_NOISE_FLOORS],
    /// bs_data_noise, Q_Delta(k, l) indexed `[l][k]`.
    pub data_noise: [[i32; MAX_NOISE_BANDS]; MAX_NOISE_FLOORS],
}

impl Default for DeltaCodedData {
    fn default() -> Self {
        Self {
            num_env: 0,
            freq_res: [false; MAX_ENVELOPES],
            df_env: [false; MAX_ENVELOPES],
            data_env: [[0; QMF_BANDS]; MAX_ENVELOPES],
            num_noise: 0,
            df_noise: [false; MAX_NOISE_FLOORS],
            data_noise: [[0; MAX_NOISE_BANDS]; MAX_NOISE_FLOORS],
        }
    }
}

/// The decoded (quantized) envelope scalefactors E(k, l) and noise floor scalefactors Q(k, l) of
/// one channel, indexed `[l][k]`.
#[derive(Clone, Debug)]
pub struct Scalefactors {
    pub env: [[i32; QMF_BANDS]; MAX_ENVELOPES],
    pub noise: [[i32; MAX_NOISE_BANDS]; MAX_NOISE_FLOORS],
}

impl Default for Scalefactors {
    fn default() -> Self {
        Self {
            env: [[0; QMF_BANDS]; MAX_ENVELOPES],
            noise: [[0; MAX_NOISE_BANDS]; MAX_NOISE_FLOORS],
        }
    }
}

/// The scalefactors of the previous SBR frame of one channel required for delta decoding in the
/// time direction across SBR frame boundaries (4.6.18.3.4).
#[derive(Clone, Copy, Debug)]
pub struct DeltaHistory {
    /// E'(k, L'_E - 1), the envelope scalefactors of the last envelope of the previous frame.
    env: [i32; QMF_BANDS],
    /// r'(L'_E - 1), the frequency resolution of the last envelope of the previous frame.
    env_high_res: bool,
    /// True if `env` and `env_high_res` are valid.
    env_valid: bool,
    /// Q'(k, L'_Q - 1), the noise floor scalefactors of the last noise floor of the previous frame.
    noise: [i32; MAX_NOISE_BANDS],
    /// True if `noise` is valid.
    noise_valid: bool,
}

impl Default for DeltaHistory {
    fn default() -> Self {
        Self {
            env: [0; QMF_BANDS],
            env_high_res: false,
            env_valid: false,
            noise: [0; MAX_NOISE_BANDS],
            noise_valid: false,
        }
    }
}

impl DeltaHistory {
    /// Discard the history. Delta coding in the time direction of the first envelope or noise
    /// floor of the next frame is then an error.
    pub fn invalidate(&mut self) {
        *self = Default::default();
    }
}

/// Compute `a + delta * b` with overflow checking.
fn add_delta(a: i32, delta: i32, b: i32) -> Result<i32> {
    match delta.checked_mul(b).and_then(|d| a.checked_add(d)) {
        Some(value) => Ok(value),
        None => decode_error("sbr: scalefactor overflow"),
    }
}

/// Decode the envelope scalefactors E(k, l) and the noise floor scalefactors Q(k, l) of one channel
/// from their delta coded representation (4.6.18.3.4).
///
/// `delta` is δ: 2 for the second channel of a coupled channel pair, 1 otherwise. `history` holds
/// the scalefactors of the previous SBR frame, and is updated with those of the current frame on
/// success. On failure, `history` is left unmodified.
pub fn decode_scalefactors(
    data: &DeltaCodedData,
    tables: &FreqBandTables,
    delta: i32,
    history: &mut DeltaHistory,
    out: &mut Scalefactors,
) -> Result<()> {
    let num_env = data.num_env;
    let num_noise = data.num_noise;

    if num_env == 0 || num_env > MAX_ENVELOPES || num_noise == 0 || num_noise > MAX_NOISE_FLOORS {
        return decode_error("sbr: invalid number of envelopes or noise floors");
    }

    let n_q = tables.f_table_noise.len().saturating_sub(1);

    if n_q > MAX_NOISE_BANDS {
        return decode_error("sbr: too many noise floor bands");
    }

    // Envelope scalefactors.
    for l in 0..num_env {
        let r = data.freq_res[l];
        let n = tables.envelope_table(r).len().saturating_sub(1);

        if n > QMF_BANDS {
            return decode_error("sbr: too many envelope bands");
        }

        let e_delta = &data.data_env[l];

        if !data.df_env[l] {
            // Delta coding in the frequency direction.
            let mut acc = 0;
            for k in 0..n {
                acc = add_delta(acc, delta, e_delta[k])?;
                out.env[l][k] = acc;
            }
        }
        else {
            // Delta coding in the time direction. g_E(k, l) and g(l) are the scalefactors and
            // frequency resolution of the previous envelope, which may be the last envelope of
            // the previous SBR frame.
            let (g, prev) = if l == 0 {
                if !history.env_valid {
                    return decode_error("sbr: time delta coded envelope without history");
                }
                (history.env_high_res, history.env)
            }
            else {
                (data.freq_res[l - 1], out.env[l - 1])
            };

            if r == g {
                for k in 0..n {
                    out.env[l][k] = add_delta(prev[k], delta, e_delta[k])?;
                }
            }
            else if !r {
                // r(l) = 0, g(l) = 1: i(k) is defined by f_TableHigh(i(k)) = f_TableLow(k).
                for k in 0..n {
                    let border = tables.f_table_low[k];
                    let Some(i) = tables.f_table_high.iter().position(|&f| f == border)
                    else {
                        return decode_error("sbr: inconsistent frequency band tables");
                    };
                    out.env[l][k] = add_delta(prev[i], delta, e_delta[k])?;
                }
            }
            else {
                // r(l) = 1, g(l) = 0: i(k) is defined by
                // f_TableLow(i(k)) <= f_TableHigh(k) < f_TableLow(i(k) + 1).
                let f_low = &tables.f_table_low;
                for k in 0..n {
                    let border = tables.f_table_high[k];
                    let Some(i) = f_low.windows(2).position(|w| w[0] <= border && border < w[1])
                    else {
                        return decode_error("sbr: inconsistent frequency band tables");
                    };
                    out.env[l][k] = add_delta(prev[i], delta, e_delta[k])?;
                }
            }
        }
    }

    // Noise floor scalefactors.
    for l in 0..num_noise {
        let q_delta = &data.data_noise[l];

        if !data.df_noise[l] {
            // Delta coding in the frequency direction.
            let mut acc = 0;
            for k in 0..n_q {
                acc = add_delta(acc, delta, q_delta[k])?;
                out.noise[l][k] = acc;
            }
        }
        else {
            // Delta coding in the time direction.
            let prev = if l == 0 {
                if !history.noise_valid {
                    return decode_error("sbr: time delta coded noise floor without history");
                }
                history.noise
            }
            else {
                out.noise[l - 1]
            };

            for k in 0..n_q {
                out.noise[l][k] = add_delta(prev[k], delta, q_delta[k])?;
            }
        }
    }

    // Update the history with the last envelope and noise floor of this frame.
    history.env = out.env[num_env - 1];
    history.env_high_res = data.freq_res[num_env - 1];
    history.env_valid = true;
    history.noise = out.noise[num_noise - 1];
    history.noise_valid = true;

    Ok(())
}

/// The number of envelope bands of each envelope, n(r(l)).
fn num_env_bands(data: &DeltaCodedData, tables: &FreqBandTables, l: usize) -> Result<usize> {
    let n = tables.envelope_table(data.freq_res[l]).len().saturating_sub(1);
    if n > QMF_BANDS {
        return decode_error("sbr: too many envelope bands");
    }
    Ok(n)
}

/// Validate the number of envelopes, noise floors, and noise floor bands.
fn check_counts(data: &DeltaCodedData, tables: &FreqBandTables) -> Result<usize> {
    let n_q = tables.f_table_noise.len().saturating_sub(1);

    if data.num_env > MAX_ENVELOPES || data.num_noise > MAX_NOISE_FLOORS || n_q > MAX_NOISE_BANDS {
        return decode_error("sbr: invalid number of envelopes or noise floors");
    }

    Ok(n_q)
}

/// Convert a dequantized value to `f32`, and verify that it is finite.
fn to_finite(value: f64) -> Result<f32> {
    let value = value as f32;
    if !value.is_finite() {
        return decode_error("sbr: scalefactor out of range");
    }
    Ok(value)
}

/// The envelope scalefactor quantization step divisor a (4.6.18.3.5).
fn amp_res_divisor(bs_amp_res: bool) -> f64 {
    if bs_amp_res { 1.0 } else { 2.0 }
}

/// Dequantize the envelope and noise floor scalefactors of a single channel element or of a
/// channel of a channel pair element without coupling (4.6.18.3.5).
///
/// `bs_amp_res` is the amplitude resolution in effect for the current SBR frame. The quantized
/// noise floor scalefactors are verified to be within [0, 30] (4.6.18.3.6).
pub fn dequantize(
    data: &DeltaCodedData,
    sf: &Scalefactors,
    tables: &FreqBandTables,
    bs_amp_res: bool,
    out: &mut SbrChannelFrame,
) -> Result<()> {
    let n_q = check_counts(data, tables)?;
    let a = amp_res_divisor(bs_amp_res);

    for l in 0..data.num_env {
        for k in 0..num_env_bands(data, tables, l)? {
            // E_Orig(k, l) = 64 * 2^(E(k, l) / a).
            let e = f64::from(sf.env[l][k]);
            out.e_orig[l][k] = to_finite(64.0 * (e / a).exp2())?;
        }
    }

    for l in 0..data.num_noise {
        for k in 0..n_q {
            let q = sf.noise[l][k];

            if !(0..=MAX_NOISE_LEVEL).contains(&q) {
                return decode_error("sbr: noise floor scalefactor out of range");
            }

            // Q_Orig(k, l) = 2^(NOISE_FLOOR_OFFSET - Q(k, l)).
            out.q_orig[l][k] = to_finite(f64::from(NOISE_FLOOR_OFFSET - q).exp2())?;
        }
    }

    Ok(())
}

/// Dequantize and stereo decode the envelope and noise floor scalefactors of a channel pair
/// element with coupling (bs_coupling = 1, 4.6.18.3.5).
///
/// Both channels share the same time/frequency grid, which is taken from `data`. `sf0` holds E_0
/// and Q_0 (the averages), and `sf1` holds E_1 and Q_1 (the ratios). The requirements of
/// 4.6.18.3.6 on the quantized values (Q_0 in [0, 30], Q_1 in [0, 2 * panOffset(1)], E_1 and Q_1
/// even) are verified.
pub fn dequantize_coupled(
    data: &DeltaCodedData,
    sf0: &Scalefactors,
    sf1: &Scalefactors,
    tables: &FreqBandTables,
    bs_amp_res: bool,
    out_left: &mut SbrChannelFrame,
    out_right: &mut SbrChannelFrame,
) -> Result<()> {
    let n_q = check_counts(data, tables)?;
    let a = amp_res_divisor(bs_amp_res);

    let pan_env = f64::from(PAN_OFFSET[usize::from(bs_amp_res)]);
    let pan_noise = PAN_OFFSET[1];

    for l in 0..data.num_env {
        for k in 0..num_env_bands(data, tables, l)? {
            let e0 = sf0.env[l][k];
            let e1 = sf1.env[l][k];

            if e1 % 2 != 0 {
                return decode_error("sbr: odd envelope scalefactor ratio");
            }

            // E_LeftOrig(k, l) = 64 * 2^(E_0(k, l) / a + 1) /
            //     (1 + 2^((panOffset(bs_amp_res) - E_1(k, l)) / a)),
            // E_RightOrig(k, l) = 64 * 2^(E_0(k, l) / a + 1) /
            //     (1 + 2^((E_1(k, l) - panOffset(bs_amp_res)) / a)).
            let level = 64.0 * (f64::from(e0) / a + 1.0).exp2();
            let ratio = (pan_env - f64::from(e1)) / a;

            out_left.e_orig[l][k] = to_finite(level / (1.0 + ratio.exp2()))?;
            out_right.e_orig[l][k] = to_finite(level / (1.0 + (-ratio).exp2()))?;
        }
    }

    for l in 0..data.num_noise {
        for k in 0..n_q {
            let q0 = sf0.noise[l][k];
            let q1 = sf1.noise[l][k];

            if !(0..=MAX_NOISE_LEVEL).contains(&q0) {
                return decode_error("sbr: noise floor scalefactor out of range");
            }

            if !(0..=2 * pan_noise).contains(&q1) || q1 % 2 != 0 {
                return decode_error("sbr: invalid noise floor scalefactor ratio");
            }

            // Q_OrigLeft(k, l) = 2^(NOISE_FLOOR_OFFSET - Q_0(k, l) + 1) /
            //     (1 + 2^(panOffset(1) - Q_1(k, l))),
            // Q_OrigRight(k, l) = 2^(NOISE_FLOOR_OFFSET - Q_0(k, l) + 1) /
            //     (1 + 2^(Q_1(k, l) - panOffset(1))).
            let level = f64::from(NOISE_FLOOR_OFFSET - q0 + 1).exp2();
            let ratio = f64::from(pan_noise - q1);

            out_left.q_orig[l][k] = to_finite(level / (1.0 + ratio.exp2()))?;
            out_right.q_orig[l][k] = to_finite(level / (1.0 + (-ratio).exp2()))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tables with f_TableHigh = [10, 11, 12, 13, 14], f_TableLow = [10, 12, 14] and
    /// f_TableNoise = [10, 12, 14].
    fn tables() -> FreqBandTables {
        FreqBandTables {
            k0: 8,
            k2: 14,
            kx: 10,
            m: 4,
            f_master: vec![8, 9, 10, 11, 12, 13, 14],
            f_table_high: vec![10, 11, 12, 13, 14],
            f_table_low: vec![10, 12, 14],
            f_table_noise: vec![10, 12, 14],
            f_table_lim: vec![10, 14],
            patches: vec![],
        }
    }

    /// Tables with an odd number of high resolution bands: f_TableHigh = [10, 11, 12, 13],
    /// f_TableLow = [10, 11, 13].
    fn odd_tables() -> FreqBandTables {
        FreqBandTables {
            f_table_high: vec![10, 11, 12, 13],
            f_table_low: vec![10, 11, 13],
            f_table_noise: vec![10, 13],
            ..tables()
        }
    }

    fn data(freq_res: &[bool], df_env: &[bool], df_noise: &[bool]) -> DeltaCodedData {
        let mut d = DeltaCodedData {
            num_env: freq_res.len(),
            num_noise: df_noise.len(),
            ..Default::default()
        };
        d.freq_res[..freq_res.len()].copy_from_slice(freq_res);
        d.df_env[..df_env.len()].copy_from_slice(df_env);
        d.df_noise[..df_noise.len()].copy_from_slice(df_noise);
        d
    }

    #[test]
    fn verify_frequency_delta_decoding() {
        let t = tables();
        let mut d = data(&[true, false], &[false, false], &[false]);
        d.data_env[0][..4].copy_from_slice(&[10, 1, -2, 3]);
        d.data_env[1][..2].copy_from_slice(&[20, -5]);
        d.data_noise[0][..2].copy_from_slice(&[7, 2]);

        let mut history = DeltaHistory::default();
        let mut sf = Scalefactors::default();
        decode_scalefactors(&d, &t, 1, &mut history, &mut sf).unwrap();

        // E(k, l) = sum_{i=0}^{k} E_Delta(i, l).
        assert_eq!(sf.env[0][..4], [10, 11, 9, 12]);
        assert_eq!(sf.env[1][..2], [20, 15]);
        assert_eq!(sf.noise[0][..2], [7, 9]);

        // The history holds the last envelope (low resolution) and the last noise floor.
        assert!(history.env_valid && history.noise_valid);
        assert_eq!(history.env[..2], [20, 15]);
        assert!(!history.env_high_res);
        assert_eq!(history.noise[..2], [7, 9]);

        // δ = 2 for the second channel of a coupled channel pair.
        let mut history = DeltaHistory::default();
        decode_scalefactors(&d, &t, 2, &mut history, &mut sf).unwrap();
        assert_eq!(sf.env[0][..4], [20, 22, 18, 24]);
        assert_eq!(sf.noise[0][..2], [14, 18]);
    }

    #[test]
    fn verify_time_delta_decoding() {
        let t = tables();

        // Frame 1: one high resolution envelope, two noise floors.
        let mut d1 = data(&[true], &[false], &[false, true]);
        d1.data_env[0][..4].copy_from_slice(&[10, 2, 2, 2]);
        d1.data_noise[0][..2].copy_from_slice(&[5, 1]);
        d1.data_noise[1][..2].copy_from_slice(&[1, -1]);

        let mut history = DeltaHistory::default();
        let mut sf = Scalefactors::default();
        decode_scalefactors(&d1, &t, 1, &mut history, &mut sf).unwrap();
        assert_eq!(sf.env[0][..4], [10, 12, 14, 16]);
        assert_eq!(sf.noise[0][..2], [5, 6]);
        // Q(k, 1) = Q(k, 0) + Q_Delta(k, 1).
        assert_eq!(sf.noise[1][..2], [6, 5]);

        // Frame 2: a low resolution envelope delta coded in time with respect to the last (high
        // resolution) envelope of frame 1, followed by a high resolution envelope delta coded in
        // time with respect to the low resolution envelope.
        let mut d2 = data(&[false, true], &[true, true], &[true]);
        d2.data_env[0][..2].copy_from_slice(&[1, -1]);
        d2.data_env[1][..4].copy_from_slice(&[0, 1, 2, 3]);
        d2.data_noise[0][..2].copy_from_slice(&[-1, 2]);

        decode_scalefactors(&d2, &t, 1, &mut history, &mut sf).unwrap();

        // r(0) = 0, g(0) = 1: f_TableHigh(i(k)) = f_TableLow(k), i = [0, 2].
        // E(k, 0) = E'(i(k)) + E_Delta(k, 0) = [10 + 1, 14 - 1].
        assert_eq!(sf.env[0][..2], [11, 13]);

        // r(1) = 1, g(1) = 0: f_TableLow(i(k)) <= f_TableHigh(k) < f_TableLow(i(k) + 1),
        // i = [0, 0, 1, 1]. E(k, 1) = E(i(k), 0) + E_Delta(k, 1).
        assert_eq!(sf.env[1][..4], [11, 12, 15, 16]);

        // Q(k, 0) = Q'(k, L'_Q - 1) + Q_Delta(k, 0).
        assert_eq!(sf.noise[0][..2], [5, 7]);
    }

    #[test]
    fn verify_time_delta_decoding_odd_bands() {
        let t = odd_tables();

        // A high resolution envelope followed by a low resolution envelope and another high
        // resolution envelope, the latter two delta coded in time.
        let mut d = data(&[true, false, true], &[false, true, true], &[false, false]);
        d.data_env[0][..3].copy_from_slice(&[10, 1, 1]);
        d.data_env[1][..2].copy_from_slice(&[0, 0]);
        d.data_env[2][..3].copy_from_slice(&[0, 0, 0]);

        let mut history = DeltaHistory::default();
        let mut sf = Scalefactors::default();
        decode_scalefactors(&d, &t, 1, &mut history, &mut sf).unwrap();

        assert_eq!(sf.env[0][..3], [10, 11, 12]);
        // i(k) such that f_TableHigh(i(k)) = f_TableLow(k): i = [0, 1].
        assert_eq!(sf.env[1][..2], [10, 11]);
        // f_TableHigh = [10, 11, 12] lies in the low bands [0, 1, 1].
        assert_eq!(sf.env[2][..3], [10, 11, 11]);
    }

    #[test]
    fn verify_time_delta_without_history() {
        let t = tables();
        let mut sf = Scalefactors::default();

        let d = data(&[true], &[true], &[false]);
        let mut history = DeltaHistory::default();
        assert!(decode_scalefactors(&d, &t, 1, &mut history, &mut sf).is_err());
        assert!(!history.env_valid);

        let d = data(&[true], &[false], &[true]);
        assert!(decode_scalefactors(&d, &t, 1, &mut history, &mut sf).is_err());

        // After a successful decode, delta coding in the time direction is allowed.
        let d = data(&[true], &[false], &[false]);
        decode_scalefactors(&d, &t, 1, &mut history, &mut sf).unwrap();
        let d = data(&[true], &[true], &[true]);
        decode_scalefactors(&d, &t, 1, &mut history, &mut sf).unwrap();

        // After invalidation, it is not.
        history.invalidate();
        assert!(decode_scalefactors(&d, &t, 1, &mut history, &mut sf).is_err());
    }

    #[test]
    fn verify_scalefactor_overflow() {
        let t = tables();
        let mut sf = Scalefactors::default();
        let mut history = DeltaHistory::default();

        let mut d = data(&[true], &[false], &[false]);
        d.data_env[0][..2].copy_from_slice(&[i32::MAX, 1]);
        assert!(decode_scalefactors(&d, &t, 1, &mut history, &mut sf).is_err());
    }

    #[test]
    fn verify_dequantization() {
        let t = tables();
        let mut d = data(&[true, false], &[false, false], &[false]);
        d.num_noise = 1;

        let mut sf = Scalefactors::default();
        sf.env[0][..4].copy_from_slice(&[0, 1, 6, -2]);
        sf.env[1][..2].copy_from_slice(&[3, 4]);
        sf.noise[0][..2].copy_from_slice(&[0, 6]);

        let mut out = SbrChannelFrame::default();

        // bs_amp_res = 0, a = 2: E_Orig = 64 * 2^(E / 2).
        dequantize(&d, &sf, &t, false, &mut out).unwrap();
        let expected = [64.0, 64.0 * 2f32.sqrt(), 512.0, 32.0];
        for (e, x) in out.e_orig[0][..4].iter().zip(expected) {
            assert!((e - x).abs() <= 1e-4 * x);
        }
        assert!((out.e_orig[1][0] - 64.0 * 8f32.sqrt()).abs() < 1e-3);
        assert_eq!(out.e_orig[1][1], 256.0);

        // Q_Orig = 2^(6 - Q).
        assert_eq!(out.q_orig[0][..2], [64.0, 1.0]);

        // bs_amp_res = 1, a = 1: E_Orig = 64 * 2^E.
        dequantize(&d, &sf, &t, true, &mut out).unwrap();
        assert_eq!(out.e_orig[0][..4], [64.0, 128.0, 4096.0, 16.0]);
        assert_eq!(out.e_orig[1][..2], [512.0, 1024.0]);

        // Noise floor scalefactors must be within [0, 30].
        sf.noise[0][1] = 30;
        assert!(dequantize(&d, &sf, &t, true, &mut out).is_ok());
        sf.noise[0][1] = 31;
        assert!(dequantize(&d, &sf, &t, true, &mut out).is_err());
        sf.noise[0][1] = -1;
        assert!(dequantize(&d, &sf, &t, true, &mut out).is_err());
        sf.noise[0][1] = 0;

        // Envelope scalefactors that cannot be represented are an error.
        sf.env[0][0] = 1000;
        assert!(dequantize(&d, &sf, &t, true, &mut out).is_err());
    }

    #[test]
    fn verify_coupled_dequantization() {
        let t = tables();
        let d = data(&[false], &[false], &[false]);

        let mut sf0 = Scalefactors::default();
        let mut sf1 = Scalefactors::default();
        let mut left = SbrChannelFrame::default();
        let mut right = SbrChannelFrame::default();

        // bs_amp_res = 1 (a = 1, panOffset = 12).
        sf0.env[0][..2].copy_from_slice(&[4, 4]);
        sf1.env[0][..2].copy_from_slice(&[12, 14]);
        sf0.noise[0][..2].copy_from_slice(&[6, 6]);
        sf1.noise[0][..2].copy_from_slice(&[12, 10]);

        dequantize_coupled(&d, &sf0, &sf1, &t, true, &mut left, &mut right).unwrap();

        // E_1 = panOffset: both channels get 64 * 2^(4 + 1) / 2 = 1024.
        assert_eq!((left.e_orig[0][0], right.e_orig[0][0]), (1024.0, 1024.0));
        // E_1 = 14: left = 2048 / (1 + 2^-2), right = 2048 / (1 + 2^2).
        assert!((left.e_orig[0][1] - 1638.4).abs() < 1e-3);
        assert!((right.e_orig[0][1] - 409.6).abs() < 1e-3);

        // Q_1 = panOffset(1): 2^(6 - 6 + 1) / 2 = 1 for both channels.
        assert_eq!((left.q_orig[0][0], right.q_orig[0][0]), (1.0, 1.0));
        // Q_1 = 10: left = 2 / (1 + 2^2), right = 2 / (1 + 2^-2).
        assert!((left.q_orig[0][1] - 0.4).abs() < 1e-6);
        assert!((right.q_orig[0][1] - 1.6).abs() < 1e-6);

        // bs_amp_res = 0 (a = 2, panOffset = 24). E_0 = 4, E_1 = 20:
        // left = 64 * 2^3 / (1 + 2^2) = 102.4, right = 512 / (1 + 2^-2) = 409.6.
        sf1.env[0][..2].copy_from_slice(&[20, 24]);
        dequantize_coupled(&d, &sf0, &sf1, &t, false, &mut left, &mut right).unwrap();
        assert!((left.e_orig[0][0] - 102.4).abs() < 1e-3);
        assert!((right.e_orig[0][0] - 409.6).abs() < 1e-3);
        assert_eq!((left.e_orig[0][1], right.e_orig[0][1]), (256.0, 256.0));

        // The sum of both channels equals 64 * 2^(E_0 / a + 1).
        for k in 0..2 {
            assert!((left.e_orig[0][k] + right.e_orig[0][k] - 512.0).abs() < 1e-3);
        }

        // E_1 shall be even.
        sf1.env[0][0] = 13;
        assert!(dequantize_coupled(&d, &sf0, &sf1, &t, true, &mut left, &mut right).is_err());
        sf1.env[0][0] = 12;

        // Q_1 shall be even and within [0, 24].
        sf1.noise[0][0] = 11;
        assert!(dequantize_coupled(&d, &sf0, &sf1, &t, true, &mut left, &mut right).is_err());
        sf1.noise[0][0] = 26;
        assert!(dequantize_coupled(&d, &sf0, &sf1, &t, true, &mut left, &mut right).is_err());
        sf1.noise[0][0] = 24;
        assert!(dequantize_coupled(&d, &sf0, &sf1, &t, true, &mut left, &mut right).is_ok());

        // Q_0 shall be within [0, 30].
        sf0.noise[0][0] = 31;
        assert!(dequantize_coupled(&d, &sf0, &sf1, &t, true, &mut left, &mut right).is_err());
    }
}
