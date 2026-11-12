//! CELT bit allocation, pulse cache lookup, post-filter parameters and time-frequency resolution
//! ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>),
//! [Section 4.3.4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.1>),
//! [Section 4.3.4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.5>) and
//! [Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
//!
//! CELT derives almost the entire per-band bit allocation implicitly from the frame's bit budget:
//! encoder and decoder run the same deterministic allocation on the range coder state, modified
//! only by a few signalled parameters. Per-band caps ([`init_caps`]) bound each band, dynamic
//! boosts and the allocation trim ([`decode_band_boosts_and_trim`]) tilt the static allocation
//! table ([`ALLOC_VECTORS`]), and [`BandAllocation::compute`] interpolates between its rows,
//! decodes band skipping and the stereo parameters, and splits each band's budget into fine
//! energy bits (`fine_quant`) and PVQ shape bits (`pulses`, in `1/8` bit units), which the pulse
//! cache ([`bits2pulses_at_level`], [`pulses2bits_at_level`], [`get_pulses`]) maps to exact pulse
//! counts.

use std::ops::Range;

use symphonia_core::errors::Result;

use crate::celt::BlockSize;
use crate::celt::constant::{
    ALLOC_STEPS, ALLOC_VECTORS, BITRES, CACHE_BITS, CACHE_CAPS, CACHE_INDEX, CDF_SPREAD,
    CDF_TAPSET, CDF_TRIM, EBANDS, FINE_OFFSET, LOG2_FRAC_TABLE, LOGN, MAX_BANDS, MAX_FINE_BITS,
    MAX_PSEUDO, Spread, TF_SELECT_TABLE, band_width,
};
use crate::entropy::Decoder;

/// One bit in `1/8` bit units, the unit of every allocation quantity.
const ONE_BIT_Q3: i32 = 1 << BITRES;

/// Pseudo-pulse index from which [`get_pulses`] grows semi-logarithmically
/// ([RFC 6716 Section 4.3.4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.1>)).
const PULSE_CODE_LINEAR_LIMIT: usize = 8;

/// Fewest bits that must remain in the frame for the post-filter flag and its parameters
/// ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>),
/// Appendix A).
const POSTFILTER_MIN_BITS: i32 = 16;

/// Number of equiprobable pitch octave symbols
/// ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
const PITCH_OCTAVE_SYMBOLS: u32 = 6;

/// Raw bits of the fine pitch in octave `0`; each higher octave adds one.
const PITCH_FINE_BITS_BASE: u32 = 4;

/// Pitch period of octave `0` before the fine pitch: `period = (16 << octave) + fine_pitch - 1`.
const PITCH_PERIOD_BASE: u32 = 16;

/// Raw bits of the post-filter gain index.
const POSTFILTER_GAIN_BITS: u32 = 3;

/// Post-filter gain step: `G = 3 * (int_gain + 1) / 32`.
const POSTFILTER_GAIN_STEP: f32 = 3.0 / 32.0;

/// Fewest bits that must remain for the tapset symbol.
const TAPSET_MIN_BITS: i32 = 2;

/// Cost in bits of the improbable time-frequency change symbol in a non-transient frame, for
/// the first coded band (PDF `{15, 1}/16`) and for each later band (PDF `{31, 1}/32`)
/// ([RFC 6716 Section 4.3.4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.5>)).
const TF_CHANGE_LOGP_STEADY: (i32, i32) = (4, 5);

/// The same costs in a transient frame (PDFs `{3, 1}/4` and `{15, 1}/16`).
const TF_CHANGE_LOGP_TRANSIENT: (i32, i32) = (2, 4);

/// Fewest bits that must remain for the spreading decision
/// ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)).
const SPREAD_MIN_BITS: i32 = 4;

/// Cost in bits of the first boost of a band (`p = 1/64`)
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
const BOOST_INITIAL_LOGP: i32 = 6;

/// Cost in bits of every further boost of the same band (`p = 1/2`).
const BOOST_REPEAT_LOGP: i32 = 1;

/// Lowest first-boost cost after boosted bands lowered it (`p = 1/4`).
const BOOST_MIN_LOGP: i32 = 2;

/// Upper bound of the boost quantum in bits: `quanta = min(8 * N, max(48, N))` eighth bits.
const BOOST_QUANTA_CAP_BITS: i32 = 6;

/// Fewest bits that must remain for the allocation trim symbol
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>), Table 58).
const TRIM_MIN_BITS: i32 = 6;

/// Allocation trim that leaves the static allocation untilted.
const TRIM_NEUTRAL: i32 = 5;

/// Shape threshold of a band: `24 * N / 16` eighth bits, written `3 * (N << BITRES) >> 4`
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
const SHAPE_THRESHOLD_NUMERATOR: i32 = 3;

/// Divisor shift of the shape threshold (`/ 16`).
const SHAPE_THRESHOLD_SHIFT: u32 = 4;

/// Divisor shift of the trim offset (`/ 64`).
const TRIM_OFFSET_SHIFT: u32 = 6;

/// Converts the `1/32` bit per bin of [`ALLOC_VECTORS`] into `1/8` bit units (`>> 2`).
const ALLOC_TABLE_SHIFT: u32 = 2;

/// Half the log2 of the bins per degree of freedom goes to fine energy (`>> 1`)
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>),
/// fine-energy versus shape split).
const FINE_HALF_LOG_SHIFT: u32 = 1;

