// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Huffman tables of the parametric stereo tool (ISO/IEC 14496-3:2009, Annex 8.B, Tables 8.B.17 to
//! 8.B.21) and ssc_huff_dec() for these tables.
//!
//! The decoding trees are built, and the code tables validated (complete and prefix-free), at
//! compile time.

use symphonia_core::errors::Result;

/// A binary Huffman decoding tree.
///
/// Node 0 is the root. Each node holds its two children, for a 0 bit and a 1 bit respectively. A
/// non-negative child is the index of another node, and a negative child `c` is a leaf for the code
/// table entry with index `-(c + 1)`.
pub(super) struct HuffTree {
    nodes: &'static [[i16; 2]],
    /// The value of the first code table entry. Entry `i` decodes to `first_value + i`.
    first_value: i32,
}

impl HuffTree {
    /// Decode one codeword, ssc_huff_dec() (Annex 8.B.1), reading the codeword bit by bit with
    /// `read_bit`.
    pub fn decode<F>(&self, mut read_bit: F) -> Result<i32>
    where
        F: FnMut() -> Result<bool>,
    {
        let mut node = 0usize;
        // The tree is complete and finite (verified at compile time), so every path ends at a leaf
        // within the maximum codeword length.
        loop {
            let child = self.nodes[node][usize::from(read_bit()?)];
            if child < 0 {
                return Ok(self.first_value + (-(i32::from(child) + 1)));
            }
            node = child as usize;
        }
    }
}

/// Build the decoding tree of a complete, prefix-free code table with `N` entries `(codeword,
/// length)`. The tree has `M = N - 1` nodes. Panics (at compile time when used for a static) if the
/// code table is not complete and prefix-free.
const fn build_tree<const N: usize, const M: usize>(codes: &[(u32, u8); N]) -> [[i16; 2]; M] {
    assert!(N >= 2 && M + 1 == N, "invalid tree size");
    assert!(N < i16::MAX as usize, "code table too large");

    let mut nodes = [[0i16; 2]; M];
    let mut num_nodes = 1;

    let mut i = 0;
    while i < N {
        let (code, len) = codes[i];
        assert!(len >= 1 && len <= 32, "invalid codeword length");
        assert!(len == 32 || code >> len == 0, "codeword exceeds its length");

        // Walk, or create, the internal nodes of the codeword prefix.
        let mut node = 0;
        let mut bit = len;
        while bit > 1 {
            bit -= 1;
            let b = ((code >> bit) & 1) as usize;
            let child = nodes[node][b];
            if child == 0 {
                // Node 0 is the root, so it is never a child. Create a new node.
                assert!(num_nodes < M, "code table is not prefix-free");
                nodes[node][b] = num_nodes as i16;
                node = num_nodes;
                num_nodes += 1;
            }
            else {
                assert!(child > 0, "code table is not prefix-free");
                node = child as usize;
            }
        }

        // Insert the leaf.
        let b = (code & 1) as usize;
        assert!(nodes[node][b] == 0, "code table is not prefix-free");
        nodes[node][b] = -(i as i16) - 1;
        i += 1;
    }

    // A complete code table has no unused branches.
    assert!(num_nodes == M, "code table is incomplete");
    let mut n = 0;
    while n < M {
        assert!(nodes[n][0] != 0 && nodes[n][1] != 0, "code table is incomplete");
        n += 1;
    }

    nodes
}

macro_rules! huff_tree {
    ($tree:ident, $nodes:ident, $codes:ident, $n:expr, $first:expr) => {
        static $nodes: [[i16; 2]; $n - 1] = build_tree::<$n, { $n - 1 }>(&$codes);
        pub(super) static $tree: HuffTree = HuffTree { nodes: &$nodes, first_value: $first };
    };
}

huff_tree!(IID_DF_FINE, IID_DF_FINE_NODES, HUFF_IID_DF_FINE_CODES, 61, -30);
huff_tree!(IID_DT_FINE, IID_DT_FINE_NODES, HUFF_IID_DT_FINE_CODES, 61, -30);
huff_tree!(IID_DF_DEFAULT, IID_DF_DEFAULT_NODES, HUFF_IID_DF_DEFAULT_CODES, 29, -14);
huff_tree!(IID_DT_DEFAULT, IID_DT_DEFAULT_NODES, HUFF_IID_DT_DEFAULT_CODES, 29, -14);
huff_tree!(ICC_DF, ICC_DF_NODES, HUFF_ICC_DF_CODES, 15, -7);
huff_tree!(ICC_DT, ICC_DT_NODES, HUFF_ICC_DT_CODES, 15, -7);
huff_tree!(IPD_DF, IPD_DF_NODES, HUFF_IPD_DF_CODES, 8, 0);
huff_tree!(IPD_DT, IPD_DT_NODES, HUFF_IPD_DT_CODES, 8, 0);
huff_tree!(OPD_DF, OPD_DF_NODES, HUFF_OPD_DF_CODES, 8, 0);
huff_tree!(OPD_DT, OPD_DT_NODES, HUFF_OPD_DT_CODES, 8, 0);

