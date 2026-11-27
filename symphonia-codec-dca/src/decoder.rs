// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::audio::{
    AsGenericAudioBufferRef, Audio, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef,
    Position,
};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::well_known::CODEC_ID_DCA;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::io::BitReaderLtr;
use symphonia_core::packet::PacketRef;
use symphonia_core::support_audio_codec;

use crate::bits::Bits;
use crate::header::{FrameHeader, MAX_CHANNELS, MAX_SUBBANDS, to_big_endian_16};
use crate::huffman::books;
use crate::qmf::Qmf;
use crate::spec_tables::*;

const DCA_CODEC_INFO: CodecInfo =
    CodecInfo { short_name: "dca", long_name: "DTS Coherent Acoustics", profiles: &[] };

/// Scales the output of the filter banks to the range of a 24-bit sample.
const OUTPUT_SCALE: f32 = std::f32::consts::SQRT_2 / 65536.0;
const LFE_SCALE: f32 = 1.0 / 8388608.0;

/// The number of quantization levels and bits of a block code of ABITS 1 to 7.
const BLOCK_LEVELS: [u32; 7] = [3, 5, 7, 9, 13, 17, 25];
const BLOCK_BITS: [u32; 7] = [7, 10, 12, 13, 15, 17, 19];

const ADJUSTMENT: [f32; 4] = [1.0, 1.125, 1.25, 1.4375];

const MISSING_ADPCM_VQ: &str = "dca: the subband ADPCM prediction needs the ADPCM codebook of ETSI TS 102 114 annex D.10, which the standard omits; it exists in FFmpeg (dcadata.c, LGPL) and cannot be used here";
const MISSING_HF_VQ: &str = "dca: the high frequency vector quantization needs the codebook of ETSI TS 102 114 annex D.10, which the standard omits; it exists in FFmpeg (dcadata.c, LGPL) and cannot be used here";

const MAX_FRAME_SAMPLES: usize = 4096;
const HISTORY: usize = 4;
const SUBFRAME_SAMPLES: usize = 32;

#[derive(Default)]
struct CodingHeader {
    nsubfs: usize,
    nchannels: usize,
    subs: [usize; MAX_CHANNELS],
    vqsub: [usize; MAX_CHANNELS],
    joinx: [usize; MAX_CHANNELS],
    thuff: [usize; MAX_CHANNELS],
    shuff: [usize; MAX_CHANNELS],
    bhuff: [usize; MAX_CHANNELS],
    sel: [[usize; 10]; MAX_CHANNELS],
    adj: [[f32; 10]; MAX_CHANNELS],
}

/// Parses the audio coding header of the core, or of the channel extension when `ext_nsubfs` is given.
fn parse_coding_header(
    bs: &mut Bits<'_>,
    hdr: &FrameHeader,
    ext_nsubfs: Option<usize>,
) -> Result<CodingHeader> {
    let nsubfs = match ext_nsubfs {
        Some(n) => n,
        None => bs.u(4)? as usize + 1,
    };
    let mut h = CodingHeader { nsubfs, ..Default::default() };
    h.nchannels = bs.u(3)? as usize + 1;

    if h.nchannels > MAX_CHANNELS || (ext_nsubfs.is_none() && h.nchannels != hdr.num_primary()) {
        return decode_error("dca: invalid number of primary channels");
    }
    let n = h.nchannels;

    for ch in 0..n {
        h.subs[ch] = (bs.u(5)? as usize + 2).min(MAX_SUBBANDS);
    }
    for ch in 0..n {
        h.vqsub[ch] = bs.u(5)? as usize + 1;
    }
    for ch in 0..n {
        h.joinx[ch] = bs.u(3)? as usize;
        if h.joinx[ch] > if ext_nsubfs.is_some() { MAX_CHANNELS } else { n } {
            return decode_error("dca: invalid joint intensity source");
        }
    }
    for ch in 0..n {
        h.thuff[ch] = bs.u(2)? as usize;
    }
    for ch in 0..n {
        h.shuff[ch] = bs.u(3)? as usize;
        if h.shuff[ch] == 7 {
            return decode_error("dca: invalid scale factor code book");
        }
    }
    for ch in 0..n {
        h.bhuff[ch] = bs.u(3)? as usize;
        if h.bhuff[ch] == 7 {
            return decode_error("dca: invalid bit allocation code book");
        }
    }

    for (range, bits) in [(0..1, 1), (1..5, 2), (5..10, 3)] {
        for k in range {
            for ch in 0..n {
                h.sel[ch][k] = bs.u(bits)? as usize;
            }
        }
    }

    for ch in 0..n {
        h.adj[ch] = [1.0; 10];
    }
    for (range, limit) in [(0..1, 1), (1..5, 3), (5..10, 7)] {
        for k in range {
            for ch in 0..n {
                if h.sel[ch][k] < limit {
                    h.adj[ch][k] = ADJUSTMENT[bs.u(2)? as usize];
                }
            }
        }
    }

    if hdr.cpf {
        bs.skip(16)?;
    }

    Ok(h)
}

