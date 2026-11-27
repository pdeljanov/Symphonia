// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Just enough of the frame headers of the MPEG audio, AC-3, and DTS elementary streams to find the
//! boundaries of their frames and the audio parameters, for the containers that carry the streams
//! unframed (a format crate must not depend on a codec).

use symphonia_core::audio::{Channels, Position, layouts};
use symphonia_core::codecs::audio::AudioCodecId;
use symphonia_core::codecs::audio::well_known::{
    CODEC_ID_AC3, CODEC_ID_DCA, CODEC_ID_MP1, CODEC_ID_MP2, CODEC_ID_MP3,
};

pub const AC3_SYNC: u16 = 0x0b77;
pub const DTS_SYNC_16_BE: u32 = 0x7ffe_8001;
pub const DTS_SYNC_16_LE: u32 = 0xfe7f_0180;
pub const DTS_SYNC_14_BE: u32 = 0x1fff_e800;
pub const DTS_SYNC_14_LE: u32 = 0xff1f_00e8;

/// The frame sizes of AC-3 in 16-bit words by frame size code and sampling frequency (48, 44.1,
/// and 32 kHz).
const AC3_FRAME_WORDS: [[u16; 3]; 38] = [
    [64, 69, 96],
    [64, 70, 96],
    [80, 87, 120],
    [80, 88, 120],
    [96, 104, 144],
    [96, 105, 144],
    [112, 121, 168],
    [112, 122, 168],
    [128, 139, 192],
    [128, 140, 192],
    [160, 174, 240],
    [160, 175, 240],
    [192, 208, 288],
    [192, 209, 288],
    [224, 243, 336],
    [224, 244, 336],
    [256, 278, 384],
    [256, 279, 384],
    [320, 348, 480],
    [320, 349, 480],
    [384, 417, 576],
    [384, 418, 576],
    [448, 487, 672],
    [448, 488, 672],
    [512, 557, 768],
    [512, 558, 768],
    [640, 696, 960],
    [640, 697, 960],
    [768, 835, 1152],
    [768, 836, 1152],
    [896, 975, 1344],
    [896, 976, 1344],
    [1024, 1114, 1536],
    [1024, 1115, 1536],
    [1152, 1253, 1728],
    [1152, 1254, 1728],
    [1280, 1393, 1920],
    [1280, 1394, 1920],
];

const DTS_SAMPLE_RATES: [u32; 16] =
    [0, 8000, 16000, 32000, 0, 0, 11025, 22050, 44100, 0, 0, 12000, 24000, 48000, 0, 0];

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

/// The audio parameters described by the header of a frame of a compressed elementary stream.
pub struct AudioFrame {
    pub codec: AudioCodecId,
    pub sample_rate: u32,
    pub channels: Channels,
    pub samples_per_frame: u64,
    /// The length of the frame in the stream, in bytes.
    pub len: usize,
}

fn mask(positions: &[Position]) -> Channels {
    Channels::Positioned(positions.iter().fold(Position::empty(), |m, &p| m | p))
}

