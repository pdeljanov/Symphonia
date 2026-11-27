// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{Result, decode_error};

pub(crate) const SYNC_BYTE: u8 = 0x9c;
pub(crate) const HEADER_LEN: usize = 4;
pub(crate) const MAX_SUBBANDS: usize = 8;
pub(crate) const MAX_BLOCKS: usize = 16;
pub(crate) const MAX_CHANNELS: usize = 2;
pub(crate) const MAX_FRAME_LEN: usize = 544;

const MAX_BITPOOL: usize = 250;
const MAX_BITS: i32 = 16;
/// Below this bit slice every subband that can take bits already has the most it can take.
const MIN_BITSLICE: i32 = -32;

const SAMPLE_RATES: [u32; 4] = [16000, 32000, 44100, 48000];
const BLOCK_COUNTS: [usize; 4] = [4, 8, 12, 16];

/// The offsets of the loudness bit allocation, by the sample rate and the subband.
const LOUDNESS_OFFSET_4: [[i32; 4]; 4] =
    [[-1, 0, 0, 0], [-2, 0, 0, 1], [-2, 0, 0, 1], [-2, 0, 0, 1]];
const LOUDNESS_OFFSET_8: [[i32; 8]; 4] = [
    [-2, 0, 0, 0, 0, 0, 0, 1],
    [-3, 0, 0, 0, 0, 0, 1, 2],
    [-4, 0, 0, 0, 0, 0, 1, 2],
    [-4, 0, 0, 0, 0, 0, 1, 2],
];

