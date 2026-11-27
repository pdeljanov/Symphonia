// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![allow(clippy::unwrap_used)]

use symphonia_bundle_truehd::{TrueHdDecoder, TrueHdReader};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia_core::formats::FormatOptions;
use symphonia_core::formats::probe::ProbeableFormat;
use symphonia_core::io::MediaSourceStream;

/// Decodes a stream, and returns its channels and its interleaved samples.
fn decode(data: &[u8]) -> (usize, Vec<i32>) {
    let mss =
        MediaSourceStream::new(Box::new(std::io::Cursor::new(data.to_vec())), Default::default());
    let mut reader = TrueHdReader::try_probe_new(mss, FormatOptions::default()).unwrap();

    let Some(CodecParameters::Audio(params)) = reader.tracks()[0].codec_params.clone()
    else {
        panic!("not an audio track");
    };
    let mut decoder = TrueHdDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();

    let mut channels = 0;
    let mut out = Vec::new();

    while let Some(packet) = reader.next_packet().unwrap() {
        let buf = decoder.decode(&packet).unwrap();
        channels = buf.spec().channels().count();
        let mut interleaved = vec![0i32; buf.frames() * channels];
        buf.copy_to_slice_interleaved(&mut interleaved);
        out.extend(interleaved);
    }

    (channels, out)
}

fn check(stream: &[u8], expected: &[u8], channels: usize) {
    let (n, got) = decode(stream);
    let want: Vec<i32> =
        expected.chunks_exact(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    assert_eq!(n, channels);
    assert_eq!(got, want);
}

#[test]
fn truehd_is_exact() {
    check(include_bytes!("fixtures/stereo.thd"), include_bytes!("fixtures/stereo_thd.s32"), 2);
}

#[test]
fn mlp_is_exact() {
    check(include_bytes!("fixtures/stereo.mlp"), include_bytes!("fixtures/stereo_mlp.s32"), 2);
}
