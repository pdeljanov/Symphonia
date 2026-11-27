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
use symphonia_core::codecs::audio::well_known::CODEC_ID_G723_1;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::packet::PacketRef;
use symphonia_core::support_audio_codec;

use crate::frame::{FRAME_LEN, FrameKind, FrameParams, SUBFRAME_LEN, SUBFRAMES, unpack};
use crate::lsp::{self, Lsp, MIN_GAP, MIN_GAP_CONCEALED, ORDER};
use crate::postfilter::{FormantFilter, pitch_postfilter};
use crate::synthesis::{self, HISTORY, Subframe};

const G723_CODEC_INFO: CodecInfo =
    CodecInfo { short_name: "g723_1", long_name: "ITU-T G.723.1", profiles: &[] };

const SAMPLE_RATE: u32 = 8000;
/// The most frames that a packet may hold.
const MAX_FRAMES: usize = 64;
/// The number of decoded samples that the voicing decision of a lost frame looks at.
const VOICING_WINDOW: usize = 120;
/// Frames that are lost in a row beyond this number are muted.
const MUTE_AFTER_LOST: u32 = 3;
const LAG_DELTA: [i32; 4] = [-1, 0, 1, 2];
const LAG_MIN: i32 = 18;
const LAG_MAX: i32 = 142;
const ONE_Q15: i64 = 1 << 15;
const ATTENUATION_Q15: i64 = 24576;

/// The state that the concealment of a lost frame continues from.
struct Concealment {
    lag: i32,
    mid_lag: i32,
    tap_sum: i32,
    unvoiced_gain: i32,
    output: [i32; VOICING_WINDOW],
    lost_run: u32,
    rng: u32,
}

struct State {
    lsp: Lsp,
    history: [i32; HISTORY],
    synthesis: [i32; ORDER],
    formant: FormantFilter,
    concealment: Concealment,
}

impl State {
    fn new() -> Self {
        State {
            lsp: lsp::initial(),
            history: [0; HISTORY],
            synthesis: [0; ORDER],
            formant: FormantFilter::new(),
            concealment: Concealment {
                lag: 60,
                mid_lag: 60,
                tap_sum: 0,
                unvoiced_gain: 0,
                output: [0; VOICING_WINDOW],
                lost_run: 0,
                rng: 0xdead_beef,
            },
        }
    }

    fn push_excitation(&mut self, sub: &Subframe) {
        self.history.copy_within(SUBFRAME_LEN.., 0);
        self.history[HISTORY - SUBFRAME_LEN..].copy_from_slice(sub);
    }

