// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The `conv` module provides methods to convert samples between different sample types (formats).
use crate::sample::{Sample, u24, i24};
use crate::util::clamp::*;

pub mod dither {
    //! The `dither` module provides methods to apply a dither to a sample.
    //!
    //! Dithering is the process of adding noise to the least significant digits of a sample before
    //! down-converting (quantizing) it to a smaller sample type. The purpose of dithering is to
    //! decorrelate the quantization error of the down-conversion from the source signal.
    //!
    //! Dithering is only applied on lossy conversions. Therefore the `dither` module will only
    //! apply a dither to the following down-conversions:
    //!
    //! * { `i32`, `u32` } to { `i24`, `u24`, `i16`, `u16`, `i8`, `u8` }
    //! * { `i24`, `u24` } to { `i16`, `u16`, `i8`, `u8` }
    //! * { `i16`, `u16` } to { `i8`, `u8` }
    //!
    //! Multiple dithering algorithms are provided, each drawing noise from a different probability
    //! distribution. In addition to different distributions, a dithering algorithm may also shape
    //! the noise such that the bulk of the noise is placed in an inaudible frequency range.
    use core::marker::PhantomData;
    use super::FromSample;
    use crate::sample::{u24, i24};
    use crate::sample::Sample;

    mod prng {
        #[inline]
        fn split_mix_64(x: &mut u64) -> u64 {
            *x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = *x;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        /// `Xoshiro128pp` implements the xoshiro128++ pseudo-random number generator.
        ///
        /// This PRNG is the basis for all built-in dithering algorithms. It is one of, if not the
        /// most, performant PRNGs that generate statistically valid random numbers. Note that it is
        /// not cryptographically secure, but for dithering audio it is more than sufficient.
        ///
        /// `Xoshiro128pp` should be initialized with a reasonably random 64-bit seed, however the
        /// seed will be further randomized via the SplitMix64 algorithm.
        pub struct Xoshiro128pp {
            s: [u32; 4],
        }

        impl Xoshiro128pp {
            pub fn new(mut seed: u64) -> Self {
                let a = split_mix_64(&mut seed);
                let b = split_mix_64(&mut seed);

                Xoshiro128pp {
                    s: [
                        (a & 0xffff_ffff) as u32, (a >> 32) as u32,
                        (b & 0xffff_ffff) as u32, (b >> 32) as u32,
                    ]
                }
            }

            #[inline(always)]
            fn rotl(x: u32, k: u32) -> u32 {
                (x << k) | (x >> (32 - k))
            }

            #[inline]
            pub fn next(&mut self) -> u32 {
                let x = self.s[0].wrapping_add(self.s[3]);

                let result = Xoshiro128pp::rotl(x, 7).wrapping_add(self.s[0]);

                let t = self.s[1] << 9;

                self.s[2] ^= self.s[0];
                self.s[3] ^= self.s[1];
                self.s[1] ^= self.s[2];
                self.s[0] ^= self.s[3];

                self.s[2] ^= t;

                self.s[3] = Xoshiro128pp::rotl(self.s[3], 11);

                result
            }
        }
    }

    /// `RandomNoise` represents a sample of noise of a specified length in bits.
    ///
    /// TODO: `RandomNoise` should be parameterized by the number of bits once const generics land.
    #[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
    pub struct RandomNoise (pub i32);

    impl RandomNoise {
        /// Instantiate a noise sample from a random 32-bit source.
        pub fn from(random: i32, n_bits: u32) -> Self {
            RandomNoise(random >> (32 - n_bits))
        }
    }

    /// `AddNoise` is a trait for converting random noise into a `Sample`.
    pub trait AddNoise<S: Sample> {
        fn add_noise(self, sample: S) -> S;
    }

    macro_rules! add_noise_impl {
        ($sample_type:ty, $self:ident, $sample:ident, $conv:expr) => (
            impl AddNoise<$sample_type> for RandomNoise {
                #[inline]
                fn add_noise($self, $sample: $sample_type) -> $sample_type { $conv }
            }
        )
    }

