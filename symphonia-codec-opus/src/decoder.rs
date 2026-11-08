//! Opus Top-Level & Multi-Stream Decoder Orchestration (RFC 6716 Section 4, RFC 7845 Section 5).
//!
//! The Opus decoder consists of two main synthesis blocks per elementary stream: the SILK
//! linear-predictive decoder (for low/medium frequencies in SILK-only and Hybrid modes) and
//! the CELT transform decoder (for high frequencies in Hybrid mode or the full spectrum in
//! CELT-only mode). At any given time, one or both of the SILK and CELT decoders may be active.
//! The output of each elementary stream is the sum of the time-aligned outputs from the SILK
//! and CELT decoders:
//! ```text
//!
//!                          +---------+    +------------+
//!                          |  SILK   |    |   Sample   |
//!                       +->| Decoder |--->|    Rate    |----+
//! Bit-    +---------+   |  |         |    | Conversion |    v
//! stream  |  Range  |---+  +---------+    +------------+  /---\  Audio
//! ------->| Decoder |                                     | + |------>
//!         |         |---+  +---------+    +------------+  \---/
//!         +---------+   |  |  CELT   |    | Decimation |    ^
//!                       +->| Decoder |--->| (Optional) |----+
//!                          |         |    |            |
//!                          +---------+    +------------+
//! ```
//!
//! Multi-stream packets (Channel Mapping Family 0 and Family 1 per RFC 7845 Section 5.1.1)
//! consist of `N - 1` self-delimited elementary stream packets followed by `1` undelimited
//! elementary stream packet. Each elementary stream is decoded by its own [`StreamDecoder`]
//! instance (`M` coupled stereo streams followed by `N - M` uncoupled mono streams), and the
//! decoded stream channels are routed to output audio planes via the `OpusHead` channel mapping
//! table with optional Q7.8 dB `output_gain` scaling.
//!
//! <https://datatracker.ietf.org/doc/html/rfc6716#section-4>

#![allow(clippy::too_many_arguments)]

use std::sync::LazyLock;

use symphonia_core::audio::{
    AsGenericAudioBufferRef, Audio, AudioBuffer, AudioMut, AudioSpec, Channels,
    GenericAudioBufferRef, Position,
};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{Error as SymphoniaError, Result, unsupported_error};
use symphonia_core::io::BufReader;
use symphonia_core::packet::PacketRef;

use crate::celt;
use crate::celt::constant::CELT_WINDOW;
use crate::entropy;
use crate::header::{self, ChannelMappingFamily};
use crate::packet::FramePacket;
use crate::silk;
use crate::silk::error::Error as SilkError;
use crate::toc::{AudioMode, Bandwidth, FrameDuration};

/// Maximum samples per channel in a single 120 ms Opus packet at 48 kHz (`5760`).
const MAX_PACKET_SAMPLES_48K: usize = 5760;

/// Maximum samples per channel in a single 20 ms frame at 48 kHz (`960`).
const MAX_20MS_SAMPLES_48K: usize = 960;

/// Maximum samples per channel in a 5 ms transition/redundancy window at 48 kHz (`240`).
const MAX_5MS_SAMPLES_48K: usize = 240;

/// Static Opus codec info.
static CODEC_INFO: LazyLock<CodecInfo> = LazyLock::new(|| CodecInfo {
    short_name: "opus",
    long_name: "Opus Audio Codec",
    profiles: &[],
});

/// Static supported codecs array.
static SUPPORTED_CODECS: LazyLock<[SupportedAudioCodec; 1]> = LazyLock::new(|| {
    [SupportedAudioCodec {
        id: CODEC_ID_OPUS,
        info: *CODEC_INFO,
    }]
});

/// Converts an Opus [`FrameDuration`] and output `sample_rate` into a per-channel sample count.
#[inline]
fn duration_samples(frame_size: FrameDuration, sample_rate: u32) -> usize {
    let fs = sample_rate as usize;
    return match frame_size {
        FrameDuration::Ms2_5 => fs / 400,
        FrameDuration::Ms5 => fs / 200,
        FrameDuration::Ms10 => fs / 100,
        FrameDuration::Ms20 => fs / 50,
        FrameDuration::Ms40 => fs / 25,
        FrameDuration::Ms60 => (fs * 3) / 50,
    };
}

/// Computes the linear amplitude scale factor $10^{\text{output\_gain} / (20 \times 256)}$
/// from an `OpusHead` Q7.8 dB gain field (`opus_multistream_decoder.c:302`).
#[inline]
fn compute_output_gain_scale(output_gain: i16) -> f32 {
    if output_gain == 0 {
        return 1.0_f32;
    }
    return (std::f64::consts::LN_2 * (6.48814081e-4_f64 * (output_gain as f64))).exp() as f32;
}

/// Applies the squared-window cross-fade `out[i] = w * out[i] + (1 - w) * in1[i]`
/// where `w = CELT_WINDOW[i * downsample]^2` (`smooth_fade` in `opus_decoder.c:262`).
#[inline]
fn smooth_fade_into_second(
    in1: &[f32],
    in2_and_out: &mut [f32],
    overlap: usize,
    downsample: usize,
) {
    for (i, (out_sample, &in1_sample)) in in2_and_out[..overlap]
        .iter_mut()
        .zip(&in1[..overlap])
        .enumerate()
    {
        let w0 = CELT_WINDOW[i * downsample];
        let w = w0 * w0;
        *out_sample = w * *out_sample + (1.0_f32 - w) * in1_sample;
    }
    return;
}

/// Applies the squared-window cross-fade `out[i] = w * in2[i] + (1 - w) * out[i]`
/// where `w = CELT_WINDOW[i * downsample]^2` (`smooth_fade` in `opus_decoder.c:262`).
#[inline]
fn smooth_fade_from_first(
    in1_and_out: &mut [f32],
    in2: &[f32],
    overlap: usize,
    downsample: usize,
) {
    for (i, (out_sample, &in2_sample)) in in1_and_out[..overlap]
        .iter_mut()
        .zip(&in2[..overlap])
        .enumerate()
    {
        let w0 = CELT_WINDOW[i * downsample];
        let w = w0 * w0;
        *out_sample = w * in2_sample + (1.0_f32 - w) * *out_sample;
    }
    return;
}

/// Parsed 5 ms CELT redundancy metadata from a SILK or Hybrid frame (RFC 6716 Section 4.4).
#[derive(Debug, Clone, Copy)]
struct RedundancyHeader {
    active_len: usize,
    redundancy: bool,
    celt_to_silk: bool,
    redundancy_bytes: usize,
}

/// Elementary Opus stream decoder pairing a SILK decoder and a CELT decoder for either a
/// mono (`output_channels == 1`) or coupled stereo (`output_channels == 2`) stream.
pub struct StreamDecoder {
    output_channels: usize,
    sample_rate: u32,
    silk_decoder: silk::Decoder,
    celt_decoder: celt::Decoder,
    prev_mode: Option<AudioMode>,
    prev_redundancy: bool,
    last_bandwidth: Bandwidth,
    last_frame_size: FrameDuration,
    last_stereo: bool,
    range_final: u32,
}

impl StreamDecoder {
    /// Creates a new [`StreamDecoder`] for `sample_rate` (`8_000, 12_000, 16_000, 24_000, 48_000` Hz)
    /// and `output_channels` (`1` or `2`).
    pub fn new(sample_rate: u32, output_channels: usize) -> Result<Self> {
        let mut silk_decoder = silk::Decoder::new();
        silk_decoder.set_output_sample_rate(sample_rate);
        let celt_decoder = celt::Decoder::new(sample_rate, output_channels)?;

        return Ok(Self {
            output_channels,
            sample_rate,
            silk_decoder,
            celt_decoder,
            prev_mode: None,
            prev_redundancy: false,
            last_bandwidth: Bandwidth::FullBand,
            last_frame_size: FrameDuration::Ms20,
            last_stereo: output_channels == 2,
            range_final: 0,
        });
    }

    /// Resets the elementary stream decoder state across stream discontinuities.
    pub fn reset(&mut self) {
        self.silk_decoder.reset();
        self.celt_decoder.reset();
        self.prev_mode = None;
        self.prev_redundancy = false;
        self.last_bandwidth = Bandwidth::FullBand;
        self.last_frame_size = FrameDuration::Ms20;
        self.last_stereo = self.output_channels == 2;
        self.range_final = 0;
        return;
    }

    /// Returns the `Channels` descriptor matching `self.output_channels` for SILK configuration.
    #[inline]
    fn silk_api_channels(&self) -> Channels {
        if self.output_channels == 2 {
            return Channels::Positioned(Position::FRONT_LEFT | Position::FRONT_RIGHT);
        }
        return Channels::Positioned(Position::FRONT_LEFT);
    }

    /// Decodes all frames of an elementary stream [`FramePacket`] into `pcm_ch0` and `pcm_ch1`.
    fn decode_packet(
        &mut self,
        pkt: &FramePacket<'_>,
        pcm_ch0: &mut [f32; MAX_PACKET_SAMPLES_48K],
        pcm_ch1: &mut [f32; MAX_PACKET_SAMPLES_48K],
    ) -> Result<usize> {
        let samples_per_frame = duration_samples(pkt.frame_size, self.sample_rate);
        let mut offset = 0usize;

        for &frame_bytes in &pkt.frames {
            let ch0_slice = &mut pcm_ch0[offset..(offset + samples_per_frame)];
            let ch1_opt = (self.output_channels == 2)
                .then(|| &mut pcm_ch1[offset..(offset + samples_per_frame)]);
            let written = self.decode_opus_frame(
                Some(frame_bytes),
                pkt.audio_mode,
                pkt.bandwidth,
                pkt.frame_size,
                pkt.stereo,
                ch0_slice,
                ch1_opt,
            )?;
            offset += written;
        }

        return Ok(offset);
    }

    /// Conceals a lost packet of duration `loss_dur` into `pcm_ch0` and `pcm_ch1`.
    fn conceal_lost_packet(
        &mut self,
        loss_dur: FrameDuration,
        pcm_ch0: &mut [f32; MAX_PACKET_SAMPLES_48K],
        pcm_ch1: &mut [f32; MAX_PACKET_SAMPLES_48K],
    ) -> Result<usize> {
        let total_samples = duration_samples(loss_dur, self.sample_rate);
        let mode = self.prev_mode.unwrap_or(AudioMode::SILK);
        let bw = self.last_bandwidth;
        let st = self.last_stereo;
        let ch0_slice = &mut pcm_ch0[..total_samples];
        let ch1_opt = (self.output_channels == 2).then(|| &mut pcm_ch1[..total_samples]);

        return self.decode_opus_frame(None, mode, bw, loss_dur, st, ch0_slice, ch1_opt);
    }

    /// Conceals a single lost/DTX frame of duration `frame_size` ($\le 20\text{ ms}$) into
    /// `out_ch0` and `out_ch1` (`opus_decode_frame` with `data == NULL` in `opus_decoder.c:302-360`).
    fn conceal_up_to_20ms(
        &mut self,
        frame_size: FrameDuration,
        out_ch0: &mut [f32],
        mut out_ch1: Option<&mut [f32]>,
    ) -> Result<usize> {
        let fs = self.sample_rate as usize;
        let f10 = fs / 100;
        let audiosize = duration_samples(frame_size, self.sample_rate);
        let frame_size_48k = duration_samples(frame_size, 48_000);

        out_ch0[..audiosize].fill(0.0);
        if let Some(ch1) = out_ch1.as_deref_mut() {
            ch1[..audiosize].fill(0.0);
        }

        let Some(prev_mode) = self.prev_mode else {
            self.range_final = 0;
            return Ok(audiosize);
        };

        let bandwidth = self.last_bandwidth;
        let packet_stereo = self.last_stereo;
        let silk_channels = self.silk_api_channels();

        if prev_mode != AudioMode::CELT {
            if audiosize < f10 {
                let mut tmp0 = [0.0_f32; MAX_20MS_SAMPLES_48K];
                let mut tmp1 = [0.0_f32; MAX_20MS_SAMPLES_48K];
                self.silk_decoder
                    .configure(silk_channels, FrameDuration::Ms10, bandwidth)?;
                let tmp1_slice = (self.output_channels == 2).then(|| &mut tmp1[..f10]);
                self.silk_decoder
                    .conceal_frame_planar(packet_stereo, &mut tmp0[..f10], tmp1_slice)?;
                out_ch0[..audiosize].copy_from_slice(&tmp0[..audiosize]);
                if let Some(ch1) = out_ch1.as_deref_mut() {
                    ch1[..audiosize].copy_from_slice(&tmp1[..audiosize]);
                }
            } else {
                self.silk_decoder
                    .configure(silk_channels, frame_size, bandwidth)?;
                self.silk_decoder.conceal_frame_planar(
                    packet_stereo,
                    &mut out_ch0[..audiosize],
                    out_ch1.as_deref_mut().map(|s| &mut s[..audiosize]),
                )?;
            }
        }

        let stream_channels = if packet_stereo { 2 } else { 1 };
        let start_band = if prev_mode != AudioMode::CELT { 17 } else { 0 };
        self.celt_decoder.set_bandwidth(bandwidth);
        self.celt_decoder.set_stream_channels(stream_channels)?;
        self.celt_decoder.set_start_band(start_band)?;

        if prev_mode != AudioMode::SILK {
            self.celt_decoder.conceal_frame_planar(
                frame_size_48k.min(MAX_20MS_SAMPLES_48K),
                &mut out_ch0[..audiosize],
                out_ch1.map(|s| &mut s[..audiosize]),
                prev_mode != AudioMode::CELT,
            )?;
        }

        self.range_final = 0;
        self.prev_redundancy = false;
        return Ok(audiosize);
    }

    /// Parses the optional 5 ms CELT redundancy header from a SILK or Hybrid frame
    /// (RFC 6716 Section 4.4 / `opus_decoder.c:500-528`).
    fn parse_redundancy_header(
        mode: AudioMode,
        range_decoder: &mut entropy::Decoder<'_>,
        raw_len: usize,
    ) -> Result<RedundancyHeader> {
        let mut active_len = raw_len;
        let mut redundancy = false;
        let mut celt_to_silk = false;
        let mut redundancy_bytes = 0usize;

        if mode != AudioMode::CELT {
            let min_bits = 17 + if mode == AudioMode::Hybrid { 20 } else { 0 };
            if range_decoder.tell() + min_bits <= (active_len as i32) * 8 {
                redundancy = if mode == AudioMode::Hybrid {
                    range_decoder.decode_symbol_logp(12)? != 0
                } else {
                    true
                };
                if redundancy {
                    celt_to_silk = range_decoder.decode_symbol_logp(1)? != 0;
                    redundancy_bytes = if mode == AudioMode::Hybrid {
                        (range_decoder.decode_uint(256)? as usize) + 2
                    } else {
                        active_len.saturating_sub(((range_decoder.tell() + 7) >> 3) as usize)
                    };
                    active_len = active_len.saturating_sub(redundancy_bytes);
                    if (active_len as i32) * 8 < range_decoder.tell() {
                        active_len = 0;
                        redundancy_bytes = 0;
                        redundancy = false;
                    } else {
                        range_decoder.shrink_storage(redundancy_bytes);
                    }
                }
            }
        }

        return Ok(RedundancyHeader {
            active_len,
            redundancy,
            celt_to_silk,
            redundancy_bytes,
        });
    }

    /// Decodes a 5 ms (`MAX_5MS_SAMPLES_48K`) redundant CELT frame into `redundant_ch0` and `redundant_ch1`.
    fn decode_redundant_celt_frame(
        &mut self,
        red_slice: &[u8],
        f5: usize,
        redundant_ch0: &mut [f32; MAX_5MS_SAMPLES_48K],
        redundant_ch1: &mut [f32; MAX_5MS_SAMPLES_48K],
    ) -> Result<u32> {
        self.celt_decoder.set_start_band(0)?;
        let red_ch1 = (self.output_channels == 2).then(|| &mut redundant_ch1[..f5]);
        self.celt_decoder.decode_frame_planar(
            Some(red_slice),
            None,
            MAX_5MS_SAMPLES_48K,
            &mut redundant_ch0[..f5],
            red_ch1,
            false,
        )?;
        return Ok(self.celt_decoder.final_range());
    }

    /// Decodes a single Opus frame (active or lost/DTX) of `frame_size` into planar slices
    /// `out_ch0` and `out_ch1`, returning the number of output samples written per channel
    /// (`opus_decode_frame` in `opus_decoder.c:285-693`).
    pub fn decode_opus_frame(
        &mut self,
        frame_data: Option<&[u8]>,
        mode: AudioMode,
        bandwidth: Bandwidth,
        frame_size: FrameDuration,
        packet_stereo: bool,
        out_ch0: &mut [f32],
        mut out_ch1: Option<&mut [f32]>,
    ) -> Result<usize> {
        let fs = self.sample_rate as usize;
        let downsample = 48_000 / fs;
        let f2_5 = fs / 400;
        let f5 = fs / 200;
        let f20 = fs / 50;
        let audiosize = duration_samples(frame_size, self.sample_rate);
        let frame_size_48k = duration_samples(frame_size, 48_000);

        let active_bytes = frame_data.filter(|b| !b.is_empty());

        let Some(raw_bytes) = active_bytes else {
            if self.prev_mode.is_none() {
                out_ch0[..audiosize].fill(0.0);
                if let Some(ch1) = out_ch1.as_deref_mut() {
                    ch1[..audiosize].fill(0.0);
                }
                self.range_final = 0;
                return Ok(audiosize);
            }

            if audiosize > f20 {
                let mut offset = 0usize;
                while offset < audiosize {
                    let step = f20.min(audiosize - offset);
                    let ch1_step = out_ch1
                        .as_deref_mut()
                        .map(|s| &mut s[offset..(offset + step)]);
                    self.conceal_up_to_20ms(
                        FrameDuration::Ms20,
                        &mut out_ch0[offset..(offset + step)],
                        ch1_step,
                    )?;
                    offset += step;
                }
                return Ok(audiosize);
            }

            return self.conceal_up_to_20ms(frame_size, out_ch0, out_ch1);
        };

        out_ch0[..audiosize].fill(0.0);
        if let Some(ch1) = out_ch1.as_deref_mut() {
            ch1[..audiosize].fill(0.0);
        }

        let stream_channels = if packet_stereo { 2 } else { 1 };
        let celt_accum = mode != AudioMode::CELT;

        let mut transition = match self.prev_mode {
            Some(prev) => {
                (mode == AudioMode::CELT && prev != AudioMode::CELT && !self.prev_redundancy)
                    || (mode != AudioMode::CELT && prev == AudioMode::CELT)
            }
            None => false,
        };

        let mut pcm_transition_ch0 = [0.0_f32; MAX_5MS_SAMPLES_48K];
        let mut pcm_transition_ch1 = [0.0_f32; MAX_5MS_SAMPLES_48K];

        if transition && mode == AudioMode::CELT {
            let trans_dur = if audiosize == f2_5 {
                FrameDuration::Ms2_5
            } else {
                FrameDuration::Ms5
            };
            let trans_samples = f5.min(audiosize);
            let trans_ch1 =
                (self.output_channels == 2).then(|| &mut pcm_transition_ch1[..trans_samples]);
            self.conceal_up_to_20ms(
                trans_dur,
                &mut pcm_transition_ch0[..trans_samples],
                trans_ch1,
            )?;
        }

        let mut range_decoder = entropy::Decoder::new(raw_bytes)?;

        // 1. SILK Layer Decode (SILK-only and Hybrid modes).
        if mode != AudioMode::CELT {
            if self.prev_mode == Some(AudioMode::CELT) {
                self.silk_decoder.reset();
            }
            let silk_channels = self.silk_api_channels();
            self.silk_decoder
                .configure(silk_channels, frame_size, bandwidth)?;
            self.silk_decoder.decode_frame_planar(
                &mut range_decoder,
                packet_stereo,
                &mut out_ch0[..audiosize],
                out_ch1.as_deref_mut().map(|s| &mut s[..audiosize]),
            )?;
        }

        // 2. Redundancy Frame Header Check (RFC 6716 Section 4.4 / opus_decoder.c:500-528).
        let RedundancyHeader {
            active_len,
            redundancy,
            celt_to_silk,
            redundancy_bytes,
        } = Self::parse_redundancy_header(mode, &mut range_decoder, raw_bytes.len())?;
        let mut redundant_rng = 0u32;

        if redundancy {
            transition = false;
        }

        // 3. Conceal 5 ms CELT transition tail when switching from CELT to SILK/Hybrid without redundancy.
        if transition && mode != AudioMode::CELT {
            let trans_dur = if audiosize == f2_5 {
                FrameDuration::Ms2_5
            } else {
                FrameDuration::Ms5
            };
            let trans_samples = f5.min(audiosize);
            let trans_ch1 =
                (self.output_channels == 2).then(|| &mut pcm_transition_ch1[..trans_samples]);
            self.conceal_up_to_20ms(
                trans_dur,
                &mut pcm_transition_ch0[..trans_samples],
                trans_ch1,
            )?;
        }

        // 4. Configure CELT bandwidth and coded channel count.
        self.celt_decoder.set_bandwidth(bandwidth);
        self.celt_decoder.set_stream_channels(stream_channels)?;

        let mut redundant_ch0 = [0.0_f32; MAX_5MS_SAMPLES_48K];
        let mut redundant_ch1 = [0.0_f32; MAX_5MS_SAMPLES_48K];

        // 5. Decode 5 ms redundant CELT frame for CELT -> SILK/Hybrid transition.
        if redundancy && celt_to_silk {
            let red_slice = &raw_bytes[active_len..(active_len + redundancy_bytes)];
            redundant_rng = self.decode_redundant_celt_frame(
                red_slice,
                f5,
                &mut redundant_ch0,
                &mut redundant_ch1,
            )?;
        }

        // 6. Set active CELT start band (17 for Hybrid/SILK, 0 for CELT-only).
        let start_band = if mode != AudioMode::CELT { 17 } else { 0 };
        self.celt_decoder.set_start_band(start_band)?;

        // 7. Decode main CELT frame (Hybrid or CELT-only) or flush CELT overlap tail on Hybrid -> SILK.
        if mode != AudioMode::SILK {
            let celt_frame_size_48k = frame_size_48k.min(MAX_20MS_SAMPLES_48K);
            if Some(mode) != self.prev_mode && self.prev_mode.is_some() && !self.prev_redundancy {
                self.celt_decoder.reset();
            }
            if active_len == 0 {
                self.celt_decoder.conceal_frame_planar(
                    celt_frame_size_48k,
                    &mut out_ch0[..audiosize],
                    out_ch1.as_deref_mut().map(|s| &mut s[..audiosize]),
                    celt_accum,
                )?;
            } else {
                self.celt_decoder.decode_frame_planar(
                    Some(&raw_bytes[..active_len]),
                    Some(&mut range_decoder),
                    celt_frame_size_48k,
                    &mut out_ch0[..audiosize],
                    out_ch1.as_deref_mut().map(|s| &mut s[..audiosize]),
                    celt_accum,
                )?;
            }
            self.range_final = self.celt_decoder.final_range();
        } else {
            if self.prev_mode == Some(AudioMode::Hybrid)
                && !(redundancy && celt_to_silk && self.prev_redundancy)
            {
                self.celt_decoder.set_start_band(0)?;
                self.celt_decoder.decode_frame_planar(
                    Some(&[0xFF, 0xFF]),
                    None,
                    120,
                    &mut out_ch0[..f2_5],
                    out_ch1.as_deref_mut().map(|s| &mut s[..f2_5]),
                    true,
                )?;
            }
            self.range_final = range_decoder.final_range();
        }

        // 8. Decode 5 ms redundant CELT frame for SILK/Hybrid -> CELT transition and cross-fade tail.
        if redundancy && !celt_to_silk {
            self.celt_decoder.reset();
            let red_slice = &raw_bytes[active_len..(active_len + redundancy_bytes)];
            redundant_rng = self.decode_redundant_celt_frame(
                red_slice,
                f5,
                &mut redundant_ch0,
                &mut redundant_ch1,
            )?;

            let tail_start = audiosize - f2_5;
            smooth_fade_from_first(
                &mut out_ch0[tail_start..audiosize],
                &redundant_ch0[f2_5..f5],
                f2_5,
                downsample,
            );
            if let Some(ch1) = out_ch1.as_deref_mut() {
                smooth_fade_from_first(
                    &mut ch1[tail_start..audiosize],
                    &redundant_ch1[f2_5..f5],
                    f2_5,
                    downsample,
                );
            }
        }

        // 9. Apply CELT -> SILK/Hybrid redundancy cross-fade at start of frame.
        if redundancy
            && celt_to_silk
            && (self.prev_mode != Some(AudioMode::SILK) || self.prev_redundancy)
        {
            out_ch0[..f2_5].copy_from_slice(&redundant_ch0[..f2_5]);
            smooth_fade_into_second(
                &redundant_ch0[f2_5..f5],
                &mut out_ch0[f2_5..f5],
                f2_5,
                downsample,
            );
            if let Some(ch1) = out_ch1.as_deref_mut() {
                ch1[..f2_5].copy_from_slice(&redundant_ch1[..f2_5]);
                smooth_fade_into_second(
                    &redundant_ch1[f2_5..f5],
                    &mut ch1[f2_5..f5],
                    f2_5,
                    downsample,
                );
            }
        }

        // 10. Apply un-redundant mode transition cross-fade at start of frame.
        if transition {
            if audiosize >= f5 {
                out_ch0[..f2_5].copy_from_slice(&pcm_transition_ch0[..f2_5]);
                smooth_fade_into_second(
                    &pcm_transition_ch0[f2_5..f5],
                    &mut out_ch0[f2_5..f5],
                    f2_5,
                    downsample,
                );
                if let Some(ch1) = out_ch1.as_deref_mut() {
                    ch1[..f2_5].copy_from_slice(&pcm_transition_ch1[..f2_5]);
                    smooth_fade_into_second(
                        &pcm_transition_ch1[f2_5..f5],
                        &mut ch1[f2_5..f5],
                        f2_5,
                        downsample,
                    );
                }
            } else {
                smooth_fade_into_second(
                    &pcm_transition_ch0[..f2_5],
                    &mut out_ch0[..f2_5],
                    f2_5,
                    downsample,
                );
                if let Some(ch1) = out_ch1 {
                    smooth_fade_into_second(
                        &pcm_transition_ch1[..f2_5],
                        &mut ch1[..f2_5],
                        f2_5,
                        downsample,
                    );
                }
            }
        }

        if active_len <= 1 {
            self.range_final = 0;
        } else {
            self.range_final ^= redundant_rng;
        }

        self.prev_mode = Some(mode);
        self.prev_redundancy = redundancy && !celt_to_silk;
        self.last_bandwidth = bandwidth;
        self.last_frame_size = frame_size;
        self.last_stereo = packet_stereo;

        return Ok(audiosize);
    }
}

