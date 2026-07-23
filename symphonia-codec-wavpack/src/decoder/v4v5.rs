// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// WavPack v4/v5 lossless PCM decoder.
// Ported from the WavPack reference implementation by Conifer Software
// (https://github.com/dbry/WavPack, BSD licence).

// ---------------------------------------------------------------------------
// Block flag bits (wavpack.h)
// ---------------------------------------------------------------------------

pub const MONO_FLAG:    u32 = 0x0000_0004;
pub const HYBRID_FLAG:  u32 = 0x0000_0008;
pub const JOINT_STEREO: u32 = 0x0000_0010;
pub const CROSS_DECORR: u32 = 0x0000_0020;
pub const FLOAT_DATA:   u32 = 0x0000_0080;
pub const INT32_DATA:   u32 = 0x0000_0100;
pub const SHIFT_LSB:    u32 = 13;
pub const SHIFT_MASK:   u32 = 0x1f << 13;
pub const FALSE_STEREO: u32 = 0x4000_0000;
pub const MONO_DATA:    u32 = MONO_FLAG | FALSE_STEREO;

// ---------------------------------------------------------------------------
// Packet magic — 4 bytes that distinguish a v4/v5 packet from a v3 prefix
// ---------------------------------------------------------------------------

pub const PACKET_MAGIC: &[u8; 4] = b"WV45";

// Fixed header size in the packet data
//   magic(4) + flags(4) + block_samples(4) + crc(4)
//   + terms_len(2) + weights_len(2) + samples_len(2) + entropy_len(2)
//   + int32_len(1) + pad(3) = 28 bytes
pub const PKT_HDR: usize = 28;

// ---------------------------------------------------------------------------
// Sub-block size limits to guard against malformed streams
// ---------------------------------------------------------------------------

const MAX_NTERMS: usize = 16;
const MAX_TERM:   usize = 8;
const LIMIT_ONES: u32   = 16;

// INC/DEC median divisors
const DIV0: u32 = 128;
const DIV1: u32 = 64;
const DIV2: u32 = 32;

// Lightweight trace toggle: set WAVPACK_TRACE=1 to enable stderr debug prints.
fn trace_enabled() -> bool {
    use std::sync::atomic::{AtomicI8, Ordering};
    static FLAG: AtomicI8 = AtomicI8::new(-1);
    match FLAG.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = match std::env::var("WAVPACK_TRACE").ok().as_deref() {
                Some("1") | Some("true") | Some("yes") => 1,
                _ => 0,
            };
            FLAG.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

macro_rules! wp_trace {
    ($($arg:tt)*) => { if $crate::decoder::v4v5::trace_enabled() { eprintln!($($arg)*); } };
}

// ---------------------------------------------------------------------------
// LSB-first bit reader (identical scheme to the v3 Bits struct)
// ---------------------------------------------------------------------------

struct Bits<'a> {
    data: &'a [u8],
    ptr:  usize,
    bc:   u32,
    sr:   u64,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, ptr: 0, bc: 0, sr: 0 }
    }

    #[inline(always)]
    fn getbit(&mut self) -> u32 {
        if self.bc == 0 {
            let byte = self.next_byte() as u64;
            self.sr = byte;
            self.bc = 7;
            let bit = (self.sr & 1) as u32;
            self.sr >>= 1;
            bit
        } else {
            self.bc -= 1;
            let bit = (self.sr & 1) as u32;
            self.sr >>= 1;
            bit
        }
    }

    #[inline(always)]
    fn getbits(&mut self, nbits: u32) -> u32 {
        if nbits == 0 { return 0; }
        while nbits > self.bc {
            let byte = self.next_byte() as u64;
            self.sr |= byte << self.bc;
            self.bc += 8;
        }
        let val = (self.sr & ((1u64 << nbits) - 1)) as u32;
        self.sr >>= nbits;
        self.bc -= nbits;
        val
    }

    #[inline(always)]
    fn next_byte(&mut self) -> u8 {
        if self.ptr < self.data.len() {
            let b = self.data[self.ptr];
            self.ptr += 1;
            b
        } else {
            0x00
        }
    }
}

// ---------------------------------------------------------------------------
// count_bits: number of bits needed to represent n
// ---------------------------------------------------------------------------

#[inline(always)]
fn count_bits(n: u32) -> u32 {
    32 - n.leading_zeros()
}