/// huff_iid_df[1] (Table 8.B.17), indices -30..=30, as (codeword, length).
#[rustfmt::skip]
pub(super) static HUFF_IID_DF_FINE_CODES: [(u32, u8); 61] = [
    (0b011111111010110100, 18), // -30
    (0b011111111010110101, 18), // -29
    (0b011111110101110110, 18), // -28
    (0b011111110101110111, 18), // -27
    (0b011111110101110100, 18), // -26
    (0b011111110101110101, 18), // -25
    (0b011111111010001010, 18), // -24
    (0b011111111010001011, 18), // -23
    (0b011111111010001000, 18), // -22
    (0b01111111010000000, 17), // -21
    (0b011111111010110110, 18), // -20
    (0b01111111010000010, 17), // -19
    (0b01111111010111000, 17), // -18
    (0b0111111101000010, 16), // -17
    (0b0111111110101110, 16), // -16
    (0b011111110101111, 15), // -15
    (0b01111111010001, 14), // -14
    (0b01111111101001, 14), // -13
    (0b0111111101001, 13), // -12
    (0b011111101010, 12), // -11
    (0b011111111011, 12), // -10
    (0b01111111011, 11), // -9
    (0b0111111011, 10), // -8
    (0b0111111111, 10), // -7
    (0b01111100, 8), // -6
    (0b0111100, 7), // -5
    (0b011100, 6), // -4
    (0b01100, 5), // -3
    (0b0000, 4), // -2
    (0b001, 3), // -1
    (0b1, 1), // 0
    (0b010, 3), // 1
    (0b0001, 4), // 2
    (0b01101, 5), // 3
    (0b011101, 6), // 4
    (0b0111101, 7), // 5
    (0b01111101, 8), // 6
    (0b011111100, 9), // 7
    (0b0111111100, 10), // 8
    (0b01111111100, 11), // 9
    (0b01111110100, 11), // 10
    (0b011111101011, 12), // 11
    (0b0111111101010, 13), // 12
    (0b01111111101010, 14), // 13
    (0b01111111010110, 14), // 14
    (0b011111111010000, 15), // 15
    (0b0111111110101111, 16), // 16
    (0b0111111101000011, 16), // 17
    (0b01111111010111001, 17), // 18
    (0b01111111010000011, 17), // 19
    (0b011111111010110111, 18), // 20
    (0b01111111010000001, 17), // 21
    (0b011111111010001001, 18), // 22
    (0b011111111010001110, 18), // 23
    (0b011111111010001111, 18), // 24
    (0b011111111010001100, 18), // 25
    (0b011111111010001101, 18), // 26
    (0b011111111010110010, 18), // 27
    (0b011111111010110011, 18), // 28
    (0b011111111010110000, 18), // 29
    (0b011111111010110001, 18), // 30
];