    add_noise_impl!(i8 , self, s, {
        i8::from_sample(f32::from_sample(s) + f32::from_sample(self.0))
    });
    add_noise_impl!(u8 , self, s, {
        u8::from_sample(f32::from_sample(s) + f32::from_sample(self.0))
    });
    add_noise_impl!(i16, self, s, {
        i16::from_sample(f32::from_sample(s) + f32::from_sample(self.0))
    });
    add_noise_impl!(u16, self, s, {
        u16::from_sample(f32::from_sample(s) + f32::from_sample(self.0))
    });
    add_noise_impl!(i24, self, s, {
        i24::from_sample(f32::from_sample(s) + f32::from_sample(self.0))
    });
    add_noise_impl!(u24, self, s, {
        u24::from_sample(f32::from_sample(s) + f32::from_sample(self.0))
    });
    add_noise_impl!(i32, self, s, {
        i32::from_sample(f64::from_sample(s) + f64::from_sample(self.0))
    });
    add_noise_impl!(u32, self, s, {
        u32::from_sample(f64::from_sample(s) + f64::from_sample(self.0))
    });
    add_noise_impl!(f32, self, s, s + f32::from_sample(self.0));
    add_noise_impl!(f64, self, s, s + f64::from_sample(self.0));

    /// `Dither` is a trait for implementing dithering algorithms.
    pub trait Dither<F: Sample, T: Sample> {
        /// Dithers a `Sample` of source sample format `F` for an eventual conversion to the
        /// destination sample format `T`.
        fn dither(&mut self, sample: F) -> F;
    }

    /// The `Identity` dithering algorithm performs no dithering and returns the original sample.
    pub struct Identity<F: Sample, T: Sample> {
        from_type: PhantomData<F>,
        to_type: PhantomData<T>,
    }

    impl<F: Sample, T: Sample> Identity<F, T> {
        pub fn new() -> Self {
            Identity {
                from_type: PhantomData,
                to_type: PhantomData,
            }
        }
    }

    impl<F: Sample, T: Sample> Dither<F, T> for Identity<F, T> {
        fn dither(&mut self, sample: F) -> F { sample }
    }

    impl<F: Sample, T: Sample> Default for Identity<F, T> {
        fn default() -> Self {
            Self::new()
        }
    }

    /// `Rectangular` implements a dither using uniformly distributed (white) noise without shaping.
    pub struct Rectangular<F: Sample, T: Sample> {
        prng: prng::Xoshiro128pp,
        from_type: PhantomData<F>,
        to_type: PhantomData<T>,
    }

    impl<F: Sample, T: Sample> Rectangular<F, T> {
        pub fn new() -> Self {
            Rectangular {
                prng: prng::Xoshiro128pp::new(0xb2c1_01f4_425b_987e),
                from_type: PhantomData,
                to_type: PhantomData,
            }
        }
    }

    impl<F: Sample, T: Sample> Dither<F, T> for Rectangular<F, T>
    where
        RandomNoise : AddNoise<F>
    {
        fn dither(&mut self, sample: F) -> F {
            // A dither should be applied if and only if the effective number of bits of the source
            // sample format is greater than that of the destination sample format.
            debug_assert!(F::EFF_BITS > T::EFF_BITS);

            // The number of low-order bits being truncated by the conversion will be dithered.
            let dither_bits = 32 - T::EFF_BITS;

            // Add the noise to the sample.
            let noise = RandomNoise::from(self.prng.next() as i32, dither_bits);
            noise.add_noise(sample)
        }
    }

    impl<F: Sample, T: Sample> Default for Rectangular<F, T> {
        fn default() -> Self {
            Self::new()
        }
    }

    /// `Triangular` implements a dither using a triangular distribution of noise without shaping.
    pub struct Triangular<F: Sample, T: Sample> {
        prng: prng::Xoshiro128pp,
        from_type: PhantomData<F>,
        to_type: PhantomData<T>,
    }

    impl<F: Sample, T: Sample> Triangular<F, T> {
        pub fn new() -> Self {
            Triangular {
                prng: prng::Xoshiro128pp::new(0xb2c1_01f4_425b_987e),
                from_type: PhantomData,
                to_type: PhantomData,
            }
        }
    }

    impl<F: Sample, T: Sample> Dither<F, T> for Triangular<F, T>
    where
        RandomNoise : AddNoise<F>
    {
        fn dither(&mut self, sample: F) -> F {
            debug_assert!(F::EFF_BITS > T::EFF_BITS);

            let dither_bits = 32 - T::EFF_BITS;

            // Generate a triangular distribution from the uniform distribution.
            let tpdf = (self.prng.next() as i32 >> 1) + (self.prng.next() as i32 >> 1);

            // Add the noise to the sample.
            let noise = RandomNoise::from(tpdf, dither_bits);
            noise.add_noise(sample)
        }
    }

