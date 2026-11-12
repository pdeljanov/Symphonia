//! CELT static tables and codebooks
//! ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)).
//!
//! Bark-scale band boundaries, coarse energy prediction coefficients and Laplace models, the
//! static bit-allocation table, the pulse cache, time-frequency selection tables, comb post-filter
//! tapsets and the 120-sample low-overlap MDCT window of the 48 kHz CELT mode.

#![expect(
    clippy::excessive_precision,
    reason = "E_MEANS, COMB_FILTER_GAINS and CELT_WINDOW keep reference digits beyond f32 precision"
)]
// `CACHE_BITS` and `CELT_WINDOW` exceed the 256-byte threshold; both are lookup tables read by index.

use crate::celt::BlockSize;
use crate::entropy::Cdf;

pub(crate) use crate::celt::{BITRES, MAX_BANDS, MAX_CHANNELS, MAX_LM, OVERLAP, Spread};

/// Number of quality rows in [`ALLOC_VECTORS`]
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>), Table 57).
pub(crate) const NB_ALLOC_VECTORS: usize = 11;

/// Log2 of the interpolation resolution between two allocation rows: the interpolation weight
/// runs in steps of `1/64`
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
pub(crate) const ALLOC_STEPS: usize = 6;

/// Largest pseudo-pulse index of any pulse cache sub-table
/// ([RFC 6716 Section 4.3.4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.1>)).
pub(crate) const MAX_PSEUDO: usize = 40;

/// Maximum number of fine energy bits per band and channel
/// ([RFC 6716 Section 4.3.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.2>)).
pub(crate) const MAX_FINE_BITS: i32 = 8;

/// Offset subtracted from the fine energy share of a band, in `1/8` bit units per degree of
/// freedom ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>),
/// fine-energy versus shape split).
pub(crate) const FINE_OFFSET: i32 = 21;

/// Offset of the split-angle (`theta`) bit allocation
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
pub(crate) const QTHETA_OFFSET: i32 = 4;

/// Offset of the two-phase split-angle allocation of stereo `N = 2` bands
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
pub(crate) const QTHETA_OFFSET_TWOPHASE: i32 = 16;

/// Pre-emphasis and de-emphasis filter coefficient at 48 kHz (`0.8500061035`, `27853 / 32768`
/// in Q15; [RFC 6716 Section 4.3.7.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.2>)).
pub(crate) const PREEMPH_COEF: f32 = 0.85000610f32;

/// Bark-scale band boundaries in 2.5 ms frame bins (`120`-sample short block) for the 48 kHz
/// CELT mode ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>),
/// Table 55).
///
/// ```text
/// +------+--------+-----------+------+--------+-----------+
/// | Band | Offset | Width (N) | Band | Offset | Width (N) |
/// +------+--------+-----------+------+--------+-----------+
/// |    0 |      0 |         1 |   11 |     14 |         2 |
/// |    1 |      1 |         1 |   12 |     16 |         4 |
/// |    2 |      2 |         1 |   13 |     20 |         4 |
/// |    3 |      3 |         1 |   14 |     24 |         4 |
/// |    4 |      4 |         1 |   15 |     28 |         6 |
/// |    5 |      5 |         1 |   16 |     34 |         6 |
/// |    6 |      6 |         1 |   17 |     40 |         8 |
/// |    7 |      7 |         1 |   18 |     48 |        12 |
/// |    8 |      8 |         2 |   19 |     60 |        18 |
/// |    9 |     10 |         2 |   20 |     78 |        22 |
/// |   10 |     12 |         2 | (21) |    100 |         - |
/// +------+--------+-----------+------+--------+-----------+
/// ```
pub(crate) const EBANDS: [usize; MAX_BANDS + 1] =
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 34, 40, 48, 60, 78, 100];

/// Returns the number of MDCT bins of `band` (`0..21`) at block size `lm`
/// ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>), Table 55).
#[inline]
pub(crate) const fn band_width(band: usize, lm: BlockSize) -> usize {
    return lm.scale(EBANDS[band + 1] - EBANDS[band]);
}

/// Base-2 logarithm of each band's 2.5 ms width `N0 = EBANDS[i + 1] - EBANDS[i]` in `1/8` bit
/// units, used by the fine-energy versus shape split
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
pub(crate) const LOGN: [i16; MAX_BANDS] =
    [0, 0, 0, 0, 0, 0, 0, 0, 8, 8, 8, 8, 16, 16, 16, 21, 21, 24, 29, 34, 36];