/// huff_iid_dt[1] (Table 8.B.17), indices -30..=30, as (codeword, length).
#[rustfmt::skip]
pub(super) static HUFF_IID_DT_FINE_CODES: [(u32, u8); 61] = [
    (0b0100111011010100, 16), // -30
    (0b0100111011010101, 16), // -29
    (0b0100111011001110, 16), // -28
    (0b0100111011001111, 16), // -27
    (0b0100111011001100, 16), // -26
    (0b0100111011010110, 16), // -25
    (0b0100111011011000, 16), // -24
    (0b0100111101000110, 16), // -23
    (0b0100111101100000, 16), // -22
    (0b010011100011000, 15), // -21
    (0b010011100011001, 15), // -20
    (0b010011101100100, 15), // -19
    (0b010011101100101, 15), // -18
    (0b010011101101101, 15), // -17
    (0b010011110110001, 15), // -16
    (0b01001110110111, 14), // -15
    (0b01001111010110, 14), // -14
    (0b0100111000111, 13), // -13
    (0b0100111101001, 13), // -12
    (0b0100111101101, 13), // -11
    (0b010011101110, 12), // -10
    (0b010011110111, 12), // -9
    (0b01001111000, 11), // -8
    (0b0100111001, 10), // -7
    (0b010011010, 9), // -6
    (0b010011111, 9), // -5
    (0b0100000, 7), // -4
    (0b010001, 6), // -3
    (0b01010, 5), // -2
    (0b011, 3), // -1
    (0b1, 1), // 0
    (0b00, 2), // 1
    (0b01011, 5), // 2
    (0b010010, 6), // 3
    (0b0100001, 7), // 4
    (0b01001100, 8), // 5
    (0b010011011, 9), // 6
    (0b0100111010, 10), // 7
    (0b01001111001, 11), // 8
    (0b01001110000, 11), // 9
    (0b010011101111, 12), // 10
    (0b010011100010, 12), // 11
    (0b0100111101010, 13), // 12
    (0b0100111011000, 13), // 13
    (0b01001111010111, 14), // 14
    (0b01001111010000, 14), // 15
    (0b010011110110010, 15), // 16
    (0b010011110100010, 15), // 17
    (0b010011100011010, 15), // 18
    (0b010011100011011, 15), // 19
    (0b0100111101100110, 16), // 20
    (0b0100111101100111, 16), // 21
    (0b0100111101100001, 16), // 22
    (0b0100111101000111, 16), // 23
    (0b0100111011011001, 16), // 24
    (0b0100111011010111, 16), // 25
    (0b0100111011001101, 16), // 26
    (0b0100111011010010, 16), // 27
    (0b0100111011010011, 16), // 28
    (0b0100111011010000, 16), // 29
    (0b0100111011010001, 16), // 30
];

/// huff_iid_df[0] (Table 8.B.18), indices -14..=14, as (codeword, length).
#[rustfmt::skip]
pub(super) static HUFF_IID_DF_DEFAULT_CODES: [(u32, u8); 29] = [
    (0b11111111111111011, 17), // -14
    (0b11111111111111100, 17), // -13
    (0b11111111111111101, 17), // -12
    (0b11111111111111010, 17), // -11
    (0b1111111111111100, 16), // -10
    (0b111111111111100, 15), // -9
    (0b1111111111101, 13), // -8
    (0b1111111110, 10), // -7
    (0b111111110, 9), // -6
    (0b1111110, 7), // -5
    (0b111100, 6), // -4
    (0b11101, 5), // -3
    (0b1101, 4), // -2
    (0b101, 3), // -1
    (0b0, 1), // 0
    (0b100, 3), // 1
    (0b1100, 4), // 2
    (0b11100, 5), // 3
    (0b111101, 6), // 4
    (0b111110, 6), // 5
    (0b11111110, 8), // 6
    (0b11111111110, 11), // 7
    (0b1111111111100, 13), // 8
    (0b11111111111100, 14), // 9
    (0b11111111111101, 14), // 10
    (0b111111111111101, 15), // 11
    (0b11111111111111110, 17), // 12
    (0b111111111111111110, 18), // 13
    (0b111111111111111111, 18), // 14
];

/// huff_iid_dt[0] (Table 8.B.18), indices -14..=14, as (codeword, length).
#[rustfmt::skip]
pub(super) static HUFF_IID_DT_DEFAULT_CODES: [(u32, u8); 29] = [
    (0b1111111111111111001, 19), // -14
    (0b1111111111111111010, 19), // -13
    (0b1111111111111111011, 19), // -12
    (0b11111111111111111000, 20), // -11
    (0b11111111111111111001, 20), // -10
    (0b11111111111111111010, 20), // -9
    (0b11111111111111101, 17), // -8
    (0b111111111111110, 15), // -7
    (0b111111111110, 12), // -6
    (0b1111111110, 10), // -5
    (0b11111110, 8), // -4
    (0b111110, 6), // -3
    (0b1110, 4), // -2
    (0b10, 2), // -1
    (0b0, 1), // 0
    (0b110, 3), // 1
    (0b11110, 5), // 2
    (0b1111110, 7), // 3
    (0b111111110, 9), // 4
    (0b11111111110, 11), // 5
    (0b1111111111110, 13), // 6
    (0b11111111111110, 14), // 7
    (0b11111111111111100, 17), // 8
    (0b1111111111111111000, 19), // 9
    (0b11111111111111111011, 20), // 10
    (0b11111111111111111100, 20), // 11
    (0b11111111111111111101, 20), // 12
    (0b11111111111111111110, 20), // 13
    (0b11111111111111111111, 20), // 14
];

