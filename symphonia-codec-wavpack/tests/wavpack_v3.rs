// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Integration tests for the WavPack v1–v3 RIFF/WAVE format reader and decoder.

use std::fs::File;
use std::io::Cursor;

use symphonia_codec_wavpack::{WavPackDecoder, WavPackReader};
use symphonia_core::audio::layouts::{CHANNEL_LAYOUT_MONO, CHANNEL_LAYOUT_STEREO};
use symphonia_core::audio::sample::SampleFormat;
use symphonia_core::audio::{Audio, GenericAudioBufferRef};
use symphonia_core::codecs::audio::well_known::CODEC_ID_WAVPACK;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::formats::{FormatOptions, FormatReader};
use symphonia_core::formats::prelude::{Duration, Timestamp};
use symphonia_core::io::MediaSourceStream;
use symphonia_core::meta::RawValue;
use symphonia_core::packet::Packet;

// ---------------------------------------------------------------------------
// Binary builders
// ---------------------------------------------------------------------------

/// Build a `fmt ` chunk for PCM audio.
fn fmt_chunk(num_channels: u16, sample_rate: u32, bytes_per_sample: u16) -> Vec<u8> {
    let bps         = bytes_per_sample * 8;
    let block_align = num_channels * bytes_per_sample;
    let byte_rate   = sample_rate * block_align as u32;

    let mut c = Vec::new();
    c.extend_from_slice(b"fmt ");
    c.extend_from_slice(&16u32.to_le_bytes());
    c.extend_from_slice(&1u16.to_le_bytes());           // PCM
    c.extend_from_slice(&num_channels.to_le_bytes());
    c.extend_from_slice(&sample_rate.to_le_bytes());
    c.extend_from_slice(&byte_rate.to_le_bytes());
    c.extend_from_slice(&block_align.to_le_bytes());
    c.extend_from_slice(&bps.to_le_bytes());
    c
}

/// Build a `smpl` chunk (GrandOrgue layout).
fn smpl_chunk(midi_note: u32, pitch_fraction: u32, loops: &[(u32, u32)]) -> Vec<u8> {
    let loop_count = loops.len() as u32;
    let data_size  = 36 + loop_count * 24;

    let mut c = Vec::new();
    c.extend_from_slice(b"smpl");
    c.extend_from_slice(&data_size.to_le_bytes());
    c.extend_from_slice(&0u32.to_le_bytes()); // manufacturer
    c.extend_from_slice(&0u32.to_le_bytes()); // product
    c.extend_from_slice(&22675u32.to_le_bytes()); // sample_period
    c.extend_from_slice(&midi_note.to_le_bytes());
    c.extend_from_slice(&pitch_fraction.to_le_bytes());
    c.extend_from_slice(&0u32.to_le_bytes()); // smpte_format
    c.extend_from_slice(&0u32.to_le_bytes()); // smpte_offset
    c.extend_from_slice(&loop_count.to_le_bytes());
    c.extend_from_slice(&0u32.to_le_bytes()); // sampler_data
    for (i, (start, end)) in loops.iter().enumerate() {
        c.extend_from_slice(&(i as u32).to_le_bytes());
        c.extend_from_slice(&0u32.to_le_bytes());
        c.extend_from_slice(&start.to_le_bytes());
        c.extend_from_slice(&end.to_le_bytes());
        c.extend_from_slice(&0u32.to_le_bytes());
        c.extend_from_slice(&0u32.to_le_bytes());
    }
    c
}

/// Build a `cue ` chunk (GrandOrgue layout).
fn cue_chunk(offsets: &[u32]) -> Vec<u8> {
    let count     = offsets.len() as u32;
    let data_size = 4 + count * 24;
    let data_fcc  = u32::from_be_bytes(*b"data");

    let mut c = Vec::new();
    c.extend_from_slice(b"cue ");
    c.extend_from_slice(&data_size.to_le_bytes());
    c.extend_from_slice(&count.to_le_bytes());
    for (i, &off) in offsets.iter().enumerate() {
        c.extend_from_slice(&(i as u32).to_le_bytes());
        c.extend_from_slice(&off.to_le_bytes());
        c.extend_from_slice(&data_fcc.to_le_bytes());
        c.extend_from_slice(&0u32.to_le_bytes());
        c.extend_from_slice(&0u32.to_le_bytes());
        c.extend_from_slice(&off.to_le_bytes());
    }
    c
}

