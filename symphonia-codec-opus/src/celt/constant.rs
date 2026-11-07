//! CELT Static Tables and Codebooks (RFC 6716 Section 4.3)
//!
//! Predefined Bark-scale frequency band boundaries, coarse/fine energy prediction
//! and Laplace probability models, static bit-allocation curves, pulse-cache tables,
//! time-frequency (TF) resolution selection tables, comb post-filter tapsets, and
//! the 120-sample low-overlap MDCT window for the 48 kHz CELT mode.
//!
//! <https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>

#![allow(clippy::excessive_precision)]
#![allow(clippy::large_const_arrays)]

pub use crate::celt::{BITRES, MAX_BANDS, MAX_CHANNELS, OVERLAP, Spread};

/// PVQ spreading mode: no rotation (`SPREAD_NONE = 0`).
pub const SPREAD_NONE: usize = 0;

/// PVQ spreading mode: light rotation (`SPREAD_LIGHT = 1`).
pub const SPREAD_LIGHT: usize = 1;

/// PVQ spreading mode: normal rotation (`SPREAD_NORMAL = 2`).
pub const SPREAD_NORMAL: usize = 2;

/// PVQ spreading mode: aggressive rotation (`SPREAD_AGGRESSIVE = 3`).
pub const SPREAD_AGGRESSIVE: usize = 3;

/// Number of static bit-allocation vectors in [`ALLOC_VECTORS`] (RFC 6716 Section 4.3.3).
pub const NB_ALLOC_VECTORS: usize = 11;

/// Number of binary search refinement steps in `interp_bits2pulses` (`ALLOC_STEPS = 6`).
pub const ALLOC_STEPS: usize = 6;

/// Maximum pseudo-pulse index in the pulse cache (`MAX_PSEUDO = 40`).
pub const MAX_PSEUDO: usize = 40;

/// Base-2 logarithm ceiling of [`MAX_PSEUDO`] used for binary search (`LOG_MAX_PSEUDO = 6`).
pub const LOG_MAX_PSEUDO: usize = 6;

/// Maximum number of PVQ pulses in a single partition (`CELT_MAX_PULSES = 128`).
pub const CELT_MAX_PULSES: usize = 128;

/// Maximum number of fine energy quantization bits per band per channel (`MAX_FINE_BITS = 8`).
pub const MAX_FINE_BITS: i32 = 8;

/// Fine energy bit-allocation offset in `1/8`-bit units (`FINE_OFFSET = 21`).
pub const FINE_OFFSET: i32 = 21;

/// Split-angle (`theta`) bit-allocation offset (`QTHETA_OFFSET = 4`).
pub const QTHETA_OFFSET: i32 = 4;

/// Two-phase stereo `N = 2` split-angle (`theta`) bit-allocation offset (`QTHETA_OFFSET_TWOPHASE = 16`).
pub const QTHETA_OFFSET_TWOPHASE: i32 = 16;

/// Pre-emphasis / de-emphasis filter coefficient at 48 kHz (`0.8500061035`, `Q15 = 27853 / 32768`,
/// RFC 6716 Section 4.3.6).
pub const PREEMPH_COEF: f32 = 0.85000610f32;

/// Bark-scale frequency band boundaries in 400 Hz (`shortMdctSize = 120` sample) units
/// for the 48 kHz CELT mode (RFC 6716 Section 4.3, Table 51).
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
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>
pub const EBANDS: [usize; MAX_BANDS + 1] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 34, 40, 48, 60, 78, 100,
];

/// Returns the number of MDCT bins in `band` (`0..21`) at block-size exponent `lm` (`0..=3`).
#[inline]
pub const fn band_width(band: usize, lm: usize) -> usize {
    return (EBANDS[band + 1] - EBANDS[band]) << lm;
}

/// Base-2 logarithm of each band's base width `N0 = EBANDS[i + 1] - EBANDS[i]` in `Q3` (`1/8`-bit)
/// fractional units (`logN400` in `libopus`, RFC 6716 Section 4.3.3).
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>
pub const LOGN: [i16; MAX_BANDS] = [
    0, 0, 0, 0, 0, 0, 0, 0, 8, 8, 8, 8, 16, 16, 16, 21, 21, 24, 29, 34, 36,
];