/// huff_icc_df (Table 8.B.19), indices -7..=7, as (codeword, length).
#[rustfmt::skip]
pub(super) static HUFF_ICC_DF_CODES: [(u32, u8); 15] = [
    (0b11111111111111, 14), // -7
    (0b11111111111110, 14), // -6
    (0b111111111110, 12), // -5
    (0b1111111110, 10), // -4
    (0b1111110, 7), // -3
    (0b11110, 5), // -2
    (0b110, 3), // -1
    (0b0, 1), // 0
    (0b10, 2), // 1
    (0b1110, 4), // 2
    (0b111110, 6), // 3
    (0b11111110, 8), // 4
    (0b111111110, 9), // 5
    (0b11111111110, 11), // 6
    (0b1111111111110, 13), // 7
];

/// huff_icc_dt (Table 8.B.19), indices -7..=7, as (codeword, length).
#[rustfmt::skip]
pub(super) static HUFF_ICC_DT_CODES: [(u32, u8); 15] = [
    (0b11111111111110, 14), // -7
    (0b1111111111110, 13), // -6
    (0b11111111110, 11), // -5
    (0b111111110, 9), // -4
    (0b1111110, 7), // -3
    (0b11110, 5), // -2
    (0b110, 3), // -1
    (0b0, 1), // 0
    (0b10, 2), // 1
    (0b1110, 4), // 2
    (0b111110, 6), // 3
    (0b11111110, 8), // 4
    (0b1111111110, 10), // 5
    (0b111111111110, 12), // 6
    (0b11111111111111, 14), // 7
];

/// huff_ipd_df (Table 8.B.20), indices 0..=7, as (codeword, length).
#[rustfmt::skip]
pub(super) static HUFF_IPD_DF_CODES: [(u32, u8); 8] = [
    (0b1, 1), // 0
    (0b000, 3), // 1
    (0b0110, 4), // 2
    (0b0100, 4), // 3
    (0b0010, 4), // 4
    (0b0011, 4), // 5
    (0b0101, 4), // 6
    (0b0111, 4), // 7
];

/// huff_ipd_dt (Table 8.B.20), indices 0..=7, as (codeword, length).
#[rustfmt::skip]
pub(super) static HUFF_IPD_DT_CODES: [(u32, u8); 8] = [
    (0b1, 1), // 0
    (0b010, 3), // 1
    (0b0010, 4), // 2
    (0b00011, 5), // 3
    (0b00010, 5), // 4
    (0b0000, 4), // 5
    (0b0011, 4), // 6
    (0b011, 3), // 7
];

/// huff_opd_df (Table 8.B.21), indices 0..=7, as (codeword, length).
#[rustfmt::skip]
pub(super) static HUFF_OPD_DF_CODES: [(u32, u8); 8] = [
    (0b1, 1), // 0
    (0b001, 3), // 1
    (0b0110, 4), // 2
    (0b0100, 4), // 3
    (0b01111, 5), // 4
    (0b01110, 5), // 5
    (0b0101, 4), // 6
    (0b000, 3), // 7
];