// ---------------------------------------------------------------------------
// read_code: read a range-coded value in [0, maxcode]
// Portable version of the WavPack read_code() function.
// ---------------------------------------------------------------------------

fn read_code(bs: &mut Bits<'_>, maxcode: u32) -> u32 {
    if maxcode == 0 { return 0; }
    if maxcode == 1 { return bs.getbit(); }

    let bitcount = count_bits(maxcode);
    let extras    = (1u32 << bitcount) - maxcode - 1;
    let code      = bs.getbits(bitcount - 1);

    if code >= extras {
        (code << 1) - extras + bs.getbit()
    } else {
        code
    }
}

// ---------------------------------------------------------------------------
// wp_exp2s: decode log2 representation stored in sub-blocks.
// Ported from entropy_utils.c; uses the same exp2_table lookup.
// ---------------------------------------------------------------------------

#[rustfmt::skip]
static EXP2_TABLE: [u8; 256] = [
    0x00,0x01,0x01,0x02,0x03,0x03,0x04,0x05,0x06,0x06,0x07,0x08,0x08,0x09,0x0a,0x0b,
    0x0b,0x0c,0x0d,0x0e,0x0e,0x0f,0x10,0x10,0x11,0x12,0x13,0x13,0x14,0x15,0x16,0x16,
    0x17,0x18,0x19,0x19,0x1a,0x1b,0x1c,0x1d,0x1d,0x1e,0x1f,0x20,0x20,0x21,0x22,0x23,
    0x24,0x24,0x25,0x26,0x27,0x28,0x28,0x29,0x2a,0x2b,0x2c,0x2c,0x2d,0x2e,0x2f,0x30,
    0x30,0x31,0x32,0x33,0x34,0x35,0x35,0x36,0x37,0x38,0x39,0x3a,0x3a,0x3b,0x3c,0x3d,
    0x3e,0x3f,0x40,0x41,0x41,0x42,0x43,0x44,0x45,0x46,0x47,0x48,0x48,0x49,0x4a,0x4b,
    0x4c,0x4d,0x4e,0x4f,0x50,0x51,0x51,0x52,0x53,0x54,0x55,0x56,0x57,0x58,0x59,0x5a,
    0x5b,0x5c,0x5d,0x5e,0x5e,0x5f,0x60,0x61,0x62,0x63,0x64,0x65,0x66,0x67,0x68,0x69,
    0x6a,0x6b,0x6c,0x6d,0x6e,0x6f,0x70,0x71,0x72,0x73,0x74,0x75,0x76,0x77,0x78,0x79,
    0x7a,0x7b,0x7c,0x7d,0x7e,0x7f,0x80,0x81,0x82,0x83,0x84,0x85,0x87,0x88,0x89,0x8a,
    0x8b,0x8c,0x8d,0x8e,0x8f,0x90,0x91,0x92,0x93,0x95,0x96,0x97,0x98,0x99,0x9a,0x9b,
    0x9c,0x9d,0x9f,0xa0,0xa1,0xa2,0xa3,0xa4,0xa5,0xa6,0xa8,0xa9,0xaa,0xab,0xac,0xad,
    0xaf,0xb0,0xb1,0xb2,0xb3,0xb4,0xb6,0xb7,0xb8,0xb9,0xba,0xbc,0xbd,0xbe,0xbf,0xc0,
    0xc2,0xc3,0xc4,0xc5,0xc6,0xc8,0xc9,0xca,0xcb,0xcd,0xce,0xcf,0xd0,0xd2,0xd3,0xd4,
    0xd6,0xd7,0xd8,0xd9,0xdb,0xdc,0xdd,0xde,0xe0,0xe1,0xe2,0xe4,0xe5,0xe6,0xe8,0xe9,
    0xea,0xec,0xed,0xee,0xf0,0xf1,0xf2,0xf4,0xf5,0xf6,0xf8,0xf9,0xfa,0xfc,0xfd,0xff,
];

fn wp_exp2s(log: i32) -> i32 {
    if log < 0 {
        return -(wp_exp2s(-log));
    }
    let value = (EXP2_TABLE[(log & 0xff) as usize] as u32) | 0x100;
    let shift  = log >> 8;
    if shift <= 9 {
        (value >> (9 - shift)) as i32
    } else {
        (value << ((shift - 9) & 0x1f)) as i32
    }
}

