//! CELT band sweep: decodes every coded band of a frame through the shape decoders of
//! [`super::vq`] and [`super::stereo`], keeps the folding history of the normalised bands, and
//! refills the short blocks that collapsed
//! ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>),
//! [Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)).
//!
//! ```text
//!   band i                 unquantize_bands            folding history (norm)
//!   +----------+   bits    +---------------------+      +-------------------+
//!   | pulses   |---------->| quant_band          |<-----| bands below i     |
//!   | tf_res   |           |  time-frequency     |      +-------------------+
//!   | spread   |           |  quant_partition    |----->| band i (scaled)   |
//!   +----------+           |   split angle       |      +-------------------+
//!        ^                 |   pvq_unquantize    |
//!        |                 |   fold / noise      |      collapse masks
//!   range decoder -------->| quant_band_stereo   |----->| x | y | per band  |
//!                          +---------------------+      +-------------------+
//! ```

use crate::celt::constant::{BITRES, EBANDS, MAX_BANDS, Spread, band_width};
use crate::celt::energy::BandEnergyHistory;
use crate::celt::rate::{BandAllocation, CacheLevel};
use crate::celt::stereo::quant_band_stereo;
use crate::celt::vq::{
    BALANCE_SHARE_BANDS, BandDecoderCtx, COLLAPSE_DEPTH_DECAY, LCG_SIGN_BIT, Lowband,
    MAX_BAND_BITS_Q3, MAX_BAND_WIDTH, MAX_NORM_BUFFER_SIZE, Partition, exp2, quant_band,
    renormalise_vector,
};
use crate::celt::{BlockSize, MAX_CHANNELS, MAX_LM, lcg_next};
use crate::entropy::Decoder;
use crate::planes::Planes;
use std::f32::consts::SQRT_2;
use symphonia_core::errors::Result;

/// Frame-wide inputs of the band shape decoding shared by every band
/// ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>),
/// [RFC 8251 Section 10](<https://datatracker.ietf.org/doc/html/rfc8251#section-10>)).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ShapeFrame {
    /// First coded Bark band.
    pub(crate) start: usize,
    /// One past the last coded Bark band.
    pub(crate) end: usize,
    /// MDCT block size `LM` of the frame.
    pub(crate) lm: BlockSize,
    /// Short MDCT blocks `B` the shapes are interleaved over: `1`, or `2^LM` in a transient frame.
    pub(crate) blocks: usize,
    /// Spreading rotation decision of the frame.
    pub(crate) spread: Spread,
    /// Time-frequency change per band.
    pub(crate) tf_res: [i32; MAX_BANDS],
    /// Frame budget in eighth bits available to the shapes.
    pub(crate) total_bits: i32,
    /// Whether the 180-degree phase shift of intensity stereo is applied to the side channel
    /// ([RFC 8251 Section 10](<https://datatracker.ietf.org/doc/html/rfc8251#section-10>)).
    pub(crate) phase_inversion: bool,
}

/// Normalised shapes of the bands decoded so far per channel: the folding source of the bands
/// above them ([RFC 6716 Section 4.3.4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.2>)).
struct FoldingHistory {
    norm: [[f32; MAX_NORM_BUFFER_SIZE]; MAX_CHANNELS],
}

impl FoldingHistory {
    /// Duplicates the tail of the first Hybrid band's history (`start = 17`, width `8M`) so the
    /// second Hybrid band (width `12M`) has a complete lowband source, for both channels when
    /// `dual_stereo` ([RFC 8251 Section 9](<https://datatracker.ietf.org/doc/html/rfc8251#section-9>)).
    fn fold_first_hybrid_band(&mut self, start: usize, m: usize, dual_stereo: bool) {
        if start + 2 > MAX_BANDS {
            return;
        }
        let n1 = m * (EBANDS[start + 1] - EBANDS[start]);
        let n2 = m * (EBANDS[start + 2] - EBANDS[start + 1]);
        if n2 <= n1 || 2 * n1 < n2 {
            return;
        }
        let src_start = 2 * n1 - n2;
        let copy_len = n2 - n1;
        Planes::from_rows(&mut self.norm, dual_stereo)
            .each_mut()
            .for_each(|plane| plane.copy_within(src_start..(src_start + copy_len), n1));
        return;
    }

