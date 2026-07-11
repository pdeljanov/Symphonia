// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Minimal MPEG-1/2/2.5 Audio (Layer I/II/III) frame framing, just enough to packetize the
//! `data` chunk of an MPEG-audio-in-WAV file (`WAVE_FORMAT_MPEGLAYER3`, tag `0x0055`) one frame at
//! a time.
//!
//! The `data` chunk of such a file is a plain MPEG audio elementary stream. MPEG audio frames are
//! self-describing and variable-length, so — unlike the block-aligned PCM/ADPCM formats — they
//! cannot be packetized with `PacketInfo`; each frame's length is computed from its own header.
//!
//! The bit-rate tables and frame-size arithmetic mirror `symphonia-bundle-mp3`'s `header.rs`. They
//! are duplicated here rather than shared because a format crate must not depend on a codec bundle;
//! only the small amount of header parsing needed to find frame boundaries is reproduced.

use symphonia_core::codecs::audio::AudioCodecId;
use symphonia_core::codecs::audio::well_known::{CODEC_ID_MP1, CODEC_ID_MP2, CODEC_ID_MP3};
use symphonia_core::errors::Result;
use symphonia_core::io::{MediaSourceStream, ReadBytes};
use symphonia_core::packet::Packet;
use symphonia_core::units::{Duration, Timestamp};

// Bit-rate lookup tables (bits/sec), indexed by the 4-bit bit-rate field.
const BIT_RATES_MPEG1_L1: [u32; 15] = [
    0, 32_000, 64_000, 96_000, 128_000, 160_000, 192_000, 224_000, 256_000, 288_000, 320_000,
    352_000, 384_000, 416_000, 448_000,
];
const BIT_RATES_MPEG1_L2: [u32; 15] = [
    0, 32_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000, 128_000, 160_000, 192_000, 224_000,
    256_000, 320_000, 384_000,
];
const BIT_RATES_MPEG1_L3: [u32; 15] = [
    0, 32_000, 40_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000, 128_000, 160_000, 192_000,
    224_000, 256_000, 320_000,
];
const BIT_RATES_MPEG2_L1: [u32; 15] = [
    0, 32_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000, 128_000, 144_000, 160_000, 176_000,
    192_000, 224_000, 256_000,
];
const BIT_RATES_MPEG2_L23: [u32; 15] = [
    0, 8_000, 16_000, 24_000, 32_000, 40_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000,
    128_000, 144_000, 160_000,
];

/// The audio parameters described by an MPEG audio frame header.
pub struct MpegFrameFormat {
    /// Codec (`CODEC_ID_MP1`, `CODEC_ID_MP2`, or `CODEC_ID_MP3`).
    pub codec: AudioCodecId,
    /// Sampling rate in Hz.
    pub sample_rate: u32,
    /// Number of PCM samples the frame decodes to.
    pub samples_per_frame: u64,
}

/// Parses a 32-bit MPEG audio frame header word into its audio parameters and the total frame
/// length in bytes (header + body). Returns `None` if the word is not a valid, supported frame
/// header, which is also how the caller detects a false sync.
fn parse_header(word: u32) -> Option<(MpegFrameFormat, usize)> {
    // The header begins with an 11-bit sync word (all ones).
    if word & 0xffe0_0000 != 0xffe0_0000 {
        return None;
    }

    let version = (word >> 19) & 0x3; // 0b00 = 2.5, 0b10 = 2, 0b11 = 1 (0b01 reserved)
    let layer = (word >> 17) & 0x3; // 0b01 = III, 0b10 = II, 0b11 = I (0b00 reserved)
    let bitrate_idx = ((word >> 12) & 0xf) as usize;
    let sr_idx = (word >> 10) & 0x3;
    let padding = (word >> 9) & 0x1;

    // Reject reserved or unsupported fields. Free-format (bit-rate index 0) is not supported.
    if version == 0b01 || layer == 0b00 || bitrate_idx == 0 || bitrate_idx == 0xf || sr_idx == 0b11
    {
        return None;
    }

    let is_mpeg1 = version == 0b11;

    let bitrate = match (is_mpeg1, layer) {
        (true, 0b11) => BIT_RATES_MPEG1_L1[bitrate_idx],
        (true, 0b10) => BIT_RATES_MPEG1_L2[bitrate_idx],
        (true, 0b01) => BIT_RATES_MPEG1_L3[bitrate_idx],
        (false, 0b11) => BIT_RATES_MPEG2_L1[bitrate_idx],
        (false, _) => BIT_RATES_MPEG2_L23[bitrate_idx],
        _ => return None,
    };

    let sample_rate = match (version, sr_idx) {
        (0b11, 0b00) => 44_100,
        (0b11, 0b01) => 48_000,
        (0b11, 0b10) => 32_000,
        (0b10, 0b00) => 22_050,
        (0b10, 0b01) => 24_000,
        (0b10, 0b10) => 16_000,
        (0b00, 0b00) => 11_025,
        (0b00, 0b01) => 12_000,
        (0b00, 0b10) => 8_000,
        _ => return None,
    };

    // Codec, samples-per-frame, and the frame-size factor/slot-size per ISO-11172 §2.4.3.1.
    let (codec, samples_per_frame, factor, slot_size) = match layer {
        0b11 => (CODEC_ID_MP1, 384, 12, 4),            // Layer I
        0b10 => (CODEC_ID_MP2, 1152, 144, 1),          // Layer II
        _ if is_mpeg1 => (CODEC_ID_MP3, 1152, 144, 1), // Layer III, MPEG 1
        _ => (CODEC_ID_MP3, 576, 72, 1),               // Layer III, MPEG 2 / 2.5
    };

    let frame_len = ((factor * bitrate / sample_rate + padding) * slot_size) as usize;

    // A frame must be at least large enough to hold its own header.
    if frame_len <= 4 {
        return None;
    }

    Some((MpegFrameFormat { codec, sample_rate, samples_per_frame }, frame_len))
}

