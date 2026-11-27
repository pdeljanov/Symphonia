// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::borrow::Cow;

use symphonia_core::audio::{Channels, Position};
use symphonia_core::errors::{Result, decode_error, unsupported_error};

use crate::bits::Bits;

pub(crate) const SYNC_16_BE: u32 = 0x7ffe_8001;
pub(crate) const SYNC_16_LE: u32 = 0xfe7f_0180;
pub(crate) const SYNC_14_BE: u32 = 0x1fff_e800;
pub(crate) const SYNC_14_LE: u32 = 0xff1f_00e8;

pub(crate) const MAX_CHANNELS: usize = 5;
pub(crate) const MAX_SUBBANDS: usize = 32;

const SAMPLE_RATES: [u32; 16] =
    [0, 8000, 16000, 32000, 0, 0, 11025, 22050, 44100, 0, 0, 12000, 24000, 48000, 0, 0];

/// Primary channels of every audio channel arrangement (AMODE), in bitstream order.
fn primary_positions(amode: u32) -> Option<&'static [Position]> {
    use Position as P;

    Some(match amode {
        0 => &[P::FRONT_CENTER],
        1..=4 => &[P::FRONT_LEFT, P::FRONT_RIGHT],
        5 => &[P::FRONT_CENTER, P::FRONT_LEFT, P::FRONT_RIGHT],
        6 => &[P::FRONT_LEFT, P::FRONT_RIGHT, P::REAR_CENTER],
        7 => &[P::FRONT_CENTER, P::FRONT_LEFT, P::FRONT_RIGHT, P::REAR_CENTER],
        8 => &[P::FRONT_LEFT, P::FRONT_RIGHT, P::SIDE_LEFT, P::SIDE_RIGHT],
        9 => &[P::FRONT_CENTER, P::FRONT_LEFT, P::FRONT_RIGHT, P::SIDE_LEFT, P::SIDE_RIGHT],
        _ => return None,
    })
}

#[derive(Clone, Debug)]
pub(crate) struct FrameHeader {
    pub(crate) num_blocks: usize,
    pub(crate) frame_len: usize,
    pub(crate) amode: u32,
    pub(crate) sample_rate: u32,
    pub(crate) rate: u32,
    pub(crate) dynf: bool,
    pub(crate) aspf: bool,
    pub(crate) ext_audio_id: u32,
    pub(crate) ext_audio: bool,
    pub(crate) lff: u32,
    pub(crate) hflag: bool,
    pub(crate) filts: bool,
    pub(crate) sumf: bool,
    pub(crate) sums: bool,
    pub(crate) cpf: bool,
    pub(crate) bits_used: usize,
}

impl FrameHeader {
    /// Parses the frame header of a big endian, 16-bit packed frame.
    pub(crate) fn parse(buf: &[u8]) -> Result<FrameHeader> {
        let mut bs = Bits(symphonia_core::io::BitReaderLtr::new(buf));

        if bs.u(32)? != SYNC_16_BE {
            return decode_error("dca: missing sync word");
        }

        bs.skip(1 + 5)?;
        let cpf = bs.flag()?;
        let nblks = bs.u(7)? as usize;
        let fsize = bs.u(14)? as usize;
        let amode = bs.u(6)?;
        let sfreq = bs.u(4)?;
        let rate = bs.u(5)?;
        bs.skip(1)?;
        let dynf = bs.flag()?;
        bs.skip(2 + 1)?;
        let ext_audio_id = bs.u(3)?;
        let ext_audio = bs.flag()?;
        let aspf = bs.flag()?;
        let lff = bs.u(2)?;
        let hflag = bs.flag()?;
        if cpf {
            bs.skip(16)?;
        }
        let filts = bs.flag()?;
        let vernum = bs.u(4)?;
        bs.skip(2 + 3)?;
        let sumf = bs.flag()?;
        let sums = bs.flag()?;
        bs.skip(4)?;
        let bits_used = 104 + if cpf { 16 } else { 0 };

        if nblks < 5 {
            return decode_error("dca: invalid number of blocks");
        }
        if fsize < 95 {
            return decode_error("dca: invalid frame size");
        }
        let sample_rate = SAMPLE_RATES[sfreq as usize];
        if sample_rate == 0 {
            return decode_error("dca: invalid sampling frequency");
        }
        if vernum > 7 {
            return unsupported_error("dca: unsupported encoder revision");
        }
        if lff == 3 {
            return decode_error("dca: invalid low frequency effects flag");
        }

        Ok(FrameHeader {
            num_blocks: nblks + 1,
            frame_len: fsize + 1,
            amode,
            sample_rate,
            rate,
            dynf,
            aspf,
            ext_audio_id,
            ext_audio,
            lff,
            hflag,
            filts,
            sumf,
            sums,
            cpf,
            bits_used,
        })
    }

    pub(crate) fn has_xch(&self) -> bool {
        self.ext_audio && self.ext_audio_id == 0 && self.amode == 9
    }