/// Two-bin bands receive a quarter bit per degree of freedom more fine energy (`>> 2`).
const FINE_TWO_BIN_SHIFT: u32 = 2;

/// Below two bits per degree of freedom the fine share grows by a quarter of the log term.
const FINE_LOW_RATE_TWO_BITS: i32 = 2;

/// Below three bits per degree of freedom the fine share grows by an eighth of the log term.
const FINE_LOW_RATE_THREE_BITS: i32 = 3;

/// Quarter of the log term (`>> 2`).
const FINE_LOW_RATE_QUARTER_SHIFT: u32 = 2;

/// Eighth of the log term (`>> 3`).
const FINE_LOW_RATE_EIGHTH_SHIFT: u32 = 3;

/// Bias of the stored caps: [`CACHE_CAPS`] holds the per-bin maximum minus `64`
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
const CAP_BIAS: i32 = 64;

/// Divisor shift of the cap product (`/ 4`).
const CAP_SHIFT: u32 = 2;

/// Result of the CELT bit allocation of one frame
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BandAllocation {
    /// PVQ shape bits per band in `1/8` bit units.
    pub(crate) pulses: [i32; MAX_BANDS],
    /// Fine energy bits per channel in each band (`0..=8`)
    /// ([RFC 6716 Section 4.3.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.2>)).
    pub(crate) fine_quant: [i32; MAX_BANDS],
    /// Final energy priority of each band: bands marked `false` take the bits left at the end of
    /// the frame first, then bands marked `true`
    /// ([RFC 6716 Section 4.3.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.2>)).
    pub(crate) fine_priority: [bool; MAX_BANDS],
    /// Surplus `1/8` bits above the cap of the highest coded bands, carried into PVQ rebalancing.
    pub(crate) balance: i32,
    /// One past the highest non-skipped band (`start < coded_bands <= end`).
    pub(crate) coded_bands: usize,
    /// First band coded with intensity stereo (`0` when intensity stereo is off).
    pub(crate) intensity: usize,
    /// Whether the two channels are coded independently rather than as mid and side.
    pub(crate) dual_stereo: bool,
}

/// Pitch comb post-filter parameters decoded at the start of a CELT frame
/// ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct PostFilterParams {
    /// Pitch period `T` in 48 kHz samples (`0` when the post-filter is off, else `15..=1022`).
    pub(crate) period: usize,
    /// Comb filter gain (`0.0` when off, else `3 * (int_gain + 1) / 32`).
    pub(crate) gain: f32,
    /// Tapset (`0..=2`) selecting a row of
    /// [`COMB_FILTER_GAINS`](crate::celt::constant::COMB_FILTER_GAINS).
    pub(crate) tapset: usize,
}

/// Converts a pseudo-pulse index `pulse_code` (`0..=40`) into the pulse count `K`
/// ([RFC 6716 Section 4.3.4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.1>)).
///
/// Below `8` the index is the pulse count; from `8` on the count grows semi-logarithmically:
/// `K = (8 + (i mod 8)) << (i / 8 - 1)`.
#[inline]
pub(crate) const fn get_pulses(pulse_code: usize) -> usize {
    if pulse_code < PULSE_CODE_LINEAR_LIMIT {
        return pulse_code;
    }
    return (PULSE_CODE_LINEAR_LIMIT + (pulse_code & (PULSE_CODE_LINEAR_LIMIT - 1)))
        << ((pulse_code / PULSE_CODE_LINEAR_LIMIT) - 1);
}

/// Level of the pulse cache a PVQ partition is looked up at
/// ([RFC 6716 Section 4.3.4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.1>)).
///
/// An unsplit band of a frame of block size `LM` sits at the level of exponent `LM`; every PVQ
/// split halves the partition and lowers the exponent by one
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)),
/// down to `-1` for the halves of a 2.5 ms band, the lowest level the cache holds. The value is
/// stored as the row of [`CACHE_INDEX`], `exponent + 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CacheLevel(usize);

impl CacheLevel {
    /// Level of an unsplit band at block size `lm`.
    pub(crate) const fn from_block_size(lm: BlockSize) -> Self {
        return Self(lm.log2() + 1);
    }

    /// Level of each half after one PVQ split, `None` at the lowest level, where a partition is
    /// never split again
    /// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
    pub(crate) const fn split(self) -> Option<Self> {
        return match self.0 {
            0 => None,
            row => Some(Self(row - 1)),
        };
    }

    /// Signed block-size exponent of the level (`-1..=3`), the `LM` in which the split-angle
    /// resolution and the short-block bit differential are defined
    /// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
    pub(crate) const fn exponent(self) -> i32 {
        return self.0 as i32 - 1;
    }

    /// Row of [`CACHE_INDEX`].
    const fn row(self) -> usize {
        return self.0;
    }
}

/// Pulse cache sub-table of `band` at `level`: `[max_pseudo, cost_1 - 1, ..., cost_max - 1]`
/// in `1/8` bit units
/// ([RFC 6716 Section 4.3.4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.1>)).
#[inline]
fn pulse_cache(level: CacheLevel, band: usize) -> &'static [u8] {
    let start = CACHE_INDEX[level.row()][band] as usize;
    return &CACHE_BITS[start..];
}