/// Static bit-allocation table in units of `1/32` bit per sample (`0.1875` dB SNR)
/// across 11 quality levels and 21 Bark bands (RFC 6716 Section 4.3.3, Table 56).
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>
pub const ALLOC_VECTORS: [[u8; MAX_BANDS]; NB_ALLOC_VECTORS] = [
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [90, 80, 75, 69, 63, 56, 49, 40, 34, 29, 20, 18, 10, 0, 0, 0, 0, 0, 0, 0, 0],
    [110, 100, 90, 84, 78, 71, 65, 58, 51, 45, 39, 32, 26, 20, 12, 0, 0, 0, 0, 0, 0],
    [118, 110, 103, 93, 86, 80, 75, 70, 65, 59, 53, 47, 40, 31, 23, 15, 4, 0, 0, 0, 0],
    [126, 119, 112, 104, 95, 89, 83, 78, 72, 66, 60, 54, 47, 39, 32, 25, 17, 12, 1, 0, 0],
    [134, 127, 120, 114, 103, 97, 91, 85, 78, 72, 66, 60, 54, 47, 41, 35, 29, 23, 16, 10, 1],
    [144, 137, 130, 124, 113, 107, 101, 95, 88, 82, 76, 70, 64, 57, 51, 45, 39, 33, 26, 15, 1],
    [152, 145, 138, 132, 123, 117, 111, 105, 98, 92, 86, 80, 74, 67, 61, 55, 49, 43, 36, 20, 1],
    [162, 155, 148, 142, 133, 127, 121, 115, 108, 102, 96, 90, 84, 77, 71, 65, 59, 53, 46, 30, 1],
    [172, 165, 158, 152, 143, 137, 131, 125, 118, 112, 106, 100, 94, 87, 81, 75, 69, 63, 56, 45, 20],
    [
        200, 200, 200, 200, 200, 200, 200, 200, 198, 193, 188, 183, 178, 173, 168, 163, 158, 153,
        148, 129, 104,
    ],
];

/// Pulse cache starting offsets into [`CACHE_BITS`], indexed by `(lm + 1) * 21 + band`
/// for `lm in -1..=3` (`cache_index50` from `static_modes_float.h`).
pub const CACHE_INDEX: [i16; 105] = [
    -1, -1, -1, -1, -1, -1, -1, -1, 0, 0, 0, 0, 41, 41, 41, 82, 82, 123, 164, 200, 222, 0, 0, 0, 0,
    0, 0, 0, 0, 41, 41, 41, 41, 123, 123, 123, 164, 164, 240, 266, 283, 295, 41, 41, 41, 41, 41,
    41, 41, 41, 123, 123, 123, 123, 240, 240, 240, 266, 266, 305, 318, 328, 336, 123, 123, 123,
    123, 123, 123, 123, 123, 240, 240, 240, 240, 305, 305, 305, 318, 318, 343, 351, 358, 364, 240,
    240, 240, 240, 240, 240, 240, 240, 305, 305, 305, 305, 343, 343, 343, 351, 351, 370, 376, 382,
    387,
];

/// Pulse cache bit-requirement lookup table (`cache_bits50` from `static_modes_float.h`,
/// RFC 6716 Section 4.3.3).
///
/// Each sub-table at offset `idx = CACHE_INDEX[(lm + 1) * 21 + band]` begins with
/// `max_pseudo = CACHE_BITS[idx]` followed by `max_pseudo` monotonically non-decreasing
/// entries storing `(required_q3_bits - 1)` for `1..=max_pseudo` pseudo-pulses.
pub const CACHE_BITS: [u8; 392] = [
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
    240, 245, 255, 16, 36, 65, 89, 110, 128, 144, 159, 173, 185, 196, 207, 217, 226, 234, 242,
    250, 11, 41, 74, 103, 128, 151, 172, 191, 209, 225, 241, 255, 9, 43, 79, 110, 138, 163, 186,
    207, 227, 246, 12, 39, 71, 99, 123, 144, 164, 182, 198, 214, 228, 241, 253, 9, 44, 81, 113,
    142, 168, 192, 214, 235, 255, 7, 49, 90, 127, 160, 191, 220, 247, 6, 51, 95, 134, 170, 203,
    234, 7, 47, 87, 123, 155, 184, 212, 237, 6, 52, 97, 137, 174, 208, 240, 5, 57, 106, 151, 192,
    231, 5, 59, 111, 158, 202, 243, 5, 55, 103, 147, 187, 224, 5, 60, 113, 161, 206, 248, 4, 65,
    122, 175, 224, 4, 67, 127, 182, 234,
];