/// Finds the byte offset of the channel extension (XCh) sync word at the end of a frame.
fn find_xch(frame: &[u8]) -> Option<usize> {
    (0..frame.len().saturating_sub(6)).step_by(4).rev().find(|&at| {
        if frame[at..at + 4] != [0x5a; 4] {
            return false;
        }
        let size = (usize::from(frame[at + 4]) << 2) | usize::from(frame[at + 5] >> 6);
        let rest = frame.len() - at;
        size + 1 == rest || size == rest
    })
}

/// The index of the code book that does not use a Huffman code, for a bit allocation index.
fn plain_select(abits: usize) -> usize {
    match abits {
        1 => 1,
        2..=5 => 3,
        6..=10 => 7,
        _ => 0,
    }
}

/// The channels of the core, or of the channel extension, that are decoded together.
#[derive(Clone, Copy)]
struct ChannelSet<'a> {
    coding: &'a CodingHeader,
    positions: &'a [Position],
    /// The index of the first channel in the per-channel decoder state.
    base: usize,
    /// The state index of the first channel and the active subbands of the channels that joint
    /// intensity coding copies from.
    joint_source: (usize, &'a [usize]),
}

pub struct DcaDecoder {
    params: AudioCodecParameters,
    buf: AudioBuffer<f32>,
    qmf: Vec<Qmf>,
    lfe_history: [f32; 8],
    /// Subband samples of every channel with the last samples of the previous subframe in front.
    samples: Vec<[[f32; HISTORY + SUBFRAME_SAMPLES]; MAX_SUBBANDS]>,
}