/// Build a single WavPack v3 `wvpk` block with the given compressed audio bytes.
///
/// For reader-only tests, pass arbitrary bytes as `audio`; for decoder tests,
/// `audio` must be a valid WavPack v3 compressed bitstream.
fn wvpk_v3_block(num_channels: u16, bytes_per_sample: u16, n_frames: i32, audio: &[u8]) -> Vec<u8> {
    const OVERHEAD: u32 = 28;
    let ck_size = OVERHEAD + audio.len() as u32;
    let bps     = (bytes_per_sample * 8) as i16;
    let flags   = if num_channels == 1 { 1i16 } else { 0i16 }; // MONO_FLAG

    let mut b = Vec::new();
    b.extend_from_slice(b"wvpk");
    b.extend_from_slice(&(ck_size as i32).to_le_bytes());
    b.extend_from_slice(&3i16.to_le_bytes());       // version
    b.extend_from_slice(&bps.to_le_bytes());
    b.extend_from_slice(&flags.to_le_bytes());
    b.extend_from_slice(&0i16.to_le_bytes());       // shift
    b.extend_from_slice(&n_frames.to_le_bytes());   // total_samples
    b.extend_from_slice(&0i32.to_le_bytes());       // crc
    b.extend_from_slice(&0i32.to_le_bytes());       // crc2
    b.extend_from_slice(&[0u8; 4]);                 // ext
    b.push(0);                                      // extra_bc
    b.extend_from_slice(&[0u8; 3]);                 // extras
    b.extend_from_slice(audio);
    b
}

/// Build a complete RIFF/WAVE file containing WavPack v3 blocks.
fn build_riff(
    num_channels:     u16,
    sample_rate:      u32,
    bytes_per_sample: u16,
    extra_chunks:     &[Vec<u8>],
    blocks:           &[Vec<u8>],  // each element is a wvpk block (from wvpk_v3_block)
) -> Vec<u8> {
    let fmt = fmt_chunk(num_channels, sample_rate, bytes_per_sample);

    let mut data_payload = Vec::new();
    for block in blocks {
        data_payload.extend_from_slice(block);
    }

    let mut data_chunk = Vec::new();
    data_chunk.extend_from_slice(b"data");
    data_chunk.extend_from_slice(&(data_payload.len() as u32).to_le_bytes());
    data_chunk.extend_from_slice(&data_payload);

    let mut wave = Vec::new();
    wave.extend_from_slice(&fmt);
    for ch in extra_chunks {
        wave.extend_from_slice(ch);
    }
    wave.extend_from_slice(&data_chunk);

    let mut riff = Vec::new();
    riff.extend_from_slice(b"RIFF");
    riff.extend_from_slice(&((wave.len() as u32) + 4).to_le_bytes());
    riff.extend_from_slice(b"WAVE");
    riff.extend_from_slice(&wave);
    riff
}

/// Build a RIFF with a simple dummy compressed block (not decodable; reader tests only).
fn build_riff_dummy(
    num_channels:     u16,
    sample_rate:      u32,
    bytes_per_sample: u16,
    extra_chunks:     &[Vec<u8>],
    n_frames:         i32,
    audio_bytes:      &[u8],
) -> Vec<u8> {
    let block = wvpk_v3_block(num_channels, bytes_per_sample, n_frames, audio_bytes);
    build_riff(num_channels, sample_rate, bytes_per_sample, extra_chunks, &[block])
}

/// Open a RIFF blob as a `WavPackReader`.
fn open(data: Vec<u8>) -> WavPackReader<'static> {
    let mss = MediaSourceStream::new(Box::new(Cursor::new(data)), Default::default());
    WavPackReader::try_new(mss, FormatOptions::default()).expect("try_new failed")
}

