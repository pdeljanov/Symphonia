// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The SBR Huffman tables (ISO/IEC 14496-3:2009, Annex 4.A.6.1, Tables 4.A.78 to 4.A.88) and the
//! Huffman decoding function sbr_huff_dec().

use lazy_static::lazy_static;
use symphonia_core::io::ReadBitsLtr;
use symphonia_core::io::vlc::{BitOrder, Codebook, CodebookBuilder, Entry8x16};

/// The SBR Huffman tables of Table 4.A.78.
///
/// f_huffman_noise_3_0dB and f_huffman_noise_bal_3_0dB are identical to f_huffman_env_3_0dB and
/// f_huffman_env_bal_3_0dB, respectively (Table 4.A.78, Note 2), and are therefore not listed
/// separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SbrHuffmanTable {
    /// t_huffman_env_1_5dB (Table 4.A.79).
    TEnv1_5dB,
    /// f_huffman_env_1_5dB (Table 4.A.80).
    FEnv1_5dB,
    /// t_huffman_env_bal_1_5dB (Table 4.A.81).
    TEnvBal1_5dB,
    /// f_huffman_env_bal_1_5dB (Table 4.A.82).
    FEnvBal1_5dB,
    /// t_huffman_env_3_0dB (Table 4.A.83).
    TEnv3_0dB,
    /// f_huffman_env_3_0dB and f_huffman_noise_3_0dB (Table 4.A.84).
    FEnv3_0dB,
    /// t_huffman_env_bal_3_0dB (Table 4.A.85).
    TEnvBal3_0dB,
    /// f_huffman_env_bal_3_0dB and f_huffman_noise_bal_3_0dB (Table 4.A.86).
    FEnvBal3_0dB,
    /// t_huffman_noise_3_0dB (Table 4.A.87).
    TNoise3_0dB,
    /// t_huffman_noise_bal_3_0dB (Table 4.A.88).
    TNoiseBal3_0dB,
}

impl SbrHuffmanTable {
    /// All tables, in the order of their index.
    pub const ALL: [SbrHuffmanTable; 10] = [
        SbrHuffmanTable::TEnv1_5dB,
        SbrHuffmanTable::FEnv1_5dB,
        SbrHuffmanTable::TEnvBal1_5dB,
        SbrHuffmanTable::FEnvBal1_5dB,
        SbrHuffmanTable::TEnv3_0dB,
        SbrHuffmanTable::FEnv3_0dB,
        SbrHuffmanTable::TEnvBal3_0dB,
        SbrHuffmanTable::FEnvBal3_0dB,
        SbrHuffmanTable::TNoise3_0dB,
        SbrHuffmanTable::TNoiseBal3_0dB,
    ];

    fn index(self) -> usize {
        self as usize
    }

    /// The largest absolute value (LAV) of the table (Table 4.A.78).
    pub fn lav(self) -> i32 {
        match self {
            SbrHuffmanTable::TEnv1_5dB | SbrHuffmanTable::FEnv1_5dB => 60,
            SbrHuffmanTable::TEnvBal1_5dB | SbrHuffmanTable::FEnvBal1_5dB => 24,
            SbrHuffmanTable::TEnv3_0dB | SbrHuffmanTable::FEnv3_0dB => 31,
            SbrHuffmanTable::TEnvBal3_0dB | SbrHuffmanTable::FEnvBal3_0dB => 12,
            SbrHuffmanTable::TNoise3_0dB => 31,
            SbrHuffmanTable::TNoiseBal3_0dB => 12,
        }
    }

    /// The (codeword length, codeword) pairs of the table, in index order.
    pub fn codes(self) -> &'static [(u8, u32)] {
        match self {
            SbrHuffmanTable::TEnv1_5dB => &T_HUFFMAN_ENV_1_5DB,
            SbrHuffmanTable::FEnv1_5dB => &F_HUFFMAN_ENV_1_5DB,
            SbrHuffmanTable::TEnvBal1_5dB => &T_HUFFMAN_ENV_BAL_1_5DB,
            SbrHuffmanTable::FEnvBal1_5dB => &F_HUFFMAN_ENV_BAL_1_5DB,
            SbrHuffmanTable::TEnv3_0dB => &T_HUFFMAN_ENV_3_0DB,
            SbrHuffmanTable::FEnv3_0dB => &F_HUFFMAN_ENV_3_0DB,
            SbrHuffmanTable::TEnvBal3_0dB => &T_HUFFMAN_ENV_BAL_3_0DB,
            SbrHuffmanTable::FEnvBal3_0dB => &F_HUFFMAN_ENV_BAL_3_0DB,
            SbrHuffmanTable::TNoise3_0dB => &T_HUFFMAN_NOISE_3_0DB,
            SbrHuffmanTable::TNoiseBal3_0dB => &T_HUFFMAN_NOISE_BAL_3_0DB,
        }
    }
}

