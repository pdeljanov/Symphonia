//! CELT band energy decoding in three refinements: coarse, fine and final
//! ([RFC 6716 Section 4.3.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2>)).
//!
//! Every band's log2 energy, in 6 dB units relative to the band mean
//! ([`E_MEANS`](crate::celt::constant::E_MEANS)), is coded per channel. The coarse stage codes an
//! integer residual against a prediction from the previous frame and from the bands already
//! decoded in this frame. Once the bit allocation is known, the fine stage adds `fine_quant[i]`
//! raw bits per channel. After the band shapes, the final stage spends the bits still left on one
//! more bit per channel for some bands, in two priority passes. [`BandEnergyHistory`] keeps the
//! energies of recent frames, which the time prediction, anti-collapse and concealment read.
//!
//! ```text
//!   coarse  E[c][i] = alpha * max(E_prev[c][i], -9) + P[c] + q        P[c] += (1 - beta) * q
//!           q: Laplace if >= 15 bits remain, {0, -1, +1} if >= 2, {0, -1} if >= 1, else -1
//!   fine    E[c][i] += (f + 1/2) / 2^B[i] - 1/2       f: B[i] = fine_quant[i] raw bits
//!   final   E[c][i] += (f - 1/2) / 2^(B[i] + 1)       f: one raw bit, priority false then true
//! ```

use std::ops::Range;

use crate::celt::BlockSize;
use crate::celt::constant::{
    BETA_COEF, BETA_INTRA, CDF_SMALL_ENERGY, E_PROB_MODEL, LaplaceModel, MAX_BANDS, MAX_CHANNELS,
    MAX_FINE_BITS, MAX_LM, PRED_COEF,
};
use crate::entropy::Decoder;
use symphonia_core::errors::Result;

/// Floor of the previous-frame energy that enters the time prediction (`-9`, that is -54 dB),
/// which keeps the predictor bounded
/// ([RFC 6716 Section 4.3.2.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.1>)).
const MIN_COARSE_PREDICTION_ENERGY: f32 = -9.0;

/// Bits that must remain for a coarse residual to use the band's Laplace model (`15`).
const LAPLACE_MIN_BITS: i32 = 15;

/// Bits that must remain for a coarse residual from [`CDF_SMALL_ENERGY`] (`2`).
const SMALL_ENERGY_MIN_BITS: i32 = 2;

/// Bits that must remain for a one-bit coarse residual (`1`).
const ONE_BIT_MIN_BITS: i32 = 1;

/// Coarse residuals of the [`CDF_SMALL_ENERGY`] symbols `0`, `1` and `2`.
const SMALL_ENERGY_RESIDUALS: [i32; 3] = [0, -1, 1];

/// `-log2` probability of the `-1` value of a one-bit coarse residual (`1`).
const ONE_BIT_RESIDUAL_LOGP: u32 = 1;

/// Coarse residual of a band reached after the bits ran out: one 6 dB step down (`-1`).
const EXHAUSTED_RESIDUAL: i32 = -1;

/// Initial and minimum log2 band energy of the history (`-28`, that is -168 dB)
/// ([RFC 6716 Section 4.3.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2>)).
pub(crate) const INITIAL_LOG_ENERGY: f32 = -28.0;

/// Largest frame-plus-loss time, in 2.5 ms units, credited to one rise of the background noise
/// floor (`160`, that is 400 ms).
const MAX_BACKGROUND_INCREASE_UNITS: usize = 160;

/// Rise of the background noise floor per 2.5 ms unit (`0.001`): at most 2.4 dB per second.
const BACKGROUND_INCREASE_PER_UNIT: f32 = 0.001;

/// Most lost frames over which a falling band energy is extrapolated after a loss (`10`).
const MAX_EXTRAPOLATED_LOST_FRAMES: usize = 10;

/// Margin subtracted from every band energy after a loss, per block size exponent: short frames
/// fluctuate most (`1.5` for 2.5 ms, `0.5` for 5 ms, none from 10 ms).
const LOSS_SAFETY_MARGIN: [f32; MAX_LM + 1] = [1.5, 0.5, 0.0, 0.0];

