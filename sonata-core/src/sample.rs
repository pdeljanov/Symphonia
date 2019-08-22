// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

/// SampleFormat describes the data encoding for an audio sample.
#[derive(Copy, Clone, Debug)]
pub enum SampleFormat {
    /// Unsigned 8bit integer.
    U8,
    /// Unsigned 16bit integer.
    U16,
    /// Unsigned 24bit integer.
    U24,
    /// Unsigned 32bit integer.
    U32,
    /// Signed 8bit integer.
    S8,
    /// Signed 16bit integer.
    S16,
    /// Signed 24bit integer.
    S24,
    /// Signed 32bit integer.
    S32,
    /// Floating point, 32bit.
    F32,
    /// Floating point, 64bit.
    F64
}

/// `Sample` provides a common interface for manipulating sample's regardless of the
/// underlying data type. Additionally, `Sample` provides information regarding the
/// format of underlying data types representing the sample when in memory, but also
/// when exported.
pub trait Sample: Copy + Clone + Sized + Default {

    /// The `StreamType` is the primitive data type, or fixed-size byte array, that
    /// represents the sample when exported.
    type StreamType : Copy;

    /// A unique enum value representing the sample format. This constant may be used
    /// to dynamically choose how to process the sample at runtime.
    const FORMAT: SampleFormat;

    /// The mid-point value between the maximum and minimum sample value. If a sample
    /// is set to this value, it is silent.
    const MID : Self;
}

/// An unsigned 24-bit integer sample.
#[allow(non_camel_case_types)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct u24 (pub u32);

/// An signed 24-bit integer sample.
#[allow(non_camel_case_types)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct i24 (pub i32);


impl Sample for u8 {
    type StreamType = u8;

    const FORMAT: SampleFormat = SampleFormat::U8;

    const MID: u8 = 128u8;
}

impl Sample for i8 {
    type StreamType = i8;

    const FORMAT: SampleFormat = SampleFormat::S8;

    const MID: i8 = 0i8;
}

impl Sample for u16 {
    type StreamType = u16;

    const FORMAT: SampleFormat = SampleFormat::U16;

    const MID: u16 = 32_768u16;
}

impl Sample for i16 {
    type StreamType = i16;

    const FORMAT: SampleFormat = SampleFormat::S16;

    const MID: i16 = 0i16;
}

impl Sample for u24 {
    type StreamType = [u8; 3];

    const FORMAT: SampleFormat = SampleFormat::U24;

    const MID: u24 = u24(8_388_608u32);
}

impl Sample for i24 {
    type StreamType = [u8; 3];

    const FORMAT: SampleFormat = SampleFormat::S24;

    const MID: i24 = i24(0i32);
}

impl Sample for u32 {
    type StreamType = u32;

    const FORMAT: SampleFormat = SampleFormat::U32;

    const MID: u32 = 2_147_483_648u32;
}

impl Sample for i32 {
    type StreamType = i32;

    const FORMAT: SampleFormat = SampleFormat::S32;

    const MID: i32 = 0i32;
}

impl Sample for f32 {
    type StreamType = f32;

    const FORMAT: SampleFormat = SampleFormat::F32;

    const MID: f32 = 0f32;
}

impl Sample for f64 {
    type StreamType = f64;

    const FORMAT: SampleFormat = SampleFormat::F64;

    const MID: f64 = 0f64;
}

// Implementation for i24

impl i24 {
    pub const MAX: i24 = i24(8_388_607i32);
    pub const MIN: i24 = i24(-8_388_608i32);

    #[inline]
    fn saturate_overflow(self) -> Self {
        self
    }

    #[inline]
    pub fn into_i32(self) -> i32 {
        self.0
    }

    #[inline]
    pub fn to_ne_bytes(&self) -> [u8; 3] {
        // Little endian platform
        #[cfg(target_endian = "little")]
        {
            [
                ((self.0 & 0x00_00ff) >>  0) as u8,
                ((self.0 & 0x00_ff00) >>  8) as u8,
                ((self.0 & 0xff_0000) >> 16) as u8,
            ]
        }
        // Big endian platform
        #[cfg(not(target_endian = "little"))]
        {
            [
                ((self.0 & 0xff_0000) >> 16) as u8,
                ((self.0 & 0x00_ff00) >>  8) as u8,
                ((self.0 & 0x00_00ff) >>  0) as u8,
            ]
        }
    }
}

impl From<i32> for i24 {
    fn from(val: i32) -> Self { i24(val).saturate_overflow() }
}

impl From<i16> for i24 {
    fn from(val: i16) -> Self { i24(i32::from(val)) }
}

impl From<i8> for i24 {
    fn from(val: i8) -> Self { i24(i32::from(val)) }
}

impl ::core::ops::Add<i24> for i24 {
    type Output = i24;

    #[inline]
    fn add(self, other: Self) -> Self { i24(self.0 + other.0) }
}

impl ::core::ops::Sub<i24> for i24 {
    type Output = i24;

