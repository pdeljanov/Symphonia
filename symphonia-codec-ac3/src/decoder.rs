// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::audio::{
    AsGenericAudioBufferRef, Audio, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef,
};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::well_known::{CODEC_ID_AC3, CODEC_ID_EAC3};
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::io::BitReaderLtr;
use symphonia_core::packet::PacketRef;
use symphonia_core::support_audio_codec;

use crate::aht::{self, AHT_BLOCKS};
use crate::bitalloc::{BitAllocParams, ChannelAlloc, DeltaSegment, compute_bap};
use crate::bits::Bits;
use crate::eac3::{Eac3Frame, parse_audfrm};
use crate::frame::{FrameHeader, SAMPLES_PER_BLOCK, SAMPLES_PER_FRAME, SYNC_WORD, verify_crc};
use crate::imdct::Imdct;
use crate::tables::{DEF_CPL_BNDSTRC, DEF_SPX_BNDSTRC, SPX_ATTEN_TAB};

const AC3_CODEC_INFO: CodecInfo =
    CodecInfo { short_name: "ac3", long_name: "Dolby Digital (AC-3)", profiles: &[] };

/// The index of the coupling channel in the per-channel state arrays.
const CPL: usize = 5;
/// The index of the low frequency effects channel in the per-channel state arrays.
const LFE: usize = 6;
const NUM_CHANNEL_SLOTS: usize = 7;
const MAX_BINS: usize = 256;
const MAX_CPL_SUBBANDS: usize = 18;
const LFE_BINS: usize = 7;
/// The first bin of the first spectral extension sub-band.
const SPX_FIRST_BIN: usize = 25;

/// The number of mantissa bits of the asymmetrically quantized baps 6 to 15.
const ASYM_BITS: [u32; 10] = [5, 6, 7, 8, 9, 10, 11, 12, 14, 16];

/// The start bins of the rematrixing bands.
const REMAT_BAND_START: [usize; 5] = [13, 25, 37, 61, 253];

/// The unquantized coefficient scale factor of every exponent, `2^-exp`.
fn exponent_scale(exp: u8) -> f32 {
    f32::from_bits((127 - u32::from(exp)) << 23)
}

/// Reads the grouped mantissas of baps 1, 2, and 4, which are shared by all channels of a block.
#[derive(Default)]
struct GroupedMantissas {
    bap1: [u8; 3],
    bap1_left: usize,
    bap2: [u8; 3],
    bap2_left: usize,
    bap4: [u8; 2],
    bap4_left: usize,
}

impl GroupedMantissas {
    fn read(&mut self, bs: &mut Bits<'_>, bap: u8) -> Result<f32> {
        Ok(match bap {
            1 => {
                if self.bap1_left == 0 {
                    let g = bs.u(5)? as u8;
                    if g >= 27 {
                        return decode_error("ac3: invalid mantissa group");
                    }
                    self.bap1 = [g / 9, (g % 9) / 3, g % 3];
                    self.bap1_left = 3;
                }
                let code = self.bap1[3 - self.bap1_left];
                self.bap1_left -= 1;
                (f32::from(code) - 1.0) * (2.0 / 3.0)
            }
            2 => {
                if self.bap2_left == 0 {
                    let g = bs.u(7)? as u8;
                    if g >= 125 {
                        return decode_error("ac3: invalid mantissa group");
                    }
                    self.bap2 = [g / 25, (g % 25) / 5, g % 5];
                    self.bap2_left = 3;
                }
                let code = self.bap2[3 - self.bap2_left];
                self.bap2_left -= 1;
                (f32::from(code) - 2.0) * (2.0 / 5.0)
            }
            3 => {
                let code = bs.u(3)?;
                if code >= 7 {
                    return decode_error("ac3: invalid mantissa");
                }
                (code as f32 - 3.0) * (2.0 / 7.0)
            }
            4 => {
                if self.bap4_left == 0 {
                    let g = bs.u(7)? as u8;
                    if g >= 121 {
                        return decode_error("ac3: invalid mantissa group");
                    }
                    self.bap4 = [g / 11, g % 11];
                    self.bap4_left = 2;
                }
                let code = self.bap4[2 - self.bap4_left];
                self.bap4_left -= 1;
                (f32::from(code) - 5.0) * (2.0 / 11.0)
            }
            5 => {
                let code = bs.u(4)?;
                if code >= 15 {
                    return decode_error("ac3: invalid mantissa");
                }
                (code as f32 - 7.0) * (2.0 / 15.0)
            }
            _ => {
                let bits = ASYM_BITS[usize::from(bap) - 6];
                let raw = bs.u(bits)? as i32;
                let signed = (raw << (32 - bits)) >> (32 - bits);
                signed as f32 / (1u32 << (bits - 1)) as f32
            }
        })
    }
}

/// Decodes the grouped exponents of a channel into `dst`, which receives one value per bin.
fn decode_exponents(
    bs: &mut Bits<'_>,
    strategy: usize,
    mut prev: i32,
    ngrps: usize,
    dst: &mut [u8],
) -> Result<()> {
    let grpsize = 1 << (strategy - 1);

    if ngrps * 3 * grpsize > dst.len() {
        return decode_error("ac3: too many exponents");
    }

    let mut pos = 0;
    for _ in 0..ngrps {
        let gexp = bs.u(7)? as i32;
        if gexp >= 125 {
            return decode_error("ac3: invalid exponent group");
        }
        for dexp in [gexp / 25, (gexp % 25) / 5, gexp % 5] {
            prev += dexp - 2;
            if !(0..=24).contains(&prev) {
                return decode_error("ac3: exponent out of range");
            }
            dst[pos..pos + grpsize].fill(prev as u8);
            pos += grpsize;
        }
    }
    Ok(())
}

/// The index of a channel's delta bit allocation state, the coupling channel being last.
fn delta_index(slot: usize) -> usize {
    if slot == CPL { 5 } else { slot }
}

/// The state of a syncframe that persists between audio blocks.
struct FrameState {
    dynrng: [u8; 2],

