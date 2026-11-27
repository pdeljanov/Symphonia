// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The Enhanced AC-3 syntax (ETSI TS 102 366, annex E): bit stream information and audio frame.

use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::io::{BitReaderLtr, FiniteBitStream};

use crate::bits::Bits;
use crate::frame::{FrameHeader, NUM_FBW_CHANNELS, SAMPLE_RATES, SYNC_WORD};
use crate::tables::FRAME_EXP_STRATEGY;

/// Parameters of an E-AC-3 syncframe that are not present in AC-3.
#[derive(Clone, Debug)]
pub struct Eac3Header {
    pub strmtyp: u8,
    pub substreamid: u8,
}

fn skip_flagged(bs: &mut Bits<'_>, bits: u32) -> Result<()> {
    if bs.flag()? {
        bs.skip(bits)?;
    }
    Ok(())
}

/// Parses the bit stream information of an E-AC-3 syncframe.
pub fn parse_header(buf: &[u8]) -> Result<FrameHeader> {
    let mut bs = Bits(BitReaderLtr::new(buf));

    if bs.u(16)? as u16 != SYNC_WORD {
        return decode_error("ac3: invalid sync word");
    }

    let strmtyp = bs.u(2)? as u8;
    let substreamid = bs.u(3)? as u8;
    let frmsiz = bs.u(11)? as usize;
    let fscod = bs.u(2)? as usize;
    let numblkscod = bs.u(2)? as usize;

    if strmtyp == 3 {
        return unsupported_error("ac3: reserved stream type");
    }
    if fscod == 3 {
        return unsupported_error("ac3: reduced sample rates of E-AC-3 are not supported");
    }

    let acmod = bs.u(3)? as usize;
    let lfeon = bs.flag()?;
    let bsid = bs.u(5)?;

    if !(11..=16).contains(&bsid) {
        return unsupported_error("ac3: unsupported bit stream id");
    }

    let nfchans = NUM_FBW_CHANNELS[acmod];

    // dialnorm
    bs.skip(5)?;
    skip_flagged(&mut bs, 8)?;

    if acmod == 0 {
        // dialnorm2
        bs.skip(5)?;
        skip_flagged(&mut bs, 8)?;
    }

    if strmtyp == 1 {
        skip_flagged(&mut bs, 16)?;
    }

    if bs.flag()? {
        parse_mixing_metadata(&mut bs, strmtyp, acmod, lfeon, numblkscod)?;
    }

    if bs.flag()? {
        // bsmod, copyrightb, origbs
        bs.skip(5)?;
        if acmod == 2 {
            // dsurmod, dheadphonmod
            bs.skip(4)?;
        }
        if acmod >= 6 {
            // dsurexmod
            bs.skip(2)?;
        }
        skip_flagged(&mut bs, 8)?;
        if acmod == 0 {
            skip_flagged(&mut bs, 8)?;
        }
        // sourcefscod
        bs.skip(1)?;
    }

    if strmtyp == 0 && numblkscod != 3 {
        // convsync
        bs.skip(1)?;
    }

    if strmtyp == 2 {
        let blkid = numblkscod == 3 || bs.flag()?;
        if blkid {
            // frmsizecod
            bs.skip(6)?;
        }
    }

    if bs.flag()? {
        let addbsil = bs.u(6)? + 1;
        bs.skip(addbsil * 8)?;
    }

    let bits_used = buf.len() * 8 - bs.0.bits_left() as usize;

    Ok(FrameHeader {
        sample_rate: SAMPLE_RATES[fscod],
        fscod,
        frame_len: 2 * (frmsiz + 1),
        acmod,
        lfeon,
        nfchans,
        num_blocks: [1, 2, 3, 6][numblkscod],
        bits_used,
        eac3: Some(Eac3Header { strmtyp, substreamid }),
    })
}

fn parse_mixing_metadata(
    bs: &mut Bits<'_>,
    strmtyp: u8,
    acmod: usize,
    lfeon: bool,
    numblkscod: usize,
) -> Result<()> {
    if acmod > 2 {
        // dmixmod
        bs.skip(2)?;
    }
    if (acmod & 1) != 0 && acmod > 2 {
        // ltrtcmixlev, lorocmixlev
        bs.skip(6)?;
    }
    if (acmod & 4) != 0 {
        // ltrtsurmixlev, lorosurmixlev
        bs.skip(6)?;
    }
    if lfeon {
        skip_flagged(bs, 5)?;
    }

    if strmtyp == 0 {
        skip_flagged(bs, 6)?;
        if acmod == 0 {
            skip_flagged(bs, 6)?;
        }
        skip_flagged(bs, 6)?;

        match bs.u(2)? {
            1 => bs.skip(5)?,
            2 => bs.skip(12)?,
            3 => {
                let mixdeflen = bs.u(5)?;
                let start_bits = 8 * (mixdeflen + 2);
                // The mixing option 4 data is a byte aligned block of a known length.
                bs.skip(start_bits)?;
            }
            _ => (),
        }

        if acmod < 2 {
            if bs.flag()? {
                bs.skip(14)?;
            }
            if acmod == 0 && bs.flag()? {
                bs.skip(14)?;
            }
        }

        if bs.flag()? {
            if numblkscod == 0 {
                bs.skip(5)?;
            }
            else {
                for _ in 0..[1, 2, 3, 6][numblkscod] {
                    skip_flagged(bs, 5)?;
                }
            }
        }
    }

    Ok(())
}