    /// Averages the two channel histories over `len` bins when dual stereo ends at the intensity
    /// band ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
    fn merge_channels(&mut self, len: usize) {
        let [norm_x, norm_y] = &mut self.norm;
        norm_x[..len]
            .iter_mut()
            .zip(norm_y[..len].iter())
            .for_each(|(n1, &n2)| *n1 = 0.5_f32 * (*n1 + n2));
        return;
    }

    /// Copies the folding source of a band of `n` bins into `scratch` (so the band can write its
    /// own shape at `write` while folding) and returns per channel its source and output slot.
    fn band_lowbands<'b>(
        &'b mut self,
        scratch: &'b mut [[f32; MAX_BAND_WIDTH]; MAX_CHANNELS],
        fold: &FoldSource,
        n: usize,
        write: Option<usize>,
        dual_stereo: bool,
    ) -> [Lowband<'b>; 2] {
        if let Some(offset) = fold.lowband {
            scratch[0][..n].copy_from_slice(&self.norm[0][offset..(offset + n)]);
            if dual_stereo {
                scratch[1][..n].copy_from_slice(&self.norm[1][offset..(offset + n)]);
            }
        }
        let [norm_x, norm_y] = &mut self.norm;
        let [src_x, src_y] = &*scratch;
        return [
            Lowband {
                source: fold.lowband.map(|_| &src_x[..n]),
                out: write.map(|w| &mut norm_x[w..(w + n)]),
            },
            Lowband {
                source: fold.lowband.map(|_| &src_y[..n]),
                out: write.map(|w| &mut norm_y[w..(w + n)]),
            },
        ];
    }
}

/// Where a band folds its zero-pulse partitions from: the offset of the source in the folding
/// history, and per channel the collapse masks of the bands the source spans (the full mask
/// when the band is noise-filled instead)
/// ([RFC 6716 Section 4.3.4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.2>)).
struct FoldSource {
    /// Offset of the folding source in the folding history, `None` for noise filling.
    lowband: Option<usize>,
    /// Fill mask of the first channel.
    x_mask: u32,
    /// Fill mask of the second channel.
    y_mask: u32,
}

/// Bookkeeping carried from one band to the next while the shapes of a frame are decoded:
/// the recursive-decoding context, the bit balance, the stereo coding in force and which lower
/// band the folding reads from
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>),
/// [Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>)).
struct BandSweep<'a> {
    frame: &'a ShapeFrame,
    alloc: &'a BandAllocation,
    ctx: BandDecoderCtx,
    /// Eighth bits left over by the bands decoded so far, shared out to the bands that follow.
    balance: i32,
    /// Whether the two channels are still coded independently (cleared at the intensity band).
    dual_stereo: bool,
    /// Band whose normalised shape the folding reads from (`0` before any band qualifies).
    lowband_offset: usize,
    /// Whether the last band spent enough bits for its shape to serve as a folding source.
    update_lowband: bool,
}

impl<'a> BandSweep<'a> {
    fn new(frame: &'a ShapeFrame, alloc: &'a BandAllocation, seed: u32) -> Self {
        return Self {
            frame,
            alloc,
            ctx: BandDecoderCtx {
                band: frame.start,
                intensity_band: alloc.intensity,
                spread: frame.spread,
                tf_change: 0,
                remaining_bits: 0,
                seed,
            },
            balance: alloc.balance,
            dual_stereo: alloc.dual_stereo,
            lowband_offset: 0,
            update_lowband: true,
        };
    }

    /// Positions the context on band `i` and returns `(tell, bits)`: the range coder position in
    /// eighth bits and the band's shape budget
    /// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
    fn begin_band(&mut self, dec: &Decoder<'_>, i: usize) -> (i32, i32) {
        let tell = dec.tell_frac() as i32;
        if i != self.frame.start {
            self.balance -= tell;
        }
        let remaining_bits = self.frame.total_bits - tell - 1;
        self.ctx.band = i;
        self.ctx.remaining_bits = remaining_bits;
        self.ctx.tf_change = self.frame.tf_res[i];
        if i >= self.alloc.coded_bands {
            return (tell, 0);
        }
        let divisor = BALANCE_SHARE_BANDS.min((self.alloc.coded_bands - i) as i32);
        let curr_balance = self.balance / divisor;
        let bits = 0.max(
            MAX_BAND_BITS_Q3.min((remaining_bits + 1).min(self.alloc.pulses[i] + curr_balance)),
        );
        return (tell, bits);
    }

