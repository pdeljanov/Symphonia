// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Synchronization information and bit stream information of an AC-3 syncframe.

use symphonia_core::audio::{Channels, Position};
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::io::{BitReaderLtr, FiniteBitStream, ReadBitsLtr};

use crate::eac3::{self, Eac3Header};
use crate::tables::FRAME_SIZE_TABLE;

pub const SYNC_WORD: u16 = 0x0b77;
pub const SAMPLES_PER_BLOCK: usize = 256;
pub const BLOCKS_PER_FRAME: usize = 6;
pub const SAMPLES_PER_FRAME: usize = SAMPLES_PER_BLOCK * BLOCKS_PER_FRAME;

/// The number of bytes of a syncframe that must be read to parse the header.
pub const MIN_HEADER_LEN: usize = 5;

pub const SAMPLE_RATES: [u32; 3] = [48000, 44100, 32000];

/// The number of full bandwidth channels for each audio coding mode.
pub const NUM_FBW_CHANNELS: [usize; 8] = [2, 1, 2, 3, 3, 4, 4, 5];

/// Parsed `syncinfo` and the parts of `bsi` that are needed to decode audio.
#[derive(Clone, Debug)]
pub struct FrameHeader {
    pub sample_rate: u32,
    pub num_blocks: usize,
    pub eac3: Option<Eac3Header>,
    pub fscod: usize,
    pub frame_len: usize,
    pub acmod: usize,
    pub lfeon: bool,
    pub nfchans: usize,
    /// The size of the header in bits, i.e. the position of the first audio block.
    pub bits_used: usize,
}

impl FrameHeader {
    /// If the frame is a dependent substream, or belongs to another programme than the first.
    pub fn is_additional_substream(&self) -> bool {
        self.eac3.as_ref().is_some_and(|e| e.strmtyp == 1 || e.substreamid != 0)
    }

    pub fn num_channels(&self) -> usize {
        self.nfchans + usize::from(self.lfeon)
    }

    /// The channel positions in the order of the coded channels, LFE last.
    pub fn channel_positions(&self) -> Vec<Position> {
        use Position as P;

        let mut pos = match self.acmod {
            0 | 2 => vec![P::FRONT_LEFT, P::FRONT_RIGHT],
            1 => vec![P::FRONT_CENTER],
            3 => vec![P::FRONT_LEFT, P::FRONT_CENTER, P::FRONT_RIGHT],
            4 => vec![P::FRONT_LEFT, P::FRONT_RIGHT, P::REAR_CENTER],
            5 => vec![P::FRONT_LEFT, P::FRONT_CENTER, P::FRONT_RIGHT, P::REAR_CENTER],
            6 => vec![P::FRONT_LEFT, P::FRONT_RIGHT, P::REAR_LEFT, P::REAR_RIGHT],
            _ => vec![P::FRONT_LEFT, P::FRONT_CENTER, P::FRONT_RIGHT, P::REAR_LEFT, P::REAR_RIGHT],
        };

        if self.lfeon {
            pos.push(P::LFE1);
        }

        pos
    }

    pub fn channels(&self) -> Channels {
        Channels::Positioned(
            self.channel_positions().into_iter().fold(Position::empty(), |a, p| a | p),
        )
    }

    /// Parses the header of a syncframe. `buf` must start with the sync word.
    pub fn parse(buf: &[u8]) -> Result<FrameHeader> {
        if buf.len() < MIN_HEADER_LEN + 3 {
            return decode_error("ac3: frame is too short");
        }

        // The bit stream id has the same position in both syntaxes.
        if buf[5] >> 3 > 10 {
            return eac3::parse_header(buf);
        }

        let mut bs = BitReaderLtr::new(buf);

        if bs.read_bits_leq32(16)? as u16 != SYNC_WORD {
            return decode_error("ac3: invalid sync word");
        }

        // crc1
        bs.ignore_bits(16)?;

        let fscod = bs.read_bits_leq32(2)? as usize;
        let frmsizecod = bs.read_bits_leq32(6)? as usize;

        let bsid = bs.read_bits_leq32(5)?;

        if bsid > 10 {
            return unsupported_error("ac3: unsupported bit stream id");
        }

        let Some(&rate) = SAMPLE_RATES.get(fscod)
        else {
            return decode_error("ac3: invalid sample rate code");
        };

        let Some(&(_, w32, w44, w48)) = FRAME_SIZE_TABLE.get(frmsizecod)
        else {
            return decode_error("ac3: invalid frame size code");
        };

        // Bit stream ids 9 and 10 halve and quarter the sample rate.
        let shift = bsid.saturating_sub(8);
        let sample_rate = rate >> shift;
        let frame_len = 2 * [w48, w44, w32][fscod] as usize;

        // bsmod
        bs.ignore_bits(3)?;

        let acmod = bs.read_bits_leq32(3)? as usize;

        if (acmod & 1) != 0 && acmod != 1 {
            // cmixlev
            bs.ignore_bits(2)?;
        }
        if (acmod & 4) != 0 {
            // surmixlev
            bs.ignore_bits(2)?;
        }
        if acmod == 2 {
            // dsurmod
            bs.ignore_bits(2)?;
        }

        let lfeon = bs.read_bool()?;
        // dialnorm
        bs.ignore_bits(5)?;

        if bs.read_bool()? {
            // compr
            bs.ignore_bits(8)?;
        }

        if bs.read_bool()? {
            // langcod
            bs.ignore_bits(8)?;
        }
        if bs.read_bool()? {
            // mixlevel, roomtyp
            bs.ignore_bits(7)?;
        }

        if acmod == 0 {
            // dialnorm2
            bs.ignore_bits(5)?;
            if bs.read_bool()? {
                // compr2
                bs.ignore_bits(8)?;
            }
            if bs.read_bool()? {
                // langcod2
                bs.ignore_bits(8)?;
            }
            if bs.read_bool()? {
                // mixlevel2, roomtyp2
                bs.ignore_bits(7)?;
            }
        }

        // copyrightb, origbs
        bs.ignore_bits(2)?;

        for _ in 0..2 {
            // timecod1e/timecod2e and the time codes
            if bs.read_bool()? {
                bs.ignore_bits(14)?;
            }
        }

        if bs.read_bool()? {
            let addbsil = bs.read_bits_leq32(6)? + 1;
            bs.ignore_bits(addbsil * 8)?;
        }

        let bits_used = buf.len() * 8 - bs.bits_left() as usize;

        Ok(FrameHeader {
            sample_rate,
            fscod,
            frame_len,
            acmod,
            lfeon,
            nfchans: NUM_FBW_CHANNELS[acmod],
            num_blocks: BLOCKS_PER_FRAME,
            eac3: None,
            bits_used,
        })
    }
}

/// Computes the CRC (x^16 + x^15 + x^2 + 1) of `data`, which is zero for an intact span.
fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
        }
    }
    crc
}

/// Checks the CRCs of a complete syncframe: both of an AC-3 frame, the one of an E-AC-3 frame.
pub fn verify_crc(frame: &[u8], eac3: bool) -> bool {
    if frame.len() < 8 {
        return false;
    }
    if eac3 {
        return crc16(&frame[2..]) == 0;
    }
    let words = frame.len() / 2;
    let first = 2 * ((words >> 1) + (words >> 3));
    crc16(&frame[2..first]) == 0 && crc16(&frame[2..]) == 0
}