/// Weight of the two-frame energy drop in the falling-trend slope (`1/2`, per frame).
const TWO_FRAME_SLOPE_WEIGHT: f32 = 0.5;

/// Steepest per-frame drop a falling trend is extrapolated with (`2`, that is 12 dB).
const MAX_TREND_SLOPE: f32 = 2.0;

/// Lowest energy a falling trend is extrapolated to (`-20`, that is -120 dB).
const MIN_TREND_ENERGY: f32 = -20.0;

/// Band log2 energies of the recent frames per channel, which drive inter-frame coarse energy
/// prediction, anti-collapse and concealment
/// ([RFC 6716 Section 4.3.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2>)).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BandEnergyHistory {
    /// Energies of the frame being decoded; after it, of the most recent frame.
    pub(crate) current: [[f32; MAX_BANDS]; MAX_CHANNELS],
    /// Energies one frame back, the anti-collapse reference.
    pub(crate) previous: [[f32; MAX_BANDS]; MAX_CHANNELS],
    /// Energies two frames back, the second anti-collapse reference.
    pub(crate) two_back: [[f32; MAX_BANDS]; MAX_CHANNELS],
    /// Slowly rising background noise floor that bounds the decay of noise concealment.
    pub(crate) background: [[f32; MAX_BANDS]; MAX_CHANNELS],
}

impl Default for BandEnergyHistory {
    fn default() -> Self {
        return Self {
            current: [[0.0; MAX_BANDS]; MAX_CHANNELS],
            previous: [[INITIAL_LOG_ENERGY; MAX_BANDS]; MAX_CHANNELS],
            two_back: [[INITIAL_LOG_ENERGY; MAX_BANDS]; MAX_CHANNELS],
            background: [[0.0; MAX_BANDS]; MAX_CHANNELS],
        };
    }
}