    impl<F: Sample, T: Sample> Default for Triangular<F, T> {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Enumeration of dither algorithms.
    pub enum DitherType {
        /// No dithering.
        Identity,
        /// Apply rectangular dithering. See `Rectangular` for more details.
        Rectangular,
        /// Apply triangular dithering. See `Triangular` for more details.
        Triangular,
    }

    /// `MaybeDither` conditionally applies a dither to a sample depending on the source and
    /// destination sample types.
    pub trait MaybeDither<T: Sample> : Sample {
        const DITHERABLE: bool;

        fn maybe_dither<D: Dither<Self, T>>(self, dither: &mut D) -> Self;
    }

    /// Never apply a dither for this conversion.
    macro_rules! dither_never {
        ($to:ty, $from:ty) => (
            impl MaybeDither<$to> for $from {
                const DITHERABLE: bool = false;
                #[inline(always)]
                fn maybe_dither<D: Dither<$from, $to>>(self, _: &mut D) -> Self {
                    self
                }
            }
        )
    }

    /// Maybe apply a dither for this conversion.
    macro_rules! dither_maybe {
        ($to:ty, $from:ty) => (
            impl MaybeDither<$to> for $from {
                const DITHERABLE: bool = true;
                #[inline(always)]
                fn maybe_dither<D: Dither<$from, $to>>(self, dither: &mut D) -> Self {
                    dither.dither(self)
                }
            }
        )
    }

    // Dither table for conversions to u8
    dither_never!(u8, u8 );
    dither_maybe!(u8, u16);
    dither_maybe!(u8, u24);
    dither_maybe!(u8, u32);
    dither_never!(u8, i8 );
    dither_maybe!(u8, i16);
    dither_maybe!(u8, i24);
    dither_maybe!(u8, i32);
    dither_never!(u8, f32);
    dither_never!(u8, f64);

    // Dither table for conversions to u16
    dither_never!(u16, u8 );
    dither_never!(u16, u16);
    dither_maybe!(u16, u24);
    dither_maybe!(u16, u32);
    dither_never!(u16, i8 );
    dither_never!(u16, i16);
    dither_maybe!(u16, i24);
    dither_maybe!(u16, i32);
    dither_never!(u16, f32);
    dither_never!(u16, f64);

    // Dither table for conversions to u24
    dither_never!(u24, u8 );
    dither_never!(u24, u16);
    dither_never!(u24, u24);
    dither_maybe!(u24, u32);
    dither_never!(u24, i8 );
    dither_never!(u24, i16);
    dither_never!(u24, i24);
    dither_maybe!(u24, i32);
    dither_never!(u24, f32);
    dither_never!(u24, f64);

    // Dither table for conversions to u32
    dither_never!(u32, u8 );
    dither_never!(u32, u16);
    dither_never!(u32, u24);
    dither_never!(u32, u32);
    dither_never!(u32, i8 );
    dither_never!(u32, i16);
    dither_never!(u32, i24);
    dither_never!(u32, i32);
    dither_never!(u32, f32);
    dither_never!(u32, f64);

    // Dither table for conversions to i8
    dither_never!(i8, u8 );
    dither_maybe!(i8, u16);
    dither_maybe!(i8, u24);
    dither_maybe!(i8, u32);
    dither_never!(i8, i8 );
    dither_maybe!(i8, i16);
    dither_maybe!(i8, i24);
    dither_maybe!(i8, i32);
    dither_never!(i8, f32);
    dither_never!(i8, f64);

    // Dither table for conversions to i16
    dither_never!(i16, u8 );
    dither_never!(i16, u16);
    dither_maybe!(i16, u24);
    dither_maybe!(i16, u32);
    dither_never!(i16, i8 );
    dither_never!(i16, i16);
    dither_maybe!(i16, i24);
    dither_maybe!(i16, i32);
    dither_never!(i16, f32);
    dither_never!(i16, f64);

    // Dither table for conversions to i24
    dither_never!(i24, u8 );
    dither_never!(i24, u16);
    dither_never!(i24, u24);
    dither_maybe!(i24, u32);
    dither_never!(i24, i8 );
    dither_never!(i24, i16);
    dither_never!(i24, i24);
    dither_maybe!(i24, i32);
    dither_never!(i24, f32);
    dither_never!(i24, f64);