/// Static allocation table, one row per quality `q` (`0..=10`) and one column per band, in
/// `1/32` bit per MDCT bin
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>),
/// Table 57; the RFC prints the transpose, bands as rows).
///
/// The static allocation of band `i` at quality `q` is
/// `channels * N0[i] * ALLOC_VECTORS[q][i] << LM >> 2` eighth bits; the decoder interpolates
/// between the two rows that bracket the available budget.
pub(crate) const ALLOC_VECTORS: [[u8; MAX_BANDS]; NB_ALLOC_VECTORS] = [
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [90, 80, 75, 69, 63, 56, 49, 40, 34, 29, 20, 18, 10, 0, 0, 0, 0, 0, 0, 0, 0],
    [110, 100, 90, 84, 78, 71, 65, 58, 51, 45, 39, 32, 26, 20, 12, 0, 0, 0, 0, 0, 0],
    [118, 110, 103, 93, 86, 80, 75, 70, 65, 59, 53, 47, 40, 31, 23, 15, 4, 0, 0, 0, 0],
    [126, 119, 112, 104, 95, 89, 83, 78, 72, 66, 60, 54, 47, 39, 32, 25, 17, 12, 1, 0, 0],
    [134, 127, 120, 114, 103, 97, 91, 85, 78, 72, 66, 60, 54, 47, 41, 35, 29, 23, 16, 10, 1],
    [144, 137, 130, 124, 113, 107, 101, 95, 88, 82, 76, 70, 64, 57, 51, 45, 39, 33, 26, 15, 1],
    [152, 145, 138, 132, 123, 117, 111, 105, 98, 92, 86, 80, 74, 67, 61, 55, 49, 43, 36, 20, 1],
    [162, 155, 148, 142, 133, 127, 121, 115, 108, 102, 96, 90, 84, 77, 71, 65, 59, 53, 46, 30, 1],
    [
        172, 165, 158, 152, 143, 137, 131, 125, 118, 112, 106, 100, 94, 87, 81, 75, 69, 63, 56, 45,
        20,
    ],
    [
        200, 200, 200, 200, 200, 200, 200, 200, 198, 193, 188, 183, 178, 173, 168, 163, 158, 153,
        148, 129, 104,
    ],
];

/// Number of pulse cache levels: the block sizes `LM = 0..=3` plus the level below the shortest
/// block that recursive partition splitting reaches
/// ([RFC 6716 Section 4.3.4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.1>),
/// [Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
pub(crate) const CACHE_LEVELS: usize = MAX_LM + 2;

/// Start of each pulse cache sub-table inside [`CACHE_BITS`], indexed `[level][band]` where
/// `level = LM + 1` (`0..CACHE_LEVELS`); `-1` marks the single-bin bands that are never split
/// ([RFC 6716 Section 4.3.4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.1>)).
pub(crate) const CACHE_INDEX: [[i16; MAX_BANDS]; CACHE_LEVELS] = [
    // LM = -1: halves of the 2.5 ms bands
    [-1, -1, -1, -1, -1, -1, -1, -1, 0, 0, 0, 0, 41, 41, 41, 82, 82, 123, 164, 200, 222],
    // LM = 0
    [0, 0, 0, 0, 0, 0, 0, 0, 41, 41, 41, 41, 123, 123, 123, 164, 164, 240, 266, 283, 295],
    // LM = 1
    [
        41, 41, 41, 41, 41, 41, 41, 41, 123, 123, 123, 123, 240, 240, 240, 266, 266, 305, 318, 328,
        336,
    ],
    // LM = 2
    [
        123, 123, 123, 123, 123, 123, 123, 123, 240, 240, 240, 240, 305, 305, 305, 318, 318, 343,
        351, 358, 364,
    ],
    // LM = 3
    [
        240, 240, 240, 240, 240, 240, 240, 240, 305, 305, 305, 305, 343, 343, 343, 351, 351, 370,
        376, 382, 387,
    ],
];

/// Pulse cache: the bit cost of every permitted pulse count per partition size
/// ([RFC 6716 Section 4.3.4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.1>)).
///
/// The sub-table of a band starts at `CACHE_INDEX[level][band]`; its first entry is the largest
/// pseudo-pulse index `max_pseudo`, followed by `max_pseudo` non-decreasing entries holding
/// `required_eighth_bits - 1` for pseudo-pulse indices `1..=max_pseudo`.
pub(crate) static CACHE_BITS: [u8; 392] = [
    40, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
    7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 40, 15, 23, 28, 31, 34, 36, 38, 39, 41, 42, 43, 44, 45, 46, 47,
    47, 49, 50, 51, 52, 53, 54, 55, 55, 57, 58, 59, 60, 61, 62, 63, 63, 65, 66, 67, 68, 69, 70, 71,
    71, 40, 20, 33, 41, 48, 53, 57, 61, 64, 66, 69, 71, 73, 75, 76, 78, 80, 82, 85, 87, 89, 91, 92,
    94, 96, 98, 101, 103, 105, 107, 108, 110, 112, 114, 117, 119, 121, 123, 124, 126, 128, 40, 23,
    39, 51, 60, 67, 73, 79, 83, 87, 91, 94, 97, 100, 102, 105, 107, 111, 115, 118, 121, 124, 126,
    129, 131, 135, 139, 142, 145, 148, 150, 153, 155, 159, 163, 166, 169, 172, 174, 177, 179, 35,
    28, 49, 65, 78, 89, 99, 107, 114, 120, 126, 132, 136, 141, 145, 149, 153, 159, 165, 171, 176,
    180, 185, 189, 192, 199, 205, 211, 216, 220, 225, 229, 232, 239, 245, 251, 21, 33, 58, 79, 97,
    112, 125, 137, 148, 157, 166, 174, 182, 189, 195, 201, 207, 217, 227, 235, 243, 251, 17, 35,
    63, 86, 106, 123, 139, 152, 165, 177, 187, 197, 206, 214, 222, 230, 237, 250, 25, 31, 55, 75,
    91, 105, 117, 128, 138, 146, 154, 161, 168, 174, 180, 185, 190, 200, 208, 215, 222, 229, 235,
    240, 245, 255, 16, 36, 65, 89, 110, 128, 144, 159, 173, 185, 196, 207, 217, 226, 234, 242, 250,
    11, 41, 74, 103, 128, 151, 172, 191, 209, 225, 241, 255, 9, 43, 79, 110, 138, 163, 186, 207,
    227, 246, 12, 39, 71, 99, 123, 144, 164, 182, 198, 214, 228, 241, 253, 9, 44, 81, 113, 142,
    168, 192, 214, 235, 255, 7, 49, 90, 127, 160, 191, 220, 247, 6, 51, 95, 134, 170, 203, 234, 7,
    47, 87, 123, 155, 184, 212, 237, 6, 52, 97, 137, 174, 208, 240, 5, 57, 106, 151, 192, 231, 5,
    59, 111, 158, 202, 243, 5, 55, 103, 147, 187, 224, 5, 60, 113, 161, 206, 248, 4, 65, 122, 175,
    224, 4, 67, 127, 182, 234,
];