/// Build a codebook returning the table index of each codeword.
fn make_codebook(table: SbrHuffmanTable) -> Codebook<Entry8x16> {
    let entries = table.codes();

    let lens: Vec<u8> = entries.iter().map(|&(len, _)| len).collect();
    let codes: Vec<u32> = entries.iter().map(|&(_, code)| code).collect();
    let values: Vec<u8> =
        (0..entries.len()).map(|i| u8::try_from(i).expect("table has < 256 entries")).collect();

    let mut builder = CodebookBuilder::new(BitOrder::Verbatim);
    builder.bits_per_read(8);
    builder.make(&codes, &lens, &values).expect("valid static codebook data")
}

lazy_static! {
    static ref CODEBOOKS: [Codebook<Entry8x16>; 10] = SbrHuffmanTable::ALL.map(make_codebook);
}

/// sbr_huff_dec() (Annex 4.A.6.1): read one codeword with the given table and return the table
/// index of the codeword with the LAV of the table subtracted.
pub fn sbr_huff_dec<B: ReadBitsLtr>(bs: &mut B, table: SbrHuffmanTable) -> std::io::Result<i32> {
    let (index, _) = bs.read_codebook(&CODEBOOKS[table.index()])?;
    Ok(i32::from(index) - table.lav())
}

/// Table 4.A.79, t_huffman_env_1_5dB: (codeword length, codeword) for each index.
#[rustfmt::skip]
static T_HUFFMAN_ENV_1_5DB: [(u8, u32); 121] = [
    (18, 0x3ffd6), (18, 0x3ffd7), (18, 0x3ffd8), (18, 0x3ffd9), (18, 0x3ffda), (18, 0x3ffdb),
    (19, 0x7ffb8), (19, 0x7ffb9), (19, 0x7ffba), (19, 0x7ffbb), (19, 0x7ffbc), (19, 0x7ffbd),
    (19, 0x7ffbe), (19, 0x7ffbf), (19, 0x7ffc0), (19, 0x7ffc1), (19, 0x7ffc2), (19, 0x7ffc3),
    (19, 0x7ffc4), (19, 0x7ffc5), (19, 0x7ffc6), (19, 0x7ffc7), (19, 0x7ffc8), (19, 0x7ffc9),
    (19, 0x7ffca), (19, 0x7ffcb), (19, 0x7ffcc), (19, 0x7ffcd), (19, 0x7ffce), (19, 0x7ffcf),
    (19, 0x7ffd0), (19, 0x7ffd1), (19, 0x7ffd2), (19, 0x7ffd3), (17, 0x1ffe6), (18, 0x3ffd4),
    (16, 0x0fff0), (17, 0x1ffe9), (18, 0x3ffd5), (17, 0x1ffe7), (16, 0x0fff1), (16, 0x0ffec),
    (16, 0x0ffed), (16, 0x0ffee), (15, 0x07ff4), (14, 0x03ff9), (14, 0x03ff7), (13, 0x01ffa),
    (13, 0x01ff9), (12, 0x00ffb), (11, 0x007fc), (10, 0x003fc), ( 9, 0x001fd), ( 8, 0x000fd),
    ( 7, 0x0007d), ( 6, 0x0003d), ( 5, 0x0001d), ( 4, 0x0000d), ( 3, 0x00005), ( 2, 0x00001),
    ( 2, 0x00000), ( 3, 0x00004), ( 4, 0x0000c), ( 5, 0x0001c), ( 6, 0x0003c), ( 7, 0x0007c),
    ( 8, 0x000fc), ( 9, 0x001fc), (10, 0x003fd), (12, 0x00ffa), (13, 0x01ff8), (14, 0x03ff6),
    (14, 0x03ff8), (15, 0x07ff5), (16, 0x0ffef), (17, 0x1ffe8), (16, 0x0fff2), (19, 0x7ffd4),
    (19, 0x7ffd5), (19, 0x7ffd6), (19, 0x7ffd7), (19, 0x7ffd8), (19, 0x7ffd9), (19, 0x7ffda),
    (19, 0x7ffdb), (19, 0x7ffdc), (19, 0x7ffdd), (19, 0x7ffde), (19, 0x7ffdf), (19, 0x7ffe0),
    (19, 0x7ffe1), (19, 0x7ffe2), (19, 0x7ffe3), (19, 0x7ffe4), (19, 0x7ffe5), (19, 0x7ffe6),
    (19, 0x7ffe7), (19, 0x7ffe8), (19, 0x7ffe9), (19, 0x7ffea), (19, 0x7ffeb), (19, 0x7ffec),
    (19, 0x7ffed), (19, 0x7ffee), (19, 0x7ffef), (19, 0x7fff0), (19, 0x7fff1), (19, 0x7fff2),
    (19, 0x7fff3), (19, 0x7fff4), (19, 0x7fff5), (19, 0x7fff6), (19, 0x7fff7), (19, 0x7fff8),
    (19, 0x7fff9), (19, 0x7fffa), (19, 0x7fffb), (19, 0x7fffc), (19, 0x7fffd), (19, 0x7fffe),
    (19, 0x7ffff),
];

