// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef,
};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::well_known::CODEC_ID_TTA;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{Error, Result, decode_error, unsupported_error};
use symphonia_core::packet::PacketRef;
use symphonia_core::support_audio_codec;

const TTA_CODEC_INFO: CodecInfo =
    CodecInfo { short_name: "tta", long_name: "True Audio", profiles: &[] };

/// The number of taps of the adaptive filter.
const FILTER_ORDER: usize = 8;

/// The length of the checksum at the end of each frame.
const FRAME_CRC_LEN: usize = 4;

/// The initial Rice parameter.
const INITIAL_RICE_K: u32 = 10;

/// The maximum Rice parameter. Keeps `1 << (k + 5)` representable.
const MAX_RICE_K: u32 = 26;

/// The maximum length of a unary code before the frame is considered corrupt.
const MAX_UNARY_LEN: u32 = 1 << 20;

/// A reader of a bitstream that is packed starting from the least significant bit.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    cache: u64,
    num_bits: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0, cache: 0, num_bits: 0 }
    }

    fn refill(&mut self) -> Result<()> {
        while self.num_bits <= 56 {
            let Some(&byte) = self.data.get(self.pos)
            else {
                break;
            };

            self.cache |= u64::from(byte) << self.num_bits;
            self.num_bits += 8;
            self.pos += 1;
        }

        if self.num_bits == 0 {
            return Err(Error::DecodeError("tta: unexpected end of frame"));
        }

        Ok(())
    }

    fn read_bit(&mut self) -> Result<u32> {
        if self.num_bits == 0 {
            self.refill()?;
        }

        let bit = (self.cache & 1) as u32;
        self.cache >>= 1;
        self.num_bits -= 1;
        Ok(bit)
    }

    /// Read up to 32 bits.
    fn read_bits(&mut self, count: u32) -> Result<u32> {
        debug_assert!(count <= 32);

        if count == 0 {
            return Ok(0);
        }

        if self.num_bits < count {
            self.refill()?;

            if self.num_bits < count {
                return Err(Error::DecodeError("tta: unexpected end of frame"));
            }
        }

        let value = (self.cache & ((1u64 << count) - 1)) as u32;
        self.cache >>= count;
        self.num_bits -= count;
        Ok(value)
    }

    /// Count the number of 1 bits before the next 0 bit, and consume the 0 bit.
    fn read_unary(&mut self) -> Result<u32> {
        let mut count = 0;

        while self.read_bit()? != 0 {
            count += 1;

            if count > MAX_UNARY_LEN {
                return decode_error("tta: invalid unary code");
            }
        }

        Ok(count)
    }
}

/// An adaptive Rice code.
#[derive(Clone, Copy)]
struct Rice {
    k0: u32,
    k1: u32,
    sum0: u32,
    sum1: u32,
}

impl Rice {
    fn new() -> Self {
        Rice {
            k0: INITIAL_RICE_K,
            k1: INITIAL_RICE_K,
            sum0: 1 << (INITIAL_RICE_K + 4),
            sum1: 1 << (INITIAL_RICE_K + 4),
        }
    }

    /// Adapt `k` to the running sum after adding `value`.
    fn adapt(sum: &mut u32, k: &mut u32, value: u32) {
        *sum = sum.wrapping_add(value).wrapping_sub(*sum >> 4);

        if *k > 0 && *sum < (1 << (*k + 4)) {
            *k -= 1;
        }
        else if *k < MAX_RICE_K && *sum > (1 << (*k + 5)) {
            *k += 1;
        }
    }

    fn read(&mut self, bs: &mut BitReader<'_>) -> Result<i32> {
        let unary = bs.read_unary()?;

        let (deep, k, unary) =
            if unary == 0 { (false, self.k0, 0) } else { (true, self.k1, unary - 1) };

        let mut value = if k > 0 {
            let low = bs.read_bits(k)?;
            unary.wrapping_shl(k).wrapping_add(low)
        }
        else {
            unary
        };

        if deep {
            Self::adapt(&mut self.sum1, &mut self.k1, value);
            value = value.wrapping_add(1 << self.k0);
        }

        Self::adapt(&mut self.sum0, &mut self.k0, value);

        // Map the unsigned value to a signed value.
        Ok(if value & 1 != 0 {
            (value.wrapping_add(1) >> 1) as i32
        }
        else {
            (value >> 1).wrapping_neg() as i32
        })
    }
}

/// The adaptive sign-sign LMS filter of a channel.
#[derive(Clone, Copy)]
struct Filter {
    shift: u32,
    round: i32,
    error: i32,
    weights: [i32; FILTER_ORDER],
    deltas: [i32; FILTER_ORDER],
    history: [i32; FILTER_ORDER],
}

impl Filter {
    fn new(shift: u32) -> Self {
        Filter {
            shift,
            round: 1 << (shift - 1),
            error: 0,
            weights: [0; FILTER_ORDER],
            deltas: [0; FILTER_ORDER],
            history: [0; FILTER_ORDER],
        }
    }