/// Per-band maximum allocation in bits per bin, offset by `-64`, indexed
/// `[lm][channels - 1][band]`; the cap of a band is `(CACHE_CAPS + 64) * channels * N / 4`
/// eighth bits ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
pub(crate) const CACHE_CAPS: [[[u8; MAX_BANDS]; MAX_CHANNELS]; MAX_LM + 1] = [
    // LM = 0
    [
        [
            224, 224, 224, 224, 224, 224, 224, 224, 160, 160, 160, 160, 185, 185, 185, 178, 178,
            168, 134, 61, 37,
        ],
        [
            224, 224, 224, 224, 224, 224, 224, 224, 240, 240, 240, 240, 207, 207, 207, 198, 198,
            183, 144, 66, 40,
        ],
    ],
    // LM = 1
    [
        [
            160, 160, 160, 160, 160, 160, 160, 160, 185, 185, 185, 185, 193, 193, 193, 183, 183,
            172, 138, 64, 38,
        ],
        [
            240, 240, 240, 240, 240, 240, 240, 240, 207, 207, 207, 207, 204, 204, 204, 193, 193,
            180, 143, 66, 40,
        ],
    ],
    // LM = 2
    [
        [
            185, 185, 185, 185, 185, 185, 185, 185, 193, 193, 193, 193, 193, 193, 193, 183, 183,
            172, 138, 65, 39,
        ],
        [
            207, 207, 207, 207, 207, 207, 207, 207, 204, 204, 204, 204, 201, 201, 201, 188, 188,
            176, 141, 66, 40,
        ],
    ],
    // LM = 3
    [
        [
            193, 193, 193, 193, 193, 193, 193, 193, 193, 193, 193, 193, 194, 194, 194, 184, 184,
            173, 139, 65, 39,
        ],
        [
            204, 204, 204, 204, 204, 204, 204, 204, 201, 201, 201, 201, 198, 198, 198, 187, 187,
            175, 140, 66, 40,
        ],
    ],
];

/// Conservative `log2` of a band count in `1/8` bit units, the reservation for the intensity
/// stereo band index
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
pub(crate) const LOG2_FRAC_TABLE: [u8; 24] =
    [0, 8, 13, 16, 19, 21, 23, 24, 26, 27, 28, 29, 30, 31, 32, 32, 33, 34, 34, 35, 36, 36, 37, 37];

/// Mean log2 energy of each band in 6 dB units, the offset added back by denormalization
/// ([RFC 6716 Section 4.3.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2>),
/// [Section 4.3.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.6>)).
pub(crate) const E_MEANS: [f32; MAX_BANDS] = [
    6.4375, 6.25, 5.75, 5.3125, 5.0625, 4.8125, 4.5, 4.375, 4.875, 4.6875, 4.5625, 4.4375, 4.875,
    4.625, 4.3125, 4.5, 4.375, 4.625, 4.75, 4.4375, 3.75,
];

/// Inter-frame coarse energy prediction coefficient `alpha` per block size `LM` (`0..=3`),
/// `[0.9, 0.8, 0.65, 0.5]` rounded to Q15
/// ([RFC 6716 Section 4.3.2.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.1>),
/// values from Appendix A).
pub(crate) const PRED_COEF: [f32; MAX_LM + 1] =
    [29440.0 / 32768.0, 26112.0 / 32768.0, 21248.0 / 32768.0, 16384.0 / 32768.0];

/// Inter-frame cross-band prediction coefficient `beta` per block size `LM` (`0..=3`) in Q15
/// ([RFC 6716 Section 4.3.2.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.1>),
/// values from Appendix A).
pub(crate) const BETA_COEF: [f32; MAX_LM + 1] =
    [30147.0 / 32768.0, 22282.0 / 32768.0, 12124.0 / 32768.0, 6554.0 / 32768.0];

