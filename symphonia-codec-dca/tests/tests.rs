// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![allow(clippy::unwrap_used)]

use symphonia_codec_dca::{DcaDecoder, DcaReader};
use symphonia_core::codecs::audio::well_known::CODEC_ID_DCA;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia_core::formats::FormatOptions;
use symphonia_core::formats::probe::ProbeableFormat;
use symphonia_core::io::MediaSourceStream;

/// Decodes a stream and returns the channels interleaved as 16-bit samples.
fn decode(data: &[u8]) -> symphonia_core::errors::Result<(usize, Vec<i16>)> {
    let mss =
        MediaSourceStream::new(Box::new(std::io::Cursor::new(data.to_vec())), Default::default());
    let mut reader = DcaReader::try_probe_new(mss, FormatOptions::default())?;

    let mut decoder = DcaDecoder::try_new(
        AudioCodecParameters::new().for_codec(CODEC_ID_DCA),
        &AudioDecoderOptions::default(),
    )?;

    let mut channels = 0;
    let mut out = Vec::new();

    while let Some(packet) = reader.next_packet()? {
        let buf = decoder.decode(&packet)?;
        channels = buf.spec().channels().count();
        let mut interleaved = vec![0f32; buf.frames() * channels];
        buf.copy_to_slice_interleaved(&mut interleaved);
        out.extend(
            interleaved.iter().map(|&s| (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16),
        );
    }

    Ok((channels, out))
}

fn reference(data: &[u8]) -> Vec<i16> {
    data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
}

/// The reference output of an independent decoder, with the samples of every channel in the order
/// of the canonical channel layout.
fn check(stream: &[u8], expected: &[u8], channels: usize) {
    let (n, got) = decode(stream).unwrap();
    let want = reference(expected);
    assert_eq!(n, channels);
    assert_eq!(got.len(), want.len());
    assert!(got.iter().zip(&want).all(|(a, b)| (i32::from(*a) - i32::from(*b)).abs() <= 1));
}

#[test]
fn mono_matches_reference() {
    check(include_bytes!("fixtures/mono_256k.dts"), include_bytes!("fixtures/mono_256k.s16"), 1);
}

#[test]
fn surround_matches_reference() {
    check(
        include_bytes!("fixtures/surround_768k.dts"),
        include_bytes!("fixtures/surround_768k.s16"),
        6,
    );
}

#[test]
fn truncated_frame_is_an_error() {
    let data = include_bytes!("fixtures/mono_256k.dts");
    let mss =
        MediaSourceStream::new(Box::new(std::io::Cursor::new(data.to_vec())), Default::default());
    let mut reader = DcaReader::try_probe_new(mss, FormatOptions::default()).unwrap();
    let mut decoder = DcaDecoder::try_new(
        AudioCodecParameters::new().for_codec(CODEC_ID_DCA),
        &AudioDecoderOptions::default(),
    )
    .unwrap();

    let packet = reader.next_packet().unwrap().unwrap();
    let cut =
        symphonia_core::packet::Packet::new(0, packet.pts, packet.dur, packet.data[..100].to_vec());
    assert!(decoder.decode(&cut).is_err());
}
