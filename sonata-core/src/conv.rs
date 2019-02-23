// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This library is free software; you can redistribute it and/or
// modify it under the terms of the GNU Lesser General Public
// License as published by the Free Software Foundation; either
// version 2.1 of the License, or (at your option) any later version.
//
// This library is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
// Lesser General Public License for more details.
//
// You should have received a copy of the GNU Lesser General Public
// License along with this library; if not, write to the Free Software
// Foundation, Inc., 51 Franklin Street, Fifth Floor, Boston, MA  02110-1301 USA

use crate::sample::{u24, i24, Sample};

/// Converts a sample of one type and value to another type with an equivalent value.
/// 
/// This may be a lossy conversion if converting from a sample type of higher precision to one of lower precision. No 
/// dither is applied.
pub trait FromSample<F> {
    fn from_sample(val: F) -> Self;
}

pub trait IntoSample<T> {
    fn into_sample(self) -> T;
}

impl<F, T: FromSample<F>> IntoSample<T> for F {
    #[inline]
    fn into_sample(self) -> T {
        T::from_sample(self)
    }
}

pub trait ConvertibleSample<S> : FromSample<S> + IntoSample<S> {}

/// Clamps the given value to the [0, 255] range.
pub fn clamp_u8(val: u16) -> u8 {
    if val & !0xff == 0 {
        val as u8
    }
    else {
        0xff
    }
}

/// Clamps the given value to the [-128,127] range.
pub fn clamp_i8(val: i16) -> i8 {
    // Add 128 (0x80) to the given value, val, to make the i8 range of [-128,127] map to [0,255]. Valid negative numbers
    // are now positive so all bits above the 8th bit should be 0. Check this by ANDing with 0xffffff00 (!0xff). If val
    // wraps, the test is still valid as it'll wrap around to the other numerical limit +/- 128, which is still well 
    // outside the limits of an i8.
    if val.wrapping_add(0x80) & !0xff == 0 {
        val as i8
    }
    else {
        // The given value was determined to be outside the valid numerical range of i8. 
        //
        // Shift right all the magnitude bits of val, leaving val to be either 0xff if val was negative (sign bit 
        // was 1), or 0x00 if val was positive (sign bit was 0). Xor the shift value with 0x7f (the positive limit) to 
        // obtain the appropriate numerical limit.
        //
        //  E.g., 0x7f ^ 0x00 = 0x7f (127)
        //  E.g., 0x7f ^ 0xff = 0x10 (-128)
        0x7f ^ val.wrapping_shr(15) as i8
    }
}

/// Clamps the given value to the [0, 65535] range.
pub fn clamp_u16(val: u32) -> u16 {
    if val & !0xffff == 0 {
        val as u16
    }
    else {
        0xffff
    }
}

/// Clamps the given value to the [-32767,32768] range.
pub fn clamp_i16(val: i32) -> i16 {
    if val.wrapping_add(0x8000) & !0xffff == 0 {
        val as i16
    }
    else {
        0x7fff ^ val.wrapping_shr(31) as i16
    }
}

/// Clamps the given value to the [0, 16777215] range.
pub fn clamp_u24(val: u32) -> u32 {
    if val & !0x00ffffff == 0 {
        val
    }
    else {
        0x00ffffff
    }
}

/// Clamps the given value to the [-8388608, 8388607] range.
pub fn clamp_i24(val: i32) -> i32 {
    if val.wrapping_add(0x800000) & !0xffffff == 0 {
        val as i32
    }
    else {
        0x7fffff ^ val.wrapping_shr(31) as i32
    }
}

/// Clamps the given value to the [0, 4294967295] range.
pub fn clamp_u32(val: u64) -> u32 {
    if val & !0xffffffff == 0 {
        val as u32
    }
    else {
        0xffffffff
    }
}

/// Clamps the given value to the [-2147483648, 2147483647] range.
pub fn clamp_i32(val: i64) -> i32 {
    if val.wrapping_add(0x80000000) & !0xffffffff == 0 {
        val as i32
    }
    else {
        0x7fffffff ^ val.wrapping_shr(63) as i32
    }
}

/// Clamps the given value to the [-1.0, 1.0] range.
pub fn clamp_f32(val: f32) -> f32 {
    if val > 1.0 {
        1.0
    }
    else if val < -1.0 {
        -1.0
    }
    else {
        val
    }
}

