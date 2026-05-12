// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Integration tests for the WavPack v1–v3 RIFF/WAVE format reader.
//!
//! Tests construct minimal in-memory RIFF/WAVE files containing WavPack v3
//! blocks and verify that the reader correctly parses codec parameters,
//! sampler metadata tags, and PCM packet payloads.

use std::io::Cursor;

use symphonia_codec_wavpack::WavPackReader;
use symphonia_core::codecs::CodecParameters;
use symphonia_core::formats::{FormatOptions, FormatReader};
use symphonia_core::io::MediaSourceStream;
use symphonia_core::meta::RawValue;

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
    // GO_WAVESAMPLERCHUNK (9 × u32)
    c.extend_from_slice(&0u32.to_le_bytes()); // manufacturer
    c.extend_from_slice(&0u32.to_le_bytes()); // product
    c.extend_from_slice(&22675u32.to_le_bytes()); // sample_period ≈ 1/44100 ns
    c.extend_from_slice(&midi_note.to_le_bytes());
    c.extend_from_slice(&pitch_fraction.to_le_bytes());
    c.extend_from_slice(&0u32.to_le_bytes()); // smpte_format
    c.extend_from_slice(&0u32.to_le_bytes()); // smpte_offset
    c.extend_from_slice(&loop_count.to_le_bytes());
    c.extend_from_slice(&0u32.to_le_bytes()); // sampler_data
    // GO_WAVESAMPLERLOOP (6 × u32 each)
    for (i, (start, end)) in loops.iter().enumerate() {
        c.extend_from_slice(&(i as u32).to_le_bytes()); // id
        c.extend_from_slice(&0u32.to_le_bytes());        // loop_type
        c.extend_from_slice(&start.to_le_bytes());
        c.extend_from_slice(&end.to_le_bytes());
        c.extend_from_slice(&0u32.to_le_bytes()); // fraction
        c.extend_from_slice(&0u32.to_le_bytes()); // play_count
    }
    c
}

/// Build a `cue ` chunk (GrandOrgue layout).
///
/// Each entry has `dwSampleOffset == offset`; `fccChunk` is "data" as BE u32.
fn cue_chunk(offsets: &[u32]) -> Vec<u8> {
    let count     = offsets.len() as u32;
    let data_size = 4 + count * 24;
    let data_fcc  = u32::from_be_bytes(*b"data");

    let mut c = Vec::new();
    c.extend_from_slice(b"cue ");
    c.extend_from_slice(&data_size.to_le_bytes());
    c.extend_from_slice(&count.to_le_bytes());
    for (i, &off) in offsets.iter().enumerate() {
        c.extend_from_slice(&(i as u32).to_le_bytes()); // dwName
        c.extend_from_slice(&off.to_le_bytes());         // dwPosition
        c.extend_from_slice(&data_fcc.to_le_bytes());   // fccChunk = "data"
        c.extend_from_slice(&0u32.to_le_bytes());        // dwChunkStart
        c.extend_from_slice(&0u32.to_le_bytes());        // dwBlockStart
        c.extend_from_slice(&off.to_le_bytes());         // dwSampleOffset
    }
    c
}

