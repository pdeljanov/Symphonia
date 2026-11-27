// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef,
};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::well_known::CODEC_ID_NELLYMOSER;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::packet::PacketRef;
use symphonia_core::support_audio_codec;

use crate::imdct::{Imdct, N};
use crate::tables::{
    NELLY_BAND_SIZES_TABLE, NELLY_DELTA_TABLE, NELLY_DEQUANTIZATION_TABLE, NELLY_INIT_TABLE,
};

const NELLYMOSER_CODEC_INFO: CodecInfo =
    CodecInfo { short_name: "nellymoser", long_name: "Nellymoser Asao", profiles: &[] };

/// The size of a block in bytes.
const BLOCK_BYTES: usize = 64;
/// The number of samples a block decodes to.
const BLOCK_SAMPLES: usize = 2 * N;
/// The most blocks a packet may hold.
const MAX_BLOCKS: usize = 64;
const HEADER_BITS: usize = 116;
/// The number of bits of the mantissas of one transform.
const DETAIL_BITS: i32 = 198;
/// The number of coefficients that carry mantissas.
const FILL_LEN: usize = 124;
const BIT_CAP: i32 = 6;
const BASE_OFF: i32 = 4228;
const BASE_SHIFT: i32 = 19;
/// Converts the output of the transform to the range of -1 to 1.
const OUTPUT_SCALE: f32 = 1.0 / 32768.0;
const RNG_SEED: u32 = 0x2545_f491;

/// A reader of little endian bit fields, the first bit being the least significant bit of the
/// first byte.
struct BitsLe<'a> {
    data: &'a [u8],
    pos: usize,
}

impl BitsLe<'_> {
    fn read(&mut self, n: usize) -> u32 {
        let mut value = 0;
        for i in 0..n {
            let byte = self.data.get(self.pos >> 3).copied().unwrap_or(0);
            let bit = (byte >> (self.pos & 7)) & 1;
            value |= u32::from(bit) << i;
            self.pos += 1;
        }
        value
    }
}

fn signed_shift(v: i32, shift: i32) -> i32 {
    if shift > 0 { v.wrapping_shl(shift as u32) } else { v >> (-shift).min(31) }
}

/// Normalizes `v` to use the full range of an `i32`, and returns the number of bits it was
/// shifted by.
fn headroom(v: &mut i32) -> i32 {
    if *v == 0 {
        return 31;
    }
    let l = v.unsigned_abs().leading_zeros() as i32 - 1;
    *v = v.wrapping_shl(l as u32);
    l
}

fn sum_bits(buf: &[i16; N], shift: i32, off: i32) -> i32 {
    buf[..FILL_LEN]
        .iter()
        .map(|&b| {
            let b = i32::from(b) - i32::from(off as i16);
            (((b >> (shift - 1)) + 1) >> 1).clamp(0, BIT_CAP)
        })
        .sum()
}

/// Derives the number of mantissa bits of every coefficient from the band exponents.
fn allocate_bits(exponents: &[f32; N]) -> [i32; N] {
    let mut max = exponents.iter().fold(0, |a, &b| a.max(b as i32));
    let mut shift = headroom(&mut max) - 16;

    let mut sbuf = [0i16; N];
    for i in 0..FILL_LEN {
        sbuf[i] = signed_shift(exponents[i] as i32, shift) as i16;
        sbuf[i] = ((3 * i32::from(sbuf[i])) >> 2) as i16;
    }

    let mut sum: i32 = sbuf.iter().map(|&s| i32::from(s)).sum();

    shift += 11;
    let shift_saved = shift;
    sum -= DETAIL_BITS << shift;
    shift += headroom(&mut sum);
    let mut small_off = (BASE_OFF * (sum >> 16)) >> 15;
    shift = shift_saved - (BASE_SHIFT + shift - 31);
    small_off = signed_shift(small_off, shift);

    let mut bitsum = sum_bits(&sbuf, shift_saved, small_off);
    if bitsum != DETAIL_BITS {
        let mut off = bitsum - DETAIL_BITS;
        shift = 0;
        while off.abs() <= 16383 {
            off *= 2;
            shift += 1;
        }

        off = (off * BASE_OFF) >> 15;
        shift = shift_saved - (BASE_SHIFT + shift - 15);
        off = signed_shift(off, shift);

        let mut last_off = small_off;
        let mut last_bitsum = bitsum;
        let mut last_j = 0;
        for j in 1..20 {
            last_off = small_off;
            small_off += off;
            last_bitsum = bitsum;
            last_j = j;

            bitsum = sum_bits(&sbuf, shift_saved, small_off);

            if (bitsum - DETAIL_BITS) * (last_bitsum - DETAIL_BITS) <= 0 {
                break;
            }
        }

        let (mut big_off, mut big_bitsum, mut small_bitsum);
        if bitsum > DETAIL_BITS {
            big_off = small_off;
            small_off = last_off;
            big_bitsum = bitsum;
            small_bitsum = last_bitsum;
        }
        else {
            big_off = last_off;
            big_bitsum = last_bitsum;
            small_bitsum = bitsum;
        }

        while bitsum != DETAIL_BITS && last_j <= 19 {
            off = (big_off + small_off) >> 1;
            bitsum = sum_bits(&sbuf, shift_saved, off);
            if bitsum > DETAIL_BITS {
                big_off = off;
                big_bitsum = bitsum;
            }
            else {
                small_off = off;
                small_bitsum = bitsum;
            }
            last_j += 1;
        }

        if (big_bitsum - DETAIL_BITS).abs() >= (small_bitsum - DETAIL_BITS).abs() {
            bitsum = small_bitsum;
        }
        else {
            small_off = big_off;
            bitsum = big_bitsum;
        }
    }

    let mut bits = [0i32; N];
    for i in 0..FILL_LEN {
        let tmp = i32::from(sbuf[i]) - small_off;
        bits[i] = (((tmp >> (shift_saved - 1)) + 1) >> 1).clamp(0, BIT_CAP);
    }

    if bitsum > DETAIL_BITS {
        let mut i = 0;
        let mut total = 0;
        while total < DETAIL_BITS {
            total += bits[i];
            i += 1;
        }

        bits[i - 1] -= total - DETAIL_BITS;
        bits[i..FILL_LEN].fill(0);
    }

    bits
}