/// Clamps the given value to the [-1.0, 1.0] range.
pub fn clamp_f64(val: f64) -> f64 {
    if val > 1.0 {
        1.0
    }
    else if val < -1.0 {
        -1.0
    }
    else {
        val
    }
}

#[test]
fn test_clipping() {
    use std::{u8, i8, u16, i16, u32, i32, u64, i64};

    assert_eq!(clamp_u8(256u16),   u8::MAX);
    assert_eq!(clamp_u8(u16::MAX), u8::MAX);

    assert_eq!(clamp_i8(  128i16), i8::MAX);
    assert_eq!(clamp_i8( -129i16), i8::MIN);
    assert_eq!(clamp_i8(i16::MAX), i8::MAX);
    assert_eq!(clamp_i8(i16::MIN), i8::MIN);

    assert_eq!(clamp_u16(65536u32), u16::MAX);
    assert_eq!(clamp_u16(u32::MAX), u16::MAX);

    assert_eq!(clamp_i16( 32_768i32), i16::MAX);
    assert_eq!(clamp_i16(-32_769i32), i16::MIN);
    assert_eq!(clamp_i16(  i32::MAX), i16::MAX);
    assert_eq!(clamp_i16(  i32::MIN), i16::MIN);

    assert_eq!(clamp_u32(4_294_967_296u64), u32::MAX);
    assert_eq!(clamp_u32(        u64::MAX), u32::MAX);

    assert_eq!(clamp_i32( 2_147_483_648i64), i32::MAX);
    assert_eq!(clamp_i32(-2_147_483_649i64), i32::MIN);
    assert_eq!(clamp_i32(         i64::MAX), i32::MAX);
    assert_eq!(clamp_i32(         i64::MIN), i32::MIN);
}

macro_rules! converter {
    ($to:ty, $from:ty, $sample:ident, $func:expr) => (
        impl FromSample<$from> for $to {
            #[inline]
            fn from_sample($sample: $from) -> Self {
                $func
            }
        }
    )
}

// Conversions to u8
converter!(u8, u8 , s, s);
converter!(u8, u16, s, (s >> 8) as u8);
converter!(u8, u24, s, ((s.inner() & 0x00ffffff) >> 16) as u8);
converter!(u8, u32, s, (s >> 24) as u8);
converter!(u8, i8 , s, (s as u8).wrapping_add(0x80));
converter!(u8, i16, s, ((s as u16).wrapping_add(0x8000) >> 8) as u8);
converter!(u8, i24, s, (((s.inner() as u32).wrapping_add(0x800000) & 0x00ffffff) >> 16) as u8);
converter!(u8, i32, s, ((s as u32).wrapping_add(0x80000000) >> 24) as u8);
converter!(u8, f32, s, {
    let s16 = (clamp_f32(s) * 128.0).round() as i16;
    clamp_u8((s16 + 0x80) as u16)
});
converter!(u8, f64, s, {
    let s16 = (clamp_f64(s) * 128.0).round() as i16;
    clamp_u8((s16 + 0x80) as u16)
});

// Conversions to u16
converter!(u16, u8 , s, (s as u16) << 8);
converter!(u16, u16, s, s);
converter!(u16, u24, s, ((s.inner() & 0x00ffffff) >> 8) as u16);
converter!(u16, u32, s, (s >> 16) as u16);
converter!(u16, i8 , s, ((s as u8).wrapping_add(0x80) as u16) << 8);
converter!(u16, i16, s, (s as u16).wrapping_add(0x8000));
converter!(u16, i24, s, (((s.inner() as u32).wrapping_add(0x800000) & 0x00ffffff) >> 8) as u16);
converter!(u16, i32, s, ((s as u32).wrapping_add(0x80000000) >> 16) as u16);
converter!(u16, f32, s, {
    let s32 = (clamp_f32(s) * 32_768.0).round() as i32;
    clamp_u16((s32 + 0x8000) as u32)
});
converter!(u16, f64, s, {
    let s32 = (clamp_f64(s) * 32_768.0).round() as i32;
    clamp_u16((s32 + 0x8000) as u32)
});

