//! CELT Full-Bandwidth MDCT Audio Layer ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)).
//!
//! This module implements the Constrained Energy Lapped Transform (CELT) layer of the Opus
//! audio codec ([RFC 6716 Section 4.3, Figure 17](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)):
//! ```text
//!                                      +--+
//!                                      |  |
//!                                      |  +------------+
//!                                      |               |
//!                                      v               |
//!    +---------+    +--------+    +---------+     +----+----+
//! -->|  Range  |--->| Decode |--->|  Shape  |---->| Stereo  |--+
//!  1 | Decoder | 2  | Pulses | 3  | Unquant |     |  Unmix  |  |
//!    +---------+    +--------+    +---------+     +---------+  |
//!         |                            ^               ^       |
//!         | 4                          | 5             | 6     | 7
//!         |      +----------------+    |               |       |
//!         +----->|Bit Allocation &|----+---------------+       |
//!         |      |    Parameters   |                            |
//!         |      +----------------+                            v
//!         |                                                 /-----\
//!         | 8    +----------------+    +--------------+     |  x  |---+
//!         +----->| Decode  Coarse |--->| Decode Fine  |---->|     |   |
//!                |     Energy     | 9  |    Energy    | 10  \-----/   |
//!                +----------------+    +--------------+               |
//!                                                                     |
//!         +-----------------------------------------------------------+
//!         | 11
//!         v
//!    +---------+    +------------+    +---------------+
//!    | Inverse |--->| Windowing &|--->| Post-Filter & |-->
//!    |  MDCT   | 12 | Overlap-Add| 13 |  De-Emphasis  | 14
//!    +---------+    +------------+    +---------------+
//! ```
//!
//! Decomposed into focused submodules with direct RFC 6716 references:
//! - [`constant`]: Bark-scale band boundaries ([Table 52](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)),
//!   energy probability models and means, bit-allocation vectors, the MDCT window, and
//!   post-filter tap gains.
//! - [`error`]: CELT layer error type, converted into `symphonia_core::errors::Error`.
//! - [`energy`]: Coarse, fine, and final energy envelope decoding
//!   ([RFC 6716 Section 4.3.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2>)).
//! - [`rate`]: Time-frequency change, spreading, and post-filter parameter decoding, band boosts
//!   and allocation trim, bit allocation, and bits-to-pulses conversion
//!   ([RFC 6716 Section 4.3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.1>),
//!   [Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>),
//!   [Section 4.3.4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.1>)).
//! - [`cwrs`]: PVQ codeword index decoding
//!   ([RFC 6716 Section 4.3.4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.2>)).
//! - [`vq`]: PVQ shape decoding, spreading rotation, split decoding with stereo unmixing,
//!   spectral folding, time-frequency resolution, and anti-collapse processing
//!   ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>) -
//!   [Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)).
//! - [`fft`]: Mixed-radix complex FFT kernel of the inverse MDCT
//!   ([RFC 6716 Section 4.3.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7>)).
//! - [`mdct`]: Inverse MDCT with TDAC windowing and overlap-add
//!   ([RFC 6716 Section 4.3.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7>)).
//! - [`synthesis`]: Band denormalisation and de-emphasis
//!   ([RFC 6716 Section 4.3.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.6>),
//!   [Section 4.3.7.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.2>)).
//! - [`postfilter`]: Pitch comb post-filter
//!   ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
//! - [`plc`]: LPC analysis, FIR/IIR filtering, and pitch search for packet loss concealment
//!   ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
//! - [`decoder`]: Frame-level decoder state, Hybrid mode start band, and concealment
//!   orchestration ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>),
//!   [Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).

pub(crate) mod constant;
pub(crate) mod cwrs;
pub(crate) mod decoder;
pub(crate) mod energy;
pub(crate) mod error;
mod fft;
pub(crate) mod mdct;
pub(crate) mod plc;
pub(crate) mod postfilter;
pub(crate) mod rate;
pub(crate) mod synthesis;
pub(crate) mod vq;

use crate::celt::error::Error;
use crate::toc::Bandwidth;
use symphonia_core::errors::Result;

pub(crate) use decoder::Decoder;

/// Number of Bark-scale frequency bands in the 48 kHz CELT mode (RFC 6716 Section 4.3).
pub(crate) const MAX_BANDS: usize = 21;

/// Base short MDCT block size at 48 kHz (2.5 ms = 120 samples, `LM = 0`).
pub(crate) const SHORT_MDCT_SIZE: usize = 120;

/// Maximum log2 multiple of the 2.5 ms short block (`LM in 0..=3`, up to 20 ms = 960 samples).
pub(crate) const MAX_LM: usize = 3;

/// Maximum number of samples per channel in a single CELT frame at 48 kHz (20 ms = 960 samples).
pub(crate) const MAX_FRAME_SIZE: usize = SHORT_MDCT_SIZE << MAX_LM;

/// Size of a CELT frame as a power-of-two multiple of the 2.5 ms short block
/// ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)).
///
/// The RFC writes this exponent `LM`: a frame holds `2^LM` short blocks of 120 samples at 48 kHz,
/// every band is `2^LM` times as wide as in the 2.5 ms frame, and every per-frame table is
/// indexed by `LM`.
///
/// ```text
///   LM   frame     short blocks   samples at 48 kHz
///   0    2.5 ms    1              120
///   1    5 ms      2              240
///   2    10 ms     4              480
///   3    20 ms     8              960
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct BlockSize(u8);

