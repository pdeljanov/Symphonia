// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The `units` module provides definitions for common units.

use std::{fmt, ops};

/// A `TimeStamp` represents an instantenous instant in time since the start of a stream. One
/// `TimeStamp` "tick" is equivalent to the stream's `TimeBase` in seconds.
pub type TimeStamp = u64;

/// A `Duration` indicates a positive span of time.
pub type Duration = u64;

#[inline(always)]
fn div_rem<T: ops::Div<Output = T> + ops::Rem<Output = T> + Copy>(x: T, y: T) -> (T, T) {
    let quot = x / y;
    let rem = x % y;
    (quot, rem)
}

/// `Time` represents a duration of time in seconds, or the number of seconds since an arbitrary
/// epoch. `Time` is stored as an integer number of seconds plus any remaining fraction of a second
/// as a floating point value.
#[derive(Copy, Clone, Debug, Default, PartialEq, PartialOrd)]
pub struct Time {
    pub seconds: u64,
    pub frac: f64,
}

impl Time {
    /// Milliseconds per second.
    const MS_PER_SEC: u32 = 1_000;
    /// Microseconds per second.
    const US_PER_SEC: u32 = 1_000_000;
    /// Nanoseconds per second.
    const NS_PER_SEC: u32 = 1_000_000_000;
    /// Seconds per hour.
    const SECS_PER_HR: u64 = 60 * 60;
    /// Seconds per minute.
    const SECS_PER_MIN: u64 = 60;

    /// Instantiate from a count and seconds, and a fraction of a second.
    ///
    /// # Panics
    ///
    /// Panics if `frac` is < 0.0, or >= 1.0.
    pub fn new(seconds: u64, frac: f64) -> Self {
        assert!(frac >= 0.0 && frac < 1.0, "fractional seconds must be between [0.0, 1.0)");
        Time { seconds, frac }
    }

    /// Instantiate from a count of nanoseconds.
    pub fn from_ns(ns: u64) -> Time {
        let (seconds, rem) = div_rem(ns, u64::from(Time::NS_PER_SEC));

        Time { seconds, frac: f64::from(rem as u32) / f64::from(Time::NS_PER_SEC) }
    }

    /// Instantiate from a count of microseconds.
    pub fn from_us(us: u64) -> Time {
        let (seconds, rem) = div_rem(us, u64::from(Time::US_PER_SEC));

        Time { seconds, frac: f64::from(rem as u32) / f64::from(Time::US_PER_SEC) }
    }

    /// Instantiate from a count of milliseconds.
    pub fn from_ms(ms: u64) -> Time {
        let (seconds, rem) = div_rem(ms, u64::from(Time::MS_PER_SEC));

        Time { seconds, frac: f64::from(rem as u32) / f64::from(Time::MS_PER_SEC) }
    }

    pub fn from_ss(s: u8, ns: u32) -> Option<Time> {
        if s > 59 || ns >= Time::NS_PER_SEC {
            return None;
        }

        let seconds = u64::from(s);
        let frac = f64::from(ns) / f64::from(Time::NS_PER_SEC);

        Some(Time { seconds, frac })
    }

    pub fn from_mmss(m: u8, s: u8, ns: u32) -> Option<Time> {
        if m > 59 || s > 59 || ns >= Time::NS_PER_SEC {
            return None;
        }

        let seconds = (Time::SECS_PER_MIN * u64::from(m)) + u64::from(s);
        let frac = f64::from(ns) / f64::from(Time::NS_PER_SEC);

        Some(Time { seconds, frac })
    }

    pub fn from_hhmmss(h: u32, m: u8, s: u8, ns: u32) -> Option<Time> {
        if m > 59 || s > 59 || ns >= Time::NS_PER_SEC {
            return None;
        }

        let seconds =
            (Time::SECS_PER_HR * u64::from(h)) + (Time::SECS_PER_MIN * u64::from(m)) + u64::from(s);

        let frac = f64::from(ns) / f64::from(Time::NS_PER_SEC);

        Some(Time { seconds, frac })
    }
}