/// Extract all `WavPack/*` unsigned-int tags into a map.
fn collect_uint_tags(reader: &mut WavPackReader<'_>) -> std::collections::HashMap<String, u64> {
    let meta = reader.metadata();
    match meta.current() {
        Some(rev) => rev
            .media
            .tags
            .iter()
            .filter_map(|t| match t.raw.value {
                RawValue::UnsignedInt(v) => Some((t.raw.key.clone(), v)),
                _ => None,
            })
            .collect(),
        None => Default::default(),
    }
}

// ---------------------------------------------------------------------------
// Codec parameter tests
// ---------------------------------------------------------------------------

#[test]
fn stereo_16bit_44100_codec_params() {
    let data = build_riff_dummy(2, 44100, 2, &[], 100, &[0u8; 4]);
    let reader = open(data);

    let track = &reader.tracks()[0];
    let Some(CodecParameters::Audio(ap)) = &track.codec_params else {
        panic!("no audio codec params");
    };
    assert_eq!(ap.sample_rate, Some(44100));
    assert_eq!(ap.channels.as_ref().map(|c| c.count()), Some(2));
    assert_eq!(ap.bits_per_sample, Some(16));
}

#[test]
fn mono_8bit_22050_codec_params() {
    let data = build_riff_dummy(1, 22050, 1, &[], 100, &[0u8; 4]);
    let reader = open(data);

    let track = &reader.tracks()[0];
    let Some(CodecParameters::Audio(ap)) = &track.codec_params else {
        panic!("no audio codec params");
    };
    assert_eq!(ap.sample_rate, Some(22050));
    assert_eq!(ap.channels.as_ref().map(|c| c.count()), Some(1));
    assert_eq!(ap.bits_per_sample, Some(8));
}

#[test]
fn stereo_24bit_48000_codec_params() {
    let data = build_riff_dummy(2, 48000, 3, &[], 100, &[0u8; 4]);
    let reader = open(data);

    let track = &reader.tracks()[0];
    let Some(CodecParameters::Audio(ap)) = &track.codec_params else {
        panic!("no audio codec params");
    };
    assert_eq!(ap.sample_rate, Some(48000));
    assert_eq!(ap.channels.as_ref().map(|c| c.count()), Some(2));
    assert_eq!(ap.bits_per_sample, Some(24));
}

// ---------------------------------------------------------------------------
// Metadata / tag tests
// ---------------------------------------------------------------------------

#[test]
fn smpl_chunk_single_loop_tags() {
    let smpl = smpl_chunk(69, 0, &[(11025, 33075)]);
    let data = build_riff_dummy(2, 44100, 2, &[smpl], 10, &[0u8; 4]);
    let mut reader = open(data);

    let tags = collect_uint_tags(&mut reader);
    assert_eq!(tags.get("WavPack/MidiNote"),      Some(&69));
    assert_eq!(tags.get("WavPack/PitchFraction"), Some(&0));
    assert_eq!(tags.get("WavPack/Loop0/Start"),   Some(&11025));
    assert_eq!(tags.get("WavPack/Loop0/End"),     Some(&33075));
}

#[test]
fn smpl_chunk_nonzero_pitch_fraction() {
    let smpl = smpl_chunk(60, 0x8000_0000, &[(1000, 9000)]);
    let data = build_riff_dummy(1, 44100, 2, &[smpl], 10, &[0u8; 4]);
    let mut reader = open(data);

    let tags = collect_uint_tags(&mut reader);
    assert_eq!(tags.get("WavPack/MidiNote"),      Some(&60));
    assert_eq!(tags.get("WavPack/PitchFraction"), Some(&0x8000_0000));
}

