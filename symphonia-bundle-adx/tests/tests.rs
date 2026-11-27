// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![allow(clippy::unwrap_used)]

use symphonia_bundle_adx::{AdxDecoder, AdxReader};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia_core::formats::FormatOptions;
use symphonia_core::formats::probe::ProbeableFormat;
use symphonia_core::io::MediaSourceStream;

/// Decodes a stream, and returns its channels and its samples interleaved as 16-bit values.
fn decode(data: &[u8]) -> (usize, Vec<i16>) {
    let mss =
        MediaSourceStream::new(Box::new(std::io::Cursor::new(data.to_vec())), Default::default());
    let mut reader = AdxReader::try_probe_new(mss, FormatOptions::default()).unwrap();

    let Some(CodecParameters::Audio(params)) = reader.tracks()[0].codec_params.clone()
    else {
        panic!("not an audio track");
    };
    let mut decoder = AdxDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();

    let mut channels = 0;
    let mut out = Vec::new();

    while let Some(packet) = reader.next_packet().unwrap() {
        let buf = decoder.decode(&packet).unwrap();
        channels = buf.spec().channels().count();
        let mut interleaved = vec![0f32; buf.frames() * channels];
        buf.copy_to_slice_interleaved(&mut interleaved);
        out.extend(interleaved.iter().map(|&s| (s * 32768.0).round() as i16));
    }

    (channels, out)
}

fn check(stream: &[u8], expected: &[u8], channels: usize) {
    let (n, got) = decode(stream);
    let want: Vec<i16> =
        expected.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    assert_eq!(n, channels);
    assert_eq!(got, want);
}

#[test]
fn mono_is_exact() {
    check(include_bytes!("fixtures/mono.adx"), include_bytes!("fixtures/mono.s16"), 1);
}

#[test]
fn stereo_is_exact() {
    check(include_bytes!("fixtures/stereo.adx"), include_bytes!("fixtures/stereo.s16"), 2);
}