    pub(crate) fn samples_per_frame(&self) -> usize {
        self.num_blocks * 32
    }

    pub(crate) fn positions(&self) -> Result<Vec<Position>> {
        match primary_positions(self.amode) {
            Some(p) => Ok(p.to_vec()),
            None => unsupported_error("dca: unsupported channel arrangement"),
        }
    }

    pub(crate) fn num_primary(&self) -> usize {
        primary_positions(self.amode).map_or(0, <[Position]>::len)
    }

    pub(crate) fn channels(&self) -> Result<Channels> {
        let mut mask = Position::empty();
        for p in self.positions()? {
            mask |= p;
        }
        if self.lff > 0 {
            mask |= Position::LFE1;
        }
        if self.has_xch() {
            mask |= Position::REAR_CENTER;
        }
        Ok(Channels::Positioned(mask))
    }
}

/// Reads the first 12 bytes of a frame of any sync word type and returns the byte length of the
/// frame and its header.
pub(crate) fn peek_header(buf: &[u8]) -> Result<(FrameHeader, usize)> {
    let packed = to_big_endian_16(buf, 16)?;
    let hdr = FrameHeader::parse(&packed)?;
    let stored = match sync_kind(buf) {
        Some(SyncKind::Bits14) => (hdr.frame_len * 8).div_ceil(14) * 2,
        _ => hdr.frame_len,
    };
    Ok((hdr, stored))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncKind {
    Bits16,
    Bits14,
}

pub(crate) fn sync_kind(buf: &[u8]) -> Option<SyncKind> {
    if buf.len() < 4 {
        return None;
    }
    match u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) {
        SYNC_16_BE | SYNC_16_LE => Some(SyncKind::Bits16),
        SYNC_14_BE | SYNC_14_LE => Some(SyncKind::Bits14),
        _ => None,
    }
}

/// Converts a frame to the big endian, 16-bit packed form, taking at most `limit` bytes of output.
pub(crate) fn to_big_endian_16(buf: &[u8], limit: usize) -> Result<Cow<'_, [u8]>> {
    if buf.len() < 4 {
        return decode_error("dca: frame is too short");
    }
    let sync = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);

    match sync {
        SYNC_16_BE => Ok(Cow::Borrowed(buf)),
        SYNC_16_LE => Ok(Cow::Owned(
            buf.chunks_exact(2).take(limit / 2 + 1).flat_map(|w| [w[1], w[0]]).collect(),
        )),
        SYNC_14_BE | SYNC_14_LE => {
            let le = sync == SYNC_14_LE;
            let mut out = Vec::with_capacity(buf.len());
            let (mut acc, mut nacc) = (0u32, 0u32);
            for w in buf.chunks_exact(2) {
                let word = if le {
                    u16::from_le_bytes([w[0], w[1]])
                }
                else {
                    u16::from_be_bytes([w[0], w[1]])
                };
                acc = (acc << 14) | u32::from(word & 0x3fff);
                nacc += 14;
                while nacc >= 8 {
                    out.push((acc >> (nacc - 8)) as u8);
                    nacc -= 8;
                }
                acc &= (1 << nacc) - 1;
                if out.len() > limit {
                    break;
                }
            }
            Ok(Cow::Owned(out))
        }
        _ => decode_error("dca: missing sync word"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The start of a 5.1 frame of a 768 kbit/s stream.
    const FRAME: [u8; 16] = [
        0x7f, 0xfe, 0x80, 0x01, 0xfc, 0x3c, 0x3e, 0xd2, 0x75, 0xe0, 0x1d, 0x3b, 0x40, 0x09, 0x8c,
        0x63,
    ];

    #[test]
    fn parses_a_frame_header() {
        let hdr = FrameHeader::parse(&FRAME).unwrap();
        assert_eq!(hdr.samples_per_frame(), 512);
        assert_eq!(hdr.frame_len, 1006);
        assert_eq!(hdr.sample_rate, 48000);
        assert_eq!(hdr.amode, 9);
        assert_eq!(hdr.lff, 2);
    }

    #[test]
    fn converts_little_endian_and_14_bit_frames() {
        let le: Vec<u8> = FRAME.chunks(2).flat_map(|w| [w[1], w[0]]).collect();
        assert_eq!(&*to_big_endian_16(&le, usize::MAX).unwrap(), &FRAME);

        let mut bits = 0u128;
        for &b in &FRAME[..12] {
            bits = (bits << 8) | u128::from(b);
        }
        let mut packed = Vec::new();
        for k in 0..6 {
            let payload = ((bits >> (96 - 14 * (k + 1))) & 0x3fff) as u16;
            let word = if payload & 0x2000 != 0 { payload | 0xc000 } else { payload };
            packed.extend_from_slice(&word.to_be_bytes());
        }
        let out = to_big_endian_16(&packed, usize::MAX).unwrap();
        assert_eq!(&out[..8], &FRAME[..8]);
    }
}