impl BlockSize {
    /// The 2.5 ms frame (`LM = 0`), the unit every band width is defined in.
    pub(crate) const SHORT: Self = Self(0);

    /// Builds a block size from the exponent `LM` (`0..=3`). Panics on a larger value, which can
    /// only come from a programming error since bitstream input goes through
    /// [`BlockSize::from_frame_size_48k`].
    pub(crate) const fn new(log2: usize) -> Self {
        assert!(log2 <= MAX_LM, "CELT block-size exponent out of range");
        return Self(log2 as u8);
    }

    /// Derives the block size from a 48 kHz frame length (`120`, `240`, `480` or `960` samples).
    pub(crate) fn from_frame_size_48k(frame_size_48k: usize) -> Result<Self> {
        return match frame_size_48k {
            120 => Ok(Self::new(0)),
            240 => Ok(Self::new(1)),
            480 => Ok(Self::new(2)),
            960 => Ok(Self::new(3)),
            _ => Err(Error::InvalidMdctSize(frame_size_48k).into()),
        };
    }

    /// The exponent `LM`, used to index per-frame-size tables.
    #[inline]
    pub(crate) const fn log2(self) -> usize {
        return self.0 as usize;
    }

    /// Number of 2.5 ms short blocks in the frame (`2^LM`).
    #[inline]
    pub(crate) const fn blocks(self) -> usize {
        return 1 << self.0;
    }

    /// Frame length in samples at 48 kHz (`120 * 2^LM`).
    #[inline]
    pub(crate) const fn frame_samples(self) -> usize {
        return SHORT_MDCT_SIZE << self.0;
    }

    /// Scales a count given in 2.5 ms units (a band width, a bin offset) to this frame size.
    #[inline]
    pub(crate) const fn scale(self, short_block_units: usize) -> usize {
        return short_block_units << self.0;
    }
}

/// MDCT window overlap length at 48 kHz (2.5 ms = 120 samples).
pub(crate) const OVERLAP: usize = 120;

/// A two-byte CELT frame whose first symbol always decodes as `silence = 1`: an all-ones range
/// coder input lands on the improbable symbol of the `{32767, 1}/32768` silence PDF
/// ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>),
/// Table 56). Decoding it flushes the overlap buffer without adding new signal.
pub(crate) const SILENCE_FRAME: [u8; 2] = [0xFF, 0xFF];

/// Shortest pitch period `T` of the comb post-filter (`15`, the smallest coded period)
/// ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
pub(crate) const MIN_PITCH_PERIOD: usize = 15;

/// Samples of decoded history that the comb post-filter and pitch concealment read (`1024`):
/// the longest coded pitch period, `1022`, plus the two outer comb taps
/// ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
pub(crate) const PITCH_HISTORY_LEN: usize = 1024;

/// Per-channel synthesis history buffer length (`DECODE_BUFFER_SIZE = 2048` samples).
pub(crate) const DECODE_BUFFER_SIZE: usize = 2048;

/// Fractional bit resolution shift (`1 << BITRES = 8` eighths of a bit).
pub(crate) const BITRES: u32 = 3;

/// Starting band index when CELT operates as the high-frequency layer in Hybrid mode (8 kHz).
pub(crate) const HYBRID_START_BAND: usize = 17;

/// Maximum number of channels in a single coupled/uncoupled CELT stream (mono = 1, stereo = 2).
pub(crate) const MAX_CHANNELS: usize = 2;

/// PVQ spreading rotation decision
/// ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum Spread {
    None = 0,
    Light = 1,
    Normal = 2,
    Aggressive = 3,
}

impl TryFrom<u32> for Spread {
    type Error = Error;

    /// Converts a decoded `spread` symbol (`0..=3`, Table 59) into a [`Spread`] mode
    /// ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)).
    fn try_from(symbol: u32) -> std::result::Result<Self, Error> {
        return match symbol {
            0 => Ok(Self::None),
            1 => Ok(Self::Light),
            2 => Ok(Self::Normal),
            3 => Ok(Self::Aggressive),
            _ => Err(Error::InvalidSpread(symbol)),
        };
    }
}

/// Returns the end band (one past the last coded band, `13..=21`) of an audio [`Bandwidth`]
/// ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>),
/// Table 55).
#[inline]
pub(crate) const fn end_band_for(bandwidth: Bandwidth) -> usize {
    return match bandwidth {
        Bandwidth::Narrowband => 13,
        Bandwidth::MediumBand | Bandwidth::Wideband => 17,
        Bandwidth::SuperWideband => 19,
        Bandwidth::Fullband => 21,
    };
}

/// Multiplier of the spreading and noise-fill linear congruential generator
/// ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)).
const SPREADING_LCG_MULTIPLIER: u32 = 1_664_525;

/// Increment of the spreading and noise-fill linear congruential generator.
const SPREADING_LCG_INCREMENT: u32 = 1_013_904_223;

/// Advances the linear congruential generator used for spectral folding, anti-collapse and
/// noise concealment
/// ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)).
#[inline(always)]
pub(crate) const fn lcg_next(seed: u32) -> u32 {
    return seed.wrapping_mul(SPREADING_LCG_MULTIPLIER).wrapping_add(SPREADING_LCG_INCREMENT);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_try_from_rejects_out_of_range_symbols() {
        assert_eq!(Spread::try_from(0), Ok(Spread::None));
        assert_eq!(Spread::try_from(3), Ok(Spread::Aggressive));
        assert_eq!(Spread::try_from(4), Err(Error::InvalidSpread(4)));
        return;
    }
}