/// Largest cached `1/8` bit cost of `band` at `level`; a partition needing more than this plus
/// `12` is split
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
#[inline]
pub(crate) fn max_cache_bits(level: CacheLevel, band: usize) -> i32 {
    let cache = pulse_cache(level, band);
    let max_pseudo = usize::from(cache[0]);
    return i32::from(cache[max_pseudo]);
}

/// Maps a `1/8` bit budget `bits` to the pseudo-pulse index (`0..=40`) whose cached cost is
/// nearest, rounding down on a tie, for `band` at `level`
/// ([RFC 6716 Section 4.3.4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.1>)).
#[inline]
pub(crate) fn bits2pulses_at_level(level: CacheLevel, band: usize, bits: i32) -> i32 {
    let cache = pulse_cache(level, band);
    let max_pseudo = usize::from(cache[0]);
    debug_assert!(max_pseudo <= MAX_PSEUDO);
    let target = bits - 1;

    // `cache[1..=max_pseudo]` is non-decreasing: `lo` is the last index below the target and
    // `hi` the first at or above it (or `max_pseudo` when no entry reaches the target).
    let lo = cache[1..=max_pseudo].partition_point(|&cost| i32::from(cost) < target);
    let hi = (lo + 1).min(max_pseudo);

    let lo_bits = if lo == 0 { -1 } else { i32::from(cache[lo]) };
    let hi_bits = i32::from(cache[hi]);
    if target - lo_bits <= hi_bits - target {
        return lo as i32;
    }
    return hi as i32;
}

/// Exact `1/8` bit cost of pseudo-pulse index `pulses` (`0..=40`) for `band` at `level`
/// ([RFC 6716 Section 4.3.4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.1>)).
#[inline]
pub(crate) fn pulses2bits_at_level(level: CacheLevel, band: usize, pulses: i32) -> i32 {
    if pulses <= 0 {
        return 0;
    }
    let cache = pulse_cache(level, band);
    return i32::from(cache[pulses as usize]) + 1;
}

/// Maximum `1/8` bit allocation of every band at block size `lm` for `channels` channels:
/// `cap[i] = (CACHE_CAPS[lm][channels - 1][i] + 64) * channels * N[i] / 4`
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
pub(crate) fn init_caps(lm: BlockSize, channels: usize) -> [i32; MAX_BANDS] {
    let caps = &CACHE_CAPS[lm.log2()][channels - 1];
    let c = channels as i32;
    return std::array::from_fn(|i| {
        return ((i32::from(caps[i]) + CAP_BIAS) * c * (band_width(i, lm) as i32)) >> CAP_SHIFT;
    });
}

/// Decodes the pitch comb post-filter parameters at the start of a CELT frame, which are coded
/// only when band `0` is coded and at least 16 bits remain
/// ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
pub(crate) fn decode_postfilter(
    range_decoder: &mut Decoder<'_>,
    start: usize,
    total_bits: i32,
) -> Result<PostFilterParams> {
    if start != 0 || range_decoder.tell() + POSTFILTER_MIN_BITS > total_bits {
        return Ok(PostFilterParams::default());
    }
    if range_decoder.decode_symbol_logp(1)? == 0 {
        return Ok(PostFilterParams::default());
    }

    let octave = range_decoder.decode_uint(PITCH_OCTAVE_SYMBOLS)?;
    let fine_pitch = range_decoder.decode_raw_bits(PITCH_FINE_BITS_BASE + octave)?;
    let period = ((PITCH_PERIOD_BASE << octave) + fine_pitch - 1) as usize;
    let gain_index = range_decoder.decode_raw_bits(POSTFILTER_GAIN_BITS)?;
    let gain = POSTFILTER_GAIN_STEP * ((gain_index + 1) as f32);

    let mut tapset = 0usize;
    if range_decoder.tell() + TAPSET_MIN_BITS <= total_bits {
        tapset = range_decoder.decode_symbol(CDF_TAPSET)? as usize;
    }
    return Ok(PostFilterParams { period, gain, tapset });
}

/// Decodes the per-band time-frequency resolution changes of bands `start..end`
/// ([RFC 6716 Section 4.3.4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.5>),
/// Tables 60 to 63).
///
/// Each band codes whether its choice differs from the previous band's; one bit is reserved for
/// the table selector `tf_select` whenever `lm > 0` and the budget allows it, and is read only
/// when the selector would change the adjustment of some band.
pub(crate) fn tf_decode(
    start: usize,
    end: usize,
    is_transient: bool,
    lm: BlockSize,
    range_decoder: &mut Decoder<'_>,
) -> Result<[i32; MAX_BANDS]> {
    let mut budget = range_decoder.storage_bits();
    let mut tell = range_decoder.tell();
    let (first_logp, later_logp) =
        if is_transient { TF_CHANGE_LOGP_TRANSIENT } else { TF_CHANGE_LOGP_STEADY };
    let tf_select_rsv = lm.log2() > 0 && tell + first_logp < budget;
    if tf_select_rsv {
        budget -= 1;
    }

    let mut tf_changes = [0usize; MAX_BANDS];
    let mut tf_changed = 0usize;
    let mut curr = 0usize;
    let mut logp = first_logp;
    for change in &mut tf_changes[start..end] {
        if tell + logp <= budget {
            curr ^= range_decoder.decode_symbol_logp(logp as u32)? as usize;
            tell = range_decoder.tell();
            tf_changed |= curr;
        }
        *change = curr;
        logp = later_logp;
    }

    let table = &TF_SELECT_TABLE[lm.log2()][usize::from(is_transient)];
    let mut tf_select = 0usize;
    if tf_select_rsv && table[0][tf_changed] != table[1][tf_changed] {
        tf_select = range_decoder.decode_symbol_logp(1)? as usize;
    }
    let adjustments = table[tf_select];

    let mut tf_res = [0i32; MAX_BANDS];
    tf_res[start..end]
        .iter_mut()
        .zip(&tf_changes[start..end])
        .for_each(|(res, &change)| *res = i32::from(adjustments[change]));
    return Ok(tf_res);
}

