//! Multi-stream packet splitting and output channel routing
//! ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)).
//!
//! A multi-stream packet
//! ([RFC 7845 Section 3](<https://datatracker.ietf.org/doc/html/rfc7845#section-3>)) carries
//! `N - 1` self-delimited elementary stream packets
//! ([RFC 6716 Appendix B](<https://datatracker.ietf.org/doc/html/rfc6716#appendix-B>)) followed
//! by one undelimited packet. [`OpusDecoder`](crate::decoder::OpusDecoder) decodes the `M`
//! coupled stereo streams and the `N - M` uncoupled mono streams with one
//! [`StreamDecoder`](crate::stream::StreamDecoder) each and routes their channels to the output
//! planes through the `OpusHead` channel mapping table
//! ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)),
//! scaled by the Q7.8 dB `output_gain`
//! ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)).

use symphonia_core::audio::{AudioBuffer, AudioMut, Channels, Position};
use symphonia_core::errors::{Error as SymphoniaError, Result};

use crate::packet::FramePacket;
use crate::stream::MAX_PACKET_SAMPLES_48K;

/// The elementary stream packets of a multi-stream Opus packet, in stream order: `count - 1`
/// self-delimited packets
/// ([RFC 6716 Appendix B](<https://datatracker.ietf.org/doc/html/rfc6716#appendix-B>)) followed
/// by one undelimited packet that runs to the end of the data
/// ([RFC 7845 Section 3](<https://datatracker.ietf.org/doc/html/rfc7845#section-3>)).
pub(crate) struct StreamPackets<'a> {
    data: &'a [u8],
    remaining: usize,
}

impl<'a> StreamPackets<'a> {
    /// Splits `data` into `count` elementary stream packets.
    pub(crate) const fn new(data: &'a [u8], count: usize) -> Self {
        return Self { data, remaining: count };
    }
}

impl<'a> Iterator for StreamPackets<'a> {
    type Item = Result<FramePacket<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        self.remaining = self.remaining.checked_sub(1)?;
        if self.remaining == 0 {
            return Some(FramePacket::new(self.data).map_err(SymphoniaError::from));
        }
        let parsed = FramePacket::parse_self_delimited(self.data).map_err(SymphoniaError::from);
        return Some(parsed.map(|(packet, consumed)| {
            self.data = &self.data[consumed..];
            return packet;
        }));
    }
}

/// Where each decoded stream channel goes
/// ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)):
/// coupled stream `i` feeds the output channels whose mapping entry is `2i` (left) or `2i + 1`
/// (right); uncoupled stream `i` feeds the entries equal to `coupled_count + i`. Samples are
/// scaled by the output gain on the way
/// ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)).
#[derive(Clone, Copy)]
pub(crate) struct OutputRouting<'a> {
    pub(crate) channel_mapping: &'a [u8],
    pub(crate) coupled_count: usize,
    /// The output plane of each output channel.
    pub(crate) plane_map: &'a [usize],
    /// Linear output gain, `None` when the `output_gain` field is zero.
    pub(crate) gain: Option<f32>,
}

impl OutputRouting<'_> {
    /// Copies `total_samples` of stream `stream_i` from the `scratch` rows into its output planes.
    pub(crate) fn route(
        &self,
        stream_i: usize,
        total_samples: usize,
        scratch: &[[f32; MAX_PACKET_SAMPLES_48K]; 2],
        buffer: &mut AudioBuffer<f32>,
    ) -> Result<()> {
        let is_coupled = stream_i < self.coupled_count;
        let mono = (self.coupled_count + stream_i, 1);
        let (first, count) = if is_coupled { (2 * stream_i, 2) } else { mono };

        for (ch, &map) in self.channel_mapping.iter().enumerate() {
            let Some(k) = usize::from(map).checked_sub(first).filter(|k| *k < count)
            else {
                continue;
            };
            let src = &scratch[k][..total_samples];
            let dst = buffer
                .plane_mut(self.plane_map.get(ch).copied().unwrap_or(ch))
                .ok_or(SymphoniaError::DecodeError("opus: missing output plane"))?;
            let dst = &mut dst[..total_samples];
            match self.gain {
                Some(scale) => dst.iter_mut().zip(src).for_each(|(d, &s)| *d = s * scale),
                None => dst.copy_from_slice(src),
            }
        }
        return Ok(());
    }
}

/// The output plane of each channel of an `OpusHead` mapping family 1 stream (Vorbis channel
/// order, [RFC 7845 Section 5.1.1.2](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1.2>)),
/// given the canonical plane order of `channels`.
pub(crate) fn vorbis_plane_map(channels: &Channels) -> Vec<usize> {
    use Position as P;

    let count = channels.count();
    let order: &[Position] = match count {
        3 => &[P::FRONT_LEFT, P::FRONT_CENTER, P::FRONT_RIGHT],
        4 => &[P::FRONT_LEFT, P::FRONT_RIGHT, P::REAR_LEFT, P::REAR_RIGHT],
        5 => &[P::FRONT_LEFT, P::FRONT_CENTER, P::FRONT_RIGHT, P::REAR_LEFT, P::REAR_RIGHT],
        6 => {
            &[P::FRONT_LEFT, P::FRONT_CENTER, P::FRONT_RIGHT, P::REAR_LEFT, P::REAR_RIGHT, P::LFE1]
        }
        7 => &[
            P::FRONT_LEFT,
            P::FRONT_CENTER,
            P::FRONT_RIGHT,
            P::SIDE_LEFT,
            P::SIDE_RIGHT,
            P::REAR_CENTER,
            P::LFE1,
        ],
        8 => &[
            P::FRONT_LEFT,
            P::FRONT_CENTER,
            P::FRONT_RIGHT,
            P::SIDE_LEFT,
            P::SIDE_RIGHT,
            P::REAR_LEFT,
            P::REAR_RIGHT,
            P::LFE1,
        ],
        _ => &[],
    };

    if order.is_empty() {
        return (0..count).collect();
    }

    let map: Option<Vec<usize>> =
        order.iter().map(|&pos| channels.get_canonical_index_for_positioned_channel(pos)).collect();

    map.unwrap_or_else(|| (0..count).collect())
}