/// The `OpusDecoder` struct implements the Symphonia [`AudioDecoder`] trait.
///
/// It owns the top-level codec parameters, multi-stream `StreamDecoder` instances,
/// `OpusHead` channel mapping and gain state, and the output [`AudioBuffer<f32>`].
pub struct OpusDecoder {
    params: AudioCodecParameters,
    channels: Channels,
    sample_rate: u32,
    buffer: AudioBuffer<f32>,
    streams: Vec<StreamDecoder>,
    scratch_ch0: [f32; MAX_PACKET_SAMPLES_48K],
    scratch_ch1: [f32; MAX_PACKET_SAMPLES_48K],
    coupled_count: usize,
    channel_mapping: Vec<u8>,
    pre_skip: u16,
    output_gain: i16,
    gain_scale: f32,
    range_final: u32,
}

impl OpusDecoder {
    /// Creates a new [`OpusDecoder`] from `params` and `_options`.
    pub fn try_new(params: &AudioCodecParameters, _options: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_OPUS {
            return unsupported_error("opus: invalid codec");
        }

        let sample_rate = params.sample_rate.ok_or(SilkError::UnsupportedConfig)?;
        let channels = params.channels.clone().ok_or(SilkError::UnsupportedConfig)?;
        let num_output_channels = channels.count();
        if num_output_channels == 0 || num_output_channels > 255 {
            return Err(SilkError::UnsupportedConfig.into());
        }

        let mut pre_skip = 0u16;
        let mut output_gain = 0i16;
        let coupled_count: usize;
        let mut streams: Vec<StreamDecoder>;
        let channel_mapping: Vec<u8>;

        if let Some(extra) = params.extra_data.as_deref()
            && !extra.is_empty()
        {
            let id = header::ID::parse(BufReader::new(extra))
                .map_err(|_| SymphoniaError::DecodeError("opus: invalid OpusHead extra_data"))?;

            if id.channel_count as usize != num_output_channels {
                return Err(SymphoniaError::DecodeError(
                    "opus: OpusHead channel count does not match AudioCodecParameters",
                ));
            }

            pre_skip = id.pre_skip;
            output_gain = id.output_gain;

            match id.mapping_family {
                ChannelMappingFamily::Rtp => {
                    if num_output_channels > 2 {
                        return Err(SymphoniaError::DecodeError(
                            "opus: RTP mapping family 0 only supports 1 or 2 channels",
                        ));
                    }
                    coupled_count = usize::from(num_output_channels == 2);
                    streams = vec![StreamDecoder::new(sample_rate, num_output_channels)?];
                    channel_mapping = if num_output_channels == 2 {
                        vec![0, 1]
                    } else {
                        vec![0]
                    };
                }
                ChannelMappingFamily::Vorbis => {
                    let table = id.channel_mapping_table.ok_or(SymphoniaError::DecodeError(
                        "opus: missing channel mapping table for Vorbis family 1",
                    ))?;
                    let stream_count = table.stream_count as usize;
                    coupled_count = table.coupled_count as usize;
                    streams = Vec::with_capacity(stream_count);
                    for i in 0..stream_count {
                        let stream_out_channels = if i < coupled_count { 2 } else { 1 };
                        streams.push(StreamDecoder::new(sample_rate, stream_out_channels)?);
                    }
                    channel_mapping = table.channel_mapping;
                }
                ChannelMappingFamily::Reserved(_) | ChannelMappingFamily::Undefined => {
                    return unsupported_error("opus: unsupported channel mapping family");
                }
            }
        } else {
            if num_output_channels > 2 {
                return Err(SilkError::UnsupportedConfig.into());
            }
            coupled_count = usize::from(num_output_channels == 2);
            streams = vec![StreamDecoder::new(sample_rate, num_output_channels)?];
            channel_mapping = if num_output_channels == 2 {
                vec![0, 1]
            } else {
                vec![0]
            };
        }

        let gain_scale = compute_output_gain_scale(output_gain);
        let spec = AudioSpec::new(sample_rate, channels.clone());
        let buffer = AudioBuffer::new(spec, MAX_PACKET_SAMPLES_48K.max(sample_rate as usize));

        return Ok(Self {
            params: params.to_owned(),
            channels,
            sample_rate,
            buffer,
            streams,
            scratch_ch0: [0.0; MAX_PACKET_SAMPLES_48K],
            scratch_ch1: [0.0; MAX_PACKET_SAMPLES_48K],
            coupled_count,
            channel_mapping,
            pre_skip,
            output_gain,
            gain_scale,
            range_final: 0,
        });
    }

    /// Returns the `pre_skip` sample count parsed from the `OpusHead` identification header.
    #[inline]
    pub fn pre_skip(&self) -> u16 {
        return self.pre_skip;
    }

    /// Returns the Q7.8 dB `output_gain` parsed from the `OpusHead` identification header.
    #[inline]
    pub fn output_gain(&self) -> i16 {
        return self.output_gain;
    }

    /// Returns the combined final range coder state (`OPUS_GET_FINAL_RANGE`) from the last decoded packet.
    #[inline]
    pub fn final_range(&self) -> u32 {
        return self.range_final;
    }

    /// Conceals a lost packet across all elementary streams using each stream's last frame duration
    /// and writes the resulting audio into the internal [`AudioBuffer<f32>`].
    pub fn decode_lost_packet(&mut self) -> Result<GenericAudioBufferRef<'_>> {
        let loss_dur = self
            .streams
            .first()
            .map(|s| s.last_frame_size)
            .unwrap_or(FrameDuration::Ms20);
        let total_samples = duration_samples(loss_dur, self.sample_rate);
        self.prepare_output_buffer(total_samples)?;

        for i in 0..self.streams.len() {
            self.streams[i].conceal_lost_packet(
                loss_dur,
                &mut self.scratch_ch0,
                &mut self.scratch_ch1,
            )?;
            self.route_stream_to_buffer(i, total_samples)?;
        }

        self.range_final = 0;
        return Ok(self.buffer.as_generic_audio_buffer_ref());
    }

    /// Clears `self.buffer`, renders `total_samples` frames, and zeroes any unmapped (`255`) output planes.
    fn prepare_output_buffer(&mut self, total_samples: usize) -> Result<()> {
        self.buffer.clear();
        self.buffer.render_uninit(Some(total_samples));
        let num_output_channels = self.channels.count();
        for ch in 0..num_output_channels {
            if self.channel_mapping.get(ch).copied().unwrap_or(255) == 255 {
                let dst = self
                    .buffer
                    .plane_mut(ch)
                    .ok_or(SymphoniaError::DecodeError("opus: missing output plane"))?;
                dst[..total_samples].fill(0.0);
            }
        }
        return Ok(());
    }

    /// Routes the decoded planar samples of `stream_i` from `self.scratch_ch0` and `self.scratch_ch1`
    /// into `self.buffer` according to `self.channel_mapping` and applies `self.gain_scale`.
    fn route_stream_to_buffer(&mut self, stream_i: usize, total_samples: usize) -> Result<()> {
        let num_output_channels = self.channels.count();
        let coupled = self.coupled_count;
        let apply_gain = self.output_gain != 0;
        let scale = self.gain_scale;

        for ch in 0..num_output_channels {
            let map_val = self.channel_mapping.get(ch).copied().unwrap_or(255) as usize;
            let src_opt = match stream_i < coupled {
                true if map_val == 2 * stream_i => Some(&self.scratch_ch0[..total_samples]),
                true if map_val == 2 * stream_i + 1 => Some(&self.scratch_ch1[..total_samples]),
                false if map_val == stream_i + coupled => Some(&self.scratch_ch0[..total_samples]),
                _ => None,
            };

            if let Some(src_slice) = src_opt {
                let dst = self
                    .buffer
                    .plane_mut(ch)
                    .ok_or(SymphoniaError::DecodeError("opus: missing output plane"))?;
                let dst_slice = &mut dst[..total_samples];
                if apply_gain {
                    for (d, &s) in dst_slice.iter_mut().zip(src_slice.iter()) {
                        *d = s * scale;
                    }
                } else {
                    dst_slice.copy_from_slice(src_slice);
                }
            }
        }

        return Ok(());
    }
}

impl AudioDecoder for OpusDecoder {
    fn reset(&mut self) {
        for stream in self.streams.iter_mut() {
            stream.reset();
        }
        self.scratch_ch0.fill(0.0);
        self.scratch_ch1.fill(0.0);
        self.range_final = 0;
        self.buffer.clear();
        return;
    }

    fn codec_info(&self) -> &CodecInfo {
        return &CODEC_INFO;
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        return &self.params;
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        if packet.data.is_empty() {
            return self.decode_lost_packet();
        }

        self.buffer.clear();
        let num_streams = self.streams.len();
        let mut cursor = packet.data;
        let mut expected_frame_count = 0usize;
        let mut expected_frame_size = FrameDuration::Ms20;
        let mut total_samples = 0usize;
        let mut combined_rng = 0u32;

        for i in 0..num_streams {
            let pkt = if i + 1 < num_streams {
                let (parsed, consumed) = FramePacket::parse_self_delimited(cursor)?;
                cursor = &cursor[consumed..];
                parsed
            } else {
                FramePacket::new(cursor)?
            };

            match i {
                0 => {
                    expected_frame_count = pkt.frames.len();
                    expected_frame_size = pkt.frame_size;
                    total_samples = expected_frame_count
                        * duration_samples(expected_frame_size, self.sample_rate);
                    self.prepare_output_buffer(total_samples)?;
                }
                _ if pkt.frames.len() != expected_frame_count
                    || pkt.frame_size != expected_frame_size =>
                {
                    self.buffer.clear();
                    return Err(SymphoniaError::DecodeError(
                        "opus: inconsistent frame count or frame duration across elementary streams",
                    ));
                }
                _ => {}
            }

            self.streams[i].decode_packet(&pkt, &mut self.scratch_ch0, &mut self.scratch_ch1)?;
            combined_rng ^= self.streams[i].range_final;
            self.route_stream_to_buffer(i, total_samples)?;
        }

        self.range_final = combined_rng;
        return Ok(self.buffer.as_generic_audio_buffer_ref());
    }

    fn finalize(&mut self) -> FinalizeResult {
        return FinalizeResult::default();
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        return self.buffer.as_generic_audio_buffer_ref();
    }
}

impl RegisterableAudioDecoder for OpusDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>> {
        return Ok(Box::new(Self::try_new(params, opts)?));
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        return &SUPPORTED_CODECS[..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symphonia_core::audio::{Audio, Position};
    use symphonia_core::codecs::audio::well_known::CODEC_ID_VORBIS;
    use symphonia_core::units::{Duration, Timestamp};

    fn quantize_i16_24(slice: &[f32]) -> [i16; 24] {
        return std::array::from_fn(|i| (slice[i] * 32768.0).round() as i16);
    }

    fn assert_plane_matches_f32_oracle(
        plane: &[f32],
        expected_len: usize,
        expected_head: &[f32; 24],
        expected_tail: &[f32; 24],
        expected_rms: f32,
        tol: f32,
    ) {
        assert_eq!(plane.len(), expected_len);
        for (i, (&actual, &exp)) in plane[..24].iter().zip(expected_head.iter()).enumerate() {
            let diff = (actual - exp).abs();
            assert!(
                diff <= tol,
                "head sample {i} mismatch: actual={actual}, expected={exp}, diff={diff}, tol={tol}"
            );
        }
        let tail_start = expected_len - 24;
        for (i, (&actual, &exp)) in plane[tail_start..]
            .iter()
            .zip(expected_tail.iter())
            .enumerate()
        {
            let diff = (actual - exp).abs();
            assert!(
                diff <= tol,
                "tail sample {i} mismatch: actual={actual}, expected={exp}, diff={diff}, tol={tol}"
            );
        }
        let rms = (plane.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>()
            / (expected_len as f64))
            .sqrt() as f32;
        let rms_diff = (rms - expected_rms).abs();
        assert!(
            rms_diff <= tol,
            "rms mismatch: actual={rms}, expected={expected_rms}, diff={rms_diff}, tol={tol}"
        );
        return;
    }

    macro_rules! assert_mono_f32_packets {
        ($params:expr, $samples:expr, [ $( ($pkt:expr, $head:expr, $tail:expr, $rms:expr $(,)?) ),+ $(,)? ]) => {{
            let mut dec = OpusDecoder::try_new($params, &AudioDecoderOptions::default()).unwrap();
            for (pkt_bytes, head, tail, rms) in [ $( (&$pkt[..], &$head, &$tail, $rms) ),+ ] {
                let pkt = PacketRef::new(0, Timestamp::from(0u32), Duration::from($samples as u64), pkt_bytes);
                dec.decode_ref(&pkt).unwrap();
                let ch0 = dec.buffer.plane(0).unwrap();
                assert_plane_matches_f32_oracle(ch0, $samples, head, tail, rms, 1e-4);
            }
        }};
    }

    macro_rules! assert_stereo_f32_packets {
        ($params:expr, $samples:expr, [ $( ($pkt:expr, $l_head:expr, $l_tail:expr, $l_rms:expr, $r_head:expr, $r_tail:expr, $r_rms:expr $(,)?) ),+ $(,)? ]) => {{
            let mut dec = OpusDecoder::try_new($params, &AudioDecoderOptions::default()).unwrap();
            for (pkt_bytes, l_head, l_tail, l_rms, r_head, r_tail, r_rms) in [
                $( (&$pkt[..], &$l_head, &$l_tail, $l_rms, &$r_head, &$r_tail, $r_rms) ),+
            ] {
                let pkt = PacketRef::new(0, Timestamp::from(0u32), Duration::from($samples as u64), pkt_bytes);
                dec.decode_ref(&pkt).unwrap();
                let ch0 = dec.buffer.plane(0).unwrap();
                let ch1 = dec.buffer.plane(1).unwrap();
                assert_plane_matches_f32_oracle(ch0, $samples, l_head, l_tail, l_rms, 1e-4);
                assert_plane_matches_f32_oracle(ch1, $samples, r_head, r_tail, r_rms, 1e-4);
            }
        }};
    }

    #[test]
    fn opus_decoder_initialization() {
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(
                Position::FRONT_LEFT | Position::FRONT_RIGHT,
            ));

        let mut decoder = OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();

        assert_eq!(decoder.codec_info().short_name, "opus");
        assert_eq!(decoder.codec_params().codec, CODEC_ID_OPUS);
        assert_eq!(decoder.last_decoded().frames(), 0);
        assert!(decoder.finalize().verify_ok.is_none());

        decoder.reset();

        assert_eq!(OpusDecoder::supported_codecs().len(), 1);
        assert_eq!(OpusDecoder::supported_codecs()[0].id, CODEC_ID_OPUS);
        return;
    }

    #[test]
    fn opus_decoder_invalid_codec() {
        let mut params = AudioCodecParameters::new();
        params.for_codec(CODEC_ID_VORBIS);

        let result = OpusDecoder::try_new(&params, &AudioDecoderOptions::default());
        assert!(result.is_err());
        return;
    }

    #[test]
    fn opus_decoder_end_to_end_mono_and_stereo_silk_packets_match_libopus() {
        const MONO_WB_PKT_0: [u8; 47] = [
            0x48, 0x82, 0xb6, 0x0a, 0x70, 0x7e, 0xfb, 0xbd, 0xd2, 0x4d, 0x85, 0xcd, 0x78, 0xed,
            0xb6, 0xdf, 0x64, 0x19, 0xe8, 0x13, 0x99, 0x45, 0x17, 0x57, 0xcb, 0xc7, 0x4b, 0xf0,
            0x86, 0xdc, 0x1b, 0xe2, 0x1a, 0xc0, 0xd0, 0x76, 0xa0, 0xd1, 0xbd, 0x0d, 0x17, 0x34,
            0xc7, 0x2b, 0xba, 0x74, 0xa0,
        ];
        const MONO_WB_OUT_0_HEAD: [i16; 24] = [0; 24];
        const MONO_WB_OUT_0_TAIL: [i16; 24] = [
            4426, 4426, 4429, 4427, 4415, 4385, 4336, 4276, 4219, 4182, 4169, 4175, 4181, 4169,
            4133, 4076, 4010, 3948, 3899, 3863, 3837, 3815, 3793, 3767,
        ];

        const MONO_WB_PKT_1: [u8; 73] = [
            0x48, 0xad, 0xe4, 0x00, 0xd2, 0x1d, 0x46, 0x85, 0x7a, 0x36, 0x9b, 0x87, 0xb0, 0x89,
            0xa5, 0xf7, 0xb4, 0xfd, 0x0f, 0x78, 0x05, 0x65, 0x14, 0x71, 0x11, 0x88, 0x36, 0xbc,
            0x0f, 0xcd, 0x35, 0xa1, 0xe4, 0xdc, 0x77, 0xf2, 0xf8, 0x4c, 0x65, 0x3a, 0xb0, 0x69,
            0x4b, 0x94, 0x60, 0xcc, 0xe8, 0x15, 0x48, 0x64, 0xff, 0xec, 0xf3, 0xfb, 0xc2, 0x18,
            0x1f, 0x5d, 0xf2, 0x27, 0x6d, 0x86, 0x26, 0x66, 0x65, 0x5d, 0x41, 0xc7, 0x12, 0xd2,
            0x61, 0x69, 0x62,
        ];
        const MONO_WB_OUT_1_HEAD: [i16; 24] = [
            3732, 3683, 3622, 3558, 3500, 3455, 3421, 3385, 3337, 3273, 3204, 3144, 3105, 3083,
            3071, 3060, 3046, 3031, 3012, 2985, 2947, 2907, 2880, 2877,
        ];
        const MONO_WB_OUT_1_TAIL: [i16; 24] = [
            -1599, -1137, -659, -176, 302, 774, 1248, 1732, 2230, 2738, 3245, 3744, 4232, 4711,
            5186, 5660, 6134, 6601, 7053, 7487, 7900, 8298, 8687, 9067,
        ];

        const MONO_WB_PKT_2: [u8; 27] = [
            0x48, 0x9d, 0x68, 0xf6, 0x16, 0x52, 0xd4, 0x44, 0x2e, 0x8f, 0xec, 0xfa, 0x23, 0xdb,
            0x55, 0xd6, 0x7c, 0x00, 0x4e, 0xe8, 0x89, 0x69, 0x21, 0x5b, 0x61, 0x48, 0x80,
        ];
        const MONO_WB_OUT_2_HEAD: [i16; 24] = [
            9436, 9787, 10117, 10426, 10716, 10994, 11255, 11495, 11707, 11892, 12056, 12204,
            12336, 12448, 12534, 12592, 12630, 12658, 12679, 12683, 12660, 12598, 12503, 12392,
        ];
        const MONO_WB_OUT_2_TAIL: [i16; 24] = [
            -4111, -4172, -4228, -4278, -4322, -4358, -4389, -4415, -4437, -4456, -4472, -4488,
            -4502, -4513, -4518, -4516, -4510, -4501, -4492, -4485, -4483, -4485, -4488, -4490,
        ];

        let mut mono_params = AudioCodecParameters::new();
        mono_params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(Position::FRONT_CENTER));
        let mut mono_decoder =
            OpusDecoder::try_new(&mono_params, &AudioDecoderOptions::default()).unwrap();

        for (pkt_bytes, expected_head, expected_tail) in [
            (&MONO_WB_PKT_0[..], &MONO_WB_OUT_0_HEAD, &MONO_WB_OUT_0_TAIL),
            (&MONO_WB_PKT_1[..], &MONO_WB_OUT_1_HEAD, &MONO_WB_OUT_1_TAIL),
            (&MONO_WB_PKT_2[..], &MONO_WB_OUT_2_HEAD, &MONO_WB_OUT_2_TAIL),
        ] {
            let pkt = PacketRef::new(0, Timestamp::from(0u32), Duration::from(960u64), pkt_bytes);
            mono_decoder.decode_ref(&pkt).unwrap();
            assert_eq!(mono_decoder.buffer.frames(), 960);
            let ch0 = mono_decoder.buffer.plane(0).unwrap();
            assert_eq!(&quantize_i16_24(&ch0[..24]), expected_head);
            assert_eq!(&quantize_i16_24(&ch0[960 - 24..]), expected_tail);
        }

