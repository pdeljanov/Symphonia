// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// WavPack v3 entropy word decoders, ported from unpack3.c by Conifer Software
// (BSD licence).  get_word1, get_old_word1, get_word2, get_word3.

use super::bits::Bits;

pub const WORD_EOF: i32 = i32::MIN;

const NUM_SAMPLES: u32 = 128;

#[inline(always)]
pub fn count_bits(x: u32) -> u32 {
    if x == 0 { 0 } else { 32 - x.leading_zeros() }
}

#[inline(always)]
pub fn bitset(n: u32) -> u32 {
    if n < 32 { 1u32 << n } else { 0 }
}

#[inline(always)]
pub fn bitmask(n: u32) -> u32 {
    if n == 0 { 0 } else if n >= 32 { u32::MAX } else { (1u32 << n) - 1 }
}

// ---------------------------------------------------------------------------
// w1 state  (used by get_word1 and get_old_word1)
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
pub struct W1State {
    // get_word1
    pub zeros_acc: u32,
    pub ave_level: [[u32; 2]; 3], // [K_DEPTH=3][2 channels]
    // get_old_word1
    pub index: [u32; 2],
    pub k_value: [u32; 2],
    pub ave_k: [u32; 2],
}

// ---------------------------------------------------------------------------
// w2 state  (used by get_word2)
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
pub struct W2State {
    pub last_dbits: [i32; 2],
    pub last_delta_sign: [i32; 2],
}

// ---------------------------------------------------------------------------
// w3 state  (used by get_word3)
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
pub struct W3State {
    pub ave_dbits: [i32; 2],
}

// ---------------------------------------------------------------------------
// Combined word state
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
pub struct WordState {
    pub w1: W1State,
    pub w2: W2State,
    pub w3: W3State,
}

// ---------------------------------------------------------------------------
// get_word1  – high-quality mode (HIGH_FLAG | NEW_HIGH_FLAG)
// ---------------------------------------------------------------------------