/// Parses the header of an AC-3 syncframe. `buf` must hold at least 8 bytes.
pub fn parse_ac3(buf: &[u8]) -> Option<AudioFrame> {
    use Position as P;

    if u16::from_be_bytes([buf[0], buf[1]]) != AC3_SYNC || buf[5] >> 3 > 10 {
        return None;
    }

    let fscod = usize::from(buf[4] >> 6);
    let frmsizecod = usize::from(buf[4] & 0x3f);
    if fscod == 3 || frmsizecod >= AC3_FRAME_WORDS.len() {
        return None;
    }

    // The channel mode follows the bit stream identification and mode, and is followed by the mix
    // levels that the mode selects, and the low frequency effects flag.
    let acmod = buf[6] >> 5;
    let mut skip = 0;
    if acmod & 1 != 0 && acmod != 1 {
        skip += 2;
    }
    if acmod & 4 != 0 {
        skip += 2;
    }
    if acmod == 2 {
        skip += 2;
    }
    let lfe_bit = 3 + skip;
    let lfeon = ((((u32::from(buf[6]) << 8) | u32::from(buf[7])) >> (16 - 1 - lfe_bit)) & 1) != 0;

    let mut positions = match acmod {
        0 | 2 => vec![P::FRONT_LEFT, P::FRONT_RIGHT],
        1 => vec![P::FRONT_CENTER],
        3 => vec![P::FRONT_LEFT, P::FRONT_CENTER, P::FRONT_RIGHT],
        4 => vec![P::FRONT_LEFT, P::FRONT_RIGHT, P::REAR_CENTER],
        5 => vec![P::FRONT_LEFT, P::FRONT_CENTER, P::FRONT_RIGHT, P::REAR_CENTER],
        6 => vec![P::FRONT_LEFT, P::FRONT_RIGHT, P::REAR_LEFT, P::REAR_RIGHT],
        _ => vec![P::FRONT_LEFT, P::FRONT_CENTER, P::FRONT_RIGHT, P::REAR_LEFT, P::REAR_RIGHT],
    };
    if lfeon {
        positions.push(P::LFE1);
    }

    Some(AudioFrame {
        codec: CODEC_ID_AC3,
        sample_rate: [48000, 44100, 32000][fscod],
        channels: mask(&positions),
        samples_per_frame: 1536,
        len: usize::from(AC3_FRAME_WORDS[frmsizecod][fscod]) * 2,
    })
}

/// Converts the first bytes of a DTS frame to the big endian, 16-bit packed form.
fn dts_header_bytes(buf: &[u8]) -> Option<([u8; 12], bool)> {
    let sync = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let mut out = [0u8; 12];

    match sync {
        DTS_SYNC_16_BE => out.copy_from_slice(&buf[..12]),
        DTS_SYNC_16_LE => {
            for (o, w) in out.chunks_exact_mut(2).zip(buf.chunks_exact(2)) {
                o.copy_from_slice(&[w[1], w[0]]);
            }
        }
        DTS_SYNC_14_BE | DTS_SYNC_14_LE => {
            let (mut acc, mut nacc, mut n) = (0u32, 0u32, 0);
            for w in buf.chunks_exact(2) {
                let word = if sync == DTS_SYNC_14_LE {
                    u16::from_le_bytes([w[0], w[1]])
                }
                else {
                    u16::from_be_bytes([w[0], w[1]])
                };
                acc = (acc << 14) | u32::from(word & 0x3fff);
                nacc += 14;
                while nacc >= 8 && n < out.len() {
                    out[n] = (acc >> (nacc - 8)) as u8;
                    n += 1;
                    nacc -= 8;
                }
                acc &= (1 << nacc) - 1;
                if n == out.len() {
                    break;
                }
            }
            if n < out.len() {
                return None;
            }
        }
        _ => return None,
    }

    Some((out, matches!(sync, DTS_SYNC_14_BE | DTS_SYNC_14_LE)))
}

