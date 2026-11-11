//! Channel layout of one elementary Opus stream: mono or coupled stereo.
//!
//! The stereo flag of the TOC byte selects the layout of a frame
//! ([RFC 6716 Section 3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>)), and the
//! channel mapping table of a multistream packet declares which of its streams are coupled
//! ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)).

/// Mono or coupled stereo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Layout {
    /// One channel.
    Mono,
    /// Two channels coded together.
    Stereo,
}

impl Layout {
    /// The layout the TOC stereo flag, or a coupled stream, selects.
    pub(crate) const fn from_stereo_flag(stereo: bool) -> Self {
        return match stereo {
            true => Self::Stereo,
            false => Self::Mono,
        };
    }

    /// The layout of `channels` channels, `None` beyond two.
    pub(crate) const fn from_channels(channels: usize) -> Option<Self> {
        return match channels {
            1 => Some(Self::Mono),
            2 => Some(Self::Stereo),
            _ => None,
        };
    }

    /// The number of channels: 1 or 2.
    pub(crate) const fn channels(self) -> usize {
        return match self {
            Self::Mono => 1,
            Self::Stereo => 2,
        };
    }

    /// Whether the layout has a second channel.
    pub(crate) const fn is_stereo(self) -> bool {
        return matches!(self, Self::Stereo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_round_trip_through_flag_and_channel_count() {
        for (stereo, channels, layout) in [(false, 1, Layout::Mono), (true, 2, Layout::Stereo)] {
            assert_eq!(Layout::from_stereo_flag(stereo), layout);
            assert_eq!(Layout::from_channels(channels), Some(layout));
            assert_eq!(layout.channels(), channels);
            assert_eq!(layout.is_stereo(), stereo);
        }
        assert_eq!(Layout::from_channels(0), None);
        assert_eq!(Layout::from_channels(3), None);
    }
}
