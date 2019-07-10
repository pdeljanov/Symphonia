// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

pub mod bits {

    /// Sign extends an arbitrary, 8-bit or less, signed two's complement integer stored within an u8
    /// to a full width i8.
    #[inline(always)]
    pub fn sign_extend_leq8_to_i8(value: u8, width: u32) -> i8 {
        // Rust uses an arithmetic shift right (the original sign bit is repeatedly shifted on) for
        // signed integer types. Therefore, shift the value to the right-hand side of the integer,
        // then shift it back to extend the sign bit.
        (value.wrapping_shl(8 - width) as i8).wrapping_shr(8 - width)
    }

    /// Sign extends an arbitrary, 16-bit or less, signed two's complement integer stored within an u16
    /// to a full width i16.
    #[inline(always)]
    pub fn sign_extend_leq16_to_i16(value: u16, width: u32) -> i16 {
        (value.wrapping_shl(16 - width) as i16).wrapping_shr(16 - width)
    }

    /// Sign extends an arbitrary, 32-bit or less, signed two's complement integer stored within an u32
    /// to a full width i32.
    #[inline(always)]
    pub fn sign_extend_leq32_to_i32(value: u32, width: u32) -> i32 {
        (value.wrapping_shl(32 - width) as i32).wrapping_shr(32 - width)
    }

    /// Sign extends an arbitrary, 64-bit or less, signed two's complement integer stored within an u64
    /// to a full width i64.
    #[inline(always)]
    pub fn sign_extend_leq64_to_i64(value: u64, width: u32) -> i64 {
        (value.wrapping_shl(64 - width) as i64).wrapping_shr(64 - width)
    }

    /// Masks the bit at the specified bit index.
    #[inline(always)]
    pub fn mask_at(idx: u32) -> u8 {
        debug_assert!(idx <= 7);
        1 << idx
    }

    /// Masks all bits with an index greater than or equal to idx.
    #[inline(always)]
    pub fn mask_upper_eq(idx: u32) -> u8 {
        debug_assert!(idx <= 7);
        !((1 << idx) - 1)
    }

    #[inline(always)]
    pub fn mask_upper(idx: u32) -> u8 {
        debug_assert!(idx <= 7);
        !((1 << idx) - 1) ^ (1 << idx)
    }

    /// Masks all bits with an index less than or equal to idx.
    #[inline(always)]
    pub fn mask_lower_eq(idx: u32) -> u8 {
        debug_assert!(idx <= 7);
        ((1 << idx) - 1) ^ (1 << idx)
    }

    #[inline(always)]
    pub fn mask_lower(idx: u32) -> u8 {
        debug_assert!(idx <= 7);
        ((1 << idx) - 1)
    }

    /// Masks out all bits in positions less than upper, but greater than or equal to lower
    /// (upper < bit <= lower)
    #[inline(always)]
    pub fn mask_range(upper: u32, lower: u32) -> u8 {
        debug_assert!(upper <= 8);
        debug_assert!(lower <= 8);
        (((0xff as u32) << upper) ^ ((0xff as u32) << lower)) as u8
    }

    #[test]
    fn verify_masks() {
        assert_eq!(mask_at(0), 0b0000_0001);
        assert_eq!(mask_at(1), 0b0000_0010);
        assert_eq!(mask_at(2), 0b0000_0100);
        assert_eq!(mask_at(3), 0b0000_1000);
        assert_eq!(mask_at(4), 0b0001_0000);
        assert_eq!(mask_at(5), 0b0010_0000);
        assert_eq!(mask_at(6), 0b0100_0000);
        assert_eq!(mask_at(7), 0b1000_0000);

        assert_eq!(mask_upper(0), 0b1111_1110);
        assert_eq!(mask_upper(1), 0b1111_1100);
        assert_eq!(mask_upper(2), 0b1111_1000);
        assert_eq!(mask_upper(3), 0b1111_0000);
        assert_eq!(mask_upper(4), 0b1110_0000);
        assert_eq!(mask_upper(5), 0b1100_0000);
        assert_eq!(mask_upper(6), 0b1000_0000);
        assert_eq!(mask_upper(7), 0b0000_0000);

        assert_eq!(mask_upper_eq(0), 0b1111_1111);
        assert_eq!(mask_upper_eq(1), 0b1111_1110);
        assert_eq!(mask_upper_eq(2), 0b1111_1100);
        assert_eq!(mask_upper_eq(3), 0b1111_1000);
        assert_eq!(mask_upper_eq(4), 0b1111_0000);
        assert_eq!(mask_upper_eq(5), 0b1110_0000);
        assert_eq!(mask_upper_eq(6), 0b1100_0000);
        assert_eq!(mask_upper_eq(7), 0b1000_0000);

        assert_eq!(mask_lower(0), 0b0000_0000);
        assert_eq!(mask_lower(1), 0b0000_0001);
        assert_eq!(mask_lower(2), 0b0000_0011);
        assert_eq!(mask_lower(3), 0b0000_0111);
        assert_eq!(mask_lower(4), 0b0000_1111);
        assert_eq!(mask_lower(5), 0b0001_1111);
        assert_eq!(mask_lower(6), 0b0011_1111);
        assert_eq!(mask_lower(7), 0b0111_1111);

        assert_eq!(mask_lower_eq(0), 0b0000_0001);
        assert_eq!(mask_lower_eq(1), 0b0000_0011);
        assert_eq!(mask_lower_eq(2), 0b0000_0111);
        assert_eq!(mask_lower_eq(3), 0b0000_1111);
        assert_eq!(mask_lower_eq(4), 0b0001_1111);
        assert_eq!(mask_lower_eq(5), 0b0011_1111);
        assert_eq!(mask_lower_eq(6), 0b0111_1111);
        assert_eq!(mask_lower_eq(7), 0b1111_1111);

        assert_eq!(mask_range(0, 0), 0b0000_0000);
        assert_eq!(mask_range(1, 1), 0b0000_0000);
        assert_eq!(mask_range(7, 7), 0b0000_0000);
        assert_eq!(mask_range(1, 0), 0b0000_0001);
        assert_eq!(mask_range(2, 0), 0b0000_0011);
        assert_eq!(mask_range(7, 0), 0b0111_1111);
        assert_eq!(mask_range(5, 2), 0b0001_1100);
        assert_eq!(mask_range(7, 2), 0b0111_1100);
        assert_eq!(mask_range(8, 2), 0b1111_1100);
    }
}