// ---------------------------------------------------------------------------
// Decorrelation pass
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
pub struct DecorrPass {
    pub term:      i32,
    pub delta:     i32,
    pub weight_a:  i32,
    pub weight_b:  i32,
    pub samples_a: [i32; MAX_TERM],
    pub samples_b: [i32; MAX_TERM],
}

// ---------------------------------------------------------------------------
// Entropy / words state
// ---------------------------------------------------------------------------

#[derive(Default, Clone, Copy)]
pub struct EntropyChannel {
    pub median: [u32; 3],
}

#[derive(Default, Clone)]
pub struct WordsState {
    pub c:           [EntropyChannel; 2],
    pub holding_one: u32,
    pub holding_zero: i32,
    pub zeros_acc:   u32,
}

// ---------------------------------------------------------------------------
// Int32 info (from ID_INT32_INFO sub-block)
// ---------------------------------------------------------------------------

#[derive(Default, Clone, Copy)]
pub struct Int32Info {
    pub sent_bits: u8,
    pub zeros:     u8,
    pub ones:      u8,
    pub dups:      u8,
}

// ---------------------------------------------------------------------------
// Sub-block parsers — called from decoder with raw sub-block bytes
// ---------------------------------------------------------------------------

fn restore_weight(b: i8) -> i32 {
    let r = (b as i32) << 3;
    if r > 0 { r + ((r + 64) >> 7) } else { r }
}

pub fn parse_decorr_terms(data: &[u8], passes: &mut Vec<DecorrPass>) {
    // ID_DECORR_TERMS stores terms in REVERSE order relative to dpp[]:
    // bytes are written by the encoder iterating dpp from last to first.
    // Mirror the C reference (decorr_utils.c read_decorr_terms) and place
    // byte[0] into passes[num-1], byte[num-1] into passes[0].
    let num = data.len().min(MAX_NTERMS);
    passes.clear();
    passes.resize_with(num, DecorrPass::default);
    for i in 0..num {
        let byte  = data[i];
        let term  = ((byte & 0x1f) as i32) - 5;
        let delta = ((byte >> 5) & 0x7) as i32;
        passes[num - 1 - i].term  = term;
        passes[num - 1 - i].delta = delta;
    }
}

pub fn parse_decorr_weights(data: &[u8], passes: &mut [DecorrPass], is_mono: bool) {
    // ID_DECORR_WEIGHTS also stores entries in REVERSE dpp[] order — see
    // decorr_utils.c read_decorr_weights. Walk bytes forward, fill passes
    // from the last index backwards. Unused weights (no byte) stay at 0
    // (already cleared via Default).
    let stride = if is_mono { 1usize } else { 2 };
    let n = passes.len();
    let mut byte = 0usize;
    for j in 0..n {
        let p = &mut passes[n - 1 - j];
        if byte + stride > data.len() {
            break;
        }
        p.weight_a = restore_weight(data[byte] as i8);
        if !is_mono {
            p.weight_b = restore_weight(data[byte + 1] as i8);
        }
        byte += stride;
    }
}

pub fn parse_decorr_samples(data: &[u8], passes: &mut [DecorrPass], is_mono: bool) {
    // ID_DECORR_SAMPLES also iterates dpp in REVERSE — see
    // decorr_utils.c read_decorr_samples. Mirror that order here.
    let mut ptr = 0usize;

    let mut read_i16 = |data: &[u8], ptr: &mut usize| -> i32 {
        let v = if *ptr + 2 <= data.len() {
            i16::from_le_bytes([data[*ptr], data[*ptr + 1]]) as i32
        } else {
            0
        };
        *ptr += 2;
        v
    };

    let n = passes.len();
    for j in 0..n {
        let p = &mut passes[n - 1 - j];
        p.samples_a = [0i32; MAX_TERM];
        p.samples_b = [0i32; MAX_TERM];

        if p.term > MAX_TERM as i32 {
            // terms 17 and 18: linear/quadratic extrapolation from 2 samples each
            p.samples_a[0] = wp_exp2s(read_i16(data, &mut ptr));
            p.samples_a[1] = wp_exp2s(read_i16(data, &mut ptr));
            if !is_mono {
                p.samples_b[0] = wp_exp2s(read_i16(data, &mut ptr));
                p.samples_b[1] = wp_exp2s(read_i16(data, &mut ptr));
            }
        } else if p.term < 0 {
            p.samples_a[0] = wp_exp2s(read_i16(data, &mut ptr));
            p.samples_b[0] = wp_exp2s(read_i16(data, &mut ptr));
        } else {
            let cnt = p.term as usize;
            for m in 0..cnt {
                p.samples_a[m] = wp_exp2s(read_i16(data, &mut ptr));
                if !is_mono {
                    p.samples_b[m] = wp_exp2s(read_i16(data, &mut ptr));
                }
            }
        }

        if ptr > data.len() { break; }
    }
}