#[test]
fn smpl_chunk_multiple_loops() {
    let smpl = smpl_chunk(69, 0, &[(1000, 5000), (6000, 9000)]);
    let data = build_riff_dummy(2, 44100, 2, &[smpl], 10, &[0u8; 4]);
    let mut reader = open(data);

    let tags = collect_uint_tags(&mut reader);
    assert_eq!(tags.get("WavPack/Loop0/Start"), Some(&1000));
    assert_eq!(tags.get("WavPack/Loop0/End"),   Some(&5000));
    assert_eq!(tags.get("WavPack/Loop1/Start"), Some(&6000));
    assert_eq!(tags.get("WavPack/Loop1/End"),   Some(&9000));
}

#[test]
fn cue_chunk_single_point_release() {
    let cue  = cue_chunk(&[40000]);
    let data = build_riff_dummy(2, 44100, 2, &[cue], 10, &[0u8; 4]);
    let mut reader = open(data);

    let tags = collect_uint_tags(&mut reader);
    assert_eq!(tags.get("WavPack/ReleasePoint"), Some(&40000));
}

#[test]
fn cue_chunk_multiple_points_max_is_release() {
    let cue  = cue_chunk(&[10000, 40000, 25000]);
    let data = build_riff_dummy(2, 44100, 2, &[cue], 10, &[0u8; 4]);
    let mut reader = open(data);

    let tags = collect_uint_tags(&mut reader);
    assert_eq!(tags.get("WavPack/ReleasePoint"), Some(&40000));
}

#[test]
fn no_smpl_chunk_no_mark_tags() {
    let data = build_riff_dummy(2, 44100, 2, &[], 10, &[0u8; 4]);
    let mut reader = open(data);

    let tags = collect_uint_tags(&mut reader);
    assert!(!tags.contains_key("WavPack/MidiNote"));
    assert!(!tags.contains_key("WavPack/ReleasePoint"));
}

#[test]
fn no_cue_chunk_no_release_point() {
    let smpl = smpl_chunk(69, 0, &[(1000, 5000)]);
    let data = build_riff_dummy(2, 44100, 2, &[smpl], 10, &[0u8; 4]);
    let mut reader = open(data);

    let tags = collect_uint_tags(&mut reader);
    assert!(!tags.contains_key("WavPack/ReleasePoint"));
}

#[test]
fn all_grandorgue_marks_present() {
    let smpl = smpl_chunk(69, 0, &[(11025, 33075)]);
    let cue  = cue_chunk(&[40000]);
    let data = build_riff_dummy(2, 44100, 2, &[smpl, cue], 10, &[0u8; 4]);
    let mut reader = open(data);

    let tags = collect_uint_tags(&mut reader);
    assert!(tags.contains_key("WavPack/MidiNote"));
    assert!(tags.contains_key("WavPack/PitchFraction"));
    assert!(tags.contains_key("WavPack/Loop0/Start"));
    assert!(tags.contains_key("WavPack/Loop0/End"));
    assert!(tags.contains_key("WavPack/ReleasePoint"));
}

// ---------------------------------------------------------------------------
// Packet layout tests (reader output format)
// ---------------------------------------------------------------------------

#[test]
fn next_packet_has_32_byte_prefix() {
    // The reader prepends a 32-byte structured prefix to the compressed audio.
    let audio = vec![0xA5u8; 16];
    let block = wvpk_v3_block(2, 2, 4, &audio);
    let data  = build_riff(2, 44100, 2, &[], &[block]);
    let mut reader = open(data);

    let pkt = reader.next_packet().expect("io error").expect("no packet");
    assert_eq!(pkt.data.len(), 32 + 16, "packet must be prefix(32) + audio bytes");

    // Verify prefix fields at known byte offsets:
    let d = pkt.data.as_ref();
    let version = i16::from_le_bytes([d[0], d[1]]);
    let flags   = i16::from_le_bytes([d[4], d[5]]);
    let n_ch    = u16::from_le_bytes([d[28], d[29]]);
    let bps     = u16::from_le_bytes([d[30], d[31]]);
    assert_eq!(version, 3);
    assert_eq!(flags & 1, 0);   // stereo (MONO_FLAG not set)
    assert_eq!(n_ch,  2);
    assert_eq!(bps,   2);

    // Compressed audio bytes are preserved verbatim after the prefix.
    assert_eq!(&d[32..], audio.as_slice());
}