// Conversions to u24
converter!(u24, u8 , s, u24::from((s as u32) << 16));
converter!(u24, u16, s, u24::from((s as u32) << 8));
converter!(u24, u24, s, u24::from(s.inner() & 0x00ffffff));
converter!(u24, u32, s, u24::from(s >> 8));
converter!(u24, i8 , s, u24::from(((s as u8).wrapping_add(0x80) as u32) << 16));
converter!(u24, i16, s, u24::from(((s as u16).wrapping_add(0x8000) as u32) << 8));
converter!(u24, i24, s, u24::from(((s.inner() as u32).wrapping_add(0x800000) & 0x00ffffff) as u32));
converter!(u24, i32, s, u24::from((s as u32).wrapping_add(0x80000000) >> 8));
converter!(u24, f32, s, {
    let s32 = (clamp_f32(s) * 8_388_608.0).round() as i32;
    u24::from(clamp_u24((s32 + 0x800000) as u32))
});
converter!(u24, f64, s, {
    let s32 = (clamp_f64(s) * 8_388_608.0).round() as i32;
    u24::from(clamp_u24((s32 + 0x800000) as u32))
});

// Conversions to u32
converter!(u32, u8 , s, (s as u32) << 24);
converter!(u32, u16, s, (s as u32) << 16);
converter!(u32, u24, s, (s.inner() & 0x00ffffff) << 8);
converter!(u32, u32, s, s);
converter!(u32, i8 , s, ((s as u8).wrapping_add(0x80) as u32) << 24);
converter!(u32, i16, s, ((s as u16).wrapping_add(0x8000) as u32) << 16);
converter!(u32, i24, s, (((s.inner() as u32).wrapping_add(0x800000) & 0x00ffffff) as u32) << 8);
converter!(u32, i32, s, (s as u32).wrapping_add(0x80000000));
converter!(u32, f32, s, {
    let s64 = (clamp_f32(s) * 2_147_483_648.0).round() as i64;
    clamp_u32((s64 + 0x80000000) as u64)
});
converter!(u32, f64, s, {
    let s64 = (clamp_f64(s) * 2_147_483_648.0).round() as i64;
    clamp_u32((s64 + 0x80000000) as u64)
});

// Conversions to i8
converter!(i8, u8 , s, s.wrapping_add(0x80) as i8);
converter!(i8, u16, s, (s.wrapping_add(0x8000) >> 8) as i8);
converter!(i8, u24, s, (s.inner().wrapping_add(0x800000) >> 16) as i8);
converter!(i8, u32, s, (s.wrapping_add(0x80000000) >> 24) as i8);
converter!(i8, i8 , s, s);
converter!(i8, i16, s, (s >> 8) as i8);
converter!(i8, i24, s, ((s.inner() & 0x00ffffff) >> 16) as i8);
converter!(i8, i32, s, (s >> 24) as i8);
converter!(i8, f32, s, clamp_i8((clamp_f32(s) * 128.0).round() as i16));
converter!(i8, f64, s, clamp_i8((clamp_f64(s) * 128.0).round() as i16));

// Conversions to i16
converter!(i16, u8 , s, (s.wrapping_add(0x80) as i16) << 8);
converter!(i16, u16, s, s.wrapping_add(0x8000) as i16);
converter!(i16, u24, s, (s.inner().wrapping_add(0x800000) >> 8) as i16);
converter!(i16, u32, s, (s.wrapping_add(0x80000000) >> 16) as i16);
converter!(i16, i8 , s, (s as i16) << 8);
converter!(i16, i16, s, s);
converter!(i16, i24, s, ((s.inner() & 0x00ffffff) >> 8) as i16);
converter!(i16, i32, s, (s >> 16) as i16);
converter!(i16, f32, s, clamp_i16((clamp_f32(s) * 32_768.0).round() as i32));
converter!(i16, f64, s, clamp_i16((clamp_f64(s) * 32_768.0).round() as i32));

// Conversions to i24
converter!(i24, u8 , s, i24::from((s as i32 - 0x80) << 16));
converter!(i24, u16, s, i24::from((s as i32 - 0x8000) << 8));
converter!(i24, u24, s, i24::from((s.inner() & 0x00ffffff) as i32 - 0x800000));
converter!(i24, u32, s, i24::from((s.wrapping_add(0x80000000) as i32) >> 8));
converter!(i24, i8 , s, i24::from((s as i32) << 16));
converter!(i24, i16, s, i24::from((s as i32) << 8));
converter!(i24, i24, s, s);
converter!(i24, i32, s, i24::from(s >> 8));
converter!(i24, f32, s, i24::from(clamp_i24((clamp_f32(s) * 16_777_216.0).round() as i32)));
converter!(i24, f64, s, i24::from(clamp_i24((clamp_f64(s) * 16_777_216.0).round() as i32)));