/// Build a single WavPack v3 `wvpk` block embedding raw PCM bytes.
fn wvpk_v3_block(num_channels: u16, bytes_per_sample: u16, pcm: &[u8]) -> Vec<u8> {
    // v3 header overhead after ck_size: version(2)+bps(2)+flags(2)+shift(2)+
    //   total_samples(4)+crc(4)+crc2(4)+ext(4)+extra_bc(1)+extras(3) = 28 bytes
    const OVERHEAD: i32 = 28;
    let ck_size    = OVERHEAD + pcm.len() as i32;
    let bps        = (bytes_per_sample * 8) as i16;
    // flags: bit 0 = mono flag (1 if mono, 0 if stereo)
    let flags: i16 = if num_channels == 1 { 1 } else { 0 };
    let n_frames   = (pcm.len() as i32) / (num_channels as i32 * bytes_per_sample as i32);

    let mut b = Vec::new();
    b.extend_from_slice(b"wvpk");
    b.extend_from_slice(&ck_size.to_le_bytes());
    b.extend_from_slice(&3i16.to_le_bytes());   // version
    b.extend_from_slice(&bps.to_le_bytes());
    b.extend_from_slice(&flags.to_le_bytes());
    b.extend_from_slice(&0i16.to_le_bytes());   // shift
    b.extend_from_slice(&n_frames.to_le_bytes()); // total_samples
    b.extend_from_slice(&0i32.to_le_bytes());   // crc
    b.extend_from_slice(&0i32.to_le_bytes());   // crc2
    b.extend_from_slice(&[0u8; 4]);             // ext
    b.push(0);                                  // extra_bc
    b.extend_from_slice(&[0u8; 3]);             // extras
    b.extend_from_slice(pcm);
    b
}