impl From<u8> for Time {
    fn from(seconds: u8) -> Self {
        Time::new(u64::from(seconds), 0.0)
    }
}

impl From<u16> for Time {
    fn from(seconds: u16) -> Self {
        Time::new(u64::from(seconds), 0.0)
    }
}

impl From<u32> for Time {
    fn from(seconds: u32) -> Self {
        Time::new(u64::from(seconds), 0.0)
    }
}

impl From<u64> for Time {
    fn from(seconds: u64) -> Self {
        Time::new(seconds, 0.0)
    }
}

impl From<f32> for Time {
    fn from(seconds: f32) -> Self {
        if seconds >= 0.0 {
            Time::new(seconds.trunc() as u64, f64::from(seconds.fract()))
        }
        else {
            Time::new(0, 0.0)
        }
    }
}

impl From<f64> for Time {
    fn from(seconds: f64) -> Self {
        if seconds >= 0.0 {
            Time::new(seconds.trunc() as u64, seconds.fract())
        }
        else {
            Time::new(0, 0.0)
        }
    }
}

impl From<std::time::Duration> for Time {
    fn from(duration: std::time::Duration) -> Self {
        Time::new(duration.as_secs(), f64::from(duration.subsec_nanos()) / 1_000_000_000.0)
    }
}

impl From<Time> for std::time::Duration {
    fn from(time: Time) -> Self {
        std::time::Duration::new(time.seconds, (1_000_000_000.0 * time.frac) as u32)
    }
}

/// A `TimeBase` is the conversion factor between time, expressed in seconds, and a `TimeStamp` or
/// `Duration`.
///
/// In other words, a `TimeBase` is the length in seconds of one tick of a `TimeStamp` or
/// `Duration`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimeBase {
    /// The numerator.
    pub numer: u32,
    /// The denominator.
    pub denom: u32,
}

impl TimeBase {
    /// Creates a new `TimeBase`. Panics if either the numerator or denominator is 0.
    pub fn new(numer: u32, denom: u32) -> Self {
        if numer == 0 || denom == 0 {
            panic!("TimeBase cannot have 0 numerator or denominator");
        }

        TimeBase { numer, denom }
    }

    /// Accurately calculates a `Time` using the `TimeBase` and the provided `TimeStamp`. On
    /// overflow, the seconds field of `Time` wraps.
    pub fn calc_time(&self, ts: TimeStamp) -> Time {
        assert!(self.numer > 0 && self.denom > 0, "TimeBase numerator or denominator are 0.");

        // The dividend possibly requires up-to 96 bits (32-bit timebase numerator * 64-bit
        // timestamp).
        let (secs, rem) = if let Some(dividend) = ts.checked_mul(u64::from(self.numer)) {
            // The dividend requires <= 64 bits (common case).
            let secs = dividend / u64::from(self.denom);
            let rem = dividend % u64::from(self.denom);
            // The denominator is a u32, therefore any value mod the denominator will yield a value
            // less-than u32::MAX, so the cast will never truncate.
            (secs, rem as u32)
        }
        else {
            // The dividend requires > 64 bits.
            let dividend = u128::from(ts) * u128::from(self.numer);
            let secs = dividend / u128::from(self.denom);
            let rem = dividend % u128::from(self.denom);
            // Note: Seconds will wrap if too large.
            (secs as u64, rem as u32)
        };

        // Use a f64 since a f32 cannot represent all 32-bit unsigned integers.
        let frac = f64::from(rem) / f64::from(self.denom);

        Time::new(secs, frac)
    }