    // Dither table for conversions to i32
    dither_never!(i32, u8 );
    dither_never!(i32, u16);
    dither_never!(i32, u24);
    dither_never!(i32, u32);
    dither_never!(i32, i8 );
    dither_never!(i32, i16);
    dither_never!(i32, i24);
    dither_never!(i32, i32);
    dither_never!(i32, f32);
    dither_never!(i32, f64);

    // Dither table for conversions to f32
    dither_never!(f32, u8 );
    dither_never!(f32, u16);
    dither_never!(f32, u24);
    dither_never!(f32, u32);
    dither_never!(f32, i8 );
    dither_never!(f32, i16);
    dither_never!(f32, i24);
    dither_never!(f32, i32);
    dither_never!(f32, f32);
    dither_never!(f32, f64);

    // Dither table for conversions to f64
    dither_never!(f64, u8 );
    dither_never!(f64, u16);
    dither_never!(f64, u24);
    dither_never!(f64, u32);
    dither_never!(f64, i8 );
    dither_never!(f64, i16);
    dither_never!(f64, i24);
    dither_never!(f64, i32);
    dither_never!(f64, f32);
    dither_never!(f64, f64);
}

/// `FromSample` implements a conversion from `Sample` type `F` to `Self`.
///
/// This may be a lossy conversion if converting from a sample type of higher precision to one of
/// lower precision. No dithering is applied.
pub trait FromSample<F> {
    fn from_sample(val: F) -> Self;
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
converter!(u8, u24, s, ((s.into_u32() & 0xff_ffff) >> 16) as u8);
converter!(u8, u32, s, (s >> 24) as u8);
converter!(u8, i8 , s, (s as u8).wrapping_add(0x80));
converter!(u8, i16, s, ((s as u16).wrapping_add(0x8000) >> 8) as u8);
converter!(u8, i24, s, (((s.into_i32() as u32).wrapping_add(0x80_0000) & 0xff_ffff) >> 16) as u8);
converter!(u8, i32, s, ((s as u32).wrapping_add(0x8000_0000) >> 24) as u8);
converter!(u8, f32, s, {
    let s16 = (clamp_f32(s) * 128.0).round() as i16;
    clamp_u8((s16 + 0x80) as u16)
});
converter!(u8, f64, s, {
    let s16 = (clamp_f64(s) * 128.0).round() as i16;
    clamp_u8((s16 + 0x80) as u16)
});

// Conversions to u16
converter!(u16, u8 , s, u16::from(s) << 8);
converter!(u16, u16, s, s);
converter!(u16, u24, s, ((s.into_u32() & 0xff_ffff) >> 8) as u16);
converter!(u16, u32, s, (s >> 16) as u16);
converter!(u16, i8 , s, u16::from((s as u8).wrapping_add(0x80)) << 8);
converter!(u16, i16, s, (s as u16).wrapping_add(0x8000));
converter!(u16, i24, s, (((s.into_i32() as u32).wrapping_add(0x80_0000) & 0xff_ffff) >> 8) as u16);
converter!(u16, i32, s, ((s as u32).wrapping_add(0x8000_0000) >> 16) as u16);
converter!(u16, f32, s, {
    let s32 = (clamp_f32(s) * 32_768.0).round() as i32;
    clamp_u16((s32 + 0x8000) as u32)
});
converter!(u16, f64, s, {
    let s32 = (clamp_f64(s) * 32_768.0).round() as i32;
    clamp_u16((s32 + 0x8000) as u32)
});

// Conversions to u24
converter!(u24, u8 , s, u24::from(u32::from(s) << 16));
converter!(u24, u16, s, u24::from(u32::from(s) << 8));
converter!(u24, u24, s, u24::from(s.into_u32() & 0xff_ffff));
converter!(u24, u32, s, u24::from(s >> 8));
converter!(u24, i8 , s, u24::from(u32::from((s as u8).wrapping_add(0x80)) << 16));
converter!(u24, i16, s, u24::from(u32::from((s as u16).wrapping_add(0x8000)) << 8));
converter!(u24, i24, s, u24::from((s.into_i32() as u32).wrapping_add(0x80_0000) & 0xff_ffff));
converter!(u24, i32, s, u24::from((s as u32).wrapping_add(0x8000_0000) >> 8));
converter!(u24, f32, s, {
    let s32 = (clamp_f32(s) * 8_388_608.0).round() as i32;
    u24::from(clamp_u24((s32 + 0x80_0000) as u32))
});
converter!(u24, f64, s, {
    let s32 = (clamp_f64(s) * 8_388_608.0).round() as i32;
    u24::from(clamp_u24((s32 + 0x80_0000) as u32))
});

// Conversions to u32
converter!(u32, u8 , s, u32::from(s) << 24);
converter!(u32, u16, s, u32::from(s) << 16);
converter!(u32, u24, s, (s.into_u32() & 0xff_ffff) << 8);
converter!(u32, u32, s, s);
converter!(u32, i8 , s, u32::from((s as u8).wrapping_add(0x80)) << 24);
converter!(u32, i16, s, u32::from((s as u16).wrapping_add(0x8000)) << 16);
converter!(u32, i24, s, ((s.into_i32() as u32).wrapping_add(0x80_0000) & 0xff_ffff) << 8);
converter!(u32, i32, s, (s as u32).wrapping_add(0x8000_0000));
converter!(u32, f32, s, {
    let s64 = (clamp_f32(s) * 2_147_483_648.0).round() as i64;
    clamp_u32((s64 + 0x8000_0000) as u64)
});
converter!(u32, f64, s, {
    let s64 = (clamp_f64(s) * 2_147_483_648.0).round() as i64;
    clamp_u32((s64 + 0x8000_0000) as u64)
});

// Conversions to i8
converter!(i8, u8 , s, s.wrapping_add(0x80) as i8);
converter!(i8, u16, s, (s.wrapping_add(0x8000) >> 8) as i8);
converter!(i8, u24, s, (s.into_u32().wrapping_add(0x80_0000) >> 16) as i8);
converter!(i8, u32, s, (s.wrapping_add(0x8000_0000) >> 24) as i8);
converter!(i8, i8 , s, s);
converter!(i8, i16, s, (s >> 8) as i8);
converter!(i8, i24, s, ((s.into_i32() & 0xff_ffff) >> 16) as i8);
converter!(i8, i32, s, (s >> 24) as i8);
converter!(i8, f32, s, clamp_i8((clamp_f32(s) * 128.0).round() as i16));
converter!(i8, f64, s, clamp_i8((clamp_f64(s) * 128.0).round() as i16));

// Conversions to i16
converter!(i16, u8 , s, i16::from(s.wrapping_add(0x80)) << 8);
converter!(i16, u16, s, s.wrapping_add(0x8000) as i16);
converter!(i16, u24, s, (s.into_u32().wrapping_add(0x80_0000) >> 8) as i16);
converter!(i16, u32, s, (s.wrapping_add(0x8000_0000) >> 16) as i16);
converter!(i16, i8 , s, i16::from(s) << 8);
converter!(i16, i16, s, s);
converter!(i16, i24, s, ((s.into_i32() & 0xff_ffff) >> 8) as i16);
converter!(i16, i32, s, (s >> 16) as i16);
converter!(i16, f32, s, clamp_i16((clamp_f32(s) * 32_768.0).round() as i32));
converter!(i16, f64, s, clamp_i16((clamp_f64(s) * 32_768.0).round() as i32));

// Conversions to i24
converter!(i24, u8 , s, i24::from((i32::from(s) - 0x80) << 16));
converter!(i24, u16, s, i24::from((i32::from(s) - 0x8000) << 8));
converter!(i24, u24, s, i24::from((s.into_u32() & 0xff_ffff) as i32 - 0x80_0000));
converter!(i24, u32, s, i24::from((s.wrapping_add(0x8000_0000) as i32) >> 8));
converter!(i24, i8 , s, i24::from(i32::from(s) << 16));
converter!(i24, i16, s, i24::from(i32::from(s) << 8));
converter!(i24, i24, s, s);
converter!(i24, i32, s, i24::from(s >> 8));
converter!(i24, f32, s, i24::from(clamp_i24((clamp_f32(s) * 8_388_608.0).round() as i32)));
converter!(i24, f64, s, i24::from(clamp_i24((clamp_f64(s) * 8_388_608.0).round() as i32)));

// Conversions to i32
converter!(i32, u8 , s, (i32::from(s) - 0x80) << 24);
converter!(i32, u16, s, (i32::from(s) - 0x8000) << 16);
converter!(i32, u24, s, ((s.into_u32() & 0xff_ffff) as i32 - 0x80_0000) << 8);
converter!(i32, u32, s, s.wrapping_add(0x8000_0000) as i32);
converter!(i32, i8 , s, i32::from(s) << 24);
converter!(i32, i16, s, i32::from(s) << 16);
converter!(i32, i24, s, (s.into_i32() & 0xff_ffff) << 8);
converter!(i32, i32, s, s);
converter!(i32, f32, s, clamp_i32((clamp_f32(s) * 2_147_483_648.0).round() as i64));
converter!(i32, f64, s, clamp_i32((clamp_f64(s) * 2_147_483_648.0).round() as i64));

// Conversions to f32
converter!(f32, u8 , s, f32::from(i8::from_sample(s)) / 128.0);
converter!(f32, u16, s, f32::from(i16::from_sample(s)) / 32_768.0);
converter!(f32, u24, s, (i24::from_sample(s).into_i32() as f32) / 8_388_608.0);
converter!(f32, u32, s, (i32::from_sample(s) as f32) / 2_147_483_648.0);
converter!(f32, i8 , s, f32::from(s) / 128.0);
converter!(f32, i16, s, f32::from(s) / 32_768.0);
converter!(f32, i24, s, (s.into_i32() as f32) / 8_388_608.0);
converter!(f32, i32, s, (s as f32) / 2_147_483_648.0);
converter!(f32, f32, s, s);
converter!(f32, f64, s, s as f32);

// Conversions to f64
converter!(f64, u8 , s, f64::from(i8::from_sample(s)) / 128.0);
converter!(f64, u16, s, f64::from(i16::from_sample(s)) / 32_768.0);
converter!(f64, u24, s, f64::from(i24::from_sample(s).into_i32()) / 8_388_608.0);
converter!(f64, u32, s, f64::from(i32::from_sample(s)) / 2_147_483_648.0);
converter!(f64, i8 , s, f64::from(s) / 128.0);
converter!(f64, i16, s, f64::from(s) / 32_768.0);
converter!(f64, i24, s, f64::from(s.into_i32()) / 8_388_608.0);
converter!(f64, i32, s, f64::from(s) / 2_147_483_648.0);
converter!(f64, f32, s, f64::from(s));
converter!(f64, f64, s, s);

/// `IntoSample` implements a conversion from `Self` to `Sample` type `T`.
///
/// This may be a lossy conversion if converting from a sample type of higher precision to one of
/// lower precision. No dithering is applied.
pub trait IntoSample<T> {
    fn into_sample(self) -> T;
}

impl<F, T: FromSample<F>> IntoSample<T> for F {
    #[inline]
    fn into_sample(self) -> T {
        T::from_sample(self)
    }
}

/// `ReversibleSample` is a trait that when implemented for `Self`, that `Sample` type implements
/// reversible conversions between `Self` and the parameterized `Sample` type `S`.
pub trait ReversibleSample<S> : Sample + FromSample<S> + IntoSample<S> {}
impl<S, T> ReversibleSample<S> for T where T: Sample + FromSample<S> + IntoSample<S> {}

pub trait ConvertibleSample :
    Sample +
        FromSample<i8> +
        FromSample<u8> +
        FromSample<i16> +
        FromSample<u16> +
        FromSample<i24> +
        FromSample<u24> +
        FromSample<i32> +
        FromSample<u32> +
        FromSample<f32> +
        FromSample<f64> {}

impl<S> ConvertibleSample for S
where
    S: Sample +
        FromSample<i8> +
        FromSample<u8> +
        FromSample<i16> +
        FromSample<u16> +
        FromSample<i24> +
        FromSample<u24> +
        FromSample<i32> +
        FromSample<u32> +
        FromSample<f32> +
        FromSample<f64> {}

#[cfg(test)]
mod tests {
    use core::{u8, i8, u16, i16, u32, i32};
    use crate::sample::{u24, i24, Sample};
    use super::FromSample;