/// Build a complete RIFF/WAVE file with WavPack v3 data blocks.
fn build_riff(
    num_channels:    u16,
    sample_rate:     u32,
    bytes_per_sample: u16,
    extra_chunks:    &[Vec<u8>],
    pcm_blocks:      &[Vec<u8>],
) -> Vec<u8> {
    let fmt = fmt_chunk(num_channels, sample_rate, bytes_per_sample);

    let mut data_payload = Vec::new();
    for pcm in pcm_blocks {
        data_payload.extend_from_slice(&wvpk_v3_block(num_channels, bytes_per_sample, pcm));
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
    // RIFF size = "WAVE" (4) + rest
    riff.extend_from_slice(&((wave.len() as u32) + 4).to_le_bytes());
    riff.extend_from_slice(b"WAVE");
    riff.extend_from_slice(&wave);
    riff
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
    let pcm = vec![0u8; 44100 * 2 * 2]; // 1 s stereo 16-bit
    let data = build_riff(2, 44100, 2, &[], &[pcm]);
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
    let pcm = vec![0u8; 22050]; // 1 s mono 8-bit
    let data = build_riff(1, 22050, 1, &[], &[pcm]);
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
    let pcm = vec![0u8; 48000 * 2 * 3]; // 1 s stereo 24-bit
    let data = build_riff(2, 48000, 3, &[], &[pcm]);
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
    let pcm  = vec![0u8; 100];
    let data = build_riff(2, 44100, 2, &[smpl], &[pcm]);
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
    let pcm  = vec![0u8; 100];
    let data = build_riff(1, 44100, 2, &[smpl], &[pcm]);
    let mut reader = open(data);

    let tags = collect_uint_tags(&mut reader);
    assert_eq!(tags.get("WavPack/MidiNote"),      Some(&60));
    assert_eq!(tags.get("WavPack/PitchFraction"), Some(&0x8000_0000));
}

#[test]
fn smpl_chunk_multiple_loops() {
    let smpl = smpl_chunk(69, 0, &[(1000, 5000), (6000, 9000)]);
    let pcm  = vec![0u8; 100];
    let data = build_riff(2, 44100, 2, &[smpl], &[pcm]);
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
    let pcm  = vec![0u8; 100];
    let data = build_riff(2, 44100, 2, &[cue], &[pcm]);
    let mut reader = open(data);

    let tags = collect_uint_tags(&mut reader);
    assert_eq!(tags.get("WavPack/ReleasePoint"), Some(&40000));
}

#[test]
fn cue_chunk_multiple_points_max_is_release() {
    // GrandOrgue uses the highest dwSampleOffset as the release point.
    let cue  = cue_chunk(&[10000, 40000, 25000]);
    let pcm  = vec![0u8; 100];
    let data = build_riff(2, 44100, 2, &[cue], &[pcm]);
    let mut reader = open(data);

    let tags = collect_uint_tags(&mut reader);
    assert_eq!(tags.get("WavPack/ReleasePoint"), Some(&40000));
}

#[test]
fn no_smpl_chunk_no_mark_tags() {
    let pcm  = vec![0u8; 100];
    let data = build_riff(2, 44100, 2, &[], &[pcm]);
    let mut reader = open(data);

    let tags = collect_uint_tags(&mut reader);
    assert!(!tags.contains_key("WavPack/MidiNote"));
    assert!(!tags.contains_key("WavPack/ReleasePoint"));
}

#[test]
fn no_cue_chunk_no_release_point() {
    let smpl = smpl_chunk(69, 0, &[(1000, 5000)]);
    let pcm  = vec![0u8; 100];
    let data = build_riff(2, 44100, 2, &[smpl], &[pcm]);
    let mut reader = open(data);

    let tags = collect_uint_tags(&mut reader);
    assert!(!tags.contains_key("WavPack/ReleasePoint"));
}

#[test]
fn all_grandorgue_marks_present() {
    let smpl = smpl_chunk(69, 0, &[(11025, 33075)]);
    let cue  = cue_chunk(&[40000]);
    let pcm  = vec![0u8; 100];
    let data = build_riff(2, 44100, 2, &[smpl, cue], &[pcm]);
    let mut reader = open(data);

    let tags = collect_uint_tags(&mut reader);
    assert!(tags.contains_key("WavPack/MidiNote"));
    assert!(tags.contains_key("WavPack/PitchFraction"));
    assert!(tags.contains_key("WavPack/Loop0/Start"));
    assert!(tags.contains_key("WavPack/Loop0/End"));
    assert!(tags.contains_key("WavPack/ReleasePoint"));
}

// ---------------------------------------------------------------------------
// Packet / PCM tests
// ---------------------------------------------------------------------------

#[test]
fn next_packet_returns_pcm_bytes() {
    let pcm  = vec![0xA5u8; 200];
    let data = build_riff(2, 44100, 2, &[], &[pcm.clone()]);
    let mut reader = open(data);

    let pkt = reader.next_packet().expect("io error").expect("no packet");
    assert_eq!(pkt.data.as_ref(), pcm.as_slice());
}

#[test]
fn next_packet_end_of_stream_returns_none() {
    let pcm  = vec![0u8; 40];
    let data = build_riff(2, 44100, 2, &[], &[pcm]);
    let mut reader = open(data);

    let _ = reader.next_packet().unwrap(); // first block
    let second = reader.next_packet().unwrap();
    assert!(second.is_none(), "expected EOS");
}

#[test]
fn multiple_blocks_correct_timestamps() {
    let pcm1 = vec![0u8; 400]; // 100 stereo 16-bit frames
    let pcm2 = vec![0u8; 400];
    let data = build_riff(2, 44100, 2, &[], &[pcm1, pcm2]);
    let mut reader = open(data);

    let p1 = reader.next_packet().unwrap().unwrap();
    let p2 = reader.next_packet().unwrap().unwrap();

    assert_eq!(p1.pts.get(), 0);
    assert_eq!(p2.pts.get(), 100); // 100 frames after first block
}

#[test]
fn multiple_blocks_drain_correctly() {
    let block = vec![0u8; 200];
    let data  = build_riff(1, 22050, 2, &[], &[block.clone(), block.clone(), block.clone()]);
    let mut reader = open(data);

    let mut count = 0usize;
    while reader.next_packet().unwrap().is_some() {
        count += 1;
    }
    assert_eq!(count, 3);
}

#[test]
fn n_samples_computed_from_fmt_chunk() {
    // 88 bytes / (2 ch * 2 bps) = 22 frames
    let pcm  = vec![0u8; 88];
    let data = build_riff(2, 44100, 2, &[], &[pcm]);
    let mut reader = open(data);

    let pkt = reader.next_packet().unwrap().unwrap();
    assert_eq!(pkt.dur.get(), 22);
}

// ---------------------------------------------------------------------------
// Error / edge case tests
// ---------------------------------------------------------------------------

#[test]
fn missing_fmt_chunk_returns_error() {
    // RIFF/WAVE with only a data chunk — no fmt
    let payload = vec![0u8; 8]; // minimal wvpk placeholder
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
    let result = WavPackReader::try_new(mss, FormatOptions::default());
    assert!(result.is_err(), "expected error for missing fmt chunk");
}

#[test]
fn non_pcm_format_tag_returns_error() {
    // fmt chunk with format_tag = 3 (IEEE float)
    let mut fmt = Vec::new();
    fmt.extend_from_slice(b"fmt ");
    fmt.extend_from_slice(&16u32.to_le_bytes());
    fmt.extend_from_slice(&3u16.to_le_bytes()); // IEEE float — not PCM
    fmt.extend_from_slice(&2u16.to_le_bytes());
    fmt.extend_from_slice(&44100u32.to_le_bytes());
    fmt.extend_from_slice(&(44100u32 * 2 * 4).to_le_bytes());
    fmt.extend_from_slice(&8u16.to_le_bytes());
    fmt.extend_from_slice(&32u16.to_le_bytes());

    let payload = wvpk_v3_block(2, 4, &[0u8; 16]);
    let mut data_chunk = Vec::new();
    data_chunk.extend_from_slice(b"data");
    data_chunk.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    data_chunk.extend_from_slice(&payload);

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
    // A minimal v4/v5 wvpk stream — just needs to not panic/crash on try_new.
    // We don't test full decoding here since v4/v5 decoding is out of scope.
    let mut block = Vec::new();
    block.extend_from_slice(b"wvpk");
    // ck_size, version (0x0410 = v4/v5), block_index_u8, total_samples_u8
    block.extend_from_slice(&32u32.to_le_bytes());  // ck_size
    block.extend_from_slice(&0x0410u16.to_le_bytes()); // version = 4.16
    block.push(0); // block_index_u8
    block.push(0); // total_samples_u8
    block.extend_from_slice(&0u32.to_le_bytes());   // total_samples_u32
    block.extend_from_slice(&0u32.to_le_bytes());   // block_index_u32
    block.extend_from_slice(&2u32.to_le_bytes());   // block_samples (stereo: 2 frames → n_samples=1)
    // flags: stereo (bit2=0), 16-bit (bits 0-1 = 1 → bytes_per_sample=2)
    // sample rate index 9 → 44100 Hz (bits 23-26 = 0b01001)
    let flags: u32 = 1 | (9 << 23);
    block.extend_from_slice(&flags.to_le_bytes());
    block.extend_from_slice(&0u32.to_le_bytes());   // crc
    block.extend_from_slice(&[0u8; 8]);             // padding to fill ck_size

    let mss = MediaSourceStream::new(Box::new(Cursor::new(block)), Default::default());
    // try_new must succeed (it returns Ok even if it can't fully decode later)
    let result = WavPackReader::try_new(mss, FormatOptions::default());
    assert!(result.is_ok(), "v4/v5 stream detection should succeed: {:?}", result.err());
}

// ---------------------------------------------------------------------------
// Unknown chunk interleaving
// ---------------------------------------------------------------------------

#[test]
fn unknown_chunks_are_skipped() {
    // Insert an unknown "JUNK" chunk before the data; reader should skip it.
    let mut junk = Vec::new();
    junk.extend_from_slice(b"JUNK");
    junk.extend_from_slice(&16u32.to_le_bytes());
    junk.extend_from_slice(&[0u8; 16]);

    let pcm  = vec![1u8; 40];
    let data = build_riff(2, 44100, 2, &[junk], &[pcm.clone()]);
    let mut reader = open(data);

    let pkt = reader.next_packet().unwrap().unwrap();
    assert_eq!(pkt.data.as_ref(), pcm.as_slice());
}