    /// Accurately calculates a `TimeStamp` from the given `Time` using the `TimeBase` as the
    /// conversion factor. On overflow, the `TimeStamp` wraps.
    pub fn calc_timestamp(&self, time: Time) -> TimeStamp {
        assert!(self.numer > 0 && self.denom > 0, "TimeBase numerator or denominator are 0.");
        assert!(time.frac >= 0.0 && time.frac < 1.0, "Invalid range for Time fractional part.");

        // The product possibly requires up-to 96 bits (32-bit timebase denominator * 64-bit
        // whole seconds).
        let (ticks, rem) = if let Some(product) = time.seconds.checked_mul(u64::from(self.denom)) {
            // The product requires <= 64 bits (common case).
            let ticks = product / u64::from(self.numer);
            let rem = product % u64::from(self.numer);
            (ticks, rem)
        }
        else {
            // The product requires > 64 bits.
            let product = u128::from(time.seconds) * u128::from(self.denom);
            let ticks = product / u128::from(self.numer);
            let rem = product % u128::from(self.numer);
            // Note: Timestamp will wrap if too large.
            (ticks as u64, rem as u64)
        };

        // Note: Use a u64 since up-to 33 bits are required for the sum.
        let frac_ticks = (rem + (time.frac * f64::from(self.denom)) as u64) / u64::from(self.numer);

        ticks.wrapping_add(frac_ticks)
    }
}

impl From<TimeBase> for f64 {
    fn from(timebase: TimeBase) -> Self {
        f64::from(timebase.numer) / f64::from(timebase.denom)
    }
}

impl fmt::Display for TimeBase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.numer, self.denom)
    }
}

#[cfg(test)]
mod tests {
    use super::{Time, TimeBase};
    use std::time::Duration;

    #[test]
    fn verify_timebase() {
        // Verify accuracy of timestamp -> time
        let tb1 = TimeBase::new(1, 320);

        assert_eq!(tb1.calc_time(0), Time::new(0, 0.0));
        assert_eq!(tb1.calc_time(12_345), Time::new(38, 0.578125));
        assert_eq!(tb1.calc_time(0x0f_ffff_ffff_ffff), Time::new(14_073_748_835_532, 0.796875));
        assert_eq!(tb1.calc_time(0x10_0000_0000_0001), Time::new(14_073_748_835_532, 0.803125));
        assert_eq!(tb1.calc_time(u64::MAX), Time::new(57_646_075_230_342_348, 0.796875));

        // More precision tests...
        assert_eq!(TimeBase::new(1, 1000).calc_time(6471214), Time::new(6471, 0.214));

        // Verify overflow wraps seconds
        let tb2 = TimeBase::new(320, 1);
        assert_eq!(tb2.calc_time(u64::MAX), Time::new(18_446_744_073_709_551_296, 0.0));

        // Verify accuracy of time -> timestamp
        assert_eq!(tb1.calc_timestamp(Time::new(0, 0.0)), 0);
        assert_eq!(tb1.calc_timestamp(Time::new(38, 0.578125)), 12_345);
        assert_eq!(
            tb1.calc_timestamp(Time::new(14_073_748_835_532, 0.796875)),
            0x0f_ffff_ffff_ffff
        );
        assert_eq!(
            tb1.calc_timestamp(Time::new(14_073_748_835_532, 0.803125)),
            0x10_0000_0000_0001
        );
        assert_eq!(tb1.calc_timestamp(Time::new(57_646_075_230_342_348, 0.796875)), u64::MAX);

        // More precision tests...
        assert_eq!(TimeBase::new(89399, 12341).calc_timestamp(Time::new(1012, 0.1129811)), 139);
        assert_eq!(TimeBase::new(89399, 12341).calc_timestamp(Time::new(1021, 0.9999999)), 141);
    }

    #[test]
    fn verify_duration_to_time() {
        // Verify accuracy of Duration -> Time
        let dur1 = Duration::from_secs_f64(38.578125);
        let time1 = Time::from(dur1);

        assert_eq!(time1.seconds, 38);
        assert_eq!(time1.frac, 0.578125);
    }

    #[test]
    fn verify_time_to_duration() {
        // Verify accuracy of Time -> Duration
        let time1 = Time::new(38, 0.578125);
        let dur1 = Duration::from(time1);

        let seconds = dur1.as_secs_f64();

        assert_eq!(seconds.trunc(), 38.0);
        assert_eq!(seconds.fract(), 0.578125);
    }
}