/// Decodes the PVQ spreading decision, which defaults
/// to normal spreading when fewer than four bits remain
/// ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>),
/// Table 59).
pub(crate) fn decode_spread_decision(
    range_decoder: &mut Decoder<'_>,
    total_bits: i32,
) -> Result<Spread> {
    if range_decoder.tell() + SPREAD_MIN_BITS > total_bits {
        return Ok(Spread::Normal);
    }
    return Ok(Spread::try_from(range_decoder.decode_symbol(CDF_SPREAD)?)?);
}

/// Decodes the band boosts (`offsets[start..end]`, in `1/8` bit units) and the allocation trim
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>),
/// Table 58).
///
/// Each boost symbol costs `dynalloc_logp` bits the first time and one bit afterwards; a boosted
/// band lowers the first-boost cost of the following bands down to two bits. The trim defaults
/// to the neutral `5` when fewer than six bits remain.
pub(crate) fn decode_band_boosts_and_trim(
    range_decoder: &mut Decoder<'_>,
    start: usize,
    end: usize,
    lm: BlockSize,
    channels: usize,
    total_bits: i32,
    cap: &[i32; MAX_BANDS],
) -> Result<([i32; MAX_BANDS], i32)> {
    let mut offsets = [0i32; MAX_BANDS];
    let mut dynalloc_logp = BOOST_INITIAL_LOGP;
    let mut total_bits_q3 = total_bits << BITRES;
    let mut tell_q3 = range_decoder.tell_frac() as i32;
    let c = channels as i32;

    for band in start..end {
        let width = c * (band_width(band, lm) as i32);
        let quanta = (width << BITRES).min((BOOST_QUANTA_CAP_BITS << BITRES).max(width));
        let mut dynalloc_loop_logp = dynalloc_logp;
        let mut boost = 0i32;

        while tell_q3 + (dynalloc_loop_logp << BITRES) < total_bits_q3 && boost < cap[band] {
            let flag = range_decoder.decode_symbol_logp(dynalloc_loop_logp as u32)?;
            tell_q3 = range_decoder.tell_frac() as i32;
            if flag == 0 {
                break;
            }
            boost += quanta;
            total_bits_q3 -= quanta;
            dynalloc_loop_logp = BOOST_REPEAT_LOGP;
        }

        offsets[band] = boost;
        if boost > 0 {
            dynalloc_logp = (dynalloc_logp - 1).max(BOOST_MIN_LOGP);
        }
    }

    let mut alloc_trim = TRIM_NEUTRAL;
    if tell_q3 + (TRIM_MIN_BITS << BITRES) <= total_bits_q3 {
        alloc_trim = range_decoder.decode_symbol(CDF_TRIM)? as i32;
    }
    return Ok((offsets, alloc_trim));
}

/// Bands, channels and block size an allocation covers
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
#[derive(Debug, Clone, Copy)]
pub(crate) struct AllocationLayout {
    /// First coded band (`0`, or `17` in Hybrid mode).
    pub(crate) start: usize,
    /// One past the last coded band.
    pub(crate) end: usize,
    /// Coded channels (`1` or `2`).
    pub(crate) channels: usize,
    /// Block size of the frame.
    pub(crate) lm: BlockSize,
}

impl AllocationLayout {
    /// The coded bands.
    fn bands(self) -> Range<usize> {
        return self.start..self.end;
    }

    /// Channel count as the signed arithmetic type of the allocation.
    fn channels_i32(self) -> i32 {
        return self.channels as i32;
    }

    /// One bit per channel in `1/8` bit units, the least any coded band receives.
    fn floor_q3(self) -> i32 {
        return self.channels_i32() << BITRES;
    }

    /// `1` in stereo and `0` in mono: halves a per-frame count into a per-channel count.
    fn stereo_shift(self) -> usize {
        return usize::from(self.channels > 1);
    }
}

/// Signalled inputs of the allocation
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
#[derive(Debug, Clone, Copy)]
pub(crate) struct AllocationBudget<'a> {
    /// Band boosts in `1/8` bit units.
    pub(crate) offsets: &'a [i32; MAX_BANDS],
    /// Per-band caps from [`init_caps`].
    pub(crate) cap: &'a [i32; MAX_BANDS],
    /// Allocation trim (`0..=10`, neutral at `5`).
    pub(crate) alloc_trim: i32,
    /// Remaining `1/8` bits after the conservative one-eighth and the anti-collapse reservation.
    pub(crate) total: i32,
}