    #[test]
    fn verify_u8_from_sample() {
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

    #[test]
    fn verify_f64_from_sample() {
        assert_eq!(f64::from_sample(u8::MAX),  127.0 / 128.0);
        assert_eq!(f64::from_sample(u8::MID),  0.0);
        assert_eq!(f64::from_sample(u8::MIN), -1.0);

        assert_eq!(f64::from_sample(u16::MAX),  32_767.0 / 32_768.0);
        assert_eq!(f64::from_sample(u16::MID),  0.0);
        assert_eq!(f64::from_sample(u16::MIN), -1.0);

        assert_eq!(f64::from_sample(u24::MAX),  8_388_607.0 / 8_388_608.0);
        assert_eq!(f64::from_sample(u24::MID),  0.0);
        assert_eq!(f64::from_sample(u24::MIN), -1.0);

        assert_eq!(f64::from_sample(u32::MAX),  2_147_483_647.0 / 2_147_483_648.0);
        assert_eq!(f64::from_sample(u32::MID),  0.0);
        assert_eq!(f64::from_sample(u32::MIN), -1.0);

        assert_eq!(f64::from_sample(i8::MAX),  127.0 / 128.0);
        assert_eq!(f64::from_sample(i8::MID),  0.0);
        assert_eq!(f64::from_sample(i8::MIN), -1.0);

        assert_eq!(f64::from_sample(i16::MAX),  32_767.0 / 32_768.0);
        assert_eq!(f64::from_sample(i16::MID),  0.0);
        assert_eq!(f64::from_sample(i16::MIN), -1.0);

        assert_eq!(f64::from_sample(i24::MAX),  8_388_607.0 / 8_388_608.0);
        assert_eq!(f64::from_sample(i24::MID),  0.0);
        assert_eq!(f64::from_sample(i24::MIN), -1.0);

        assert_eq!(f64::from_sample(i32::MAX),  2_147_483_647.0 / 2_147_483_648.0);
        assert_eq!(f64::from_sample(i32::MID),  0.0);
        assert_eq!(f64::from_sample(i32::MIN), -1.0);

        assert_eq!(f64::from_sample( 1.0f32),  1.0);
        assert_eq!(f64::from_sample(   0f32),  0.0);
        assert_eq!(f64::from_sample(-1.0f32), -1.0);

        assert_eq!(f64::from_sample( 1.0f64),  1.0);
        assert_eq!(f64::from_sample(   0f64),  0.0);
        assert_eq!(f64::from_sample(-1.0f64), -1.0);
    }

    #[test]
    fn verify_f32_from_sample() {
        assert_eq!(f32::from_sample(u8::MAX),  127.0 / 128.0);
        assert_eq!(f32::from_sample(u8::MID),  0.0);
        assert_eq!(f32::from_sample(u8::MIN), -1.0);

        assert_eq!(f32::from_sample(u16::MAX),  32_767.0 / 32_768.0);
        assert_eq!(f32::from_sample(u16::MID),  0.0);
        assert_eq!(f32::from_sample(u16::MIN), -1.0);

        assert_eq!(f32::from_sample(u24::MAX),  8_388_607.0 / 8_388_608.0);
        assert_eq!(f32::from_sample(u24::MID),  0.0);
        assert_eq!(f32::from_sample(u24::MIN), -1.0);

        assert_eq!(f32::from_sample(u32::MAX),  2_147_483_647.0 / 2_147_483_648.0);
        assert_eq!(f32::from_sample(u32::MID),  0.0);
        assert_eq!(f32::from_sample(u32::MIN), -1.0);

        assert_eq!(f32::from_sample(i8::MAX),  127.0 / 128.0);
        assert_eq!(f32::from_sample(i8::MID),  0.0);
        assert_eq!(f32::from_sample(i8::MIN), -1.0);

        assert_eq!(f32::from_sample(i16::MAX),  32_767.0 / 32_768.0);
        assert_eq!(f32::from_sample(i16::MID),  0.0);
        assert_eq!(f32::from_sample(i16::MIN), -1.0);

        assert_eq!(f32::from_sample(i24::MAX),  8_388_607.0 / 8_388_608.0);
        assert_eq!(f32::from_sample(i24::MID),  0.0);
        assert_eq!(f32::from_sample(i24::MIN), -1.0);

        assert_eq!(f32::from_sample(i32::MAX),  2_147_483_647.0 / 2_147_483_648.0);
        assert_eq!(f32::from_sample(i32::MID),  0.0);
        assert_eq!(f32::from_sample(i32::MIN), -1.0);

        assert_eq!(f32::from_sample( 1.0f32),  1.0);
        assert_eq!(f32::from_sample(   0f32),  0.0);
        assert_eq!(f32::from_sample(-1.0f32), -1.0);

        assert_eq!(f32::from_sample( 1.0f64),  1.0);
        assert_eq!(f32::from_sample(   0f64),  0.0);
        assert_eq!(f32::from_sample(-1.0f64), -1.0);
    }
}