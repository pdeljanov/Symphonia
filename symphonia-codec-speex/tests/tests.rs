// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![allow(clippy::unwrap_used)]

use symphonia_codec_speex::SpeexDecoder;
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia_core::formats::FormatOptions;
use symphonia_core::formats::probe::ProbeableFormat;
use symphonia_core::io::MediaSourceStream;
use symphonia_format_ogg::OggReader;

/// Decodes a stream, and returns its channels and its samples interleaved as 16-bit values.
fn decode(data: &[u8]) -> (usize, Vec<i16>) {
    let mss =
        MediaSourceStream::new(Box::new(std::io::Cursor::new(data.to_vec())), Default::default());
    let mut reader = OggReader::try_probe_new(mss, FormatOptions::default()).unwrap();

    let Some(CodecParameters::Audio(params)) = reader.tracks()[0].codec_params.clone()
    else {
        panic!("not an audio track");
    };
    let mut decoder = SpeexDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();

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

/// The output of libspeex. It rounds to 16-bit samples with a decoder that is not bit exact with
/// this one, so the streams are compared by their signal to noise ratio.
fn check(stream: &[u8], expected: &[u8], channels: usize) {
    let (n, got) = decode(stream);
    let want: Vec<i16> =
        expected.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    assert_eq!(n, channels);
    assert_eq!(got.len(), want.len());

    let signal: f64 = want.iter().map(|&w| f64::from(w).powi(2)).sum();
    let noise: f64 = got.iter().zip(&want).map(|(&g, &w)| f64::from(g - w).powi(2)).sum();
    let snr = 10.0 * (signal / noise.max(1.0)).log10();
    assert!(snr > 50.0, "snr {snr}");
}

#[test]
fn narrowband_matches_libspeex() {
    check(include_bytes!("fixtures/narrowband.ogg"), include_bytes!("fixtures/narrowband.s16"), 1);
}

#[test]
fn wideband_matches_libspeex() {
    check(include_bytes!("fixtures/wideband.ogg"), include_bytes!("fixtures/wideband.s16"), 1);
}

#[test]
fn ultra_wideband_stereo_matches_libspeex() {
    check(
        include_bytes!("fixtures/ultra_stereo.ogg"),
        include_bytes!("fixtures/ultra_stereo.s16"),
        2,
    );
}