pub fn get_word1(ws: &mut WordState, bits: &mut Bits<'_>, chan: usize, flags: i16) -> i32 {
    const EXTREME_DECORR: i16 = 0x8000u16 as i16;
    const OVER_20: i16 = 0x40;

    if (flags & EXTREME_DECORR) != 0 && (flags & OVER_20) == 0 {
        if ws.w1.zeros_acc > 0 {
            ws.w1.zeros_acc -= 1;
            if ws.w1.zeros_acc > 0 {
                return 0;
            }
        } else if ws.w1.ave_level[0][0] < 0x20 && ws.w1.ave_level[0][1] < 0x20 {
            let mut cbits = 0i32;
            while cbits < 33 && bits.getbit() != 0 {
                cbits += 1;
            }
            if cbits == 33 {
                return WORD_EOF;
            }
            if cbits < 2 {
                ws.w1.zeros_acc = cbits as u32;
            } else {
                let mut mask = 1u32;
                ws.w1.zeros_acc = 0;
                let mut c = cbits - 1;
                while c > 0 {
                    if bits.getbit() != 0 {
                        ws.w1.zeros_acc |= mask;
                    }
                    mask <<= 1;
                    c -= 1;
                }
                ws.w1.zeros_acc |= mask;
            }
            if ws.w1.zeros_acc > 0 {
                return 0;
            }
        }
    }

    let mut ones_count = 0u32;
    while ones_count < 25 && bits.getbit() != 0 {
        ones_count += 1;
    }
    if ones_count == 25 {
        return WORD_EOF;
    }

    let k0 = {
        let v = ws.w1.ave_level[0][chan];
        let k = count_bits(v.wrapping_add(v >> 3).wrapping_add(0x40) >> 7);
        if k & !31 != 0 { return WORD_EOF; }
        k
    };

    let avalue: u32;
    if ones_count == 0 {
        let raw = bits.getbits(k0);
        avalue = raw & bitmask(k0);
    } else {
        let tmp1 = bitset(k0);
        let k1 = {
            let v = ws.w1.ave_level[1][chan];
            let k = count_bits(v.wrapping_add(v >> 4).wrapping_add(0x20) >> 6);
            if k & !31 != 0 { return WORD_EOF; }
            k
        };
        if ones_count == 1 {
            let raw = bits.getbits(k1);
            avalue = raw & bitmask(k1);
            ws.w1.ave_level[1][chan] = ws.w1.ave_level[1][chan]
                .wrapping_sub((ws.w1.ave_level[1][chan].wrapping_add(0x10)) >> 5)
                .wrapping_add(avalue);
            let avalue = avalue.wrapping_add(tmp1);
            ws.w1.ave_level[0][chan] = ws.w1.ave_level[0][chan]
                .wrapping_sub((ws.w1.ave_level[0][chan].wrapping_add(0x20)) >> 6)
                .wrapping_add(avalue);
            return if avalue != 0 && bits.getbit() != 0 { -(avalue as i32) } else { avalue as i32 };
        } else {
            let tmp2 = bitset(k1);
            let raw_av: u32;
            if ones_count == 24 {
                raw_av = bits.getbits(24) & 0xffffff;
            } else {
                let k2 = {
                    let v = ws.w1.ave_level[2][chan];
                    let k = count_bits(v.wrapping_add(0x10) >> 5);
                    if k & !31 != 0 { return WORD_EOF; }
                    k
                };
                let raw = bits.getbits(k2);
                raw_av = (raw & bitmask(k2)).wrapping_add(bitset(k2).wrapping_mul(ones_count - 2));
            }
            ws.w1.ave_level[2][chan] = ws.w1.ave_level[2][chan]
                .wrapping_sub((ws.w1.ave_level[2][chan].wrapping_add(0x8)) >> 4)
                .wrapping_add(raw_av);
            let av2 = raw_av.wrapping_add(tmp2);
            ws.w1.ave_level[1][chan] = ws.w1.ave_level[1][chan]
                .wrapping_sub((ws.w1.ave_level[1][chan].wrapping_add(0x10)) >> 5)
                .wrapping_add(av2);
            avalue = av2.wrapping_add(tmp1);
        }
    }

    ws.w1.ave_level[0][chan] = ws.w1.ave_level[0][chan]
        .wrapping_sub((ws.w1.ave_level[0][chan].wrapping_add(0x20)) >> 6)
        .wrapping_add(avalue);

    if avalue != 0 && bits.getbit() != 0 { -(avalue as i32) } else { avalue as i32 }
}

// ---------------------------------------------------------------------------
// get_old_word1  – older high-quality mode (HIGH_FLAG without NEW_HIGH_FLAG)
// ---------------------------------------------------------------------------

pub fn get_old_word1(ws: &mut WordState, bits: &mut Bits<'_>, chan: usize) -> i32 {
    if ws.w1.index[chan] == 0 {
        let guess_k = (ws.w1.ave_k[chan].wrapping_add(128) >> 8) as i32;
        let mut ones = 0i32;
        while ones < 72 && bits.getbit() != 0 {
            ones += 1;
        }
        if ones == 72 {
            return WORD_EOF;
        }
        ws.w1.k_value[chan] = if ones % 3 == 1 {
            (guess_k - (ones / 3) - 1).max(0) as u32
        } else {
            (guess_k + ones - (ones + 1) / 3).max(0) as u32
        };
        ws.w1.ave_k[chan] = ws.w1.ave_k[chan]
            .wrapping_sub((ws.w1.ave_k[chan].wrapping_add(0x10)) >> 5)
            .wrapping_add(ws.w1.k_value[chan] << 3);
    }

    ws.w1.index[chan] += 1;
    if ws.w1.index[chan] == NUM_SAMPLES {
        ws.w1.index[chan] = 0;
    }

    let k = ws.w1.k_value[chan];
    let raw = bits.getbits(k);

    let mut bc = 0u32;
    while bc < 32 && bits.getbit() != 0 {
        bc += 1;
    }
    if bc == 32 || (k & !31 != 0) {
        return WORD_EOF;
    }

    let avalue = (raw & bitmask(k)).wrapping_add(bitset(k).wrapping_mul(bc));
    if avalue != 0 && bits.getbit() != 0 { -(avalue as i32) } else { avalue as i32 }
}