    cplinu: bool,
    chincpl: [bool; 5],
    phsflginu: bool,
    cplbegf: usize,
    cplendf: i32,
    spxinu: bool,
    chinspx: [bool; 5],
    spx_strtf: usize,
    spx_begf: usize,
    spx_begin_subbnd: usize,
    spx_end_subbnd: usize,
    spxbndstrc: [bool; 18],
    spxco: [[f32; 18]; 5],
    spx_nblend: [[f32; 18]; 5],
    spx_sblend: [[f32; 18]; 5],
    first_spx_coords: [bool; 5],
    spx_atten: [Option<u8>; 5],
    cplbndstrc: [bool; MAX_CPL_SUBBANDS],
    cplbndstrc_known: bool,
    first_cpl_coords: [bool; 5],
    first_cpl_leak: bool,
    cplco: [[f32; MAX_CPL_SUBBANDS]; 5],
    phsflg: [bool; MAX_CPL_SUBBANDS],

    rematflg: [bool; 4],

    expstr: [usize; NUM_CHANNEL_SLOTS],
    chbwcod: [usize; 5],
    exps: [[u8; MAX_BINS]; NUM_CHANNEL_SLOTS],
    bap: [[u8; MAX_BINS]; NUM_CHANNEL_SLOTS],

    alloc: BitAllocParams,
    csnroffst: usize,
    fsnroffst: [usize; NUM_CHANNEL_SLOTS],
    fgaincod: [usize; NUM_CHANNEL_SLOTS],
    cplfleak: i32,
    cplsleak: i32,

    deltbae: [usize; 6],
    delta: [Vec<DeltaSegment>; 6],

    blksw: [bool; 5],
    dithflag: [bool; 5],

    coef: [[f32; MAX_BINS]; NUM_CHANNEL_SLOTS],

    aht_in_use: [bool; NUM_CHANNEL_SLOTS],
    aht_ready: [bool; NUM_CHANNEL_SLOTS],
    aht: Vec<[[f32; MAX_BINS]; AHT_BLOCKS]>,
}

impl FrameState {
    fn new() -> Self {
        FrameState {
            dynrng: [0; 2],
            cplinu: false,
            chincpl: [false; 5],
            phsflginu: false,
            cplbegf: 0,
            cplendf: 0,
            spxinu: false,
            chinspx: [false; 5],
            spx_strtf: 0,
            spx_begf: 0,
            spx_begin_subbnd: 0,
            spx_end_subbnd: 0,
            spxbndstrc: DEF_SPX_BNDSTRC,
            spxco: [[0.0; 18]; 5],
            spx_nblend: [[0.0; 18]; 5],
            spx_sblend: [[0.0; 18]; 5],
            first_spx_coords: [true; 5],
            spx_atten: [None; 5],
            cplbndstrc: [false; MAX_CPL_SUBBANDS],
            cplbndstrc_known: false,
            first_cpl_coords: [true; 5],
            first_cpl_leak: true,
            cplco: [[0.0; MAX_CPL_SUBBANDS]; 5],
            phsflg: [false; MAX_CPL_SUBBANDS],
            rematflg: [false; 4],
            expstr: [0; NUM_CHANNEL_SLOTS],
            chbwcod: [0; 5],
            exps: [[0; MAX_BINS]; NUM_CHANNEL_SLOTS],
            bap: [[0; MAX_BINS]; NUM_CHANNEL_SLOTS],
            alloc: BitAllocParams::default(),
            csnroffst: 0,
            fsnroffst: [0; NUM_CHANNEL_SLOTS],
            fgaincod: [0; NUM_CHANNEL_SLOTS],
            cplfleak: 0,
            cplsleak: 0,
            deltbae: [2; 6],
            delta: Default::default(),
            blksw: [false; 5],
            dithflag: [false; 5],
            coef: [[0.0; MAX_BINS]; NUM_CHANNEL_SLOTS],
            aht_in_use: [false; NUM_CHANNEL_SLOTS],
            aht_ready: [false; NUM_CHANNEL_SLOTS],
            aht: Vec::new(),
        }
    }

    fn cpl_start(&self) -> usize {
        self.cplbegf * 12 + 37
    }

    fn cpl_end(&self) -> usize {
        ((self.cplendf + 3) * 12 + 37) as usize
    }

    fn num_remat_bands(&self) -> usize {
        if self.cplinu {
            match self.cplbegf {
                0 => 2,
                1 | 2 => 3,
                _ => 4,
            }
        }
        else if self.spxinu && self.spx_begf < 2 {
            3
        }
        else {
            4
        }
    }

    fn spx_start(&self) -> usize {
        SPX_FIRST_BIN + 12 * self.spx_begin_subbnd
    }

    fn spx_end(&self) -> usize {
        SPX_FIRST_BIN + 12 * self.spx_end_subbnd
    }

    /// The sizes of the spectral extension bands.
    fn spx_band_sizes(&self) -> Vec<usize> {
        let mut sizes = vec![12];
        for &merged in &self.spxbndstrc[self.spx_begin_subbnd + 1..self.spx_end_subbnd] {
            match sizes.last_mut() {
                Some(last) if merged => *last += 12,
                _ => sizes.push(12),
            }
        }
        sizes
    }

    fn channel_end(&self, ch: usize) -> usize {
        if self.chincpl[ch] {
            self.cpl_start()
        }
        else if self.spxinu && self.chinspx[ch] {
            self.spx_start()
        }
        else {
            (self.chbwcod[ch] + 12) * 3 + 37
        }
    }

    /// The coupling bands as `(first bin, end bin)`.
    fn cpl_bands(&self) -> Vec<(usize, usize)> {
        let nsub = (3 + self.cplendf - self.cplbegf as i32) as usize;
        let mut bands: Vec<(usize, usize)> = Vec::with_capacity(nsub);
        for sbnd in 0..nsub {
            let lo = self.cpl_start() + sbnd * 12;
            match bands.last_mut() {
                Some(last) if self.cplbndstrc[sbnd] => last.1 = lo + 12,
                _ => bands.push((lo, lo + 12)),
            }
        }
        bands
    }
}

/// An AC-3 (Dolby Digital) audio decoder.
pub struct Ac3Decoder {
    params: AudioCodecParameters,
    buf: AudioBuffer<f32>,
    imdct: Imdct,
    delay: [[f32; SAMPLES_PER_BLOCK]; 6],
    rng: u32,
    nodither: bool,
    verify: bool,
}