#[test]
fn next_packet_end_of_stream_returns_none() {
    let block = wvpk_v3_block(2, 2, 10, &[0u8; 4]);
    let data  = build_riff(2, 44100, 2, &[], &[block]);
    let mut reader = open(data);

    let _ = reader.next_packet().unwrap(); // first block
    let second = reader.next_packet().unwrap();
    assert!(second.is_none(), "expected EOS after single block");
}

#[test]
fn multiple_blocks_correct_timestamps() {
    // Two blocks of 100 stereo 16-bit frames each.
    let b1 = wvpk_v3_block(2, 2, 100, &[0u8; 4]);
    let b2 = wvpk_v3_block(2, 2, 100, &[0u8; 4]);
    let data = build_riff(2, 44100, 2, &[], &[b1, b2]);
    let mut reader = open(data);

    let p1 = reader.next_packet().unwrap().unwrap();
    let p2 = reader.next_packet().unwrap().unwrap();

    assert_eq!(p1.pts.get(), 0);
    assert_eq!(p2.pts.get(), 100);
}

#[test]
fn multiple_blocks_drain_correctly() {
    let b = wvpk_v3_block(1, 2, 50, &[0u8; 4]);
    let data = build_riff(1, 22050, 2, &[], &[b.clone(), b.clone(), b.clone()]);
    let mut reader = open(data);

    let mut count = 0usize;
    while reader.next_packet().unwrap().is_some() {
        count += 1;
    }
    assert_eq!(count, 3);
}

#[test]
fn packet_duration_from_total_samples_field() {
    // 22 frames in total_samples → packet duration must be 22.
    let block = wvpk_v3_block(2, 2, 22, &[0u8; 8]);
    let data  = build_riff(2, 44100, 2, &[], &[block]);
    let mut reader = open(data);

    let pkt = reader.next_packet().unwrap().unwrap();
    assert_eq!(pkt.dur.get(), 22);
}

// ---------------------------------------------------------------------------
// Error / edge case tests
// ---------------------------------------------------------------------------

#[test]
fn missing_fmt_chunk_returns_error() {
    let payload = vec![0u8; 8];
    let mut wave = Vec::new();
    wave.extend_from_slice(b"data");
    wave.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    wave.extend_from_slice(&payload);

    let mut riff = Vec::new();
    riff.extend_from_slice(b"RIFF");
    riff.extend_from_slice(&((wave.len() as u32) + 4).to_le_bytes());
    riff.extend_from_slice(b"WAVE");
    riff.extend_from_slice(&wave);

    let mss = MediaSourceStream::new(Box::new(Cursor::new(riff)), Default::default());
    assert!(WavPackReader::try_new(mss, FormatOptions::default()).is_err());
}

#[test]
fn non_pcm_format_tag_returns_error() {
    let mut fmt = Vec::new();
    fmt.extend_from_slice(b"fmt ");
    fmt.extend_from_slice(&16u32.to_le_bytes());
    fmt.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    fmt.extend_from_slice(&2u16.to_le_bytes());
    fmt.extend_from_slice(&44100u32.to_le_bytes());
    fmt.extend_from_slice(&(44100u32 * 2 * 4).to_le_bytes());
    fmt.extend_from_slice(&8u16.to_le_bytes());
    fmt.extend_from_slice(&32u16.to_le_bytes());

    let block = wvpk_v3_block(2, 4, 1, &[0u8; 4]);
    let mut data_chunk = Vec::new();
    data_chunk.extend_from_slice(b"data");
    data_chunk.extend_from_slice(&(block.len() as u32).to_le_bytes());
    data_chunk.extend_from_slice(&block);

    let mut wave = Vec::new();
    wave.extend_from_slice(&fmt);
    wave.extend_from_slice(&data_chunk);

    let mut riff = Vec::new();
    riff.extend_from_slice(b"RIFF");
    riff.extend_from_slice(&((wave.len() as u32) + 4).to_le_bytes());
    riff.extend_from_slice(b"WAVE");
    riff.extend_from_slice(&wave);

    let mss = MediaSourceStream::new(Box::new(Cursor::new(riff)), Default::default());
    assert!(WavPackReader::try_new(mss, FormatOptions::default()).is_err());
}