/// Per-band maximum bit allocation caps (`cache_caps50` from `static_modes_float.h`,
/// RFC 6716 Section 4.3.3), indexed by `21 * (2 * lm + channels - 1) + band`.
pub const CACHE_CAPS: [u8; 168] = [
    224, 224, 224, 224, 224, 224, 224, 224, 160, 160, 160, 160, 185, 185, 185, 178, 178, 168, 134,
    61, 37, 224, 224, 224, 224, 224, 224, 224, 224, 240, 240, 240, 240, 207, 207, 207, 198, 198,
    183, 144, 66, 40, 160, 160, 160, 160, 160, 160, 160, 160, 185, 185, 185, 185, 193, 193, 193,
    183, 183, 172, 138, 64, 38, 240, 240, 240, 240, 240, 240, 240, 240, 207, 207, 207, 207, 204,
    204, 204, 193, 193, 180, 143, 66, 40, 185, 185, 185, 185, 185, 185, 185, 185, 193, 193, 193,
    193, 193, 193, 193, 183, 183, 172, 138, 65, 39, 207, 207, 207, 207, 207, 207, 207, 207, 204,
    204, 204, 204, 201, 201, 201, 188, 188, 176, 141, 66, 40, 193, 193, 193, 193, 193, 193, 193,
    193, 193, 193, 193, 193, 194, 194, 194, 184, 184, 173, 139, 65, 39, 204, 204, 204, 204, 204,
    204, 204, 204, 201, 201, 201, 201, 198, 198, 198, 187, 187, 175, 140, 66, 40,
];

/// Fractional base-2 logarithm table (`Q3` units) used to reserve intensity stereo bits
/// (`LOG2_FRAC_TABLE` from `rate.c`, RFC 6716 Section 4.3.3).
pub const LOG2_FRAC_TABLE: [u8; 24] = [
    0, 8, 13, 16, 19, 21, 23, 24, 26, 27, 28, 29, 30, 31, 32, 32, 33, 34, 34, 35, 36, 36, 37, 37,
];

/// Mean log2 energy in each Bark band quantized in `Q4` and converted to `f32`
/// (`eMeans` in `quant_bands.c`, RFC 6716 Section 4.3.2).
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2>
pub const E_MEANS: [f32; MAX_BANDS] = [
    6.4375, 6.25, 5.75, 5.3125, 5.0625, 4.8125, 4.5, 4.375, 4.875, 4.6875, 4.5625, 4.4375, 4.875,
    4.625, 4.3125, 4.5, 4.375, 4.625, 4.75, 4.4375, 3.75,
];

/// Inter-frame coarse energy temporal prediction coefficients `alpha` indexed by `lm` (`0..=3`),
/// approximating `[0.9, 0.8, 0.65, 0.5]` in `Q15` (`pred_coef` in `quant_bands.c`,
/// RFC 6716 Section 4.3.2.1, Table 54).
pub const PRED_COEF: [f32; 4] = [
    29440.0 / 32768.0,
    26112.0 / 32768.0,
    21248.0 / 32768.0,
    16384.0 / 32768.0,
];

/// Inter-frame coarse energy cross-band prediction coefficients `beta` indexed by `lm` (`0..=3`)
/// in `Q15` (`beta_coef` in `quant_bands.c`, RFC 6716 Section 4.3.2.1, Table 54).
pub const BETA_COEF: [f32; 4] = [
    30147.0 / 32768.0,
    22282.0 / 32768.0,
    12124.0 / 32768.0,
    6554.0 / 32768.0,
];

/// Intra-frame coarse energy cross-band prediction coefficient `beta_intra` (`4915 / 32768 = 0.15`,
/// RFC 6716 Section 4.3.2.1).
pub const BETA_INTRA: f32 = 4915.0 / 32768.0;

