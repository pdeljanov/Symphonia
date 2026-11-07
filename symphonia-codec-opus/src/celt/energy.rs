//! CELT Coarse, Fine, and Final Energy Quantization (RFC 6716 Section 4.3.2).
//!
//! In CELT, each of the 21 critical (Bark-scale) frequency bands is normalized to unit energy
//! before Pyramid Vector Quantization (PVQ) of the spectral shape. The band log-energies (in 6 dB
//! base-2 log units relative to [`E_MEANS`](crate::celt::constant::E_MEANS)) are transmitted in
//! three progressive stages within every frame:
//!
//! 1. **Coarse Energy ([`unquant_coarse_energy`], RFC 6716 Section 4.3.2.1)**:
//!    Quantizes band energies in 6 dB steps using a 2D autoregressive moving-average (ARMA)
//!    predictor across time (previous frame energy with coefficient $\alpha$) and frequency
//!    (previous band energy within the current frame with coefficient $1 - \beta$). The integer
//!    prediction residual $q_i$ is entropy-coded with a band-dependent Laplace distribution
//!    parameterized by [`E_PROB_MODEL`](crate::celt::constant::E_PROB_MODEL).
//!
//! 2. **Fine Energy ([`unquant_fine_energy`], RFC 6716 Section 4.3.2.2)**:
//!    After dynamic bit allocation determines `fine_quant[i]` extra bits for band $i$, refines
//!    the coarse quantization interval $[-0.5, +0.5)$ by reading `fine_quant[i]` raw uniform bits
//!    per channel and adding the sub-step offset:
//!    $$\text{offset} = \frac{q_2 + 0.5}{2^{\text{fine\_quant}[i]}} - 0.5$$
//!
//! 3. **Final Energy Bits ([`unquant_energy_finalise`], RFC 6716 Section 4.3.2.3)**:
//!    Consumes any surplus bits left at the very end of the frame (after PVQ shape decoding) by
//!    appending 1 extra bit of precision to bands in two priority passes (`prio = 0`, then
//!    `prio = 1`):
//!    $$\text{offset} = \frac{q_2 - 0.5}{2^{\text{fine\_quant}[i] + 1}}$$

use crate::celt::constant::{
    BETA_COEF, BETA_INTRA, E_PROB_MODEL, MAX_BANDS, MAX_CHANNELS, MAX_FINE_BITS, PRED_COEF,
    SMALL_ENERGY_ICDF,
};
use crate::entropy::Decoder;
use symphonia_core::errors::Result;

/// Minimum clamped previous-frame band log-energy (-9.0 in base-2 log units, or -54 dB)
/// used as input to the inter-frame coarse energy predictor (RFC 6716 Section 4.3.2.1).
const MIN_COARSE_PREDICTION_ENERGY: f32 = -9.0;

/// Minimum remaining bit budget required to decode a full Laplace-distributed coarse
/// energy residual symbol (RFC 6716 Section 4.3.2.1).
const LAPLACE_MIN_BUDGET_BITS: i32 = 15;

/// Decodes the 1-bit `intra_ener` flag from the bitstream if at least 3 bits remain
/// in the frame budget (RFC 6716 Section 4.3.1).
///
/// When `tell() + 3 > total_bits`, intra-frame energy mode is forced to `false` without
/// reading from the range coder.
pub fn decode_intra_energy_flag(range_decoder: &mut Decoder<'_>, total_bits: i32) -> Result<bool> {
    if range_decoder.tell() + 3 <= total_bits {
        let bit = range_decoder.decode_symbol_logp(3)?;
        return Ok(bit != 0);
    }
    return Ok(false);
}