/// Bits set aside before the search and released when their symbol is not coded
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
#[derive(Debug, Clone, Copy)]
struct Reservations {
    /// Budget left for the bands.
    total: i32,
    /// One bit for the final skip flag, when it fits.
    skip: i32,
    /// `log2(end - start)` bits for the intensity stereo band, when they fit.
    intensity: i32,
    /// One bit for the dual stereo flag, when it fits.
    dual_stereo: i32,
}

impl Reservations {
    /// Takes the skip, intensity and dual stereo reservations from `total`.
    fn take(layout: AllocationLayout, total: i32) -> Self {
        let mut total = total.max(0);
        let skip = reserve_bit(&mut total);
        if layout.channels != 2 {
            return Self { total, skip, intensity: 0, dual_stereo: 0 };
        }
        let intensity = i32::from(LOG2_FRAC_TABLE[layout.end - layout.start]);
        if intensity > total {
            return Self { total, skip, intensity: 0, dual_stereo: 0 };
        }
        total -= intensity;
        let dual_stereo = reserve_bit(&mut total);
        return Self { total, skip, intensity, dual_stereo };
    }
}

/// Takes one bit from `total` when at least one bit is left and returns what was taken.
fn reserve_bit(total: &mut i32) -> i32 {
    let reserved = if *total >= ONE_BIT_Q3 { ONE_BIT_Q3 } else { 0 };
    *total -= reserved;
    return reserved;
}

/// Shape threshold of band `j`: one bit per channel or `24 / 16` eighth bits per bin, whichever
/// is greater ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
fn shape_threshold(layout: AllocationLayout, j: usize) -> i32 {
    let n = band_width(j, layout.lm) as i32;
    return layout
        .floor_q3()
        .max((SHAPE_THRESHOLD_NUMERATOR * (n << BITRES)) >> SHAPE_THRESHOLD_SHIFT);
}

/// Tilt of band `j` from the allocation trim,
/// `channels * N0 * (trim - 5 - LM) * remaining_bands * 2^LM * 8 / 64`, one bit per channel less
/// for single-bin bands
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
fn trim_offset(layout: AllocationLayout, alloc_trim: i32, j: usize) -> i32 {
    let lm = layout.lm.log2();
    let n0 = band_width(j, BlockSize::SHORT) as i32;
    let remaining_bands = (layout.end - j - 1) as i32;
    let tilt = alloc_trim - TRIM_NEUTRAL - (lm as i32);
    let mut offset =
        (layout.channels_i32() * n0 * tilt * remaining_bands * (1 << (lm + BITRES as usize)))
            >> TRIM_OFFSET_SHIFT;
    if band_width(j, layout.lm) == 1 {
        offset -= layout.floor_q3();
    }
    return offset;
}

/// Per-band limits shared by every allocation phase: thresholds, trim offsets, caps and boosts
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
struct BandLimits<'a> {
    layout: AllocationLayout,
    cap: &'a [i32; MAX_BANDS],
    offsets: &'a [i32; MAX_BANDS],
    /// Least shape allocation of each coded band.
    thresh: [i32; MAX_BANDS],
    /// Tilt added to the static allocation of each coded band.
    trim_offset: [i32; MAX_BANDS],
}

impl<'a> BandLimits<'a> {
    /// Computes the thresholds and trim offsets of the coded bands.
    fn new(layout: AllocationLayout, budget: AllocationBudget<'a>) -> Self {
        let coded = |j: usize| layout.bands().contains(&j);
        let thresh = std::array::from_fn(|j| if coded(j) { shape_threshold(layout, j) } else { 0 });
        let trim_offset = std::array::from_fn(|j| {
            if coded(j) { trim_offset(layout, budget.alloc_trim, j) } else { 0 }
        });
        return Self { layout, cap: budget.cap, offsets: budget.offsets, thresh, trim_offset };
    }

    /// Static allocation of `row` for band `j` in `1/8` bits before trim and boosts:
    /// `channels * N0 * row[j] << LM >> 2`.
    fn static_bits(&self, row: &[u8; MAX_BANDS], j: usize) -> i32 {
        let n0 = band_width(j, BlockSize::SHORT) as i32;
        return ((self.layout.channels_i32() * n0 * i32::from(row[j])) << self.layout.lm.log2())
            >> ALLOC_TABLE_SHIFT;
    }

    /// Applies the trim tilt to a non-zero static allocation, never below zero.
    fn trimmed(&self, bits: i32, j: usize) -> i32 {
        if bits > 0 {
            return (bits + self.trim_offset[j]).max(0);
        }
        return bits;
    }

    /// Static allocation of `row` for band `j` with trim and boost applied.
    fn row_bits(&self, row: &[u8; MAX_BANDS], j: usize) -> i32 {
        return self.trimmed(self.static_bits(row, j), j) + self.offsets[j];
    }

    /// Total of the candidate allocation `band_bits` over the coded bands, walking down from the
    /// top band: until a band reaches its threshold, bands receive the floor or nothing; from
    /// there down every band counts up to its cap.
    fn curve_total(&self, band_bits: impl Fn(usize) -> i32) -> i32 {
        let floor = self.layout.floor_q3();
        let (psum, _) = self.layout.bands().rev().fold((0i32, false), |(psum, done), j| {
            let bits = band_bits(j);
            return match (bits >= self.thresh[j] || done, bits >= floor) {
                (true, _) => (psum + bits.min(self.cap[j]), true),
                (false, true) => (psum + floor, false),
                (false, false) => (psum, false),
            };
        });
        return psum;
    }