        const STEREO_MB_PKT_0: [u8; 83] = [
            0x2c, 0xa4, 0xd1, 0xb0, 0xc4, 0xb7, 0x87, 0xab, 0x55, 0x78, 0x1c, 0x0b, 0x60, 0x22,
            0xff, 0x6b, 0xf3, 0x0a, 0xae, 0xb0, 0x53, 0x72, 0x31, 0xd3, 0x5a, 0xdb, 0x47, 0x3d,
            0x29, 0x53, 0xd7, 0xcc, 0x19, 0x14, 0x89, 0x5a, 0x36, 0xf2, 0xf7, 0x69, 0xfb, 0x5a,
            0x69, 0xca, 0x00, 0x46, 0x96, 0x77, 0xb4, 0xf1, 0xa5, 0x26, 0x2d, 0xe3, 0x66, 0x13,
            0x9b, 0xb4, 0x1c, 0x8e, 0x34, 0x8f, 0xca, 0x48, 0x87, 0xd8, 0x61, 0x43, 0xf2, 0xe4,
            0xc2, 0xe0, 0x7e, 0x8a, 0x88, 0x82, 0xb6, 0x7d, 0xa8, 0x07, 0xe8, 0x97, 0x58,
        ];
        const STEREO_MB_OUT_L_0_TAIL: [i16; 24] = [
            -2480, -2233, -2000, -1777, -1558, -1337, -1109, -875, -639, -403, -174, 47, 260, 469,
            674, 878, 1079, 1276, 1464, 1645, 1818, 1986, 2151, 2317,
        ];
        const STEREO_MB_OUT_R_0_TAIL: [i16; 24] = [
            1806, 1686, 1557, 1414, 1254, 1080, 899, 718, 543, 370, 194, 7, -193, -405, -620, -830,
            -1029, -1217, -1398, -1583, -1776, -1979, -2189, -2399,
        ];

        const STEREO_MB_PKT_1: [u8; 81] = [
            0x2c, 0xa9, 0x89, 0x67, 0x1d, 0xbe, 0x46, 0x11, 0x74, 0x23, 0x93, 0x7f, 0xec, 0x8b,
            0xb7, 0x10, 0x4e, 0x98, 0xe6, 0x5d, 0x20, 0x79, 0xff, 0x19, 0x92, 0xcf, 0xf5, 0xaa,
            0x90, 0xe3, 0x6b, 0x05, 0x4f, 0x03, 0x95, 0xfc, 0xfc, 0xbc, 0xf8, 0x45, 0xc6, 0x9d,
            0x87, 0x57, 0x02, 0x67, 0x21, 0xba, 0x5f, 0xbf, 0x7a, 0xaa, 0x2e, 0xf4, 0xbc, 0x6e,
            0xd6, 0x3f, 0xb3, 0x34, 0x02, 0x2e, 0x8a, 0xf8, 0x8e, 0x43, 0x6e, 0xfd, 0x81, 0x3f,
            0xd7, 0xf1, 0xd1, 0x97, 0xd2, 0x79, 0x6e, 0x19, 0x0b, 0x30, 0x60,
        ];
        const STEREO_MB_OUT_L_1_TAIL: [i16; 24] = [
            3192, 3168, 3148, 3133, 3123, 3117, 3116, 3120, 3128, 3139, 3149, 3159, 3167, 3174,
            3180, 3185, 3190, 3192, 3190, 3183, 3170, 3154, 3136, 3119,
        ];
        const STEREO_MB_OUT_R_1_TAIL: [i16; 24] = [
            -697, -391, -81, 233, 549, 870, 1191, 1512, 1830, 2145, 2455, 2760, 3061, 3360, 3657,
            3953, 4247, 4535, 4817, 5091, 5355, 5612, 5860, 6102,
        ];