    fn decode(&mut self, p: &FrameParams) -> [i32; FRAME_LEN] {
        let mut cur = lsp::decode(p.lsp_index, &self.lsp);
        if !lsp::stabilize(&mut cur, MIN_GAP) {
            cur = self.lsp;
        }

        let lag0 = p.lag_index[0] as i32 + LAG_MIN;
        let lag2 = p.lag_index[2] as i32 + LAG_MIN;
        let lags = [
            lag0,
            lag0 + LAG_DELTA[p.lag_index[1] as usize],
            lag2,
            lag2 + LAG_DELTA[p.lag_index[3] as usize],
        ];

        let history = self.history;
        let mut excitation = [0i32; FRAME_LEN];
        let mut fixed_gains = [0i32; SUBFRAMES];
        let mut tap_sum = 0i32;
        for s in 0..SUBFRAMES {
            let lag_base = if s < 2 { lag0 } else { lag2 };
            let gain = synthesis::decode_gain(p.high_rate, lag_base, p.gain_index[s]);
            fixed_gains[s] = gain.fixed;
            tap_sum = gain.taps.iter().map(|&t| i32::from(t) * 2).sum();

            let adaptive = synthesis::adaptive(&self.history, lags[s], gain.taps);
            let fixed = if p.high_rate {
                synthesis::fixed_high(p, s, &gain, lag_base)
            }
            else {
                synthesis::fixed_low(p, s, &gain, lags[s])
            };

            let sub: Subframe =
                std::array::from_fn(|n| adaptive[n].saturating_add(fixed[n]).clamp(-32768, 32767));
            excitation[s * SUBFRAME_LEN..(s + 1) * SUBFRAME_LEN].copy_from_slice(&sub);
            self.push_excitation(&sub);
        }

        let lpc: [lsp::Lpc; SUBFRAMES] =
            std::array::from_fn(|s| lsp::to_lpc(&lsp::interpolate(s, &self.lsp, &cur)));

        let mut pcm = [0i32; FRAME_LEN];
        for s in 0..SUBFRAMES {
            let start = s * SUBFRAME_LEN;
            let lag_base = if s < 2 { lag0 } else { lag2 };
            let x = pitch_postfilter(&history, &excitation, start, lag_base, p.high_rate);
            let synthesized = synthesis::synthesize(&lpc[s], &x, &mut self.synthesis);
            let out = self.formant.process(&lpc[s], &synthesized);
            pcm[start..start + SUBFRAME_LEN].copy_from_slice(&out);
        }

        self.lsp = cur;
        let c = &mut self.concealment;
        c.lag = lags[SUBFRAMES - 1];
        c.mid_lag = lags[2];
        c.tap_sum = tap_sum.clamp(0, 32767);
        c.unvoiced_gain = (fixed_gains[2] + fixed_gains[3]) / 2;
        c.lost_run = 0;
        c.output.copy_from_slice(&pcm[FRAME_LEN - VOICING_WINDOW..]);
        pcm
    }

    /// Decides whether the decoded tail is voiced, and finds its pitch lag.
    fn classify(&self) -> (bool, i32) {
        let c = &self.concealment;
        let mut best = (c.mid_lag, false);
        let mut best_ratio = -1i128;
        for delta in -3..=3 {
            let lag = (c.mid_lag + delta).clamp(LAG_MIN, LAG_MAX) as usize;
            if lag >= VOICING_WINDOW {
                continue;
            }
            let (mut corr, mut past, mut cur) = (0i64, 0i64, 0i64);
            for k in lag..VOICING_WINDOW {
                let (x, y) = (i64::from(c.output[k]), i64::from(c.output[k - lag]));
                corr += x * y;
                past += y * y;
                cur += x * x;
            }
            if past == 0 || cur == 0 || corr <= 0 {
                continue;
            }
            let num = i128::from(corr) * i128::from(corr);
            let den = (i128::from(past) * i128::from(cur)).max(1);
            let ratio = (num << 30) / den;
            if ratio > best_ratio {
                best_ratio = ratio;
                best = (lag as i32, num * 8 >= den);
            }
        }
        (best.1, best.0)
    }

    /// Regenerates a lost frame from the state of the previous frames.
    fn conceal(&mut self) -> [i32; FRAME_LEN] {
        let run = self.concealment.lost_run.saturating_add(1);
        self.concealment.lost_run = run;

        let attenuation = if run > MUTE_AFTER_LOST {
            0
        }
        else {
            (0..run).fold(ONE_Q15, |a, _| (a * ATTENUATION_Q15) >> 15)
        };

        let mut cur = lsp::conceal(&self.lsp);
        if !lsp::stabilize(&mut cur, MIN_GAP_CONCEALED) {
            cur = self.lsp;
        }

        let (voiced, voiced_lag) = self.classify();
        let lag = if voiced { voiced_lag } else { self.concealment.lag }.clamp(LAG_MIN, LAG_MAX);

        let adaptive_gain = (i64::from(self.concealment.tap_sum) * attenuation) >> 15;
        let unvoiced_gain = (i64::from(self.concealment.unvoiced_gain) * attenuation) >> 15;

        let lpc: [lsp::Lpc; SUBFRAMES] =
            std::array::from_fn(|s| lsp::to_lpc(&lsp::interpolate(s, &self.lsp, &cur)));

        let mut pcm = [0i32; FRAME_LEN];
        for s in 0..SUBFRAMES {
            let sub: Subframe = if voiced {
                let l = lag as usize;
                std::array::from_fn(|n| {
                    let idx = if l > n { HISTORY - (l - n) } else { HISTORY - l + ((n - l) % l) };
                    scale_q15(self.history[idx], adaptive_gain)
                })
            }
            else {
                std::array::from_fn(|_| scale_q15(self.next_noise(), unvoiced_gain))
            };
            let sub: Subframe = sub.map(|v| v.clamp(-32768, 32767));
            self.push_excitation(&sub);

            let synthesized = synthesis::synthesize(&lpc[s], &sub, &mut self.synthesis);
            let out = self.formant.process(&lpc[s], &synthesized);
            pcm[s * SUBFRAME_LEN..(s + 1) * SUBFRAME_LEN].copy_from_slice(&out);
        }

        self.lsp = cur;
        self.concealment.output.copy_from_slice(&pcm[FRAME_LEN - VOICING_WINDOW..]);
        pcm
    }