// Conversions to i32
converter!(i32, u8 , s, ((s as i32 - 0x80) << 24));
converter!(i32, u16, s, ((s as i32 - 0x8000) << 16));
converter!(i32, u24, s, ((s.inner() & 0x00ffffff) as i32 - 0x800000) << 8);
converter!(i32, u32, s, s.wrapping_add(0x80000000) as i32);
converter!(i32, i8 , s, (s as i32) << 24);
converter!(i32, i16, s, (s as i32) << 16);
converter!(i32, i24, s, (s.inner() & 0x00ffffff) << 8);
converter!(i32, i32, s, s);
converter!(i32, f32, s, clamp_i32((clamp_f32(s) * 2_147_483_648.0).round() as i64));
converter!(i32, f64, s, clamp_i32((clamp_f64(s) * 2_147_483_648.0).round() as i64));

// Conversions to f32
converter!(f32, u8 , s, i8::from_sample(s) as f32 / 128.0);
converter!(f32, u16, s, i16::from_sample(s) as f32 / 32_768.0);
converter!(f32, u24, s, i24::from_sample(s).inner() as f32 / 16_777_216.0);
converter!(f32, u32, s, i32::from_sample(s) as f32 / 2_147_483_648.0);
converter!(f32, i8 , s, s as f32 / 128.0);
converter!(f32, i16, s, s as f32 / 32_768.0);
converter!(f32, i24, s, s.inner() as f32 / 16_777_216.0);
converter!(f32, i32, s, s as f32 / 2_147_483_648.0);
converter!(f32, f32, s, s);
converter!(f32, f64, s, s as f32);

// Conversions to f64
converter!(f64, u8 , s, i8::from_sample(s) as f64 / 128.0);
converter!(f64, u16, s, i16::from_sample(s) as f64 / 32_768.0);
converter!(f64, u24, s, i24::from_sample(s).inner() as f64 / 16_777_216.0);
converter!(f64, u32, s, i32::from_sample(s) as f64 / 2_147_483_648.0);
converter!(f64, i8 , s, s as f64 / 128.0);
converter!(f64, i16, s, s as f64 / 32_768.0);
converter!(f64, i24, s, s.inner() as f64 / 16_777_216.0);
converter!(f64, i32, s, s as f64 / 2_147_483_648.0);
converter!(f64, f32, s, s as f64);
converter!(f64, f64, s, s);

#[test]
fn verify_u8_from_sample() {
    use std::{u8, i8, u16, i16, u32, i32};

    assert_eq!(u8::from_sample(u8::MAX), u8::MAX);
    assert_eq!(u8::from_sample(u8::MID), u8::MID);
    assert_eq!(u8::from_sample(u8::MIN), u8::MIN);

    assert_eq!(u8::from_sample(u16::MAX), u8::MAX);
    assert_eq!(u8::from_sample(u16::MID), u8::MID);
    assert_eq!(u8::from_sample(u16::MIN), u8::MIN);

    assert_eq!(u8::from_sample(u24::MAX), u8::MAX);
    assert_eq!(u8::from_sample(u24::MID), u8::MID);
    assert_eq!(u8::from_sample(u24::MIN), u8::MIN);

    assert_eq!(u8::from_sample(u32::MAX), u8::MAX);
    assert_eq!(u8::from_sample(u32::MID), u8::MID);
    assert_eq!(u8::from_sample(u32::MIN), u8::MIN);

    assert_eq!(u8::from_sample(i8::MAX), u8::MAX);
    assert_eq!(u8::from_sample(i8::MID), u8::MID);
    assert_eq!(u8::from_sample(i8::MIN), u8::MIN);

    assert_eq!(u8::from_sample(i16::MAX), u8::MAX);
    assert_eq!(u8::from_sample(i16::MID), u8::MID);
    assert_eq!(u8::from_sample(i16::MIN), u8::MIN);

    assert_eq!(u8::from_sample(i24::MAX), u8::MAX);
    assert_eq!(u8::from_sample(i24::MID), u8::MID);
    assert_eq!(u8::from_sample(i24::MIN), u8::MIN);

    assert_eq!(u8::from_sample(i32::MAX), u8::MAX);
    assert_eq!(u8::from_sample(i32::MID), u8::MID);
    assert_eq!(u8::from_sample(i32::MIN), u8::MIN);

    assert_eq!(u8::from_sample( 1.0f32), u8::MAX);
    assert_eq!(u8::from_sample(   0f32), u8::MID);
    assert_eq!(u8::from_sample(-1.0f32), u8::MIN);

    assert_eq!(u8::from_sample( 1.0f64), u8::MAX);
    assert_eq!(u8::from_sample(   0f64), u8::MID);
    assert_eq!(u8::from_sample(-1.0f64), u8::MIN);
}