/// Decodes a single signed integer coarse energy residual $q_i$ for one band and channel
/// according to the remaining bit budget (RFC 6716 Section 4.3.2.1).
///
/// - If `budget_bits - tell >= 15`: decodes from the band's Laplace model `(zero_freq, decay)`.
/// - If `budget_bits - tell >= 2`: decodes from [`SMALL_ENERGY_ICDF`] (`{0, -1, +1}`).
/// - If `budget_bits - tell >= 1`: decodes a 1-bit symbol in `{0, -1}` with $\log_2 P = 1$.
/// - Otherwise: returns `-1` (-6 dB decay fallback).
#[inline]
fn decode_coarse_energy_residual(
    range_decoder: &mut Decoder<'_>,
    budget_bits: i32,
    zero_freq: u32,
    decay: i32,
) -> Result<i32> {
    let remaining_bits = budget_bits - range_decoder.tell();
    if remaining_bits >= LAPLACE_MIN_BUDGET_BITS {
        let q_i = range_decoder.decode_laplace(zero_freq, decay);
        return Ok(q_i);
    }
    if remaining_bits >= 2 {
        let sym = range_decoder.decode_symbol_with_icdf(&SMALL_ENERGY_ICDF)? as i32;
        let q_i = (sym >> 1) ^ -(sym & 1);
        return Ok(q_i);
    }
    if remaining_bits >= 1 {
        let bit = range_decoder.decode_symbol_logp(1)? as i32;
        return Ok(-bit);
    }
    return Ok(-1);
}

/// Core implementation of coarse energy unquantization (RFC 6716 Section 4.3.2.1,
/// `unquant_coarse_energy` in `quant_bands.c`).
///
/// Updates `old_band_e[0..channels][start..end]` in place using the 2D prediction recurrence:
/// $$E(c, i) = \alpha \cdot \max(E_{\text{prev}}(c, i), -9.0) + S(c) + q_i$$
/// $$S(c) \leftarrow S(c) + (1 - \beta) \cdot q_i$$
/// where $\alpha = 0, \beta = \text{BETA\_INTRA}$ in intra mode, and
/// $\alpha = \text{PRED\_COEF}[\text{lm}], \beta = \text{BETA\_COEF}[\text{lm}]$ in inter mode.
pub fn unquant_coarse_energy_impl(
    range_decoder: &mut Decoder<'_>,
    start: usize,
    end: usize,
    intra: bool,
    lm: usize,
    channels: usize,
    old_band_e: &mut [[f32; MAX_BANDS]],
) -> Result<()> {
    let (alpha, beta) = if intra {
        (0.0_f32, BETA_INTRA)
    } else {
        (PRED_COEF[lm], BETA_COEF[lm])
    };

    let prob_model = &E_PROB_MODEL[lm][usize::from(intra)];
    let budget_bits = (range_decoder.storage_bytes() * 8) as i32;
    let mut freq_pred_state = [0.0_f32; MAX_CHANNELS];

    for band in start..end {
        let model_band = band.min(MAX_BANDS - 1);
        let zero_freq = u32::from(prob_model[2 * model_band]) << 7;
        let decay = i32::from(prob_model[2 * model_band + 1]) << 6;

        for (channel_energies, prev_state) in old_band_e
            .iter_mut()
            .take(channels)
            .zip(freq_pred_state.iter_mut().take(channels))
        {
            let q_i = decode_coarse_energy_residual(range_decoder, budget_bits, zero_freq, decay)?;
            let q = q_i as f32;

            let clamped_old = channel_energies[band].max(MIN_COARSE_PREDICTION_ENERGY);
            let predicted = alpha * clamped_old + *prev_state + q;
            channel_energies[band] = predicted;
            *prev_state = *prev_state + q - beta * q;
        }
    }

    return Ok(());
}

/// Decodes coarse energy quantization for bands `start..end` across `channels` channels
/// (RFC 6716 Section 4.3.2.1).
///
/// When `channels == 1` (mono stream), callers that maintain a stereo state buffer should
/// pre-merge `old_band_e[0][i] = old_band_e[0][i].max(old_band_e[1][i])` before invoking
/// coarse energy decoding, matching `celt_decoder.c`.
pub fn unquant_coarse_energy(
    range_decoder: &mut Decoder<'_>,
    start: usize,
    end: usize,
    intra: bool,
    lm: usize,
    channels: usize,
    old_band_e: &mut [[f32; MAX_BANDS]],
) -> Result<()> {
    return unquant_coarse_energy_impl(range_decoder, start, end, intra, lm, channels, old_band_e);
}