    /// Makes band `i` (`n` bins from `band_start`) the folding source of the bands above it when
    /// it lies a full band width above the history start (or is the second Hybrid band) and the
    /// previous source may be replaced.
    fn select_lowband(&mut self, i: usize, band_start: usize, norm_offset: usize, n: usize) {
        let reaches_down = band_start >= norm_offset + n || i == self.frame.start + 1;
        if reaches_down && (self.update_lowband || self.lowband_offset == 0) {
            self.lowband_offset = i;
        }
        return;
    }

    /// Locates the folding source of band `i` (`n` bins) and the collapse masks of the bands it
    /// spans, or the full fill mask when the band is noise-filled
    /// ([RFC 6716 Section 4.3.4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.2>)).
    fn folding_source(
        &self,
        i: usize,
        n: usize,
        collapse_masks: &[u8],
        channels: usize,
    ) -> FoldSource {
        let ShapeFrame { start, lm, blocks, spread, .. } = *self.frame;
        let folds = self.lowband_offset != 0
            && (spread != Spread::Aggressive || blocks > 1 || self.ctx.tf_change < 0);
        if !folds {
            let full_mask = (1u32 << blocks) - 1;
            return FoldSource { lowband: None, x_mask: full_mask, y_mask: full_mask };
        }

        let m = lm.blocks();
        let norm_offset = m * EBANDS[start];
        let offset = (m * EBANDS[self.lowband_offset]).saturating_sub(norm_offset + n);
        let fold_bin = offset + norm_offset;
        // First band whose bins the folding source reaches down into.
        let fold_start = (0..=self.lowband_offset)
            .rev()
            .find(|&j| j == 0 || m * EBANDS[j - 1] <= fold_bin)
            .unwrap_or(0)
            .saturating_sub(1);
        // One past the last band whose bins the folding source reaches up into.
        let fold_end = ((self.lowband_offset - 1)..i)
            .find(|&j| j + 1 >= i || m * EBANDS[j + 1] >= fold_bin + n)
            .unwrap_or(i - 1)
            + 1;
        let (x_mask, y_mask) = (fold_start..fold_end).fold((0u32, 0u32), |(x_mask, y_mask), j| {
            let x = u32::from(collapse_masks[j * channels]);
            let y = u32::from(collapse_masks[j * channels + channels - 1]);
            return (x_mask | x, y_mask | y);
        });
        return FoldSource { lowband: Some(offset), x_mask, y_mask };
    }

