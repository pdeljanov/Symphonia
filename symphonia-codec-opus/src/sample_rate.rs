//! Output sample rates of an Opus decoder
//! ([RFC 6716 Section 2](<https://datatracker.ietf.org/doc/html/rfc6716#section-2>)).
//!
//! A decoder produces 8, 12, 16, 24 or 48 kHz output. Every per-frame sample count in the crate
//! derives from the rate and an RFC 6716 frame duration, and the CELT layer decimates its 48 kHz
//! synthesis by the integer factor `48_000 / rate`
//! ([RFC 6716 Section 4.3.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7>)).
//!
//! ```text
//!   rate (Hz)   2.5 ms   5 ms   10 ms   20 ms   40 ms   60 ms   48 kHz / rate
//!   8_000       20       40     80      160     320     480     6
//!   12_000      30       60     120     240     480     720     4
//!   16_000      40       80     160     320     640     960     3
//!   24_000      60       120    240     480     960     1440    2
//!   48_000      120      240    480     960     1920    2880    1
//! ```

use symphonia_core::errors::Error;

use crate::toc::FrameDuration;

/// One of the five output sample rates an Opus decoder can produce
/// ([RFC 6716 Section 2](<https://datatracker.ietf.org/doc/html/rfc6716#section-2>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum SampleRate {
    /// 8 kHz, the narrowband rate.
    Hz8000 = 8_000,
    /// 12 kHz, the medium-band rate.
    Hz12000 = 12_000,
    /// 16 kHz, the wideband rate.
    Hz16000 = 16_000,
    /// 24 kHz, the super-wideband rate.
    Hz24000 = 24_000,
    /// 48 kHz, the fullband rate and the CELT synthesis rate.
    Hz48000 = 48_000,
}

impl SampleRate {
    const NANOS_PER_SECOND: u64 = 1_000_000_000;

    /// The rate in Hz ([RFC 6716 Section 2](<https://datatracker.ietf.org/doc/html/rfc6716#section-2>)).
    #[inline]
    pub(crate) const fn hz(self) -> u32 {
        return self as u32;
    }

    /// Number of samples per channel in a frame of `duration` at this rate
    /// ([RFC 6716 Section 2.1.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-2.1.4>)). Exact for every RFC 6716 duration, since
    /// all of them are multiples of 2.5 ms and every rate is a multiple of 400 Hz.
    #[inline]
    pub(crate) const fn samples_per(self, duration: FrameDuration) -> usize {
        return ((self.hz() as u64 * duration as u64) / Self::NANOS_PER_SECOND) as usize;
    }

    /// Integer decimation factor from the 48 kHz CELT synthesis rate to this rate
    /// ([RFC 6716 Section 4.3.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7>)).
    #[inline]
    pub(crate) const fn downsample_from_48k(self) -> usize {
        return (Self::Hz48000.hz() / self.hz()) as usize;
    }
}

impl TryFrom<u32> for SampleRate {
    type Error = Error;

    fn try_from(hz: u32) -> Result<Self, Self::Error> {
        return match hz {
            8_000 => Ok(Self::Hz8000),
            12_000 => Ok(Self::Hz12000),
            16_000 => Ok(Self::Hz16000),
            24_000 => Ok(Self::Hz24000),
            48_000 => Ok(Self::Hz48000),
            _ => Err(Error::DecodeError("opus: unsupported output sample rate")),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_counts_and_decimation_match_the_rate_table() {
        const RATES: [(SampleRate, usize, usize); 5] = [
            (SampleRate::Hz8000, 20, 6),
            (SampleRate::Hz12000, 30, 4),
            (SampleRate::Hz16000, 40, 3),
            (SampleRate::Hz24000, 60, 2),
            (SampleRate::Hz48000, 120, 1),
        ];
        for (rate, samples_2_5ms, downsample) in RATES {
            assert_eq!(rate.samples_per(FrameDuration::Ms2_5), samples_2_5ms);
            assert_eq!(rate.samples_per(FrameDuration::Ms20), samples_2_5ms * 8);
            assert_eq!(rate.samples_per(FrameDuration::Ms60), samples_2_5ms * 24);
            assert_eq!(rate.downsample_from_48k(), downsample);
            assert_eq!(SampleRate::try_from(rate.hz()).unwrap(), rate);
        }
        assert!(SampleRate::try_from(44_100).is_err());
        return;
    }
}