/// Applies packet-loss recovery safety clamping to `old_band_e` prior to inter-frame coarse
/// energy prediction when recovering from one or more lost frames (`celt_decoder.c` lines 1469-1497).
///
/// If the previous frame was lost (`loss_duration > 0`) and the current frame uses inter-frame
/// energy prediction (`!intra`), this prevents loud energy spikes by continuing a downward energy
/// trend or taking the minimum across the last three frames, plus a short-frame safety margin.
pub fn apply_loss_recovery_energy_safety(
    start: usize,
    end: usize,
    lm: usize,
    loss_duration: usize,
    old_band_e: &mut [[f32; MAX_BANDS]; MAX_CHANNELS],
    old_log_e: &[[f32; MAX_BANDS]; MAX_CHANNELS],
    old_log_e2: &[[f32; MAX_BANDS]; MAX_CHANNELS],
) {
    let missing = (loss_duration >> lm).min(10) as f32;
    let safety = match lm {
        0 => 1.5_f32,
        1 => 0.5_f32,
        _ => 0.0_f32,
    };

    for ((band_e_ch, log_e_ch), log_e2_ch) in old_band_e
        .iter_mut()
        .zip(old_log_e.iter())
        .zip(old_log_e2.iter())
    {
        for band in start..end {
            let e0 = band_e_ch[band];
            let e1 = log_e_ch[band];
            let e2 = log_e2_ch[band];

            let adjusted = if e0 < e1.max(e2) {
                let slope = (e1 - e0).max(0.5 * (e2 - e0)).min(2.0);
                let decayed = e0 - ((1.0 + missing) * slope).max(0.0);
                decayed.max(-20.0)
            } else {
                e0.min(e1).min(e2)
            };

            band_e_ch[band] = adjusted - safety;
        }
    }

    return;
}

/// Decodes fine energy quantization bits for bands `start..end` across `channels` channels
/// (RFC 6716 Section 4.3.2.2, `unquant_fine_energy` in `quant_bands.c`).
///
/// For each band $i \in [\text{start}, \text{end})$ with `fine_quant[i] > 0`, reads `fine_quant[i]`
/// raw bits from `range_decoder` for each channel and refines `old_band_e[c][i]`:
/// $$\text{offset} = (q_2 + 0.5) \cdot 2^{14 - \text{fine\_quant}[i]} \cdot 2^{-14} - 0.5$$
pub fn unquant_fine_energy(
    range_decoder: &mut Decoder<'_>,
    start: usize,
    end: usize,
    channels: usize,
    fine_quant: &[i32; MAX_BANDS],
    old_band_e: &mut [[f32; MAX_BANDS]],
) -> Result<()> {
    let budget_bits = (range_decoder.storage_bytes() * 8) as i32;
    let num_channels = channels as i32;

    for (band, &fine_bits) in fine_quant.iter().enumerate().take(end).skip(start) {
        if fine_bits <= 0 {
            continue;
        }
        if range_decoder.tell() + num_channels * fine_bits > budget_bits {
            continue;
        }

        let scale = ((1_i32 << (14 - fine_bits)) as f32) * (1.0_f32 / 16384.0_f32);
        for channel_energies in old_band_e.iter_mut().take(channels) {
            let q2 = range_decoder.decode_raw_bits(fine_bits as u32)? as f32;
            let offset = (q2 + 0.5_f32) * scale - 0.5_f32;
            channel_energies[band] += offset;
        }
    }

    return Ok(());
}