impl Ac3Decoder {
    pub fn try_new(params: &AudioCodecParameters, opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_AC3 && params.codec != CODEC_ID_EAC3 {
            return unsupported_error("ac3: invalid codec");
        }

        let rate = params.sample_rate.unwrap_or(48000);
        let channels = params
            .channels
            .clone()
            .unwrap_or(symphonia_core::audio::layouts::CHANNEL_LAYOUT_STEREO);

        Ok(Ac3Decoder {
            params: params.clone(),
            buf: AudioBuffer::new(AudioSpec::new(rate, channels), SAMPLES_PER_FRAME),
            imdct: Imdct::new(),
            delay: [[0.0; SAMPLES_PER_BLOCK]; 6],
            rng: 1,
            verify: opts.verify,
            nodither: std::env::var_os("AC3_NODITHER").is_some(),
        })
    }

    /// Zero-mean, unit-variance noise for spectral extension.
    fn noise(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        if self.nodither { 0.0 } else { (self.rng as i32 as f32) * (3f32.sqrt() / 2147483648.0) }
    }

    /// Synthesizes the high frequency coefficients of a channel from its low frequency ones.
    fn synthesize_spx(&mut self, st: &mut FrameState, ch: usize) {
        let (begin, end) = (st.spx_start(), st.spx_end());
        let copy_start = SPX_FIRST_BIN + 12 * st.spx_strtf;
        let sizes = st.spx_band_sizes();

        // Translate the low frequency coefficients to the high frequency region.
        let mut wrap = vec![false; sizes.len()];
        {
            let tc = &mut st.coef[ch];
            let (mut copy, mut insert) = (copy_start, begin);
            for (bnd, &size) in sizes.iter().enumerate() {
                if copy + size > begin {
                    copy = copy_start;
                    wrap[bnd] = true;
                }
                for _ in 0..size {
                    if copy == begin {
                        copy = copy_start;
                    }
                    tc[insert] = tc[copy];
                    insert += 1;
                    copy += 1;
                }
            }
        }

        let mut starts = Vec::with_capacity(sizes.len());
        let mut start = begin;
        for &size in &sizes {
            starts.push(start);
            start += size;
        }
        debug_assert_eq!(start, end);

        let rms: Vec<f32> = starts
            .iter()
            .zip(&sizes)
            .map(|(&start, &size)| {
                let energy: f32 = st.coef[ch][start..start + size].iter().map(|c| c * c).sum();
                (energy / size as f32).sqrt()
            })
            .collect();

        // Attenuate the coefficients around the borders where the translation wraps.
        if let Some(code) = st.spx_atten[ch] {
            let table = &SPX_ATTEN_TAB[usize::from(code)];
            let weights = [table[0], table[1], table[2], table[1], table[0]];
            for (bnd, &start) in starts.iter().enumerate() {
                if bnd == 0 || wrap[bnd] {
                    for (k, &w) in weights.iter().enumerate() {
                        st.coef[ch][start - 2 + k] *= w;
                    }
                }
            }
        }

        // Blend with noise and scale by the spectral extension coordinates.
        for (bnd, (&start, &size)) in starts.iter().zip(&sizes).enumerate() {
            let nscale = rms[bnd] * st.spx_nblend[ch][bnd];
            let sscale = st.spx_sblend[ch][bnd];
            let gain = st.spxco[ch][bnd] * 32.0;
            for coef in &mut st.coef[ch][start..start + size] {
                let noise = self.noise();
                *coef = (*coef * sscale + noise * nscale) * gain;
            }
        }
    }

    fn dither(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        if self.nodither { 0.0 } else { (self.rng as i32 as f32) * (0.707 / 2147483648.0) }
    }

    /// Finds the syncframe to decode in a packet: the first AC-3 frame or the first E-AC-3 frame
    /// of independent substream 0. Dependent substreams are skipped.
    fn find_frame(data: &[u8]) -> Result<(&[u8], FrameHeader)> {
        let mut pos = 0;

        while let Some(offset) =
            data[pos..].windows(2).position(|w| u16::from_be_bytes([w[0], w[1]]) == SYNC_WORD)
        {
            let frame = &data[pos + offset..];
            let hdr = FrameHeader::parse(frame)?;

            if frame.len() < hdr.frame_len {
                return decode_error("ac3: frame is truncated");
            }

            match &hdr.eac3 {
                Some(e) if e.strmtyp == 1 || e.substreamid != 0 => {
                    pos += offset + hdr.frame_len;
                }
                _ => return Ok((&frame[..hdr.frame_len], hdr)),
            }
        }

        decode_error("ac3: no frame in packet")
    }