/// Table 4.A.80, f_huffman_env_1_5dB: (codeword length, codeword) for each index.
#[rustfmt::skip]
static F_HUFFMAN_ENV_1_5DB: [(u8, u32); 121] = [
    (19, 0x7ffe7), (19, 0x7ffe8), (20, 0xfffd2), (20, 0xfffd3), (20, 0xfffd4), (20, 0xfffd5),
    (20, 0xfffd6), (20, 0xfffd7), (20, 0xfffd8), (19, 0x7ffda), (20, 0xfffd9), (20, 0xfffda),
    (20, 0xfffdb), (20, 0xfffdc), (19, 0x7ffdb), (20, 0xfffdd), (19, 0x7ffdc), (19, 0x7ffdd),
    (20, 0xfffde), (18, 0x3ffe4), (20, 0xfffdf), (20, 0xfffe0), (20, 0xfffe1), (19, 0x7ffde),
    (20, 0xfffe2), (20, 0xfffe3), (20, 0xfffe4), (19, 0x7ffdf), (20, 0xfffe5), (19, 0x7ffe0),
    (18, 0x3ffe8), (19, 0x7ffe1), (18, 0x3ffe0), (18, 0x3ffe9), (17, 0x1ffef), (18, 0x3ffe5),
    (17, 0x1ffec), (17, 0x1ffed), (17, 0x1ffee), (16, 0x0fff4), (16, 0x0fff3), (16, 0x0fff0),
    (15, 0x07ff7), (15, 0x07ff6), (14, 0x03ffa), (13, 0x01ffa), (13, 0x01ff9), (12, 0x00ffa),
    (12, 0x00ff8), (11, 0x007f9), (10, 0x003fb), ( 9, 0x001fc), ( 9, 0x001fa), ( 8, 0x000fb),
    ( 7, 0x0007c), ( 6, 0x0003c), ( 5, 0x0001c), ( 4, 0x0000c), ( 3, 0x00005), ( 2, 0x00001),
    ( 2, 0x00000), ( 3, 0x00004), ( 4, 0x0000d), ( 5, 0x0001d), ( 6, 0x0003d), ( 8, 0x000fa),
    ( 8, 0x000fc), ( 9, 0x001fb), (10, 0x003fa), (11, 0x007f8), (11, 0x007fa), (11, 0x007fb),
    (12, 0x00ff9), (12, 0x00ffb), (13, 0x01ff8), (13, 0x01ffb), (14, 0x03ff8), (14, 0x03ff9),
    (16, 0x0fff1), (16, 0x0fff2), (17, 0x1ffea), (17, 0x1ffeb), (18, 0x3ffe1), (18, 0x3ffe2),
    (18, 0x3ffea), (18, 0x3ffe3), (18, 0x3ffe6), (18, 0x3ffe7), (18, 0x3ffeb), (20, 0xfffe6),
    (19, 0x7ffe2), (20, 0xfffe7), (20, 0xfffe8), (20, 0xfffe9), (20, 0xfffea), (20, 0xfffeb),
    (20, 0xfffec), (19, 0x7ffe3), (20, 0xfffed), (20, 0xfffee), (20, 0xfffef), (20, 0xffff0),
    (19, 0x7ffe4), (20, 0xffff1), (18, 0x3ffec), (20, 0xffff2), (20, 0xffff3), (19, 0x7ffe5),
    (19, 0x7ffe6), (20, 0xffff4), (20, 0xffff5), (20, 0xffff6), (20, 0xffff7), (20, 0xffff8),
    (20, 0xffff9), (20, 0xffffa), (20, 0xffffb), (20, 0xffffc), (20, 0xffffd), (20, 0xffffe),
    (20, 0xfffff),
];