/// Decodes final energy quantization bits using surplus bits remaining at the end of the frame
/// (RFC 6716 Section 4.3.2.3, `unquant_energy_finalise` in `quant_bands.c`).
///
/// Iterates over priority `0` then priority `1`. For each band $i \in [\text{start}, \text{end})$
/// matching the priority and having `fine_quant[i] < MAX_FINE_BITS`, consumes 1 raw bit per channel
/// (while `bits_left >= channels`) and refines `old_band_e[c][i]`:
/// $$\text{offset} = (q_2 - 0.5) \cdot 2^{14 - \text{fine\_quant}[i] - 1} \cdot 2^{-14}$$
#[allow(clippy::too_many_arguments)]
pub fn unquant_energy_finalise(
    range_decoder: &mut Decoder<'_>,
    start: usize,
    end: usize,
    channels: usize,
    fine_quant: &[i32; MAX_BANDS],
    fine_priority: &[i32; MAX_BANDS],
    bits_left: i32,
    old_band_e: &mut [[f32; MAX_BANDS]],
) -> Result<()> {
    let mut remaining_bits = bits_left;
    let num_channels = channels as i32;

    for priority in 0..2 {
        for band in start..end {
            if remaining_bits < num_channels {
                break;
            }
            let fine_bits = fine_quant[band];
            if fine_bits >= MAX_FINE_BITS || fine_priority[band] != priority {
                continue;
            }

            let scale = ((1_i32 << (14 - fine_bits - 1)) as f32) * (1.0_f32 / 16384.0_f32);
            for channel_energies in old_band_e.iter_mut().take(channels) {
                let q2 = range_decoder.decode_raw_bits(1)? as f32;
                let offset = (q2 - 0.5_f32) * scale;
                channel_energies[band] += offset;
                remaining_bits -= 1;
            }
        }
    }

    return Ok(());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies Mono, Inter (`intra = false`), `LM = 3` (20 ms), `start = 0, end = 21`
    /// coarse, fine, and final energy decoding bit-for-bit against the `libopus 1.6.1` float C oracle.
    #[test]
    fn test_energy_mono_inter_lm3_oracle() {
        let mut packet = [0u8; 32];
        for (i, byte) in packet.iter_mut().enumerate() {
            *byte = ((i * 73 + 19) & 0xFF) as u8;
        }
        let mut dec = Decoder::new(&packet).expect("valid range decoder");

        let mut old_band_e = [[0.0_f32; MAX_BANDS]; MAX_CHANNELS];
        for (i, val) in old_band_e[0].iter_mut().enumerate() {
            *val = ((i % 7) as f32) * 0.5 - 1.5;
        }

        unquant_coarse_energy(&mut dec, 0, 21, false, 3, 1, &mut old_band_e)
            .expect("coarse energy decode failed");
        assert_eq!(dec.tell(), 39);
        assert_eq!(dec.tell_frac(), 305);

        const CASE_A_COARSE_CH0: [u32; 21] = [
            0xbf400000, 0xbfc00000, 0xbf866600, 0x3e4cd000, 0x3fa00000, 0x40133300, 0x3faccc00,
            0xbf733400, 0x3f000000, 0x3f0ccc00, 0x3f4ccc00, 0x3f866600, 0x3fa66600, 0x3fc66600,
            0x3f866600, 0x3dccc000, 0x3fc66600, 0x3fcccc00, 0x40366600, 0x40399900, 0x40099900,
        ];
        for (band, &expected_bits) in CASE_A_COARSE_CH0.iter().enumerate() {
            assert_eq!(
                old_band_e[0][band].to_bits(),
                expected_bits,
                "coarse mismatch at band {}",
                band
            );
        }

        let fine_quant: [i32; MAX_BANDS] = [
            3, 3, 2, 2, 2, 1, 1, 1, 2, 2, 1, 1, 1, 0, 0, 0, 1, 1, 0, 0, 0,
        ];
        let fine_priority: [i32; MAX_BANDS] = [
            0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0,
        ];

        unquant_fine_energy(&mut dec, 0, 21, 1, &fine_quant, &mut old_band_e)
            .expect("fine energy decode failed");
        assert_eq!(dec.tell(), 63);
        assert_eq!(dec.tell_frac(), 497);

        const CASE_A_FINE_CH0: [u32; 21] = [
            0xbf700000, 0xbfa80000, 0xbf2ccc00, 0x3d99a000, 0x3f600000, 0x40033300, 0x3fcccc00,
            0xbf999a00, 0x3ec00000, 0x3e333000, 0x3f866600, 0x3fa66600, 0x3f866600, 0x3fc66600,
            0x3f866600, 0x3dccc000, 0x3fe66600, 0x3faccc00, 0x40366600, 0x40399900, 0x40099900,
        ];
        for (band, &expected_bits) in CASE_A_FINE_CH0.iter().enumerate() {
            assert_eq!(
                old_band_e[0][band].to_bits(),
                expected_bits,
                "fine mismatch at band {}",
                band
            );
        }

        unquant_energy_finalise(
            &mut dec,
            0,
            21,
            1,
            &fine_quant,
            &fine_priority,
            10,
            &mut old_band_e,
        )
        .expect("finalise energy decode failed");
        assert_eq!(dec.tell(), 73);
        assert_eq!(dec.tell_frac(), 577);

        const CASE_A_FINAL_CH0: [u32; 21] = [
            0xbf680000, 0xbfa80000, 0xbf1ccc00, 0x3d99a000, 0x3f700000, 0x40033300, 0x3fdccc00,
            0xbf999a00, 0x3ea00000, 0x3e333000, 0x3f6ccc00, 0x3fa66600, 0x3f6ccc00, 0x3fc66600,
            0x3f4ccc00, 0x3dccc000, 0x3fd66600, 0x3faccc00, 0x40466600, 0x40399900, 0x40099900,
        ];
        for (band, &expected_bits) in CASE_A_FINAL_CH0.iter().enumerate() {
            assert_eq!(
                old_band_e[0][band].to_bits(),
                expected_bits,
                "finalise mismatch at band {}",
                band
            );
        }

        return;
    }

    /// Verifies Stereo, Intra (`intra = true`), `LM = 1` (5 ms), `start = 0, end = 21`
    /// with a tight 12-byte budget that exercises Laplace, `SMALL_ENERGY_ICDF`, 1-bit logp,
    /// and `-1` budget exhaustion branches against the `libopus 1.6.1` C oracle.
    #[test]
    fn test_energy_stereo_intra_tight_budget_oracle() {
        let mut packet = [0u8; 12];
        for (i, byte) in packet.iter_mut().enumerate() {
            *byte = ((i * 131 + 42) & 0xFF) as u8;
        }
        let mut dec = Decoder::new(&packet).expect("valid range decoder");

        let mut old_band_e = [[0.0_f32; MAX_BANDS]; MAX_CHANNELS];
        for (c, ch_row) in old_band_e.iter_mut().enumerate() {
            for (b, slot) in ch_row.iter_mut().enumerate() {
                let idx = c * MAX_BANDS + b;
                *slot = -12.0 + (idx as f32) * 0.25;
            }
        }

        unquant_coarse_energy(&mut dec, 0, 21, true, 1, 2, &mut old_band_e)
            .expect("stereo intra coarse energy decode failed");
        assert_eq!(dec.tell(), 96);
        assert_eq!(dec.tell_frac(), 761);

        const CASE_B_COARSE_CH0: [u32; 21] = [
            0xbf800000, 0x40499980, 0x40233380, 0x409199c0, 0x3e800400, 0x40db3340, 0x40de66c0,
            0x410ccd00, 0x41080040, 0x41080040, 0x41080040, 0x41080040, 0x41080040, 0x40f00080,
            0x40f4cd40, 0x410a66a0, 0x41080040, 0x40f00080, 0x40d4cd40, 0x40b99a00, 0x409e66c0,
        ];
        const CASE_B_COARSE_CH1: [u32; 21] = [
            0x40000000, 0x3f333400, 0xbe199800, 0x00000000, 0x40000000, 0x3fd99a00, 0xbfa66600,
            0xbf599a00, 0xbf599a00, 0x3f933300, 0x3f599a00, 0x3feccd00, 0x3fd99a00, 0xbfa66600,
            0xbf599a00, 0xbfeccd00, 0xc02ccd00, 0xc0633380, 0xc08ccd00, 0xc0a80040, 0xc0c33380,
        ];

        for (band, &expected_bits) in CASE_B_COARSE_CH0.iter().enumerate() {
            assert_eq!(
                old_band_e[0][band].to_bits(),
                expected_bits,
                "ch0 mismatch at band {}",
                band
            );
        }
        for (band, &expected_bits) in CASE_B_COARSE_CH1.iter().enumerate() {
            assert_eq!(
                old_band_e[1][band].to_bits(),
                expected_bits,
                "ch1 mismatch at band {}",
                band
            );
        }

        return;
    }

    /// Verifies Hybrid mode (`start = 17, end = 21`), Stereo, Inter (`intra = false`), `LM = 2`
    /// coarse, fine, and final energy decoding against the `libopus 1.6.1` C oracle.
    #[test]
    fn test_energy_hybrid_stereo_inter_lm2_oracle() {
        let mut packet = [0u8; 16];
        for (i, byte) in packet.iter_mut().enumerate() {
            *byte = ((i * 59 + 201) & 0xFF) as u8;
        }
        let mut dec = Decoder::new(&packet).expect("valid range decoder");

        let mut old_band_e = [[0.0_f32; MAX_BANDS]; MAX_CHANNELS];
        for (c, ch_row) in old_band_e.iter_mut().enumerate() {
            for (b, slot) in ch_row.iter_mut().enumerate() {
                let idx = (c * MAX_BANDS + b) as isize;
                *slot = 0.25 * ((idx - 10) as f32);
            }
        }

        unquant_coarse_energy(&mut dec, 17, 21, false, 2, 2, &mut old_band_e)
            .expect("hybrid coarse energy decode failed");

        let mut fine_quant = [0i32; MAX_BANDS];
        let mut fine_priority = [0i32; MAX_BANDS];
        fine_quant[17] = 4;
        fine_quant[18] = 3;
        fine_quant[19] = 2;
        fine_quant[20] = 1;
        fine_priority[17] = 0;
        fine_priority[18] = 1;
        fine_priority[19] = 0;
        fine_priority[20] = 1;

        unquant_fine_energy(&mut dec, 17, 21, 2, &fine_quant, &mut old_band_e)
            .expect("hybrid fine energy decode failed");
        unquant_energy_finalise(
            &mut dec,
            17,
            21,
            2,
            &fine_quant,
            &fine_priority,
            6,
            &mut old_band_e,
        )
        .expect("hybrid finalise energy decode failed");

        assert_eq!(dec.tell(), 37);
        assert_eq!(dec.tell_frac(), 291);

        const CASE_C_FINAL_CH0: [u32; 21] = [
            0xc0200000, 0xc0100000, 0xc0000000, 0xbfe00000, 0xbfc00000, 0xbfa00000, 0xbf800000,
            0xbf400000, 0xbf000000, 0xbe800000, 0x00000000, 0x3e800000, 0x3f000000, 0x3f400000,
            0x3f800000, 0x3fa00000, 0x3fc00000, 0x3f068000, 0x3f22b800, 0x3f043800, 0x3f3db800,
        ];
        const CASE_C_FINAL_CH1: [u32; 21] = [
            0x40300000, 0x40400000, 0x40500000, 0x40600000, 0x40700000, 0x40800000, 0x40880000,
            0x40900000, 0x40980000, 0x40a00000, 0x40a80000, 0x40b00000, 0x40b80000, 0x40c00000,
            0x40c80000, 0x40d00000, 0x40d80000, 0x4087c000, 0x404ee000, 0x407aee00, 0x4094a700,
        ];

        for (band, &expected_bits) in CASE_C_FINAL_CH0.iter().enumerate() {
            assert_eq!(
                old_band_e[0][band].to_bits(),
                expected_bits,
                "hybrid ch0 mismatch at band {}",
                band
            );
        }
        for (band, &expected_bits) in CASE_C_FINAL_CH1.iter().enumerate() {
            assert_eq!(
                old_band_e[1][band].to_bits(),
                expected_bits,
                "hybrid ch1 mismatch at band {}",
                band
            );
        }

        return;
    }
}
