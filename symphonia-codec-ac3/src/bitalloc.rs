// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The parametric bit allocation routine (ETSI TS 102 366, clause 6.2).

use crate::tables::{
    BAPTAB, BNDSZ, BNDTAB, DBPBTAB, FASTDEC, FASTGAIN, FLOORTAB, HEBAPTAB, HTH, LATAB, MASKTAB,
    SLOWDEC, SLOWGAIN,
};

/// The global bit allocation parameters sent by `baie`.
#[derive(Copy, Clone, Debug, Default)]
pub struct BitAllocParams {
    pub sdcycod: usize,
    pub fdcycod: usize,
    pub sgaincod: usize,
    pub dbpbcod: usize,
    pub floorcod: usize,
}

/// A segment of delta bit allocation.
#[derive(Copy, Clone, Debug, Default)]
pub struct DeltaSegment {
    pub offset: usize,
    pub len: usize,
    pub ba: usize,
}

/// The inputs of the bit allocation of a single channel.
pub struct ChannelAlloc<'a> {
    pub exps: &'a [u8],
    pub start: usize,
    pub end: usize,
    pub fscod: usize,
    pub fgaincod: usize,
    pub snroffset: i32,
    /// The initial fast and slow leak, only used by the coupling channel.
    pub leak: Option<(i32, i32)>,
    pub delta: &'a [DeltaSegment],
    /// Allocate high efficiency bit allocation pointers for the adaptive hybrid transform.
    pub aht: bool,
}

fn logadd(a: i32, b: i32) -> i32 {
    let c = a - b;
    let address = ((c.abs() >> 1) as usize).min(255);
    if c >= 0 { a + i32::from(LATAB[address]) } else { b + i32::from(LATAB[address]) }
}

fn calc_lowcomp(a: i32, b0: i32, b1: i32, bin: usize) -> i32 {
    if bin < 7 {
        if b0 + 256 == b1 {
            384
        }
        else if b0 > b1 {
            (a - 64).max(0)
        }
        else {
            a
        }
    }
    else if bin < 20 {
        if b0 + 256 == b1 {
            320
        }
        else if b0 > b1 {
            (a - 64).max(0)
        }
        else {
            a
        }
    }
    else {
        (a - 128).max(0)
    }
}

/// Computes the bit allocation pointers of a channel, writing `bap[start..end]`.
pub fn compute_bap(p: &BitAllocParams, ch: &ChannelAlloc<'_>, bap: &mut [u8]) {
    let (start, end) = (ch.start, ch.end);

    if end <= start {
        return;
    }

    let sdecay = SLOWDEC[p.sdcycod];
    let fdecay = FASTDEC[p.fdcycod];
    let sgain = SLOWGAIN[p.sgaincod];
    let dbknee = DBPBTAB[p.dbpbcod];
    let floor = FLOORTAB[p.floorcod];
    let fgain = FASTGAIN[ch.fgaincod];

    let mut psd = [0i32; 256];
    for bin in start..end {
        psd[bin] = 3072 - (i32::from(ch.exps[bin]) << 7);
    }

    // Integrate the psd in each band.
    let mut bndpsd = [0i32; 51];
    let mut j = start;
    let mut k = usize::from(MASKTAB[start]);
    loop {
        let lastbin = ((BNDTAB[k] + BNDSZ[k]) as usize).min(end);
        bndpsd[k] = psd[j];
        j += 1;
        while j < lastbin {
            bndpsd[k] = logadd(bndpsd[k], psd[j]);
            j += 1;
        }
        k += 1;
        if end <= lastbin {
            break;
        }
    }

    // Compute the excitation function.
    let bndstrt = usize::from(MASKTAB[start]);
    let bndend = usize::from(MASKTAB[end - 1]) + 1;

    let mut excite = [0i32; 50];

    if bndstrt == 0 {
        let mut lowcomp = 0;
        lowcomp = calc_lowcomp(lowcomp, bndpsd[0], bndpsd[1], 0);
        excite[0] = bndpsd[0] - fgain - lowcomp;
        lowcomp = calc_lowcomp(lowcomp, bndpsd[1], bndpsd[2], 1);
        excite[1] = bndpsd[1] - fgain - lowcomp;

        let mut next = 7;
        let mut fastleak = 0;
        let mut slowleak = 0;

        for bin in 2..7 {
            if bndend != 7 || bin != 6 {
                lowcomp = calc_lowcomp(lowcomp, bndpsd[bin], bndpsd[bin + 1], bin);
            }
            fastleak = bndpsd[bin] - fgain;
            slowleak = bndpsd[bin] - sgain;
            excite[bin] = fastleak - lowcomp;
            if (bndend != 7 || bin != 6) && bndpsd[bin] <= bndpsd[bin + 1] {
                next = bin + 1;
                break;
            }
        }

        for bin in next..bndend.min(22) {
            if bndend != 7 || bin != 6 {
                lowcomp = calc_lowcomp(lowcomp, bndpsd[bin], bndpsd[bin + 1], bin);
            }
            fastleak -= fdecay;
            fastleak = fastleak.max(bndpsd[bin] - fgain);
            slowleak -= sdecay;
            slowleak = slowleak.max(bndpsd[bin] - sgain);
            excite[bin] = (fastleak - lowcomp).max(slowleak);
        }

        for bin in 22..bndend {
            fastleak -= fdecay;
            fastleak = fastleak.max(bndpsd[bin] - fgain);
            slowleak -= sdecay;
            slowleak = slowleak.max(bndpsd[bin] - sgain);
            excite[bin] = fastleak.max(slowleak);
        }
    }
    else {
        let (mut fastleak, mut slowleak) = ch.leak.unwrap_or((0, 0));
        for bin in bndstrt..bndend {
            fastleak -= fdecay;
            fastleak = fastleak.max(bndpsd[bin] - fgain);
            slowleak -= sdecay;
            slowleak = slowleak.max(bndpsd[bin] - sgain);
            excite[bin] = fastleak.max(slowleak);
        }
    }

    // Compute the masking curve.
    let mut mask = [0i32; 50];
    for bin in bndstrt..bndend {
        if bndpsd[bin] < dbknee {
            excite[bin] += (dbknee - bndpsd[bin]) >> 2;
        }
        mask[bin] = excite[bin].max(i32::from(HTH[ch.fscod][bin]));
    }

    // Apply the delta bit allocation.
    let mut band = 0;
    for seg in ch.delta {
        band += seg.offset;
        let delta = if seg.ba >= 4 { (seg.ba as i32 - 3) << 7 } else { (seg.ba as i32 - 4) << 7 };
        for _ in 0..seg.len {
            if let Some(m) = mask.get_mut(band) {
                *m += delta;
            }
            band += 1;
        }
    }

    // Compute the bit allocation pointers.
    let mut i = start;
    let mut j = bndstrt;
    loop {
        let lastbin = ((BNDTAB[j] + BNDSZ[j]) as usize).min(end);
        let mut m = mask[j] - ch.snroffset - floor;
        if m < 0 {
            m = 0;
        }
        m &= 0x1fe0;
        m += floor;
        while i < lastbin {
            let address = ((psd[i] - m) >> 5).clamp(0, 63);
            bap[i] = if ch.aht { HEBAPTAB[address as usize] } else { BAPTAB[address as usize] };
            i += 1;
        }
        j += 1;
        if end <= lastbin {
            break;
        }
    }
}