impl DcaDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_DCA {
            return unsupported_error("dca: invalid codec");
        }

        let rate = params.sample_rate.unwrap_or(48000);
        let channels = params
            .channels
            .clone()
            .unwrap_or(symphonia_core::audio::layouts::CHANNEL_LAYOUT_STEREO);

        Ok(DcaDecoder {
            params: params.clone(),
            buf: AudioBuffer::new(AudioSpec::new(rate, channels), MAX_FRAME_SAMPLES),
            qmf: (0..2 * MAX_CHANNELS).map(|_| Qmf::new()).collect(),
            lfe_history: [0.0; 8],
            samples: vec![[[0.0; HISTORY + SUBFRAME_SAMPLES]; MAX_SUBBANDS]; 2 * MAX_CHANNELS],
        })
    }

    fn decode_inner(&mut self, data: &[u8]) -> Result<()> {
        self.buf.clear();

        let packed = to_big_endian_16(data, usize::MAX)?;
        let hdr = FrameHeader::parse(&packed)?;

        let xch = hdr.has_xch();
        let positions = hdr.positions()?;
        let channels = hdr.channels()?;

        if self.buf.spec().rate() != hdr.sample_rate || *self.buf.spec().channels() != channels {
            self.buf = AudioBuffer::new(
                AudioSpec::new(hdr.sample_rate, channels.clone()),
                MAX_FRAME_SAMPLES,
            );
            self.params.with_sample_rate(hdr.sample_rate).with_channels(channels.clone());
        }

        let mut planes: Vec<usize> = positions
            .iter()
            .map(|&p| channels.get_canonical_index_for_positioned_channel(p).unwrap_or(0))
            .collect();
        if hdr.lff > 0 {
            planes.push(
                channels.get_canonical_index_for_positioned_channel(Position::LFE1).unwrap_or(0),
            );
        }
        if xch {
            planes.push(
                channels
                    .get_canonical_index_for_positioned_channel(Position::REAR_CENTER)
                    .unwrap_or(0),
            );
        }

        let mut bs = Bits(BitReaderLtr::new(&packed));
        bs.skip(hdr.bits_used as u32)?;

        let coding = parse_coding_header(&mut bs, &hdr, None)?;

        if !hdr.hflag {
            for ch in &mut self.samples {
                for band in ch.iter_mut() {
                    band[..HISTORY].fill(0.0);
                }
            }
        }

        let nout = coding.nchannels + usize::from(hdr.lff > 0) + usize::from(xch);
        let mut pcm: Vec<Vec<f32>> = vec![Vec::with_capacity(hdr.samples_per_frame()); nout];

        let mut ext = None;
        if xch {
            let len = packed.len().min(hdr.frame_len);
            if let Some(at) = find_xch(&packed[..len]) {
                let mut xbs = Bits(BitReaderLtr::new(&packed[at..len]));
                xbs.skip(32 + 10 + 4)?;
                let xc = parse_coding_header(&mut xbs, &hdr, Some(coding.nsubfs))?;
                if xc.nchannels != 1 {
                    return unsupported_error("dca: unsupported number of extension channels");
                }
                ext = Some((xbs, xc));
            }
        }

        let xch_plane = nout - 1;
        let core = ChannelSet {
            coding: &coding,
            positions: &positions,
            base: 0,
            joint_source: (0, &coding.subs),
        };
        for _ in 0..coding.nsubfs {
            let nssc = self.decode_subframe(&mut bs, &hdr, &core, None, &mut pcm)?;

            if let Some((xbs, xc)) = ext.as_mut() {
                let set = ChannelSet {
                    coding: xc,
                    positions: &[Position::REAR_CENTER],
                    base: MAX_CHANNELS,
                    joint_source: (0, &coding.subs),
                };
                self.decode_subframe(xbs, &hdr, &set, Some(nssc), &mut pcm[xch_plane..])?;
            }
            else if xch {
                let len = pcm[xch_plane].len() + nssc * 256;
                pcm[xch_plane].resize(len, 0.0);
            }
        }

        if xch {
            // The core carries the centre surround channel in both surrounds, 3 dB down.
            let (core, rest) = pcm.split_at_mut(xch_plane);
            for side in [Position::SIDE_LEFT, Position::SIDE_RIGHT] {
                if let Some(i) = positions.iter().position(|&p| p == side) {
                    for (s, c) in core[i].iter_mut().zip(&rest[0]) {
                        *s -= std::f32::consts::FRAC_1_SQRT_2 * c;
                    }
                }
            }
        }

        let frames = pcm[0].len();
        self.buf.render_uninit(Some(frames));
        for (ch, &plane) in planes.iter().enumerate() {
            if let Some(dst) = self.buf.plane_mut(plane) {
                dst.copy_from_slice(&pcm[ch]);
            }
        }

        Ok(())
    }

    fn decode_subframe(
        &mut self,
        bs: &mut Bits<'_>,
        hdr: &FrameHeader,
        set: &ChannelSet<'_>,
        ext_nssc: Option<usize>,
        pcm: &mut [Vec<f32>],
    ) -> Result<usize> {
        let ChannelSet { coding, positions, base, joint_source } = *set;
        let books = books();
        let nch = coding.nchannels;
        let core = ext_nssc.is_none();

        let nssc = match ext_nssc {
            Some(n) => n,
            None => {
                let n = bs.u(2)? as usize + 1;
                bs.skip(3)?;
                n
            }
        };
        let nsamples = nssc * 8;

        let mut pmode = [[false; MAX_SUBBANDS]; MAX_CHANNELS];
        let mut pvq = [[0usize; MAX_SUBBANDS]; MAX_CHANNELS];
        for ch in 0..nch {
            for n in 0..coding.subs[ch] {
                pmode[ch][n] = bs.flag()?;
            }
        }
        for ch in 0..nch {
            for n in 0..coding.subs[ch] {
                if pmode[ch][n] {
                    pvq[ch][n] = bs.u(12)? as usize;
                }
            }
        }

        let mut abits = [[0usize; MAX_SUBBANDS]; MAX_CHANNELS];
        for ch in 0..nch {
            for n in 0..coding.vqsub[ch].min(coding.subs[ch]) {
                abits[ch][n] = match coding.bhuff[ch] {
                    0..=4 => books.bit_alloc[coding.bhuff[ch]].read(bs)? as usize,
                    5 => bs.u(4)? as usize,
                    _ => bs.u(5)? as usize,
                };
                if abits[ch][n] > 26 {
                    return decode_error("dca: invalid bit allocation");
                }
            }
        }

        let mut tmode = [[0usize; MAX_SUBBANDS]; MAX_CHANNELS];
        if nssc > 1 {
            for ch in 0..nch {
                for n in 0..coding.vqsub[ch].min(coding.subs[ch]) {
                    if abits[ch][n] > 0 {
                        tmode[ch][n] = books.transient[coding.thuff[ch]].read(bs)? as usize;
                    }
                }
            }
        }

        let mut scales = [[[0u32; 2]; MAX_SUBBANDS]; MAX_CHANNELS];
        for ch in 0..nch {
            let table: &[u32] = if coding.shuff[ch] == 6 { &SCALE_7BIT } else { &SCALE_6BIT };
            let mut sum = 0i32;
            let vq_start = coding.vqsub[ch].min(coding.subs[ch]);

            let read = |bs: &mut Bits<'_>, sum: &mut i32| -> Result<u32> {
                match coding.shuff[ch] {
                    0..=4 => *sum += i32::from(books.scale[coding.shuff[ch]].read(bs)?),
                    5 => *sum = bs.u(6)? as i32,
                    _ => *sum = bs.u(7)? as i32,
                }
                match table.get(usize::try_from(*sum).unwrap_or(usize::MAX)) {
                    Some(&v) if v > 0 => Ok(v),
                    _ => decode_error("dca: invalid scale factor"),
                }
            };

            for n in 0..vq_start {
                if abits[ch][n] > 0 {
                    scales[ch][n][0] = read(bs, &mut sum)?;
                    if tmode[ch][n] > 0 {
                        scales[ch][n][1] = read(bs, &mut sum)?;
                    }
                }
            }
            for n in vq_start..coding.subs[ch] {
                scales[ch][n][0] = read(bs, &mut sum)?;
            }
        }

        let mut join_shuff = [0usize; MAX_CHANNELS];
        for ch in 0..nch {
            if coding.joinx[ch] > 0 {
                join_shuff[ch] = bs.u(3)? as usize;
                if join_shuff[ch] == 7 {
                    return decode_error("dca: invalid joint scale factor code book");
                }
            }
        }
        let mut join_scales = [[0f32; MAX_SUBBANDS]; MAX_CHANNELS];
        for ch in 0..nch {
            if coding.joinx[ch] == 0 {
                continue;
            }
            let src = coding.joinx[ch] - 1;
            for n in coding.subs[ch]..joint_source.1[src] {
                let v = match join_shuff[ch] {
                    0..=4 => i32::from(books.scale[join_shuff[ch]].read(bs)?),
                    5 => bs.u(6)? as i32,
                    _ => bs.u(7)? as i32,
                };
                match JOIN_SCALE.get(usize::try_from(v + 64).unwrap_or(usize::MAX)) {
                    Some(&s) => join_scales[ch][n] = s,
                    None => return decode_error("dca: invalid joint scale factor"),
                }
            }
        }

        if hdr.dynf && core {
            bs.skip(8)?;
        }
        if hdr.cpf {
            bs.skip(16)?;
        }

        for ch in 0..nch {
            for band in self.samples[base + ch].iter_mut() {
                band[HISTORY..].fill(0.0);
            }
        }

        for ch in 0..nch {
            if coding.vqsub[ch].min(coding.subs[ch]) < coding.subs[ch] {
                return unsupported_error(MISSING_HF_VQ);
            }
        }

        let nlfe = if core && hdr.lff > 0 { 2 * hdr.lff as usize * nssc } else { 0 };
        let mut lfe = [0f32; 32];
        if nlfe > 0 {
            let mut raw = [0i32; 32];
            for r in raw.iter_mut().take(nlfe) {
                *r = bs.s(8)?;
            }
            let index = bs.u(8)? as usize;
            let scale = match SCALE_7BIT.get(index) {
                Some(&s) if s > 0 => s as f32 * 0.035,
                _ => return decode_error("dca: invalid low frequency effects scale"),
            };
            for (l, &r) in lfe.iter_mut().zip(&raw).take(nlfe) {
                *l = r as f32 * scale;
            }
        }

        let step_table = if hdr.rate == 0x1f { &STEP_LOSSLESS } else { &STEP_LOSSY };

        for ss in 0..nssc {
            for ch in 0..nch {
                for n in 0..coding.vqsub[ch].min(coding.subs[ch]) {
                    let ab = abits[ch][n];
                    let mut audio = [0i32; 8];

                    if ab > 0 {
                        let sel = if ab > 10 { 0 } else { coding.sel[ch][ab - 1] };

                        if sel != plain_select(ab) {
                            let Some(tree) = &books.audio[ab - 1][sel]
                            else {
                                return decode_error("dca: invalid quantization code book");
                            };
                            for a in &mut audio {
                                *a = i32::from(tree.read(bs)?);
                            }
                        }
                        else if ab <= 7 {
                            let levels = BLOCK_LEVELS[ab - 1];
                            let offset = ((levels - 1) / 2) as i32;
                            for half in 0..2 {
                                let mut code = bs.u(BLOCK_BITS[ab - 1])?;
                                for k in 0..4 {
                                    audio[half * 4 + k] = (code % levels) as i32 - offset;
                                    code /= levels;
                                }
                            }
                        }
                        else {
                            for a in &mut audio {
                                *a = bs.s(ab as u32 - 3)?;
                            }
                        }
                    }

                    let step = step_table[ab] as f32 / 4194304.0;
                    let first = if tmode[ch][n] == 0 { nssc } else { tmode[ch][n] };
                    let scale = scales[ch][n][usize::from(ss >= first)] as f32;
                    let adj = if ab > 0 && ab <= 10 { coding.adj[ch][ab - 1] } else { 1.0 };
                    let factor = step * scale * adj;

                    let band = &mut self.samples[base + ch][n];
                    for (m, &a) in audio.iter().enumerate() {
                        band[HISTORY + ss * 8 + m] = factor * a as f32;
                    }

                    if pmode[ch][n] {
                        return unsupported_error(MISSING_ADPCM_VQ);
                    }
                }
            }

            if (ss == nssc - 1 || hdr.aspf) && bs.u(16)? != 0xffff {
                return decode_error("dca: dsync mismatch");
            }
        }

        // The prediction of the next subframe continues from the samples before any stereo decoding.
        let mut history = [[[0f32; HISTORY]; MAX_SUBBANDS]; MAX_CHANNELS];
        for ch in 0..nch {
            for n in 0..MAX_SUBBANDS {
                history[ch][n]
                    .copy_from_slice(&self.samples[base + ch][n][nsamples..nsamples + HISTORY]);
            }
        }

        // Joint intensity coding copies the subbands of the source channel.
        let mut active = [0usize; MAX_CHANNELS];
        active[..nch].copy_from_slice(&coding.subs[..nch]);
        for ch in 0..nch {
            if coding.joinx[ch] == 0 {
                continue;
            }
            let src = coding.joinx[ch] - 1;
            for n in coding.subs[ch]..joint_source.1[src] {
                for m in 0..nsamples {
                    self.samples[base + ch][n][HISTORY + m] =
                        join_scales[ch][n] * self.samples[joint_source.0 + src][n][HISTORY + m];
                }
            }
            active[ch] = active[ch].max(joint_source.1[src]);
        }

        let pair = |a: Position, b: Position| -> Option<(usize, usize)> {
            Some((positions.iter().position(|&p| p == a)?, positions.iter().position(|&p| p == b)?))
        };
        if hdr.sumf || hdr.amode == 3 {
            if let Some((l, r)) = pair(Position::FRONT_LEFT, Position::FRONT_RIGHT) {
                self.sum_difference(base + l, base + r, nsamples);
                active[l] = active[l].max(active[r]);
                active[r] = active[l];
            }
        }
        if hdr.sums {
            if let Some((l, r)) = pair(Position::SIDE_LEFT, Position::SIDE_RIGHT) {
                self.sum_difference(base + l, base + r, nsamples);
                active[l] = active[l].max(active[r]);
                active[r] = active[l];
            }
        }

        let coeff = if hdr.filts { &QMF_PERFECT } else { &QMF_NON_PERFECT };
        for ch in 0..nch {
            let mut out = vec![0f32; nsamples * 32];
            for m in 0..nsamples {
                let mut input = [0f32; 32];
                for n in 0..active[ch] {
                    input[n] = self.samples[base + ch][n][HISTORY + m];
                }
                self.qmf[base + ch].synthesize(&input, coeff, &mut out[m * 32..(m + 1) * 32]);
            }
            pcm[ch].extend(out.iter().map(|&v| v * OUTPUT_SCALE));
        }

        if nlfe > 0 {
            let decimation = if hdr.lff == 1 { 128 } else { 64 };
            let taps = 512 / decimation;
            let table = if hdr.lff == 1 { &LFE_FIR_128 } else { &LFE_FIR_64 };
            let mut input = [0f32; 8 + 32];
            input[..8].copy_from_slice(&self.lfe_history);
            input[8..8 + nlfe].copy_from_slice(&lfe[..nlfe]);

            let out = &mut pcm[nch];
            for i in 0..nlfe {
                for k in 0..decimation {
                    let mut acc = 0f32;
                    for j in 0..taps {
                        acc += input[8 + i - j] * table[k + j * decimation];
                    }
                    out.push(acc * LFE_SCALE);
                }
            }
            self.lfe_history.copy_from_slice(&input[nlfe..nlfe + 8]);
        }

        for ch in 0..nch {
            for (band, saved) in self.samples[base + ch].iter_mut().zip(&history[ch]) {
                band[..HISTORY].copy_from_slice(saved);
            }
        }

        Ok(nssc)
    }

    fn sum_difference(&mut self, l: usize, r: usize, nsamples: usize) {
        for n in 0..MAX_SUBBANDS {
            for m in 0..nsamples {
                let (a, b) = (self.samples[l][n][HISTORY + m], self.samples[r][n][HISTORY + m]);
                self.samples[l][n][HISTORY + m] = a + b;
                self.samples[r][n][HISTORY + m] = a - b;
            }
        }
    }
}

impl AudioDecoder for DcaDecoder {
    fn reset(&mut self) {
        for q in &mut self.qmf {
            q.reset();
        }
        self.lfe_history = [0.0; 8];
        self.samples.fill([[0.0; HISTORY + SUBFRAME_SAMPLES]; MAX_SUBBANDS]);
    }

    fn codec_info(&self) -> &CodecInfo {
        &DCA_CODEC_INFO
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

impl RegisterableAudioDecoder for DcaDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(DcaDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(CODEC_ID_DCA, "dca", "DTS Coherent Acoustics")]
    }
}