/// Parses the header of a DTS core frame of any packing. `buf` must hold at least 16 bytes.
pub fn parse_dts(buf: &[u8]) -> Option<AudioFrame> {
    use Position as P;

    let (b, is_14_bit) = dts_header_bytes(buf)?;
    let bits = u128::from_be_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], 0, 0, 0, 0,
    ]);
    let field = |at: u32, n: u32| ((bits >> (128 - at - n)) & ((1 << n) - 1)) as usize;

    let nblks = field(39, 7);
    let fsize = field(46, 14);
    let amode = field(60, 6);
    let sample_rate = DTS_SAMPLE_RATES[field(66, 4)];
    let lff = field(85, 2);

    if nblks < 5 || fsize < 95 || sample_rate == 0 || lff == 3 {
        return None;
    }

    let mut positions = match amode {
        0 => vec![P::FRONT_CENTER],
        1..=4 => vec![P::FRONT_LEFT, P::FRONT_RIGHT],
        5 => vec![P::FRONT_CENTER, P::FRONT_LEFT, P::FRONT_RIGHT],
        6 => vec![P::FRONT_LEFT, P::FRONT_RIGHT, P::REAR_CENTER],
        7 => vec![P::FRONT_CENTER, P::FRONT_LEFT, P::FRONT_RIGHT, P::REAR_CENTER],
        8 => vec![P::FRONT_LEFT, P::FRONT_RIGHT, P::SIDE_LEFT, P::SIDE_RIGHT],
        9 => vec![P::FRONT_CENTER, P::FRONT_LEFT, P::FRONT_RIGHT, P::SIDE_LEFT, P::SIDE_RIGHT],
        _ => return None,
    };
    if lff > 0 {
        positions.push(P::LFE1);
    }

    let len = fsize + 1;
    Some(AudioFrame {
        codec: CODEC_ID_DCA,
        sample_rate,
        channels: mask(&positions),
        samples_per_frame: ((nblks + 1) * 32) as u64,
        len: if is_14_bit { (len * 8).div_ceil(14) * 2 } else { len },
    })
}

pub fn parse_mpeg_audio(word: u32) -> Option<AudioFrame> {
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

    let channels = if word & 0xc0 == 0xc0 {
        layouts::CHANNEL_LAYOUT_MONO
    }
    else {
        layouts::CHANNEL_LAYOUT_STEREO
    };
    Some(AudioFrame { codec, sample_rate, channels, samples_per_frame, len: frame_len })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_ac3_header() {
        // A stereo, 48 kHz frame of 768 bytes (192 kbit/s).
        let header = [
            0x0b, 0x77, 0x1c, 0x84, 0x14, 0x40, 0x43, 0xe1, 0x06, 0xf4, 0x63, 0x70, 0xc0, 0xc0,
            0xc2, 0x27,
        ];
        let frame = parse_ac3(&header).unwrap();
        assert_eq!(frame.sample_rate, 48000);
        assert_eq!(frame.samples_per_frame, 1536);
        assert_eq!(frame.len, 768);
        assert_eq!(frame.channels.count(), 2);
    }

    #[test]
    fn parses_little_endian_14_bit_dts_header() {
        // The header of a 5.1 stream of a DTS CD, which stores 14-bit words.
        let header = [
            0xff, 0x1f, 0x00, 0xe8, 0xf1, 0x07, 0xdf, 0xfc, 0x98, 0xfc, 0x00, 0xec, 0xee, 0xf4,
            0x09, 0x00,
        ];
        let frame = parse_dts(&header).unwrap();
        assert_eq!(frame.sample_rate, 44100);
        assert_eq!(frame.samples_per_frame, 1024);
        assert_eq!(frame.channels.count(), 6);
        assert_eq!(frame.len, 4096);
    }

    #[test]
    fn parses_mpeg_audio_headers() {
        // MPEG-1 Layer III, 128 kbps, 44.1 kHz, stereo.
        let frame = parse_mpeg_audio(0xFFFB_9000).unwrap();
        assert_eq!(frame.codec, CODEC_ID_MP3);
        assert_eq!((frame.sample_rate, frame.samples_per_frame, frame.len), (44100, 1152, 417));

        // MPEG-2 Layer III, 64 kbps, 22.05 kHz.
        let frame = parse_mpeg_audio(0xFFF3_8000).unwrap();
        assert_eq!((frame.sample_rate, frame.samples_per_frame, frame.len), (22050, 576, 208));
    }

    #[test]
    fn rejects_other_data() {
        assert!(parse_ac3(&[0u8; 16]).is_none());
        assert!(parse_dts(&[0u8; 16]).is_none());
        assert!(parse_mpeg_audio(0).is_none());
    }
}