// ---------------------------------------------------------------------------
// get_word2  – version 2 codec
// ---------------------------------------------------------------------------

pub fn get_word2(ws: &mut WordState, bits: &mut Bits<'_>, chan: usize, wphdr_bits: i16) -> i32 {
    let mut cbits = 0i32;
    while bits.getbit() != 0 {
        cbits += 2;
        if cbits == 50 {
            return WORD_EOF;
        }
    }
    if bits.getbit() != 0 {
        cbits += 1;
    }

    let delta_dbits = if cbits == 0 {
        0
    } else if cbits & 1 != 0 {
        let d = (cbits + 1) / 2;
        if ws.w2.last_delta_sign[chan] > 0 { -d } else { d }
    } else {
        let d = cbits / 2;
        if ws.w2.last_delta_sign[chan] <= 0 { -d } else { d }
    };

    ws.w2.last_delta_sign[chan] = delta_dbits;
    ws.w2.last_dbits[chan] += delta_dbits;
    let dbits = ws.w2.last_dbits[chan];

    if dbits < 0 || dbits > 20 {
        return WORD_EOF;
    }
    if dbits == 0 {
        return 0;
    }

    let dbits = dbits as u32;
    let mut value = 1i32 << (dbits - 1);
    let mut mask = 1i32;
    let mut d = dbits - 1;

    if wphdr_bits != 0 {
        while d > 0 {
            if d < wphdr_bits as u32 && bits.getbit() != 0 {
                value |= mask;
            }
            mask <<= 1;
            d -= 1;
        }
    } else {
        while d > 0 {
            if bits.getbit() != 0 {
                value |= mask;
            }
            mask <<= 1;
            d -= 1;
        }
    }

    if bits.getbit() != 0 { -value } else { value }
}

// ---------------------------------------------------------------------------
// get_word3  – fast/default mode
// ---------------------------------------------------------------------------

pub fn get_word3(ws: &mut WordState, bits: &mut Bits<'_>, chan: usize, wphdr_bits: i16) -> i32 {
    let mut cbits = 0i32;
    while cbits < 72 && bits.getbit() != 0 {
        cbits += 1;
    }
    if cbits == 72 {
        return WORD_EOF;
    }

    if cbits != 0 || bits.getbit() != 0 {
        cbits += 1;
    }

    let delta_dbits = if (cbits + 1) % 3 == 0 {
        (cbits + 1) / 3
    } else {
        -(cbits - cbits / 3)
    };

    let dbits = {
        let ave = ws.w3.ave_dbits[chan];
        let d = (ave >> 8) + 1 + delta_dbits;
        ws.w3.ave_dbits[chan] = ave.wrapping_sub((ave.wrapping_add(0x10)) >> 5).wrapping_add(d << 3);
        d
    };

    if dbits < 0 || dbits > 24 {
        return WORD_EOF;
    }
    if dbits == 0 {
        return 0;
    }

    let dbits = dbits as u32;

    if wphdr_bits != 0 && dbits > wphdr_bits as u32 {
        let value = bits.getbits(wphdr_bits as u32);
        let hbit = wphdr_bits as u32 - 1;
        if value & bitset(hbit) != 0 {
            -(((value & bitmask(wphdr_bits as u32)) as i32) << (dbits - wphdr_bits as u32))
        } else {
            (((value & bitmask(hbit)) | bitset(hbit)) as i32) << (dbits - wphdr_bits as u32)
        }
    } else {
        let value = bits.getbits(dbits);
        if value & bitset(dbits - 1) != 0 {
            -((value & bitmask(dbits)) as i32)
        } else {
            ((value & bitmask(dbits - 1)) | bitset(dbits - 1)) as i32
        }
    }
}