/// Intra-frame cross-band prediction coefficient `beta = 4915 / 32768`
/// ([RFC 6716 Section 4.3.2.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.1>)).
pub(crate) const BETA_INTRA: f32 = 4915.0 / 32768.0;

/// Margin subtracted from every band energy after a loss, per block size exponent: short frames
/// fluctuate most (`1.5` for 2.5 ms, `0.5` for 5 ms, none from 10 ms).
pub(crate) const LOSS_SAFETY_MARGIN: [f32; MAX_LM + 1] = [1.5, 0.5, 0.0, 0.0];

/// Scale from the 8-bit stored zero-residual probability to the Q15 total of the Laplace decoder.
const LAPLACE_ZERO_FREQ_SHIFT: u32 = 7;

/// Scale from the 8-bit stored decay to the Q14 decay of the Laplace decoder.
const LAPLACE_DECAY_SHIFT: u32 = 6;

/// Laplace distribution of the coarse energy prediction residual of one band
/// ([RFC 6716 Section 4.3.2.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.1>)):
/// the probability of a zero residual and the geometric decay of the probability per step away
/// from zero, both stored in 8 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LaplaceModel {
    /// Probability of a zero residual in `1 / 256` of the Q15 total.
    pub(crate) zero_freq: u8,
    /// Decay per unit step in `1 / 256` of the Q14 unit.
    pub(crate) decay: u8,
}

impl LaplaceModel {
    /// Builds a model from its two stored 8-bit parameters.
    pub(crate) const fn new(zero_freq: u8, decay: u8) -> Self {
        return Self { zero_freq, decay };
    }

    /// Probability of a zero residual in Q15, as the range decoder's Laplace decoder expects.
    #[inline]
    pub(crate) fn zero_freq_q15(self) -> u32 {
        return u32::from(self.zero_freq) << LAPLACE_ZERO_FREQ_SHIFT;
    }

    /// Decay of the residual probability in Q14.
    #[inline]
    pub(crate) fn decay_q14(self) -> i32 {
        return i32::from(self.decay) << LAPLACE_DECAY_SHIFT;
    }
}