#[test]
fn verify_u16_from_sample() {
    use std::{u8, i8, u16, i16, u32, i32};

    assert_eq!(u16::from_sample(u8::MAX), u16::MAX - 255);
    assert_eq!(u16::from_sample(u8::MID), u16::MID);
    assert_eq!(u16::from_sample(u8::MIN), u16::MIN);

    assert_eq!(u16::from_sample(u16::MAX), u16::MAX);
    assert_eq!(u16::from_sample(u16::MID), u16::MID);
    assert_eq!(u16::from_sample(u16::MIN), u16::MIN);

    assert_eq!(u16::from_sample(u24::MAX), u16::MAX);
    assert_eq!(u16::from_sample(u24::MID), u16::MID);
    assert_eq!(u16::from_sample(u24::MIN), u16::MIN);

    assert_eq!(u16::from_sample(u32::MAX), u16::MAX);
    assert_eq!(u16::from_sample(u32::MID), u16::MID);
    assert_eq!(u16::from_sample(u32::MIN), u16::MIN);

    assert_eq!(u16::from_sample(i8::MAX), u16::MAX - 255);
    assert_eq!(u16::from_sample(i8::MID), u16::MID);
    assert_eq!(u16::from_sample(i8::MIN), u16::MIN);

    assert_eq!(u16::from_sample(i16::MAX), u16::MAX);
    assert_eq!(u16::from_sample(i16::MID), u16::MID);
    assert_eq!(u16::from_sample(i16::MIN), u16::MIN);

    assert_eq!(u16::from_sample(i24::MAX), u16::MAX);
    assert_eq!(u16::from_sample(i24::MID), u16::MID);
    assert_eq!(u16::from_sample(i24::MIN), u16::MIN);

    assert_eq!(u16::from_sample(i32::MAX), u16::MAX);
    assert_eq!(u16::from_sample(i32::MID), u16::MID);
    assert_eq!(u16::from_sample(i32::MIN), u16::MIN);

    assert_eq!(u16::from_sample( 1.0f32), u16::MAX);
    assert_eq!(u16::from_sample(   0f32), u16::MID);
    assert_eq!(u16::from_sample(-1.0f32), u16::MIN);

    assert_eq!(u16::from_sample( 1.0f64), u16::MAX);
    assert_eq!(u16::from_sample(   0f64), u16::MID);
    assert_eq!(u16::from_sample(-1.0f64), u16::MIN);
}

#[test]
fn verify_u24_from_sample() {
    use std::{u8, i8, u16, i16, u32, i32};

    assert_eq!(u24::from_sample(u8::MAX), u24::MAX - u24::from(65_535u32));
    assert_eq!(u24::from_sample(u8::MID), u24::MID);
    assert_eq!(u24::from_sample(u8::MIN), u24::MIN);

    assert_eq!(u24::from_sample(u16::MAX), u24::MAX - u24::from(255u32));
    assert_eq!(u24::from_sample(u16::MID), u24::MID);
    assert_eq!(u24::from_sample(u16::MIN), u24::MIN);

    assert_eq!(u24::from_sample(u24::MAX), u24::MAX);
    assert_eq!(u24::from_sample(u24::MID), u24::MID);
    assert_eq!(u24::from_sample(u24::MIN), u24::MIN);

    assert_eq!(u24::from_sample(u32::MAX), u24::MAX);
    assert_eq!(u24::from_sample(u32::MID), u24::MID);
    assert_eq!(u24::from_sample(u32::MIN), u24::MIN);

    assert_eq!(u24::from_sample(i8::MAX), u24::MAX - u24::from(65_535u32));
    assert_eq!(u24::from_sample(i8::MID), u24::MID);
    assert_eq!(u24::from_sample(i8::MIN), u24::MIN);

    assert_eq!(u24::from_sample(i16::MAX), u24::MAX - u24::from(255u32));
    assert_eq!(u24::from_sample(i16::MID), u24::MID);
    assert_eq!(u24::from_sample(i16::MIN), u24::MIN);

    assert_eq!(u24::from_sample(i24::MAX), u24::MAX);
    assert_eq!(u24::from_sample(i24::MID), u24::MID);
    assert_eq!(u24::from_sample(i24::MIN), u24::MIN);

    assert_eq!(u24::from_sample(i32::MAX), u24::MAX);
    assert_eq!(u24::from_sample(i32::MID), u24::MID);
    assert_eq!(u24::from_sample(i32::MIN), u24::MIN);

    assert_eq!(u24::from_sample( 1.0f32), u24::MAX);
    assert_eq!(u24::from_sample(   0f32), u24::MID);
    assert_eq!(u24::from_sample(-1.0f32), u24::MIN);

    assert_eq!(u24::from_sample( 1.0f64), u24::MAX);
    assert_eq!(u24::from_sample(   0f64), u24::MID);
    assert_eq!(u24::from_sample(-1.0f64), u24::MIN);
}