    fn decode_inner(&mut self, data: &[u8]) -> Result<()> {
        self.buf.clear();

        let (data, hdr) = Self::find_frame(data)?;

        if self.verify && !verify_crc(data, hdr.eac3.is_some()) {
            return decode_error("ac3: crc mismatch");
        }

        let positions = hdr.channel_positions();
        let channels = hdr.channels();

        if self.buf.spec().rate() != hdr.sample_rate || *self.buf.spec().channels() != channels {
            self.buf = AudioBuffer::new(
                AudioSpec::new(hdr.sample_rate, channels.clone()),
                SAMPLES_PER_FRAME,
            );
            self.params.with_sample_rate(hdr.sample_rate).with_channels(channels.clone());
        }

        let plane_of: Vec<usize> = positions
            .iter()
            .map(|&p| channels.get_canonical_index_for_positioned_channel(p).unwrap_or(0))
            .collect();

        let mut bs = Bits(BitReaderLtr::new(data));
        bs.skip(hdr.bits_used as u32)?;

        let frame = if hdr.eac3.is_some() { Some(parse_audfrm(&mut bs, &hdr)?) } else { None };

        let mut st = FrameState::new();
        if let Some(f) = &frame {
            st.aht_in_use[..5].copy_from_slice(&f.chahtinu);
            st.aht_in_use[CPL] = f.cplahtinu;
            st.aht_in_use[LFE] = f.lfeahtinu;
            st.spx_atten = f.spx_atten;
            if st.aht_in_use.iter().any(|&a| a) {
                st.aht = vec![[[0.0; MAX_BINS]; AHT_BLOCKS]; NUM_CHANNEL_SLOTS];
            }
        }

        let num_samples = hdr.num_blocks * SAMPLES_PER_BLOCK;
        let mut pcm = vec![vec![0f32; num_samples]; hdr.num_channels()];

        for blk in 0..hdr.num_blocks {
            self.decode_block(&mut bs, &hdr, &mut st, frame.as_ref(), blk)?;

            for ch in 0..hdr.num_channels() {
                let slot = if ch < hdr.nfchans { ch } else { LFE };
                let short = slot < 5 && st.blksw[slot];
                let mut windowed = [0f32; 512];
                let mut coef = [0f32; MAX_BINS];
                coef.copy_from_slice(&st.coef[slot]);
                if short {
                    self.imdct.short(&coef, &mut windowed);
                }
                else {
                    self.imdct.long(&coef, &mut windowed);
                }
                let out = &mut pcm[ch][blk * SAMPLES_PER_BLOCK..(blk + 1) * SAMPLES_PER_BLOCK];
                for n in 0..SAMPLES_PER_BLOCK {
                    out[n] = 2.0 * (windowed[n] + self.delay[ch][n]);
                    self.delay[ch][n] = windowed[SAMPLES_PER_BLOCK + n];
                }
            }
        }

        self.buf.render_uninit(Some(num_samples));
        for (ch, &plane) in plane_of.iter().enumerate() {
            if let Some(dst) = self.buf.plane_mut(plane) {
                dst.copy_from_slice(&pcm[ch]);
            }
        }

        Ok(())
    }