/// Parameters of the Laplace probability models used for coarse energy decoding,
/// indexed by `[lm][intra as usize][2 * band + {0, 1}]` (`e_prob_model` in `quant_bands.c`,
/// RFC 6716 Section 4.3.2.1, Table 53).
///
/// For each band `i`, entry `2 * i` is the zero frequency (`prob_model[2 * i] << 7`)
/// and entry `2 * i + 1` is the decay rate (`prob_model[2 * i + 1] << 6`).
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.1>
pub const E_PROB_MODEL: [[[u8; 42]; 2]; 4] = [
    // 120-sample frames (LM = 0, 2.5 ms)
    [
        // Inter
        [
            72, 127, 65, 129, 66, 128, 65, 128, 64, 128, 62, 128, 64, 128, 64, 128, 92, 78, 92, 79,
            92, 78, 90, 79, 116, 41, 115, 40, 114, 40, 132, 26, 132, 26, 145, 17, 161, 12, 176, 10,
            177, 11,
        ],
        // Intra
        [
            24, 179, 48, 138, 54, 135, 54, 132, 53, 134, 56, 133, 55, 132, 55, 132, 61, 114, 70,
            96, 74, 88, 75, 88, 87, 74, 89, 66, 91, 67, 100, 59, 108, 50, 120, 40, 122, 37, 97, 43,
            78, 50,
        ],
    ],
    // 240-sample frames (LM = 1, 5 ms)
    [
        // Inter
        [
            83, 78, 84, 81, 88, 75, 86, 74, 87, 71, 90, 73, 93, 74, 93, 74, 109, 40, 114, 36, 117,
            34, 117, 34, 143, 17, 145, 18, 146, 19, 162, 12, 165, 10, 178, 7, 189, 6, 190, 8, 177,
            9,
        ],
        // Intra
        [
            23, 178, 54, 115, 63, 102, 66, 98, 69, 99, 74, 89, 71, 91, 73, 91, 78, 89, 86, 80, 92,
            66, 93, 64, 102, 59, 103, 60, 104, 60, 117, 52, 123, 44, 138, 35, 133, 31, 97, 38, 77,
            45,
        ],
    ],
    // 480-sample frames (LM = 2, 10 ms)
    [
        // Inter
        [
            61, 90, 93, 60, 105, 42, 107, 41, 110, 45, 116, 38, 113, 38, 112, 38, 124, 26, 132, 27,
            136, 19, 140, 20, 155, 14, 159, 16, 158, 18, 170, 13, 177, 10, 187, 8, 192, 6, 175, 9,
            159, 10,
        ],
        // Intra
        [
            21, 178, 59, 110, 71, 86, 75, 85, 84, 83, 91, 66, 88, 73, 87, 72, 92, 75, 98, 72, 105,
            58, 107, 54, 115, 52, 114, 55, 112, 56, 129, 51, 132, 40, 150, 33, 140, 29, 98, 35, 77,
            42,
        ],
    ],
    // 960-sample frames (LM = 3, 20 ms)
    [
        // Inter
        [
            42, 121, 96, 66, 108, 43, 111, 40, 117, 44, 123, 32, 120, 36, 119, 33, 127, 33, 134,
            34, 139, 21, 147, 23, 152, 20, 158, 25, 154, 26, 166, 21, 173, 16, 184, 13, 184, 10,
            150, 13, 139, 15,
        ],
        // Intra
        [
            22, 178, 63, 114, 74, 82, 84, 83, 92, 82, 103, 62, 96, 72, 96, 67, 101, 73, 107, 72,
            113, 55, 118, 52, 125, 52, 118, 52, 117, 55, 135, 49, 137, 39, 157, 32, 145, 29, 97,
            33, 77, 40,
        ],
    ],
];

/// Ascending cumulative distribution table for low-budget (`2 <= budget - tell < 15`)
/// coarse energy steps `0, -1, +1` (derived from `small_energy_icdf = {2, 1, 0}` with total `4`,
/// RFC 6716 Section 4.3.2.1).
pub const SMALL_ENERGY_ICDF: [u32; 4] = [4, 2, 3, 4];

/// Ascending cumulative distribution table for allocation trim (`alloc_trim in 0..=10`,
/// derived from `trim_icdf = {126, 124, 119, 109, 87, 41, 19, 9, 4, 2, 0}` with total `128`,
/// RFC 6716 Section 4.3.3).
pub const TRIM_ICDF: [u32; 12] = [128, 2, 4, 9, 19, 41, 87, 109, 119, 124, 126, 128];

/// Ascending cumulative distribution table for PVQ spreading rotation decision (`0..=3`,
/// derived from `spread_icdf = {25, 23, 2, 0}` with total `32`, RFC 6716 Section 4.3.4.2).
pub const SPREAD_ICDF: [u32; 5] = [32, 7, 9, 30, 32];