/// Table 4.A.81, t_huffman_env_bal_1_5dB: (codeword length, codeword) for each index.
#[rustfmt::skip]
static T_HUFFMAN_ENV_BAL_1_5DB: [(u8, u32); 49] = [
    (16, 0x0ffe4), (16, 0x0ffe5), (16, 0x0ffe6), (16, 0x0ffe7), (16, 0x0ffe8), (16, 0x0ffe9),
    (16, 0x0ffea), (16, 0x0ffeb), (16, 0x0ffec), (16, 0x0ffed), (16, 0x0ffee), (16, 0x0ffef),
    (16, 0x0fff0), (16, 0x0fff1), (16, 0x0fff2), (16, 0x0fff3), (16, 0x0fff4), (16, 0x0ffe2),
    (12, 0x00ffc), (11, 0x007fc), ( 9, 0x001fe), ( 7, 0x0007e), ( 5, 0x0001e), ( 3, 0x00006),
    ( 1, 0x00000), ( 2, 0x00002), ( 4, 0x0000e), ( 6, 0x0003e), ( 8, 0x000fe), (11, 0x007fd),
    (12, 0x00ffd), (15, 0x07ff0), (16, 0x0ffe3), (16, 0x0fff5), (16, 0x0fff6), (16, 0x0fff7),
    (16, 0x0fff8), (16, 0x0fff9), (16, 0x0fffa), (17, 0x1fff6), (17, 0x1fff7), (17, 0x1fff8),
    (17, 0x1fff9), (17, 0x1fffa), (17, 0x1fffb), (17, 0x1fffc), (17, 0x1fffd), (17, 0x1fffe),
    (17, 0x1ffff),
];

/// Table 4.A.82, f_huffman_env_bal_1_5dB: (codeword length, codeword) for each index.
#[rustfmt::skip]
static F_HUFFMAN_ENV_BAL_1_5DB: [(u8, u32); 49] = [
    (18, 0x3ffe2), (18, 0x3ffe3), (18, 0x3ffe4), (18, 0x3ffe5), (18, 0x3ffe6), (18, 0x3ffe7),
    (18, 0x3ffe8), (18, 0x3ffe9), (18, 0x3ffea), (18, 0x3ffeb), (18, 0x3ffec), (18, 0x3ffed),
    (18, 0x3ffee), (18, 0x3ffef), (18, 0x3fff0), (16, 0x0fff7), (17, 0x1fff0), (14, 0x03ffc),
    (11, 0x007fe), (11, 0x007fc), ( 8, 0x000fe), ( 7, 0x0007e), ( 4, 0x0000e), ( 2, 0x00002),
    ( 1, 0x00000), ( 3, 0x00006), ( 5, 0x0001e), ( 6, 0x0003e), ( 9, 0x001fe), (11, 0x007fd),
    (12, 0x00ffe), (15, 0x07ffa), (16, 0x0fff6), (18, 0x3fff1), (18, 0x3fff2), (18, 0x3fff3),
    (18, 0x3fff4), (18, 0x3fff5), (18, 0x3fff6), (18, 0x3fff7), (18, 0x3fff8), (18, 0x3fff9),
    (18, 0x3fffa), (18, 0x3fffb), (18, 0x3fffc), (18, 0x3fffd), (18, 0x3fffe), (19, 0x7fffe),
    (19, 0x7ffff),
];