impl BandEnergyHistory {
    /// Returns the history to its initial state
    /// ([RFC 6716 Section 4.3.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2>)).
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
        return;
    }

    /// Keeps the louder of both channel histories in channel 0, which a mono frame predicts from
    /// ([RFC 6716 Section 4.3.2.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.1>)).
    pub(crate) fn merge_mono(&mut self) {
        let [left, right] = &mut self.current;
        left.iter_mut().zip(right.iter()).for_each(|(l, &r)| *l = l.max(r));
        return;
    }

    /// Sets the energies of the first `channels` channels of a silence frame to the floor
    /// ([RFC 6716 Section 4.3.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2>)).
    pub(crate) fn silence(&mut self, channels: usize) {
        self.current[..channels].iter_mut().for_each(|bands| bands.fill(INITIAL_LOG_ENERGY));
        return;
    }

    /// Lowers the energies that the time prediction of the first inter frame after a loss of
    /// `loss_duration` 2.5 ms units starts from, so that the frame cannot return louder than its
    /// trend: a falling band keeps falling for every lost frame, any other band restarts from the
    /// lowest of its last three energies, and frames shorter than 10 ms drop a further margin.
    /// This is a concealment rule, which RFC 6716 leaves to the implementation
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    pub(crate) fn apply_loss_recovery_safety(
        &mut self,
        bands: Range<usize>,
        lm: BlockSize,
        loss_duration: usize,
    ) {
        let lost_frames = (loss_duration >> lm.log2()).min(MAX_EXTRAPOLATED_LOST_FRAMES) as f32;
        let margin = LOSS_SAFETY_MARGIN[lm.log2()];
        let older = self.previous.iter().zip(&self.two_back);
        for (current, (previous, two_back)) in self.current.iter_mut().zip(older) {
            let history = previous[bands.clone()].iter().zip(&two_back[bands.clone()]);
            current[bands.clone()].iter_mut().zip(history).for_each(|(e0, (&e1, &e2))| {
                *e0 = recovered_energy(*e0, e1, e2, lost_frames) - margin;
            });
        }
        return;
    }

    /// Lowers the energies of the first `channels` channels by `decay` in `bands` for a
    /// noise-concealed frame, never below the background floor
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    pub(crate) fn decay_for_concealment(
        &mut self,
        channels: usize,
        bands: Range<usize>,
        decay: f32,
    ) {
        let rows = self.current.iter_mut().zip(&self.background).take(channels);
        for (current, background) in rows {
            let floors = &background[bands.clone()];
            current[bands.clone()].iter_mut().zip(floors).for_each(|(e, &floor)| {
                *e = floor.max(*e - decay);
            });
        }
        return;
    }

    /// Shifts the history after a decoded frame of block size `lm` with `channels` coded
    /// channels: a mono frame copies its energies to channel 1, a transient frame only lowers
    /// `previous`, and bands outside `bands` return to their initial values
    /// ([RFC 6716 Section 4.3.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2>),
    /// [Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)). The
    /// background floor follows the energies down at once but rises by at most
    /// [`BACKGROUND_INCREASE_PER_UNIT`] per 2.5 ms of frame and preceding loss, a concealment rule
    /// ([Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    pub(crate) fn commit_frame(
        &mut self,
        channels: usize,
        bands: Range<usize>,
        lm: BlockSize,
        loss_duration: usize,
        is_transient: bool,
    ) {
        if channels == 1 {
            self.current[1] = self.current[0];
        }
        if is_transient {
            let previous = self.previous.as_flattened_mut().iter_mut();
            previous.zip(self.current.as_flattened()).for_each(|(e, &now)| *e = e.min(now));
        } else {
            self.two_back = self.previous;
            self.previous = self.current;
        }

        let elapsed_units = (loss_duration + lm.blocks()).min(MAX_BACKGROUND_INCREASE_UNITS);
        let max_increase = (elapsed_units as f32) * BACKGROUND_INCREASE_PER_UNIT;
        let floors = self.background.as_flattened_mut().iter_mut();
        floors.zip(self.current.as_flattened()).for_each(|(floor, &now)| {
            *floor = (*floor + max_increase).min(now);
        });

        self.current.iter_mut().for_each(|row| fill_outside(row, &bands, 0.0));
        let older = self.previous.iter_mut().chain(self.two_back.iter_mut());
        older.for_each(|row| fill_outside(row, &bands, INITIAL_LOG_ENERGY));
        return;
    }
}

/// Energy a band restarts from after `lost_frames` lost frames, from its current energy `e0` and
/// its energies `e1` and `e2` one and two frames back: a falling trend is extended, otherwise the
/// lowest of the three is kept.
fn recovered_energy(e0: f32, e1: f32, e2: f32, lost_frames: f32) -> f32 {
    if e0 < e1.max(e2) {
        let slope = (e1 - e0).max(TWO_FRAME_SLOPE_WEIGHT * (e2 - e0)).min(MAX_TREND_SLOPE);
        return (e0 - ((1.0 + lost_frames) * slope).max(0.0)).max(MIN_TREND_ENERGY);
    }
    return e0.min(e1).min(e2);
}

/// Sets every band of `row` outside `bands` to `value`.
fn fill_outside(row: &mut [f32; MAX_BANDS], bands: &Range<usize>, value: f32) {
    row[..bands.start].fill(value);
    row[bands.end..].fill(value);
    return;
}

/// Decodes the coarse energies of `bands` for every channel row of `band_e`, in place
/// ([RFC 6716 Section 4.3.2.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.1>)).
/// Each residual `q` is added to a prediction from the band's energy in the previous frame and
/// from the residuals of the lower bands of this frame:
///
/// ```text
///   E[c][i] = alpha * max(E[c][i], -9) + P[c] + q        P[c] += (1 - beta) * q
///   intra frame:  alpha = 0,              beta = 4915/32768
///   inter frame:  alpha = PRED_COEF[lm],  beta = BETA_COEF[lm]
/// ```
///
/// A mono frame predicts from the louder channel of the history, which
/// [`BandEnergyHistory::merge_mono`] prepares.
pub(crate) fn unquant_coarse_energy(
    range_decoder: &mut Decoder<'_>,
    bands: Range<usize>,
    intra: bool,
    lm: BlockSize,
    band_e: &mut [[f32; MAX_BANDS]],
) -> Result<()> {
    let (alpha, beta) =
        if intra { (0.0, BETA_INTRA) } else { (PRED_COEF[lm.log2()], BETA_COEF[lm.log2()]) };
    let models = &E_PROB_MODEL[lm.log2()][usize::from(intra)];
    let budget = range_decoder.storage_bits();
    let mut prediction = [0.0_f32; MAX_CHANNELS];

    for band in bands {
        for (energies, p) in band_e.iter_mut().zip(prediction.iter_mut()) {
            let q = decode_coarse_residual(range_decoder, budget, models[band])? as f32;
            energies[band] = alpha * energies[band].max(MIN_COARSE_PREDICTION_ENERGY) + *p + q;
            *p = *p + q - beta * q;
        }
    }
    return Ok(());
}

