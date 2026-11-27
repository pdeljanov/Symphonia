// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![allow(clippy::unwrap_used)]

use symphonia_bundle_sbc::{SbcDecoder, SbcReader};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia_core::formats::{FormatOptions, FormatReader};
use symphonia_core::io::MediaSourceStream;

/// The most that the floating point output may differ from the fixed point one of ffmpeg.
const TOLERANCE: i32 = 4;

/// Decodes a stream, and returns its channels and its samples interleaved as 16-bit values.
fn decode(data: &[u8]) -> (usize, Vec<i16>) {
    let mss =
        MediaSourceStream::new(Box::new(std::io::Cursor::new(data.to_vec())), Default::default());
    let mut reader = SbcReader::try_new(mss, FormatOptions::default()).unwrap();

    let Some(CodecParameters::Audio(params)) = reader.tracks()[0].codec_params.clone()
    else {
        panic!("not an audio track");
    };
    let mut decoder = SbcDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();

    let mut channels = 0;
    let mut out = Vec::new();

    while let Some(packet) = reader.next_packet().unwrap() {
        let buf = decoder.decode(&packet).unwrap();
        channels = buf.spec().channels().count();
        let mut interleaved = vec![0f32; buf.frames() * channels];
        buf.copy_to_slice_interleaved(&mut interleaved);
        out.extend(
            interleaved.iter().map(|&s| (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16),
        );
    }

    (channels, out)
}

fn check(stream: &[u8], expected: &[u8], channels: usize) {
    let (n, got) = decode(stream);
    let want: Vec<i16> =
        expected.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    assert_eq!(n, channels);
    assert_eq!(got.len(), want.len());
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        assert!((i32::from(*g) - i32::from(*w)).abs() <= TOLERANCE, "sample {i}: {g} != {w}");
    }
}

macro_rules! fixture_test {
    ($name:ident, $file:literal, $channels:expr) => {
        #[test]
        fn $name() {
            check(
                include_bytes!(concat!("fixtures/", $file, ".sbc")),
                include_bytes!(concat!("fixtures/", $file, ".s16")),
                $channels,
            );
        }
    };
}

fixture_test!(joint_stereo_8_subbands_loudness, "joint8_loud", 2);
fixture_test!(joint_stereo_4_subbands_loudness, "joint4_loud", 2);
fixture_test!(stereo_8_subbands_snr, "stereo8_snr", 2);
fixture_test!(dual_channel_8_subbands_loudness, "dual8_loud", 2);
fixture_test!(mono_4_subbands_snr, "mono4_snr", 1);
fixture_test!(mono_8_subbands_16khz, "mono8_16k", 1);

#[test]
fn resynchronizes_after_junk() {
    let stream = include_bytes!("fixtures/mono4_snr.sbc");
    let mut junk = vec![0x12, 0x9c, 0x00, 0x34];
    junk.extend_from_slice(stream);

    assert_eq!(decode(&junk), decode(stream));
}