/// Table 4.A.83, t_huffman_env_3_0dB: (codeword length, codeword) for each index.
#[rustfmt::skip]
static T_HUFFMAN_ENV_3_0DB: [(u8, u32); 63] = [
    (18, 0x3ffed), (18, 0x3ffee), (19, 0x7ffde), (19, 0x7ffdf), (19, 0x7ffe0), (19, 0x7ffe1),
    (19, 0x7ffe2), (19, 0x7ffe3), (19, 0x7ffe4), (19, 0x7ffe5), (19, 0x7ffe6), (19, 0x7ffe7),
    (19, 0x7ffe8), (19, 0x7ffe9), (19, 0x7ffea), (19, 0x7ffeb), (19, 0x7ffec), (17, 0x1fff4),
    (16, 0x0fff7), (16, 0x0fff9), (16, 0x0fff8), (14, 0x03ffb), (14, 0x03ffa), (14, 0x03ff8),
    (13, 0x01ffa), (12, 0x00ffc), (11, 0x007fc), ( 8, 0x000fe), ( 6, 0x0003e), ( 4, 0x0000e),
    ( 2, 0x00002), ( 1, 0x00000), ( 3, 0x00006), ( 5, 0x0001e), ( 7, 0x0007e), ( 9, 0x001fe),
    (11, 0x007fd), (13, 0x01ffb), (14, 0x03ff9), (14, 0x03ffc), (15, 0x07ffa), (16, 0x0fff6),
    (17, 0x1fff5), (18, 0x3ffec), (19, 0x7ffed), (19, 0x7ffee), (19, 0x7ffef), (19, 0x7fff0),
    (19, 0x7fff1), (19, 0x7fff2), (19, 0x7fff3), (19, 0x7fff4), (19, 0x7fff5), (19, 0x7fff6),
    (19, 0x7fff7), (19, 0x7fff8), (19, 0x7fff9), (19, 0x7fffa), (19, 0x7fffb), (19, 0x7fffc),
    (19, 0x7fffd), (19, 0x7fffe), (19, 0x7ffff),
];

/// Table 4.A.84, f_huffman_env_3_0dB: (codeword length, codeword) for each index.
#[rustfmt::skip]
static F_HUFFMAN_ENV_3_0DB: [(u8, u32); 63] = [
    (20, 0xffff0), (20, 0xffff1), (20, 0xffff2), (20, 0xffff3), (20, 0xffff4), (20, 0xffff5),
    (20, 0xffff6), (18, 0x3fff3), (19, 0x7fff5), (19, 0x7ffee), (19, 0x7ffef), (19, 0x7fff6),
    (18, 0x3fff4), (18, 0x3fff2), (20, 0xffff7), (19, 0x7fff0), (17, 0x1fff5), (18, 0x3fff0),
    (17, 0x1fff4), (16, 0x0fff7), (16, 0x0fff6), (15, 0x07ff8), (14, 0x03ffb), (12, 0x00ffd),
    (11, 0x007fd), (10, 0x003fd), ( 9, 0x001fd), ( 8, 0x000fd), ( 6, 0x0003e), ( 4, 0x0000e),
    ( 2, 0x00002), ( 1, 0x00000), ( 3, 0x00006), ( 5, 0x0001e), ( 8, 0x000fc), ( 9, 0x001fc),
    (10, 0x003fc), (11, 0x007fc), (12, 0x00ffc), (13, 0x01ffc), (14, 0x03ffa), (15, 0x07ff9),
    (15, 0x07ffa), (16, 0x0fff8), (16, 0x0fff9), (17, 0x1fff6), (17, 0x1fff7), (18, 0x3fff5),
    (18, 0x3fff6), (18, 0x3fff1), (20, 0xffff8), (19, 0x7fff1), (19, 0x7fff2), (19, 0x7fff3),
    (20, 0xffff9), (19, 0x7fff7), (19, 0x7fff4), (20, 0xffffa), (20, 0xffffb), (20, 0xffffc),
    (20, 0xffffd), (20, 0xffffe), (20, 0xfffff),
];

/// Table 4.A.85, t_huffman_env_bal_3_0dB: (codeword length, codeword) for each index.
#[rustfmt::skip]
static T_HUFFMAN_ENV_BAL_3_0DB: [(u8, u32); 25] = [
    (13, 0x01ff2), (13, 0x01ff3), (13, 0x01ff4), (13, 0x01ff5), (13, 0x01ff6), (13, 0x01ff7),
    (13, 0x01ff8), (12, 0x00ff8), ( 8, 0x000fe), ( 7, 0x0007e), ( 4, 0x0000e), ( 3, 0x00006),
    ( 1, 0x00000), ( 2, 0x00002), ( 5, 0x0001e), ( 6, 0x0003e), ( 9, 0x001fe), (13, 0x01ff9),
    (13, 0x01ffa), (13, 0x01ffb), (13, 0x01ffc), (13, 0x01ffd), (13, 0x01ffe), (14, 0x03ffe),
    (14, 0x03fff),
];