pub fn parse_entropy_vars(data: &[u8], ws: &mut WordsState, is_mono: bool) {
    wp_trace!("[pev] entropy_raw ({} bytes): {:02x?}", data.len(), data);
    let mut ptr = 0usize;
    let mut read_i16 = |data: &[u8], ptr: &mut usize| -> i32 {
        let v = if *ptr + 2 <= data.len() {
            i16::from_le_bytes([data[*ptr], data[*ptr + 1]]) as i32
        } else {
            0
        };
        *ptr += 2;
        v
    };
    let log0 = read_i16(data, &mut ptr);
    let log1 = read_i16(data, &mut ptr);
    let log2 = read_i16(data, &mut ptr);
    ws.c[0].median[0] = wp_exp2s(log0) as u32;
    ws.c[0].median[1] = wp_exp2s(log1) as u32;
    ws.c[0].median[2] = wp_exp2s(log2) as u32;
    wp_trace!("[pev] ch0 logs={},{},{} medians={},{},{}", log0,log1,log2, ws.c[0].median[0],ws.c[0].median[1],ws.c[0].median[2]);
    if !is_mono {
        let log0b = read_i16(data, &mut ptr);
        let log1b = read_i16(data, &mut ptr);
        let log2b = read_i16(data, &mut ptr);
        ws.c[1].median[0] = wp_exp2s(log0b) as u32;
        ws.c[1].median[1] = wp_exp2s(log1b) as u32;
        ws.c[1].median[2] = wp_exp2s(log2b) as u32;
        wp_trace!("[pev] ch1 logs={},{},{} medians={},{},{}", log0b,log1b,log2b, ws.c[1].median[0],ws.c[1].median[1],ws.c[1].median[2]);
    }
}

pub fn parse_int32_info(data: &[u8]) -> Int32Info {
    if data.len() < 4 { return Int32Info::default(); }
    Int32Info { sent_bits: data[0], zeros: data[1], ones: data[2], dups: data[3] }
}

// ---------------------------------------------------------------------------
// Median helpers (INC/DEC/GET from wavpack_local.h)
// ---------------------------------------------------------------------------

#[inline(always)]
fn get_med(med: u32) -> u32 { (med >> 4) + 1 }

// Median update helpers: use wrapping arithmetic to match C's uint32_t behaviour.

#[inline(always)]
fn inc_med0(c: &mut EntropyChannel) {
    c.median[0] = c.median[0].wrapping_add(
        c.median[0].wrapping_add(DIV0) / DIV0 * 5,
    );
}
#[inline(always)]
fn dec_med0(c: &mut EntropyChannel) {
    c.median[0] = c.median[0].wrapping_sub(
        (c.median[0].wrapping_add(DIV0 - 2)) / DIV0 * 2,
    );
}
#[inline(always)]
fn inc_med1(c: &mut EntropyChannel) {
    c.median[1] = c.median[1].wrapping_add(
        c.median[1].wrapping_add(DIV1) / DIV1 * 5,
    );
}
#[inline(always)]
fn dec_med1(c: &mut EntropyChannel) {
    c.median[1] = c.median[1].wrapping_sub(
        (c.median[1].wrapping_add(DIV1 - 2)) / DIV1 * 2,
    );
}
#[inline(always)]
fn inc_med2(c: &mut EntropyChannel) {
    c.median[2] = c.median[2].wrapping_add(
        c.median[2].wrapping_add(DIV2) / DIV2 * 5,
    );
}
#[inline(always)]
fn dec_med2(c: &mut EntropyChannel) {
    c.median[2] = c.median[2].wrapping_sub(
        (c.median[2].wrapping_add(DIV2 - 2)) / DIV2 * 2,
    );
}

