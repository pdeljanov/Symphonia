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

/// Starting CELT band index at 8 kHz for Hybrid and SILK modes (`17`, RFC 6716 Section 4.3).
const HYBRID_START_BAND: usize = celt::HYBRID_START_BAND;

/// Sample count at 48 kHz for the 2.5 ms MDCT overlap flush (`120`, RFC 6716 Section 4.4).
const FLUSH_SILENT_FRAME_SAMPLES_48K: usize = celt::SHORT_MDCT_SIZE;

/// Two-byte all-ones payload used to synthesize a silent 2.5 ms CELT frame when flushing
/// the Hybrid MDCT overlap tail into a subsequent SILK frame (`opus_decoder.c:588`).
const SILENT_CELT_FRAME: [u8; 2] = [0xFF, 0xFF];

/// Minimum remaining range-coder bit budget in SILK mode to signal a 5 ms redundant CELT frame.
const SILK_REDUNDANCY_MIN_BITS: i32 = 17;

/// Minimum remaining range-coder bit budget in Hybrid mode (`17 + 20 = 37` bits) to signal
/// a 5 ms redundant CELT frame (RFC 6716 Section 4.4).
const HYBRID_REDUNDANCY_MIN_BITS: i32 = 37;

/// Log-probability (`logp = 12`, $P = 2^{-12}$) for the Hybrid redundancy presence flag.
const HYBRID_REDUNDANCY_FLAG_LOGP: u32 = 12;

/// Log-probability (`logp = 1`, $P = 2^{-1}$) for the `celt_to_silk` transition direction flag.
const REDUNDANCY_DIRECTION_LOGP: u32 = 1;

/// Alphabet size (`256`) for the uniform uint encoding `redundancy_bytes - 2` in Hybrid mode.
const HYBRID_REDUNDANCY_LEN_RADIX: u32 = 256;

/// Minimum byte length (`2`) of a coded Hybrid redundant CELT frame.
const HYBRID_REDUNDANCY_MIN_BYTES: usize = 2;

/// Vorbis channel mapping value (`255`) indicating a silent output channel (RFC 7845 Section 5.1.1).
const UNMAPPED_CHANNEL_INDEX: u8 = 255;

/// Scale factor converting Q7.8 dB `output_gain` to a base-2 exponent (`1 / (256 * 20 * log10(2))`).
const OUTPUT_GAIN_DB_Q8_TO_EXP2: f64 = 6.48814081e-4_f64;

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
    return (std::f64::consts::LN_2 * (OUTPUT_GAIN_DB_Q8_TO_EXP2 * (output_gain as f64))).exp()
        as f32;
}