/// Laplace models of the coarse energy residual indexed `[lm][intra][band]`, where `intra` is
/// `0` for inter-frame and `1` for intra-frame prediction
/// ([RFC 6716 Section 4.3.2.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.1>),
/// values from Appendix A).
pub(crate) static E_PROB_MODEL: [[[LaplaceModel; MAX_BANDS]; 2]; MAX_LM + 1] = [
    [
        [
            LaplaceModel::new(72, 127),
            LaplaceModel::new(65, 129),
            LaplaceModel::new(66, 128),
            LaplaceModel::new(65, 128),
            LaplaceModel::new(64, 128),
            LaplaceModel::new(62, 128),
            LaplaceModel::new(64, 128),
            LaplaceModel::new(64, 128),
            LaplaceModel::new(92, 78),
            LaplaceModel::new(92, 79),
            LaplaceModel::new(92, 78),
            LaplaceModel::new(90, 79),
            LaplaceModel::new(116, 41),
            LaplaceModel::new(115, 40),
            LaplaceModel::new(114, 40),
            LaplaceModel::new(132, 26),
            LaplaceModel::new(132, 26),
            LaplaceModel::new(145, 17),
            LaplaceModel::new(161, 12),
            LaplaceModel::new(176, 10),
            LaplaceModel::new(177, 11),
        ],
        // LM = 0 (2.5 ms), inter
        [
            LaplaceModel::new(24, 179),
            LaplaceModel::new(48, 138),
            LaplaceModel::new(54, 135),
            LaplaceModel::new(54, 132),
            LaplaceModel::new(53, 134),
            LaplaceModel::new(56, 133),
            LaplaceModel::new(55, 132),
            LaplaceModel::new(55, 132),
            LaplaceModel::new(61, 114),
            LaplaceModel::new(70, 96),
            LaplaceModel::new(74, 88),
            LaplaceModel::new(75, 88),
            LaplaceModel::new(87, 74),
            LaplaceModel::new(89, 66),
            LaplaceModel::new(91, 67),
            LaplaceModel::new(100, 59),
            LaplaceModel::new(108, 50),
            LaplaceModel::new(120, 40),
            LaplaceModel::new(122, 37),
            LaplaceModel::new(97, 43),
            LaplaceModel::new(78, 50),
        ],
    ],
    [
        // intra
        [
            LaplaceModel::new(83, 78),
            LaplaceModel::new(84, 81),
            LaplaceModel::new(88, 75),
            LaplaceModel::new(86, 74),
            LaplaceModel::new(87, 71),
            LaplaceModel::new(90, 73),
            LaplaceModel::new(93, 74),
            LaplaceModel::new(93, 74),
            LaplaceModel::new(109, 40),
            LaplaceModel::new(114, 36),
            LaplaceModel::new(117, 34),
            LaplaceModel::new(117, 34),
            LaplaceModel::new(143, 17),
            LaplaceModel::new(145, 18),
            LaplaceModel::new(146, 19),
            LaplaceModel::new(162, 12),
            LaplaceModel::new(165, 10),
            LaplaceModel::new(178, 7),
            LaplaceModel::new(189, 6),
            LaplaceModel::new(190, 8),
            LaplaceModel::new(177, 9),
        ],
        // LM = 1 (5 ms), inter
        [
            LaplaceModel::new(23, 178),
            LaplaceModel::new(54, 115),
            LaplaceModel::new(63, 102),
            LaplaceModel::new(66, 98),
            LaplaceModel::new(69, 99),
            LaplaceModel::new(74, 89),
            LaplaceModel::new(71, 91),
            LaplaceModel::new(73, 91),
            LaplaceModel::new(78, 89),
            LaplaceModel::new(86, 80),
            LaplaceModel::new(92, 66),
            LaplaceModel::new(93, 64),
            LaplaceModel::new(102, 59),
            LaplaceModel::new(103, 60),
            LaplaceModel::new(104, 60),
            LaplaceModel::new(117, 52),
            LaplaceModel::new(123, 44),
            LaplaceModel::new(138, 35),
            LaplaceModel::new(133, 31),
            LaplaceModel::new(97, 38),
            LaplaceModel::new(77, 45),
        ],
    ],
    [
        // intra
        [
            LaplaceModel::new(61, 90),
            LaplaceModel::new(93, 60),
            LaplaceModel::new(105, 42),
            LaplaceModel::new(107, 41),
            LaplaceModel::new(110, 45),
            LaplaceModel::new(116, 38),
            LaplaceModel::new(113, 38),
            LaplaceModel::new(112, 38),
            LaplaceModel::new(124, 26),
            LaplaceModel::new(132, 27),
            LaplaceModel::new(136, 19),
            LaplaceModel::new(140, 20),
            LaplaceModel::new(155, 14),
            LaplaceModel::new(159, 16),
            LaplaceModel::new(158, 18),
            LaplaceModel::new(170, 13),
            LaplaceModel::new(177, 10),
            LaplaceModel::new(187, 8),
            LaplaceModel::new(192, 6),
            LaplaceModel::new(175, 9),
            LaplaceModel::new(159, 10),
        ],
        // LM = 2 (10 ms), inter
        [
            LaplaceModel::new(21, 178),
            LaplaceModel::new(59, 110),
            LaplaceModel::new(71, 86),
            LaplaceModel::new(75, 85),
            LaplaceModel::new(84, 83),
            LaplaceModel::new(91, 66),
            LaplaceModel::new(88, 73),
            LaplaceModel::new(87, 72),
            LaplaceModel::new(92, 75),
            LaplaceModel::new(98, 72),
            LaplaceModel::new(105, 58),
            LaplaceModel::new(107, 54),
            LaplaceModel::new(115, 52),
            LaplaceModel::new(114, 55),
            LaplaceModel::new(112, 56),
            LaplaceModel::new(129, 51),
            LaplaceModel::new(132, 40),
            LaplaceModel::new(150, 33),
            LaplaceModel::new(140, 29),
            LaplaceModel::new(98, 35),
            LaplaceModel::new(77, 42),
        ],
    ],
    [
        // intra
        [
            LaplaceModel::new(42, 121),
            LaplaceModel::new(96, 66),
            LaplaceModel::new(108, 43),
            LaplaceModel::new(111, 40),
            LaplaceModel::new(117, 44),
            LaplaceModel::new(123, 32),
            LaplaceModel::new(120, 36),
            LaplaceModel::new(119, 33),
            LaplaceModel::new(127, 33),
            LaplaceModel::new(134, 34),
            LaplaceModel::new(139, 21),
            LaplaceModel::new(147, 23),
            LaplaceModel::new(152, 20),
            LaplaceModel::new(158, 25),
            LaplaceModel::new(154, 26),
            LaplaceModel::new(166, 21),
            LaplaceModel::new(173, 16),
            LaplaceModel::new(184, 13),
            LaplaceModel::new(184, 10),
            LaplaceModel::new(150, 13),
            LaplaceModel::new(139, 15),
        ],
        // LM = 3 (20 ms), inter
        [
            LaplaceModel::new(22, 178),
            LaplaceModel::new(63, 114),
            LaplaceModel::new(74, 82),
            LaplaceModel::new(84, 83),
            LaplaceModel::new(92, 82),
            LaplaceModel::new(103, 62),
            LaplaceModel::new(96, 72),
            LaplaceModel::new(96, 67),
            LaplaceModel::new(101, 73),
            LaplaceModel::new(107, 72),
            LaplaceModel::new(113, 55),
            LaplaceModel::new(118, 52),
            LaplaceModel::new(125, 52),
            LaplaceModel::new(118, 52),
            LaplaceModel::new(117, 55),
            LaplaceModel::new(135, 49),
            LaplaceModel::new(137, 39),
            LaplaceModel::new(157, 32),
            LaplaceModel::new(145, 29),
            LaplaceModel::new(97, 33),
            LaplaceModel::new(77, 40),
        ],
    ],
];

