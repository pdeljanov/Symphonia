// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![allow(clippy::unwrap_used)]

use symphonia_bundle_dfpwm::{DfpwmDecoder, DfpwmReader};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia_core::formats::{FormatOptions, FormatReader};
use symphonia_core::io::MediaSourceStream;

/// Decodes a stream, and returns its samples interleaved as unsigned 8-bit values.
fn decode(data: &[u8], channels: u8) -> Vec<u8> {
    let mss =
        MediaSourceStream::new(Box::new(std::io::Cursor::new(data.to_vec())), Default::default());
    let mut reader =
        DfpwmReader::try_new_with(mss, FormatOptions::default(), 48000, channels).unwrap();

    let Some(CodecParameters::Audio(params)) = reader.tracks()[0].codec_params.clone()
    else {
        panic!("not an audio track");
    };
    let mut decoder = DfpwmDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();

    let mut out = Vec::new();

    while let Some(packet) = reader.next_packet().unwrap() {
        let buf = decoder.decode(&packet).unwrap();
        let mut interleaved = vec![0f32; buf.frames() * usize::from(channels)];
        buf.copy_to_slice_interleaved(&mut interleaved);
        out.extend(interleaved.iter().map(|&s| (s * 128.0).round() as i32 as u8 ^ 0x80));
    }

    out
}

#[test]
fn mono_is_exact() {
    let got = decode(include_bytes!("fixtures/mono.dfpwm"), 1);
    assert_eq!(got, include_bytes!("fixtures/mono.u8"));
}

#[test]
fn stereo_is_exact() {
    let got = decode(include_bytes!("fixtures/stereo.dfpwm"), 2);
    assert_eq!(got, include_bytes!("fixtures/stereo.u8"));
}