/// Table 4.A.86, f_huffman_env_bal_3_0dB: (codeword length, codeword) for each index.
#[rustfmt::skip]
static F_HUFFMAN_ENV_BAL_3_0DB: [(u8, u32); 25] = [
    (13, 0x01ff7), (13, 0x01ff8), (13, 0x01ff9), (13, 0x01ffa), (13, 0x01ffb), (14, 0x03ff8),
    (14, 0x03ff9), (11, 0x007fc), ( 8, 0x000fe), ( 7, 0x0007e), ( 4, 0x0000e), ( 2, 0x00002),
    ( 1, 0x00000), ( 3, 0x00006), ( 5, 0x0001e), ( 6, 0x0003e), ( 9, 0x001fe), (12, 0x00ffa),
    (13, 0x01ff6), (14, 0x03ffa), (14, 0x03ffb), (14, 0x03ffc), (14, 0x03ffd), (14, 0x03ffe),
    (14, 0x03fff),
];

/// Table 4.A.87, t_huffman_noise_3_0dB: (codeword length, codeword) for each index.
#[rustfmt::skip]
static T_HUFFMAN_NOISE_3_0DB: [(u8, u32); 63] = [
    (13, 0x01fce), (13, 0x01fcf), (13, 0x01fd0), (13, 0x01fd1), (13, 0x01fd2), (13, 0x01fd3),
    (13, 0x01fd4), (13, 0x01fd5), (13, 0x01fd6), (13, 0x01fd7), (13, 0x01fd8), (13, 0x01fd9),
    (13, 0x01fda), (13, 0x01fdb), (13, 0x01fdc), (13, 0x01fdd), (13, 0x01fde), (13, 0x01fdf),
    (13, 0x01fe0), (13, 0x01fe1), (13, 0x01fe2), (13, 0x01fe3), (13, 0x01fe4), (13, 0x01fe5),
    (13, 0x01fe6), (13, 0x01fe7), (11, 0x007f2), ( 8, 0x000fd), ( 6, 0x0003e), ( 4, 0x0000e),
    ( 3, 0x00006), ( 1, 0x00000), ( 2, 0x00002), ( 5, 0x0001e), ( 8, 0x000fc), (10, 0x003f8),
    (13, 0x01fcc), (13, 0x01fe8), (13, 0x01fe9), (13, 0x01fea), (13, 0x01feb), (13, 0x01fec),
    (13, 0x01fcd), (13, 0x01fed), (13, 0x01fee), (13, 0x01fef), (13, 0x01ff0), (13, 0x01ff1),
    (13, 0x01ff2), (13, 0x01ff3), (13, 0x01ff4), (13, 0x01ff5), (13, 0x01ff6), (13, 0x01ff7),
    (13, 0x01ff8), (13, 0x01ff9), (13, 0x01ffa), (13, 0x01ffb), (13, 0x01ffc), (13, 0x01ffd),
    (13, 0x01ffe), (14, 0x03ffe), (14, 0x03fff),
];

/// Table 4.A.88, t_huffman_noise_bal_3_0dB: (codeword length, codeword) for each index.
#[rustfmt::skip]
static T_HUFFMAN_NOISE_BAL_3_0DB: [(u8, u32); 25] = [
    ( 8, 0x000ec), ( 8, 0x000ed), ( 8, 0x000ee), ( 8, 0x000ef), ( 8, 0x000f0), ( 8, 0x000f1),
    ( 8, 0x000f2), ( 8, 0x000f3), ( 8, 0x000f4), ( 8, 0x000f5), ( 5, 0x0001c), ( 2, 0x00002),
    ( 1, 0x00000), ( 3, 0x00006), ( 6, 0x0003a), ( 8, 0x000f6), ( 8, 0x000f7), ( 8, 0x000f8),
    ( 8, 0x000f9), ( 8, 0x000fa), ( 8, 0x000fb), ( 8, 0x000fc), ( 8, 0x000fd), ( 8, 0x000fe),
    ( 8, 0x000ff),
];