    fn decode_block(
        &mut self,
        bs: &mut Bits<'_>,
        hdr: &FrameHeader,
        st: &mut FrameState,
        frame: Option<&Eac3Frame>,
        blk: usize,
    ) -> Result<()> {
        let nfchans = hdr.nfchans;
        let acmod = hdr.acmod;
        // Every E-AC-3 block recomputes its bit allocation, which is cheap and always correct.
        let mut realloc = blk == 0 || frame.is_some();

        let (blkswe, dithflage) = frame.map_or((true, true), |f| (f.blkswe, f.dithflage));
        for flag in st.blksw.iter_mut().take(nfchans) {
            *flag = blkswe && bs.flag()?;
        }
        for flag in st.dithflag.iter_mut().take(nfchans) {
            *flag = !dithflage || bs.flag()?;
        }

        if bs.flag()? {
            st.dynrng[0] = bs.u(8)? as u8;
        }
        else if blk == 0 {
            st.dynrng[0] = 0;
        }
        if acmod == 0 {
            if bs.flag()? {
                st.dynrng[1] = bs.u(8)? as u8;
            }
            else if blk == 0 {
                st.dynrng[1] = 0;
            }
        }

        if frame.is_some() && (blk == 0 || bs.flag()?) {
            st.spxinu = bs.flag()?;
            if st.spxinu {
                if acmod == 1 {
                    st.chinspx[0] = true;
                }
                else {
                    for flag in st.chinspx.iter_mut().take(nfchans) {
                        *flag = bs.flag()?;
                    }
                }
                st.spx_strtf = bs.u(2)? as usize;
                st.spx_begf = bs.u(3)? as usize;
                let endf = bs.u(3)? as usize;
                st.spx_begin_subbnd =
                    if st.spx_begf < 6 { st.spx_begf + 2 } else { st.spx_begf * 2 - 3 };
                st.spx_end_subbnd = if endf < 3 { endf + 5 } else { endf * 2 + 3 };
                if st.spx_end_subbnd <= st.spx_begin_subbnd
                    || SPX_FIRST_BIN + 12 * st.spx_strtf >= st.spx_start()
                {
                    return decode_error("ac3: invalid spectral extension band range");
                }
                if bs.flag()? {
                    for sbnd in st.spx_begin_subbnd + 1..st.spx_end_subbnd {
                        st.spxbndstrc[sbnd] = bs.flag()?;
                    }
                }
            }
            else {
                st.chinspx = [false; 5];
                st.first_spx_coords = [true; 5];
            }
        }

        if st.spxinu {
            let sizes = st.spx_band_sizes();
            for ch in 0..nfchans {
                if !st.chinspx[ch] {
                    st.first_spx_coords[ch] = true;
                    continue;
                }
                let spxcoe = if st.first_spx_coords[ch] {
                    st.first_spx_coords[ch] = false;
                    true
                }
                else {
                    bs.flag()?
                };
                if spxcoe {
                    let blend = bs.u(5)?;
                    let master = bs.u(2)?;
                    let noffset = blend as f32 / 32.0;
                    let mut start = st.spx_start();
                    for (bnd, &size) in sizes.iter().enumerate() {
                        let exp = bs.u(4)?;
                        let mant = bs.u(2)?;
                        let m = if exp == 15 { mant as f32 / 4.0 } else { (mant + 4) as f32 / 8.0 };
                        st.spxco[ch][bnd] = m * 0.5f32.powi((exp + 3 * master) as i32);

                        let nratio = (((start as f32 + 0.5 * size as f32) / st.spx_end() as f32)
                            - noffset)
                            .clamp(0.0, 1.0);
                        st.spx_nblend[ch][bnd] = nratio.sqrt();
                        st.spx_sblend[ch][bnd] = (1.0 - nratio).sqrt();
                        start += size;
                    }
                }
            }
        }

        // Coupling strategy.
        let cplstre = frame.map_or_else(|| bs.flag(), |f| Ok(f.cplstre[blk]))?;
        if cplstre {
            realloc = true;
            st.cplinu = frame.map_or_else(|| bs.flag(), |f| Ok(f.cplinu[blk]))?;
            st.chincpl = [false; 5];
            if st.cplinu {
                if frame.is_some() && bs.flag()? {
                    return unsupported_error("ac3: enhanced coupling is not supported");
                }
                if frame.is_some() && acmod == 2 {
                    st.chincpl[0] = true;
                    st.chincpl[1] = true;
                }
                else {
                    for flag in st.chincpl.iter_mut().take(nfchans) {
                        *flag = bs.flag()?;
                    }
                }
                if acmod == 2 {
                    st.phsflginu = bs.flag()?;
                }
                st.cplbegf = bs.u(4)? as usize;
                st.cplendf = if st.spxinu {
                    if st.spx_begf < 6 {
                        st.spx_begf as i32 - 2
                    }
                    else {
                        st.spx_begf as i32 * 2 - 7
                    }
                }
                else {
                    bs.u(4)? as i32
                };
                if st.cplendf + 3 <= st.cplbegf as i32 {
                    return decode_error("ac3: invalid coupling band range");
                }
                let nsub = (3 + st.cplendf - st.cplbegf as i32) as usize;
                if frame.is_none() || bs.flag()? {
                    st.cplbndstrc = [false; MAX_CPL_SUBBANDS];
                    for flag in st.cplbndstrc.iter_mut().take(nsub).skip(1) {
                        *flag = bs.flag()?;
                    }
                    st.cplbndstrc_known = true;
                }
                else if !st.cplbndstrc_known {
                    st.cplbndstrc = [false; MAX_CPL_SUBBANDS];
                    for bnd in 1..nsub {
                        st.cplbndstrc[bnd] = DEF_CPL_BNDSTRC[st.cplbegf + bnd];
                    }
                    st.cplbndstrc_known = true;
                }
            }
            else {
                st.first_cpl_coords = [true; 5];
                st.first_cpl_leak = true;
                st.phsflginu = false;
                st.cplbndstrc_known = false;
            }
        }
        else if blk == 0 && frame.is_none() {
            return decode_error("ac3: coupling strategy missing in the first block");
        }

        // Coupling coordinates.
        if st.cplinu {
            let bands = st.cpl_bands();
            let mut any_coords = false;
            for ch in 0..nfchans {
                if !st.chincpl[ch] {
                    st.first_cpl_coords[ch] = true;
                    continue;
                }
                let cplcoe = if frame.is_some() && st.first_cpl_coords[ch] {
                    st.first_cpl_coords[ch] = false;
                    true
                }
                else {
                    bs.flag()?
                };
                if cplcoe {
                    any_coords = any_coords || ch < 2;
                    let master = bs.u(2)?;
                    for co in st.cplco[ch].iter_mut().take(bands.len()) {
                        let exp = bs.u(4)?;
                        let mant = bs.u(4)?;
                        let m =
                            if exp == 15 { mant as f32 / 16.0 } else { (mant + 16) as f32 / 32.0 };
                        *co = m * 0.5f32.powi((exp + 3 * master) as i32);
                    }
                }
            }
            if acmod == 2 && st.phsflginu && any_coords {
                for flag in st.phsflg.iter_mut().take(bands.len()) {
                    *flag = bs.flag()?;
                }
            }
        }

        // Rematrixing.
        if acmod == 2 && ((frame.is_some() && blk == 0) || bs.flag()?) {
            let nbands = st.num_remat_bands();
            st.rematflg = [false; 4];
            for flag in st.rematflg.iter_mut().take(nbands) {
                *flag = bs.flag()?;
            }
        }

        // Exponent strategies.
        let mut new_exps = [false; NUM_CHANNEL_SLOTS];
        if let Some(f) = frame {
            if st.cplinu {
                st.expstr[CPL] = usize::from(f.cplexpstr[blk]);
            }
            for ch in 0..nfchans {
                st.expstr[ch] = usize::from(f.chexpstr[blk][ch]);
            }
            if hdr.lfeon {
                st.expstr[LFE] = usize::from(f.lfeexpstr[blk]);
            }
        }
        else {
            if st.cplinu {
                st.expstr[CPL] = bs.u(2)? as usize;
            }
            for e in st.expstr.iter_mut().take(nfchans) {
                *e = bs.u(2)? as usize;
            }
            if hdr.lfeon {
                st.expstr[LFE] = bs.u(1)? as usize;
            }
        }
        new_exps[CPL] = st.cplinu && st.expstr[CPL] != 0;
        for ch in 0..nfchans {
            new_exps[ch] = st.expstr[ch] != 0;
        }
        new_exps[LFE] = hdr.lfeon && st.expstr[LFE] != 0;

        if blk == 0 && (0..nfchans).any(|ch| !new_exps[ch]) {
            return decode_error("ac3: exponents are reused in the first block");
        }

        for ch in 0..nfchans {
            if new_exps[ch] && !st.chincpl[ch] && !(st.spxinu && st.chinspx[ch]) {
                st.chbwcod[ch] = bs.u(6)? as usize;
                if st.chbwcod[ch] > 60 {
                    return decode_error("ac3: invalid channel bandwidth");
                }
            }
        }

        // Exponents.
        if new_exps[CPL] {
            let (start, end) = (st.cpl_start(), st.cpl_end());
            let strategy = st.expstr[CPL];
            let abs = (bs.u(4)? << 1) as i32;
            let ngrps = (end - start) / (3 << (strategy - 1));
            decode_exponents(bs, strategy, abs, ngrps, &mut st.exps[CPL][start..])?;
        }
        for ch in 0..nfchans {
            if !new_exps[ch] {
                continue;
            }
            let end = st.channel_end(ch);
            let strategy = st.expstr[ch];
            let abs = bs.u(4)? as i32;
            st.exps[ch][0] = abs as u8;
            let ngrps = (end - 1 + [0, 0, 3, 9][strategy]) / (3 << (strategy - 1));
            let (_, rest) = st.exps[ch].split_at_mut(1);
            decode_exponents(bs, strategy, abs, ngrps, rest)?;
            bs.skip(2)?;
        }
        if new_exps[LFE] {
            let abs = bs.u(4)? as i32;
            st.exps[LFE][0] = abs as u8;
            let (_, rest) = st.exps[LFE].split_at_mut(1);
            decode_exponents(bs, 1, abs, 2, rest)?;
        }
        realloc |= new_exps.iter().any(|&n| n);

        // Bit allocation parameters.
        match frame {
            Some(f) if !f.bamode => {
                st.alloc =
                    BitAllocParams { sdcycod: 2, fdcycod: 1, sgaincod: 1, dbpbcod: 2, floorcod: 7 };
            }
            _ => {
                if bs.flag()? {
                    realloc = true;
                    st.alloc = BitAllocParams {
                        sdcycod: bs.u(2)? as usize,
                        fdcycod: bs.u(2)? as usize,
                        sgaincod: bs.u(2)? as usize,
                        dbpbcod: bs.u(2)? as usize,
                        floorcod: bs.u(3)? as usize,
                    };
                }
                else if blk == 0 && frame.is_none() {
                    return decode_error(
                        "ac3: bit allocation parameters missing in the first block",
                    );
                }
            }
        }

        if let Some(f) = frame {
            if f.snroffststr == 0 {
                st.csnroffst = f.frmcsnroffst;
                st.fsnroffst = [f.frmfsnroffst; NUM_CHANNEL_SLOTS];
            }
            else if blk == 0 || bs.flag()? {
                realloc = true;
                st.csnroffst = bs.u(6)? as usize;
                if f.snroffststr == 1 {
                    st.fsnroffst = [bs.u(4)? as usize; NUM_CHANNEL_SLOTS];
                }
                else {
                    if st.cplinu {
                        st.fsnroffst[CPL] = bs.u(4)? as usize;
                    }
                    for f in st.fsnroffst.iter_mut().take(nfchans) {
                        *f = bs.u(4)? as usize;
                    }
                    if hdr.lfeon {
                        st.fsnroffst[LFE] = bs.u(4)? as usize;
                    }
                }
            }

            if f.frmfgaincode && bs.flag()? {
                if st.cplinu {
                    st.fgaincod[CPL] = bs.u(3)? as usize;
                }
                for g in st.fgaincod.iter_mut().take(nfchans) {
                    *g = bs.u(3)? as usize;
                }
                if hdr.lfeon {
                    st.fgaincod[LFE] = bs.u(3)? as usize;
                }
            }
            else {
                st.fgaincod = [4; NUM_CHANNEL_SLOTS];
            }

            if f.strmtyp == 0 {
                // convsnroffste, convsnroffst
                if bs.flag()? {
                    bs.skip(10)?;
                }
            }
        }
        else {
            if bs.flag()? {
                realloc = true;
                st.csnroffst = bs.u(6)? as usize;
                if st.cplinu {
                    st.fsnroffst[CPL] = bs.u(4)? as usize;
                    st.fgaincod[CPL] = bs.u(3)? as usize;
                }
                for ch in 0..nfchans {
                    st.fsnroffst[ch] = bs.u(4)? as usize;
                    st.fgaincod[ch] = bs.u(3)? as usize;
                }
                if hdr.lfeon {
                    st.fsnroffst[LFE] = bs.u(4)? as usize;
                    st.fgaincod[LFE] = bs.u(3)? as usize;
                }
            }
            else if blk == 0 {
                return decode_error("ac3: SNR offsets missing in the first block");
            }
        }

        if st.cplinu {
            let cplleake = if frame.is_some() && st.first_cpl_leak {
                st.first_cpl_leak = false;
                true
            }
            else {
                bs.flag()?
            };
            if cplleake {
                realloc = true;
                st.cplfleak = bs.u(3)? as i32;
                st.cplsleak = bs.u(3)? as i32;
            }
        }

        // Delta bit allocation.
        if frame.is_none_or(|f| f.dbaflde) && bs.flag()? {
            realloc = true;
            let slots: Vec<usize> = if st.cplinu {
                std::iter::once(CPL).chain(0..nfchans).collect()
            }
            else {
                (0..nfchans).collect()
            };
            for &slot in &slots {
                st.deltbae[delta_index(slot)] = bs.u(2)? as usize;
            }
            for &slot in &slots {
                let idx = delta_index(slot);
                if st.deltbae[idx] == 1 {
                    let nseg = bs.u(3)? as usize + 1;
                    st.delta[idx].clear();
                    for _ in 0..nseg {
                        let offset = bs.u(5)? as usize;
                        let len = bs.u(4)? as usize;
                        let ba = bs.u(3)? as usize;
                        st.delta[idx].push(DeltaSegment { offset, len, ba });
                    }
                }
            }
        }

        // Skipped data.
        if frame.is_none_or(|f| f.skipflde) && bs.flag()? {
            let skipl = bs.u(9)?;
            bs.skip(skipl * 8)?;
        }

        if realloc {
            self.allocate_bits(hdr, st);
        }

        self.read_mantissas(bs, hdr, st, blk)?;
        self.reconstruct(hdr, st);

        Ok(())
    }