/// Ascending cumulative distribution table for comb post-filter tapset (`0..=2`,
/// derived from `tapset_icdf = {2, 1, 0}` with total `4`, RFC 6716 Section 4.3.1).
pub const TAPSET_ICDF: [u32; 4] = [4, 2, 3, 4];

/// Time-Frequency (TF) resolution selection table indexed by `[lm][4 * is_transient + 2 * tf_select + tf_res]`
/// (`tf_select_table` in `celt.c`, RFC 6716 Section 4.3.4.1, Table 57).
///
/// Positive values indicate better frequency resolution (band recombining), whereas
/// negative values indicate better time resolution (time dividing).
///
/// ```text
/// +----+-----------------------+-----------------------+
/// | LM | isTransient = 0       | isTransient = 1       |
/// +----+-----+-----+-----+-----+-----+-----+-----+-----+
/// |  0 |   0 |  -1 |   0 |  -1 |   0 |  -1 |   0 |  -1 |
/// |  1 |   0 |  -1 |   0 |  -2 |   1 |   0 |   1 |  -1 |
/// |  2 |   0 |  -2 |   0 |  -3 |   2 |   0 |   1 |  -1 |
/// |  3 |   0 |  -2 |   0 |  -3 |   3 |   0 |   1 |  -1 |
/// +----+-----+-----+-----+-----+-----+-----+-----+-----+
/// ```
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.1>
pub const TF_SELECT_TABLE: [[i8; 8]; 4] = [
    [0, -1, 0, -1, 0, -1, 0, -1],
    [0, -1, 0, -2, 1, 0, 1, -1],
    [0, -2, 0, -3, 2, 0, 1, -1],
    [0, -2, 0, -3, 3, 0, 1, -1],
];

/// Comb post-filter tap weights `[g0, g1, g2]` for each of the 3 tapsets
/// (`gains` in `celt.c`, RFC 6716 Section 4.3.6, Table 59).
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.6>
pub const COMB_FILTER_GAINS: [[f32; 3]; 3] = [
    [0.3066406250, 0.2170410156, 0.1296386719],
    [0.4638671875, 0.2680664062, 0.0],
    [0.7998046875, 0.1000976562, 0.0],
];

/// Collapse-mask bit interleaving table used during Haar band recombining (`bands.c`).
pub const BIT_INTERLEAVE_TABLE: [u8; 16] = [0, 1, 1, 1, 2, 3, 3, 3, 2, 3, 3, 3, 2, 3, 3, 3];

/// Collapse-mask bit de-interleaving table used when undoing Haar band recombining (`bands.c`).
pub const BIT_DEINTERLEAVE_TABLE: [u8; 16] = [
    0x00, 0x03, 0x0C, 0x0F, 0x30, 0x33, 0x3C, 0x3F, 0xC0, 0xC3, 0xCC, 0xCF, 0xF0, 0xF3, 0xFC, 0xFF,
];