// ---------------------------------------------------------------------------
// count_ones: read 1-bits until 0 (or LIMIT_ONES), with extended range code
// Returns None on end-of-stream (33 ones or 33 cbits seen)
// ---------------------------------------------------------------------------

fn count_ones_lim(bs: &mut Bits<'_>) -> Option<u32> {
    let mut ones_count: u32 = 0;
    while ones_count <= LIMIT_ONES && bs.getbit() == 1 {
        ones_count += 1;
    }

    if ones_count >= LIMIT_ONES {
        if ones_count > LIMIT_ONES { return None; } // 17+ consecutive ones = EOS

        // Extended range coding for large values
        let mut cbits: u32 = 0;
        while cbits < 33 && bs.getbit() == 1 { cbits += 1; }
        if cbits == 33 { return None; }

        if cbits < 2 {
            ones_count = cbits;
        } else {
            let mut mask = 1u32;
            ones_count = 0;
            let mut remaining = cbits - 1;
            while remaining > 0 {
                remaining -= 1;
                if bs.getbit() == 1 { ones_count |= mask; }
                mask <<= 1;
            }
            ones_count |= mask;
        }

        ones_count += LIMIT_ONES;
    }

    Some(ones_count)
}

// ---------------------------------------------------------------------------
// get_words_lossless: entropy decode block_samples frames into buffer
// Returns interleaved samples: stereo → L0,R0,L1,R1,…; mono → S0,S1,…
// ---------------------------------------------------------------------------

pub fn get_words_lossless(
    bs:            &mut Bits<'_>,
    ws:            &mut WordsState,
    flags:         u32,
    block_samples: u32,
) -> Option<Vec<i32>> {
    let is_mono  = (flags & MONO_DATA) != 0;
    // The C reference doubles nsamples for stereo and iterates one sample at a time.
    let nsamples = if is_mono { block_samples } else { block_samples * 2 };
    let mut buffer = vec![0i32; nsamples as usize];
    let mut csamples: u32 = 0;

    wp_trace!("[gwl] nsamples={} is_mono={} med0={} med1={} med1b={} holding_one={} zeros_acc={}",
        nsamples, is_mono,
        ws.c[0].median[0], ws.c[0].median[1],
        ws.c[1].median[0],
        ws.holding_one, ws.zeros_acc);

    while csamples < nsamples {
        // Select the entropy channel: even→L(0), odd→R(1) for stereo.
        let chan = if is_mono { 0usize } else { (csamples & 1) as usize };

        // ---- holding_zero fast path (from previous iteration's split) ----
        if ws.holding_zero != 0 {
            ws.holding_zero = 0;
            let med0 = ws.c[chan].median[0];
            let low  = read_code(bs, get_med(med0).saturating_sub(1));
            dec_med0(&mut ws.c[chan]);
            let samp = if bs.getbit() != 0 { !(low as i32) } else { low as i32 };
            if csamples < 10 { wp_trace!("[gwl] cs={} holding_zero path low={} val={}", csamples, low, samp); }
            buffer[csamples as usize] = samp;
            csamples += 1;
            continue;
        }

        // ---- zero-run check (both medians near zero and not mid-run) ----
        let c0_med = ws.c[0].median[0];
        let c1_med = ws.c[1].median[0];
        if c0_med < 2 && ws.holding_one == 0 && (is_mono || c1_med < 2) {
            if ws.zeros_acc != 0 {
                // Still inside a zero run
                ws.zeros_acc -= 1;
                if ws.zeros_acc != 0 {
                    if csamples < 10 { wp_trace!("[gwl] cs={} zero-run emit (zeros_acc={})", csamples, ws.zeros_acc); }
                    buffer[csamples as usize] = 0;
                    csamples += 1;
                    continue;
                }
                // zeros_acc just hit 0 — fall through to normal decode
            } else {
                // Read a zero-run count from the bitstream
                let mut cbits: u32 = 0;
                while cbits < 33 && bs.getbit() == 1 { cbits += 1; }
                if cbits == 33 { wp_trace!("[gwl] cs={} EOS (cbits=33)", csamples); break; }

                if cbits < 2 {
                    ws.zeros_acc = cbits;
                } else {
                    let mut mask = 1u32;
                    ws.zeros_acc = 0;
                    let mut rem = cbits - 1;
                    while rem > 0 {
                        rem -= 1;
                        if bs.getbit() == 1 { ws.zeros_acc |= mask; }
                        mask <<= 1;
                    }
                    ws.zeros_acc |= mask;
                }

                wp_trace!("[gwl] cs={} zeros path cbits={} zeros_acc={}", csamples, cbits, ws.zeros_acc);

                if ws.zeros_acc != 0 {
                    // Reset both channels' medians then emit one zero sample
                    ws.c[0].median = [0; 3];
                    ws.c[1].median = [0; 3];
                    buffer[csamples as usize] = 0;
                    csamples += 1;
                    continue;
                }
                // zeros_acc == 0 → no run, fall through to decode a real sample
            }
        }

        // ---- count ones (with extended range for large values) ----
        let ones_raw = match count_ones_lim(bs) {
            Some(v) => v,
            None    => break,
        };

        // ---- holding_one / holding_zero state machine ----
        let low_hold        = ws.holding_one;
        ws.holding_one      = ones_raw & 1;
        ws.holding_zero     = (!(ones_raw) & 1) as i32;
        let ones_count      = (ones_raw >> 1) + low_hold;

        // ---- select median interval ----
        let c = &mut ws.c[chan];
        let (low, high): (u32, u32) = if ones_count == 0 {
            let h = get_med(c.median[0]).saturating_sub(1);
            dec_med0(c);
            (0, h)
        } else {
            let l = get_med(c.median[0]);
            inc_med0(c);
            if ones_count == 1 {
                let h = l.wrapping_add(get_med(c.median[1])).saturating_sub(1);
                dec_med1(c);
                (l, h)
            } else {
                let l2 = l.wrapping_add(get_med(c.median[1]));
                inc_med1(c);
                if ones_count == 2 {
                    let h = l2.wrapping_add(get_med(c.median[2])).saturating_sub(1);
                    dec_med2(c);
                    (l2, h)
                } else {
                    let add = (ones_count - 2).wrapping_mul(get_med(c.median[2]));
                    let l3  = l2.wrapping_add(add);
                    let h   = l3.wrapping_add(get_med(c.median[2])).saturating_sub(1);
                    inc_med2(c);
                    (l3, h)
                }
            }
        };

        let value = low.wrapping_add(read_code(bs, high.wrapping_sub(low)));
        let samp = if bs.getbit() != 0 { !(value as i32) } else { value as i32 };
        if csamples < 10 {
            wp_trace!("[gwl] cs={} ones_count={} low={} high={} value={} samp={}",
                csamples, ones_count, low, high, value, samp);
        }
        buffer[csamples as usize] = samp;
        csamples += 1;
    }

    wp_trace!("[gwl] done: decoded {} of {} samples", csamples, nsamples);
    Some(buffer)
}