    /// Decodes the shapes of one band: both channels independently in dual stereo, mid/side for
    /// a coupled band, or the single mono channel; returns the collapse masks `(x, y)`
    /// ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>)).
    fn decode_band_shapes(
        &mut self,
        dec: &mut Decoder<'_>,
        band: Planes<'_>,
        lowbands: [Lowband<'_>; 2],
        (n, bits): (usize, i32),
        fold: &FoldSource,
    ) -> Result<(u32, u32)> {
        let Planes { left: x, right: y } = band;
        let [x_lowband, y_lowband] = lowbands;
        let part = Partition {
            n,
            blocks: self.frame.blocks,
            level: CacheLevel::from_block_size(self.frame.lm),
            bits,
            fill: fold.x_mask | fold.y_mask,
            gain: 1.0_f32,
        };
        let ctx = &mut self.ctx;
        // Dual stereo is only coded for two channels, so a mono band ignores the flag
        // ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
        return match (y, self.dual_stereo) {
            (Some(y), true) => {
                let x_part = Partition { bits: bits / 2, fill: fold.x_mask, ..part };
                let y_part = Partition { bits: bits / 2, fill: fold.y_mask, ..part };
                let x_cm = quant_band(ctx, dec, x, x_lowband, x_part)?;
                let y_cm = quant_band(ctx, dec, y, y_lowband, y_part)?;
                Ok((x_cm, y_cm))
            }
            (Some(y), false) => {
                let cm =
                    quant_band_stereo(ctx, dec, x, y, x_lowband, part, self.frame.phase_inversion)?;
                Ok((cm, cm))
            }
            (None, _) => {
                let cm = quant_band(ctx, dec, x, x_lowband, part)?;
                Ok((cm, cm))
            }
        };
    }

    /// Returns the band's unused budget to the balance and records whether its shape may serve
    /// as a folding source ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
    fn end_band(&mut self, i: usize, tell: i32, bits: i32, n: usize) {
        self.balance += self.alloc.pulses[i] + tell;
        self.update_lowband = bits > ((n as i32) << BITRES);
        return;
    }
}

/// Unquantizes the shapes of all bands `frame.start..frame.end` of a mono or stereo CELT frame
/// into `spectra`, recording the per-band, per-channel collapse masks and advancing the folding
/// noise `seed` ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>)).
pub(crate) fn unquantize_bands(
    frame: &ShapeFrame,
    alloc: &BandAllocation,
    dec: &mut Decoder<'_>,
    mut spectra: Planes<'_>,
    collapse_masks: &mut [u8],
    seed: &mut u32,
) -> Result<()> {
    let ShapeFrame { start, end, lm, .. } = *frame;
    let m = lm.blocks();
    let norm_offset = m * EBANDS[start];
    let channels = if spectra.is_stereo() { 2usize } else { 1usize };
    let mut history = FoldingHistory { norm: [[0.0_f32; MAX_NORM_BUFFER_SIZE]; MAX_CHANNELS] };
    let mut sweep = BandSweep::new(frame, alloc, *seed);

    for i in start..end {
        let band_start = m * EBANDS[i];
        let n = band_width(i, lm);
        let (tell, bits) = sweep.begin_band(dec, i);
        sweep.select_lowband(i, band_start, norm_offset, n);
        if i == start + 1 {
            history.fold_first_hybrid_band(start, m, sweep.dual_stereo);
        }
        let fold = sweep.folding_source(i, n, collapse_masks, channels);
        if sweep.dual_stereo && i == alloc.intensity {
            sweep.dual_stereo = false;
            history.merge_channels(band_start - norm_offset);
        }

        let mut scratch = [[0.0_f32; MAX_BAND_WIDTH]; MAX_CHANNELS];
        let write = (i != end - 1).then_some(band_start - norm_offset);
        let lowbands = history.band_lowbands(&mut scratch, &fold, n, write, sweep.dual_stereo);
        let band = spectra.range(band_start..(band_start + n));
        let (x_cm, y_cm) = sweep.decode_band_shapes(dec, band, lowbands, (n, bits), &fold)?;

        collapse_masks[i * channels] = x_cm as u8;
        collapse_masks[i * channels + channels - 1] = y_cm as u8;
        sweep.end_band(i, tell, bits, n);
    }

    *seed = sweep.ctx.seed;
    return Ok(());
}

/// Noise amplitude for the collapsed blocks of band `i` of channel `c`: the band's energy rise
/// over the two previous frames, capped by `thresh`, scaled to one block
/// ([RFC 6716 Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)).
fn collapse_noise_level(
    energy: &BandEnergyHistory,
    channels: usize,
    channel: usize,
    i: usize,
    lm: BlockSize,
    thresh: f32,
    sqrt_1: f32,
) -> f32 {
    let mut prev1 = energy.previous[channel][i];
    let mut prev2 = energy.two_back[channel][i];
    if channels == 1 {
        prev1 = prev1.max(energy.previous[1][i]);
        prev2 = prev2.max(energy.two_back[1][i]);
    }
    let ediff = (energy.current[channel][i] - prev1.min(prev2)).max(0.0_f32);

    let mut r = 2.0_f32 * exp2(-ediff);
    if lm.log2() == MAX_LM {
        r *= SQRT_2;
    }
    return thresh.min(r) * sqrt_1;
}

/// Prevents energy collapse across short MDCT sub-blocks during transient frames by injecting
/// pseudorandom noise into sub-blocks that received zero pulses and renormalizing the band
/// ([RFC 6716 Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)).
pub(crate) fn anti_collapse(
    x: &mut [f32],
    collapse_masks: &[u8],
    frame: &ShapeFrame,
    channels: usize,
    energy: &BandEnergyHistory,
    pulses: &[i32; MAX_BANDS],
    mut seed: u32,
) {
    let ShapeFrame { start, end, lm, .. } = *frame;
    let blocks = lm.blocks();
    let size = lm.frame_samples();

    for i in start..end {
        let n0 = EBANDS[i + 1] - EBANDS[i];
        let depth = ((1 + pulses[i]) / (n0 as i32)) >> lm.log2();
        let thresh = 0.5_f32 * exp2(-COLLAPSE_DEPTH_DECAY * (depth as f32));
        let sqrt_1 = 1.0_f32 / (lm.scale(n0) as f32).sqrt();

        for c in 0..channels {
            let r = collapse_noise_level(energy, channels, c, i, lm, thresh, sqrt_1);
            let band_offset = c * size + lm.scale(EBANDS[i]);
            let band_slice = &mut x[band_offset..(band_offset + lm.scale(n0))];
            let mask = collapse_masks[i * channels + c];

            let mut any_collapsed = false;
            for k in (0..blocks).filter(|&k| (mask & (1u8 << k)) == 0) {
                any_collapsed = true;
                for coeff in band_slice.iter_mut().skip(k).step_by(blocks) {
                    seed = lcg_next(seed);
                    *coeff = if (seed & LCG_SIGN_BIT) != 0 { r } else { -r };
                }
            }
            if any_collapsed {
                renormalise_vector(band_slice, lm.scale(n0), 1.0_f32);
            }
        }
    }
    return;
}

#[cfg(test)]
#[allow(clippy::excessive_precision)] // recorded values keep the 9 significant digits of the dump
mod tests {
    // Fixtures: packets and expected values recorded from libopus 1.6.1. The behaviour they pin is
    // normative (RFC 6716 Section 4.3.4) and is verified against the RFC 8251 test vectors by
    // `tests/rfc6716_conformance.rs`.
    use super::*;
    /// A [`BandAllocation`] with only the fields the shape decoding reads.
    fn allocation(
        pulses: [i32; MAX_BANDS],
        coded_bands: usize,
        intensity: usize,
        dual_stereo: bool,
    ) -> BandAllocation {
        return BandAllocation {
            pulses,
            fine_quant: [0; MAX_BANDS],
            fine_priority: [false; MAX_BANDS],
            balance: 0,
            coded_bands,
            intensity,
            dual_stereo,
        };
    }