#[test]
fn verify_u32_from_sample() {
    use std::{u8, i8, u16, i16, u32, i32};

    assert_eq!(u32::from_sample(u8::MAX), u32::MAX - 16_777_215);
    assert_eq!(u32::from_sample(u8::MID), u32::MID);
    assert_eq!(u32::from_sample(u8::MIN), u32::MIN);

    assert_eq!(u32::from_sample(u16::MAX), u32::MAX - 65_535);
    assert_eq!(u32::from_sample(u16::MID), u32::MID);
    assert_eq!(u32::from_sample(u16::MIN), u32::MIN);

    assert_eq!(u32::from_sample(u24::MAX), u32::MAX - 255);
    assert_eq!(u32::from_sample(u24::MID), u32::MID);
    assert_eq!(u32::from_sample(u24::MIN), u32::MIN);

    assert_eq!(u32::from_sample(u32::MAX), u32::MAX);
    assert_eq!(u32::from_sample(u32::MID), u32::MID);
    assert_eq!(u32::from_sample(u32::MIN), u32::MIN);

    assert_eq!(u32::from_sample(i8::MAX), u32::MAX - 16_777_215);
    assert_eq!(u32::from_sample(i8::MID), u32::MID);
    assert_eq!(u32::from_sample(i8::MIN), u32::MIN);

    assert_eq!(u32::from_sample(i16::MAX), u32::MAX - 65_535);
    assert_eq!(u32::from_sample(i16::MID), u32::MID);
    assert_eq!(u32::from_sample(i16::MIN), u32::MIN);

    assert_eq!(u32::from_sample(i24::MAX), u32::MAX - 255);
    assert_eq!(u32::from_sample(i24::MID), u32::MID);
    assert_eq!(u32::from_sample(i24::MIN), u32::MIN);

    assert_eq!(u32::from_sample(i32::MAX), u32::MAX);
    assert_eq!(u32::from_sample(i32::MID), u32::MID);
    assert_eq!(u32::from_sample(i32::MIN), u32::MIN);

    assert_eq!(u32::from_sample( 1.0f32), u32::MAX);
    assert_eq!(u32::from_sample(   0f32), u32::MID);
    assert_eq!(u32::from_sample(-1.0f32), u32::MIN);

    assert_eq!(u32::from_sample( 1.0f64), u32::MAX);
    assert_eq!(u32::from_sample(   0f64), u32::MID);
    assert_eq!(u32::from_sample(-1.0f64), u32::MIN);
}


