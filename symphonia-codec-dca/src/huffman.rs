// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::sync::OnceLock;

use symphonia_core::errors::{Result, decode_error};

use crate::bits::Bits;
use crate::spec_tables::*;

/// A prefix code stored as a binary tree.
pub(crate) struct Tree {
    nodes: Vec<Node>,
}

#[derive(Clone, Copy, Default)]
struct Node {
    child: [u32; 2],
    value: Option<i16>,
}

impl Tree {
    fn new(book: &[(u8, u16, i16)]) -> Tree {
        let mut nodes = vec![Node::default()];

        for &(len, code, value) in book {
            let mut at = 0usize;
            for bit in (0..len).rev() {
                let b = usize::from((code >> bit) & 1);
                if nodes[at].child[b] == 0 {
                    nodes.push(Node::default());
                    nodes[at].child[b] = (nodes.len() - 1) as u32;
                }
                at = nodes[at].child[b] as usize;
            }
            nodes[at].value = Some(value);
        }

        Tree { nodes }
    }

    pub(crate) fn read(&self, bs: &mut Bits<'_>) -> Result<i16> {
        let mut at = 0usize;
        loop {
            if let Some(v) = self.nodes[at].value {
                return Ok(v);
            }
            let next = self.nodes[at].child[bs.u(1)? as usize];
            if next == 0 {
                return decode_error("dca: invalid huffman code");
            }
            at = next as usize;
        }
    }
}

pub(crate) struct Books {
    /// Quantization index code books by ABITS - 1 (0..10) and SEL (0..6).
    pub(crate) audio: Vec<Vec<Option<Tree>>>,
    pub(crate) bit_alloc: Vec<Tree>,
    pub(crate) transient: Vec<Tree>,
    pub(crate) scale: Vec<Tree>,
}

pub(crate) fn books() -> &'static Books {
    static BOOKS: OnceLock<Books> = OnceLock::new();
    BOOKS.get_or_init(build)
}

fn build() -> Books {
    let t = |b: &[(u8, u16, i16)]| Some(Tree::new(b));

    let audio = vec![
        vec![t(&HUFF_A3)],
        vec![t(&HUFF_A5), t(&HUFF_B5), t(&HUFF_C5)],
        vec![t(&HUFF_A7), t(&HUFF_B7), t(&HUFF_C7)],
        vec![t(&HUFF_A9), t(&HUFF_B9), t(&HUFF_C9)],
        vec![t(&HUFF_A13), t(&HUFF_B13), t(&HUFF_C13)],
        vec![
            t(&HUFF_A17),
            t(&HUFF_B17),
            t(&HUFF_C17),
            t(&HUFF_D17),
            t(&HUFF_E17),
            t(&HUFF_F17),
            t(&HUFF_G17),
        ],
        vec![
            t(&HUFF_A25),
            t(&HUFF_B25),
            t(&HUFF_C25),
            t(&HUFF_D25),
            t(&HUFF_E25),
            t(&HUFF_F25),
            t(&HUFF_G25),
        ],
        vec![
            t(&HUFF_A33),
            t(&HUFF_B33),
            t(&HUFF_C33),
            t(&HUFF_D33),
            t(&HUFF_E33),
            t(&HUFF_F33),
            t(&HUFF_G33),
        ],
        vec![
            t(&HUFF_A65),
            t(&HUFF_B65),
            t(&HUFF_C65),
            t(&HUFF_D65),
            t(&HUFF_E65),
            t(&HUFF_F65),
            t(&HUFF_G65),
        ],
        vec![
            t(&HUFF_A129),
            t(&HUFF_B129),
            t(&HUFF_C129),
            t(&HUFF_D129),
            t(&HUFF_E129),
            t(&HUFF_F129),
            t(&HUFF_G129),
        ],
    ];

    let tree = |b: &[(u8, u16, i16)]| Tree::new(b);

    Books {
        audio,
        bit_alloc: vec![
            tree(&HUFF_A12),
            tree(&HUFF_B12),
            tree(&HUFF_C12),
            tree(&HUFF_D12),
            tree(&HUFF_E12),
        ],
        transient: vec![tree(&HUFF_A4), tree(&HUFF_B4), tree(&HUFF_C4), tree(&HUFF_D4)],
        scale: vec![
            tree(&HUFF_SA129),
            tree(&HUFF_SB129),
            tree(&HUFF_SC129),
            tree(&HUFF_SD129),
            tree(&HUFF_SE129),
        ],
    }
}

#[cfg(test)]
mod tests {
    use symphonia_core::io::BitReaderLtr;

    use super::*;

    fn round_trip(book: &[(u8, u16, i16)]) {
        let tree = Tree::new(book);
        let kraft: f64 = book.iter().map(|&(len, _, _)| 0.5f64.powi(i32::from(len))).sum();
        assert!((kraft - 1.0).abs() < 1e-9);

        for &(len, code, value) in book {
            let bytes = (u32::from(code) << (32 - u32::from(len))).to_be_bytes();
            let mut bs = Bits(BitReaderLtr::new(&bytes));
            assert_eq!(tree.read(&mut bs).unwrap(), value);
        }
    }

    #[test]
    fn code_books_are_complete_prefix_codes() {
        round_trip(&HUFF_A3);
        round_trip(&HUFF_D4);
        round_trip(&HUFF_C13);
        round_trip(&HUFF_G25);
        round_trip(&HUFF_E12);
        round_trip(&HUFF_A65);
        round_trip(&HUFF_D65);
        round_trip(&HUFF_SA129);
        round_trip(&HUFF_D129);
        round_trip(&HUFF_G129);
    }
}