// ---------------------------------------------------------------------------
// Weight helpers (wavpack_local.h)
// ---------------------------------------------------------------------------

#[inline(always)]
fn apply_weight(weight: i32, sample: i32) -> i32 {
    ((weight as i64 * sample as i64 + 512) >> 10) as i32
}

#[inline(always)]
fn update_weight(weight: &mut i32, delta: i32, source: i32, result: i32) {
    if source != 0 && result != 0 {
        let s = ((source ^ result) >> 31) as i32;
        *weight = (delta ^ s) + (*weight - s);
    }
}

#[inline(always)]
fn update_weight_clip(weight: &mut i32, delta: i32, source: i32, result: i32) {
    // Match the C macro in wavpack_local.h exactly:
    //   if (source && result) {
    //     const int32_t s = (source ^ result) >> 31;
    //     if ((weight = (weight ^ s) + (delta - s)) > 1024) weight = 1024;
    //     weight = (weight ^ s) - s;
    //   }
    if source != 0 && result != 0 {
        let s = (source ^ result) >> 31;
        let mut w = (*weight ^ s).wrapping_add(delta.wrapping_sub(s));
        if w > 1024 { w = 1024; }
        *weight = (w ^ s).wrapping_sub(s);
    }
}

// ---------------------------------------------------------------------------
// Decorrelation passes (unpack.c decorr_stereo_pass / decorr_mono_pass)
// ---------------------------------------------------------------------------