    fn process(&mut self, residual: i32) -> i32 {
        if self.error < 0 {
            for (w, d) in self.weights.iter_mut().zip(&self.deltas) {
                *w = w.wrapping_sub(*d);
            }
        }
        else if self.error > 0 {
            for (w, d) in self.weights.iter_mut().zip(&self.deltas) {
                *w = w.wrapping_add(*d);
            }
        }

        let mut sum = self.round;
        for (h, w) in self.history.iter().zip(&self.weights) {
            sum = sum.wrapping_add(h.wrapping_mul(*w));
        }

        // The history entries 0 to 3 are a delay line of entry 4, and are shifted before the
        // derived entries 4 to 7 are used.
        self.history.copy_within(1..5, 0);

        let h = &self.history;
        let d = &mut self.deltas;

        d[0] = d[1];
        d[1] = d[2];
        d[2] = d[3];
        d[3] = d[4];
        d[4] = (h[4] >> 30) | 1;
        d[5] = ((h[5] >> 30) | 2) & !1;
        d[6] = ((h[6] >> 30) | 2) & !1;
        d[7] = ((h[7] >> 30) | 4) & !3;

        self.error = residual;
        let value = residual.wrapping_add(sum >> self.shift);

        let h = &mut self.history;
        h[4] = h[5].wrapping_neg();
        h[5] = h[6].wrapping_neg();
        h[6] = value.wrapping_sub(h[7]);
        h[7] = value;
        h[5] = h[5].wrapping_add(h[6]);
        h[4] = h[4].wrapping_add(h[5]);

        value
    }
}

#[derive(Clone, Copy)]
struct ChannelState {
    rice: Rice,
    filter: Filter,
    predictor: i32,
}

/// True Audio (TTA) decoder.
pub struct TtaDecoder {
    params: AudioCodecParameters,
    num_channels: usize,
    bits_per_sample: u32,
    filter_shift: u32,
    predictor_shift: u32,
    states: Vec<ChannelState>,
    buf: AudioBuffer<i32>,
}

impl TtaDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_TTA {
            return unsupported_error("tta: invalid codec");
        }

        let Some(rate) = params.sample_rate
        else {
            return unsupported_error("tta: sample rate is required");
        };

        let Some(channels) = params.channels.clone()
        else {
            return unsupported_error("tta: channels or channel layout is required");
        };

        let Some(bits_per_sample) = params.bits_per_sample.filter(|b| (1..=32).contains(b))
        else {
            return unsupported_error("tta: bits per sample is required");
        };

        let num_channels = channels.count();
        let num_bytes = bits_per_sample.div_ceil(8);

        let filter_shift = match num_bytes {
            1 => 10,
            2 => 9,
            3 => 10,
            _ => 12,
        };

        let predictor_shift = if num_bytes == 1 { 4 } else { 5 };

        let max_frames = params.max_frames_per_packet.unwrap_or(1 << 16) as usize;

        if !symphonia_core::audio::is_reasonable_capacity(max_frames, num_channels) {
            return unsupported_error("tta: maximum frames per packet is too large");
        }

        let state =
            ChannelState { rice: Rice::new(), filter: Filter::new(filter_shift), predictor: 0 };

        Ok(TtaDecoder {
            params: params.clone(),
            num_channels,
            bits_per_sample,
            filter_shift,
            predictor_shift,
            states: vec![state; num_channels],
            buf: AudioBuffer::new(AudioSpec::new(rate, channels), max_frames),
        })
    }

    fn decode_inner(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        self.buf.clear();

        let Some(len) = packet.data.len().checked_sub(FRAME_CRC_LEN)
        else {
            return decode_error("tta: frame is too short");
        };

        let num_frames = packet.dur.get() as usize;
        let channels = self.num_channels;

        let state = ChannelState {
            rice: Rice::new(),
            filter: Filter::new(self.filter_shift),
            predictor: 0,
        };
        self.states.fill(state);

        let mut bs = BitReader::new(&packet.data[..len]);

        self.buf.render_uninit(Some(num_frames));

        let shift = 32 - self.bits_per_sample;
        let pred_shift = self.predictor_shift;

        let mut samples = [0i32; 32];

        for frame in 0..num_frames {
            for (ch, state) in self.states.iter_mut().enumerate() {
                let residual = state.rice.read(&mut bs)?;
                let filtered = state.filter.process(residual);

                let predicted =
                    (state.predictor.wrapping_shl(pred_shift).wrapping_sub(state.predictor))
                        >> pred_shift;
                let value = filtered.wrapping_add(predicted);

                state.predictor = value;
                samples[ch] = value;
            }

            if channels > 1 {
                samples[channels - 1] =
                    samples[channels - 1].wrapping_add(samples[channels - 2] / 2);

                for ch in (0..channels - 1).rev() {
                    samples[ch] = samples[ch + 1].wrapping_sub(samples[ch]);
                }
            }

            for (ch, sample) in samples.iter().enumerate().take(channels) {
                if let Some(plane) = self.buf.plane_mut(ch) {
                    plane[frame] = sample.wrapping_shl(shift);
                }
            }
        }

        Ok(())
    }
}

impl AudioDecoder for TtaDecoder {
    fn reset(&mut self) {
        // Every frame is decoded independently.
    }

    fn codec_info(&self) -> &CodecInfo {
        &TTA_CODEC_INFO
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        if let Err(err) = self.decode_inner(packet) {
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

impl RegisterableAudioDecoder for TtaDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(TtaDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(CODEC_ID_TTA, "tta", "True Audio")]
    }
}