/// Synchronises to and reads the next whole MPEG audio frame (header and body) from the `data`
/// chunk, stopping at `end_pos`. Returns the frame's audio parameters and its bytes, or `None` at
/// the end of the data chunk.
pub fn read_frame(
    reader: &mut MediaSourceStream<'_>,
    end_pos: u64,
) -> Result<Option<(MpegFrameFormat, Vec<u8>)>> {
    let mut sync = 0u32;
    let mut have = 0u32;

    loop {
        if reader.pos() >= end_pos {
            return Ok(None);
        }

        let byte = match reader.read_u8() {
            Ok(byte) => byte,
            // The data chunk ended part way through a sync search; treat as end of stream.
            Err(_) => return Ok(None),
        };

        sync = (sync << 8) | u32::from(byte);
        have += 1;

        // A sync word is only meaningful once four bytes have been shifted in.
        if have < 4 {
            continue;
        }

        if let Some((format, frame_len)) = parse_header(sync) {
            // The final frame may be truncated by the end of the data chunk (or file). If the whole
            // body is not available, stop cleanly rather than reading past the audio or failing on
            // a partial frame — a mid-stream demux failure would otherwise discard the whole hash.
            if reader.pos().saturating_add(frame_len as u64 - 4) > end_pos {
                return Ok(None);
            }
            let mut frame = vec![0u8; frame_len];
            frame[0..4].copy_from_slice(&sync.to_be_bytes());
            if reader.read_buf_exact(&mut frame[4..]).is_err() {
                return Ok(None);
            }
            return Ok(Some((format, frame)));
        }
    }
}

/// Reads the next MPEG audio frame from the `data` chunk and wraps it in a [`Packet`], advancing
/// `next_ts` by the frame's sample count. Returns `None` at the end of the data chunk.
pub fn next_packet(
    reader: &mut MediaSourceStream<'_>,
    end_pos: u64,
    next_ts: &mut u64,
) -> Result<Option<Packet>> {
    let Some((format, frame)) = read_frame(reader, end_pos)?
    else {
        return Ok(None);
    };

    let pts = match Timestamp::try_from(*next_ts) {
        Ok(pts) => pts,
        Err(_) => return Ok(None),
    };
    let dur = Duration::from(format.samples_per_frame);
    *next_ts = next_ts.saturating_add(format.samples_per_frame);

    Ok(Some(Packet::new(0, pts, dur, frame)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mpeg1_layer3_header() {
        // MPEG-1 Layer III, 128 kbps, 44.1 kHz, stereo, no padding.
        let (fmt, len) = parse_header(0xFFFB_9000).unwrap();
        assert_eq!(fmt.codec, CODEC_ID_MP3);
        assert_eq!(fmt.sample_rate, 44_100);
        assert_eq!(fmt.samples_per_frame, 1152);
        assert_eq!(len, 417); // 144 * 128000 / 44100

        // The padding bit adds one slot (one byte for Layer III).
        let (_, padded_len) = parse_header(0xFFFB_9200).unwrap();
        assert_eq!(padded_len, 418);
    }

    #[test]
    fn parses_mpeg2_layer3_header() {
        // MPEG-2 Layer III, 64 kbps, 22.05 kHz — half the samples per frame.
        let (fmt, len) = parse_header(0xFFF3_8000).unwrap();
        assert_eq!(fmt.codec, CODEC_ID_MP3);
        assert_eq!(fmt.sample_rate, 22_050);
        assert_eq!(fmt.samples_per_frame, 576);
        assert_eq!(len, 208); // 72 * 64000 / 22050
    }

    #[test]
    fn rejects_invalid_headers() {
        assert!(parse_header(0x0000_0000).is_none()); // no sync word
        assert!(parse_header(0xFFFF_FFFF).is_none()); // reserved bit-rate (0xf)
        assert!(parse_header(0xFFE0_0000).is_none()); // free-format bit-rate (0)
    }
}