/// Cumulative distribution of the low-budget coarse energy step `{0, -1, +1}`, PDF `{2, 1, 1}/4`
/// ([RFC 6716 Section 4.3.2.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.1>)).
pub(crate) const CDF_SMALL_ENERGY: Cdf<'static> = Cdf::new(&[4, 2, 3, 4]);

/// Coarse residuals of the [`CDF_SMALL_ENERGY`] symbols `0`, `1` and `2`
/// ([RFC 6716 Section 4.3.2.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.1>)).
pub(crate) const SMALL_ENERGY_RESIDUALS: [i32; 3] = [0, -1, 1];

const _: () = assert!(CDF_SMALL_ENERGY.symbols() == SMALL_ENERGY_RESIDUALS.len());

/// Cumulative distribution of the allocation trim (`0..=10`),
/// PDF `{2, 2, 5, 10, 22, 46, 22, 10, 5, 2, 2}/128`
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>), Table 58).
pub(crate) const CDF_TRIM: Cdf<'static> =
    Cdf::new(&[128, 2, 4, 9, 19, 41, 87, 109, 119, 124, 126, 128]);

/// Cumulative distribution of the PVQ spreading decision (`0..=3`), PDF `{7, 2, 21, 2}/32`
/// ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>), Table 59).
pub(crate) const CDF_SPREAD: Cdf<'static> = Cdf::new(&[32, 7, 9, 30, 32]);

/// Cumulative distribution of the comb post-filter tapset (`0..=2`), PDF `{2, 1, 1}/4`
/// ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
pub(crate) const CDF_TAPSET: Cdf<'static> = Cdf::new(&[4, 2, 3, 4]);

/// Time-frequency resolution adjustment indexed `[lm][is_transient][tf_select][tf_change]`
/// ([RFC 6716 Section 4.3.4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.5>),
/// Tables 60 to 63).
///
/// Positive values recombine short blocks (finer frequency resolution), negative values split
/// the band in time (finer time resolution).
///
/// ```text
/// +----+---------------------------------+---------------------------------+
/// |    | Non-transient frame             | Transient frame                 |
/// | LM | tf_select = 0  | tf_select = 1  | tf_select = 0  | tf_select = 1  |
/// |    | (Table 60)     | (Table 61)     | (Table 62)     | (Table 63)     |
/// |    | change 0 | 1   | change 0 | 1   | change 0 | 1   | change 0 | 1   |
/// +----+----------+-----+----------+-----+----------+-----+----------+-----+
/// |  0 |        0 |  -1 |        0 |  -1 |        0 |  -1 |        0 |  -1 |
/// |  1 |        0 |  -1 |        0 |  -2 |        1 |   0 |        1 |  -1 |
/// |  2 |        0 |  -2 |        0 |  -3 |        2 |   0 |        1 |  -1 |
/// |  3 |        0 |  -2 |        0 |  -3 |        3 |   0 |        1 |  -1 |
/// +----+----------+-----+----------+-----+----------+-----+----------+-----+
/// ```
pub(crate) const TF_SELECT_TABLE: [[[[i8; 2]; 2]; 2]; MAX_LM + 1] = [
    [[[0, -1], [0, -1]], [[0, -1], [0, -1]]],
    [[[0, -1], [0, -2]], [[1, 0], [1, -1]]],
    [[[0, -2], [0, -3]], [[2, 0], [1, -1]]],
    [[[0, -2], [0, -3]], [[3, 0], [1, -1]]],
];

/// Comb post-filter tap weights `[g0, g1, g2]` of the three tapsets
/// ([RFC 6716 Section 4.3.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1>)).
pub(crate) const COMB_FILTER_GAINS: [[f32; 3]; 3] = [
    [0.3066406250, 0.2170410156, 0.1296386719],
    [0.4638671875, 0.2680664062, 0.0],
    [0.7998046875, 0.1000976562, 0.0],
];

/// Collapse-mask bit interleaving used when Haar recombining doubles a band's block count
/// ([RFC 6716 Section 4.3.4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.5>),
/// [Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)).
pub(crate) const BIT_INTERLEAVE_TABLE: [u8; 16] = [0, 1, 1, 1, 2, 3, 3, 3, 2, 3, 3, 3, 2, 3, 3, 3];

/// Collapse-mask bit de-interleaving, the inverse of [`BIT_INTERLEAVE_TABLE`]
/// ([RFC 6716 Section 4.3.4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.5>),
/// [Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)).
pub(crate) const BIT_DEINTERLEAVE_TABLE: [u8; 16] = [
    0x00, 0x03, 0x0C, 0x0F, 0x30, 0x33, 0x3C, 0x3F, 0xC0, 0xC3, 0xCC, 0xCF, 0xF0, 0xF3, 0xFC, 0xFF,
];

/// Natural-to-sequency Hadamard permutation rows for strides `2, 4, 8, 16` (`2 << level`) used by
/// the time-frequency change ([RFC 6716 Section 4.3.4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.5>)).
pub(crate) const HADAMARD_SEQUENCY_ROWS: [&[u8]; 4] = validated_hadamard_rows([
    &[1, 0],
    &[3, 0, 2, 1],
    &[7, 0, 4, 3, 6, 1, 5, 2],
    &[15, 0, 8, 7, 12, 3, 11, 4, 14, 1, 9, 6, 13, 2, 10, 5],
]);