pub fn decorr_stereo_pass(p: &mut DecorrPass, buf: &mut [i32]) {
    let n = buf.len() / 2;
    let mut m = 0usize;

    match p.term {
        t if t > 0 && t <= MAX_TERM as i32 => {
            for i in 0..n {
                let sam_a = p.samples_a[m];
                let sam_b = p.samples_b[m];
                let k     = (m + t as usize) & (MAX_TERM - 1);

                let na = buf[i * 2    ].wrapping_add(apply_weight(p.weight_a, sam_a));
                let nb = buf[i * 2 + 1].wrapping_add(apply_weight(p.weight_b, sam_b));
                update_weight(&mut p.weight_a, p.delta, sam_a, buf[i * 2    ]);
                update_weight(&mut p.weight_b, p.delta, sam_b, buf[i * 2 + 1]);
                p.samples_a[k] = na; buf[i * 2    ] = na;
                p.samples_b[k] = nb; buf[i * 2 + 1] = nb;
                m = (m + 1) & (MAX_TERM - 1);
            }
        }
        17 => {
            for i in 0..n {
                let sa = (2i32).wrapping_mul(p.samples_a[0]).wrapping_sub(p.samples_a[1]);
                let sb = (2i32).wrapping_mul(p.samples_b[0]).wrapping_sub(p.samples_b[1]);
                p.samples_a[1] = p.samples_a[0];
                p.samples_b[1] = p.samples_b[0];

                let na = buf[i * 2    ].wrapping_add(apply_weight(p.weight_a, sa));
                let nb = buf[i * 2 + 1].wrapping_add(apply_weight(p.weight_b, sb));
                update_weight(&mut p.weight_a, p.delta, sa, buf[i * 2    ]);
                update_weight(&mut p.weight_b, p.delta, sb, buf[i * 2 + 1]);
                p.samples_a[0] = na; buf[i * 2    ] = na;
                p.samples_b[0] = nb; buf[i * 2 + 1] = nb;
            }
        }
        18 => {
            for i in 0..n {
                let sa = ((3i32).wrapping_mul(p.samples_a[0]).wrapping_sub(p.samples_a[1])) >> 1;
                let sb = ((3i32).wrapping_mul(p.samples_b[0]).wrapping_sub(p.samples_b[1])) >> 1;
                p.samples_a[1] = p.samples_a[0];
                p.samples_b[1] = p.samples_b[0];

                let na = buf[i * 2    ].wrapping_add(apply_weight(p.weight_a, sa));
                let nb = buf[i * 2 + 1].wrapping_add(apply_weight(p.weight_b, sb));
                update_weight(&mut p.weight_a, p.delta, sa, buf[i * 2    ]);
                update_weight(&mut p.weight_b, p.delta, sb, buf[i * 2 + 1]);
                p.samples_a[0] = na; buf[i * 2    ] = na;
                p.samples_b[0] = nb; buf[i * 2 + 1] = nb;
            }
        }
        -1 => {
            for i in 0..n {
                let sam = buf[i * 2].wrapping_add(apply_weight(p.weight_a, p.samples_a[0]));
                update_weight_clip(&mut p.weight_a, p.delta, p.samples_a[0], buf[i * 2]);
                buf[i * 2] = sam;
                p.samples_a[0] = buf[i * 2 + 1].wrapping_add(apply_weight(p.weight_b, sam));
                update_weight_clip(&mut p.weight_b, p.delta, sam, buf[i * 2 + 1]);
                buf[i * 2 + 1] = p.samples_a[0];
            }
        }
        -2 => {
            for i in 0..n {
                let sam = buf[i * 2 + 1].wrapping_add(apply_weight(p.weight_b, p.samples_b[0]));
                update_weight_clip(&mut p.weight_b, p.delta, p.samples_b[0], buf[i * 2 + 1]);
                buf[i * 2 + 1] = sam;
                p.samples_b[0] = buf[i * 2].wrapping_add(apply_weight(p.weight_a, sam));
                update_weight_clip(&mut p.weight_a, p.delta, sam, buf[i * 2]);
                buf[i * 2] = p.samples_b[0];
            }
        }
        -3 => {
            for i in 0..n {
                let sam_a = buf[i * 2    ].wrapping_add(apply_weight(p.weight_a, p.samples_a[0]));
                update_weight_clip(&mut p.weight_a, p.delta, p.samples_a[0], buf[i * 2]);
                p.samples_a[0] = buf[i * 2 + 1].wrapping_add(apply_weight(p.weight_b, p.samples_b[0]));
                update_weight_clip(&mut p.weight_b, p.delta, p.samples_b[0], buf[i * 2 + 1]);
                buf[i * 2    ] = sam_a;
                buf[i * 2 + 1] = p.samples_a[0];
                p.samples_b[0] = sam_a;
            }
        }
        _ => {}
    }
}