pub(crate) type SubbandSamples = [[[f32; MAX_SUBBANDS]; MAX_CHANNELS]; MAX_BLOCKS];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ChannelMode {
    Mono,
    Dual,
    Stereo,
    Joint,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct FrameHeader {
    pub sample_rate: u32,
    pub sample_rate_index: usize,
    pub blocks: usize,
    pub mode: ChannelMode,
    pub snr_allocation: bool,
    pub subbands: usize,
    pub bitpool: usize,
    pub crc: u8,
    pub frame_len: usize,
}

impl FrameHeader {
    pub fn parse(buf: &[u8]) -> Option<FrameHeader> {
        if buf.len() < HEADER_LEN || buf[0] != SYNC_BYTE {
            return None;
        }

        let sample_rate_index = usize::from(buf[1] >> 6);
        let blocks = BLOCK_COUNTS[usize::from((buf[1] >> 4) & 3)];
        let mode = match (buf[1] >> 2) & 3 {
            0 => ChannelMode::Mono,
            1 => ChannelMode::Dual,
            2 => ChannelMode::Stereo,
            _ => ChannelMode::Joint,
        };
        let snr_allocation = (buf[1] >> 1) & 1 != 0;
        let subbands = if buf[1] & 1 != 0 { 8 } else { 4 };
        let bitpool = usize::from(buf[2]);

        let per_channel_max = 16 * subbands;
        let max_bitpool = match mode {
            ChannelMode::Mono | ChannelMode::Dual => per_channel_max,
            ChannelMode::Stereo | ChannelMode::Joint => 2 * per_channel_max,
        };
        if bitpool < 2 || bitpool > max_bitpool.min(MAX_BITPOOL) {
            return None;
        }

        let channels = if mode == ChannelMode::Mono { 1 } else { 2 };
        let scale_factor_len = 4 * subbands * channels / 8;
        let audio_bits = match mode {
            ChannelMode::Mono | ChannelMode::Dual => blocks * channels * bitpool,
            ChannelMode::Stereo => blocks * bitpool,
            ChannelMode::Joint => subbands + blocks * bitpool,
        };
        let frame_len = HEADER_LEN + scale_factor_len + audio_bits.div_ceil(8);

        Some(FrameHeader {
            sample_rate: SAMPLE_RATES[sample_rate_index],
            sample_rate_index,
            blocks,
            mode,
            snr_allocation,
            subbands,
            bitpool,
            crc: buf[3],
            frame_len,
        })
    }

    pub fn channels(&self) -> usize {
        if self.mode == ChannelMode::Mono { 1 } else { 2 }
    }

    pub fn samples_per_frame(&self) -> usize {
        self.blocks * self.subbands
    }

    fn crc_bits(&self) -> usize {
        let join = if self.mode == ChannelMode::Joint { self.subbands } else { 0 };
        join + 4 * self.subbands * self.channels()
    }

    /// Checks the CRC of a whole frame.
    pub fn check_crc(&self, frame: &[u8]) -> bool {
        if frame.len() < self.frame_len {
            return false;
        }

        let mut crc = crc8(0x0f, frame[1], 8);
        crc = crc8(crc, frame[2], 8);

        let bits = self.crc_bits();
        for &byte in &frame[HEADER_LEN..HEADER_LEN + bits / 8] {
            crc = crc8(crc, byte, 8);
        }
        if bits % 8 != 0 {
            crc = crc8(crc, frame[HEADER_LEN + bits / 8], (bits % 8) as u32);
        }

        crc == self.crc
    }
}

/// Feeds the most significant `count` bits of a byte into a CRC-8 with the polynomial 0x1d.
fn crc8(mut crc: u8, byte: u8, count: u32) -> u8 {
    for i in 0..count {
        let bit = (byte >> (7 - i)) & 1;
        let feedback = (crc >> 7) ^ bit;
        crc <<= 1;
        if feedback != 0 {
            crc ^= 0x1d;
        }
    }
    crc
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn read(&mut self, count: u32) -> u32 {
        let mut value = 0;
        for _ in 0..count {
            let byte = self.data.get(self.pos >> 3).copied().unwrap_or(0);
            value = (value << 1) | u32::from((byte >> (7 - (self.pos & 7))) & 1);
            self.pos += 1;
        }
        value
    }
}

/// Distributes the bits of the bit pool between the subbands. `bitneed` has a row for every
/// channel that shares the pool.
fn allocate(
    bitneed: &[[i32; MAX_SUBBANDS]],
    subbands: usize,
    bitpool: usize,
    bits: &mut [[i32; MAX_SUBBANDS]],
) {
    let channels = bitneed.len();
    let bitpool = bitpool as i32;

    let max_need =
        bitneed.iter().flat_map(|row| row[..subbands].iter()).copied().max().unwrap_or(0);

    let mut bitcount = 0;
    let mut slicecount = 0;
    let mut bitslice = max_need + 1;
    loop {
        bitslice -= 1;
        bitcount += slicecount;
        slicecount = 0;

        for need in bitneed.iter().flat_map(|row| row[..subbands].iter()).copied() {
            if need > bitslice + 1 && need < bitslice + MAX_BITS {
                slicecount += 1;
            }
            else if need == bitslice + 1 {
                slicecount += 2;
            }
        }

        if bitcount + slicecount >= bitpool || bitslice < MIN_BITSLICE {
            break;
        }
    }
    if bitcount + slicecount == bitpool {
        bitcount += slicecount;
        bitslice -= 1;
    }

    for (need_row, bits_row) in bitneed.iter().zip(bits.iter_mut()) {
        for sb in 0..subbands {
            bits_row[sb] = if need_row[sb] < bitslice + 2 {
                0
            }
            else {
                (need_row[sb] - bitslice).min(MAX_BITS)
            };
        }
    }

    'first: for sb in 0..subbands {
        for ch in 0..channels {
            if bitcount >= bitpool {
                break 'first;
            }
            if bits[ch][sb] >= 2 && bits[ch][sb] < MAX_BITS {
                bits[ch][sb] += 1;
                bitcount += 1;
            }
            else if bitneed[ch][sb] == bitslice + 1 && bitpool > bitcount + 1 {
                bits[ch][sb] = 2;
                bitcount += 2;
            }
        }
    }

    'second: for sb in 0..subbands {
        for ch in 0..channels {
            if bitcount >= bitpool {
                break 'second;
            }
            if bits[ch][sb] < MAX_BITS {
                bits[ch][sb] += 1;
                bitcount += 1;
            }
        }
    }
}

