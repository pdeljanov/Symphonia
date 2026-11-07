//! CELT Full-Bandwidth MDCT Audio Layer
//!
//! This module implements the CELT transform layer of the Opus audio codec
//! (RFC 6716 Section 4.3), decomposed into focused submodules:
//! - [`constant`]: Bark-scale frequency band boundaries, coarse/fine energy tables,
//!   bit-allocation curves, windowing coefficients, and post-filter tapsets.
//! - [`energy`]: Coarse energy (Laplace + 2D prediction), fine energy, and final energy bits.
//! - [`rate`]: Dynamic bit allocation (`clt_compute_allocation`), band boost, trim,
//!   intensity stereo, and dual stereo bit splitting.
//! - [`cwrs`]: Combinatorial Waveform / Pyramid Vector Quantizer index decoding (`decode_pulses`).
//! - [`vq`]: PVQ algebraic shape unquantization (`alg_unquant`), spreading rotations
//!   (`exp_rotation`), pitch folding, stereo Mid-Side / Intensity unmixing, and anti-collapse.
//! - [`mdct`]: Inverse Modified Discrete Cosine Transform (`IMDCT`) for block sizes
//!   120, 240, 480, and 960, Vorbis-like windowing, overlap-add, comb post-filter, and de-emphasis.
//! - [`decoder`]: Frame-level CELT decoder state, Hybrid mode (`start_band = 17`) support,
//!   and CELT Packet Loss Concealment (PLC).

pub(crate) mod constant;
pub(crate) mod cwrs;
pub(crate) mod decoder;
pub(crate) mod energy;
pub(crate) mod error;
pub(crate) mod mdct;
pub(crate) mod rate;
pub(crate) mod vq;

use crate::celt::error::Error;
use crate::toc::{Bandwidth, FrameDuration};

pub use decoder::Decoder;

/// Number of Bark-scale frequency bands in the 48 kHz CELT mode (RFC 6716 Section 4.3).
pub const MAX_BANDS: usize = 21;

/// Base short MDCT block size at 48 kHz (2.5 ms = 120 samples, `LM = 0`).
pub const SHORT_MDCT_SIZE: usize = 120;

/// Maximum log2 multiple of the 2.5 ms short block (`LM in 0..=3`, up to 20 ms = 960 samples).
pub const MAX_LM: usize = 3;

/// Maximum number of samples per channel in a single CELT frame at 48 kHz (20 ms = 960 samples).
pub const MAX_FRAME_SIZE: usize = SHORT_MDCT_SIZE << MAX_LM;

/// MDCT window overlap length at 48 kHz (2.5 ms = 120 samples).
pub const OVERLAP: usize = 120;

/// Minimum pitch period for the CELT comb post-filter (`COMBFILTER_MINPERIOD`).
pub const COMBFILTER_MINPERIOD: usize = 15;

/// Maximum pitch period for the CELT comb post-filter (`COMBFILTER_MAXPERIOD`).
pub const COMBFILTER_MAXPERIOD: usize = 1024;

/// Per-channel synthesis history buffer length (`DECODE_BUFFER_SIZE = 2048` samples).
pub const DECODE_BUFFER_SIZE: usize = 2048;

/// Fractional bit resolution shift (`1 << BITRES = 8` eighths of a bit).
pub const BITRES: u32 = 3;

/// Starting band index when CELT operates as the high-frequency layer in Hybrid mode (8 kHz).
pub const HYBRID_START_BAND: usize = 17;

/// Maximum number of channels in a single coupled/uncoupled CELT stream (mono = 1, stereo = 2).
pub const MAX_CHANNELS: usize = 2;

/// Number of past frames tracked for pitch/PLC/anti-collapse state.
pub const LOSS_HIST_FRAMES: usize = 2;

/// PVQ spreading rotation decision (RFC 6716 Section 4.3.4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Spread {
    None = 0,
    Light = 1,
    Normal = 2,
    Aggressive = 3,
}

impl Spread {
    /// Converts a decoded integer symbol `0..=3` into a [`Spread`] mode.
    #[inline]
    pub const fn from_symbol(symbol: u32) -> Self {
        return match symbol {
            0 => Self::None,
            1 => Self::Light,
            2 => Self::Normal,
            _ => Self::Aggressive,
        };
    }
}

/// Returns the effective upper band boundary `end` (`1..=21`) for the given audio [`Bandwidth`]
/// (RFC 6716 Section 4.3, Table 52).
#[inline]
pub const fn effective_ebands(bandwidth: Bandwidth) -> usize {
    return match bandwidth {
        Bandwidth::NarrowBand => 13,
        Bandwidth::MediumBand | Bandwidth::WideBand => 17,
        Bandwidth::SuperWideBand => 19,
        Bandwidth::FullBand => 21,
    };
}

/// Returns the CELT block size exponent `LM` (`0..=3`) for a given [`FrameDuration`],
/// where `N = 120 << LM` samples at 48 kHz.
#[inline]
pub const fn frame_duration_to_lm(duration: FrameDuration) -> Result<usize, Error> {
    return match duration {
        FrameDuration::Ms2_5 => Ok(0),
        FrameDuration::Ms5 => Ok(1),
        FrameDuration::Ms10 => Ok(2),
        FrameDuration::Ms20 => Ok(3),
        FrameDuration::Ms40 | FrameDuration::Ms60 => Err(Error::UnsupportedFrameDuration),
    };
}

/// Linear congruential generator used by CELT for spectral folding and noise injection
/// (`celt_lcg_rand`, RFC 6716 Section 4.3.4.3).
#[inline(always)]
pub const fn celt_lcg_rand(seed: u32) -> u32 {
    return seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
}