/// A Nellymoser Asao audio decoder.
pub struct NellymoserDecoder {
    params: AudioCodecParameters,
    buf: AudioBuffer<f32>,
    imdct: Imdct,
    rng: u32,
}

impl NellymoserDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_NELLYMOSER {
            return unsupported_error("nellymoser: invalid codec");
        }

        let Some(rate) = params.sample_rate
        else {
            return unsupported_error("nellymoser: missing sample rate");
        };

        let channels =
            params.channels.clone().unwrap_or(symphonia_core::audio::layouts::CHANNEL_LAYOUT_MONO);

        if channels.count() != 1 {
            return unsupported_error("nellymoser: only mono is supported");
        }

        Ok(NellymoserDecoder {
            params: params.clone(),
            buf: AudioBuffer::new(AudioSpec::new(rate, channels), MAX_BLOCKS * BLOCK_SAMPLES),
            imdct: Imdct::new(),
            rng: RNG_SEED,
        })
    }

    /// A pseudo-random sign for the noise that replaces coefficients without mantissa bits.
    fn noise_sign(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        if self.rng & 0x100 != 0 { -1.0 } else { 1.0 }
    }

    fn decode_block(&mut self, block: &[u8], out: &mut [f32]) {
        let mut bits = BitsLe { data: block, pos: 0 };

        let mut exponents = [0f32; N];
        let mut scales = [0f32; N];
        let mut val = f32::from(NELLY_INIT_TABLE[bits.read(6) as usize]);
        let mut ptr = 0;
        for (i, &size) in NELLY_BAND_SIZES_TABLE.iter().enumerate() {
            if i > 0 {
                val += f32::from(NELLY_DELTA_TABLE[bits.read(5) as usize]);
            }

            let scale = (val / 2048.0).exp2();
            for _ in 0..size {
                exponents[ptr] = val;
                scales[ptr] = scale;
                ptr += 1;
            }
        }

        let alloc = allocate_bits(&exponents);

        for (half, out) in out.chunks_exact_mut(N).enumerate() {
            let mut r = BitsLe { data: block, pos: HEADER_BITS + half * DETAIL_BITS as usize };

            let mut coefs = [0f32; N];
            for j in 0..FILL_LEN {
                let value = if alloc[j] <= 0 {
                    std::f32::consts::FRAC_1_SQRT_2 * self.noise_sign()
                }
                else {
                    let v = r.read(alloc[j] as usize) as usize;
                    NELLY_DEQUANTIZATION_TABLE[(1 << alloc[j]) - 1 + v] as f32
                };
                coefs[j] = value * scales[j];
            }

            self.imdct.process(&coefs, out);
            for s in out.iter_mut() {
                *s *= OUTPUT_SCALE;
            }
        }
    }

    fn decode_inner(&mut self, data: &[u8]) -> Result<()> {
        self.buf.clear();

        let blocks = data.len() / BLOCK_BYTES;
        if blocks == 0 {
            return decode_error("nellymoser: packet is shorter than a block");
        }
        if blocks > MAX_BLOCKS {
            return decode_error("nellymoser: packet holds too many blocks");
        }

        self.buf.render_uninit(Some(blocks * BLOCK_SAMPLES));

        let mut samples = vec![0f32; blocks * BLOCK_SAMPLES];
        for (block, out) in
            data.chunks_exact(BLOCK_BYTES).zip(samples.chunks_exact_mut(BLOCK_SAMPLES))
        {
            self.decode_block(block, out);
        }

        if let Some(plane) = self.buf.plane_mut(0) {
            plane.copy_from_slice(&samples);
        }

        Ok(())
    }
}

impl AudioDecoder for NellymoserDecoder {
    fn reset(&mut self) {
        self.imdct.reset();
        self.rng = RNG_SEED;
    }

    fn codec_info(&self) -> &CodecInfo {
        &NELLYMOSER_CODEC_INFO
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

impl RegisterableAudioDecoder for NellymoserDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(NellymoserDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(CODEC_ID_NELLYMOSER, "nellymoser", "Nellymoser Asao")]
    }
}
