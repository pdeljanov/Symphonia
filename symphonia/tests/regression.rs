// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Decodes small files with the default registries, and compares a hash of the decoded 16-bit
//! samples with the hash of a decoding that was verified against `ffmpeg` (except where noted).
//!
//! The G.726 hashes are those of this decoder, which follows ITU-T G.726 and differs slightly from
//! the output of `ffmpeg`.
#![cfg(all(
    feature = "adpcm",
    feature = "isomp4",
    feature = "pcm",
    feature = "tta",
    feature = "wav",
    feature = "wavpack"
))]

use std::fs::File;
use std::path::PathBuf;

use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// The FNV-1a hash of the samples, as little-endian 16-bit integers.
fn fnv1a(samples: &[i16]) -> u64 {
    samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
        })
}

fn decode_file(name: &str) -> Vec<i16> {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", name].iter().collect();
    let mss = MediaSourceStream::new(Box::new(File::open(path).unwrap()), Default::default());

    let mut format = symphonia::default::get_probe()
        .probe(&Hint::new(), mss, FormatOptions::default(), MetadataOptions::default())
        .unwrap();

    let track = format.default_track(TrackType::Audio).unwrap();
    let track_id = track.id;

    let Some(CodecParameters::Audio(params)) = &track.codec_params
    else {
        panic!("expected audio codec parameters");
    };

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .unwrap();

    let mut samples = Vec::new();
    let mut buf = Vec::new();

    while let Some(packet) = format.next_packet().unwrap() {
        if packet.track_id != track_id {
            continue;
        }

        decoder.decode(&packet).unwrap().copy_to_vec_interleaved::<f32>(&mut buf);
        samples.extend(buf.iter().map(|&s| (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16));
    }

    samples
}

#[test]
fn decode_matches_known_hashes() {
    #[rustfmt::skip]
    let expected = [
        ("g722_mono.wav", 0xa38f_49f8_0bd7_e4c7, 4800),
        ("g726_32k_mono.wav", 0x1392_32ca_a73d_637f, 2400),
        ("g726_40k_mono.wav", 0xbb48_502a_2eb7_ab09, 2400),
        ("ima_qt_stereo.mov", 0x89a7_ad92_d4fe_a86a, 13230),
        ("ima_wav_stereo.wav", 0x0aab_f986_7887_2055, 14238),
        ("ms_stereo.wav", 0xdef8_8ccd_1cf3_870a, 14168),
        ("pcm_be_mono.mov", 0x1e85_7beb_8442_2f45, 2400),
        ("tta_stereo.tta", 0x33d6_0f4f_d155_9c21, 9600),
        ("wavpack_mono.wv", 0x3698_be9d_ad8a_4373, 4800),
        ("yamaha_stereo.wav", 0xb199_4795_e8f7_c657, 14336),
    ];

    for (name, hash, len) in expected {
        let samples = decode_file(name);
        assert_eq!(samples.len(), len, "{name}: number of samples");
        assert_eq!(fnv1a(&samples), hash, "{name}: hash of samples");
    }
}