/// Decodes one coarse energy residual with the richest code that the bits left in a frame of
/// `budget` bits allow: the band's Laplace `model`, then [`CDF_SMALL_ENERGY`] for `{0, -1, +1}`,
/// then one bit for `{0, -1}`, and `-1` once no bit is left
/// ([RFC 6716 Section 4.3.2.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.1>)).
fn decode_coarse_residual(
    range_decoder: &mut Decoder<'_>,
    budget: i32,
    model: LaplaceModel,
) -> Result<i32> {
    return match budget - range_decoder.tell() {
        left if left >= LAPLACE_MIN_BITS => {
            Ok(range_decoder.decode_laplace(model.zero_freq_q15(), model.decay_q14()))
        }
        left if left >= SMALL_ENERGY_MIN_BITS => {
            let symbol = range_decoder.decode_symbol(CDF_SMALL_ENERGY)?;
            Ok(SMALL_ENERGY_RESIDUALS[symbol as usize])
        }
        left if left >= ONE_BIT_MIN_BITS => {
            Ok(-(range_decoder.decode_symbol_logp(ONE_BIT_RESIDUAL_LOGP)? as i32))
        }
        _ => Ok(EXHAUSTED_RESIDUAL),
    };
}

/// Width `2^-bits` of one quantization step refined by `bits` bits, in 6 dB units; exact in
/// `f32` for every bit count up to [`MAX_FINE_BITS`] plus one.
fn step_size(bits: i32) -> f32 {
    return 1.0 / ((1_i32 << bits) as f32);
}

/// Adds the fine energy refinement of `bands` to every channel row of `band_e`
/// ([RFC 6716 Section 4.3.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.2>)):
/// a band with `B = fine_quant[i] > 0` reads a `B`-bit integer `f` per channel, which moves its
/// energy by `(f + 1/2) / 2^B - 1/2`.
pub(crate) fn unquant_fine_energy(
    range_decoder: &mut Decoder<'_>,
    bands: Range<usize>,
    fine_quant: &[i32; MAX_BANDS],
    band_e: &mut [[f32; MAX_BANDS]],
) -> Result<()> {
    for band in bands.filter(|&i| fine_quant[i] > 0) {
        let fine_bits = fine_quant[band];
        let step = step_size(fine_bits);
        for energies in band_e.iter_mut() {
            let f = range_decoder.decode_raw_bits(fine_bits as u32)? as f32;
            energies[band] += (f + 0.5) * step - 0.5;
        }
    }
    return Ok(());
}