/// The audio frame information of an E-AC-3 syncframe.
#[derive(Clone, Debug, Default)]
pub struct Eac3Frame {
    pub strmtyp: u8,
    pub cplstre: [bool; 6],
    pub cplinu: [bool; 6],
    pub cplexpstr: [u8; 6],
    pub chexpstr: [[u8; 5]; 6],
    pub lfeexpstr: [u8; 6],
    pub snroffststr: u8,
    pub frmcsnroffst: usize,
    pub frmfsnroffst: usize,
    pub blkswe: bool,
    pub dithflage: bool,
    pub bamode: bool,
    pub frmfgaincode: bool,
    pub dbaflde: bool,
    pub skipflde: bool,
    pub chahtinu: [bool; 5],
    pub cplahtinu: bool,
    pub lfeahtinu: bool,
    pub spx_atten: [Option<u8>; 5],
}

/// Parses the audio frame information following the bit stream information.
pub fn parse_audfrm(bs: &mut Bits<'_>, hdr: &FrameHeader) -> Result<Eac3Frame> {
    let strmtyp = hdr.eac3.as_ref().map_or(0, |e| e.strmtyp);
    let nblks = hdr.num_blocks;
    let nfchans = hdr.nfchans;
    let mut f = Eac3Frame { strmtyp, ..Default::default() };

    let (expstre, ahte) = if nblks == 6 { (bs.flag()?, bs.flag()?) } else { (true, false) };

    f.snroffststr = bs.u(2)? as u8;
    let transproce = bs.flag()?;
    f.blkswe = bs.flag()?;
    f.dithflage = bs.flag()?;
    f.bamode = bs.flag()?;
    f.frmfgaincode = bs.flag()?;
    f.dbaflde = bs.flag()?;
    f.skipflde = bs.flag()?;
    let spxattene = bs.flag()?;

    if hdr.acmod > 1 {
        f.cplstre[0] = true;
        f.cplinu[0] = bs.flag()?;
        for blk in 1..nblks {
            f.cplstre[blk] = bs.flag()?;
            f.cplinu[blk] = if f.cplstre[blk] { bs.flag()? } else { f.cplinu[blk - 1] };
        }
    }

    let ncplblks = f.cplinu[..nblks].iter().filter(|&&c| c).count();

    if expstre {
        for blk in 0..nblks {
            if f.cplinu[blk] {
                f.cplexpstr[blk] = bs.u(2)? as u8;
            }
            for e in f.chexpstr[blk].iter_mut().take(nfchans) {
                *e = bs.u(2)? as u8;
            }
        }
    }
    else {
        if hdr.acmod > 1 && ncplblks > 0 {
            let code = bs.u(5)? as usize;
            f.cplexpstr = FRAME_EXP_STRATEGY[code];
        }
        for ch in 0..nfchans {
            let code = bs.u(5)? as usize;
            for (row, &strategy) in f.chexpstr.iter_mut().zip(FRAME_EXP_STRATEGY[code].iter()) {
                row[ch] = strategy;
            }
        }
    }

    if hdr.lfeon {
        for e in f.lfeexpstr.iter_mut().take(nblks) {
            *e = bs.u(1)? as u8;
        }
    }

    if strmtyp == 0 {
        let convexpstre = nblks == 6 || bs.flag()?;
        if convexpstre {
            bs.skip(5 * nfchans as u32)?;
        }
    }

    if ahte {
        let regs =
            |strategies: &mut dyn Iterator<Item = u8>| strategies.filter(|&s| s != 0).count();
        let ncplregs = (0..nblks).filter(|&b| f.cplstre[b] || f.cplexpstr[b] != 0).count();
        if ncplblks == 6 && ncplregs == 1 {
            f.cplahtinu = bs.flag()?;
        }
        for ch in 0..nfchans {
            if regs(&mut f.chexpstr[..nblks].iter().map(|row| row[ch])) == 1 {
                f.chahtinu[ch] = bs.flag()?;
            }
        }
        if hdr.lfeon && regs(&mut f.lfeexpstr[..nblks].iter().copied()) == 1 {
            f.lfeahtinu = bs.flag()?;
        }
    }

    if f.snroffststr == 0 {
        f.frmcsnroffst = bs.u(6)? as usize;
        f.frmfsnroffst = bs.u(4)? as usize;
    }

    if transproce {
        for _ in 0..nfchans {
            if bs.flag()? {
                // transprocloc, transproclen
                bs.skip(18)?;
            }
        }
    }

    if spxattene {
        for atten in f.spx_atten.iter_mut().take(nfchans) {
            if bs.flag()? {
                *atten = Some(bs.u(5)? as u8);
            }
        }
    }

    if nblks > 1 && bs.flag()? {
        let words = hdr.frame_len / 2;
        let bits = (usize::BITS - (words - 1).leading_zeros()) as usize;
        bs.skip(((nblks - 1) * (4 + bits)) as u32)?;
    }

    Ok(f)
}