    /// First row of [`ALLOC_VECTORS`] whose total exceeds `total` (`1..=NB_ALLOC_VECTORS`); the
    /// allocation interpolates between it and the row below. Row totals are non-decreasing, so
    /// the search is a partition point.
    fn first_row_over_budget(&self, total: i32) -> usize {
        return 1 + ALLOC_VECTORS[1..]
            .partition_point(|row| self.curve_total(|j| self.row_bits(row, j)) <= total);
    }
}

/// The two allocations the budget falls between: `lower` fits and `lower + delta` exceeds it
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>),
/// linear interpolation).
struct InterpolationCurve {
    lower: [i32; MAX_BANDS],
    delta: [i32; MAX_BANDS],
    /// Highest boosted band (or `start`); skipping never descends past it.
    skip_start: usize,
}

impl InterpolationCurve {
    /// Builds the bounds from the rows around `over_row`; above the last row the cap bounds the
    /// allocation, and the row of all zeros carries no boosts.
    fn new(limits: &BandLimits<'_>, over_row: usize) -> Self {
        let layout = limits.layout;
        let under_row = over_row - 1;
        let coded = |j: usize| layout.bands().contains(&j);
        let lower = std::array::from_fn(|j| {
            if !coded(j) {
                return 0;
            }
            let bits = limits.trimmed(limits.static_bits(&ALLOC_VECTORS[under_row], j), j);
            if under_row > 0 {
                return bits + limits.offsets[j];
            }
            return bits;
        });
        let delta = std::array::from_fn(|j| {
            if !coded(j) {
                return 0;
            }
            let raw =
                ALLOC_VECTORS.get(over_row).map_or(limits.cap[j], |row| limits.static_bits(row, j));
            let upper = limits.trimmed(raw, j) + limits.offsets[j];
            return (upper - lower[j]).max(0);
        });
        let skip_start =
            layout.bands().rev().find(|&j| limits.offsets[j] > 0).unwrap_or(layout.start);
        return Self { lower, delta, skip_start };
    }

    /// Allocation of band `j` at interpolation `weight` (`0..64`).
    fn at(&self, weight: i32, j: usize) -> i32 {
        return self.lower[j] + ((weight * self.delta[j]) >> ALLOC_STEPS);
    }

    /// Largest interpolation weight (`0..64`) whose total fits `total`. Totals are non-decreasing
    /// in the weight, so the search is a partition point.
    fn weight_within(&self, limits: &BandLimits<'_>, total: i32) -> i32 {
        let weights: [i32; 1 << ALLOC_STEPS] = std::array::from_fn(|i| i as i32);
        return weights[1..]
            .partition_point(|&weight| limits.curve_total(|j| self.at(weight, j)) <= total)
            as i32;
    }

    /// Per-band allocation at `weight` with thresholds, floor and caps applied, and its total.
    fn evaluate(&self, limits: &BandLimits<'_>, weight: i32) -> ([i32; MAX_BANDS], i32) {
        let floor = limits.layout.floor_q3();
        let mut bits = [0i32; MAX_BANDS];
        let mut psum = 0i32;
        let mut done = false;
        for j in limits.layout.bands().rev() {
            let interpolated = self.at(weight, j);
            let shape = match (interpolated >= limits.thresh[j] || done, interpolated >= floor) {
                (true, _) => {
                    done = true;
                    interpolated
                }
                (false, true) => floor,
                (false, false) => 0,
            };
            bits[j] = shape.min(limits.cap[j]);
            psum += bits[j];
        }
        return (bits, psum);
    }
}

/// Decodes band skipping from the top band down and returns the number of coded bands: a band
/// is skipped (reduced to its floor or nothing) until its flag says otherwise or the boosted
/// `skip_start` band is reached, while the unused bits are redistributed to the bands below
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>),
/// reallocation of unused bits with concurrent skip decoding).
fn decode_band_skips(
    range_decoder: &mut Decoder<'_>,
    limits: &BandLimits<'_>,
    skip_start: usize,
    bits: &mut [i32; MAX_BANDS],
    bits_so_far: &mut i32,
    reservations: &mut Reservations,
) -> Result<usize> {
    let layout = limits.layout;
    let floor = layout.floor_q3();
    let mut coded_bands = layout.end;
    loop {
        let j = coded_bands - 1;
        if j <= skip_start {
            reservations.total += reservations.skip;
            return Ok(coded_bands);
        }

        // Bits this band would receive from the per-bin redistribution of what is left.
        let left = reservations.total - *bits_so_far;
        let coeff_span = (EBANDS[coded_bands] - EBANDS[layout.start]) as i32;
        let percoeff = left / coeff_span;
        let rem_total = left - coeff_span * percoeff;
        let rem = (rem_total - ((EBANDS[j] - EBANDS[layout.start]) as i32)).max(0);
        let band_w = (EBANDS[coded_bands] - EBANDS[j]) as i32;
        let mut band_bits = bits[j] + percoeff * band_w + rem;

        if band_bits >= limits.thresh[j].max(floor + ONE_BIT_Q3) {
            if range_decoder.decode_symbol_logp(1)? != 0 {
                return Ok(coded_bands);
            }
            *bits_so_far += ONE_BIT_Q3;
            band_bits -= ONE_BIT_Q3;
        }

        *bits_so_far -= bits[j] + reservations.intensity;
        if reservations.intensity > 0 {
            reservations.intensity = i32::from(LOG2_FRAC_TABLE[j - layout.start]);
        }
        *bits_so_far += reservations.intensity;

        bits[j] = if band_bits >= floor { floor } else { 0 };
        *bits_so_far += bits[j];
        coded_bands -= 1;
    }
}