#[test]
fn v4v5_stream_starts_with_wvpk_not_riff() {
    let mut block = Vec::new();
    block.extend_from_slice(b"wvpk");
    block.extend_from_slice(&32u32.to_le_bytes());
    block.extend_from_slice(&0x0410u16.to_le_bytes()); // version 4.16
    block.push(0);
    block.push(0);
    block.extend_from_slice(&0u32.to_le_bytes());
    block.extend_from_slice(&0u32.to_le_bytes());
    block.extend_from_slice(&2u32.to_le_bytes());
    let flags: u32 = 1 | (9 << 23);
    block.extend_from_slice(&flags.to_le_bytes());
    block.extend_from_slice(&0u32.to_le_bytes());
    block.extend_from_slice(&[0u8; 8]);

    let mss = MediaSourceStream::new(Box::new(Cursor::new(block)), Default::default());
    let result = WavPackReader::try_new(mss, FormatOptions::default());
    assert!(result.is_ok(), "v4/v5 stream detection should succeed: {:?}", result.err());
}

#[test]
fn unknown_chunks_are_skipped() {
    let mut junk = Vec::new();
    junk.extend_from_slice(b"JUNK");
    junk.extend_from_slice(&16u32.to_le_bytes());
    junk.extend_from_slice(&[0u8; 16]);

    let audio = vec![0xBBu8; 8];
    let block = wvpk_v3_block(2, 2, 4, &audio);
    let data  = build_riff(2, 44100, 2, &[junk], &[block]);
    let mut reader = open(data);

    let pkt = reader.next_packet().unwrap().unwrap();
    // Reader should skip JUNK and return the block; audio bytes are after prefix.
    assert_eq!(&pkt.data[32..], audio.as_slice());
}

// ---------------------------------------------------------------------------
// Decoder tests
// ---------------------------------------------------------------------------

/// Build a 32-byte packet prefix (matches the layout defined in decoder/mod.rs).
fn make_prefix(
    version:          i16,
    bits:             i16,
    flags:            i16,
    shift:            i16,
    total_samples:    i32,
    crc:              i32,
    num_channels:     u16,
    bytes_per_sample: u16,
) -> [u8; 32] {
    let mut p = [0u8; 32];
    p[0..2].copy_from_slice(&version.to_le_bytes());
    p[2..4].copy_from_slice(&bits.to_le_bytes());
    p[4..6].copy_from_slice(&flags.to_le_bytes());
    p[6..8].copy_from_slice(&shift.to_le_bytes());
    p[8..12].copy_from_slice(&total_samples.to_le_bytes());
    p[12..16].copy_from_slice(&crc.to_le_bytes());
    // crc2=0, ext=[0;4], extra_bc=0, extras=[0;3] — already zero
    p[28..30].copy_from_slice(&num_channels.to_le_bytes());
    p[30..32].copy_from_slice(&bytes_per_sample.to_le_bytes());
    p
}

/// Create a `WavPackDecoder` configured for the given parameters.
fn make_decoder(
    num_channels:     u16,
    sample_format:    SampleFormat,
) -> WavPackDecoder {
    let channels = if num_channels == 1 {
        CHANNEL_LAYOUT_MONO.clone()
    } else {
        CHANNEL_LAYOUT_STEREO.clone()
    };
    let mut params = AudioCodecParameters::new();
    params.for_codec(CODEC_ID_WAVPACK);
    params.with_channels(channels);
    params.with_sample_format(sample_format);
    params.with_sample_rate(44100);
    WavPackDecoder::try_new(&params, &AudioDecoderOptions::default())
        .expect("decoder construction failed")
}