/// Spends the `bits_left` bits that remain after the band shapes on one more energy bit per
/// channel ([RFC 6716 Section 4.3.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.2>)):
/// first in the bands of `bands` whose `fine_priority` is `false`, then in those whose priority is
/// `true`, each pass from the lowest band up. Bands that already hold [`MAX_FINE_BITS`] are
/// skipped, and the passes stop once fewer bits than channels remain. A bit `f` moves the energy
/// of a band with `B = fine_quant[i]` by `(f - 1/2) / 2^(B + 1)`.
pub(crate) fn unquant_energy_finalise(
    range_decoder: &mut Decoder<'_>,
    bands: Range<usize>,
    fine_quant: &[i32; MAX_BANDS],
    fine_priority: &[bool; MAX_BANDS],
    bits_left: i32,
    band_e: &mut [[f32; MAX_BANDS]],
) -> Result<()> {
    let channels = band_e.len() as i32;
    let mut bits_left = bits_left;
    for priority in [false, true] {
        let pass = bands.clone().filter(|&i| {
            return fine_priority[i] == priority && fine_quant[i] < MAX_FINE_BITS;
        });
        for band in pass {
            if bits_left < channels {
                return Ok(());
            }
            let step = step_size(fine_quant[band] + 1);
            for energies in band_e.iter_mut() {
                let f = range_decoder.decode_raw_bits(1)? as f32;
                energies[band] += (f - 0.5) * step;
            }
            bits_left -= channels;
        }
    }
    return Ok(());
}

#[cfg(test)]
mod tests {
    // Fixtures: packets and expected values recorded from libopus 1.6.1. The behaviour they pin is
    // normative (RFC 6716 Section 4.3.2) and is verified against the RFC 8251 test vectors by
    // `tests/rfc6716_conformance.rs`.
    use super::*;

    #[test]
    fn band_energy_history_reset_merge_and_silence() {
        let mut history = BandEnergyHistory::default();
        history.current[0][3] = -4.0;
        history.current[1][3] = 2.5;
        history.merge_mono();
        assert_eq!(history.current[0][3], 2.5);

        history.silence(1);
        assert!(history.current[0].iter().all(|&e| e == INITIAL_LOG_ENERGY));
        assert_eq!(history.current[1][3], 2.5);

        history.reset();
        assert_eq!(history, BandEnergyHistory::default());
        return;
    }

    #[test]
    fn band_energy_history_commit_frame_rule() {
        let mut history = BandEnergyHistory::default();
        history.current[0] = [1.0; MAX_BANDS];
        history.previous[0] = [3.0; MAX_BANDS];

        // Mono, non-transient: channel 1 mirrors channel 0 and the history shifts by one frame.
        history.commit_frame(1, 0..17, BlockSize::new(3), 0, false);
        assert_eq!(history.current[1][..17], [1.0; 17]);
        assert_eq!(history.two_back[0][..17], [3.0; 17]);
        assert_eq!(history.previous[0][..17], [1.0; 17]);
        assert_eq!(history.background[0][0], 0.008);
        // Bands at and above `end` return to their initial values.
        assert_eq!((history.current[0][17], history.previous[0][17]), (0.0, INITIAL_LOG_ENERGY));
        assert_eq!(history.two_back[1][20], INITIAL_LOG_ENERGY);

        // Transient: `previous` only takes the minimum and `two_back` stays.
        history.current[0][0] = -2.0;
        history.commit_frame(2, 0..17, BlockSize::new(3), 0, true);
        assert_eq!(history.previous[0][0], -2.0);
        assert_eq!(history.two_back[0][0], 3.0);
        return;
    }