/// Decodes the intensity stereo band and the dual stereo flag when their bits were reserved
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
fn decode_stereo_params(
    range_decoder: &mut Decoder<'_>,
    layout: AllocationLayout,
    coded_bands: usize,
    reservations: &mut Reservations,
) -> Result<(usize, bool)> {
    let mut intensity = 0usize;
    if reservations.intensity > 0 {
        let count = (coded_bands + 1 - layout.start) as u32;
        intensity = layout.start + (range_decoder.decode_uint(count)? as usize);
    }
    if intensity <= layout.start {
        reservations.total += reservations.dual_stereo;
        reservations.dual_stereo = 0;
    }
    let dual_stereo = reservations.dual_stereo > 0 && range_decoder.decode_symbol_logp(1)? != 0;
    return Ok((intensity, dual_stereo));
}

/// Spreads the `left` unallocated bits over the coded bands in proportion to their 2.5 ms
/// width, then hands out the remainder one bin at a time from the lowest band
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>),
/// final reallocation).
fn distribute_remainder(
    layout: AllocationLayout,
    coded_bands: usize,
    mut left: i32,
    bits: &mut [i32; MAX_BANDS],
) {
    let coded_span = (EBANDS[coded_bands] - EBANDS[layout.start]) as i32;
    let percoeff = left / coded_span;
    left -= coded_span * percoeff;

    for (shape, j) in bits[layout.start..coded_bands].iter_mut().zip(layout.start..) {
        *shape += percoeff * (band_width(j, BlockSize::SHORT) as i32);
    }
    for (shape, j) in bits[layout.start..coded_bands].iter_mut().zip(layout.start..) {
        let add = left.min(band_width(j, BlockSize::SHORT) as i32);
        *shape += add;
        left -= add;
    }
    return;
}

/// Fine energy bits, priority and shape bits of one band, plus the excess over its cap.
#[derive(Debug, Clone, Copy)]
struct BandSplit {
    shape: i32,
    fine: i32,
    priority: bool,
    excess: i32,
}

/// Offset that biases the fine energy share of a band of `n` bins and `den` degrees of freedom
/// holding `shape` bits: half the log2 of the bins per degree of freedom minus [`FINE_OFFSET`],
/// a quarter bit more for two-bin bands, and more again at very low rates
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
fn fine_energy_offset(layout: AllocationLayout, j: usize, n: i32, den: i32, shape: i32) -> i32 {
    let log_m = (layout.lm.log2() as i32) << BITRES;
    let nc_log_n = den * (i32::from(LOGN[j]) + log_m);
    let mut offset = (nc_log_n >> FINE_HALF_LOG_SHIFT) - den * FINE_OFFSET;
    if n == 2 {
        offset += (den << BITRES) >> FINE_TWO_BIN_SHIFT;
    }
    offset += match (
        shape + offset < (den * FINE_LOW_RATE_TWO_BITS) << BITRES,
        shape + offset < (den * FINE_LOW_RATE_THREE_BITS) << BITRES,
    ) {
        (true, _) => nc_log_n >> FINE_LOW_RATE_QUARTER_SHIFT,
        (false, true) => nc_log_n >> FINE_LOW_RATE_EIGHTH_SHIFT,
        (false, false) => 0,
    };
    return offset;
}

/// Turns the excess above the cap into extra fine energy bits, up to [`MAX_FINE_BITS`], and
/// leaves the rest in `excess` as the balance carried into the next band.
fn carry_excess_into_fine(
    mut split: BandSplit,
    layout: AllocationLayout,
    balance: i32,
) -> BandSplit {
    if split.excess <= 0 {
        return split;
    }
    let extra_fine =
        (split.excess >> (layout.stereo_shift() + BITRES as usize)).min(MAX_FINE_BITS - split.fine);
    split.fine += extra_fine;
    let extra_bits = (extra_fine * layout.channels_i32()) << BITRES;
    split.priority = extra_bits >= split.excess - balance;
    split.excess -= extra_bits;
    return split;
}