        let mut stereo_params = AudioCodecParameters::new();
        stereo_params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(
                Position::FRONT_LEFT | Position::FRONT_RIGHT,
            ));
        let mut stereo_decoder =
            OpusDecoder::try_new(&stereo_params, &AudioDecoderOptions::default()).unwrap();

        for (pkt_bytes, expected_l_tail, expected_r_tail) in [
            (
                &STEREO_MB_PKT_0[..],
                &STEREO_MB_OUT_L_0_TAIL,
                &STEREO_MB_OUT_R_0_TAIL,
            ),
            (
                &STEREO_MB_PKT_1[..],
                &STEREO_MB_OUT_L_1_TAIL,
                &STEREO_MB_OUT_R_1_TAIL,
            ),
        ] {
            let pkt = PacketRef::new(0, Timestamp::from(0u32), Duration::from(960u64), pkt_bytes);
            stereo_decoder.decode_ref(&pkt).unwrap();
            assert_eq!(stereo_decoder.buffer.frames(), 960);
            let ch0 = stereo_decoder.buffer.plane(0).unwrap();
            let ch1 = stereo_decoder.buffer.plane(1).unwrap();
            assert_eq!(&quantize_i16_24(&ch0[960 - 24..]), expected_l_tail);
            assert_eq!(&quantize_i16_24(&ch1[960 - 24..]), expected_r_tail);
        }
        return;
    }
    // Generated by gen_opus_oracle.c linked against libopus 1.6.1 (float).
    // Reference test vectors for Issues #9 and #10 (Hybrid, CELT-only, Mode Transitions, Multi-Stream).

    // ============================================================================
    // HYBRID_MONO_SWB_10MS (channels=1, frame_size=480, bitrate=32000)
    // ============================================================================
    // TOC = 0x60 (config=12, stereo=0, code=0)
    const HYBRID_MONO_SWB_10MS_PKT_0: [u8; 40] = [
        0x60, 0x85, 0x07, 0x40, 0xfa, 0xfa, 0x53, 0xe6, 0xc0, 0x9f, 0x7d, 0xb5,
        0x6a, 0x50, 0xb8, 0x9a, 0x57, 0x2e, 0x95, 0x10, 0x3b, 0x56, 0x12, 0x83,
        0x7c, 0x87, 0xb4, 0xb0, 0x0a, 0x31, 0x4a, 0xb9, 0xae, 0xbf, 0x1c, 0x93,
        0xa5, 0x2f, 0xeb, 0x7b,
    ];
    const HYBRID_MONO_SWB_10MS_OUT_0_HEAD_F32: [f32; 24] = [
        0.00000007f32, -0.00000114f32, -0.00000521f32, -0.0000038f32, 0.00001082f32, 0.00002302f32,
        0.00000694f32, -0.00003006f32, -0.00004233f32, -0.00000185f32, 0.00005563f32, 0.00006189f32,
        -0.00000522f32, -0.00008528f32, -0.0000866f32, 0.00001575f32, 0.00012985f32, 0.00011881f32,
        -0.00004301f32, -0.00019628f32, -0.00014283f32, 0.00010289f32, 0.00027254f32, 0.00013643f32,
    ];
    const HYBRID_MONO_SWB_10MS_OUT_0_TAIL_F32: [f32; 24] = [
        -0.04202562f32, -0.14788361f32, -0.16255501f32, -0.08661938f32, 0.02392347f32, 0.09721507f32,
        0.10178745f32, 0.07319634f32, 0.07801814f32, 0.1458706f32, 0.23772083f32, 0.2795248f32,
        0.22537272f32, 0.0950497f32, -0.03880129f32, -0.10236387f32, -0.07659539f32, -0.010488f32,
        0.02521396f32, 0.00346319f32, -0.04064683f32, -0.05211462f32, -0.01059853f32, 0.05346409f32,
    ];
    const HYBRID_MONO_SWB_10MS_OUT_0_RMS: f32 = 0.15902945f32;

    // TOC = 0x60 (config=12, stereo=0, code=0)
    const HYBRID_MONO_SWB_10MS_PKT_1: [u8; 40] = [
        0x60, 0xb8, 0x4e, 0xed, 0x06, 0xe9, 0x1f, 0x87, 0x3c, 0x6c, 0xcb, 0x7f,
        0xef, 0x2d, 0xa9, 0xff, 0x43, 0x39, 0x2e, 0xd9, 0x78, 0xcd, 0xf8, 0xed,
        0xb9, 0xe0, 0xf4, 0xe5, 0x0e, 0x0c, 0x92, 0x47, 0xb5, 0x0e, 0x3c, 0x4f,
        0xdc, 0xd2, 0x53, 0xf3,
    ];
    const HYBRID_MONO_SWB_10MS_OUT_1_HEAD_F32: [f32; 24] = [
        0.08147597f32, 0.02595682f32, -0.116289f32, -0.29376045f32, -0.4313706f32, -0.4847077f32,
        -0.47073346f32, -0.4443053f32, -0.44756484f32, -0.47829276f32, -0.4970349f32, -0.4607192f32,
        -0.3637759f32, -0.25545922f32, -0.21047346f32, -0.26628187f32, -0.38635236f32, -0.49070123f32,
        -0.5226791f32, -0.48751393f32, -0.43349347f32, -0.40501693f32, -0.4076105f32, -0.40454397f32,
    ];
    const HYBRID_MONO_SWB_10MS_OUT_1_TAIL_F32: [f32; 24] = [
        -0.31958804f32, -0.4426717f32, -0.4667184f32, -0.40527508f32, -0.32404804f32, -0.28640732f32,
        -0.30247545f32, -0.32503986f32, -0.2967789f32, -0.2095612f32, -0.12113837f32, -0.11209338f32,
        -0.21651803f32, -0.38743806f32, -0.5289647f32, -0.56848174f32, -0.50909317f32, -0.41753435f32,
        -0.36079955f32, -0.34862822f32, -0.33016858f32, -0.24590899f32, -0.08689242f32, 0.0899343f32,
    ];
    const HYBRID_MONO_SWB_10MS_OUT_1_RMS: f32 = 0.29587138f32;

    // TOC = 0x60 (config=12, stereo=0, code=0)
    const HYBRID_MONO_SWB_10MS_PKT_2: [u8; 40] = [
        0x60, 0xaf, 0x60, 0x8a, 0xa3, 0xb3, 0x6d, 0xff, 0xa7, 0x13, 0xed, 0x83,
        0x1c, 0xb3, 0x63, 0xca, 0xd7, 0xd1, 0xb1, 0x00, 0xbd, 0xb5, 0x05, 0x26,
        0x3a, 0x8f, 0xa4, 0xd7, 0xc9, 0x7e, 0x24, 0x05, 0xbb, 0x69, 0xf2, 0x5f,
        0x1a, 0xfb, 0x7a, 0x8e,
    ];
    const HYBRID_MONO_SWB_10MS_OUT_2_HEAD_F32: [f32; 24] = [
        0.20224115f32, 0.20458542f32, 0.12118077f32, 0.02673205f32, -0.00903838f32, 0.03164082f32,
        0.11012148f32, 0.17002007f32, 0.18654157f32, 0.18830879f32, 0.2320044f32, 0.35068676f32,
        0.5164816f32, 0.6520143f32, 0.68427676f32, 0.6004657f32, 0.45881534f32, 0.34357607f32,
        0.30050156f32, 0.30644426f32, 0.29867125f32, 0.23771623f32, 0.14778544f32, 0.09856714f32,
    ];
    const HYBRID_MONO_SWB_10MS_OUT_2_TAIL_F32: [f32; 24] = [
        0.03333003f32, -0.00867699f32, 0.03974438f32, 0.14400055f32, 0.24222231f32, 0.29392305f32,
        0.30972606f32, 0.33750424f32, 0.41597965f32, 0.5333049f32, 0.62892455f32, 0.6386599f32,
        0.549331f32, 0.41470394f32, 0.31683487f32, 0.30417782f32, 0.35742497f32, 0.41181844f32,
        0.41574603f32, 0.37450546f32, 0.3414893f32, 0.3659154f32, 0.44461864f32, 0.5190324f32,
    ];
    const HYBRID_MONO_SWB_10MS_OUT_2_RMS: f32 = 0.31196588f32;

    // ============================================================================
    // HYBRID_MONO_FB_20MS (channels=1, frame_size=960, bitrate=40000)
    // ============================================================================
    // TOC = 0x78 (config=15, stereo=0, code=0)
    const HYBRID_MONO_FB_20MS_PKT_0: [u8; 100] = [
        0x78, 0x85, 0x2a, 0xae, 0x47, 0xfd, 0x4f, 0x18, 0xf5, 0xd9, 0x5d, 0xd3,
        0xbb, 0xad, 0x54, 0xb4, 0xd9, 0xa2, 0xdc, 0xc7, 0xf5, 0x61, 0x89, 0x44,
        0x48, 0xe1, 0xc0, 0x4b, 0xc3, 0x25, 0xd8, 0x4c, 0x8e, 0x13, 0x38, 0xda,
        0xbc, 0x99, 0xf2, 0xfd, 0x00, 0x43, 0x72, 0x6b, 0xc2, 0x39, 0xc9, 0xc5,
        0xfd, 0xc8, 0x03, 0x44, 0x0e, 0xc2, 0xff, 0x62, 0xc2, 0xcb, 0xab, 0x77,
        0xe2, 0x48, 0x44, 0xa3, 0xf8, 0xa4, 0x20, 0xde, 0xa2, 0x07, 0xb5, 0x60,
        0xba, 0xcc, 0xae, 0x62, 0x7d, 0xa1, 0x92, 0x3d, 0xa7, 0xb1, 0x64, 0xa8,
        0xf7, 0x7a, 0xcd, 0x2a, 0x85, 0xbb, 0x35, 0xac, 0x9b, 0xe9, 0xb9, 0x57,
        0x06, 0xd9, 0x6b, 0xbf,
    ];
    const HYBRID_MONO_FB_20MS_OUT_0_HEAD_F32: [f32; 24] = [
        -0.0f32, -0.0f32, -0.0f32, 0.0f32, 0.0f32, -0.0f32,
        -0.0f32, 0.0f32, 0.0f32, -0.0f32, -0.0f32, 0.0f32,
        -0.0f32, -0.0f32, -0.0f32, 0.0f32, 0.0f32, -0.0f32,
        -0.0f32, 0.0f32, 0.0f32, -0.0f32, -0.0f32, 0.0f32,
    ];
    const HYBRID_MONO_FB_20MS_OUT_0_TAIL_F32: [f32; 24] = [
        -0.2955996f32, -0.42845795f32, -0.46513215f32, -0.39835918f32, -0.29119882f32, -0.22702183f32,
        -0.24243581f32, -0.30245578f32, -0.33580655f32, -0.30188555f32, -0.23145731f32, -0.20260789f32,
        -0.27186194f32, -0.42234394f32, -0.5714938f32, -0.63172793f32, -0.5767004f32, -0.45485273f32,
        -0.34458464f32, -0.29165235f32, -0.27747086f32, -0.24224328f32, -0.14281407f32, 0.0063291f32,
    ];
    const HYBRID_MONO_FB_20MS_OUT_0_RMS: f32 = 0.2520697f32;

    // TOC = 0x78 (config=15, stereo=0, code=0)
    const HYBRID_MONO_FB_20MS_PKT_1: [u8; 100] = [
        0x78, 0xb1, 0x57, 0x92, 0x93, 0xa0, 0xa7, 0x70, 0xf8, 0x84, 0x90, 0x48,
        0xaf, 0x6d, 0xc2, 0x8c, 0xef, 0x5b, 0xe8, 0x0e, 0xfe, 0x75, 0x22, 0x00,
        0x53, 0xa5, 0x21, 0x6e, 0x91, 0x08, 0xc1, 0x26, 0xdf, 0x5c, 0x17, 0x44,
        0x4c, 0x66, 0x87, 0x20, 0x30, 0xf4, 0x8d, 0x47, 0xde, 0xe3, 0x9b, 0x82,
        0x6d, 0x12, 0xa8, 0xf9, 0x30, 0x82, 0xa2, 0x91, 0x0a, 0xe2, 0x32, 0x6d,
        0xd7, 0x13, 0x32, 0x7b, 0xeb, 0x3d, 0x58, 0xf2, 0x86, 0x5f, 0xc8, 0xf6,
        0x66, 0xd7, 0x74, 0x81, 0xdd, 0x5f, 0xa8, 0x4c, 0xa7, 0x67, 0xea, 0x63,
        0x55, 0x72, 0xc9, 0x3c, 0x31, 0x90, 0x8e, 0x54, 0xa9, 0x0d, 0x38, 0xcd,
        0x59, 0xd7, 0xea, 0x83,
    ];
    const HYBRID_MONO_FB_20MS_OUT_1_HEAD_F32: [f32; 24] = [
        0.13952899f32, 0.18819332f32, 0.13477314f32, 0.03117897f32, -0.03784001f32, -0.0148243f32,
        0.09057961f32, 0.21164075f32, 0.2813831f32, 0.28853977f32, 0.28377864f32, 0.33309677f32,
        0.45421216f32, 0.5911134f32, 0.6545852f32, 0.594122f32, 0.440602f32, 0.28472257f32,
        0.20671128f32, 0.21619989f32, 0.25501338f32, 0.25381184f32, 0.19384582f32, 0.12279637f32,
    ];
    const HYBRID_MONO_FB_20MS_OUT_1_TAIL_F32: [f32; 24] = [
        -0.11675392f32, -0.24918589f32, -0.2790742f32, -0.22025305f32, -0.1449061f32, -0.12630966f32,
        -0.1818164f32, -0.262759f32, -0.3010406f32, -0.27350786f32, -0.22712529f32, -0.24151447f32,
        -0.35903078f32, -0.5430967f32, -0.7017102f32, -0.7567788f32, -0.70254594f32, -0.60329497f32,
        -0.53632057f32, -0.52941805f32, -0.5441547f32, -0.5162723f32, -0.41634205f32, -0.27901748f32,
    ];
    const HYBRID_MONO_FB_20MS_OUT_1_RMS: f32 = 0.31372604f32;

    // TOC = 0x78 (config=15, stereo=0, code=0)
    const HYBRID_MONO_FB_20MS_PKT_2: [u8; 100] = [
        0x78, 0xa7, 0xc0, 0x32, 0x21, 0x6b, 0xbb, 0x55, 0x82, 0xac, 0x66, 0xea,
        0x4a, 0x0b, 0x00, 0x59, 0xc1, 0xc6, 0x6d, 0x39, 0xb5, 0x14, 0x37, 0xa0,
        0xdc, 0x36, 0x5e, 0x2f, 0x0f, 0x69, 0x2b, 0x28, 0xd2, 0x90, 0xa7, 0xed,
        0x1c, 0xb8, 0xbf, 0x5a, 0x04, 0x90, 0x89, 0x23, 0x8a, 0xdd, 0x90, 0xf9,
        0xb0, 0x14, 0xf5, 0xca, 0x33, 0x0d, 0x56, 0x0e, 0xc5, 0xd8, 0xa9, 0x52,
        0x82, 0x9d, 0xb3, 0xd0, 0xe1, 0x06, 0x28, 0x84, 0xc8, 0x66, 0xac, 0x51,
        0xaf, 0xda, 0x08, 0x62, 0x1a, 0xdd, 0x2d, 0xf3, 0x4a, 0x17, 0x2a, 0xba,
        0xfa, 0x2c, 0xf8, 0x9b, 0xd0, 0x48, 0x08, 0xe5, 0x4a, 0x06, 0x9c, 0x66,
        0xa0, 0x57, 0xe9, 0xe7,
    ];
    const HYBRID_MONO_FB_20MS_OUT_2_HEAD_F32: [f32; 24] = [
        -0.17920187f32, -0.17396395f32, -0.25795504f32, -0.3646408f32, -0.41421548f32, -0.37009025f32,
        -0.262526f32, -0.16086085f32, -0.11642921f32, -0.12038919f32, -0.1135864f32, -0.04072123f32,
        0.09477489f32, 0.2240005f32, 0.26298675f32, 0.18135433f32, 0.03018847f32, -0.09648248f32,
        -0.1345559f32, -0.09670096f32, -0.053046f32, -0.06461038f32, -0.12817338f32, -0.18009104f32,
    ];
    const HYBRID_MONO_FB_20MS_OUT_2_TAIL_F32: [f32; 24] = [
        0.29678833f32, 0.1698662f32, 0.136678f32, 0.18654825f32, 0.2544155f32, 0.27239996f32,
        0.22231956f32, 0.14631626f32, 0.10560403f32, 0.12162383f32, 0.1524167f32, 0.12554939f32,
        0.00137758f32, -0.1878439f32, -0.35755268f32, -0.43376228f32, -0.40704116f32, -0.33284235f32,
        -0.28198537f32, -0.28319836f32, -0.30553016f32, -0.29263717f32, -0.21683165f32, -0.1078036f32,
    ];
    const HYBRID_MONO_FB_20MS_OUT_2_RMS: f32 = 0.30947834f32;

    // ============================================================================
    // HYBRID_STEREO_SWB_20MS (channels=2, frame_size=960, bitrate=48000)
    // ============================================================================
    // TOC = 0x6c (config=13, stereo=1, code=0)
    const HYBRID_STEREO_SWB_20MS_PKT_0: [u8; 120] = [
        0x6c, 0xa9, 0x52, 0xb6, 0x24, 0x78, 0xf3, 0x26, 0x03, 0xe5, 0x86, 0x5d,
        0xeb, 0xe0, 0x6c, 0x52, 0x89, 0x84, 0x6d, 0x0e, 0xa1, 0xbe, 0x55, 0x7e,
        0x02, 0x47, 0xeb, 0x4b, 0x4a, 0x33, 0xab, 0xd6, 0xd4, 0xf1, 0x6c, 0xa1,
        0x0d, 0x85, 0xb2, 0xd5, 0xd3, 0x4a, 0xb0, 0xda, 0xa3, 0xf3, 0xe6, 0x77,
        0xf6, 0x5d, 0x9e, 0xce, 0x93, 0x5f, 0xa5, 0x9c, 0x79, 0x99, 0x14, 0x62,
        0xed, 0xba, 0x71, 0xe6, 0x01, 0x23, 0xd4, 0x6f, 0x86, 0x92, 0xd2, 0xa5,
        0x9b, 0xcd, 0x37, 0x55, 0x77, 0xaf, 0x90, 0xc8, 0x2d, 0xe8, 0x92, 0xad,
        0xf4, 0x59, 0x3a, 0x44, 0x4f, 0xeb, 0x1d, 0xf6, 0x92, 0x1f, 0x7a, 0xf2,
        0x45, 0x83, 0xe0, 0x7b, 0xb7, 0x88, 0x7c, 0x34, 0x56, 0x5e, 0x66, 0x8d,
        0xac, 0xe4, 0xe6, 0xc5, 0x0a, 0xad, 0x94, 0x10, 0x84, 0x55, 0xc5, 0xb7,
    ];
    const HYBRID_STEREO_SWB_20MS_OUT_L_0_HEAD_F32: [f32; 24] = [
        0.0f32, -0.0f32, -0.0f32, -0.0f32, 0.0f32, 0.0f32,
        -0.0f32, -0.0f32, 0.0f32, 0.0f32, 0.0f32, -0.0f32,
        -0.0f32, 0.0f32, 0.0f32, 0.0f32, -0.0f32, -0.0f32,
        0.0f32, 0.0f32, 0.0f32, -0.0f32, -0.0f32, -0.0f32,
    ];
    const HYBRID_STEREO_SWB_20MS_OUT_L_0_TAIL_F32: [f32; 24] = [
        -0.32421038f32, -0.40358084f32, -0.40449864f32, -0.3322821f32, -0.24667682f32, -0.21474817f32,
        -0.25489864f32, -0.32067895f32, -0.34068453f32, -0.28266385f32, -0.1846967f32, -0.12493148f32,
        -0.15468545f32, -0.25406998f32, -0.3485347f32, -0.37075874f32, -0.31407318f32, -0.23101903f32,
        -0.1827846f32, -0.18497024f32, -0.19695564f32, -0.15963162f32, -0.04866211f32, 0.10240816f32,
    ];
    const HYBRID_STEREO_SWB_20MS_OUT_L_0_RMS: f32 = 0.21162118f32;
    const HYBRID_STEREO_SWB_20MS_OUT_R_0_HEAD_F32: [f32; 24] = [
        0.0f32, -0.0f32, -0.0f32, -0.0f32, 0.0f32, 0.0f32,
        -0.0f32, -0.0f32, -0.0f32, -0.0f32, -0.0f32, 0.0f32,
        0.0f32, 0.0f32, -0.0f32, -0.0f32, 0.0f32, 0.0f32,
        -0.0f32, -0.0f32, -0.0f32, 0.0f32, 0.0f32, -0.0f32,
    ];
    const HYBRID_STEREO_SWB_20MS_OUT_R_0_TAIL_F32: [f32; 24] = [
        0.14838664f32, 0.1173456f32, 0.10825685f32, 0.01912367f32, -0.15215817f32, -0.2907852f32,
        -0.2861691f32, -0.15692566f32, -0.03850814f32, -0.04528151f32, -0.1569544f32, -0.2531212f32,
        -0.25456274f32, -0.20953798f32, -0.22863404f32, -0.34376755f32, -0.4554929f32, -0.43419716f32,
        -0.26537788f32, -0.07511441f32, -0.00239158f32, -0.05715144f32, -0.1204516f32, -0.08629186f32,
    ];
    const HYBRID_STEREO_SWB_20MS_OUT_R_0_RMS: f32 = 0.18528086f32;

    // TOC = 0x6c (config=13, stereo=1, code=0)
    const HYBRID_STEREO_SWB_20MS_PKT_1: [u8; 120] = [
        0x6c, 0xa9, 0x58, 0x10, 0x33, 0xbb, 0x9e, 0x94, 0x75, 0x00, 0x49, 0x8e,
        0x66, 0xca, 0xfc, 0xe3, 0xfe, 0x73, 0x05, 0xd8, 0xa2, 0xcc, 0xa2, 0x8d,
        0x31, 0x19, 0xfa, 0x9d, 0x7a, 0x50, 0x47, 0xf1, 0x4b, 0xf4, 0x15, 0xec,
        0xee, 0x12, 0xdc, 0x8a, 0x5b, 0x0d, 0x89, 0x44, 0x6e, 0x53, 0x13, 0x94,
        0x28, 0xab, 0x6d, 0x02, 0xbf, 0x68, 0x83, 0xca, 0xc0, 0x97, 0xd4, 0x7e,
        0x96, 0xa8, 0x5e, 0x44, 0x9d, 0xfe, 0x2e, 0x46, 0x09, 0x18, 0x2f, 0x57,
        0x1d, 0x94, 0x73, 0x09, 0x76, 0x7b, 0x4a, 0x06, 0x15, 0x3b, 0x94, 0xa8,
        0xad, 0xc5, 0x37, 0x16, 0x0d, 0xe0, 0xef, 0x77, 0x0b, 0x4f, 0x0d, 0x77,
        0xe4, 0x60, 0x06, 0x49, 0xf5, 0x49, 0x73, 0x59, 0x45, 0xb8, 0xc6, 0x18,
        0x65, 0x27, 0x02, 0x57, 0x2c, 0x5f, 0x74, 0xd8, 0x24, 0x30, 0xd8, 0x23,
    ];
    const HYBRID_STEREO_SWB_20MS_OUT_L_1_HEAD_F32: [f32; 24] = [
        0.22436742f32, 0.26163507f32, 0.20986953f32, 0.11706021f32, 0.04842292f32, 0.04206477f32,
        0.08710435f32, 0.13876392f32, 0.15937033f32, 0.15336485f32, 0.1650784f32, 0.23941663f32,
        0.3746412f32, 0.5105957f32, 0.56713545f32, 0.5054815f32, 0.36196995f32, 0.22216648f32,
        0.15651497f32, 0.16992475f32, 0.20836568f32, 0.21452296f32, 0.18086745f32, 0.15397608f32,
    ];
    const HYBRID_STEREO_SWB_20MS_OUT_L_1_TAIL_F32: [f32; 24] = [
        0.05435362f32, -0.0696411f32, -0.12439288f32, -0.13103509f32, -0.14533544f32, -0.20766222f32,
        -0.3056833f32, -0.38193098f32, -0.38309863f32, -0.3116171f32, -0.23298939f32, -0.22938679f32,
        -0.33481035f32, -0.5059416f32, -0.6541964f32, -0.71316004f32, -0.6856019f32, -0.6307655f32,
        -0.60658234f32, -0.61758256f32, -0.61356574f32, -0.5380505f32, -0.38425413f32, -0.20937434f32,
    ];
    const HYBRID_STEREO_SWB_20MS_OUT_L_1_RMS: f32 = 0.33603913f32;
    const HYBRID_STEREO_SWB_20MS_OUT_R_1_HEAD_F32: [f32; 24] = [
        0.01446877f32, 0.04947144f32, -0.05893438f32, -0.23200211f32, -0.30734882f32, -0.21094254f32,
        -0.04039817f32, 0.03419143f32, -0.05422112f32, -0.20791796f32, -0.28073087f32, -0.24075617f32,
        -0.1989124f32, -0.2742548f32, -0.4489703f32, -0.5764973f32, -0.5350735f32, -0.35692304f32,
        -0.19283026f32, -0.15344599f32, -0.20232464f32, -0.21426316f32, -0.12574589f32, -0.00528952f32,
    ];
    const HYBRID_STEREO_SWB_20MS_OUT_R_1_TAIL_F32: [f32; 24] = [
        0.2381251f32, 0.16285184f32, 0.13416776f32, 0.04675083f32, -0.12896265f32, -0.2980357f32,
        -0.337512f32, -0.23309489f32, -0.10305595f32, -0.08135422f32, -0.18582386f32, -0.3127177f32,
        -0.3592119f32, -0.33281842f32, -0.32722247f32, -0.3965888f32, -0.47681332f32, -0.44768474f32,
        -0.2695315f32, -0.03958458f32, 0.09861861f32, 0.0953997f32, 0.03199154f32, 0.01634113f32,
    ];
    const HYBRID_STEREO_SWB_20MS_OUT_R_1_RMS: f32 = 0.2924892f32;

    // ============================================================================
    // HYBRID_STEREO_FB_20MS (channels=2, frame_size=960, bitrate=64000)
    // ============================================================================
    // TOC = 0x7c (config=15, stereo=1, code=0)
    const HYBRID_STEREO_FB_20MS_PKT_0: [u8; 160] = [
        0x7c, 0xa9, 0x52, 0xb2, 0x91, 0x3d, 0x2f, 0x26, 0x03, 0xe5, 0x86, 0x5d,
        0xec, 0x6d, 0x85, 0xfa, 0x11, 0xc5, 0xe4, 0x92, 0x65, 0x12, 0x41, 0xa3,
        0x5c, 0x93, 0xee, 0xba, 0x8f, 0xf2, 0x07, 0x73, 0x6b, 0x1d, 0xbf, 0xcb,
        0x3b, 0x8c, 0xc9, 0xf9, 0x88, 0x0e, 0x99, 0xef, 0x34, 0x55, 0xcd, 0x05,
        0x88, 0x99, 0xd8, 0x9f, 0x6e, 0x73, 0x81, 0x6b, 0x9d, 0x51, 0x41, 0x79,
        0xec, 0x8d, 0x0c, 0x76, 0x9e, 0xd3, 0x8f, 0x3f, 0xad, 0xd4, 0xa6, 0xca,
        0x69, 0x53, 0x2f, 0x88, 0x3e, 0xfc, 0x7d, 0xe9, 0xc1, 0x5e, 0x2a, 0xd5,
        0x42, 0x38, 0xbe, 0x2e, 0xbf, 0x97, 0xf6, 0xf4, 0xe1, 0x67, 0x72, 0xe1,
        0x89, 0xa3, 0x22, 0x7b, 0xe0, 0x38, 0x3d, 0x37, 0xb7, 0x0d, 0x06, 0xcc,
        0x66, 0xe5, 0xb6, 0xca, 0xa4, 0xc1, 0x45, 0x44, 0xba, 0x8c, 0x0f, 0x42,
        0xb2, 0x1b, 0x19, 0x69, 0x99, 0xd5, 0x63, 0x6e, 0xd9, 0x19, 0xc4, 0x26,
        0xde, 0xe0, 0x88, 0x14, 0x1f, 0x6d, 0xf9, 0x80, 0x15, 0x0a, 0x8b, 0xbf,
        0x3a, 0xd5, 0x6b, 0xc6, 0xbf, 0x8d, 0xe0, 0x59, 0x90, 0x16, 0x98, 0x43,
        0x84, 0xa9, 0x15, 0xb7,
    ];
    const HYBRID_STEREO_FB_20MS_OUT_L_0_HEAD_F32: [f32; 24] = [
        -0.0f32, 0.0f32, 0.0f32, -0.0f32, -0.0f32, -0.0f32,
        -0.0f32, -0.0f32, 0.0f32, 0.0f32, 0.0f32, -0.0f32,
        -0.0f32, 0.0f32, 0.0f32, -0.0f32, 0.0f32, 0.0f32,
        -0.0f32, -0.0f32, 0.0f32, 0.0f32, -0.0f32, -0.0f32,
    ];
    const HYBRID_STEREO_FB_20MS_OUT_L_0_TAIL_F32: [f32; 24] = [
        -0.19192564f32, -0.29739237f32, -0.3325336f32, -0.2905514f32, -0.2299775f32, -0.21130928f32,
        -0.24774738f32, -0.2974805f32, -0.2925076f32, -0.21412426f32, -0.10882207f32, -0.05629345f32,
        -0.10492206f32, -0.23318747f32, -0.36457285f32, -0.41860896f32, -0.38920268f32, -0.32784f32,
        -0.29096076f32, -0.29645887f32, -0.2973418f32, -0.23934473f32, -0.10429502f32, 0.07065529f32,
    ];
    const HYBRID_STEREO_FB_20MS_OUT_L_0_RMS: f32 = 0.22521979f32;
    const HYBRID_STEREO_FB_20MS_OUT_R_0_HEAD_F32: [f32; 24] = [
        -0.0f32, 0.0f32, 0.0f32, -0.0f32, -0.0f32, -0.0f32,
        0.0f32, 0.0f32, -0.0f32, -0.0f32, 0.0f32, 0.0f32,
        -0.0f32, -0.0f32, 0.0f32, -0.0f32, -0.0f32, 0.0f32,
        0.0f32, -0.0f32, -0.0f32, -0.0f32, 0.0f32, 0.0f32,
    ];
    const HYBRID_STEREO_FB_20MS_OUT_R_0_TAIL_F32: [f32; 24] = [
        0.19231707f32, 0.12370855f32, 0.08354951f32, 0.01917048f32, -0.12744522f32, -0.26948917f32,
        -0.28934735f32, -0.19545266f32, -0.07191946f32, -0.05587259f32, -0.16394287f32, -0.29434764f32,
        -0.3420601f32, -0.32271132f32, -0.33918026f32, -0.4001278f32, -0.4798397f32, -0.47622252f32,
        -0.33212644f32, -0.15468663f32, -0.04052492f32, -0.05332866f32, -0.12944879f32, -0.14862455f32,
    ];
    const HYBRID_STEREO_FB_20MS_OUT_R_0_RMS: f32 = 0.19312575f32;

    // TOC = 0x7c (config=15, stereo=1, code=0)
    const HYBRID_STEREO_FB_20MS_PKT_1: [u8; 160] = [
        0x7c, 0xa9, 0x57, 0xca, 0x63, 0x2c, 0x9e, 0x94, 0x75, 0x00, 0x49, 0x8e,
        0x66, 0xca, 0xfc, 0xe3, 0xfe, 0x6c, 0x56, 0x43, 0x5d, 0xc8, 0xa4, 0x80,
        0x2e, 0x19, 0xa6, 0x3f, 0x56, 0x25, 0x31, 0xa1, 0x29, 0x21, 0xdc, 0x62,
        0x44, 0x3c, 0xfa, 0x0f, 0xc5, 0x44, 0xae, 0xe4, 0x23, 0xb9, 0x97, 0xac,
        0x0f, 0x49, 0xef, 0x34, 0xb4, 0xbf, 0xee, 0xf8, 0xd6, 0xc3, 0x6a, 0xdf,
        0x26, 0x33, 0xd6, 0x26, 0xff, 0x62, 0xa9, 0xbc, 0xd8, 0x9b, 0xd8, 0x4d,
        0xd1, 0x0c, 0x70, 0x5b, 0x68, 0xd1, 0x64, 0xd5, 0x34, 0xf7, 0x6f, 0xe3,
        0x8c, 0x0c, 0x23, 0xa5, 0x4c, 0xc7, 0x10, 0xa3, 0xd3, 0x59, 0xf7, 0x06,
        0x1d, 0x2b, 0xfe, 0xdf, 0xcc, 0x1d, 0x16, 0x90, 0x2a, 0x0b, 0x22, 0x75,
        0xa8, 0x93, 0xca, 0x38, 0x98, 0xae, 0x64, 0x0e, 0xb1, 0xf9, 0x61, 0xd2,
        0x86, 0x91, 0xf7, 0xdf, 0x2f, 0xaf, 0xb7, 0x6f, 0x60, 0x8f, 0x66, 0x28,
        0x57, 0x35, 0xf1, 0x2b, 0x8b, 0xa8, 0x57, 0xc5, 0xf7, 0x2c, 0x59, 0x49,
        0xfd, 0x38, 0xb9, 0xcb, 0xb8, 0x43, 0x94, 0xec, 0xf6, 0xdb, 0xce, 0x6e,
        0xb6, 0x37, 0xc8, 0x23,
    ];
    const HYBRID_STEREO_FB_20MS_OUT_L_1_HEAD_F32: [f32; 24] = [
        0.20862862f32, 0.24928756f32, 0.18842605f32, 0.088008f32, 0.01556829f32, 0.00736561f32,
        0.05466709f32, 0.10675245f32, 0.13249715f32, 0.13659808f32, 0.16269408f32, 0.26072723f32,
        0.42199594f32, 0.5803316f32, 0.6523868f32, 0.6060645f32, 0.47627068f32, 0.34274274f32,
        0.27740029f32, 0.27784437f32, 0.29394677f32, 0.2695461f32, 0.19986978f32, 0.14842905f32,
    ];
    const HYBRID_STEREO_FB_20MS_OUT_L_1_TAIL_F32: [f32; 24] = [
        -0.04198641f32, -0.17266577f32, -0.20785786f32, -0.15343755f32, -0.07853718f32, -0.06014063f32,
        -0.12116846f32, -0.21450798f32, -0.2676225f32, -0.25101328f32, -0.20857804f32, -0.22215252f32,
        -0.3392298f32, -0.52650845f32, -0.690791f32, -0.7498766f32, -0.69442165f32, -0.58843553f32,
        -0.51264614f32, -0.4994769f32, -0.5141721f32, -0.49383682f32, -0.40818223f32, -0.2897823f32,
    ];
    const HYBRID_STEREO_FB_20MS_OUT_L_1_RMS: f32 = 0.3168763f32;
    const HYBRID_STEREO_FB_20MS_OUT_R_1_HEAD_F32: [f32; 24] = [
        -0.09690896f32, -0.06035934f32, -0.13096651f32, -0.2411659f32, -0.29706663f32, -0.23424397f32,
        -0.07589512f32, 0.01589216f32, -0.01269081f32, -0.13141148f32, -0.24141179f32, -0.25046536f32,
        -0.22777568f32, -0.2723004f32, -0.40310943f32, -0.49190915f32, -0.4421062f32, -0.27631772f32,
        -0.08268348f32, -0.00749391f32, -0.0286456f32, -0.05802923f32, -0.0425138f32, 0.03720953f32,
    ];
    const HYBRID_STEREO_FB_20MS_OUT_R_1_TAIL_F32: [f32; 24] = [
        0.20635647f32, 0.11997405f32, 0.09155568f32, 0.02699041f32, -0.10634811f32, -0.2294387f32,
        -0.23851614f32, -0.13035882f32, -0.0213807f32, -0.03365484f32, -0.17009994f32, -0.3167237f32,
        -0.3698659f32, -0.34486476f32, -0.34540817f32, -0.4293072f32, -0.52662426f32, -0.5072974f32,
        -0.32848495f32, -0.09675527f32, 0.02922162f32, -0.00622091f32, -0.10996751f32, -0.15134743f32,
    ];
    const HYBRID_STEREO_FB_20MS_OUT_R_1_RMS: f32 = 0.2820779f32;

    // ============================================================================
    // CELT_MONO_FB_2_5MS (channels=1, frame_size=120, bitrate=64000)
    // ============================================================================
    // TOC = 0xe0 (config=28, stereo=0, code=0)
    const CELT_MONO_FB_2_5MS_PKT_0: [u8; 20] = [
        0xe0, 0x78, 0x27, 0xa4, 0xe4, 0xf0, 0xef, 0x37, 0x03, 0x60, 0x4f, 0x29,
        0x11, 0x76, 0xed, 0xb6, 0x3f, 0x90, 0x18, 0x99,
    ];
    const CELT_MONO_FB_2_5MS_OUT_0_HEAD_F32: [f32; 24] = [
        -0.00000229f32, -0.00000368f32, -0.00003497f32, -0.00005896f32, -0.00002812f32, 0.00012796f32,
        0.00069119f32, 0.00123922f32, 0.00113993f32, 0.00059779f32, -0.00057975f32, -0.00184428f32,
        -0.00196021f32, -0.00128275f32, -0.00028798f32, -0.00053121f32, -0.0018521f32, -0.00126668f32,
        0.00092283f32, 0.00467423f32, 0.00969011f32, 0.01354215f32, 0.01309447f32, 0.0056241f32,
    ];
    const CELT_MONO_FB_2_5MS_OUT_0_TAIL_F32: [f32; 24] = [
        0.09331454f32, 0.0674241f32, 0.01373805f32, -0.03988598f32, -0.07261665f32, -0.08379989f32,
        -0.07502002f32, -0.04438764f32, -0.03324511f32, -0.04259399f32, -0.04409761f32, -0.02807047f32,
        0.01438024f32, 0.04910528f32, 0.05709554f32, 0.04406111f32, -0.00572895f32, -0.05620896f32,
        -0.06646124f32, -0.06053276f32, -0.04258096f32, -0.01725926f32, -0.01180053f32, 0.02394409f32,
    ];
    const CELT_MONO_FB_2_5MS_OUT_0_RMS: f32 = 0.03672114f32;

    // TOC = 0xe0 (config=28, stereo=0, code=0)
    const CELT_MONO_FB_2_5MS_PKT_1: [u8; 20] = [
        0xe0, 0xb1, 0x5e, 0xc4, 0x25, 0xf4, 0xb4, 0xd9, 0xd3, 0x32, 0xdb, 0x60,
        0xfb, 0x0f, 0x49, 0x8c, 0x67, 0x12, 0x83, 0xb2,
    ];
    const CELT_MONO_FB_2_5MS_OUT_1_HEAD_F32: [f32; 24] = [
        0.12492175f32, 0.23984265f32, 0.3143396f32, 0.29351342f32, 0.21150836f32, 0.16132446f32,
        0.1606474f32, 0.21488824f32, 0.30127075f32, 0.3583552f32, 0.36335933f32, 0.3650854f32,
        0.43395525f32, 0.55535567f32, 0.6745069f32, 0.70050573f32, 0.5879961f32, 0.4403501f32,
        0.31107292f32, 0.22391742f32, 0.211261f32, 0.21182394f32, 0.1723265f32, 0.08472858f32,
    ];
    const CELT_MONO_FB_2_5MS_OUT_1_TAIL_F32: [f32; 24] = [
        -0.12778567f32, -0.03788517f32, -0.05210233f32, -0.13879128f32, -0.22652715f32, -0.2618854f32,
        -0.26277864f32, -0.28853565f32, -0.37985766f32, -0.51545864f32, -0.60853314f32, -0.58126414f32,
        -0.4221781f32, -0.20164908f32, -0.02771689f32, 0.04742675f32, 0.05602633f32, 0.07167384f32,
        0.14122982f32, 0.23994812f32, 0.2940961f32, 0.24857293f32, 0.12538329f32, 0.02105263f32,
    ];
    const CELT_MONO_FB_2_5MS_OUT_1_RMS: f32 = 0.32582703f32;

    // TOC = 0xe0 (config=28, stereo=0, code=0)
    const CELT_MONO_FB_2_5MS_PKT_2: [u8; 20] = [
        0xe0, 0xad, 0x22, 0xf0, 0x59, 0xef, 0x6e, 0x75, 0x77, 0x09, 0x5c, 0xd3,
        0x3e, 0x89, 0x69, 0x23, 0x51, 0x07, 0xc9, 0xb2,
    ];
    const CELT_MONO_FB_2_5MS_OUT_2_HEAD_F32: [f32; 24] = [
        0.03252015f32, 0.17702952f32, 0.38083458f32, 0.5392513f32, 0.59982735f32, 0.59590405f32,
        0.60219806f32, 0.660316f32, 0.73533833f32, 0.7450516f32, 0.63379836f32, 0.4283075f32,
        0.2275368f32, 0.12697646f32, 0.15048303f32, 0.23925826f32, 0.30571195f32, 0.30667225f32,
        0.27007598f32, 0.26154655f32, 0.31538185f32, 0.39578372f32, 0.4195177f32, 0.32905278f32,
    ];
    const CELT_MONO_FB_2_5MS_OUT_2_TAIL_F32: [f32; 24] = [
        -0.1991958f32, -0.22926413f32, -0.3167447f32, -0.42301306f32, -0.4617561f32, -0.37384635f32,
        -0.18273513f32, 0.0192403f32, 0.1418926f32, 0.16405533f32, 0.13952565f32, 0.13980608f32,
        0.18691517f32, 0.2360202f32, 0.22375235f32, 0.13643005f32, 0.03546786f32, 0.01380497f32,
        0.1188383f32, 0.31304714f32, 0.5039217f32, 0.61613464f32, 0.64639026f32, 0.6519639f32,
    ];
    const CELT_MONO_FB_2_5MS_OUT_2_RMS: f32 = 0.34622616f32;

    // ============================================================================
    // CELT_MONO_FB_5MS (channels=1, frame_size=240, bitrate=64000)
    // ============================================================================
    // TOC = 0xe8 (config=29, stereo=0, code=0)
    const CELT_MONO_FB_5MS_PKT_0: [u8; 40] = [
        0xe8, 0xb5, 0x44, 0x77, 0xa9, 0x56, 0x68, 0xca, 0xc6, 0x4b, 0xd5, 0xb2,
        0xb1, 0x43, 0xf5, 0xef, 0x93, 0xab, 0x83, 0xd3, 0xb3, 0xde, 0xc1, 0xc9,
        0xaa, 0x73, 0xf0, 0xc2, 0x92, 0x93, 0x98, 0x38, 0x7a, 0x1e, 0x2e, 0x21,
        0x41, 0x76, 0x31, 0x32,
    ];
    const CELT_MONO_FB_5MS_OUT_0_HEAD_F32: [f32; 24] = [
        -0.00000161f32, 0.00000269f32, -0.00001741f32, -0.00001728f32, 0.00007923f32, 0.00028602f32,
        0.00078945f32, 0.00112515f32, 0.00086051f32, 0.00041446f32, -0.00054861f32, -0.00188186f32,
        -0.00253042f32, -0.0020257f32, -0.00041946f32, 0.00013509f32, -0.00127231f32, -0.00232149f32,
        -0.00175885f32, 0.00121625f32, 0.00584956f32, 0.00911665f32, 0.00781502f32, 0.00151203f32,
    ];
    const CELT_MONO_FB_5MS_OUT_0_TAIL_F32: [f32; 24] = [
        -0.12487194f32, -0.17211495f32, -0.23414679f32, -0.2476469f32, -0.19901124f32, -0.14377797f32,
        -0.14183511f32, -0.22893237f32, -0.36780947f32, -0.46212238f32, -0.45982295f32, -0.36250544f32,
        -0.22840455f32, -0.13238591f32, -0.09771314f32, -0.0873636f32, -0.02861967f32, 0.09370268f32,
        0.23106013f32, 0.31303155f32, 0.27850243f32, 0.1615653f32, 0.05597094f32, 0.03714997f32,
    ];
    const CELT_MONO_FB_5MS_OUT_0_RMS: f32 = 0.19765383f32;

    // TOC = 0xe8 (config=29, stereo=0, code=0)
    const CELT_MONO_FB_5MS_PKT_1: [u8; 40] = [
        0xe8, 0xb3, 0x39, 0xe3, 0xcd, 0x40, 0x45, 0x04, 0x87, 0x1e, 0x70, 0x09,
        0x97, 0x61, 0xaf, 0xbb, 0x2a, 0xfc, 0xc1, 0xb3, 0x2e, 0x41, 0x00, 0x6c,
        0xfc, 0xc2, 0xfe, 0xbd, 0xf0, 0x5b, 0x85, 0x4d, 0xc6, 0x06, 0x70, 0x54,
        0x6c, 0xe7, 0x29, 0xb2,
    ];
    const CELT_MONO_FB_5MS_OUT_1_HEAD_F32: [f32; 24] = [
        0.12164f32, 0.25114903f32, 0.35506028f32, 0.39313233f32, 0.39843673f32, 0.43991217f32,
        0.53257173f32, 0.6453203f32, 0.70295155f32, 0.64386773f32, 0.48782903f32, 0.311077f32,
        0.19966654f32, 0.17992732f32, 0.21814023f32, 0.2506752f32, 0.22817838f32, 0.18030696f32,
        0.1634766f32, 0.20984806f32, 0.3048208f32, 0.37504405f32, 0.35381067f32, 0.22954784f32,
    ];
    const CELT_MONO_FB_5MS_OUT_1_TAIL_F32: [f32; 24] = [
        0.07723603f32, 0.06457945f32, 0.06658913f32, 0.12985411f32, 0.23632284f32, 0.31570297f32,
        0.30471098f32, 0.2037147f32, 0.08390714f32, 0.03642058f32, 0.10398851f32, 0.25103337f32,
        0.39524084f32, 0.4741968f32, 0.49140868f32, 0.5041828f32, 0.5655376f32, 0.671433f32,
        0.7580874f32, 0.75136447f32, 0.6305817f32, 0.4516629f32, 0.30784655f32, 0.26194593f32,
    ];
    const CELT_MONO_FB_5MS_OUT_1_RMS: f32 = 0.31464815f32;

    // TOC = 0xe8 (config=29, stereo=0, code=0)
    const CELT_MONO_FB_5MS_PKT_2: [u8; 40] = [
        0xe8, 0xb1, 0x4f, 0x61, 0xdc, 0xd8, 0xd5, 0xdb, 0x27, 0xd1, 0xe7, 0xc8,
        0x16, 0x12, 0xd9, 0x68, 0x10, 0x09, 0x14, 0xc1, 0x53, 0xe5, 0x7b, 0x9c,
        0x9a, 0xfa, 0xb6, 0xce, 0x77, 0x20, 0xcf, 0x0d, 0xda, 0x8d, 0xb1, 0x99,
        0x71, 0xc6, 0x5d, 0xb2,
    ];
    const CELT_MONO_FB_5MS_OUT_2_HEAD_F32: [f32; 24] = [
        0.30267733f32, 0.36083138f32, 0.3704663f32, 0.32171643f32, 0.2640769f32, 0.25661945f32,
        0.31031f32, 0.37294155f32, 0.3663943f32, 0.2497043f32, 0.05625197f32, -0.12799187f32,
        -0.2273028f32, -0.2294611f32, -0.18629384f32, -0.16470884f32, -0.18887553f32, -0.22209531f32,
        -0.20398434f32, -0.10734922f32, 0.03055152f32, 0.12926115f32, 0.12571315f32, 0.02359555f32,
    ];
    const CELT_MONO_FB_5MS_OUT_2_TAIL_F32: [f32; 24] = [
        0.4419526f32, 0.4414518f32, 0.44367975f32, 0.49469635f32, 0.5801195f32, 0.6342916f32,
        0.5960707f32, 0.46457055f32, 0.30747622f32, 0.21331626f32, 0.22558887f32, 0.31205207f32,
        0.3936668f32, 0.40874034f32, 0.3591726f32, 0.30123916f32, 0.29013506f32, 0.3263238f32,
        0.35009062f32, 0.28995687f32, 0.12498064f32, -0.09151661f32, -0.26776546f32, -0.3404647f32,
    ];
    const CELT_MONO_FB_5MS_OUT_2_RMS: f32 = 0.31024322f32;

    // ============================================================================
    // CELT_MONO_FB_10MS (channels=1, frame_size=480, bitrate=64000)
    // ============================================================================
    // TOC = 0xf0 (config=30, stereo=0, code=0)
    const CELT_MONO_FB_10MS_PKT_0: [u8; 80] = [
        0xf0, 0xb5, 0x45, 0xb1, 0x95, 0x2b, 0x10, 0x8c, 0x69, 0x92, 0xad, 0x52,
        0xb5, 0x8e, 0xdd, 0x40, 0x52, 0x1e, 0x0f, 0x71, 0xeb, 0x50, 0x83, 0xe9,
        0xf3, 0x38, 0x17, 0xf3, 0x12, 0x4e, 0x44, 0x16, 0xc8, 0xdd, 0xca, 0xc0,
        0x6b, 0x61, 0x78, 0x4f, 0x48, 0x73, 0xf6, 0x4a, 0x72, 0x51, 0x6d, 0xea,
        0x3c, 0x75, 0x78, 0x0f, 0xb0, 0xb5, 0x7b, 0x61, 0xed, 0xd3, 0x75, 0x30,
        0x2e, 0xdc, 0x06, 0x05, 0x6a, 0x04, 0xac, 0x5d, 0xbc, 0x1b, 0x04, 0x91,
        0xd8, 0x70, 0x77, 0x64, 0xbc, 0x65, 0x91, 0x72,
    ];
    const CELT_MONO_FB_10MS_OUT_0_HEAD_F32: [f32; 24] = [
        -0.00000119f32, -0.00000252f32, -0.00001494f32, -0.00003677f32, -0.00006779f32, 0.00007314f32,
        0.00023113f32, 0.00050613f32, 0.00072684f32, 0.00020438f32, -0.00078801f32, -0.00182811f32,
        -0.00214994f32, -0.00153912f32, -0.0004908f32, 0.00010355f32, -0.00046587f32, -0.00135161f32,
        -0.00167875f32, 0.00052254f32, 0.00445018f32, 0.00602763f32, 0.0043001f32, -0.0003426f32,
    ];
    const CELT_MONO_FB_10MS_OUT_0_TAIL_F32: [f32; 24] = [
        0.04519421f32, 0.0395912f32, 0.05032735f32, 0.11565484f32, 0.2159597f32, 0.28793272f32,
        0.2793137f32, 0.19470058f32, 0.09645859f32, 0.06164899f32, 0.12517022f32, 0.25570026f32,
        0.38499254f32, 0.46141264f32, 0.48526117f32, 0.5014757f32, 0.55361605f32, 0.63944006f32,
        0.70903f32, 0.7037488f32, 0.60472697f32, 0.45424178f32, 0.3282656f32, 0.28110677f32,
    ];
    const CELT_MONO_FB_10MS_OUT_0_RMS: f32 = 0.269838f32;

    // TOC = 0xf0 (config=30, stereo=0, code=0)
    const CELT_MONO_FB_10MS_PKT_1: [u8; 80] = [
        0xf0, 0xb3, 0x10, 0x9d, 0x05, 0xc0, 0xed, 0x63, 0x26, 0xf8, 0x47, 0x23,
        0xea, 0xe3, 0x14, 0xe2, 0xb0, 0x29, 0x35, 0x24, 0xb4, 0x4f, 0xc3, 0x7e,
        0xa2, 0x06, 0x37, 0x32, 0x26, 0x03, 0xbc, 0xa0, 0x0d, 0x92, 0x72, 0xdf,
        0xe5, 0x36, 0xd9, 0xcc, 0x41, 0x62, 0x73, 0x34, 0xad, 0xa6, 0xdd, 0x84,
        0xcd, 0xba, 0x6b, 0xe1, 0x08, 0x5e, 0xa7, 0x94, 0xe8, 0x6d, 0x1a, 0x09,
        0xa3, 0x8f, 0xde, 0xd2, 0xa3, 0x0e, 0x0d, 0xfa, 0x6f, 0x99, 0xb4, 0xea,
        0x2c, 0x46, 0x5b, 0x5c, 0x07, 0x46, 0x9f, 0x72,
    ];
    const CELT_MONO_FB_10MS_OUT_1_HEAD_F32: [f32; 24] = [
        0.30942073f32, 0.3606769f32, 0.3771509f32, 0.3428762f32, 0.2928502f32, 0.27657285f32,
        0.31114018f32, 0.3609128f32, 0.3606218f32, 0.26834148f32, 0.10327692f32, -0.06502688f32,
        -0.16528854f32, -0.17426491f32, -0.13062133f32, -0.09846438f32, -0.11159032f32, -0.14769483f32,
        -0.15035942f32, -0.08129041f32, 0.03814899f32, 0.1362638f32, 0.14571373f32, 0.05652456f32,
    ];
    const CELT_MONO_FB_10MS_OUT_1_TAIL_F32: [f32; 24] = [
        0.34546992f32, 0.28322783f32, 0.225349f32, 0.22042927f32, 0.25210062f32, 0.25139165f32,
        0.15637141f32, -0.03060961f32, -0.2364974f32, -0.3689374f32, -0.38467085f32, -0.31974027f32,
        -0.25750256f32, -0.2611685f32, -0.32625675f32, -0.39126074f32, -0.39617592f32, -0.33881766f32,
        -0.28099078f32, -0.2985504f32, -0.41635036f32, -0.582376f32, -0.7034521f32, -0.7142301f32,
    ];
    const CELT_MONO_FB_10MS_OUT_1_RMS: f32 = 0.30644906f32;

    // TOC = 0xf0 (config=30, stereo=0, code=0)
    const CELT_MONO_FB_10MS_PKT_2: [u8; 80] = [
        0xf0, 0xac, 0x2e, 0x0e, 0xe0, 0x99, 0xc9, 0xc3, 0x95, 0x5c, 0x0d, 0x42,
        0xb9, 0xc1, 0xd8, 0x8f, 0xbc, 0xd7, 0xf6, 0x18, 0x57, 0x86, 0x6d, 0xa3,
        0x0d, 0xc1, 0xe7, 0x93, 0x96, 0x24, 0x1b, 0x7b, 0xaf, 0xd1, 0x75, 0xf0,
        0xfe, 0xf3, 0x7d, 0x9f, 0xce, 0xc9, 0x4a, 0xbc, 0x10, 0x22, 0xbf, 0x19,
        0xfb, 0x0d, 0xe7, 0xd1, 0x6c, 0x3a, 0x37, 0x32, 0xb3, 0xe3, 0x40, 0xc5,
        0x44, 0x10, 0xca, 0xcf, 0xea, 0x71, 0x7f, 0xb7, 0xd2, 0x45, 0xf4, 0xf3,
        0x22, 0x01, 0xfd, 0xfc, 0x5c, 0xf0, 0x53, 0x72,
    ];
    const CELT_MONO_FB_10MS_OUT_2_HEAD_F32: [f32; 24] = [
        -0.6239997f32, -0.50410104f32, -0.42759308f32, -0.4116402f32, -0.40953174f32, -0.35759553f32,
        -0.23645145f32, -0.09451022f32, -0.0134162f32, -0.04310495f32, -0.15965892f32, -0.2809236f32,
        -0.3276518f32, -0.28105122f32, -0.19103992f32, -0.13016291f32, -0.13223025f32, -0.16646057f32,
        -0.16784051f32, -0.09740753f32, 0.01845869f32, 0.10330503f32, 0.08906467f32, -0.02955481f32,
    ];
    const CELT_MONO_FB_10MS_OUT_2_TAIL_F32: [f32; 24] = [
        -0.35665736f32, -0.3733644f32, -0.3763169f32, -0.32039502f32, -0.22161998f32, -0.14546962f32,
        -0.15075794f32, -0.23596194f32, -0.33346817f32, -0.35798556f32, -0.27064785f32, -0.1071477f32,
        0.05242575f32, 0.14678997f32, 0.17785253f32, 0.20068017f32, 0.26897532f32, 0.3829047f32,
        0.4838547f32, 0.50126636f32, 0.41295704f32, 0.26855743f32, 0.1557532f32, 0.13599677f32,
    ];
    const CELT_MONO_FB_10MS_OUT_2_RMS: f32 = 0.30965945f32;

    // ============================================================================
    // CELT_MONO_FB_20MS (channels=1, frame_size=960, bitrate=64000)
    // ============================================================================
    // TOC = 0xf8 (config=31, stereo=0, code=0)
    const CELT_MONO_FB_20MS_PKT_0: [u8; 160] = [
        0xf8, 0xb5, 0x46, 0x42, 0x0d, 0x87, 0x88, 0x17, 0x51, 0xa6, 0x89, 0x6f,
        0x47, 0xd7, 0x60, 0xa0, 0x9e, 0x53, 0x8c, 0x6f, 0x49, 0x20, 0x31, 0x43,
        0x2d, 0xf9, 0x33, 0x0e, 0x05, 0xd0, 0xbf, 0x69, 0x97, 0x25, 0x81, 0xd6,
        0xd0, 0xd2, 0x5c, 0x54, 0xf2, 0xa1, 0xb9, 0xfc, 0x4f, 0xae, 0x4e, 0xba,
        0x6d, 0xf4, 0x7c, 0xc4, 0xe5, 0x16, 0x05, 0x8e, 0x78, 0xd1, 0x09, 0xe3,
        0xc1, 0xde, 0x37, 0x2e, 0x48, 0xf0, 0x82, 0x42, 0x1d, 0x5d, 0xb3, 0xb4,
        0xb6, 0xd4, 0xda, 0x5a, 0xca, 0x6e, 0x61, 0xb9, 0x86, 0xb2, 0x9c, 0x97,
        0xe8, 0xd8, 0xf2, 0x65, 0xae, 0x35, 0x21, 0x25, 0xde, 0x91, 0xf2, 0xc0,
        0x12, 0x0f, 0xa6, 0x9c, 0x72, 0x69, 0xc7, 0x26, 0x9c, 0x70, 0x95, 0x0d,
        0x19, 0xa4, 0x20, 0x4c, 0x4f, 0x1c, 0x73, 0x6a, 0xb5, 0x5a, 0xaa, 0x19,
        0x45, 0xcf, 0xb2, 0xa4, 0x7d, 0xb2, 0x7e, 0xdd, 0x90, 0xbe, 0xb7, 0x8f,
        0xbe, 0xf3, 0x1e, 0xc6, 0x4d, 0x86, 0x5c, 0x50, 0x7d, 0x82, 0xe7, 0x2a,
        0x4c, 0x4a, 0x68, 0x69, 0x3c, 0x87, 0x29, 0xbc, 0x1b, 0x99, 0x7d, 0x7f,
        0xa7, 0x0e, 0x83, 0x72,
    ];
    const CELT_MONO_FB_20MS_OUT_0_HEAD_F32: [f32; 24] = [
        -0.00000226f32, -0.00000921f32, -0.00001254f32, 0.00006469f32, 0.00019715f32, 0.00038582f32,
        0.00027258f32, 0.00000427f32, -0.00007973f32, -0.00013501f32, 0.00022054f32, 0.0002787f32,
        -0.00054247f32, -0.0016487f32, -0.0023572f32, -0.00196152f32, -0.00032721f32, 0.00180756f32,
        0.00236897f32, 0.00160213f32, 0.00120299f32, 0.00103419f32, 0.00182933f32, 0.00307424f32,
    ];
    const CELT_MONO_FB_20MS_OUT_0_TAIL_F32: [f32; 24] = [
        0.3025617f32, 0.24835904f32, 0.2066853f32, 0.21602903f32, 0.2521691f32, 0.24644113f32,
        0.14499193f32, -0.04184248f32, -0.24042583f32, -0.36459577f32, -0.3783056f32, -0.31946114f32,
        -0.26550442f32, -0.27131125f32, -0.32926822f32, -0.38184714f32, -0.37656835f32, -0.3165718f32,
        -0.26233625f32, -0.28383914f32, -0.4019712f32, -0.56546426f32, -0.6850603f32, -0.6987218f32,
    ];
    const CELT_MONO_FB_20MS_OUT_0_RMS: f32 = 0.291224f32;

    // TOC = 0xf8 (config=31, stereo=0, code=0)
    const CELT_MONO_FB_20MS_PKT_1: [u8; 160] = [
        0xf8, 0xb3, 0x11, 0x52, 0xfd, 0xc7, 0x5e, 0xea, 0x4b, 0xfc, 0xd7, 0xb3,
        0x65, 0xbb, 0x39, 0x04, 0x6f, 0xab, 0x97, 0xca, 0xed, 0xb4, 0x3f, 0x2a,
        0x49, 0x46, 0x92, 0x49, 0x62, 0x93, 0xc4, 0x51, 0xe3, 0xc7, 0xe6, 0x25,
        0x74, 0x91, 0xf7, 0x6f, 0x3f, 0xc5, 0x06, 0xb5, 0x60, 0xe8, 0xad, 0x01,
        0x3f, 0x00, 0xc3, 0x4c, 0xaa, 0xc0, 0x04, 0x75, 0xf2, 0xfa, 0x09, 0x0a,
        0xbf, 0xaf, 0xd0, 0xfe, 0x95, 0xd7, 0xed, 0xb4, 0x39, 0xe6, 0xa6, 0xa6,
        0x9b, 0x7a, 0x18, 0x80, 0x57, 0xe5, 0x06, 0x08, 0xaa, 0xb5, 0x59, 0xf9,
        0x24, 0x2d, 0xe0, 0x30, 0xca, 0xff, 0x13, 0xd2, 0x54, 0xcb, 0x50, 0x2f,
        0x4f, 0x68, 0x67, 0xf3, 0x7d, 0x95, 0xd8, 0xd2, 0x95, 0xa8, 0x53, 0x29,
        0xaa, 0x6b, 0x8a, 0x26, 0x00, 0x87, 0xf5, 0xc4, 0xaf, 0x9d, 0x3e, 0x49,
        0xb5, 0xa2, 0x03, 0xa2, 0xf7, 0xec, 0xcc, 0x9f, 0x5e, 0x00, 0xd8, 0xf6,
        0xff, 0xf9, 0x10, 0x82, 0x55, 0x70, 0x59, 0xc9, 0xc3, 0x2a, 0x13, 0xcb,
        0xa8, 0x56, 0x0c, 0x83, 0x87, 0x16, 0xbc, 0xb2, 0x2f, 0xb3, 0xba, 0xb8,
        0x58, 0x26, 0xc5, 0x72,
    ];
    const CELT_MONO_FB_20MS_OUT_1_HEAD_F32: [f32; 24] = [
        -0.61434525f32, -0.49823648f32, -0.42014724f32, -0.39845124f32, -0.3909305f32, -0.33801934f32,
        -0.21990336f32, -0.08047966f32, 0.00188606f32, -0.02388f32, -0.13813049f32, -0.26242962f32,
        -0.3162675f32, -0.2747521f32, -0.18299949f32, -0.11454608f32, -0.10908427f32, -0.14216621f32,
        -0.14960906f32, -0.08637252f32, 0.02806461f32, 0.11867469f32, 0.11236081f32, -0.00375728f32,
    ];
    const CELT_MONO_FB_20MS_OUT_1_TAIL_F32: [f32; 24] = [
        0.65945303f32, 0.6067839f32, 0.5496333f32, 0.5477766f32, 0.5933149f32, 0.61635053f32,
        0.54567146f32, 0.37362564f32, 0.1703344f32, 0.03405715f32, 0.01740238f32, 0.08926843f32,
        0.16306397f32, 0.16729832f32, 0.10022262f32, 0.02425062f32, 0.00647006f32, 0.05640939f32,
        0.11407106f32, 0.09881051f32, -0.0215243f32, -0.19905974f32, -0.33886513f32, -0.36999997f32,
    ];
    const CELT_MONO_FB_20MS_OUT_1_RMS: f32 = 0.311236f32;

    // TOC = 0xf8 (config=31, stereo=0, code=0)
    const CELT_MONO_FB_20MS_PKT_2: [u8; 160] = [
        0xf8, 0xab, 0xb9, 0xc7, 0xce, 0xea, 0xb5, 0x39, 0x24, 0x41, 0xac, 0x99,
        0x31, 0xb5, 0x4b, 0x64, 0x9e, 0xfd, 0x5a, 0x39, 0xc3, 0xb1, 0xbc, 0x74,
        0x30, 0x16, 0x34, 0xa9, 0x52, 0xcf, 0x91, 0xac, 0xb8, 0x71, 0x77, 0xdc,
        0xce, 0x9c, 0x02, 0xd1, 0xd5, 0x71, 0xfc, 0x65, 0x92, 0x89, 0xb0, 0x92,
        0xc7, 0x53, 0xe8, 0x65, 0x48, 0x18, 0xeb, 0x9c, 0x27, 0xae, 0x18, 0x4b,
        0x92, 0x14, 0x9a, 0xe1, 0x9c, 0xe9, 0x72, 0x01, 0x6e, 0x9d, 0x5d, 0xa8,
        0x34, 0x0f, 0xa8, 0x92, 0x8d, 0xbd, 0xaa, 0x3a, 0xbd, 0xb1, 0x3e, 0x2d,
        0xaa, 0x15, 0xae, 0x1a, 0xb9, 0x53, 0x0a, 0x7f, 0x5f, 0xf9, 0x51, 0xdb,
        0xb7, 0xcf, 0xec, 0xae, 0xc6, 0x94, 0xcd, 0x45, 0xaf, 0x64, 0x03, 0xb2,
        0x29, 0x97, 0xd9, 0x5e, 0x23, 0x04, 0xef, 0x63, 0x21, 0xde, 0xe3, 0x54,
        0x8b, 0x27, 0x3e, 0x5e, 0x15, 0x5e, 0xef, 0xba, 0x33, 0x38, 0xc3, 0x74,
        0x0e, 0xd8, 0x55, 0xfb, 0xc1, 0xad, 0xc4, 0xca, 0x4a, 0x97, 0x21, 0x4c,
        0x69, 0x3b, 0xab, 0xcb, 0x3e, 0x1f, 0x36, 0xe7, 0xf1, 0xa8, 0x3e, 0x15,
        0xa6, 0xe8, 0x53, 0x72,
    ];
    const CELT_MONO_FB_20MS_OUT_2_HEAD_F32: [f32; 24] = [
        -0.29699114f32, -0.19061893f32, -0.12724909f32, -0.12791267f32, -0.14702575f32, -0.1184677f32,
        -0.01893113f32, 0.1053995f32, 0.1717646f32, 0.1266301f32, -0.0096295f32, -0.15592663f32,
        -0.23101579f32, -0.21259579f32, -0.14802691f32, -0.10973577f32, -0.13337442f32, -0.1900295f32,
        -0.21478727f32, -0.16696922f32, -0.07111855f32, -0.00364952f32, -0.03486697f32, -0.17306778f32,
    ];
    const CELT_MONO_FB_20MS_OUT_2_TAIL_F32: [f32; 24] = [
        0.47820038f32, 0.45796683f32, 0.4320901f32, 0.45221055f32, 0.5100748f32, 0.54352534f32,
        0.4908172f32, 0.34775302f32, 0.17920576f32, 0.07400045f32, 0.07899256f32, 0.16563001f32,
        0.25575837f32, 0.28490528f32, 0.25090784f32, 0.20859703f32, 0.21651834f32, 0.28160194f32,
        0.34910738f32, 0.347003f32, 0.24805441f32, 0.09756649f32, -0.01716843f32, -0.0315209f32,
    ];
    const CELT_MONO_FB_20MS_OUT_2_RMS: f32 = 0.30851743f32;

    // ============================================================================
    // CELT_STEREO_FB_10MS (channels=2, frame_size=480, bitrate=96000)
    // ============================================================================
    // TOC = 0xf4 (config=30, stereo=1, code=0)
    const CELT_STEREO_FB_10MS_PKT_0: [u8; 120] = [
        0xf4, 0x7f, 0x51, 0x10, 0x9f, 0x8b, 0x94, 0x20, 0x29, 0x70, 0xac, 0x2e,
        0x27, 0xe8, 0xaa, 0x9e, 0x0f, 0xef, 0x47, 0x90, 0x01, 0x3c, 0x89, 0x05,
        0xa4, 0x5c, 0x3d, 0x97, 0x6d, 0x1c, 0x31, 0x63, 0x07, 0x91, 0x69, 0xcf,
        0x43, 0xfd, 0xc5, 0x93, 0xfb, 0xde, 0x1d, 0x8c, 0xda, 0x68, 0x48, 0x2d,
        0x33, 0x30, 0x1c, 0xe3, 0x5a, 0x86, 0x97, 0x1d, 0x91, 0xa3, 0xd3, 0x88,
        0x77, 0x97, 0xbc, 0x3b, 0x7e, 0x6e, 0x2e, 0xfd, 0xa8, 0x88, 0xcd, 0x58,
        0x21, 0x3d, 0x08, 0x51, 0x94, 0x74, 0x80, 0x7a, 0x25, 0xc1, 0x0b, 0xbc,
        0x8c, 0xa2, 0x46, 0x99, 0x9c, 0x55, 0x56, 0x16, 0xa5, 0xf9, 0x26, 0x20,
        0x83, 0x10, 0x8d, 0x0a, 0x17, 0x29, 0x63, 0x93, 0x80, 0x9d, 0x78, 0xc6,
        0xb2, 0x5d, 0xd1, 0x4d, 0xcd, 0xf5, 0x6a, 0xd6, 0x9c, 0x8d, 0xfb, 0x40,
    ];
    const CELT_STEREO_FB_10MS_OUT_L_0_HEAD_F32: [f32; 24] = [
        -0.00000038f32, 0.00001125f32, -0.00002438f32, -0.00017609f32, -0.00033255f32, -0.00038534f32,
        -0.00024069f32, 0.00006129f32, -0.00021975f32, -0.00120103f32, -0.00252057f32, -0.00353542f32,
        -0.0026049f32, -0.00037189f32, 0.00199059f32, 0.00285683f32, -0.00008524f32, -0.00361613f32,
        -0.00460138f32, -0.0032398f32, 0.00022116f32, 0.00318693f32, 0.00243463f32, -0.0028763f32,
    ];
    const CELT_STEREO_FB_10MS_OUT_L_0_TAIL_F32: [f32; 24] = [
        0.06637539f32, 0.04361007f32, 0.03402629f32, 0.08546302f32, 0.18505499f32, 0.26834467f32,
        0.27203777f32, 0.18985331f32, 0.08861553f32, 0.05144162f32, 0.12046637f32, 0.26709592f32,
        0.40829816f32, 0.48136133f32, 0.48663434f32, 0.48543608f32, 0.53803027f32, 0.63849944f32,
        0.7340118f32, 0.7477648f32, 0.6438913f32, 0.4835624f32, 0.35736403f32, 0.32434252f32,
    ];
    const CELT_STEREO_FB_10MS_OUT_L_0_RMS: f32 = 0.2714454f32;
    const CELT_STEREO_FB_10MS_OUT_R_0_HEAD_F32: [f32; 24] = [
        -0.00000832f32, 0.000018f32, 0.00009745f32, 0.00000357f32, -0.00043286f32, -0.00118637f32,
        -0.0011617f32, 0.00014f32, -0.00044446f32, -0.00138571f32, -0.00084789f32, -0.00103106f32,
        0.00061669f32, 0.00029443f32, 0.00025718f32, 0.00230053f32, -0.00394521f32, -0.00735139f32,
        -0.00167824f32, 0.00216942f32, 0.00231428f32, 0.0002185f32, 0.00209606f32, 0.00775586f32,
    ];
    const CELT_STEREO_FB_10MS_OUT_R_0_TAIL_F32: [f32; 24] = [
        -0.1889996f32, -0.08812124f32, 0.0773031f32, 0.18300387f32, 0.19745368f32, 0.20491551f32,
        0.28908038f32, 0.4187918f32, 0.49584287f32, 0.42491525f32, 0.25942773f32, 0.16391867f32,
        0.21015169f32, 0.35259068f32, 0.4501994f32, 0.4479273f32, 0.42076898f32, 0.42848718f32,
        0.5298563f32, 0.6154319f32, 0.51900464f32, 0.3073179f32, 0.10567217f32, 0.02751026f32,
    ];
    const CELT_STEREO_FB_10MS_OUT_R_0_RMS: f32 = 0.24964389f32;

    // TOC = 0xf4 (config=30, stereo=1, code=0)
    const CELT_STEREO_FB_10MS_PKT_1: [u8; 120] = [
        0xf4, 0xdd, 0x89, 0x6b, 0x66, 0xe0, 0x3b, 0x05, 0x3d, 0x4d, 0xd8, 0x80,
        0xd9, 0x03, 0x9a, 0x08, 0xbb, 0x56, 0xfc, 0x8d, 0x64, 0x53, 0x77, 0xb0,
        0xf0, 0x05, 0x57, 0xc5, 0x31, 0x92, 0xa6, 0x1f, 0x06, 0x8d, 0x7c, 0xfc,
        0x1b, 0xe5, 0xc9, 0xba, 0xa9, 0xb4, 0x54, 0x6d, 0x86, 0xd1, 0x77, 0x0d,
        0xa4, 0xfb, 0x94, 0x8f, 0x97, 0x63, 0x52, 0xbc, 0x82, 0x29, 0x30, 0xc0,
        0x97, 0xf9, 0xd0, 0x24, 0xbb, 0x58, 0x50, 0x23, 0x26, 0x1f, 0xcd, 0xb0,
        0xf2, 0x16, 0xc2, 0x36, 0x43, 0x5d, 0x58, 0x33, 0xec, 0x14, 0x72, 0x97,
        0xe3, 0xed, 0xd9, 0xc7, 0x6f, 0x61, 0x67, 0xd2, 0xd8, 0x50, 0xce, 0x5b,
        0x86, 0xd0, 0x29, 0x55, 0xc4, 0x43, 0xd1, 0x3a, 0x01, 0x42, 0xe0, 0x1f,
        0x34, 0xa6, 0x25, 0x6c, 0x3d, 0x4f, 0x22, 0x29, 0x8c, 0x1d, 0xf4, 0x54,
    ];
    const CELT_STEREO_FB_10MS_OUT_L_1_HEAD_F32: [f32; 24] = [
        0.37418947f32, 0.43306756f32, 0.4369759f32, 0.37012407f32, 0.28926417f32, 0.26959366f32,
        0.31425887f32, 0.3716383f32, 0.36519098f32, 0.2472817f32, 0.05833995f32, -0.11100416f32,
        -0.18495902f32, -0.1606633f32, -0.10462258f32, -0.08603533f32, -0.1281252f32, -0.18301125f32,
        -0.17593357f32, -0.08001044f32, 0.05893657f32, 0.15046424f32, 0.13168223f32, 0.01693336f32,
    ];
    const CELT_STEREO_FB_10MS_OUT_L_1_TAIL_F32: [f32; 24] = [
        0.30250975f32, 0.26724613f32, 0.24104927f32, 0.25348827f32, 0.27871633f32, 0.25558627f32,
        0.14147699f32, -0.04587862f32, -0.23450741f32, -0.34841466f32, -0.36158052f32, -0.3124601f32,
        -0.26854634f32, -0.27146053f32, -0.30805886f32, -0.33017892f32, -0.3044054f32, -0.24819951f32,
        -0.22063486f32, -0.27458674f32, -0.41003698f32, -0.56780624f32, -0.6699843f32, -0.674999f32,
    ];
    const CELT_STEREO_FB_10MS_OUT_L_1_RMS: f32 = 0.3037692f32;
    const CELT_STEREO_FB_10MS_OUT_R_1_HEAD_F32: [f32; 24] = [
        0.08184452f32, 0.13253556f32, 0.11594574f32, 0.02938154f32, -0.00964076f32, 0.09660951f32,
        0.19860445f32, 0.17754577f32, 0.02278912f32, -0.19130501f32, -0.2955011f32, -0.25437564f32,
        -0.16102922f32, -0.13185376f32, -0.18439609f32, -0.18768027f32, -0.07643939f32, 0.09097307f32,
        0.18169178f32, 0.09996673f32, -0.08286694f32, -0.21837825f32, -0.22141221f32, -0.15602149f32,
    ];
    const CELT_STEREO_FB_10MS_OUT_R_1_TAIL_F32: [f32; 24] = [
        -0.5736482f32, -0.55249304f32, -0.42945212f32, -0.31625953f32, -0.284957f32, -0.30179295f32,
        -0.28253272f32, -0.20208138f32, -0.13022418f32, -0.15136181f32, -0.2604051f32, -0.35576314f32,
        -0.34106845f32, -0.224132f32, -0.10432389f32, -0.0645037f32, -0.08897024f32, -0.0966786f32,
        -0.0473858f32, 0.00313387f32, -0.03670467f32, -0.18601573f32, -0.3580258f32, -0.44150287f32,
    ];
    const CELT_STEREO_FB_10MS_OUT_R_1_RMS: f32 = 0.27630734f32;

    // ============================================================================
    // CELT_STEREO_FB_20MS (channels=2, frame_size=960, bitrate=96000)
    // ============================================================================
    // TOC = 0xfc (config=31, stereo=1, code=0)
    const CELT_STEREO_FB_20MS_PKT_0: [u8; 240] = [
        0xfc, 0xdf, 0xef, 0xcd, 0xd6, 0x70, 0x52, 0x6e, 0x1a, 0xdf, 0xcc, 0x9d,
        0x39, 0x04, 0x04, 0x9b, 0xd2, 0x20, 0x81, 0x9b, 0x9b, 0x9b, 0x18, 0x11,
        0x49, 0x4f, 0xcd, 0x8f, 0x1f, 0x3e, 0xfc, 0xa2, 0xca, 0x05, 0x9a, 0xf8,
        0x06, 0xd2, 0xdd, 0xc1, 0x57, 0xf0, 0x63, 0x09, 0xd3, 0xf6, 0x2b, 0x7a,
        0xc0, 0x6f, 0x1a, 0x90, 0x81, 0xe7, 0x52, 0x81, 0xff, 0x34, 0x21, 0xf3,
        0x4f, 0xe0, 0xee, 0x7d, 0x94, 0xc2, 0xf7, 0x18, 0x07, 0x5d, 0x90, 0xf1,
        0x17, 0x91, 0x53, 0xa0, 0x72, 0x1c, 0x94, 0xfa, 0x22, 0xfd, 0xcc, 0xf5,
        0xee, 0x6c, 0xa0, 0x1e, 0x03, 0x97, 0x52, 0x83, 0x77, 0x79, 0x70, 0xfc,
        0xa6, 0x24, 0xf3, 0x51, 0xf1, 0x4a, 0xfd, 0x19, 0x00, 0x3f, 0x02, 0x0b,
        0xa0, 0x4d, 0x59, 0xb7, 0x9f, 0x0e, 0x1d, 0xe1, 0x39, 0xcd, 0x46, 0x37,
        0x2d, 0x51, 0x32, 0xcf, 0xa4, 0x21, 0x15, 0x72, 0x32, 0x88, 0x42, 0xee,
        0x1a, 0xc6, 0x53, 0x53, 0xd6, 0xa2, 0xae, 0x36, 0x01, 0xd2, 0xe3, 0x84,
        0xf0, 0x97, 0x7a, 0xee, 0x27, 0xb8, 0x92, 0x32, 0xa3, 0xbc, 0x2d, 0x97,
        0x38, 0xab, 0xd6, 0x42, 0x0e, 0xde, 0x9e, 0xe4, 0x97, 0xbf, 0x4c, 0xa3,
        0xbf, 0xe5, 0x8b, 0xdc, 0xc1, 0xb8, 0x17, 0xfa, 0xa5, 0xcb, 0x06, 0x94,
        0x54, 0xd7, 0xd8, 0x87, 0x4b, 0x29, 0xa4, 0x46, 0xbb, 0xad, 0x60, 0x87,
        0x2d, 0x4f, 0x92, 0x64, 0xa8, 0x9d, 0x04, 0x5f, 0xf6, 0x34, 0x08, 0x1d,
        0x92, 0xd6, 0xce, 0xe4, 0x70, 0x40, 0xfb, 0xfc, 0x16, 0x0f, 0x82, 0xec,
        0xbe, 0x4f, 0x82, 0x0d, 0x8a, 0xe9, 0xf3, 0xd2, 0x0b, 0x0d, 0x1a, 0x44,
        0x94, 0x80, 0x25, 0x45, 0x25, 0xca, 0xbe, 0xe7, 0xd4, 0x94, 0xc3, 0x54,
    ];
    const CELT_STEREO_FB_20MS_OUT_L_0_HEAD_F32: [f32; 24] = [
        -0.00000292f32, -0.00001181f32, -0.0000621f32, -0.00014871f32, -0.00010634f32, 0.0000689f32,
        0.00020211f32, 0.00021361f32, -0.00013928f32, -0.00021651f32, 0.00050025f32, 0.00144145f32,
        0.00227632f32, 0.00137891f32, -0.00095384f32, -0.00276342f32, -0.00389967f32, -0.00232558f32,
        0.00099568f32, 0.00145871f32, -0.00104747f32, -0.0029293f32, -0.00062314f32, 0.00549238f32,
    ];
    const CELT_STEREO_FB_20MS_OUT_L_0_TAIL_F32: [f32; 24] = [
        0.3195691f32, 0.2889901f32, 0.26245102f32, 0.26676983f32, 0.2807982f32, 0.25089616f32,
        0.142232f32, -0.0407643f32, -0.24102245f32, -0.37922513f32, -0.41756484f32, -0.3765786f32,
        -0.31794715f32, -0.29969645f32, -0.3317373f32, -0.37303963f32, -0.37428084f32, -0.32633454f32,
        -0.2727314f32, -0.286413f32, -0.40026352f32, -0.56928396f32, -0.7077367f32, -0.7409001f32,
    ];
    const CELT_STEREO_FB_20MS_OUT_L_0_RMS: f32 = 0.29302272f32;
    const CELT_STEREO_FB_20MS_OUT_R_0_HEAD_F32: [f32; 24] = [
        -0.00000702f32, 0.00002132f32, 0.0000865f32, -0.00001516f32, -0.00032423f32, -0.00084343f32,
        -0.00078474f32, 0.00050109f32, 0.00018593f32, -0.00042559f32, 0.00018626f32, -0.00002435f32,
        0.00216909f32, 0.00244664f32, 0.00150559f32, 0.00241899f32, -0.00385944f32, -0.00722593f32,
        -0.00199231f32, -0.00041591f32, -0.00220864f32, -0.00268278f32, 0.00029703f32, 0.003073f32,
    ];
    const CELT_STEREO_FB_20MS_OUT_R_0_TAIL_F32: [f32; 24] = [
        -0.6645027f32, -0.6196998f32, -0.48903117f32, -0.40141556f32, -0.40248266f32, -0.4294567f32,
        -0.36195663f32, -0.23385926f32, -0.17192581f32, -0.23628634f32, -0.391206f32, -0.49079823f32,
        -0.42965105f32, -0.25814727f32, -0.11329499f32, -0.07002388f32, -0.07904232f32, -0.03934388f32,
        0.08575769f32, 0.17041937f32, 0.09309807f32, -0.11001436f32, -0.31709415f32, -0.3864448f32,
    ];
    const CELT_STEREO_FB_20MS_OUT_R_0_RMS: f32 = 0.26567492f32;

    // TOC = 0xfc (config=31, stereo=1, code=0)
    const CELT_STEREO_FB_20MS_PKT_1: [u8; 240] = [
        0xfc, 0xdd, 0x89, 0xf5, 0xe2, 0x3b, 0x63, 0x17, 0x2b, 0x65, 0xc4, 0xf7,
        0x7f, 0x94, 0x94, 0x54, 0x80, 0x38, 0x0f, 0x5b, 0x1e, 0xcc, 0xa3, 0x09,
        0x5d, 0x60, 0xf8, 0xf8, 0xe2, 0x48, 0xd2, 0x7d, 0x40, 0x71, 0x14, 0xe0,
        0x97, 0xa8, 0x0f, 0x2a, 0x5a, 0x4b, 0xa2, 0x50, 0x20, 0x40, 0xf3, 0xec,
        0xbd, 0x0b, 0xb6, 0x33, 0x63, 0x8c, 0x7c, 0x52, 0xc5, 0x2c, 0x52, 0xbe,
        0xc1, 0xd3, 0xee, 0xe3, 0xa7, 0xdd, 0xc5, 0x2c, 0x52, 0xc7, 0x4f, 0xee,
        0xe3, 0xa7, 0xbb, 0x8e, 0x17, 0x08, 0x7d, 0xc8, 0x73, 0x44, 0x72, 0x10,
        0xe7, 0xf6, 0x22, 0x87, 0x96, 0x0f, 0x20, 0x77, 0xe8, 0xba, 0xd7, 0x51,
        0xe4, 0x1f, 0x3b, 0x29, 0xa8, 0x2d, 0xe3, 0xe2, 0x5d, 0xed, 0xee, 0xfc,
        0x73, 0xc1, 0x45, 0xde, 0x4b, 0x2a, 0x25, 0xd4, 0x6b, 0xe3, 0x9e, 0x38,
        0xdc, 0xed, 0xe8, 0xfb, 0x95, 0x6d, 0x7f, 0x4e, 0xf3, 0x37, 0xd3, 0xc7,
        0x84, 0xa6, 0x43, 0x74, 0xfd, 0x41, 0xf8, 0xb2, 0xb2, 0x10, 0xa3, 0xa5,
        0x8b, 0x63, 0x19, 0x46, 0xf9, 0x00, 0x09, 0x29, 0x72, 0x87, 0x9f, 0xdb,
        0xb9, 0xa8, 0x8e, 0x87, 0xe3, 0xca, 0xe1, 0x12, 0x4c, 0x8b, 0x14, 0xa0,
        0x85, 0x29, 0xc0, 0x65, 0xfa, 0x33, 0xb9, 0x43, 0xd4, 0x4d, 0x39, 0x04,
        0x95, 0x7f, 0x6d, 0x70, 0x6b, 0x8c, 0x65, 0x5c, 0x0c, 0x1f, 0x34, 0x9a,
        0xe1, 0x8d, 0xff, 0x38, 0x55, 0x03, 0x88, 0x4f, 0x09, 0xf6, 0x03, 0x87,
        0xdc, 0xfd, 0x01, 0xa0, 0x75, 0xe3, 0xa7, 0xb2, 0xda, 0x19, 0xf0, 0x3c,
        0x9b, 0xc5, 0xb8, 0x6a, 0x1e, 0xdd, 0xe0, 0xa5, 0xa9, 0x54, 0x5b, 0xab,
        0x94, 0xf9, 0xc3, 0xae, 0x6b, 0xec, 0x7d, 0x7f, 0x26, 0x35, 0x9b, 0xe1,
    ];
    const CELT_STEREO_FB_20MS_OUT_L_1_HEAD_F32: [f32; 24] = [
        -0.66202164f32, -0.53451437f32, -0.43598038f32, -0.4027748f32, -0.3936966f32, -0.3391875f32,
        -0.2161514f32, -0.06385326f32, 0.03073535f32, -0.0010668f32, -0.14057687f32, -0.2937639f32,
        -0.35933822f32, -0.30552617f32, -0.19375078f32, -0.11840348f32, -0.12513378f32, -0.17596036f32,
        -0.18205702f32, -0.08743479f32, 0.06731574f32, 0.17305976f32, 0.14083907f32, -0.02491136f32,
    ];
    const CELT_STEREO_FB_20MS_OUT_L_1_TAIL_F32: [f32; 24] = [
        0.597741f32, 0.61185557f32, 0.6503413f32, 0.6980374f32, 0.70643705f32, 0.63034856f32,
        0.47157505f32, 0.2893122f32, 0.16149753f32, 0.12515563f32, 0.14727512f32, 0.15527609f32,
        0.10361161f32, 0.01734867f32, -0.0296802f32, 0.0137438f32, 0.11932705f32, 0.19235975f32,
        0.14886552f32, -0.00941968f32, -0.19084935f32, -0.288073f32, -0.26706922f32, -0.19147433f32,
    ];
    const CELT_STEREO_FB_20MS_OUT_L_1_RMS: f32 = 0.31116617f32;
    const CELT_STEREO_FB_20MS_OUT_R_1_HEAD_F32: [f32; 24] = [
        -0.31463075f32, -0.2279367f32, -0.21809852f32, -0.28592274f32, -0.2975283f32, -0.17952403f32,
        -0.05244486f32, -0.03993177f32, -0.17643112f32, -0.36728722f32, -0.43558145f32, -0.3284607f32,
        -0.15226819f32, -0.04208264f32, -0.02652145f32, 0.01442838f32, 0.19013232f32, 0.4312355f32,
        0.56555974f32, 0.5077031f32, 0.33543804f32, 0.22821039f32, 0.27882457f32, 0.3919757f32,
    ];
    const CELT_STEREO_FB_20MS_OUT_R_1_TAIL_F32: [f32; 24] = [
        -0.53992414f32, -0.46882132f32, -0.4056678f32, -0.40892208f32, -0.43311468f32, -0.3809783f32,
        -0.23018643f32, -0.08978809f32, -0.09964613f32, -0.26757383f32, -0.43499652f32, -0.4335205f32,
        -0.26817802f32, -0.11136939f32, -0.10230343f32, -0.18347272f32, -0.1745567f32, -0.00299241f32,
        0.1869591f32, 0.18802287f32, -0.02909105f32, -0.2754237f32, -0.35471353f32, -0.27427915f32,
    ];
    const CELT_STEREO_FB_20MS_OUT_R_1_RMS: f32 = 0.29328534f32;

    // ============================================================================
    // MODE TRANSITIONS: SILK (WB) -> Hybrid (FB) -> CELT (FB) -> SILK (WB)
    // ============================================================================
    // Step 0 (SILK_WB): TOC = 0x4b (config=9)
    const TRANSITION_0_SILK_WB_PKT: [u8; 120] = [
        0x4b, 0x41, 0x02, 0x83, 0xca, 0x24, 0x8a, 0x8a, 0x11, 0x31, 0xa9, 0xa3,
        0xde, 0x35, 0xde, 0x25, 0x0f, 0x27, 0xb6, 0xb5, 0x04, 0x8a, 0xe7, 0x96,
        0xdf, 0x29, 0xbf, 0x62, 0xa1, 0x74, 0x48, 0x4e, 0x1d, 0x8d, 0x5d, 0x47,
        0x10, 0xfd, 0xc4, 0x11, 0x94, 0x77, 0x7b, 0x6d, 0xec, 0xb4, 0xa0, 0xee,
        0xbb, 0x26, 0xfc, 0x3f, 0x5c, 0x8f, 0x84, 0x81, 0x80, 0x74, 0x44, 0x26,
        0x97, 0x36, 0xd7, 0x62, 0x00, 0x8e, 0xc6, 0xb4, 0x10, 0x84, 0x89, 0x87,
        0xac, 0x54, 0x19, 0xe3, 0xdb, 0x90, 0xe2, 0x29, 0x39, 0xa2, 0xb4, 0x46,
        0x0c, 0xa5, 0x36, 0xa4, 0xed, 0x71, 0x46, 0x27, 0xd4, 0x9d, 0xdc, 0x53,
        0xdd, 0xac, 0x13, 0x17, 0x1b, 0xbd, 0x19, 0x0c, 0x48, 0x21, 0xc3, 0xd0,
        0xfc, 0x33, 0x92, 0x95, 0x52, 0xa7, 0x74, 0xdc, 0xe7, 0xc0, 0x00, 0x00,
    ];
    const TRANSITION_0_SILK_WB_OUT_HEAD_F32: [f32; 24] = [
        0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32,
        0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32,
        0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32,
        0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32,
    ];
    const TRANSITION_0_SILK_WB_OUT_TAIL_F32: [f32; 24] = [
        -0.2720642f32, -0.40100098f32, -0.43380737f32, -0.36779785f32, -0.26687622f32, -0.21020508f32,
        -0.22949219f32, -0.28704834f32, -0.3140869f32, -0.27661133f32, -0.2102356f32, -0.1923523f32,
        -0.2746277f32, -0.43417358f32, -0.5860901f32, -0.646698f32, -0.5956116f32, -0.484375f32,
        -0.38946533f32, -0.34994507f32, -0.34176636f32, -0.30581665f32, -0.20458984f32, -0.05892944f32,
    ];
    const TRANSITION_0_SILK_WB_OUT_RMS: f32 = 0.2568071f32;

    // Step 1 (HYBRID_FB): TOC = 0x78 (config=15)
    const TRANSITION_1_HYBRID_FB_PKT: [u8; 120] = [
        0x78, 0xa6, 0x62, 0x0a, 0x23, 0x9b, 0x6b, 0x13, 0x83, 0x39, 0xed, 0x87,
        0x94, 0xf2, 0xb7, 0xd6, 0xae, 0x50, 0xe5, 0x17, 0x75, 0xf3, 0x92, 0x01,
        0xa3, 0x4a, 0xc9, 0xba, 0x0f, 0x41, 0x71, 0x8f, 0xfc, 0x14, 0x20, 0x3f,
        0xc9, 0x3a, 0xc5, 0x2a, 0xf4, 0x68, 0xc6, 0xf4, 0x08, 0x02, 0xc4, 0xff,
        0xc2, 0x8d, 0xc4, 0xfb, 0xc3, 0x31, 0x98, 0x0e, 0xfc, 0x6c, 0x44, 0x18,
        0xc1, 0x3b, 0xe5, 0x1a, 0x5f, 0x29, 0x56, 0x39, 0x18, 0x22, 0x34, 0xa1,
        0xe7, 0x75, 0x19, 0xc4, 0xf2, 0x02, 0xf5, 0xc5, 0xcc, 0xd4, 0x0a, 0xfb,
        0xc7, 0x52, 0x9a, 0x88, 0x41, 0xb6, 0x1e, 0x9e, 0x9e, 0x0d, 0x9e, 0x47,
        0x61, 0x04, 0x78, 0xad, 0x9f, 0xc6, 0x48, 0x21, 0xd4, 0x1f, 0xab, 0xb9,
        0x06, 0x4b, 0xeb, 0xde, 0x68, 0xdc, 0xb9, 0xf6, 0xe4, 0x49, 0x08, 0xc3,
    ];
    const TRANSITION_1_HYBRID_FB_OUT_HEAD_F32: [f32; 24] = [
        0.06332397f32, 0.09875474f32, 0.03775003f32, -0.06390347f32, -0.12368642f32, -0.09097159f32,
        0.01803527f32, 0.13686837f32, 0.20467873f32, 0.21664552f32, 0.2254996f32, 0.29224068f32,
        0.42556563f32, 0.56484205f32, 0.6231322f32, 0.5577422f32, 0.4075123f32, 0.26386347f32,
        0.20031267f32, 0.21879579f32, 0.25786352f32, 0.25235763f32, 0.19215794f32, 0.12937416f32,
    ];
    const TRANSITION_1_HYBRID_FB_OUT_TAIL_F32: [f32; 24] = [
        -0.03846251f32, -0.17778048f32, -0.21961983f32, -0.17058241f32, -0.0969716f32, -0.0720764f32,
        -0.11870339f32, -0.19501851f32, -0.23592098f32, -0.21596847f32, -0.17678352f32, -0.19433667f32,
        -0.31209096f32, -0.49844146f32, -0.6659308f32, -0.7365077f32, -0.6990785f32, -0.61067396f32,
        -0.54505956f32, -0.53251284f32, -0.5413463f32, -0.51308644f32, -0.41914353f32, -0.29007196f32,
    ];
    const TRANSITION_1_HYBRID_FB_OUT_RMS: f32 = 0.3119331f32;

    // Step 2 (CELT_FB): TOC = 0x78 (config=15)
    const TRANSITION_2_CELT_FB_PKT: [u8; 120] = [
        0x78, 0xa6, 0xc7, 0x33, 0xe0, 0xda, 0x87, 0xcc, 0xd1, 0xfa, 0x24, 0x20,
        0x3a, 0x22, 0x8b, 0xb8, 0xf8, 0x6f, 0xbb, 0x68, 0x12, 0x7d, 0x2a, 0xb2,
        0x90, 0x11, 0x5e, 0x81, 0x41, 0x4e, 0xc5, 0xae, 0xf6, 0xfd, 0x96, 0xa1,
        0x10, 0x45, 0xb6, 0x93, 0x5d, 0xda, 0xbc, 0xd3, 0x81, 0xa3, 0xc2, 0x73,
        0x75, 0x2b, 0x42, 0x4c, 0x86, 0x30, 0xd5, 0x7e, 0x5a, 0xe1, 0x26, 0xb2,
        0x0a, 0xc4, 0x28, 0x16, 0x10, 0xad, 0x51, 0x39, 0x42, 0xfd, 0xe6, 0x74,
        0x2d, 0x42, 0xd6, 0x8f, 0xbd, 0xa9, 0x74, 0x07, 0x5b, 0x76, 0x47, 0x2a,
        0x54, 0x86, 0x9c, 0x66, 0xac, 0xe8, 0x70, 0x67, 0x7e, 0x73, 0x85, 0x1d,
        0x43, 0x8d, 0xfa, 0xd1, 0xf2, 0xb2, 0xb8, 0x26, 0x21, 0x37, 0xb3, 0x88,
        0x86, 0x64, 0xcb, 0x8b, 0x2a, 0x34, 0x35, 0x09, 0x84, 0x8e, 0x74, 0x7a,
    ];
    const TRANSITION_2_CELT_FB_OUT_HEAD_F32: [f32; 24] = [
        -0.1957514f32, -0.19187701f32, -0.27631712f32, -0.38761616f32, -0.44825056f32, -0.41877383f32,
        -0.32306862f32, -0.22518079f32, -0.17590716f32, -0.17136568f32, -0.15894236f32, -0.08667168f32,
        0.0442205f32, 0.17047092f32, 0.21194504f32, 0.13636303f32, -0.01167012f32, -0.14379717f32,
        -0.19522285f32, -0.17121515f32, -0.13341844f32, -0.14022219f32, -0.19372368f32, -0.23900947f32,
    ];
    const TRANSITION_2_CELT_FB_OUT_TAIL_F32: [f32; 24] = [
        0.4261471f32, 0.3539164f32, 0.32017478f32, 0.31519702f32, 0.3295257f32, 0.35655048f32,
        0.38553923f32, 0.39980492f32, 0.38716397f32, 0.35193765f32, 0.31137514f32, 0.27475512f32,
        0.22571285f32, 0.13148597f32, -0.02176565f32, -0.19903961f32, -0.327093f32, -0.3469542f32,
        -0.26660222f32, -0.1629442f32, -0.12254903f32, -0.1679734f32, -0.2369739f32, -0.23981929f32,
    ];
    const TRANSITION_2_CELT_FB_OUT_RMS: f32 = 0.30973357f32;

    // Step 3 (SILK_WB_RETURN): TOC = 0x4b (config=9)
    const TRANSITION_3_SILK_WB_RETURN_PKT: [u8; 120] = [
        0x4b, 0x41, 0x02, 0x95, 0x93, 0xf7, 0x94, 0x08, 0x2b, 0xad, 0xde, 0x3b,
        0xb8, 0x53, 0xe5, 0xad, 0xb2, 0x06, 0x73, 0x37, 0x23, 0x16, 0xf0, 0x87,
        0xbc, 0x7e, 0x86, 0x25, 0xa5, 0x79, 0xc7, 0x8d, 0x67, 0x04, 0xf0, 0x02,
        0x3f, 0xcb, 0x0e, 0xa7, 0x32, 0xee, 0x98, 0x7f, 0x00, 0x3c, 0xe2, 0x8f,
        0x72, 0xb8, 0x8d, 0xf1, 0x27, 0x3d, 0x67, 0x4b, 0xe9, 0x29, 0xf1, 0x37,
        0x75, 0x9c, 0xeb, 0x88, 0x60, 0x43, 0x3c, 0x6f, 0x46, 0x00, 0x34, 0x3b,
        0xc4, 0x6a, 0xe7, 0x72, 0x03, 0x44, 0xd5, 0x7e, 0xab, 0xa7, 0x20, 0x6e,
        0x1f, 0x90, 0xd3, 0x53, 0xa5, 0x92, 0x66, 0xfb, 0xfb, 0x33, 0xeb, 0x77,
        0xe6, 0xf6, 0x68, 0x10, 0x41, 0xca, 0xf4, 0x25, 0x02, 0x2b, 0x9e, 0xab,
        0x14, 0xa2, 0x4f, 0xf4, 0xc2, 0xf4, 0x9a, 0xd9, 0xc9, 0xe3, 0x00, 0x00,
    ];
    const TRANSITION_3_SILK_WB_RETURN_OUT_HEAD_F32: [f32; 24] = [
        -0.14798525f32, -0.03243579f32, -0.00938609f32, -0.13512535f32, -0.34421688f32, -0.49274147f32,
        -0.47544524f32, -0.31571254f32, -0.14664248f32, -0.09622506f32, -0.1762341f32, -0.27416447f32,
        -0.25726333f32, -0.09711263f32, 0.09671141f32, 0.16576864f32, 0.03780626f32, -0.21001355f32,
        -0.41445893f32, -0.4602661f32, -0.36866248f32, -0.26662576f32, -0.26874518f32, -0.378149f32,
    ];
    const TRANSITION_3_SILK_WB_RETURN_OUT_TAIL_F32: [f32; 24] = [
        0.2562256f32, 0.1524353f32, 0.2026062f32, 0.34536743f32, 0.45681763f32, 0.44985962f32,
        0.3440857f32, 0.24731445f32, 0.26193237f32, 0.39068604f32, 0.5255127f32, 0.5325012f32,
        0.36569214f32, 0.11254883f32, -0.07168579f32, -0.08700562f32, 0.0340271f32, 0.16119385f32,
        0.1809082f32, 0.09185791f32, 0.00045776f32, 0.02374268f32, 0.1845398f32, 0.3887024f32,
    ];
    const TRANSITION_3_SILK_WB_RETURN_OUT_RMS: f32 = 0.32217064f32;

    // ============================================================================
    // MULTISTREAM FAMILY 1 (3ch: streams=2, coupled_streams=1, mapping=[0, 2, 1])
    // ============================================================================
    // Multistream pkt 0: total_len=240, stream0_self_delimited_len=149 (frames=1, size0=147)
    const MS_3CH_PKT_0: [u8; 240] = [
        0xfc, 0x93, 0xb5, 0x3f, 0xc1, 0x90, 0x13, 0x3e, 0xb7, 0xdb, 0xc4, 0x4f,
        0x1a, 0xa3, 0xf3, 0x59, 0xd4, 0xf3, 0x0c, 0x94, 0xdf, 0x91, 0x4a, 0x64,
        0x8d, 0xd7, 0x2c, 0xd2, 0xb9, 0x9b, 0x0d, 0x82, 0x63, 0x08, 0x5d, 0xa3,
        0x3a, 0xb5, 0xf6, 0xbf, 0x44, 0xf2, 0xea, 0x8b, 0x97, 0x18, 0x44, 0x24,
        0x52, 0x3f, 0x48, 0x5e, 0x55, 0x0d, 0x8c, 0x9f, 0xdb, 0xfb, 0x6c, 0xf9,
        0x50, 0x41, 0x42, 0xa9, 0xb9, 0x7e, 0x8a, 0x41, 0x52, 0xbb, 0x74, 0xa3,
        0xbe, 0x21, 0xe1, 0xb1, 0x9c, 0x5f, 0xae, 0x4f, 0xd0, 0xd0, 0xa5, 0xd4,
        0xe4, 0xb5, 0x53, 0xdd, 0xba, 0x97, 0xa1, 0x7a, 0xa5, 0xd4, 0xd0, 0x00,
        0x7c, 0xec, 0x7e, 0x9f, 0x64, 0x16, 0x05, 0x65, 0xde, 0x00, 0xd8, 0xe3,
        0x1f, 0x13, 0x6b, 0xc9, 0x1a, 0xdc, 0x5a, 0x4b, 0x14, 0xc2, 0x37, 0xc2,
        0xf1, 0x2d, 0x16, 0x23, 0x69, 0x48, 0x63, 0xbf, 0x1a, 0x64, 0x91, 0x7d,
        0x59, 0x65, 0x0a, 0x55, 0xa6, 0x0d, 0x31, 0x55, 0xd0, 0x60, 0xa3, 0x20,
        0xdb, 0xb0, 0x19, 0x3f, 0x72, 0x78, 0x84, 0x9d, 0xae, 0x42, 0x11, 0xe4,
        0x26, 0xf7, 0xda, 0x80, 0x20, 0xb5, 0x0c, 0xbe, 0xcc, 0xc5, 0x88, 0xd4,
        0x38, 0x79, 0x58, 0x38, 0xc8, 0x26, 0x34, 0xf8, 0x22, 0xdb, 0xc3, 0xf3,
        0xf9, 0x7f, 0x3c, 0xaa, 0x00, 0xc5, 0xa3, 0x6b, 0xba, 0xfa, 0x96, 0xed,
        0x5c, 0x5c, 0x1b, 0x55, 0xb0, 0x81, 0x6b, 0xec, 0xe0, 0xa6, 0x64, 0xac,
        0x4e, 0xa9, 0xca, 0x96, 0x3d, 0x2e, 0x6d, 0x5d, 0x4d, 0x2a, 0x7b, 0x6d,
        0xe8, 0xbc, 0x98, 0xb2, 0x60, 0x14, 0x56, 0xed, 0x04, 0x0b, 0x02, 0x87,
        0xa8, 0x25, 0x77, 0x99, 0xb9, 0x06, 0xd7, 0x06, 0xf3, 0x42, 0x58, 0xde,
    ];
    const MS_3CH_OUT_CH0_0_HEAD_F32: [f32; 24] = [
        0.0f32, -0.0f32, -0.0f32, 0.0f32, 0.0f32, -0.0f32,
        0.0f32, -0.0f32, -0.0f32, -0.0f32, -0.0f32, -0.0f32,
        -0.0f32, -0.0f32, -0.0f32, -0.0f32, -0.0f32, -0.0f32,
        -0.0f32, -0.0f32, -0.0f32, 0.0f32, 0.0f32, 0.0f32,
    ];
    const MS_3CH_OUT_CH0_0_TAIL_F32: [f32; 24] = [
        -0.17438415f32, -0.2401244f32, -0.24763016f32, -0.2443027f32, -0.27998435f32, -0.3554502f32,
        -0.4257416f32, -0.4335428f32, -0.35288644f32, -0.22755334f32, -0.13657928f32, -0.13817585f32,
        -0.23022234f32, -0.34970158f32, -0.4297772f32, -0.44806755f32, -0.44025224f32, -0.45852184f32,
        -0.518749f32, -0.59216815f32, -0.61483526f32, -0.53943455f32, -0.38234046f32, -0.21063145f32,
    ];
    const MS_3CH_OUT_CH0_0_RMS: f32 = 0.26055145f32;
    const MS_3CH_OUT_CH1_0_HEAD_F32: [f32; 24] = [
        0.0f32, -0.0f32, 0.0f32, 0.0f32, -0.0f32, -0.0f32,
        -0.0f32, -0.0f32, 0.0f32, 0.0f32, -0.0f32, -0.0f32,
        0.0f32, 0.0f32, -0.0f32, 0.0f32, 0.0f32, -0.0f32,
        -0.0f32, -0.0f32, 0.0f32, 0.0f32, 0.0f32, -0.0f32,
    ];
    const MS_3CH_OUT_CH1_0_TAIL_F32: [f32; 24] = [
        0.24636096f32, 0.15340133f32, 0.13860513f32, 0.07613062f32, -0.05178681f32, -0.1699689f32,
        -0.18510437f32, -0.07507397f32, 0.02322089f32, -0.00075736f32, -0.1480507f32, -0.3144607f32,
        -0.39384305f32, -0.37674966f32, -0.34411296f32, -0.40772644f32, -0.49714035f32, -0.4755177f32,
        -0.30746138f32, -0.06983455f32, 0.04719018f32, -0.00310528f32, -0.11193174f32, -0.16469806f32,
    ];
    const MS_3CH_OUT_CH1_0_RMS: f32 = 0.22965334f32;
    const MS_3CH_OUT_CH2_0_HEAD_F32: [f32; 24] = [
        0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32,
        0.0f32, 0.0f32, 0.0f32, 0.0f32, -0.0f32, -0.0f32,
        -0.0f32, -0.0f32, -0.0f32, -0.0f32, -0.0f32, -0.0f32,
        -0.0f32, -0.0f32, -0.0f32, -0.0f32, -0.0f32, -0.0f32,
    ];
    const MS_3CH_OUT_CH2_0_TAIL_F32: [f32; 24] = [
        0.21079724f32, 0.14846714f32, 0.09228598f32, 0.04381455f32, 0.00157582f32, -0.0213842f32,
        -0.02750464f32, -0.02321088f32, 0.00559427f32, 0.04851797f32, 0.10017876f32, 0.1569725f32,
        0.20758492f32, 0.25621274f32, 0.29932845f32, 0.3357263f32, 0.35414565f32, 0.34893337f32,
        0.3328866f32, 0.29416394f32, 0.23648538f32, 0.17349942f32, 0.10355857f32, 0.03295793f32,
    ];
    const MS_3CH_OUT_CH2_0_RMS: f32 = 0.19460475f32;

    // Multistream pkt 1: total_len=240, stream0_self_delimited_len=149 (frames=1, size0=147)
    const MS_3CH_PKT_1: [u8; 240] = [
        0xfc, 0x93, 0xb2, 0xfc, 0x66, 0xe6, 0x5a, 0x8e, 0x96, 0xfd, 0x2a, 0x70,
        0xbe, 0x6e, 0x23, 0xa9, 0x5f, 0xe9, 0xf0, 0x99, 0x8a, 0x91, 0xe7, 0x7d,
        0x20, 0x96, 0x87, 0xa9, 0xb3, 0x46, 0xcd, 0xbd, 0xaf, 0xcb, 0xa8, 0xdc,
        0x62, 0xd1, 0xf2, 0xb6, 0xb4, 0xe2, 0xd3, 0x61, 0x76, 0x17, 0x61, 0x76,
        0x17, 0x61, 0x79, 0x93, 0x90, 0x73, 0x27, 0x20, 0xe0, 0xa2, 0x07, 0xa2,
        0x57, 0xbd, 0xe9, 0x91, 0xaa, 0x63, 0x41, 0xe8, 0xb0, 0x64, 0xfc, 0xba,
        0x95, 0x5b, 0xee, 0xcd, 0xa0, 0x09, 0xe2, 0x02, 0x64, 0xfb, 0x12, 0x39,
        0x0e, 0x54, 0xd3, 0xa5, 0xc1, 0x92, 0xc6, 0x08, 0x4f, 0xcb, 0x99, 0xc3,
        0x03, 0x1f, 0x3d, 0x13, 0xd0, 0xdb, 0x7d, 0xfa, 0x0c, 0x1b, 0x21, 0xcf,
        0xfb, 0x81, 0xf6, 0xa6, 0x73, 0x0d, 0xc9, 0x02, 0x5e, 0x1d, 0x74, 0xa7,
        0x8a, 0xc9, 0xec, 0xfc, 0x8b, 0x0e, 0x3f, 0xf8, 0x67, 0x6d, 0x9b, 0x22,
        0xbb, 0x5c, 0x15, 0xc3, 0x20, 0xe9, 0x42, 0xae, 0x4c, 0x7e, 0x67, 0xe8,
        0x52, 0x32, 0xb4, 0xb9, 0x72, 0x78, 0xaa, 0xfc, 0x2b, 0x2d, 0x5d, 0x1a,
        0x34, 0xed, 0xbe, 0x35, 0xb7, 0xa9, 0x49, 0x6a, 0x7e, 0xc2, 0x51, 0x81,
        0xab, 0x0f, 0x84, 0x07, 0xb3, 0xf6, 0xcb, 0x45, 0x81, 0x34, 0xc1, 0xcc,
        0x9b, 0x76, 0x95, 0x94, 0x2b, 0x13, 0x76, 0x59, 0x99, 0xf7, 0xd8, 0x5c,
        0xe4, 0xe0, 0xda, 0x82, 0x3b, 0x5a, 0x16, 0x55, 0xcd, 0x01, 0xb5, 0xd1,
        0x24, 0xc8, 0x44, 0x83, 0x5f, 0xb2, 0x2d, 0x2d, 0x50, 0x71, 0x2f, 0x28,
        0xb3, 0x78, 0x58, 0x40, 0x39, 0x3c, 0xc4, 0xf0, 0xfa, 0x78, 0xa2, 0xd2,
        0x57, 0x81, 0xc7, 0xff, 0x8c, 0x34, 0x5a, 0x9c, 0x48, 0x8f, 0xf3, 0xf2,
    ];
    const MS_3CH_OUT_CH0_1_HEAD_F32: [f32; 24] = [
        -0.09892515f32, -0.07269688f32, -0.09591347f32, -0.1054665f32, -0.06262508f32, 0.01030403f32,
        0.0629319f32, 0.06060874f32, 0.01285994f32, -0.01990942f32, 0.02143808f32, 0.14567095f32,
        0.3005367f32, 0.41316193f32, 0.4500647f32, 0.43106112f32, 0.4100167f32, 0.42010206f32,
        0.4429083f32, 0.43946683f32, 0.38153315f32, 0.2812288f32, 0.18796608f32, 0.14911637f32,
    ];
    const MS_3CH_OUT_CH0_1_TAIL_F32: [f32; 24] = [
        0.09532706f32, -0.01348571f32, -0.02716826f32, 0.03464718f32, 0.0892074f32, 0.05899974f32,
        -0.06374542f32, -0.20954633f32, -0.28788984f32, -0.26153895f32, -0.17815216f32, -0.13311341f32,
        -0.19341959f32, -0.34402654f32, -0.49965936f32, -0.5721153f32, -0.53879714f32, -0.45808354f32,
        -0.42042392f32, -0.47516555f32, -0.5913629f32, -0.68266654f32, -0.67541206f32, -0.5659787f32,
    ];
    const MS_3CH_OUT_CH0_1_RMS: f32 = 0.3041686f32;
    const MS_3CH_OUT_CH1_1_HEAD_F32: [f32; 24] = [
        -0.12720817f32, -0.08687872f32, -0.13155755f32, -0.2736168f32, -0.3587911f32, -0.2677006f32,
        -0.08517006f32, 0.04265523f32, -0.02105068f32, -0.19810894f32, -0.30863583f32, -0.32059604f32,
        -0.30441672f32, -0.37259716f32, -0.52878076f32, -0.66412044f32, -0.6268667f32, -0.40763438f32,
        -0.20025213f32, -0.12862936f32, -0.2003681f32, -0.2569427f32, -0.16706598f32, -0.0437484f32,
    ];
    const MS_3CH_OUT_CH1_1_TAIL_F32: [f32; 24] = [
        0.19885625f32, 0.12506333f32, 0.1063849f32, 0.05153473f32, -0.07106887f32, -0.1878409f32,
        -0.20105678f32, -0.10616004f32, -0.01123814f32, -0.03169706f32, -0.17206597f32, -0.32426068f32,
        -0.38409346f32, -0.35703415f32, -0.337216f32, -0.38688746f32, -0.45546737f32, -0.43249404f32,
        -0.27719337f32, -0.07830158f32, 0.02717194f32, -0.01011585f32, -0.10883341f32, -0.15276504f32,
    ];
    const MS_3CH_OUT_CH1_1_RMS: f32 = 0.28467718f32;
    const MS_3CH_OUT_CH2_1_HEAD_F32: [f32; 24] = [
        -0.04036196f32, -0.11032866f32, -0.17012483f32, -0.21443698f32, -0.23239245f32, -0.23731577f32,
        -0.22550423f32, -0.19055972f32, -0.14852937f32, -0.09793626f32, -0.03998401f32, 0.02320671f32,
        0.08393256f32, 0.12946603f32, 0.16127743f32, 0.16998729f32, 0.16186288f32, 0.14155208f32,
        0.09612508f32, 0.03751067f32, -0.03087893f32, -0.11074441f32, -0.19606669f32, -0.27928114f32,
    ];
    const MS_3CH_OUT_CH2_1_TAIL_F32: [f32; 24] = [
        -0.23572081f32, -0.2910532f32, -0.33629003f32, -0.36739078f32, -0.38135773f32, -0.37646064f32,
        -0.35236746f32, -0.31016764f32, -0.25229093f32, -0.18232627f32, -0.10475183f32, -0.02460097f32,
        0.05291316f32, 0.12278633f32, 0.18059565f32, 0.2228283f32, 0.24714072f32, 0.25253f32,
        0.23940459f32, 0.20955211f32, 0.16600522f32, 0.11281195f32, 0.05473247f32, -0.00311761f32,
    ];
    const MS_3CH_OUT_CH2_1_RMS: f32 = 0.24902533f32;

    #[test]
    fn opus_decoder_hybrid_mono_and_stereo_match_libopus() {
        let mut mono_params = AudioCodecParameters::new();
        mono_params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(Position::FRONT_CENTER));

        assert_mono_f32_packets!(
            &mono_params,
            480,
            [
                (
                    HYBRID_MONO_SWB_10MS_PKT_0,
                    HYBRID_MONO_SWB_10MS_OUT_0_HEAD_F32,
                    HYBRID_MONO_SWB_10MS_OUT_0_TAIL_F32,
                    HYBRID_MONO_SWB_10MS_OUT_0_RMS,
                ),
                (
                    HYBRID_MONO_SWB_10MS_PKT_1,
                    HYBRID_MONO_SWB_10MS_OUT_1_HEAD_F32,
                    HYBRID_MONO_SWB_10MS_OUT_1_TAIL_F32,
                    HYBRID_MONO_SWB_10MS_OUT_1_RMS,
                ),
                (
                    HYBRID_MONO_SWB_10MS_PKT_2,
                    HYBRID_MONO_SWB_10MS_OUT_2_HEAD_F32,
                    HYBRID_MONO_SWB_10MS_OUT_2_TAIL_F32,
                    HYBRID_MONO_SWB_10MS_OUT_2_RMS,
                ),
            ]
        );

        assert_mono_f32_packets!(
            &mono_params,
            960,
            [
                (
                    HYBRID_MONO_FB_20MS_PKT_0,
                    HYBRID_MONO_FB_20MS_OUT_0_HEAD_F32,
                    HYBRID_MONO_FB_20MS_OUT_0_TAIL_F32,
                    HYBRID_MONO_FB_20MS_OUT_0_RMS,
                ),
                (
                    HYBRID_MONO_FB_20MS_PKT_1,
                    HYBRID_MONO_FB_20MS_OUT_1_HEAD_F32,
                    HYBRID_MONO_FB_20MS_OUT_1_TAIL_F32,
                    HYBRID_MONO_FB_20MS_OUT_1_RMS,
                ),
                (
                    HYBRID_MONO_FB_20MS_PKT_2,
                    HYBRID_MONO_FB_20MS_OUT_2_HEAD_F32,
                    HYBRID_MONO_FB_20MS_OUT_2_TAIL_F32,
                    HYBRID_MONO_FB_20MS_OUT_2_RMS,
                ),
            ]
        );

        let mut stereo_params = AudioCodecParameters::new();
        stereo_params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(
                Position::FRONT_LEFT | Position::FRONT_RIGHT,
            ));

        assert_stereo_f32_packets!(
            &stereo_params,
            960,
            [
                (
                    HYBRID_STEREO_SWB_20MS_PKT_0,
                    HYBRID_STEREO_SWB_20MS_OUT_L_0_HEAD_F32,
                    HYBRID_STEREO_SWB_20MS_OUT_L_0_TAIL_F32,
                    HYBRID_STEREO_SWB_20MS_OUT_L_0_RMS,
                    HYBRID_STEREO_SWB_20MS_OUT_R_0_HEAD_F32,
                    HYBRID_STEREO_SWB_20MS_OUT_R_0_TAIL_F32,
                    HYBRID_STEREO_SWB_20MS_OUT_R_0_RMS,
                ),
                (
                    HYBRID_STEREO_SWB_20MS_PKT_1,
                    HYBRID_STEREO_SWB_20MS_OUT_L_1_HEAD_F32,
                    HYBRID_STEREO_SWB_20MS_OUT_L_1_TAIL_F32,
                    HYBRID_STEREO_SWB_20MS_OUT_L_1_RMS,
                    HYBRID_STEREO_SWB_20MS_OUT_R_1_HEAD_F32,
                    HYBRID_STEREO_SWB_20MS_OUT_R_1_TAIL_F32,
                    HYBRID_STEREO_SWB_20MS_OUT_R_1_RMS,
                ),
            ]
        );

        assert_stereo_f32_packets!(
            &stereo_params,
            960,
            [
                (
                    HYBRID_STEREO_FB_20MS_PKT_0,
                    HYBRID_STEREO_FB_20MS_OUT_L_0_HEAD_F32,
                    HYBRID_STEREO_FB_20MS_OUT_L_0_TAIL_F32,
                    HYBRID_STEREO_FB_20MS_OUT_L_0_RMS,
                    HYBRID_STEREO_FB_20MS_OUT_R_0_HEAD_F32,
                    HYBRID_STEREO_FB_20MS_OUT_R_0_TAIL_F32,
                    HYBRID_STEREO_FB_20MS_OUT_R_0_RMS,
                ),
                (
                    HYBRID_STEREO_FB_20MS_PKT_1,
                    HYBRID_STEREO_FB_20MS_OUT_L_1_HEAD_F32,
                    HYBRID_STEREO_FB_20MS_OUT_L_1_TAIL_F32,
                    HYBRID_STEREO_FB_20MS_OUT_L_1_RMS,
                    HYBRID_STEREO_FB_20MS_OUT_R_1_HEAD_F32,
                    HYBRID_STEREO_FB_20MS_OUT_R_1_TAIL_F32,
                    HYBRID_STEREO_FB_20MS_OUT_R_1_RMS,
                ),
            ]
        );
        return;
    }

    #[test]
    fn opus_decoder_celt_only_all_frame_sizes_match_libopus() {
        let mut mono_params = AudioCodecParameters::new();
        mono_params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(Position::FRONT_CENTER));

        // 2.5 ms (120 samples)
        assert_mono_f32_packets!(
            &mono_params,
            120,
            [
                (
                    CELT_MONO_FB_2_5MS_PKT_0,
                    CELT_MONO_FB_2_5MS_OUT_0_HEAD_F32,
                    CELT_MONO_FB_2_5MS_OUT_0_TAIL_F32,
                    CELT_MONO_FB_2_5MS_OUT_0_RMS,
                ),
                (
                    CELT_MONO_FB_2_5MS_PKT_1,
                    CELT_MONO_FB_2_5MS_OUT_1_HEAD_F32,
                    CELT_MONO_FB_2_5MS_OUT_1_TAIL_F32,
                    CELT_MONO_FB_2_5MS_OUT_1_RMS,
                ),
                (
                    CELT_MONO_FB_2_5MS_PKT_2,
                    CELT_MONO_FB_2_5MS_OUT_2_HEAD_F32,
                    CELT_MONO_FB_2_5MS_OUT_2_TAIL_F32,
                    CELT_MONO_FB_2_5MS_OUT_2_RMS,
                ),
            ]
        );

        // 5 ms (240 samples)
        assert_mono_f32_packets!(
            &mono_params,
            240,
            [
                (
                    CELT_MONO_FB_5MS_PKT_0,
                    CELT_MONO_FB_5MS_OUT_0_HEAD_F32,
                    CELT_MONO_FB_5MS_OUT_0_TAIL_F32,
                    CELT_MONO_FB_5MS_OUT_0_RMS,
                ),
                (
                    CELT_MONO_FB_5MS_PKT_1,
                    CELT_MONO_FB_5MS_OUT_1_HEAD_F32,
                    CELT_MONO_FB_5MS_OUT_1_TAIL_F32,
                    CELT_MONO_FB_5MS_OUT_1_RMS,
                ),
                (
                    CELT_MONO_FB_5MS_PKT_2,
                    CELT_MONO_FB_5MS_OUT_2_HEAD_F32,
                    CELT_MONO_FB_5MS_OUT_2_TAIL_F32,
                    CELT_MONO_FB_5MS_OUT_2_RMS,
                ),
            ]
        );

        // 10 ms (480 samples)
        assert_mono_f32_packets!(
            &mono_params,
            480,
            [
                (
                    CELT_MONO_FB_10MS_PKT_0,
                    CELT_MONO_FB_10MS_OUT_0_HEAD_F32,
                    CELT_MONO_FB_10MS_OUT_0_TAIL_F32,
                    CELT_MONO_FB_10MS_OUT_0_RMS,
                ),
                (
                    CELT_MONO_FB_10MS_PKT_1,
                    CELT_MONO_FB_10MS_OUT_1_HEAD_F32,
                    CELT_MONO_FB_10MS_OUT_1_TAIL_F32,
                    CELT_MONO_FB_10MS_OUT_1_RMS,
                ),
                (
                    CELT_MONO_FB_10MS_PKT_2,
                    CELT_MONO_FB_10MS_OUT_2_HEAD_F32,
                    CELT_MONO_FB_10MS_OUT_2_TAIL_F32,
                    CELT_MONO_FB_10MS_OUT_2_RMS,
                ),
            ]
        );

        // 20 ms (960 samples)
        assert_mono_f32_packets!(
            &mono_params,
            960,
            [
                (
                    CELT_MONO_FB_20MS_PKT_0,
                    CELT_MONO_FB_20MS_OUT_0_HEAD_F32,
                    CELT_MONO_FB_20MS_OUT_0_TAIL_F32,
                    CELT_MONO_FB_20MS_OUT_0_RMS,
                ),
                (
                    CELT_MONO_FB_20MS_PKT_1,
                    CELT_MONO_FB_20MS_OUT_1_HEAD_F32,
                    CELT_MONO_FB_20MS_OUT_1_TAIL_F32,
                    CELT_MONO_FB_20MS_OUT_1_RMS,
                ),
                (
                    CELT_MONO_FB_20MS_PKT_2,
                    CELT_MONO_FB_20MS_OUT_2_HEAD_F32,
                    CELT_MONO_FB_20MS_OUT_2_TAIL_F32,
                    CELT_MONO_FB_20MS_OUT_2_RMS,
                ),
            ]
        );

        let mut stereo_params = AudioCodecParameters::new();
        stereo_params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(
                Position::FRONT_LEFT | Position::FRONT_RIGHT,
            ));

        // Stereo 10 ms
        assert_stereo_f32_packets!(
            &stereo_params,
            480,
            [
                (
                    CELT_STEREO_FB_10MS_PKT_0,
                    CELT_STEREO_FB_10MS_OUT_L_0_HEAD_F32,
                    CELT_STEREO_FB_10MS_OUT_L_0_TAIL_F32,
                    CELT_STEREO_FB_10MS_OUT_L_0_RMS,
                    CELT_STEREO_FB_10MS_OUT_R_0_HEAD_F32,
                    CELT_STEREO_FB_10MS_OUT_R_0_TAIL_F32,
                    CELT_STEREO_FB_10MS_OUT_R_0_RMS,
                ),
                (
                    CELT_STEREO_FB_10MS_PKT_1,
                    CELT_STEREO_FB_10MS_OUT_L_1_HEAD_F32,
                    CELT_STEREO_FB_10MS_OUT_L_1_TAIL_F32,
                    CELT_STEREO_FB_10MS_OUT_L_1_RMS,
                    CELT_STEREO_FB_10MS_OUT_R_1_HEAD_F32,
                    CELT_STEREO_FB_10MS_OUT_R_1_TAIL_F32,
                    CELT_STEREO_FB_10MS_OUT_R_1_RMS,
                ),
            ]
        );

        // Stereo 20 ms
        assert_stereo_f32_packets!(
            &stereo_params,
            960,
            [
                (
                    CELT_STEREO_FB_20MS_PKT_0,
                    CELT_STEREO_FB_20MS_OUT_L_0_HEAD_F32,
                    CELT_STEREO_FB_20MS_OUT_L_0_TAIL_F32,
                    CELT_STEREO_FB_20MS_OUT_L_0_RMS,
                    CELT_STEREO_FB_20MS_OUT_R_0_HEAD_F32,
                    CELT_STEREO_FB_20MS_OUT_R_0_TAIL_F32,
                    CELT_STEREO_FB_20MS_OUT_R_0_RMS,
                ),
                (
                    CELT_STEREO_FB_20MS_PKT_1,
                    CELT_STEREO_FB_20MS_OUT_L_1_HEAD_F32,
                    CELT_STEREO_FB_20MS_OUT_L_1_TAIL_F32,
                    CELT_STEREO_FB_20MS_OUT_L_1_RMS,
                    CELT_STEREO_FB_20MS_OUT_R_1_HEAD_F32,
                    CELT_STEREO_FB_20MS_OUT_R_1_TAIL_F32,
                    CELT_STEREO_FB_20MS_OUT_R_1_RMS,
                ),
            ]
        );
        return;
    }

    #[test]
    fn opus_decoder_mode_transitions_and_redundancy_match_libopus() {
        let mut mono_params = AudioCodecParameters::new();
        mono_params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(Position::FRONT_CENTER));

        assert_mono_f32_packets!(
            &mono_params,
            960,
            [
                (
                    TRANSITION_0_SILK_WB_PKT,
                    TRANSITION_0_SILK_WB_OUT_HEAD_F32,
                    TRANSITION_0_SILK_WB_OUT_TAIL_F32,
                    TRANSITION_0_SILK_WB_OUT_RMS,
                ),
                (
                    TRANSITION_1_HYBRID_FB_PKT,
                    TRANSITION_1_HYBRID_FB_OUT_HEAD_F32,
                    TRANSITION_1_HYBRID_FB_OUT_TAIL_F32,
                    TRANSITION_1_HYBRID_FB_OUT_RMS,
                ),
                (
                    TRANSITION_2_CELT_FB_PKT,
                    TRANSITION_2_CELT_FB_OUT_HEAD_F32,
                    TRANSITION_2_CELT_FB_OUT_TAIL_F32,
                    TRANSITION_2_CELT_FB_OUT_RMS,
                ),
                (
                    TRANSITION_3_SILK_WB_RETURN_PKT,
                    TRANSITION_3_SILK_WB_RETURN_OUT_HEAD_F32,
                    TRANSITION_3_SILK_WB_RETURN_OUT_TAIL_F32,
                    TRANSITION_3_SILK_WB_RETURN_OUT_RMS,
                ),
            ]
        );
        return;
    }

    #[test]
    fn opus_decoder_multistream_vorbis_family1_and_opus_head_match_libopus() {
        // 3-channel Vorbis Family 1 OpusHead:
        // "OpusHead" (8B) | version=1 (1B) | channels=3 (1B) | pre_skip=312 (2B LE)
        // | input_sample_rate=48000 (4B LE) | output_gain=0 (2B LE) | mapping_family=1 (1B)
        // | stream_count=2 (1B) | coupled_count=1 (1B) | channel_mapping=[0, 2, 1] (3B)
        let opus_head_3ch: [u8; 24] = [
            b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', 1, 3, 0x38, 0x01, 0x80, 0xbb, 0x00,
            0x00, 0x00, 0x00, 1, 2, 1, 0, 2, 1,
        ];

        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(
                Position::FRONT_LEFT | Position::FRONT_CENTER | Position::FRONT_RIGHT,
            ))
            .with_extra_data(Box::from(opus_head_3ch));

        let mut dec = OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();
        assert_eq!(dec.pre_skip(), 312);
        assert_eq!(dec.output_gain(), 0);

        for (pkt_bytes, ch0_head, ch0_tail, ch0_rms, ch1_head, ch1_tail, ch1_rms, ch2_head, ch2_tail, ch2_rms) in [
            (
                &MS_3CH_PKT_0[..],
                &MS_3CH_OUT_CH0_0_HEAD_F32,
                &MS_3CH_OUT_CH0_0_TAIL_F32,
                MS_3CH_OUT_CH0_0_RMS,
                &MS_3CH_OUT_CH1_0_HEAD_F32,
                &MS_3CH_OUT_CH1_0_TAIL_F32,
                MS_3CH_OUT_CH1_0_RMS,
                &MS_3CH_OUT_CH2_0_HEAD_F32,
                &MS_3CH_OUT_CH2_0_TAIL_F32,
                MS_3CH_OUT_CH2_0_RMS,
            ),
            (
                &MS_3CH_PKT_1[..],
                &MS_3CH_OUT_CH0_1_HEAD_F32,
                &MS_3CH_OUT_CH0_1_TAIL_F32,
                MS_3CH_OUT_CH0_1_RMS,
                &MS_3CH_OUT_CH1_1_HEAD_F32,
                &MS_3CH_OUT_CH1_1_TAIL_F32,
                MS_3CH_OUT_CH1_1_RMS,
                &MS_3CH_OUT_CH2_1_HEAD_F32,
                &MS_3CH_OUT_CH2_1_TAIL_F32,
                MS_3CH_OUT_CH2_1_RMS,
            ),
        ] {
            let pkt = PacketRef::new(0, Timestamp::from(0u32), Duration::from(960u64), pkt_bytes);
            dec.decode_ref(&pkt).unwrap();
            assert_eq!(dec.buffer.frames(), 960);
            let p0 = dec.buffer.plane(0).unwrap();
            let p1 = dec.buffer.plane(1).unwrap();
            let p2 = dec.buffer.plane(2).unwrap();
            assert_plane_matches_f32_oracle(p0, 960, ch0_head, ch0_tail, ch0_rms, 1e-4);
            assert_plane_matches_f32_oracle(p1, 960, ch1_head, ch1_tail, ch1_rms, 1e-4);
            assert_plane_matches_f32_oracle(p2, 960, ch2_head, ch2_tail, ch2_rms, 1e-4);
        }

        // Also verify non-zero output_gain (+256 Q7.8 = +1.0 dB) and muted channel mapping (255)
        let opus_head_gain_mute: [u8; 24] = [
            b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', 1, 3, 0x00, 0x00, 0x80, 0xbb, 0x00,
            0x00, 0x00, 0x01, 1, 2, 1, 0, 255, 1,
        ];
        let mut gain_params = AudioCodecParameters::new();
        gain_params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(
                Position::FRONT_LEFT | Position::FRONT_CENTER | Position::FRONT_RIGHT,
            ))
            .with_extra_data(Box::from(opus_head_gain_mute));
        let mut gain_dec =
            OpusDecoder::try_new(&gain_params, &AudioDecoderOptions::default()).unwrap();
        assert_eq!(gain_dec.output_gain(), 256);
        let pkt0 = PacketRef::new(
            0,
            Timestamp::from(0u32),
            Duration::from(960u64),
            &MS_3CH_PKT_0[..],
        );
        gain_dec.decode_ref(&pkt0).unwrap();
        let muted_plane = gain_dec.buffer.plane(1).unwrap();
        assert!(muted_plane.iter().all(|&s| s == 0.0));
        let expected_scale = 10.0_f32.powf(256.0 / (20.0 * 256.0));
        let p0_scaled = gain_dec.buffer.plane(0).unwrap();
        assert!((p0_scaled[960 - 1] - MS_3CH_OUT_CH0_0_TAIL_F32[23] * expected_scale).abs() < 1e-4);
        return;
    }

    #[test]
    fn opus_decoder_plc_and_dtx_across_all_modes() {
        let mut mono_params = AudioCodecParameters::new();
        mono_params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(Position::FRONT_CENTER));

        let mut dec = OpusDecoder::try_new(&mono_params, &AudioDecoderOptions::default()).unwrap();

        // 1. Decode active Hybrid 20 ms frame, then conceal lost packet, then 1-byte Code 0 DTX packet
        let active_hybrid = PacketRef::new(
            0,
            Timestamp::from(0u32),
            Duration::from(960u64),
            &HYBRID_MONO_FB_20MS_PKT_0[..],
        );
        dec.decode_ref(&active_hybrid).unwrap();
        assert_eq!(dec.buffer.frames(), 960);

        let empty_pkt = PacketRef::new(0, Timestamp::from(960u32), Duration::from(960u64), &[]);
        dec.decode_ref(&empty_pkt).unwrap();
        assert_eq!(dec.buffer.frames(), 960);
        assert!(dec.buffer.plane(0).unwrap().iter().all(|s| s.is_finite()));

        // 1-byte Code 0 DTX packet (Hybrid FB 20 ms TOC = 0x78)
        let dtx_code0 = PacketRef::new(
            0,
            Timestamp::from(1920u32),
            Duration::from(960u64),
            &[0x78],
        );
        dec.decode_ref(&dtx_code0).unwrap();
        assert_eq!(dec.buffer.frames(), 960);
        assert!(dec.buffer.plane(0).unwrap().iter().all(|s| s.is_finite()));

        // 2. Decode active CELT 10 ms frame, then conceal lost packet, then 1-byte Code 1 DTX packet (2 x 10 ms = 960 samples)
        let active_celt = PacketRef::new(
            0,
            Timestamp::from(2880u32),
            Duration::from(480u64),
            &CELT_MONO_FB_10MS_PKT_0[..],
        );
        dec.decode_ref(&active_celt).unwrap();
        assert_eq!(dec.buffer.frames(), 480);

        dec.decode_lost_packet().unwrap();
        assert_eq!(dec.buffer.frames(), 480);
        assert!(dec.buffer.plane(0).unwrap().iter().all(|s| s.is_finite()));

        // 1-byte Code 1 DTX packet (CELT FB 10 ms Code 1 TOC = 0xf1)
        let dtx_code1 = PacketRef::new(
            0,
            Timestamp::from(3840u32),
            Duration::from(960u64),
            &[0xf1],
        );
        dec.decode_ref(&dtx_code1).unwrap();
        assert_eq!(dec.buffer.frames(), 960);
        assert!(dec.buffer.plane(0).unwrap().iter().all(|s| s.is_finite()));
        return;
    }
}
