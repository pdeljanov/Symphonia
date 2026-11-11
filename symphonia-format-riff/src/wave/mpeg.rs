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

use std::io::{ErrorKind, Seek, SeekFrom};

use symphonia_core::codecs::audio::AudioCodecId;
use symphonia_core::codecs::audio::well_known::{CODEC_ID_MP1, CODEC_ID_MP2, CODEC_ID_MP3};
use symphonia_core::errors::{Result, SeekErrorKind, seek_error};
use symphonia_core::io::{MediaSource, MediaSourceStream, ReadBytes, SeekBuffered};
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
    /// Channel count, used when confirming consecutive headers.
    pub n_channels: u8,
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

    let n_channels = if word & 0xc0 == 0xc0 { 1 } else { 2 };
    Some((MpegFrameFormat { codec, sample_rate, samples_per_frame, n_channels }, frame_len))
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
            Err(err) if err.kind() == ErrorKind::UnexpectedEof => return Ok(None),
            Err(err) => return Err(err.into()),
        };

        sync = (sync << 8) | u32::from(byte);
        have = (have + 1).min(4);
        if have < 4 {
            continue;
        }

        let Some((format, frame_len)) = parse_header(sync)
        else {
            continue;
        };
        let start_pos = reader.pos() - 4;
        let mut frame = vec![0u8; frame_len];
        frame[..4].copy_from_slice(&sync.to_be_bytes());

        let complete = if frame_len as u64 - 4 > end_pos - reader.pos() {
            false
        }
        else {
            match reader.read_buf_exact(&mut frame[4..]) {
                Ok(()) => true,
                Err(err) if err.kind() == ErrorKind::UnexpectedEof => false,
                Err(err) => return Err(err.into()),
            }
        };

        if complete {
            let frame_end_pos = reader.pos();
            // Confirm another compatible header at the computed frame boundary. Bitrate and
            // padding may vary. A complete final frame needs no following header.
            let confirmed = if end_pos - frame_end_pos < 4 {
                true
            }
            else {
                match reader.read_be_u32() {
                    Ok(word) => parse_header(word).is_some_and(|(next, _)| {
                        next.codec == format.codec
                            && next.sample_rate == format.sample_rate
                            && next.samples_per_frame == format.samples_per_frame
                            && next.n_channels == format.n_channels
                    }),
                    Err(err) if err.kind() == ErrorKind::UnexpectedEof => true,
                    Err(err) => return Err(err.into()),
                }
            };
            if confirmed {
                rewind(reader, frame_end_pos)?;
                return Ok(Some((format, frame)));
            }
        }

        // The candidate may contain the actual sync word, so resume one byte after its start.
        // Only one MPEG frame plus a header was read, well within the minimum lookback capacity.
        rewind(reader, start_pos + 1)?;
        sync = 0;
        have = 0;
    }
}

fn rewind(reader: &mut MediaSourceStream<'_>, pos: u64) -> Result<()> {
    if reader.seek_buffered(pos) != pos {
        return seek_error(SeekErrorKind::Unseekable);
    }
    Ok(())
}

/// Seek by counting frame samples rather than estimating byte offsets. Coarse seeks use the same
/// scan as accurate seeks. Up to four reference frames are retained for the Layer III bit reservoir,
/// matching the elementary MPEG audio demuxer.
pub fn seek(
    reader: &mut MediaSourceStream<'_>,
    end_pos: u64,
    first_frame_pos: u64,
    next_ts: &mut u64,
    required_ts: u64,
) -> Result<u64> {
    if reader.is_seekable() {
        if reader.seek(SeekFrom::Start(first_frame_pos))? != first_frame_pos {
            return seek_error(SeekErrorKind::Unseekable);
        }
        *next_ts = 0;
    }
    else if required_ts < *next_ts {
        return seek_error(SeekErrorKind::ForwardOnly);
    }

    let mut positions = [(0u64, 0u64); 4];
    let mut n_parsed = 0usize;
    loop {
        let Some((format, frame)) = read_frame(reader, end_pos)?
        else {
            return seek_error(SeekErrorKind::OutOfRange);
        };
        let pos = reader.pos() - frame.len() as u64;
        positions[n_parsed % positions.len()] = (pos, *next_ts);
        n_parsed += 1;
        let frame_end_ts = next_ts.saturating_add(format.samples_per_frame);
        if required_ts < frame_end_ts {
            let mut reference = (pos, *next_ts);
            if format.codec == CODEC_ID_MP3 {
                let side_info = 4 + if frame[1] & 1 == 0 { 2 } else { 0 };
                let main_data_begin = if format.samples_per_frame == 1152 {
                    u64::from(u16::from_be_bytes([frame[side_info], frame[side_info + 1]]) >> 7)
                }
                else {
                    u64::from(frame[side_info])
                };
                for back in 1..n_parsed.min(positions.len()) {
                    if pos - reference.0 >= main_data_begin {
                        break;
                    }
                    reference = positions[(n_parsed - back - 1) % positions.len()];
                }
            }
            rewind(reader, reference.0)?;
            *next_ts = reference.1;
            return Ok(*next_ts);
        }
        *next_ts = frame_end_ts;
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