#[test]
fn decoder_zero_samples_returns_empty() {
    let mut dec = make_decoder(1, SampleFormat::S32);
    let prefix = make_prefix(3, 0, 1 /*MONO*/, 0, 0, 0, 1, 2);
    let pkt = Packet::new(0, Timestamp::new(0), Duration::new(0), prefix.to_vec());

    let buf = dec.decode(&pkt).expect("decode failed");
    match buf {
        GenericAudioBufferRef::S32(b) => assert_eq!(b.frames(), 0, "expected empty buffer"),
        _ => panic!("expected S32 buffer"),
    }
}

/// Mono lossless silence, v3 FAST_FLAG (flag bits: MONO=1, FAST=2 → flags=3).
///
/// Bitstream derivation for N=4 zero samples via get_word3 with ave_dbits=0:
///   Each zero sample: bits [0,1] — while loop reads 0 (cbits=0), then
///   secondary check reads 1 (cbits_final=1), giving delta_dbits=-1, dbits=0.
///   4 samples × 2 bits = 8 bits = byte 0xAA (0b10101010, LSB-first).
#[test]
fn decoder_mono_fast_silence_4_samples() {
    const MONO_FLAG: i16 = 1;
    const FAST_FLAG: i16 = 2;

    let mut dec = make_decoder(1, SampleFormat::S32);
    let prefix = make_prefix(3, 0, MONO_FLAG | FAST_FLAG, 0, 4, 0, 1, 2);
    let mut data = prefix.to_vec();
    data.push(0xAA); // 4 zero samples: each [0,1] pair → 0b10101010

    let pkt = Packet::new(0, Timestamp::new(0), Duration::new(4), data);
    let buf = dec.decode(&pkt).expect("decode failed");

    match buf {
        GenericAudioBufferRef::S32(b) => {
            assert_eq!(b.frames(), 4, "expected 4 decoded frames");
            let plane = b.plane(0).expect("no plane 0");
            assert!(plane.iter().all(|&s| s == 0), "all samples must be zero");
        }
        _ => panic!("expected S32 buffer"),
    }
}

// ---------------------------------------------------------------------------
// Fixture-based tests using real WavPack 3.97 encoded files
// ---------------------------------------------------------------------------

/// Open a .wv fixture file and return a WavPackReader over it.
fn open_fixture(name: &str) -> WavPackReader<'static> {
    let path = format!("tests/fixtures/{}", name);
    let file = File::open(&path).unwrap_or_else(|e| panic!("cannot open {}: {}", path, e));
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    WavPackReader::try_new(mss, FormatOptions::default())
        .unwrap_or_else(|e| panic!("try_new failed for {}: {:?}", path, e))
}

/// Decode all packets from a WavPackReader using WavPackDecoder.
/// Returns a Vec<i32> of all samples from channel 0 (left/mono).
fn decode_all(reader: &mut WavPackReader<'_>) -> Vec<i32> {
    let track = &reader.tracks()[0];
    let Some(CodecParameters::Audio(ap)) = &track.codec_params else {
        panic!("no audio params");
    };
    let num_channels = ap.channels.as_ref().map(|c| c.count()).unwrap_or(1) as u16;
    let sample_format = ap.sample_format.unwrap_or(SampleFormat::S32);

    let mut dec = make_decoder(num_channels, sample_format);
    let mut all_samples: Vec<i32> = Vec::new();

    loop {
        let pkt = reader.next_packet().expect("io error");
        let Some(pkt) = pkt else { break };
        let buf = dec.decode(&pkt).expect("decode error");
        match buf {
            GenericAudioBufferRef::S32(b) => {
                let plane = b.plane(0).expect("missing plane 0");
                all_samples.extend_from_slice(plane);
            }
            GenericAudioBufferRef::S16(b) => {
                let plane = b.plane(0).expect("missing plane 0");
                all_samples.extend(plane.iter().map(|&s| s as i32));
            }
            _ => panic!("unexpected sample format {:?}", sample_format),
        }
    }
    all_samples
}

#[test]
fn fixture_silence_mono_fast_decodes_to_zeros() {
    let mut reader = open_fixture("test_silence_mono_fast.wv");
    let samples = decode_all(&mut reader);
    assert_eq!(samples.len(), 100, "expected 100 samples");
    assert!(samples.iter().all(|&s| s == 0), "all samples must be zero");
}