/// Verifies at compile time that row `k` has length `2 << k` and is a permutation of `0..(2 << k)`.
const fn validated_hadamard_rows(rows: [&'static [u8]; 4]) -> [&'static [u8]; 4] {
    let mut k = 0;
    while k < rows.len() {
        let row = rows[k];
        let expected_len = 2usize << k;
        assert!(row.len() == expected_len);
        let mut seen = 0u32;
        let mut i = 0;
        while i < expected_len {
            let val = row[i] as usize;
            assert!(val < expected_len);
            seen |= 1u32 << val;
            i += 1;
        }
        assert!(seen == (1u32 << expected_len) - 1);
        k += 1;
    }
    return rows;
}

/// Base-2 exponential lookup table for fractional Q3 bit resolution in split-angle quantization
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
pub(crate) const EXP2_TABLE8: [i32; 8] = [16384, 17866, 19483, 21247, 23170, 25267, 27554, 30048];

/// PVQ spreading rotation factor indexed by `(spread as usize) - 1`
/// ([RFC 6716 Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>)).
pub(crate) const SPREAD_FACTOR: [usize; 3] = [15, 10, 5];

/// Low-overlap MDCT window of 120 samples (2.5 ms at 48 kHz),
/// `w[i] = sin(pi/2 * sin^2(pi/2 * (i + 0.5) / 120))`
/// ([RFC 6716 Section 4.3.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7>)).
pub(crate) static CELT_WINDOW: [f32; OVERLAP] = [
    6.7286966e-05,
    0.00060551348,
    0.0016815970,
    0.0032947962,
    0.0054439943,
    0.0081276923,
    0.011344001,
    0.015090633,
    0.019364886,
    0.024163635,
    0.029483315,
    0.035319905,
    0.041668911,
    0.048525347,
    0.055883718,
    0.063737999,
    0.072081616,
    0.080907428,
    0.090207705,
    0.099974111,
    0.11019769,
    0.12086883,
    0.13197729,
    0.14351214,
    0.15546177,
    0.16781389,
    0.18055550,
    0.19367290,
    0.20715171,
    0.22097682,
    0.23513243,
    0.24960208,
    0.26436860,
    0.27941419,
    0.29472040,
    0.31026818,
    0.32603788,
    0.34200931,
    0.35816177,
    0.37447407,
    0.39092462,
    0.40749142,
    0.42415215,
    0.44088423,
    0.45766484,
    0.47447104,
    0.49127978,
    0.50806798,
    0.52481261,
    0.54149077,
    0.55807973,
    0.57455701,
    0.59090049,
    0.60708841,
    0.62309951,
    0.63891306,
    0.65450896,
    0.66986776,
    0.68497077,
    0.69980010,
    0.71433873,
    0.72857055,
    0.74248043,
    0.75605425,
    0.76927895,
    0.78214257,
    0.79463430,
    0.80674445,
    0.81846456,
    0.82978733,
    0.84070669,
    0.85121779,
    0.86131698,
    0.87100183,
    0.88027111,
    0.88912479,
    0.89756398,
    0.90559094,
    0.91320904,
    0.92042270,
    0.92723738,
    0.93365955,
    0.93969656,
    0.94535671,
    0.95064907,
    0.95558353,
    0.96017067,
    0.96442171,
    0.96834849,
    0.97196334,
    0.97527906,
    0.97830883,
    0.98106616,
    0.98356480,
    0.98581869,
    0.98784191,
    0.98964856,
    0.99125274,
    0.99266849,
    0.99390969,
    0.99499004,
    0.99592297,
    0.99672162,
    0.99739874,
    0.99796667,
    0.99843728,
    0.99882195,
    0.99913147,
    0.99937606,
    0.99956527,
    0.99970802,
    0.99981248,
    0.99988613,
    0.99993565,
    0.99996697,
    0.99998518,
    0.99999457,
    0.99999859,
    0.99999982,
    1.0000000,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_valid_icdf(name: &str, icdf: &[u32]) {
        assert!(icdf.len() >= 3, "{name}: iCDF must have at least 3 elements");
        let total = icdf[0];
        assert!(total > 0 && total.is_power_of_two(), "{name}: total {total} must be a power of 2");
        assert_eq!(
            icdf.last().copied(),
            Some(total),
            "{name}: last entry must equal total {total}"
        );

        let mut prev = 0;
        for (i, &val) in icdf[1..].iter().enumerate() {
            assert!(
                val > prev && val <= total,
                "{name}[{}]: cumulative bound {val} must be strictly greater than {prev} and <= {total}",
                i + 1
            );
            prev = val;
        }
    }

    #[test]
    fn ebands_and_logn_invariants() {
        assert_eq!(EBANDS.len(), MAX_BANDS + 1);
        assert_eq!(EBANDS[0], 0);
        assert_eq!(EBANDS[MAX_BANDS], 100);

        for band in 0..MAX_BANDS {
            let width = EBANDS[band + 1] - EBANDS[band];
            assert!(width >= 1, "Band {band} width must be positive");
            assert_eq!(band_width(band, BlockSize::SHORT), width);
            assert_eq!(band_width(band, BlockSize::new(3)), width * 8);
            match band {
                0..=7 => {
                    assert_eq!(width, 1);
                    assert_eq!(LOGN[band], 0);
                }
                _ => assert!(LOGN[band] >= LOGN[band - 1]),
            }
        }
    }

    #[test]
    fn icdf_tables_are_strictly_monotonic() {
        assert_valid_icdf("CDF_SMALL_ENERGY", CDF_SMALL_ENERGY.as_slice());
        assert_valid_icdf("CDF_TRIM", CDF_TRIM.as_slice());
        assert_valid_icdf("CDF_SPREAD", CDF_SPREAD.as_slice());
        assert_valid_icdf("CDF_TAPSET", CDF_TAPSET.as_slice());
    }

    #[test]
    fn pulse_cache_and_alloc_vector_invariants() {
        for rows in ALLOC_VECTORS.windows(2) {
            let [lower, upper] = rows else { unreachable!() };
            for band in 0..MAX_BANDS {
                assert!(
                    upper[band] >= lower[band],
                    "ALLOC_VECTORS must be non-decreasing across quality rows at band {band}"
                );
            }
        }

        // Single-bin bands have no half-band level; every other sub-table is well formed.
        assert!(CACHE_INDEX[0][..8].iter().all(|&offset| offset == -1));
        let sub_tables = CACHE_INDEX[0][8..].iter().chain(CACHE_INDEX[1..].iter().flatten());
        for &offset in sub_tables {
            let base = usize::try_from(offset).expect("cache offset is non-negative");
            let max_k = usize::from(CACHE_BITS[base]);
            assert!((1..=MAX_PSEUDO).contains(&max_k));
            assert!(base + max_k < CACHE_BITS.len());
            let costs = &CACHE_BITS[base + 1..=base + max_k];
            assert!(
                costs.windows(2).all(|pair| pair[0] <= pair[1]),
                "CACHE_BITS sub-table at {base} must be non-decreasing"
            );
        }

        // Caps grow with the channel count at every block size and band.
        for caps in &CACHE_CAPS {
            let [mono, stereo] = caps;
            assert!(mono.iter().zip(stereo).all(|(m, s)| m <= s));
        }
    }

    #[test]
    fn tf_select_table_matches_rfc_tables_60_to_63() {
        // Non-transient frames, tf_select = 0 (Table 60) and tf_select = 1 (Table 61).
        assert_eq!(TF_SELECT_TABLE.map(|lm| lm[0][0]), [[0, -1], [0, -1], [0, -2], [0, -2]]);
        assert_eq!(TF_SELECT_TABLE.map(|lm| lm[0][1]), [[0, -1], [0, -2], [0, -3], [0, -3]]);
        // Transient frames, tf_select = 0 (Table 62) and tf_select = 1 (Table 63).
        assert_eq!(TF_SELECT_TABLE.map(|lm| lm[1][0]), [[0, -1], [1, 0], [2, 0], [3, 0]]);
        assert_eq!(TF_SELECT_TABLE.map(|lm| lm[1][1]), [[0, -1], [1, -1], [1, -1], [1, -1]]);
    }

    #[test]
    fn celt_window_princen_bradley_symmetry_and_monotonicity() {
        assert_eq!(CELT_WINDOW.len(), OVERLAP);
        for i in 0..(OVERLAP - 1) {
            assert!(
                CELT_WINDOW[i] < CELT_WINDOW[i + 1],
                "CELT_WINDOW must strictly increase at index {i}"
            );
        }
        for i in 0..(OVERLAP / 2) {
            let w_left = CELT_WINDOW[i];
            let w_right = CELT_WINDOW[OVERLAP - 1 - i];
            let sum_sq = w_left * w_left + w_right * w_right;
            assert!(
                (sum_sq - 1.0).abs() < 1e-6,
                "Princen-Bradley TDAC power complementarity violated at {i}: {sum_sq}"
            );
        }
    }

    #[test]
    fn coarse_energy_probability_and_prediction_bounds() {
        for lm in 0..=MAX_LM {
            assert!(PRED_COEF[lm] > 0.0 && PRED_COEF[lm] < 1.0);
            assert!(BETA_COEF[lm] > 0.0 && BETA_COEF[lm] < 1.0);
            for models in &E_PROB_MODEL[lm] {
                for model in models {
                    assert!(model.zero_freq > 0, "Zero frequency must be positive");
                    assert!(model.decay > 0, "Laplace decay must be positive");
                }
            }
        }
        assert_eq!(LaplaceModel::new(72, 127).zero_freq_q15(), 72 << 7);
        assert_eq!(LaplaceModel::new(72, 127).decay_q14(), 127 << 6);
        assert!((BETA_INTRA - 4915.0 / 32768.0).abs() < 1e-7);
    }
}