/// huff_opd_dt (Table 8.B.21), indices 0..=7, as (codeword, length).
#[rustfmt::skip]
pub(super) static HUFF_OPD_DT_CODES: [(u32, u8); 8] = [
    (0b1, 1), // 0
    (0b010, 3), // 1
    (0b0001, 4), // 2
    (0b00111, 5), // 3
    (0b00110, 5), // 4
    (0b0000, 4), // 5
    (0b0010, 4), // 6
    (0b011, 3), // 7
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A code table with its name, decoding tree, and value range.
    type Table = (&'static str, &'static [(u32, u8)], &'static HuffTree, i32, i32);

    /// All code tables.
    fn all_tables() -> [Table; 10] {
        [
            ("iid_df_fine", &HUFF_IID_DF_FINE_CODES, &IID_DF_FINE, -30, 30),
            ("iid_dt_fine", &HUFF_IID_DT_FINE_CODES, &IID_DT_FINE, -30, 30),
            ("iid_df_default", &HUFF_IID_DF_DEFAULT_CODES, &IID_DF_DEFAULT, -14, 14),
            ("iid_dt_default", &HUFF_IID_DT_DEFAULT_CODES, &IID_DT_DEFAULT, -14, 14),
            ("icc_df", &HUFF_ICC_DF_CODES, &ICC_DF, -7, 7),
            ("icc_dt", &HUFF_ICC_DT_CODES, &ICC_DT, -7, 7),
            ("ipd_df", &HUFF_IPD_DF_CODES, &IPD_DF, 0, 7),
            ("ipd_dt", &HUFF_IPD_DT_CODES, &IPD_DT, 0, 7),
            ("opd_df", &HUFF_OPD_DF_CODES, &OPD_DF, 0, 7),
            ("opd_dt", &HUFF_OPD_DT_CODES, &OPD_DT, 0, 7),
        ]
    }

    fn decode_bits(tree: &HuffTree, code: u32, len: u8) -> (Result<i32>, u8) {
        let mut pos = len;
        let mut used = 0;
        let res = tree.decode(|| {
            if pos == 0 {
                return symphonia_core::errors::decode_error("out of bits");
            }
            pos -= 1;
            used += 1;
            Ok((code >> pos) & 1 == 1)
        });
        (res, used)
    }

    #[test]
    fn verify_counts_and_lengths() {
        for (name, codes, _, lo, hi) in all_tables() {
            assert_eq!(codes.len() as i32, hi - lo + 1, "{name}");
            for &(code, len) in codes {
                assert!((1..=20).contains(&len), "{name}");
                assert_eq!(code >> len, 0, "{name}");
            }
        }
        let max_len = |codes: &[(u32, u8)]| codes.iter().map(|c| c.1).max();
        assert_eq!(max_len(&HUFF_IID_DF_FINE_CODES), Some(18));
        assert_eq!(max_len(&HUFF_IID_DT_FINE_CODES), Some(16));
        assert_eq!(max_len(&HUFF_IID_DF_DEFAULT_CODES), Some(18));
        assert_eq!(max_len(&HUFF_IID_DT_DEFAULT_CODES), Some(20));
        assert_eq!(max_len(&HUFF_ICC_DF_CODES), Some(14));
        assert_eq!(max_len(&HUFF_ICC_DT_CODES), Some(14));
        assert_eq!(max_len(&HUFF_IPD_DF_CODES), Some(4));
        assert_eq!(max_len(&HUFF_IPD_DT_CODES), Some(5));
        assert_eq!(max_len(&HUFF_OPD_DF_CODES), Some(5));
        assert_eq!(max_len(&HUFF_OPD_DT_CODES), Some(5));
    }

    #[test]
    fn verify_kraft_equality() {
        for (name, codes, _, _, _) in all_tables() {
            let sum: u64 = codes.iter().map(|&(_, len)| 1u64 << (32 - u32::from(len))).sum();
            assert_eq!(sum, 1u64 << 32, "{name}");
        }
    }

    #[test]
    fn verify_prefix_free() {
        for (name, codes, _, _, _) in all_tables() {
            for (i, &(ci, li)) in codes.iter().enumerate() {
                for (j, &(cj, lj)) in codes.iter().enumerate() {
                    if i != j && li <= lj {
                        assert_ne!(cj >> (lj - li), ci, "{name}: {i} is a prefix of {j}");
                    }
                }
            }
        }
    }

    #[test]
    fn verify_decode_all_codewords() {
        for (name, codes, tree, lo, _) in all_tables() {
            for (i, &(code, len)) in codes.iter().enumerate() {
                let (res, used) = decode_bits(tree, code, len);
                assert_eq!(res.ok(), Some(lo + i as i32), "{name}");
                assert_eq!(used, len, "{name}");
            }
        }
    }

    #[test]
    fn verify_spot_values() {
        // Spot checks against Tables 8.B.17 to 8.B.21.
        assert_eq!(HUFF_IID_DF_FINE_CODES[30], (0b1, 1));
        assert_eq!(HUFF_IID_DF_FINE_CODES[0], (0b011111111010110100, 18));
        assert_eq!(HUFF_IID_DT_FINE_CODES[60], (0b0100111011010001, 16));
        assert_eq!(HUFF_IID_DT_FINE_CODES[31], (0b00, 2));
        assert_eq!(HUFF_IID_DF_DEFAULT_CODES[14 - 11], (0b11111111111111010, 17));
        assert_eq!(HUFF_IID_DT_DEFAULT_CODES[14 - 11], (0b11111111111111111000, 20));
        assert_eq!(HUFF_ICC_DF_CODES[0], (0b11111111111111, 14));
        assert_eq!(HUFF_ICC_DT_CODES[14], (0b11111111111111, 14));
        assert_eq!(HUFF_IPD_DT_CODES[3], (0b00011, 5));
        assert_eq!(HUFF_OPD_DF_CODES[7], (0b000, 3));
    }

    #[test]
    fn verify_decode_out_of_bits() {
        // A truncated codeword is reported as an error.
        let (res, used) = decode_bits(&IID_DT_DEFAULT, 0b1111, 4);
        assert!(res.is_err());
        assert_eq!(used, 4);
    }
}