/// Reads the scale factors and the quantized subband samples of a frame, and dequantizes them.
pub(crate) fn unpack(
    header: &FrameHeader,
    frame: &[u8],
    samples: &mut SubbandSamples,
) -> Result<()> {
    if !header.check_crc(frame) {
        return decode_error("sbc: crc mismatch");
    }

    let channels = header.channels();
    let subbands = header.subbands;

    let mut reader = BitReader { data: &frame[..header.frame_len], pos: HEADER_LEN * 8 };

    let mut join = [false; MAX_SUBBANDS];
    if header.mode == ChannelMode::Joint {
        for flag in join[..subbands].iter_mut() {
            *flag = reader.read(1) != 0;
        }
        // The last subband is never joined, the bit is reserved.
        join[subbands - 1] = false;
    }

    let mut scale_factors = [[0u32; MAX_SUBBANDS]; MAX_CHANNELS];
    for row in scale_factors[..channels].iter_mut() {
        for sf in row[..subbands].iter_mut() {
            *sf = reader.read(4);
        }
    }

    let mut bitneed = [[0i32; MAX_SUBBANDS]; MAX_CHANNELS];
    for ch in 0..channels {
        for sb in 0..subbands {
            let sf = scale_factors[ch][sb] as i32;
            bitneed[ch][sb] = if header.snr_allocation {
                sf
            }
            else if sf == 0 {
                -5
            }
            else {
                let offset = if subbands == 4 {
                    LOUDNESS_OFFSET_4[header.sample_rate_index][sb]
                }
                else {
                    LOUDNESS_OFFSET_8[header.sample_rate_index][sb]
                };
                let loudness = sf - offset;
                if loudness > 0 { loudness / 2 } else { loudness }
            };
        }
    }

    let mut bits = [[0i32; MAX_SUBBANDS]; MAX_CHANNELS];
    match header.mode {
        ChannelMode::Mono | ChannelMode::Dual => {
            for ch in 0..channels {
                allocate(&bitneed[ch..=ch], subbands, header.bitpool, &mut bits[ch..=ch]);
            }
        }
        ChannelMode::Stereo | ChannelMode::Joint => {
            allocate(&bitneed[..channels], subbands, header.bitpool, &mut bits[..channels]);
        }
    }

    for block in samples[..header.blocks].iter_mut() {
        for ch in 0..channels {
            for sb in 0..subbands {
                let nbits = bits[ch][sb];
                block[ch][sb] = if nbits == 0 {
                    0.0
                }
                else {
                    let quantized = reader.read(nbits as u32) as f32;
                    let levels = ((1u32 << nbits) - 1) as f32;
                    let scale = (1u32 << (scale_factors[ch][sb] + 1)) as f32;
                    ((2.0 * quantized + 1.0) / levels - 1.0) * scale
                };
            }
        }

        if header.mode == ChannelMode::Joint {
            for sb in 0..subbands {
                if join[sb] {
                    let (left, right) = (block[0][sb], block[1][sb]);
                    block[0][sb] = left + right;
                    block[1][sb] = left - right;
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn header_bytes(second: u8, bitpool: u8) -> [u8; 4] {
        [SYNC_BYTE, second, bitpool, 0]
    }

    #[test]
    fn parses_joint_stereo_header() {
        // 44.1 kHz, 16 blocks, joint stereo, loudness, 8 subbands, bit pool 17.
        let header = FrameHeader::parse(&header_bytes(0b10_11_11_0_1, 17)).unwrap();
        assert_eq!(header.sample_rate, 44100);
        assert_eq!(header.blocks, 16);
        assert_eq!(header.mode, ChannelMode::Joint);
        assert!(!header.snr_allocation);
        assert_eq!(header.subbands, 8);
        assert_eq!(header.frame_len, 4 + 8 + (8 + 16 * 17usize).div_ceil(8));
        assert_eq!(header.samples_per_frame(), 128);
    }

    #[test]
    fn rejects_invalid_headers() {
        assert!(FrameHeader::parse(&[0x9d, 0, 20, 0]).is_none());
        assert!(FrameHeader::parse(&header_bytes(0b10_11_11_0_1, 1)).is_none());
        // A bit pool of 65 is too large for 4 subbands of a single channel.
        assert!(FrameHeader::parse(&header_bytes(0b10_11_00_0_0, 65)).is_none());
    }

    #[test]
    fn crc_detects_a_changed_frame() {
        let mut frame = vec![SYNC_BYTE, 0b10_11_00_0_0, 8, 0];
        frame.resize(4 + 2 + (16 * 8usize).div_ceil(8), 0);
        let crc = {
            let mut crc = crc8(0x0f, frame[1], 8);
            crc = crc8(crc, frame[2], 8);
            for &byte in &frame[4..6] {
                crc = crc8(crc, byte, 8);
            }
            crc
        };
        frame[3] = crc;
        let header = FrameHeader::parse(&frame).unwrap();
        assert!(header.check_crc(&frame));
        frame[5] ^= 1;
        assert!(!header.check_crc(&frame));
    }

    #[test]
    fn allocation_uses_the_whole_pool() {
        let need = [[5, 4, 3, 2, 1, 0, -1, -2]];
        let mut bits = [[0; MAX_SUBBANDS]];
        allocate(&need, 8, 20, &mut bits);
        assert_eq!(bits[0].iter().sum::<i32>(), 20);
        assert!(bits[0].iter().all(|&b| b <= MAX_BITS));
    }
}