    #[test]
    fn band_shapes_anti_collapse_and_denormalisation_match_recorded_values() {
        let qbuf: [u8; 32] = [
            0x31, 0x66, 0xd7, 0x3e, 0x26, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];
        let pulses: [i32; MAX_BANDS] =
            [40, 36, 32, 28, 24, 24, 20, 16, 32, 28, 20, 12, 8, 0, 0, 0, 0, 0, 0, 0, 0];
        let tf_res: [i32; MAX_BANDS] =
            [0, 0, 0, 0, 0, 0, 0, 0, 1, 0, -1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let frame = ShapeFrame {
            start: 0,
            end: 13,
            lm: BlockSize::new(1),
            blocks: 2,
            spread: Spread::Normal,
            tf_res,
            total_bits: 32 * 8 * 8,
            phase_inversion: true,
        };
        let alloc = allocation(pulses, 12, 10, false);

        let mut dec = Decoder::new(&qbuf);
        let mut x_dec = [0.0_f32; 240];
        let mut y_dec = [0.0_f32; 240];
        let mut cm_masks = [0u8; 42];
        let mut seed = 12345u32;

        unquantize_bands(
            &frame,
            &alloc,
            &mut dec,
            Planes::new(&mut x_dec, Some(&mut y_dec)),
            &mut cm_masks,
            &mut seed,
        )
        .expect("unquantize_bands succeeds");

        assert_eq!(seed, 3_194_601_925);
        let expected_cm: [u8; 26] =
            [3, 3, 3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 3, 3, 2, 2, 1, 1, 3, 3, 3, 3];
        assert_eq!(&cm_masks[..26], &expected_cm);

        let expected_x_sample: [f32; 32] = [
            5.54683268e-01,
            8.32024932e-01,
            7.07085192e-01,
            7.07085192e-01,
            8.94399881e-01,
            4.47199941e-01,
            0.00000000e+00,
            9.99969482e-01,
            0.00000000e+00,
            -9.99969482e-01,
            0.00000000e+00,
            -9.99969482e-01,
            -9.99969482e-01,
            0.00000000e+00,
            -9.99969482e-01,
            0.00000000e+00,
            -8.43568146e-02,
            9.59317237e-02,
            7.45065928e-01,
            6.54643178e-01,
            0.00000000e+00,
            9.97945368e-01,
            0.00000000e+00,
            6.40701875e-02,
            7.07106769e-01,
            0.00000000e+00,
            7.07106769e-01,
            0.00000000e+00,
            -1.94773776e-03,
            -7.07104146e-01,
            -7.07104146e-01,
            -1.94773776e-03,
        ];
        for (i, (&got, &exp)) in x_dec[..32].iter().zip(expected_x_sample.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "quant_all_bands X mismatch at {i}: got {got}, expected {exp}"
            );
        }

        // Anti-collapse against the recorded values
        let mut xy_ac = [0.0_f32; 480];
        xy_ac[..240].copy_from_slice(&x_dec);
        xy_ac[240..].copy_from_slice(&y_dec);

        let energy = BandEnergyHistory {
            current: [[2.5_f32; MAX_BANDS]; 2],
            previous: [[0.5_f32; MAX_BANDS]; 2],
            two_back: [[1.0_f32; MAX_BANDS]; 2],
            background: [[0.0_f32; MAX_BANDS]; 2],
        };
        cm_masks[8 * 2] = 1;
        cm_masks[9 * 2 + 1] = 2;

        anti_collapse(&mut xy_ac, &cm_masks, &frame, 2, &energy, &pulses, seed);

        let expected_ac_b8_x: [f32; 4] =
            [-1.09499902e-01, 1.62257046e-01, 9.67137575e-01, -1.62257046e-01];
        let expected_ac_b9_y: [f32; 4] =
            [1.33849055e-01, 9.79903519e-01, 1.33849055e-01, 6.29118606e-02];
        for (i, (&got, &exp)) in xy_ac[16..20].iter().zip(expected_ac_b8_x.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "anti_collapse B8 X mismatch at {i}: got {got}, expected {exp}"
            );
        }
        for (i, (&got, &exp)) in xy_ac[260..264].iter().zip(expected_ac_b9_y.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "anti_collapse B9 Y mismatch at {i}: got {got}, expected {exp}"
            );
        }