pub fn decorr_mono_pass(p: &mut DecorrPass, buf: &mut [i32]) {
    let mut m = 0usize;

    match p.term {
        t if t > 0 && t <= MAX_TERM as i32 => {
            for i in 0..buf.len() {
                let sam = p.samples_a[m];
                let k   = (m + t as usize) & (MAX_TERM - 1);
                let na  = buf[i].wrapping_add(apply_weight(p.weight_a, sam));
                update_weight(&mut p.weight_a, p.delta, sam, buf[i]);
                p.samples_a[k] = na;
                buf[i] = na;
                m = (m + 1) & (MAX_TERM - 1);
            }
        }
        17 => {
            for i in 0..buf.len() {
                let sa = (2i32).wrapping_mul(p.samples_a[0]).wrapping_sub(p.samples_a[1]);
                p.samples_a[1] = p.samples_a[0];
                let na = buf[i].wrapping_add(apply_weight(p.weight_a, sa));
                update_weight(&mut p.weight_a, p.delta, sa, buf[i]);
                p.samples_a[0] = na;
                buf[i] = na;
            }
        }
        18 => {
            for i in 0..buf.len() {
                let sa = ((3i32).wrapping_mul(p.samples_a[0]).wrapping_sub(p.samples_a[1])) >> 1;
                p.samples_a[1] = p.samples_a[0];
                let na = buf[i].wrapping_add(apply_weight(p.weight_a, sa));
                update_weight(&mut p.weight_a, p.delta, sa, buf[i]);
                p.samples_a[0] = na;
                buf[i] = na;
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// fixup_samples: apply shift, joint-stereo undo, int32 restoration
// ---------------------------------------------------------------------------

pub fn fixup_samples(buf: &mut [i32], flags: u32, i32info: &Int32Info) {
    let is_mono = (flags & MONO_DATA) != 0;

    // Joint stereo undo: bptr[0] += (bptr[1] -= (bptr[0] >> 1))
    if !is_mono && (flags & JOINT_STEREO) != 0 {
        let n = buf.len() / 2;
        for i in 0..n {
            let r_new = buf[i * 2 + 1] - (buf[i * 2] >> 1);
            buf[i * 2    ] += r_new;
            buf[i * 2 + 1] = r_new;
        }
    }

    // INT32_DATA: restore extra bits (for lossless 32-bit sources)
    if (flags & INT32_DATA) != 0 && i32info.sent_bits == 0 {
        let shift = i32info.zeros + i32info.ones + i32info.dups;
        if shift != 0 {
            for s in buf.iter_mut() {
                *s <<= shift;
                if i32info.ones != 0 {
                    *s |= ((1i32 << i32info.ones) - 1) << i32info.zeros;
                }
            }
        }
    }

    // SHIFT_MASK: left-shift to restore precision
    let shift = (flags & SHIFT_MASK) >> SHIFT_LSB;
    if shift != 0 {
        for s in buf.iter_mut() {
            *s <<= shift;
        }
    }
}

// ---------------------------------------------------------------------------
// Main entry point: decode one v4/v5 block
// ---------------------------------------------------------------------------

pub fn unpack_samples_v4v5(
    flags:         u32,
    block_samples: u32,
    passes:        &mut [DecorrPass],
    ws:            &mut WordsState,
    i32info:       &Int32Info,
    audio:         &[u8],
) -> Option<Vec<i32>> {
    if (flags & HYBRID_FLAG) != 0 {
        return None; // hybrid/lossy not supported
    }
    if (flags & FLOAT_DATA) != 0 {
        return None; // float not supported
    }

    let is_mono = (flags & MONO_DATA) != 0;
    let mut bs   = Bits::new(audio);

    let mut buf = get_words_lossless(&mut bs, ws, flags, block_samples)?;

    // Apply decorrelation passes in forward order
    for p in passes.iter_mut() {
        if is_mono {
            decorr_mono_pass(p, &mut buf);
        } else {
            decorr_stereo_pass(p, &mut buf);
        }
    }

    fixup_samples(&mut buf, flags, i32info);

    Some(buf)
}