    fn allocate_bits(&mut self, hdr: &FrameHeader, st: &mut FrameState) {
        let nfchans = hdr.nfchans;
        let all_zero = st.csnroffst == 0
            && (0..nfchans).all(|ch| st.fsnroffst[ch] == 0)
            && (!st.cplinu || st.fsnroffst[CPL] == 0)
            && (!hdr.lfeon || st.fsnroffst[LFE] == 0);

        let snroffset = |fsnr: usize| ((st.csnroffst as i32 - 15) * 16 + fsnr as i32) * 4;

        if all_zero {
            st.bap = [[0; MAX_BINS]; NUM_CHANNEL_SLOTS];
            return;
        }

        let alloc = st.alloc;
        let no_delta: [DeltaSegment; 0] = [];

        for ch in 0..nfchans {
            let delta: &[DeltaSegment] =
                if st.deltbae[ch] == 1 || st.deltbae[ch] == 0 { &st.delta[ch] } else { &no_delta };
            let challoc = ChannelAlloc {
                exps: &st.exps[ch],
                start: 0,
                end: st.channel_end(ch),
                fscod: hdr.fscod,
                fgaincod: st.fgaincod[ch],
                snroffset: snroffset(st.fsnroffst[ch]),
                leak: None,
                delta,
                aht: st.aht_in_use[ch],
            };
            let mut bap = [0u8; MAX_BINS];
            compute_bap(&alloc, &challoc, &mut bap);
            st.bap[ch] = bap;
        }

        if st.cplinu {
            let delta: &[DeltaSegment] =
                if st.deltbae[5] == 1 || st.deltbae[5] == 0 { &st.delta[5] } else { &no_delta };
            let challoc = ChannelAlloc {
                exps: &st.exps[CPL],
                start: st.cpl_start(),
                end: st.cpl_end(),
                fscod: hdr.fscod,
                fgaincod: st.fgaincod[CPL],
                snroffset: snroffset(st.fsnroffst[CPL]),
                leak: Some(((st.cplfleak << 8) + 768, (st.cplsleak << 8) + 768)),
                delta,
                aht: st.aht_in_use[CPL],
            };
            let mut bap = [0u8; MAX_BINS];
            compute_bap(&alloc, &challoc, &mut bap);
            st.bap[CPL] = bap;
        }

        if hdr.lfeon {
            let challoc = ChannelAlloc {
                exps: &st.exps[LFE],
                start: 0,
                end: LFE_BINS,
                fscod: hdr.fscod,
                fgaincod: st.fgaincod[LFE],
                snroffset: snroffset(st.fsnroffst[LFE]),
                leak: None,
                delta: &no_delta,
                aht: st.aht_in_use[LFE],
            };
            let mut bap = [0u8; MAX_BINS];
            compute_bap(&alloc, &challoc, &mut bap);
            st.bap[LFE] = bap;
        }
    }