    fn next_noise(&mut self) -> i32 {
        let rng = &mut self.concealment.rng;
        *rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (((*rng >> 8) & 0xffff) as i32) - 32768
    }
}

fn scale_q15(value: i32, gain: i64) -> i32 {
    ((i64::from(value) * gain) >> 15).clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// An ITU-T G.723.1 audio decoder.
///
/// Silence insertion descriptors and untransmitted frames are concealed like lost frames rather
/// than replaced by comfort noise.
pub struct G723Decoder {
    params: AudioCodecParameters,
    buf: AudioBuffer<f32>,
    state: State,
}

impl G723Decoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_G723_1 {
            return unsupported_error("g723: invalid codec");
        }

        let rate = params.sample_rate.unwrap_or(SAMPLE_RATE);
        if rate != SAMPLE_RATE {
            return unsupported_error("g723: the sample rate must be 8000 Hz");
        }

        let channels =
            params.channels.clone().unwrap_or(symphonia_core::audio::layouts::CHANNEL_LAYOUT_MONO);
        if channels.count() != 1 {
            return unsupported_error("g723: only mono is supported");
        }

        Ok(G723Decoder {
            params: params.clone(),
            buf: AudioBuffer::new(AudioSpec::new(rate, channels), MAX_FRAMES * FRAME_LEN),
            state: State::new(),
        })
    }

    fn decode_inner(&mut self, mut data: &[u8]) -> Result<()> {
        self.buf.clear();

        let mut pcm = Vec::with_capacity(FRAME_LEN);
        while let Some(&first) = data.first() {
            if pcm.len() >= MAX_FRAMES * FRAME_LEN {
                return decode_error("g723: packet holds too many frames");
            }

            let kind = FrameKind::from_first_byte(first);
            let Some((frame, rest)) = data.split_at_checked(kind.bytes())
            else {
                return decode_error("g723: truncated frame");
            };
            data = rest;

            let decoded = match kind {
                FrameKind::High | FrameKind::Low => match unpack(frame, kind) {
                    Ok(params) => self.state.decode(&params),
                    Err(_) => self.state.conceal(),
                },
                FrameKind::Sid | FrameKind::Untransmitted => self.state.conceal(),
            };
            pcm.extend(decoded.iter().map(|&v| v as f32 / 32768.0));
        }

        if pcm.is_empty() {
            return decode_error("g723: empty packet");
        }

        self.buf.render_uninit(Some(pcm.len()));
        if let Some(plane) = self.buf.plane_mut(0) {
            plane.copy_from_slice(&pcm);
        }

        Ok(())
    }
}

impl AudioDecoder for G723Decoder {
    fn reset(&mut self) {
        self.state = State::new();
    }

    fn codec_info(&self) -> &CodecInfo {
        &G723_CODEC_INFO
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

impl RegisterableAudioDecoder for G723Decoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(G723Decoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(CODEC_ID_G723_1, "g723_1", "ITU-T G.723.1")]
    }
}
