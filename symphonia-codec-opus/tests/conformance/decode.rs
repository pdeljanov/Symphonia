//! Driving the decoder like `opus_demo -d`: output layout, packet loop and final range checks.

use super::*;
use symphonia_codec_opus::OpusDecoder;
use symphonia_core::audio::{Channels, Position};
use symphonia_core::codecs::audio::AudioDecoder;
use symphonia_core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoderOptions};
use symphonia_core::packet::PacketRef;
use symphonia_core::units::{Duration, Timestamp};

/// Output channel layout of one conformance run (`opus_demo -d <rate> <channels>`).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Layout {
    Mono,
    Stereo,
}

impl Layout {
    pub(crate) fn channels(self) -> usize {
        return match self {
            Layout::Mono => 1,
            Layout::Stereo => 2,
        };
    }

    pub(crate) fn positions(self) -> Channels {
        return match self {
            Layout::Mono => Channels::Positioned(Position::FRONT_CENTER),
            Layout::Stereo => Channels::Positioned(Position::FRONT_LEFT | Position::FRONT_RIGHT),
        };
    }

    pub(crate) fn name(self) -> &'static str {
        return match self {
            Layout::Mono => "mono",
            Layout::Stereo => "stereo",
        };
    }
}

/// `FLOAT2INT16` of the reference float build (`celt/float_cast.h`): scale by `CELT_SIG_SCALE`,
/// saturate to 16 bits and round to nearest with ties to even (`lrintf`).
pub(crate) fn float_to_pcm16(sample: f32) -> i16 {
    let scaled = (sample * CELT_SIG_SCALE).clamp(f32::from(i16::MIN), f32::from(i16::MAX));
    return scaled.round_ties_even() as i16;
}

/// First packet whose decoder final range differs from the encoder final range.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RangeMismatch {
    pub(crate) packet: usize,
    pub(crate) expected: u32,
    pub(crate) actual: u32,
}

/// Output and final range outcome of decoding one `.bit` file.
pub(crate) struct Decoded {
    pub(crate) packets: usize,
    pub(crate) lost: usize,
    /// Packets whose final range `opus_demo` checks.
    pub(crate) checked: usize,
    pub(crate) range_mismatches: usize,
    pub(crate) first_range_mismatch: Option<RangeMismatch>,
    pub(crate) first_decode_error: Option<(usize, String)>,
    pub(crate) pcm: Vec<i16>,
}

/// Creates the decoder of `opus_demo -d <rate> <channels>`.
pub(crate) fn new_decoder(rate: u32, layout: Layout) -> OpusDecoder {
    let mut params = AudioCodecParameters::new();
    params.for_codec(CODEC_ID_OPUS).with_sample_rate(rate).with_channels(layout.positions());
    return OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap_or_else(|err| {
        panic!("cannot create a {rate} Hz {} decoder: {err}", layout.name())
    });
}

/// Decodes `records` like `opus_demo -d <rate> <channels>`: a zero-length payload is a lost
/// packet (`decode_ref` with an empty packet runs the loss path), a failed packet writes no
/// output, and the final range is checked unless the encoder range is zero or this or the
/// previous packet was lost (the first packet counts as preceded by a loss).
pub(crate) fn decode_records(records: &[BitRecord<'_>], rate: u32, layout: Layout) -> Decoded {
    let mut decoder = new_decoder(rate, layout);
    let mut decoded = Decoded {
        packets: records.len(),
        lost: 0,
        checked: 0,
        range_mismatches: 0,
        first_range_mismatch: None,
        first_decode_error: None,
        pcm: Vec::new(),
    };
    let mut samples: Vec<f32> = Vec::new();
    let mut lost_prev = true;
    for (i, record) in records.iter().enumerate() {
        let lost = record.payload.is_empty();
        let packet = PacketRef::new(0, Timestamp::from(0u32), Duration::from(0u64), record.payload);
        match decoder.decode_ref(&packet) {
            Ok(buf) => {
                buf.copy_to_vec_interleaved(&mut samples);
                decoded.pcm.extend(samples.iter().map(|&sample| float_to_pcm16(sample)));
            }
            Err(err) => {
                decoded.first_decode_error.get_or_insert_with(|| (i, err.to_string()));
            }
        }

        let actual = decoder.final_range();
        let checked = record.enc_final_range != 0 && !lost && !lost_prev;
        decoded.checked += usize::from(checked);
        if checked && actual != record.enc_final_range {
            decoded.range_mismatches += 1;
            decoded.first_range_mismatch.get_or_insert(RangeMismatch {
                packet: i,
                expected: record.enc_final_range,
                actual,
            });
        }
        decoded.lost += usize::from(lost);
        lost_prev = lost;
    }
    return decoded;
}