#[test]
fn fixture_silence_mono_high_decodes_to_zeros() {
    let mut reader = open_fixture("test_silence_mono_high.wv");
    let samples = decode_all(&mut reader);
    assert_eq!(samples.len(), 100, "expected 100 samples");
    assert!(samples.iter().all(|&s| s == 0), "all samples must be zero");
}

#[test]
fn fixture_silence_stereo_fast_decodes_to_zeros() {
    let mut reader = open_fixture("test_silence_stereo_fast.wv");
    let samples = decode_all(&mut reader);
    assert_eq!(samples.len(), 100, "expected 100 samples (left channel only)");
    assert!(samples.iter().all(|&s| s == 0), "all samples must be zero");
}

#[test]
fn fixture_ramp_mono_fast_decodes_correctly() {
    let mut reader = open_fixture("test_ramp_mono_fast.wv");
    let samples = decode_all(&mut reader);
    assert_eq!(samples.len(), 100, "expected 100 samples");
    for (i, &s) in samples.iter().enumerate() {
        assert_eq!(s, (i as i32) * 100, "sample[{}]: expected {}, got {}", i, i * 100, s);
    }
}

#[test]
fn fixture_ramp_mono_high_decodes_correctly() {
    let mut reader = open_fixture("test_ramp_mono_high.wv");
    let samples = decode_all(&mut reader);
    assert_eq!(samples.len(), 100, "expected 100 samples");
    for (i, &s) in samples.iter().enumerate() {
        assert_eq!(s, (i as i32) * 100, "sample[{}]: expected {}, got {}", i, i * 100, s);
    }
}

/// Mono silence decoded through the reader→decoder pipeline.
/// Uses a real RIFF/WAVE file with a FAST-mode mono block, decoding via
/// WavPackDecoder fed the packet produced by WavPackReader.
#[test]
fn reader_decoder_pipeline_mono_silence() {
    const MONO_FLAG: i16 = 1;
    const FAST_FLAG: i16 = 2;
    const N: i32 = 4;

    // Build a RIFF file with the same 5-bit silence block used above.
    // wvpk_v3_block uses flags=MONO_FLAG only; we need MONO|FAST, so build manually.
    let prefix_block = {
        let audio = [0xAAu8]; // 4 zero samples, 2 bits each = 0b10101010
        let ck_size: i32 = 28 + audio.len() as i32;
        let mut b = Vec::new();
        b.extend_from_slice(b"wvpk");
        b.extend_from_slice(&ck_size.to_le_bytes());
        b.extend_from_slice(&3i16.to_le_bytes());               // version
        b.extend_from_slice(&0i16.to_le_bytes());               // bits=0 (lossless)
        b.extend_from_slice(&(MONO_FLAG | FAST_FLAG).to_le_bytes());
        b.extend_from_slice(&0i16.to_le_bytes());               // shift
        b.extend_from_slice(&N.to_le_bytes());                  // total_samples
        b.extend_from_slice(&0i32.to_le_bytes());               // crc
        b.extend_from_slice(&0i32.to_le_bytes());               // crc2
        b.extend_from_slice(&[0u8; 4]);                         // ext
        b.push(0);                                              // extra_bc
        b.extend_from_slice(&[0u8; 3]);                         // extras
        b.extend_from_slice(&audio);
        b
    };

    let riff = build_riff(1, 44100, 2, &[], &[prefix_block]);
    let mut reader = open(riff);

    let pkt = reader.next_packet().unwrap().expect("expected a packet");
    assert_eq!(pkt.dur.get(), N as u64);

    // Now decode with WavPackDecoder.
    let mut dec = make_decoder(1, SampleFormat::S32);
    let buf = dec.decode(&pkt).expect("decode failed");

    match buf {
        GenericAudioBufferRef::S32(b) => {
            assert_eq!(b.frames(), N as usize);
            assert!(b.plane(0).expect("no plane 0").iter().all(|&s| s == 0));
        }
        _ => panic!("expected S32 buffer"),
    }
}