        // Denormalisation against the recorded values
        let mut freq = [0.0_f32; 240];
        let layout = crate::celt::synthesis::Denormalisation {
            start: 0,
            end: 13,
            lm: BlockSize::new(1),
            downsample: 1,
            silence: false,
        };
        crate::celt::synthesis::denormalise_bands(
            &xy_ac[..240],
            &mut freq,
            &energy.current[0],
            &layout,
        );
        let expected_denorm: [f32; 16] = [
            2.71957245e+02,
            4.07935883e+02,
            3.04427734e+02,
            3.04427734e+02,
            2.72288422e+02,
            1.36144211e+02,
            -2.35004654e+01,
            2.23568542e+02,
            -2.34474545e+01,
            -1.87573914e+02,
            1.97168789e+01,
            -1.57730225e+02,
            -1.26608727e+02,
            1.88210735e+01,
            -1.15584290e+02,
            2.04332314e+01,
        ];
        for (i, (&got, &exp)) in freq[..16].iter().zip(expected_denorm.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-4,
                "denormalise_bands mismatch at {i}: got {got}, expected {exp}"
            );
        }
        return;
    }

    #[test]
    fn hybrid_folding_and_recursive_split_match_recorded_values() {
        let hbuf: [u8; 64] = [
            0xee, 0x02, 0x68, 0x74, 0xc2, 0x8c, 0x4e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x01, 0x18, 0x05, 0xd9, 0xb7, 0x77,
        ];
        let mut pulses = [0i32; MAX_BANDS];
        pulses[17] = 560;
        pulses[18] = 0;
        pulses[19] = 160;
        pulses[20] = 80;

        let mut tf_res = [0i32; MAX_BANDS];
        tf_res[17] = -1;
        tf_res[19] = -1;
        let frame = ShapeFrame {
            start: 17,
            end: 21,
            lm: BlockSize::new(3),
            blocks: 1,
            spread: Spread::Aggressive,
            tf_res,
            total_bits: 64 * 8 * 8,
            phase_inversion: true,
        };
        let alloc = allocation(pulses, 21, 19, true);

        let mut dec = Decoder::new(&hbuf);
        let mut x_dec = [0.0_f32; 960];
        let mut y_dec = [0.0_f32; 960];
        let mut cm_dec = [0u8; 42];
        let mut seed = 54321u32;

        unquantize_bands(
            &frame,
            &alloc,
            &mut dec,
            Planes::new(&mut x_dec, Some(&mut y_dec)),
            &mut cm_dec,
            &mut seed,
        )
        .expect("hybrid unquantize_bands succeeds");

        assert_eq!(seed, 3_429_552_881);
        assert_eq!(&cm_dec[34..42], &[1, 1, 1, 1, 1, 1, 1, 1]);

        let expected_b17_x: [f32; 8] = [
            -2.58565307e-01,
            -2.58565307e-01,
            -1.23134904e-01,
            -1.23134904e-01,
            -3.38164717e-02,
            -3.38164717e-02,
            1.36362864e-02,
            1.36362864e-02,
        ];
        let expected_b18_x: [f32; 8] = [
            9.48805958e-02,
            1.30983472e-01,
            -4.69578542e-02,
            1.42883748e-01,
            1.63870052e-01,
            7.79950665e-03,
            -9.67299640e-02,
            -4.98525146e-03,
        ];
        let expected_b18_y: [f32; 8] = [
            7.00975358e-02,
            1.43721988e-02,
            4.78779413e-02,
            -7.78567567e-02,
            1.24147587e-01,
            -1.47601604e-01,
            3.63272764e-02,
            -1.36050940e-01,
        ];
        let expected_b19_y: [f32; 8] = [
            3.84046286e-02,
            3.84046286e-02,
            5.76669723e-02,
            5.76669723e-02,
            4.15627807e-02,
            4.15627807e-02,
            1.84608120e-02,
            1.84608120e-02,
        ];

        for (i, (&got, &exp)) in x_dec[320..328].iter().zip(expected_b17_x.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "Hybrid B17 X mismatch at {i}: got {got}, expected {exp}"
            );
        }
        for (i, (&got, &exp)) in x_dec[384..392].iter().zip(expected_b18_x.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "Hybrid B18 folded X mismatch at {i}: got {got}, expected {exp}"
            );
        }
        for (i, (&got, &exp)) in y_dec[384..392].iter().zip(expected_b18_y.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "Hybrid B18 folded Y mismatch at {i}: got {got}, expected {exp}"
            );
        }
        for (i, (&got, &exp)) in y_dec[480..488].iter().zip(expected_b19_y.iter()).enumerate() {
            assert!(
                (got - exp).abs() <= 1e-6,
                "Hybrid B19 intensity Y mismatch at {i}: got {got}, expected {exp}"
            );
        }
        return;
    }

    // RFC 8251 Section 10: a decoder downmixing to mono may skip the 180-degree phase shift of
    // intensity stereo while still consuming the inversion bit from the range coder.
    #[test]
    fn intensity_stereo_phase_inversion_is_skipped_when_disabled() {
        let buf = [0xFFu8; 16];
        let mut pulses = [0i32; MAX_BANDS];
        pulses[0] = 64;
        let alloc = allocation(pulses, 1, 0, false);

        let decode = |phase_inversion: bool| {
            let frame = ShapeFrame {
                start: 0,
                end: 1,
                lm: BlockSize::new(2),
                blocks: 1,
                spread: Spread::None,
                tf_res: [0; MAX_BANDS],
                total_bits: 16 * 8 * 8,
                phase_inversion,
            };
            let mut dec = Decoder::new(&buf);
            let mut x = [0.0_f32; 4];
            let mut y = [0.0_f32; 4];
            let mut cm = [0u8; 2 * MAX_BANDS];
            let mut seed = 0u32;
            unquantize_bands(
                &frame,
                &alloc,
                &mut dec,
                Planes::new(&mut x, Some(&mut y)),
                &mut cm,
                &mut seed,
            )
            .expect("intensity band decodes");
            return (x, y, dec.tell_frac(), dec.final_range());
        };

        let (x_inv, y_inv, tell_inv, rng_inv) = decode(true);
        let (x_no_inv, y_no_inv, tell_no_inv, rng_no_inv) = decode(false);

        assert_eq!(tell_inv, tell_no_inv);
        assert_eq!(rng_inv, rng_no_inv);
        assert_eq!(x_inv, x_no_inv);
        assert!(x_inv.iter().any(|&v| v != 0.0));
        assert_eq!(y_inv, x_inv.map(|v| -v));
        assert_eq!(y_no_inv, x_no_inv);
        return;
    }
}