/// Zeroes the first `len` samples of `out_ch0` and `out_ch1` (when present).
#[inline]
fn clear_planar(out_ch0: &mut [f32], out_ch1: Option<&mut [f32]>, len: usize) {
    out_ch0[..len].fill(0.0);
    if let Some(ch1) = out_ch1 {
        ch1[..len].fill(0.0);
    }
    return;
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

/// Applies [`smooth_fade_into_second`] across one or two planar channels over `overlap` samples.
#[inline]
fn fade_planar_into_second(
    in1_ch0: &[f32],
    in1_ch1: &[f32],
    out_ch0: &mut [f32],
    out_ch1: Option<&mut [f32]>,
    overlap: usize,
    downsample: usize,
) {
    smooth_fade_into_second(in1_ch0, out_ch0, overlap, downsample);
    if let Some(ch1) = out_ch1 {
        smooth_fade_into_second(in1_ch1, ch1, overlap, downsample);
    }
    return;
}

/// Applies [`smooth_fade_from_first`] across one or two planar channels at the `f2_5` frame tail.
#[inline]
fn fade_planar_tail_from_first(
    out_ch0: &mut [f32],
    out_ch1: Option<&mut [f32]>,
    red_ch0: &[f32],
    red_ch1: &[f32],
    audiosize: usize,
    f2_5: usize,
    f5: usize,
    downsample: usize,
) {
    let tail_start = audiosize - f2_5;
    smooth_fade_from_first(
        &mut out_ch0[tail_start..audiosize],
        &red_ch0[f2_5..f5],
        f2_5,
        downsample,
    );
    if let Some(ch1) = out_ch1 {
        smooth_fade_from_first(
            &mut ch1[tail_start..audiosize],
            &red_ch1[f2_5..f5],
            f2_5,
            downsample,
        );
    }
    return;
}

/// Copies the leading `2.5 ms` (`f2_5`) prefix from `src_ch0`/`src_ch1` (when `audiosize >= f5`)
/// and cross-fades the subsequent `2.5 ms` (`f2_5`) overlap into `out_ch0`/`out_ch1`
/// (RFC 6716 Section 4.4 / `opus_decoder.c:616-662`).
#[inline]
fn apply_5ms_prefix_crossfade(
    src_ch0: &[f32; MAX_5MS_SAMPLES_48K],
    src_ch1: &[f32; MAX_5MS_SAMPLES_48K],
    out_ch0: &mut [f32],
    out_ch1: Option<&mut [f32]>,
    audiosize: usize,
    f2_5: usize,
    f5: usize,
    downsample: usize,
) {
    match audiosize >= f5 {
        true => {
            out_ch0[..f2_5].copy_from_slice(&src_ch0[..f2_5]);
            let ch1_window = out_ch1.map(|ch1| {
                ch1[..f2_5].copy_from_slice(&src_ch1[..f2_5]);
                return &mut ch1[f2_5..f5];
            });
            fade_planar_into_second(
                &src_ch0[f2_5..f5],
                &src_ch1[f2_5..f5],
                &mut out_ch0[f2_5..f5],
                ch1_window,
                f2_5,
                downsample,
            );
        }
        false => {
            fade_planar_into_second(
                &src_ch0[..f2_5],
                &src_ch1[..f2_5],
                &mut out_ch0[..f2_5],
                out_ch1.map(|ch1| &mut ch1[..f2_5]),
                f2_5,
                downsample,
            );
        }
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

impl RedundancyHeader {
    /// Returns a default header when no redundant 5 ms CELT frame is present.
    #[inline]
    const fn none(active_len: usize) -> Self {
        return Self {
            active_len,
            redundancy: false,
            celt_to_silk: false,
            redundancy_bytes: 0,
        };
    }
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
        return match self.output_channels {
            2 => Channels::Positioned(Position::FRONT_LEFT | Position::FRONT_RIGHT),
            _ => Channels::Positioned(Position::FRONT_LEFT),
        };
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

        clear_planar(out_ch0, out_ch1.as_deref_mut(), audiosize);

        let Some(prev_mode) = self.prev_mode else {
            self.range_final = 0;
            return Ok(audiosize);
        };

        let bandwidth = self.last_bandwidth;
        let packet_stereo = self.last_stereo;
        let silk_channels = self.silk_api_channels();

        match (prev_mode, audiosize < f10) {
            (AudioMode::SILK | AudioMode::Hybrid, true) => {
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
            }
            (AudioMode::SILK | AudioMode::Hybrid, false) => {
                self.silk_decoder
                    .configure(silk_channels, frame_size, bandwidth)?;
                self.silk_decoder.conceal_frame_planar(
                    packet_stereo,
                    &mut out_ch0[..audiosize],
                    out_ch1.as_deref_mut().map(|s| &mut s[..audiosize]),
                )?;
            }
            (AudioMode::CELT, _) => {}
        }

        let stream_channels = if packet_stereo { 2 } else { 1 };
        let start_band = match prev_mode {
            AudioMode::CELT => 0,
            AudioMode::SILK | AudioMode::Hybrid => HYBRID_START_BAND,
        };
        self.celt_decoder.set_bandwidth(bandwidth);
        self.celt_decoder.set_stream_channels(stream_channels)?;
        self.celt_decoder.set_start_band(start_band)?;

        match prev_mode {
            AudioMode::CELT | AudioMode::Hybrid => {
                self.celt_decoder.conceal_frame_planar(
                    frame_size_48k.min(MAX_20MS_SAMPLES_48K),
                    &mut out_ch0[..audiosize],
                    out_ch1.map(|s| &mut s[..audiosize]),
                    prev_mode == AudioMode::Hybrid,
                )?;
            }
            AudioMode::SILK => {}
        }

        self.range_final = 0;
        self.prev_redundancy = false;
        return Ok(audiosize);
    }

    /// Conceals a lost or zero-length DTX frame of arbitrary duration (`2.5 ms`..`60 ms`)
    /// by splitting durations $> 20\text{ ms}$ into 20 ms concealment steps.
    fn decode_lost_or_dtx_frame(
        &mut self,
        frame_size: FrameDuration,
        audiosize: usize,
        f20: usize,
        out_ch0: &mut [f32],
        mut out_ch1: Option<&mut [f32]>,
    ) -> Result<usize> {
        match (self.prev_mode.is_some(), audiosize > f20) {
            (false, _) => {
                clear_planar(out_ch0, out_ch1, audiosize);
                self.range_final = 0;
                return Ok(audiosize);
            }
            (true, true) => {
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
            (true, false) => {
                return self.conceal_up_to_20ms(frame_size, out_ch0, out_ch1);
            }
        }
    }

    /// Conceals a `2.5 ms` or `5 ms` transition tail from the outgoing decoder into
    /// `pcm_transition_ch0` and `pcm_transition_ch1` when switching modes without redundancy.
    fn conceal_transition_tail(
        &mut self,
        audiosize: usize,
        f2_5: usize,
        f5: usize,
        pcm_transition_ch0: &mut [f32; MAX_5MS_SAMPLES_48K],
        pcm_transition_ch1: &mut [f32; MAX_5MS_SAMPLES_48K],
    ) -> Result<()> {
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
        return Ok(());
    }

    /// Parses the optional 5 ms CELT redundancy header from a SILK or Hybrid frame
    /// (RFC 6716 Section 4.4 / `opus_decoder.c:500-528`).
    fn parse_redundancy_header(
        mode: AudioMode,
        range_decoder: &mut entropy::Decoder<'_>,
        raw_len: usize,
    ) -> Result<RedundancyHeader> {
        let min_bits = match mode {
            AudioMode::CELT => return Ok(RedundancyHeader::none(raw_len)),
            AudioMode::SILK => SILK_REDUNDANCY_MIN_BITS,
            AudioMode::Hybrid => HYBRID_REDUNDANCY_MIN_BITS,
        };

        if range_decoder.tell() + min_bits > (raw_len as i32) * 8 {
            return Ok(RedundancyHeader::none(raw_len));
        }

        let redundancy = match mode {
            AudioMode::Hybrid => range_decoder.decode_symbol_logp(HYBRID_REDUNDANCY_FLAG_LOGP)? != 0,
            AudioMode::SILK | AudioMode::CELT => true,
        };
        if !redundancy {
            return Ok(RedundancyHeader::none(raw_len));
        }

        let celt_to_silk = range_decoder.decode_symbol_logp(REDUNDANCY_DIRECTION_LOGP)? != 0;
        let redundancy_bytes = match mode {
            AudioMode::Hybrid => {
                (range_decoder.decode_uint(HYBRID_REDUNDANCY_LEN_RADIX)? as usize)
                    + HYBRID_REDUNDANCY_MIN_BYTES
            }
            AudioMode::SILK | AudioMode::CELT => {
                raw_len.saturating_sub(((range_decoder.tell() + 7) >> 3) as usize)
            }
        };

        let active_len = raw_len.saturating_sub(redundancy_bytes);
        if (active_len as i32) * 8 < range_decoder.tell() {
            return Ok(RedundancyHeader::none(0));
        }

        range_decoder.shrink_storage(redundancy_bytes);
        return Ok(RedundancyHeader {
            active_len,
            redundancy: true,
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

    /// Decodes the SILK low-frequency layer for `AudioMode::SILK` and `AudioMode::Hybrid`.
    fn decode_silk_layer(
        &mut self,
        range_decoder: &mut entropy::Decoder<'_>,
        frame_size: FrameDuration,
        bandwidth: Bandwidth,
        packet_stereo: bool,
        audiosize: usize,
        out_ch0: &mut [f32],
        out_ch1: Option<&mut [f32]>,
    ) -> Result<()> {
        if self.prev_mode == Some(AudioMode::CELT) {
            self.silk_decoder.reset();
        }
        let silk_channels = self.silk_api_channels();
        self.silk_decoder
            .configure(silk_channels, frame_size, bandwidth)?;
        self.silk_decoder.decode_frame_planar(
            range_decoder,
            packet_stereo,
            &mut out_ch0[..audiosize],
            out_ch1.map(|s| &mut s[..audiosize]),
        )?;
        return Ok(());
    }

    /// Decodes the active CELT layer for `AudioMode::CELT` (`start_band = 0`) or
    /// `AudioMode::Hybrid` (`start_band = 17`, accumulated onto the SILK output).
    fn decode_active_celt_layer(
        &mut self,
        mode: AudioMode,
        raw_bytes: &[u8],
        active_len: usize,
        range_decoder: &mut entropy::Decoder<'_>,
        frame_size_48k: usize,
        audiosize: usize,
        out_ch0: &mut [f32],
        out_ch1: Option<&mut [f32]>,
    ) -> Result<u32> {
        let celt_frame_size_48k = frame_size_48k.min(MAX_20MS_SAMPLES_48K);
        let accumulate = mode == AudioMode::Hybrid;

        if self.prev_mode.is_some_and(|prev| prev != mode) && !self.prev_redundancy {
            self.celt_decoder.reset();
        }

        match active_len {
            0 => {
                self.celt_decoder.conceal_frame_planar(
                    celt_frame_size_48k,
                    &mut out_ch0[..audiosize],
                    out_ch1.map(|s| &mut s[..audiosize]),
                    accumulate,
                )?;
            }
            _ => {
                self.celt_decoder.decode_frame_planar(
                    Some(&raw_bytes[..active_len]),
                    Some(range_decoder),
                    celt_frame_size_48k,
                    &mut out_ch0[..audiosize],
                    out_ch1.map(|s| &mut s[..audiosize]),
                    accumulate,
                )?;
            }
        }

        return Ok(self.celt_decoder.final_range());
    }

    /// Flushes the 2.5 ms (`120` samples at 48 kHz) CELT MDCT overlap tail into the start of a
    /// `AudioMode::SILK` frame when transitioning from `AudioMode::Hybrid` (`opus_decoder.c:581-594`).
    fn flush_hybrid_celt_overlap_tail(
        &mut self,
        range_decoder: &entropy::Decoder<'_>,
        redundancy: bool,
        celt_to_silk: bool,
        f2_5: usize,
        out_ch0: &mut [f32],
        out_ch1: Option<&mut [f32]>,
    ) -> Result<u32> {
        let should_flush = self.prev_mode == Some(AudioMode::Hybrid)
            && !(redundancy && celt_to_silk && self.prev_redundancy);

        if should_flush {
            self.celt_decoder.set_start_band(0)?;
            self.celt_decoder.decode_frame_planar(
                Some(&SILENT_CELT_FRAME),
                None,
                FLUSH_SILENT_FRAME_SAMPLES_48K,
                &mut out_ch0[..f2_5],
                out_ch1.map(|s| &mut s[..f2_5]),
                true,
            )?;
        }

        return Ok(range_decoder.final_range());
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

        let Some(raw_bytes) = frame_data.filter(|b| !b.is_empty()) else {
            return self.decode_lost_or_dtx_frame(frame_size, audiosize, f20, out_ch0, out_ch1);
        };

        clear_planar(out_ch0, out_ch1.as_deref_mut(), audiosize);

        let mut transition = match (self.prev_mode, mode) {
            (Some(prev), AudioMode::CELT) => prev != AudioMode::CELT && !self.prev_redundancy,
            (Some(AudioMode::CELT), AudioMode::SILK | AudioMode::Hybrid) => true,
            _ => false,
        };

        let mut pcm_transition_ch0 = [0.0_f32; MAX_5MS_SAMPLES_48K];
        let mut pcm_transition_ch1 = [0.0_f32; MAX_5MS_SAMPLES_48K];

        if transition && mode == AudioMode::CELT {
            self.conceal_transition_tail(
                audiosize,
                f2_5,
                f5,
                &mut pcm_transition_ch0,
                &mut pcm_transition_ch1,
            )?;
        }

        let mut range_decoder = entropy::Decoder::new(raw_bytes)?;

        // 1. SILK Layer Decode (SILK-only and Hybrid modes).
        match mode {
            AudioMode::SILK | AudioMode::Hybrid => {
                self.decode_silk_layer(
                    &mut range_decoder,
                    frame_size,
                    bandwidth,
                    packet_stereo,
                    audiosize,
                    out_ch0,
                    out_ch1.as_deref_mut(),
                )?;
            }
            AudioMode::CELT => {}
        }

        // 2. Redundancy Frame Header Check (RFC 6716 Section 4.4 / opus_decoder.c:500-528).
        let RedundancyHeader {
            active_len,
            redundancy,
            celt_to_silk,
            redundancy_bytes,
        } = Self::parse_redundancy_header(mode, &mut range_decoder, raw_bytes.len())?;

        if redundancy {
            transition = false;
        }

        // 3. Conceal 5 ms CELT transition tail when switching from CELT to SILK/Hybrid without redundancy.
        if transition && mode != AudioMode::CELT {
            self.conceal_transition_tail(
                audiosize,
                f2_5,
                f5,
                &mut pcm_transition_ch0,
                &mut pcm_transition_ch1,
            )?;
        }

        // 4. Configure CELT bandwidth and coded channel count.
        let stream_channels = if packet_stereo { 2 } else { 1 };
        self.celt_decoder.set_bandwidth(bandwidth);
        self.celt_decoder.set_stream_channels(stream_channels)?;

        let mut redundant_ch0 = [0.0_f32; MAX_5MS_SAMPLES_48K];
        let mut redundant_ch1 = [0.0_f32; MAX_5MS_SAMPLES_48K];
        let mut redundant_rng = 0u32;

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
        let start_band = match mode {
            AudioMode::CELT => 0,
            AudioMode::SILK | AudioMode::Hybrid => HYBRID_START_BAND,
        };
        self.celt_decoder.set_start_band(start_band)?;

        // 7. Decode main CELT frame (Hybrid or CELT-only) or flush CELT overlap tail on Hybrid -> SILK.
        self.range_final = match mode {
            AudioMode::CELT | AudioMode::Hybrid => self.decode_active_celt_layer(
                mode,
                raw_bytes,
                active_len,
                &mut range_decoder,
                frame_size_48k,
                audiosize,
                out_ch0,
                out_ch1.as_deref_mut(),
            )?,
            AudioMode::SILK => self.flush_hybrid_celt_overlap_tail(
                &range_decoder,
                redundancy,
                celt_to_silk,
                f2_5,
                out_ch0,
                out_ch1.as_deref_mut(),
            )?,
        };

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
            fade_planar_tail_from_first(
                out_ch0,
                out_ch1.as_deref_mut(),
                &redundant_ch0,
                &redundant_ch1,
                audiosize,
                f2_5,
                f5,
                downsample,
            );
        }

        // 9. Apply CELT -> SILK/Hybrid redundancy cross-fade at start of frame.
        if redundancy
            && celt_to_silk
            && (self.prev_mode != Some(AudioMode::SILK) || self.prev_redundancy)
        {
            apply_5ms_prefix_crossfade(
                &redundant_ch0,
                &redundant_ch1,
                out_ch0,
                out_ch1.as_deref_mut(),
                audiosize,
                f2_5,
                f5,
                downsample,
            );
        }

        // 10. Apply un-redundant mode transition cross-fade at start of frame.
        if transition {
            apply_5ms_prefix_crossfade(
                &pcm_transition_ch0,
                &pcm_transition_ch1,
                out_ch0,
                out_ch1,
                audiosize,
                f2_5,
                f5,
                downsample,
            );
        }

        self.range_final = match active_len <= 1 {
            true => 0,
            false => self.range_final ^ redundant_rng,
        };

        self.prev_mode = Some(mode);
        self.prev_redundancy = redundancy && !celt_to_silk;
        self.last_bandwidth = bandwidth;
        self.last_frame_size = frame_size;
        self.last_stereo = packet_stereo;

        return Ok(audiosize);
    }
}

/// Parsed multi-stream layout and `OpusHead` metadata for [`OpusDecoder`].
struct StreamLayout {
    streams: Vec<StreamDecoder>,
    coupled_count: usize,
    channel_mapping: Vec<u8>,
    pre_skip: u16,
    output_gain: i16,
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
    /// Builds a single-stream RTP (Family 0) [`StreamLayout`] for `1` or `2` output channels.
    fn build_rtp_stream_layout(
        sample_rate: u32,
        num_output_channels: usize,
        pre_skip: u16,
        output_gain: i16,
    ) -> Result<StreamLayout> {
        let channel_mapping = match num_output_channels {
            1 => vec![0],
            2 => vec![0, 1],
            _ => {
                return Err(SymphoniaError::DecodeError(
                    "opus: RTP mapping family 0 only supports 1 or 2 channels",
                ));
            }
        };

        return Ok(StreamLayout {
            streams: vec![StreamDecoder::new(sample_rate, num_output_channels)?],
            coupled_count: usize::from(num_output_channels == 2),
            channel_mapping,
            pre_skip,
            output_gain,
        });
    }

    /// Parses the [`StreamLayout`] from `params.extra_data` (`OpusHead`) or defaults to Family 0.
    fn parse_stream_layout(
        params: &AudioCodecParameters,
        sample_rate: u32,
        num_output_channels: usize,
    ) -> Result<StreamLayout> {
        let Some(extra) = params.extra_data.as_deref().filter(|b| !b.is_empty()) else {
            if num_output_channels > 2 {
                return Err(SilkError::UnsupportedConfig.into());
            }
            return Self::build_rtp_stream_layout(sample_rate, num_output_channels, 0, 0);
        };

        let id = header::ID::parse(BufReader::new(extra))
            .map_err(|_| SymphoniaError::DecodeError("opus: invalid OpusHead extra_data"))?;

        if id.channel_count as usize != num_output_channels {
            return Err(SymphoniaError::DecodeError(
                "opus: OpusHead channel count does not match AudioCodecParameters",
            ));
        }

        match id.mapping_family {
            ChannelMappingFamily::Rtp => {
                return Self::build_rtp_stream_layout(
                    sample_rate,
                    num_output_channels,
                    id.pre_skip,
                    id.output_gain,
                );
            }
            ChannelMappingFamily::Vorbis => {
                let table = id.channel_mapping_table.ok_or(SymphoniaError::DecodeError(
                    "opus: missing channel mapping table for Vorbis family 1",
                ))?;
                let stream_count = table.stream_count as usize;
                let coupled_count = table.coupled_count as usize;
                let streams = (0..stream_count)
                    .map(|i| {
                        let stream_out_channels = if i < coupled_count { 2 } else { 1 };
                        return StreamDecoder::new(sample_rate, stream_out_channels);
                    })
                    .collect::<Result<Vec<_>>>()?;

                return Ok(StreamLayout {
                    streams,
                    coupled_count,
                    channel_mapping: table.channel_mapping,
                    pre_skip: id.pre_skip,
                    output_gain: id.output_gain,
                });
            }
            ChannelMappingFamily::Reserved(_) | ChannelMappingFamily::Undefined => {
                return unsupported_error("opus: unsupported channel mapping family");
            }
        }
    }

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

        let StreamLayout {
            streams,
            coupled_count,
            channel_mapping,
            pre_skip,
            output_gain,
        } = Self::parse_stream_layout(params, sample_rate, num_output_channels)?;

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
            let map_val = self
                .channel_mapping
                .get(ch)
                .copied()
                .unwrap_or(UNMAPPED_CHANNEL_INDEX);
            if map_val == UNMAPPED_CHANNEL_INDEX {
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
            let map_val = self
                .channel_mapping
                .get(ch)
                .copied()
                .unwrap_or(UNMAPPED_CHANNEL_INDEX) as usize;
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
                match apply_gain {
                    true => {
                        dst_slice
                            .iter_mut()
                            .zip(src_slice.iter())
                            .for_each(|(d, &s)| *d = s * scale);
                    }
                    false => dst_slice.copy_from_slice(src_slice),
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
            let pkt = match i + 1 < num_streams {
                true => {
                    let (parsed, consumed) = FramePacket::parse_self_delimited(cursor)?;
                    cursor = &cursor[consumed..];
                    parsed
                }
                false => FramePacket::new(cursor)?,
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
    use crate::test_vectors::*;
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