#[cfg(test)]
mod tests {
    use super::*;
    use symphonia_core::io::BitReaderLtr;

    /// The number of entries of each table is 2 * LAV + 1 (Table 4.A.78).
    #[test]
    fn verify_table_sizes() {
        let expected = [
            (SbrHuffmanTable::TEnv1_5dB, 60),
            (SbrHuffmanTable::FEnv1_5dB, 60),
            (SbrHuffmanTable::TEnvBal1_5dB, 24),
            (SbrHuffmanTable::FEnvBal1_5dB, 24),
            (SbrHuffmanTable::TEnv3_0dB, 31),
            (SbrHuffmanTable::FEnv3_0dB, 31),
            (SbrHuffmanTable::TEnvBal3_0dB, 12),
            (SbrHuffmanTable::FEnvBal3_0dB, 12),
            (SbrHuffmanTable::TNoise3_0dB, 31),
            (SbrHuffmanTable::TNoiseBal3_0dB, 12),
        ];

        for (table, lav) in expected {
            assert_eq!(table.lav(), lav);
            assert_eq!(table.codes().len(), 2 * lav as usize + 1, "{:?}", table);
        }
    }

    /// Every table is a complete (Kraft equality) prefix-free code.
    #[test]
    fn verify_tables_are_complete_prefix_codes() {
        for table in SbrHuffmanTable::ALL {
            let codes = table.codes();

            // Kraft sum, scaled by 2^20 (the longest codeword has 20 bits).
            let mut kraft = 0u64;

            for (i, &(len, code)) in codes.iter().enumerate() {
                assert!(len >= 1 && len <= 20, "{:?}[{}]", table, i);
                assert!(u64::from(code) < (1u64 << len), "{:?}[{}]", table, i);
                kraft += 1u64 << (20 - len);

                // No codeword may be a prefix of another codeword.
                for (j, &(len2, code2)) in codes.iter().enumerate() {
                    if i != j && len <= len2 {
                        assert_ne!(
                            code2 >> (len2 - len),
                            code,
                            "{:?}: {} prefix of {}",
                            table,
                            i,
                            j
                        );
                    }
                }
            }

            assert_eq!(kraft, 1u64 << 20, "{:?}", table);
        }
    }

    /// Spot values transcribed from the specification.
    #[test]
    fn verify_table_spot_values() {
        // Table 4.A.79, index 60 (delta 0) and index 75.
        assert_eq!(T_HUFFMAN_ENV_1_5DB[60], (2, 0x0));
        assert_eq!(T_HUFFMAN_ENV_1_5DB[75], (17, 0x1ffe8));
        // Table 4.A.80, index 2 and index 104.
        assert_eq!(F_HUFFMAN_ENV_1_5DB[2], (20, 0xfffd2));
        assert_eq!(F_HUFFMAN_ENV_1_5DB[104], (18, 0x3ffec));
        // Table 4.A.84, index 7 and index 50.
        assert_eq!(F_HUFFMAN_ENV_3_0DB[7], (18, 0x3fff3));
        assert_eq!(F_HUFFMAN_ENV_3_0DB[50], (20, 0xffff8));
        // Table 4.A.87, index 36 and 42.
        assert_eq!(T_HUFFMAN_NOISE_3_0DB[36], (13, 0x1fcc));
        assert_eq!(T_HUFFMAN_NOISE_3_0DB[42], (13, 0x1fcd));
        // Table 4.A.88, index 14.
        assert_eq!(T_HUFFMAN_NOISE_BAL_3_0DB[14], (6, 0x3a));
    }

    /// Decoding each codeword of each table returns its index minus LAV.
    #[test]
    fn verify_sbr_huff_dec() {
        for table in SbrHuffmanTable::ALL {
            for (i, &(len, code)) in table.codes().iter().enumerate() {
                // Left-align the codeword in a 32-bit word, followed by ones.
                let word = (code << (32 - len)) | ((1u32 << (32 - len)) - 1);
                let buf = word.to_be_bytes();
                let mut bs = BitReaderLtr::new(&buf);
                let value = sbr_huff_dec(&mut bs, table).unwrap();
                assert_eq!(value, i as i32 - table.lav(), "{:?}[{}]", table, i);
                // Exactly the codeword length was consumed.
                assert_eq!(bs.read_bits_leq32(32 - u32::from(len)).unwrap(), (1 << (32 - len)) - 1);
            }
        }
    }
}