    /// Verifies Mono, Inter (`intra = false`), `LM = 3` (20 ms), `start = 0, end = 21`
    /// coarse, fine, and final energy decoding bit-for-bit against the recorded values.
    #[test]
    fn energy_mono_inter_lm3_matches_recorded_values() {
        let mut packet = [0u8; 32];
        for (i, byte) in packet.iter_mut().enumerate() {
            *byte = ((i * 73 + 19) & 0xFF) as u8;
        }
        let mut dec = Decoder::new(&packet);

        let mut old_band_e = [[0.0_f32; MAX_BANDS]; MAX_CHANNELS];
        for (i, val) in old_band_e[0].iter_mut().enumerate() {
            *val = ((i % 7) as f32) * 0.5 - 1.5;
        }

        unquant_coarse_energy(&mut dec, 0..21, false, BlockSize::new(3), &mut old_band_e[..1])
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

        let fine_quant: [i32; MAX_BANDS] =
            [3, 3, 2, 2, 2, 1, 1, 1, 2, 2, 1, 1, 1, 0, 0, 0, 1, 1, 0, 0, 0];
        let fine_priority: [bool; MAX_BANDS] =
            [0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0].map(|p| p != 0);

        unquant_fine_energy(&mut dec, 0..21, &fine_quant, &mut old_band_e[..1])
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
            0..21,
            &fine_quant,
            &fine_priority,
            10,
            &mut old_band_e[..1],
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
    /// with a tight 12-byte budget that exercises Laplace, `CDF_SMALL_ENERGY`, 1-bit logp,
    /// and `-1` budget exhaustion branches against the recorded values.
    #[test]
    fn energy_stereo_intra_tight_budget_matches_recorded_values() {
        let mut packet = [0u8; 12];
        for (i, byte) in packet.iter_mut().enumerate() {
            *byte = ((i * 131 + 42) & 0xFF) as u8;
        }
        let mut dec = Decoder::new(&packet);

        let mut old_band_e = [[0.0_f32; MAX_BANDS]; MAX_CHANNELS];
        for (c, ch_row) in old_band_e.iter_mut().enumerate() {
            for (b, slot) in ch_row.iter_mut().enumerate() {
                let i = c * MAX_BANDS + b;
                *slot = -12.0 + (i as f32) * 0.25;
            }
        }

        unquant_coarse_energy(&mut dec, 0..21, true, BlockSize::new(1), &mut old_band_e)
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
    /// coarse, fine, and final energy decoding against the recorded values.
    #[test]
    fn energy_hybrid_stereo_inter_lm2_matches_recorded_values() {
        let mut packet = [0u8; 16];
        for (i, byte) in packet.iter_mut().enumerate() {
            *byte = ((i * 59 + 201) & 0xFF) as u8;
        }
        let mut dec = Decoder::new(&packet);

        let mut old_band_e = [[0.0_f32; MAX_BANDS]; MAX_CHANNELS];
        for (c, ch_row) in old_band_e.iter_mut().enumerate() {
            for (b, slot) in ch_row.iter_mut().enumerate() {
                let i = (c * MAX_BANDS + b) as isize;
                *slot = 0.25 * ((i - 10) as f32);
            }
        }

        unquant_coarse_energy(&mut dec, 17..21, false, BlockSize::new(2), &mut old_band_e)
            .expect("hybrid coarse energy decode failed");

        let mut fine_quant = [0i32; MAX_BANDS];
        let mut fine_priority = [false; MAX_BANDS];
        fine_quant[17] = 4;
        fine_quant[18] = 3;
        fine_quant[19] = 2;
        fine_quant[20] = 1;
        fine_priority[17] = false;
        fine_priority[18] = true;
        fine_priority[19] = false;
        fine_priority[20] = true;

        unquant_fine_energy(&mut dec, 17..21, &fine_quant, &mut old_band_e)
            .expect("hybrid fine energy decode failed");
        unquant_energy_finalise(&mut dec, 17..21, &fine_quant, &fine_priority, 6, &mut old_band_e)
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

    /// Fine energy reads `fine_quant[i]` raw bits per channel for every band with
    /// `fine_quant[i] > 0` and never checks the frame budget, so `tell()` advances by every bit
    /// the allocation granted
    /// ([RFC 6716 Section 4.3.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.2>),
    /// Appendix A `unquant_fine_energy`). Raw bits come from the end of the frame backwards.
    #[test]
    fn fine_energy_reads_every_band_without_budget_check() {
        let packet = [0x5A_u8, 0xC3];
        let mut dec = Decoder::new(&packet);
        let mut fine_quant = [0_i32; MAX_BANDS];
        fine_quant[..2].fill(8);
        let mut old_band_e = [[0.0_f32; MAX_BANDS]; MAX_CHANNELS];

        unquant_fine_energy(&mut dec, 0..2, &fine_quant, &mut old_band_e[..1]).unwrap();

        // Band 0 takes the last byte, band 1 the first: offset = (f + 1/2) / 2^8 - 1/2.
        assert_eq!(old_band_e[0][..2], [(195.5 - 128.0) / 256.0, (90.5 - 128.0) / 256.0]);
        assert_eq!(dec.tell(), 17);
        return;
    }
}