    #[inline]
    fn sub(self, other: Self) -> Self { i24(self.0 - other.0) }
}

impl ::core::ops::Mul<i24> for i24 {
    type Output = i24;

    #[inline]
    fn mul(self, other: Self) -> Self { i24::from(self.0 * other.0) }
}

impl ::core::ops::Div<i24> for i24 {
    type Output = i24;

    #[inline]
    fn div(self, other: Self) -> Self { i24(self.0 / other.0) }
}

impl ::core::ops::Not for i24 {
    type Output = i24;

    #[inline]
    fn not(self) -> Self { i24(!self.0) }
}

impl ::core::ops::Rem<i24> for i24 {
    type Output = i24;

    #[inline]
    fn rem(self, other: Self) -> Self { i24(self.0 % other.0) }
}

impl ::core::ops::Shl<i24> for i24 {
    type Output = i24;

    #[inline]
    fn shl(self, other: Self) -> Self { i24(self.0 << other.0) }
}

impl ::core::ops::Shr<i24> for i24 {
    type Output = i24;

    #[inline]
    fn shr(self, other: Self) -> Self { i24(self.0 >> other.0) }
}

impl ::core::ops::BitAnd<i24> for i24 {
    type Output = i24;

    #[inline]
    fn bitand(self, other: Self) -> Self { i24(self.0 & other.0) }
}

impl ::core::ops::BitOr<i24> for i24 {
    type Output = i24;

    #[inline]
    fn bitor(self, other: Self) -> Self { i24(self.0 | other.0) }
}

impl ::core::ops::BitXor<i24> for i24 {
    type Output = i24;

    #[inline]
    fn bitxor(self, other: Self) -> Self { i24(self.0 ^ other.0) }
}

// Implementation for u24

impl u24 {

    pub const MAX: u24 = u24(16_777_215u32);
    pub const MIN: u24 = u24(0u32);

    #[inline]
    fn saturate_overflow(self) -> Self {
        self
    }

    #[inline]
    pub fn into_u32(self) -> u32 {
        self.0
    }

    #[inline]
    pub fn to_ne_bytes(&self) -> [u8; 3] {
        // Little endian platform
        #[cfg(target_endian = "little")]
        {
            [
                ((self.0 & 0x00_00ff) >>  0) as u8,
                ((self.0 & 0x00_ff00) >>  8) as u8,
                ((self.0 & 0xff_0000) >> 16) as u8,
            ]
        }
        // Big endian platform
        #[cfg(not(target_endian = "little"))]
        {
            [
                ((self.0 & 0xff_0000) >> 16) as u8,
                ((self.0 & 0x00_ff00) >>  8) as u8,
                ((self.0 & 0x00_00ff) >>  0) as u8,
            ]
        }
    }

}

impl From<u32> for u24 {
    fn from(val: u32) -> Self { u24(val).saturate_overflow() }
}

impl From<u16> for u24 {
    fn from(val: u16) -> Self { u24(u32::from(val)) }
}

impl From<u8> for u24 {
    fn from(val: u8) -> Self { u24(u32::from(val)) }
}

impl ::core::ops::Add<u24> for u24 {
    type Output = u24;

    #[inline]
    fn add(self, other: Self) -> Self { u24(self.0 + other.0) }
}

impl ::core::ops::Sub<u24> for u24 {
    type Output = u24;

    #[inline]
    fn sub(self, other: Self) -> Self { u24(self.0 - other.0) }
}

impl ::core::ops::Mul<u24> for u24 {
    type Output = u24;

    #[inline]
    fn mul(self, other: Self) -> Self { u24::from(self.0 * other.0) }
}

impl ::core::ops::Div<u24> for u24 {
    type Output = u24;

    #[inline]
    fn div(self, other: Self) -> Self { u24(self.0 / other.0) }
}

impl ::core::ops::Not for u24 {
    type Output = u24;

    #[inline]
    fn not(self) -> Self { u24(!self.0) }
}

impl ::core::ops::Rem<u24> for u24 {
    type Output = u24;

    #[inline]
    fn rem(self, other: Self) -> Self { u24(self.0 % other.0) }
}

impl ::core::ops::Shl<u24> for u24 {
    type Output = u24;

    #[inline]
    fn shl(self, other: Self) -> Self { u24(self.0 << other.0) }
}

impl ::core::ops::Shr<u24> for u24 {
    type Output = u24;

    #[inline]
    fn shr(self, other: Self) -> Self { u24(self.0 >> other.0) }
}

impl ::core::ops::BitAnd<u24> for u24 {
    type Output = u24;

    #[inline]
    fn bitand(self, other: Self) -> Self { u24(self.0 & other.0) }
}

impl ::core::ops::BitOr<u24> for u24 {
    type Output = u24;

    #[inline]
    fn bitor(self, other: Self) -> Self { u24(self.0 | other.0) }
}

impl ::core::ops::BitXor<u24> for u24 {
    type Output = u24;

    #[inline]
    fn bitxor(self, other: Self) -> Self { u24(self.0 ^ other.0) }
}