/// Splits the `bit` budget of band `j` (its allocation plus the carried `balance`) into fine
/// energy bits per channel and PVQ shape bits
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>),
/// fine-energy versus shape split).
fn split_band_bits(
    limits: &BandLimits<'_>,
    j: usize,
    bit: i32,
    balance: i32,
    intensity: usize,
    dual_stereo: bool,
) -> BandSplit {
    let layout = limits.layout;
    let c = layout.channels_i32();
    let n = band_width(j, layout.lm) as i32;
    if n <= 1 {
        // A single bin has no shape to code: one bit per channel, everything else is excess.
        let excess = (bit - layout.floor_q3()).max(0);
        let split = BandSplit { shape: bit - excess, fine: 0, priority: true, excess };
        return carry_excess_into_fine(split, layout, balance);
    }

    let excess = (bit - limits.cap[j]).max(0);
    let shape = bit - excess;
    let extra_dof = i32::from(layout.channels == 2 && n > 2 && !dual_stereo && j < intensity);
    let den = c * n + extra_dof;
    let offset = fine_energy_offset(layout, j, n, den, shape);

    // Whole fine bits per degree of freedom, rounded, and never more than the shape bits allow.
    let num = (shape + offset + (den << (BITRES - 1))).max(0);
    let mut fine = (num / den) >> BITRES;
    if c * fine > (shape >> BITRES) {
        fine = (shape >> layout.stereo_shift()) >> BITRES;
    }
    fine = fine.min(MAX_FINE_BITS);
    let priority = fine * (den << BITRES) >= shape + offset;
    let split = BandSplit { shape: shape - ((c * fine) << BITRES), fine, priority, excess };
    return carry_excess_into_fine(split, layout, balance);
}

impl BandAllocation {
    /// Computes the per-band allocation of a frame in the order of
    /// [RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>):
    /// reservations, thresholds and trim offsets, search of the static table, interpolation,
    /// skip decoding with reallocation, stereo parameters, final reallocation, and the
    /// fine-energy versus shape split.
    pub(crate) fn compute(
        range_decoder: &mut Decoder<'_>,
        layout: AllocationLayout,
        budget: AllocationBudget<'_>,
    ) -> Result<Self> {
        let mut reservations = Reservations::take(layout, budget.total);
        let limits = BandLimits::new(layout, budget);
        let over_row = limits.first_row_over_budget(reservations.total);
        let curve = InterpolationCurve::new(&limits, over_row);
        let weight = curve.weight_within(&limits, reservations.total);
        let (mut bits, mut psum) = curve.evaluate(&limits, weight);
        let coded_bands = decode_band_skips(
            range_decoder,
            &limits,
            curve.skip_start,
            &mut bits,
            &mut psum,
            &mut reservations,
        )?;
        let (intensity, dual_stereo) =
            decode_stereo_params(range_decoder, layout, coded_bands, &mut reservations)?;
        distribute_remainder(layout, coded_bands, reservations.total - psum, &mut bits);
        return Ok(Self::split_fine_and_shape(&limits, coded_bands, intensity, dual_stereo, bits));
    }

    /// Splits every coded band's bits into fine energy and shape bits, carrying the excess over
    /// the caps upwards, and turns the floor of the skipped bands into fine energy bits
    /// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
    fn split_fine_and_shape(
        limits: &BandLimits<'_>,
        coded_bands: usize,
        intensity: usize,
        dual_stereo: bool,
        mut bits: [i32; MAX_BANDS],
    ) -> Self {
        let layout = limits.layout;
        let mut fine_quant = [0i32; MAX_BANDS];
        let mut fine_priority = [false; MAX_BANDS];
        let mut balance = 0i32;

        for j in layout.start..coded_bands {
            let split =
                split_band_bits(limits, j, bits[j] + balance, balance, intensity, dual_stereo);
            bits[j] = split.shape;
            fine_quant[j] = split.fine;
            fine_priority[j] = split.priority;
            balance = split.excess;
        }

        for j in coded_bands..layout.end {
            fine_quant[j] = (bits[j] >> layout.stereo_shift()) >> BITRES;
            bits[j] = 0;
            fine_priority[j] = fine_quant[j] < 1;
        }

        return Self {
            pulses: bits,
            fine_quant,
            fine_priority,
            balance,
            coded_bands,
            intensity,
            dual_stereo,
        };
    }
}

/// Smallest block size exponent whose transient frames can code the anti-collapse flag
/// ([RFC 6716 Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)).
const ANTI_COLLAPSE_MIN_LM: usize = 2;

/// Decodes the band boosts and the allocation trim, reserves one bit for the anti-collapse flag
/// of a transient frame, and computes the bit allocation of a frame of `total_bits`
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>),
/// [Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)). Returns the
/// allocation and the eighth bits reserved for the anti-collapse flag (`0` or `8`).
pub(crate) fn decode_bit_allocation(
    range_decoder: &mut Decoder<'_>,
    layout: AllocationLayout,
    is_transient: bool,
    total_bits: i32,
) -> Result<(BandAllocation, i32)> {
    let AllocationLayout { start, end, channels, lm } = layout;
    let cap = init_caps(lm, channels);
    let (offsets, alloc_trim) =
        decode_band_boosts_and_trim(range_decoder, start, end, lm, channels, total_bits, &cap)?;

    let available = (total_bits << BITRES) - (range_decoder.tell_frac() as i32) - 1;
    let reserve_anti_collapse = is_transient
        && lm.log2() >= ANTI_COLLAPSE_MIN_LM
        && available >= (((lm.log2() as i32) + 2) << BITRES);
    let anti_collapse_rsv = if reserve_anti_collapse { ONE_BIT_Q3 } else { 0 };
    let budget = AllocationBudget {
        offsets: &offsets,
        cap: &cap,
        alloc_trim,
        total: available - anti_collapse_rsv,
    };
    let allocation = BandAllocation::compute(range_decoder, layout, budget)?;
    return Ok((allocation, anti_collapse_rsv));
}

#[cfg(test)]
mod tests;