#[test]
fn verify_i8_from_sample() {
    use std::{u8, i8, u16, i16, u32, i32};

    assert_eq!(i8::from_sample(u8::MAX), i8::MAX);
    assert_eq!(i8::from_sample(u8::MID), i8::MID);
    assert_eq!(i8::from_sample(u8::MIN), i8::MIN);

    assert_eq!(i8::from_sample(u16::MAX), i8::MAX);
    assert_eq!(i8::from_sample(u16::MID), i8::MID);
    assert_eq!(i8::from_sample(u16::MIN), i8::MIN);

    assert_eq!(i8::from_sample(u24::MAX), i8::MAX);
    assert_eq!(i8::from_sample(u24::MID), i8::MID);
    assert_eq!(i8::from_sample(u24::MIN), i8::MIN);

    assert_eq!(i8::from_sample(u32::MAX), i8::MAX);
    assert_eq!(i8::from_sample(u32::MID), i8::MID);
    assert_eq!(i8::from_sample(u32::MIN), i8::MIN);

    assert_eq!(i8::from_sample(i8::MAX), i8::MAX);
    assert_eq!(i8::from_sample(i8::MID), i8::MID);
    assert_eq!(i8::from_sample(i8::MIN), i8::MIN);

    assert_eq!(i8::from_sample(i16::MAX), i8::MAX);
    assert_eq!(i8::from_sample(i16::MID), i8::MID);
    assert_eq!(i8::from_sample(i16::MIN), i8::MIN);

    assert_eq!(i8::from_sample(i24::MAX), i8::MAX);
    assert_eq!(i8::from_sample(i24::MID), i8::MID);
    assert_eq!(i8::from_sample(i24::MIN), i8::MIN);

    assert_eq!(i8::from_sample(i32::MAX), i8::MAX);
    assert_eq!(i8::from_sample(i32::MID), i8::MID);
    assert_eq!(i8::from_sample(i32::MIN), i8::MIN);

    assert_eq!(i8::from_sample( 1.0f32), i8::MAX);
    assert_eq!(i8::from_sample(   0f32), i8::MID);
    assert_eq!(i8::from_sample(-1.0f32), i8::MIN);

    assert_eq!(i8::from_sample( 1.0f64), i8::MAX);
    assert_eq!(i8::from_sample(   0f64), i8::MID);
    assert_eq!(i8::from_sample(-1.0f64), i8::MIN);
}

#[test]
fn verify_i16_from_sample() {
    use std::{u8, i8, u16, i16, u32, i32};

    assert_eq!(i16::from_sample(u8::MAX), i16::MAX - 255);
    assert_eq!(i16::from_sample(u8::MID), i16::MID);
    assert_eq!(i16::from_sample(u8::MIN), i16::MIN);

    assert_eq!(i16::from_sample(u16::MAX), i16::MAX);
    assert_eq!(i16::from_sample(u16::MID), i16::MID);
    assert_eq!(i16::from_sample(u16::MIN), i16::MIN);

    assert_eq!(i16::from_sample(u24::MAX), i16::MAX);
    assert_eq!(i16::from_sample(u24::MID), i16::MID);
    assert_eq!(i16::from_sample(u24::MIN), i16::MIN);

    assert_eq!(i16::from_sample(u32::MAX), i16::MAX);
    assert_eq!(i16::from_sample(u32::MID), i16::MID);
    assert_eq!(i16::from_sample(u32::MIN), i16::MIN);

    assert_eq!(i16::from_sample(i8::MAX), i16::MAX - 255);
    assert_eq!(i16::from_sample(i8::MID), i16::MID);
    assert_eq!(i16::from_sample(i8::MIN), i16::MIN);

    assert_eq!(i16::from_sample(i16::MAX), i16::MAX);
    assert_eq!(i16::from_sample(i16::MID), i16::MID);
    assert_eq!(i16::from_sample(i16::MIN), i16::MIN);

    assert_eq!(i16::from_sample(i24::MAX), i16::MAX);
    assert_eq!(i16::from_sample(i24::MID), i16::MID);
    assert_eq!(i16::from_sample(i24::MIN), i16::MIN);

    assert_eq!(i16::from_sample(i32::MAX), i16::MAX);
    assert_eq!(i16::from_sample(i32::MID), i16::MID);
    assert_eq!(i16::from_sample(i32::MIN), i16::MIN);

    assert_eq!(i16::from_sample( 1.0f32), i16::MAX);
    assert_eq!(i16::from_sample(   0f32), i16::MID);
    assert_eq!(i16::from_sample(-1.0f32), i16::MIN);

    assert_eq!(i16::from_sample( 1.0f64), i16::MAX);
    assert_eq!(i16::from_sample(   0f64), i16::MID);
    assert_eq!(i16::from_sample(-1.0f64), i16::MIN);
}