    /// Reads the adaptive hybrid transform mantissas of a channel once per frame.
    fn read_aht(
        &mut self,
        bs: &mut Bits<'_>,
        st: &mut FrameState,
        slot: usize,
        range: std::ops::Range<usize>,
    ) -> Result<()> {
        if !st.aht_ready[slot] {
            st.aht_ready[slot] = true;
            let dithered = slot < 5 && st.dithflag[slot];
            let mut noise = || self.dither();
            aht::read_channel(
                bs,
                &st.bap[slot],
                &st.exps[slot],
                range,
                exponent_scale,
                if dithered { Some(&mut noise) } else { None },
                &mut st.aht[slot],
            )?;
        }
        Ok(())
    }

    fn read_mantissas(
        &mut self,
        bs: &mut Bits<'_>,
        hdr: &FrameHeader,
        st: &mut FrameState,
        blk: usize,
    ) -> Result<()> {
        let mut groups = GroupedMantissas::default();
        let mut cpl_coef = [0f32; MAX_BINS];
        let mut cpl_dither_bins = [false; MAX_BINS];
        let mut got_cpl = false;

        for slot in st.coef.iter_mut() {
            slot.fill(0.0);
        }

        for ch in 0..hdr.nfchans {
            let end = st.channel_end(ch);
            if st.aht_in_use[ch] {
                self.read_aht(bs, st, ch, 0..end)?;
                st.coef[ch][..end].copy_from_slice(&st.aht[ch][blk][..end]);
            }
            for bin in 0..end {
                if st.aht_in_use[ch] {
                    break;
                }
                let bap = st.bap[ch][bin];
                st.coef[ch][bin] = if bap == 0 {
                    if st.dithflag[ch] {
                        self.dither() * exponent_scale(st.exps[ch][bin])
                    }
                    else {
                        0.0
                    }
                }
                else {
                    groups.read(bs, bap)? * exponent_scale(st.exps[ch][bin])
                };
            }

            if st.cplinu && st.chincpl[ch] && !got_cpl {
                got_cpl = true;
                if st.aht_in_use[CPL] {
                    let range = st.cpl_start()..st.cpl_end();
                    self.read_aht(bs, st, CPL, range.clone())?;
                    cpl_coef[range.clone()].copy_from_slice(&st.aht[CPL][blk][range]);
                }
                for bin in st.cpl_start()..st.cpl_end() {
                    if st.aht_in_use[CPL] {
                        break;
                    }
                    let bap = st.bap[CPL][bin];
                    if bap == 0 {
                        cpl_dither_bins[bin] = true;
                    }
                    else {
                        cpl_coef[bin] = groups.read(bs, bap)? * exponent_scale(st.exps[CPL][bin]);
                    }
                }
            }
        }

        if hdr.lfeon && st.aht_in_use[LFE] {
            self.read_aht(bs, st, LFE, 0..LFE_BINS)?;
            st.coef[LFE][..LFE_BINS].copy_from_slice(&st.aht[LFE][blk][..LFE_BINS]);
        }
        if hdr.lfeon && !st.aht_in_use[LFE] {
            for bin in 0..LFE_BINS {
                let bap = st.bap[LFE][bin];
                if bap != 0 {
                    st.coef[LFE][bin] = groups.read(bs, bap)? * exponent_scale(st.exps[LFE][bin]);
                }
            }
        }

        // Decouple the channels.
        if st.cplinu {
            let bands = st.cpl_bands();
            for ch in 0..hdr.nfchans {
                if !st.chincpl[ch] {
                    continue;
                }
                for (bnd, &(lo, hi)) in bands.iter().enumerate() {
                    let mut gain = st.cplco[ch][bnd] * 8.0;
                    if ch == 1 && hdr.acmod == 2 && st.phsflginu && st.phsflg[bnd] {
                        gain = -gain;
                    }
                    for bin in lo..hi.min(MAX_BINS) {
                        let c = if cpl_dither_bins[bin] {
                            if st.dithflag[ch] {
                                self.dither() * exponent_scale(st.exps[CPL][bin])
                            }
                            else {
                                0.0
                            }
                        }
                        else {
                            cpl_coef[bin]
                        };
                        st.coef[ch][bin] = c * gain;
                    }
                }
            }
        }

        Ok(())
    }

    /// Applies rematrixing and dynamic range control to the coefficients of a block.
    fn reconstruct(&mut self, hdr: &FrameHeader, st: &mut FrameState) {
        if hdr.acmod == 2 {
            let nbands = st.num_remat_bands();
            let limit = if st.cplinu { st.cpl_start() } else { REMAT_BAND_START[4] };
            let (left, right) = st.coef.split_at_mut(1);
            for band in 0..nbands {
                if !st.rematflg[band] {
                    continue;
                }
                let lo = REMAT_BAND_START[band];
                let hi =
                    if band + 1 == nbands { limit } else { REMAT_BAND_START[band + 1] }.min(limit);
                for bin in lo..hi {
                    let (l, r) = (left[0][bin], right[0][bin]);
                    left[0][bin] = l + r;
                    right[0][bin] = l - r;
                }
            }
        }

        if st.spxinu {
            for ch in 0..hdr.nfchans {
                if st.chinspx[ch] {
                    self.synthesize_spx(st, ch);
                }
            }
        }

        for ch in 0..hdr.nfchans {
            let code = st.dynrng[if hdr.acmod == 0 { ch } else { 0 }];
            let gain = dynrng_gain(code);
            if gain != 1.0 {
                st.coef[ch].iter_mut().for_each(|c| *c *= gain);
            }
        }
        if hdr.lfeon {
            let gain = dynrng_gain(st.dynrng[0]);
            if gain != 1.0 {
                st.coef[LFE].iter_mut().for_each(|c| *c *= gain);
            }
        }
    }
}