/// Vorbis-like 120-sample (2.5 ms at 48 kHz) low-overlap MDCT window:
/// `w[i] = sin(pi/2 * sin^2(pi/2 * (i + 0.5) / 120))` (`window120` in `static_modes_float.h`,
/// RFC 6716 Section 4.3.6).
///
/// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.6>
pub const CELT_WINDOW: [f32; OVERLAP] = [
    6.7286966e-05, 0.00060551348, 0.0016815970, 0.0032947962, 0.0054439943, 0.0081276923,
    0.011344001, 0.015090633, 0.019364886, 0.024163635, 0.029483315, 0.035319905, 0.041668911,
    0.048525347, 0.055883718, 0.063737999, 0.072081616, 0.080907428, 0.090207705, 0.099974111,
    0.11019769, 0.12086883, 0.13197729, 0.14351214, 0.15546177, 0.16781389, 0.18055550, 0.19367290,
    0.20715171, 0.22097682, 0.23513243, 0.24960208, 0.26436860, 0.27941419, 0.29472040, 0.31026818,
    0.32603788, 0.34200931, 0.35816177, 0.37447407, 0.39092462, 0.40749142, 0.42415215, 0.44088423,
    0.45766484, 0.47447104, 0.49127978, 0.50806798, 0.52481261, 0.54149077, 0.55807973, 0.57455701,
    0.59090049, 0.60708841, 0.62309951, 0.63891306, 0.65450896, 0.66986776, 0.68497077, 0.69980010,
    0.71433873, 0.72857055, 0.74248043, 0.75605425, 0.76927895, 0.78214257, 0.79463430, 0.80674445,
    0.81846456, 0.82978733, 0.84070669, 0.85121779, 0.86131698, 0.87100183, 0.88027111, 0.88912479,
    0.89756398, 0.90559094, 0.91320904, 0.92042270, 0.92723738, 0.93365955, 0.93969656, 0.94535671,
    0.95064907, 0.95558353, 0.96017067, 0.96442171, 0.96834849, 0.97196334, 0.97527906, 0.97830883,
    0.98106616, 0.98356480, 0.98581869, 0.98784191, 0.98964856, 0.99125274, 0.99266849, 0.99390969,
    0.99499004, 0.99592297, 0.99672162, 0.99739874, 0.99796667, 0.99843728, 0.99882195, 0.99913147,
    0.99937606, 0.99956527, 0.99970802, 0.99981248, 0.99988613, 0.99993565, 0.99996697, 0.99998518,
    0.99999457, 0.99999859, 0.99999982, 1.0000000,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_valid_icdf(name: &str, icdf: &[u32]) {
        assert!(icdf.len() >= 3, "{name}: iCDF must have at least 3 elements");
        let total = icdf[0];
        assert!(total > 0 && total.is_power_of_two(), "{name}: total {total} must be a power of 2");
        assert_eq!(icdf.last().copied(), Some(total), "{name}: last entry must equal total {total}");

        let mut prev = 0;
        for (idx, &val) in icdf[1..].iter().enumerate() {
            assert!(
                val > prev && val <= total,
                "{name}[{}]: cumulative bound {val} must be strictly greater than {prev} and <= {total}",
                idx + 1
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
            assert_eq!(band_width(band, 0), width);
            assert_eq!(band_width(band, 3), width * 8);
            if band < 8 {
                assert_eq!(width, 1);
                assert_eq!(LOGN[band], 0);
            } else {
                assert!(LOGN[band] >= LOGN[band - 1]);
            }
        }
    }

    #[test]
    fn icdf_tables_are_strictly_monotonic() {
        assert_valid_icdf("SMALL_ENERGY_ICDF", &SMALL_ENERGY_ICDF);
        assert_valid_icdf("TRIM_ICDF", &TRIM_ICDF);
        assert_valid_icdf("SPREAD_ICDF", &SPREAD_ICDF);
        assert_valid_icdf("TAPSET_ICDF", &TAPSET_ICDF);
    }

    #[test]
    fn pulse_cache_and_alloc_vector_invariants() {
        for (row_idx, row) in ALLOC_VECTORS.iter().enumerate() {
            if row_idx > 0 {
                for band in 0..MAX_BANDS {
                    assert!(
                        row[band] >= ALLOC_VECTORS[row_idx - 1][band],
                        "ALLOC_VECTORS must be non-decreasing across quality rows at band {band}"
                    );
                }
            }
        }

        for (slot, &offset) in CACHE_INDEX.iter().enumerate() {
            if slot < 8 {
                assert_eq!(offset, -1, "N0=1 bands at LM=-1 are never split");
            } else {
                let idx = offset as usize;
                let max_k = CACHE_BITS[idx] as usize;
                assert!((1..=MAX_PSEUDO).contains(&max_k));
                assert!(idx + max_k < CACHE_BITS.len());
                for k in 1..max_k {
                    assert!(
                        CACHE_BITS[idx + k] <= CACHE_BITS[idx + k + 1],
                        "CACHE_BITS sub-table at {idx} must be non-decreasing"
                    );
                }
            }
        }
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
        for lm in 0..4 {
            assert!(PRED_COEF[lm] > 0.0 && PRED_COEF[lm] < 1.0);
            assert!(BETA_COEF[lm] > 0.0 && BETA_COEF[lm] < 1.0);
            for prob_row in &E_PROB_MODEL[lm] {
                for band in 0..MAX_BANDS {
                    let f0 = prob_row[2 * band];
                    let decay = prob_row[2 * band + 1];
                    assert!(f0 > 0, "Zero frequency must be positive");
                    assert!(decay > 0, "Laplace decay must be positive");
                }
            }
        }
        assert!((BETA_INTRA - 4915.0 / 32768.0).abs() < 1e-7);
    }
}