#[test]
fn verify_i24_from_sample() {
    use std::{u8, i8, u16, i16, u32, i32};

    assert_eq!(i24::from_sample(u8::MAX), i24::MAX - i24::from(65_535));
    assert_eq!(i24::from_sample(u8::MID), i24::MID);
    assert_eq!(i24::from_sample(u8::MIN), i24::MIN);

    assert_eq!(i24::from_sample(u16::MAX), i24::MAX - i24::from(255));
    assert_eq!(i24::from_sample(u16::MID), i24::MID);
    assert_eq!(i24::from_sample(u16::MIN), i24::MIN);

    assert_eq!(i24::from_sample(u24::MAX), i24::MAX);
    assert_eq!(i24::from_sample(u24::MID), i24::MID);
    assert_eq!(i24::from_sample(u24::MIN), i24::MIN);

    assert_eq!(i24::from_sample(u32::MAX), i24::MAX);
    assert_eq!(i24::from_sample(u32::MID), i24::MID);
    assert_eq!(i24::from_sample(u32::MIN), i24::MIN);

    assert_eq!(i24::from_sample(i8::MAX), i24::MAX - i24::from(65_535));
    assert_eq!(i24::from_sample(i8::MID), i24::MID);
    assert_eq!(i24::from_sample(i8::MIN), i24::MIN);

    assert_eq!(i24::from_sample(i16::MAX), i24::MAX - i24::from(255));
    assert_eq!(i24::from_sample(i16::MID), i24::MID);
    assert_eq!(i24::from_sample(i16::MIN), i24::MIN);

    assert_eq!(i24::from_sample(i24::MAX), i24::MAX);
    assert_eq!(i24::from_sample(i24::MID), i24::MID);
    assert_eq!(i24::from_sample(i24::MIN), i24::MIN);

    assert_eq!(i24::from_sample(i32::MAX), i24::MAX);
    assert_eq!(i24::from_sample(i32::MID), i24::MID);
    assert_eq!(i24::from_sample(i32::MIN), i24::MIN);

    assert_eq!(i24::from_sample( 1.0f32), i24::MAX);
    assert_eq!(i24::from_sample(   0f32), i24::MID);
    assert_eq!(i24::from_sample(-1.0f32), i24::MIN);

    assert_eq!(i24::from_sample( 1.0f64), i24::MAX);
    assert_eq!(i24::from_sample(   0f64), i24::MID);
    assert_eq!(i24::from_sample(-1.0f64), i24::MIN);
}

#[test]
fn verify_i32_from_sample() {
    use std::{u8, i8, u16, i16, u32, i32};

    assert_eq!(i32::from_sample(u8::MAX), i32::MAX - 16_777_215);
    assert_eq!(i32::from_sample(u8::MID), i32::MID);
    assert_eq!(i32::from_sample(u8::MIN), i32::MIN);

    assert_eq!(i32::from_sample(u16::MAX), i32::MAX - 65_535);
    assert_eq!(i32::from_sample(u16::MID), i32::MID);
    assert_eq!(i32::from_sample(u16::MIN), i32::MIN);

    assert_eq!(i32::from_sample(u24::MAX), i32::MAX - 255);
    assert_eq!(i32::from_sample(u24::MID), i32::MID);
    assert_eq!(i32::from_sample(u24::MIN), i32::MIN);

    assert_eq!(i32::from_sample(u32::MAX), i32::MAX);
    assert_eq!(i32::from_sample(u32::MID), i32::MID);
    assert_eq!(i32::from_sample(u32::MIN), i32::MIN);

    assert_eq!(i32::from_sample(i8::MAX), i32::MAX - 16_777_215);
    assert_eq!(i32::from_sample(i8::MID), i32::MID);
    assert_eq!(i32::from_sample(i8::MIN), i32::MIN);

    assert_eq!(i32::from_sample(i16::MAX), i32::MAX - 65_535);
    assert_eq!(i32::from_sample(i16::MID), i32::MID);
    assert_eq!(i32::from_sample(i16::MIN), i32::MIN);

    assert_eq!(i32::from_sample(i24::MAX), i32::MAX - 255);
    assert_eq!(i32::from_sample(i24::MID), i32::MID);
    assert_eq!(i32::from_sample(i24::MIN), i32::MIN);

    assert_eq!(i32::from_sample(i32::MAX), i32::MAX);
    assert_eq!(i32::from_sample(i32::MID), i32::MID);
    assert_eq!(i32::from_sample(i32::MIN), i32::MIN);

    assert_eq!(i32::from_sample( 1.0f32), i32::MAX);
    assert_eq!(i32::from_sample(   0f32), i32::MID);
    assert_eq!(i32::from_sample(-1.0f32), i32::MIN);

    assert_eq!(i32::from_sample( 1.0f64), i32::MAX);
    assert_eq!(i32::from_sample(   0f64), i32::MID);
    assert_eq!(i32::from_sample(-1.0f64), i32::MIN);
}