/// The linear gain of a `dynrng` word.
fn dynrng_gain(code: u8) -> f32 {
    let x = i32::from((code as i8) >> 5);
    let y = f32::from(code & 0x1f);
    2f32.powi(x) * (1.0 + y / 32.0)
}

impl AudioDecoder for Ac3Decoder {
    fn reset(&mut self) {
        self.delay = [[0.0; SAMPLES_PER_BLOCK]; 6];
    }

    fn codec_info(&self) -> &CodecInfo {
        &AC3_CODEC_INFO
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        if let Err(err) = self.decode_inner(packet.data) {
            self.buf.clear();
            Err(err)
        }
        else {
            Ok(self.buf.as_generic_audio_buffer_ref())
        }
    }

    fn finalize(&mut self) -> FinalizeResult {
        Default::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for Ac3Decoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(Ac3Decoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[
            support_audio_codec!(CODEC_ID_AC3, "ac3", "Dolby Digital (AC-3)"),
            support_audio_codec!(CODEC_ID_EAC3, "eac3", "Dolby Digital Plus (E-AC-3)"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::verify_crc;

    /// A mono 32 kbit/s frame of a 1 kHz tone, encoded by an independent encoder.
    ///
    /// The reference samples include the reference decoder's dither noise, hence the tolerance.
    const TONE_FRAME: &str = concat!(
        "0b77523700402f842b01cf7bb0fdf4332fe76c1b9c9d1cd152fb6ead37431baf639f6dd5a25ac3aa8eee22d4654711a7",
        "0123f2a382665103f480344f1fc91752be7cf9f3e86f9f3e7cf9f3ea6f8040fbcaff346f2336c803000541d38615baca",
        "402001643598b78d5d8a0081f795fe68de466d9006000a83a70c2b7594002660",
    );

    /// A mono E-AC-3 frame of six blocks of a 1 kHz tone, encoded by an independent encoder.
    const TONE_EAC3_FRAME: &str = concat!(
        "0b77007f3287c00021054820e7bdd87efa1be7d5df06865f2272416ef4a584b6ec9b99b39dd2190c5d2bad9b4e5d9754",
        "7488d264529770ec4cd1357112b5c144e2ad168aa3dac044ef14a36cb3d12cb3657249b7dc1e8b96035ed232dbda624c",
        "b8501eb79541aabad8153eed112f2f111f7019fee215df2411bf660daf67098fa9097fea10fd200d13c7f245d4af9f3e",
        "7cfa1be7cf9f3e7cfa9be7cf9f3e7cf9f3e7cf9f3e7d0a9be0d8b3401fadc917c799e3378f339eab9ca18d6080201d4e",
        "c36a700be2b3eac83b58c364223f811e3e9a2cb95b8c615cf55be106c59a00fd6e48be3ccf19bc799cf55ce50c6b0401",
        "00ea761b53805f159f5641dac600c797",
    );

    #[test]
    fn verify_decoded_eac3_tone_matches_reference_decoder() {
        let frame: Vec<u8> = (0..TONE_EAC3_FRAME.len() / 2)
            .map(|i| u8::from_str_radix(&TONE_EAC3_FRAME[2 * i..2 * i + 2], 16).unwrap())
            .collect();
        assert!(verify_crc(&frame, true));

        let mut dec = make_decoder();
        dec.decode_inner(&frame).unwrap();

        let expected = [(100, -5.5e-05), (700, 0.300153), (1000, 0.000124), (1500, -0.149982)];
        let plane = dec.buf.plane(0).unwrap();
        assert_eq!(plane.len(), 1536);
        for (i, v) in expected {
            assert!((plane[i] - v).abs() < 2e-2, "sample {i}: {} != {v}", plane[i]);
        }
    }

    fn tone_frame() -> Vec<u8> {
        (0..TONE_FRAME.len() / 2)
            .map(|i| u8::from_str_radix(&TONE_FRAME[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    fn make_decoder() -> Ac3Decoder {
        let mut params = AudioCodecParameters::new();
        params.for_codec(CODEC_ID_AC3);
        Ac3Decoder::try_new(&params, &AudioDecoderOptions::default()).unwrap()
    }

    #[test]
    fn verify_decoded_tone_matches_reference_decoder() {
        let mut dec = make_decoder();
        dec.decode_inner(&tone_frame()).unwrap();

        let expected = [(100, -0.00036), (700, 0.299533), (1000, -0.001029), (1500, -0.139593)];
        let plane = dec.buf.plane(0).unwrap();
        for (i, v) in expected {
            assert!((plane[i] - v).abs() < 2e-2, "sample {i}: {} != {v}", plane[i]);
        }
    }

    #[test]
    fn verify_crc_detects_corruption() {
        let mut frame = tone_frame();
        assert!(verify_crc(&frame, false));
        frame[100] ^= 0x04;
        assert!(!verify_crc(&frame, false));
    }

    #[test]
    fn verify_truncated_and_garbage_frames_are_rejected() {
        let frame = tone_frame();
        let mut dec = make_decoder();
        assert!(dec.decode_inner(&frame[..100]).is_err());
        assert!(dec.decode_inner(&[0u8; 64]).is_err());
    }

    #[test]
    fn verify_dynrng_gain() {
        assert_eq!(dynrng_gain(0), 1.0);
        assert_eq!(dynrng_gain(0b0010_0000), 2.0);
        assert_eq!(dynrng_gain(0b1110_0000), 0.5);
    }
}
