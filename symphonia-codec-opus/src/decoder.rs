//! Opus top-level and multi-stream decoder orchestration
//! ([RFC 6716 Section 4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4>),
//! [RFC 7845 Section 5](<https://datatracker.ietf.org/doc/html/rfc7845#section-5>)).
//!
//! The Opus decoder consists of two synthesis blocks per elementary stream: the SILK
//! linear-predictive decoder (the low frequencies of SILK-only and Hybrid frames) and the CELT
//! transform decoder (the high frequencies of Hybrid frames or the full spectrum of CELT-only
//! frames). The output of each elementary stream is the sum of the time-aligned outputs of the
//! two decoders
//! ([RFC 6716 Section 4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4>) block diagram):
//!
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
//! [`StreamDecoder`] sequences the layers of one frame, conceals lost frames
//! ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)) and
//! applies the configuration-switching rules of
//! [RFC 6716 Section 4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5>): the
//! 5 ms redundant CELT frame (`Redundancy`), the recommended cross-fade with concealed audio
//! when a mode switch carries no redundancy (`Transition`), the Hybrid to SILK-only overlap
//! flush, and the state resets of
//! [Section 4.5.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.2>).
//!
//! A multi-stream packet
//! ([RFC 7845 Section 3](<https://datatracker.ietf.org/doc/html/rfc7845#section-3>)) carries
//! `N - 1` self-delimited elementary stream packets
//! ([RFC 6716 Appendix B](<https://datatracker.ietf.org/doc/html/rfc6716#appendix-B>)) followed
//! by one undelimited packet. [`OpusDecoder`] decodes the `M` coupled stereo streams and the
//! `N - M` uncoupled mono streams with one [`StreamDecoder`] each and routes their channels to
//! the output planes through the `OpusHead` channel mapping table
//! ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)),
//! scaled by the Q7.8 dB `output_gain`
//! ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)).

use std::sync::LazyLock;

use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef,
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
use crate::celt::BlockSize;
use crate::celt::constant::CELT_WINDOW;
use crate::celt::decoder::{FrameInput, FrameLayout, Mix};
use crate::entropy;
use crate::header::{self, ChannelMappingFamily};
use crate::packet::FramePacket;
use crate::planes::Planes;
use crate::sample_rate::SampleRate;
use crate::silk;
use crate::toc::{AudioMode, Bandwidth, FrameDuration};

/// Maximum samples per channel in one Opus packet: 120 ms at 48 kHz (`5760`,
/// [RFC 6716 Section 3.2.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.5>)).
const MAX_PACKET_SAMPLES_48K: usize = 5760;

/// Samples per channel in the longest single SILK or CELT frame: 20 ms at 48 kHz (`960`).
const MAX_20MS_SAMPLES_48K: usize = 960;

/// Samples per channel in the 5 ms redundant CELT frame and transition window at 48 kHz
/// (`240`, [RFC 6716 Section 4.5.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1>)).
const MAX_5MS_SAMPLES_48K: usize = 240;

/// First CELT band coded in Hybrid mode: band 17, starting at 8 kHz
/// ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)).
const HYBRID_START_BAND: usize = celt::HYBRID_START_BAND;

/// Samples at 48 kHz of the 2.5 ms silence frame that flushes the CELT overlap buffer into a
/// SILK-only frame following a Hybrid frame (`120`,
/// [RFC 6716 Section 4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5>)).
const OVERLAP_FLUSH_SAMPLES_48K: usize = celt::SHORT_MDCT_SIZE;

/// Bits that must remain after the SILK layer of a SILK-only frame for it to carry redundancy
/// ([RFC 6716 Section 4.5.1.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.1>)).
const SILK_REDUNDANCY_MIN_BITS: i32 = 17;

/// Bits that must remain after the SILK layer of a Hybrid frame for the redundancy flag to be
/// coded
/// ([RFC 6716 Section 4.5.1.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.1>)).
const HYBRID_REDUNDANCY_MIN_BITS: i32 = 37;

/// Log-probability of the Hybrid redundancy flag, PDF `{4095, 1}/4096`
/// ([RFC 6716 Table 64](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.1>)).
const HYBRID_REDUNDANCY_FLAG_LOGP: u32 = 12;

/// Log-probability of the redundancy position flag, PDF `{1, 1}/2`
/// ([RFC 6716 Table 65](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.2>)).
const REDUNDANCY_POSITION_LOGP: u32 = 1;

/// Alphabet size of the Hybrid redundancy size integer (`256`,
/// [RFC 6716 Section 4.5.1.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.3>)).
const HYBRID_REDUNDANCY_LEN_RADIX: u32 = 256;

/// Bytes added to the decoded Hybrid redundancy size integer (`2`,
/// [RFC 6716 Section 4.5.1.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.3>)).
const HYBRID_REDUNDANCY_MIN_BYTES: usize = 2;

/// Bits per byte, in the `i32` domain of [`entropy::Decoder::tell`].
const BITS_PER_BYTE: i32 = u8::BITS as i32;

/// Right shift that divides a bit count by [`BITS_PER_BYTE`].
const BYTE_SHIFT: u32 = 3;

/// Channel mapping entry that marks a silent output channel (`255`,
/// [RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)).
const UNMAPPED_CHANNEL_INDEX: u8 = 255;

/// Largest output channel count an `OpusHead` can describe: the channel count is one byte
/// ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)).
const MAX_OUTPUT_CHANNELS: usize = u8::MAX as usize;

/// Units of the Q7.8 `output_gain` field per decibel
/// ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)).
const OUTPUT_GAIN_UNITS_PER_DB: f64 = 256.0;

/// Decibels per decade of amplitude: a gain of `g` dB scales samples by `10^(g / 20)`.
const DB_PER_AMPLITUDE_DECADE: f64 = 20.0;

/// Static Opus codec info.
static CODEC_INFO: LazyLock<CodecInfo> = LazyLock::new(|| CodecInfo {
    short_name: "opus",
    long_name: "Opus Audio Codec",
    profiles: &[],
});

/// Static supported codecs array.
static SUPPORTED_CODECS: LazyLock<[SupportedAudioCodec; 1]> =
    LazyLock::new(|| [SupportedAudioCodec { id: CODEC_ID_OPUS, info: *CODEC_INFO }]);

/// Linear scale of the Q7.8 dB `output_gain` field, `10^(output_gain / (20 * 256))`, the formula
/// RFC 7845 gives for applying the gain
/// ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)).
#[inline]
fn output_gain_scale(output_gain: i16) -> f32 {
    let gain_db = f64::from(output_gain) / OUTPUT_GAIN_UNITS_PER_DB;
    return 10.0_f64.powf(gain_db / DB_PER_AMPLITUDE_DECADE) as f32;
}

/// Which operand of a 5 ms cross-lap the rising CELT window favours
/// ([RFC 6716 Section 4.5.1.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.4>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FadeDirection {
    /// The buffer fades in while the other signal fades out (start of a frame).
    In,
    /// The buffer fades out while the other signal fades in (end of a frame).
    Out,
}

/// Cross-laps `out` with `other` over their common length using the squared CELT window
/// `w[i] = CELT_WINDOW[i * downsample]^2`, whose power-complementary halves sum to one
/// ([RFC 6716 Section 4.5.1.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.4>)):
///
/// ```text
///   FadeDirection::In   out[i] = w[i] * out[i]   + (1 - w[i]) * other[i]
///   FadeDirection::Out  out[i] = w[i] * other[i] + (1 - w[i]) * out[i]
/// ```
fn crossfade(out: &mut [f32], other: &[f32], direction: FadeDirection, downsample: usize) {
    let weights = CELT_WINDOW.iter().step_by(downsample).map(|w| w * w);
    out.iter_mut().zip(other).zip(weights).for_each(|((sample, &other_sample), w)| {
        let (fading_out, fading_in) = match direction {
            FadeDirection::In => (other_sample, *sample),
            FadeDirection::Out => (*sample, other_sample),
        };
        *sample = w * fading_in + (1.0_f32 - w) * fading_out;
    });
    return;
}

/// Applies [`crossfade`] to every present plane of `out` against the matching row of `other`.
fn crossfade_planes(
    mut out: Planes<'_>,
    other: [&[f32]; 2],
    direction: FadeDirection,
    downsample: usize,
) {
    out.each_mut().zip(other).for_each(|(plane, other_plane)| {
        crossfade(plane, other_plane, direction, downsample);
    });
    return;
}

/// Whether `mode` carries a SILK layer: SILK-only and Hybrid
/// ([RFC 6716 Section 2](<https://datatracker.ietf.org/doc/html/rfc6716#section-2>)).
const fn has_silk_layer(mode: AudioMode) -> bool {
    return matches!(mode, AudioMode::Silk | AudioMode::Hybrid);
}

/// Whether `mode` carries a CELT layer: CELT-only and Hybrid
/// ([RFC 6716 Section 2](<https://datatracker.ietf.org/doc/html/rfc6716#section-2>)).
const fn has_celt_layer(mode: AudioMode) -> bool {
    return matches!(mode, AudioMode::Celt | AudioMode::Hybrid);
}

/// First CELT band decoded or concealed for `mode`: band 0 in CELT-only mode and
/// [`HYBRID_START_BAND`] otherwise
/// ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)). A
/// SILK-only frame keeps the Hybrid start band for its overlap flush and concealment.
const fn celt_start_band(mode: AudioMode) -> usize {
    return match mode {
        AudioMode::Celt => 0,
        AudioMode::Silk | AudioMode::Hybrid => HYBRID_START_BAND,
    };
}

/// How the CELT output is mixed into the planes: added onto the SILK output of a Hybrid frame,
/// written directly otherwise
/// ([RFC 6716 Section 4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4>) block diagram).
const fn celt_mix(mode: AudioMode) -> Mix {
    return match mode {
        AudioMode::Hybrid => Mix::Accumulate,
        AudioMode::Silk | AudioMode::Celt => Mix::Overwrite,
    };
}

/// Channel count a frame codes, from its TOC stereo flag
/// ([RFC 6716 Section 3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>)).
const fn coded_channels(stereo: bool) -> usize {
    return if stereo { 2 } else { 1 };
}

/// Bit count of `bytes` whole bytes, in the `i32` domain of [`entropy::Decoder::tell`].
const fn bits_in(bytes: usize) -> i32 {
    return (bytes as i32) * BITS_PER_BYTE;
}

/// Whole bytes that hold `bits` bits, rounded up to the next byte boundary.
const fn whole_bytes(bits: i32) -> usize {
    return ((bits + (BITS_PER_BYTE - 1)) >> BYTE_SHIFT) as usize;
}

/// TOC configuration shared by every frame of a packet
/// ([RFC 6716 Section 3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.1>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameConfig {
    mode: AudioMode,
    bandwidth: Bandwidth,
    duration: FrameDuration,
    stereo: bool,
}

impl From<&FramePacket<'_>> for FrameConfig {
    fn from(packet: &FramePacket<'_>) -> Self {
        return Self {
            mode: packet.audio_mode,
            bandwidth: packet.bandwidth,
            duration: packet.frame_duration,
            stereo: packet.stereo,
        };
    }
}

/// The last decoded frame, remembered to select the concealment decoders
/// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)) and to
/// apply the configuration-switching rules
/// ([RFC 6716 Section 4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5>)) to the
/// next frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LastFrame {
    config: FrameConfig,
    /// The frame carried a [`Redundancy::SilkToCelt`] frame, so the CELT state is already
    /// primed for a following CELT-only frame
    /// ([RFC 6716 Section 4.5.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.2>)).
    silk_to_celt_redundancy: bool,
}

/// The 5 ms redundant CELT frame a SILK-only or Hybrid frame may carry
/// ([RFC 6716 Section 4.5.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1>)),
/// placed by the position flag of
/// [Section 4.5.1.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.2>).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Redundancy<'a> {
    /// No redundant frame.
    None,
    /// Position flag `1`: this is the second frame of a CELT-only to SILK-only or Hybrid
    /// transition; the redundant frame replaces and cross-laps the start of this frame.
    CeltToSilk(&'a [u8]),
    /// Position flag `0`: this is the first frame of a SILK-only or Hybrid to CELT-only
    /// transition; the redundant frame cross-laps the end of this frame.
    SilkToCelt(&'a [u8]),
}

impl Redundancy<'_> {
    /// Whether a redundant frame is present.
    const fn is_present(&self) -> bool {
        return !matches!(self, Self::None);
    }
}

/// An Opus frame split into the bytes its SILK and CELT layers decode and the redundant CELT
/// frame at its end
/// ([RFC 6716 Section 4.5.1.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.3>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FramePayload<'a> {
    /// Bytes available to the SILK and CELT layers; the range decoder storage is shrunk to them.
    active: &'a [u8],
    redundancy: Redundancy<'a>,
}

impl<'a> FramePayload<'a> {
    /// The whole frame is active.
    const fn without_redundancy(bytes: &'a [u8]) -> Self {
        return Self { active: bytes, redundancy: Redundancy::None };
    }

    /// Reads the redundancy flag, position flag and size once the SILK layer has been decoded
    /// from `range_decoder`
    /// ([RFC 6716 Section 4.5.1.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.1>)
    /// to [4.5.1.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.3>)) and shrinks
    /// the range decoder storage by the redundant frame. A CELT-only frame never carries
    /// redundancy. A size that leaves fewer active bytes than the range decoder has already
    /// consumed invalidates the frame: nothing stays active and the CELT layer is concealed.
    fn split(
        mode: AudioMode,
        range_decoder: &mut entropy::Decoder<'_>,
        bytes: &'a [u8],
    ) -> Result<Self> {
        let min_bits = match mode {
            AudioMode::Celt => return Ok(Self::without_redundancy(bytes)),
            AudioMode::Silk => SILK_REDUNDANCY_MIN_BITS,
            AudioMode::Hybrid => HYBRID_REDUNDANCY_MIN_BITS,
        };
        if range_decoder.tell() + min_bits > bits_in(bytes.len()) {
            return Ok(Self::without_redundancy(bytes));
        }

        // A Hybrid frame codes the flag and the size; a SILK-only frame implies both.
        let explicit = mode == AudioMode::Hybrid;
        if explicit && range_decoder.decode_symbol_logp(HYBRID_REDUNDANCY_FLAG_LOGP)? == 0 {
            return Ok(Self::without_redundancy(bytes));
        }
        let at_start = range_decoder.decode_symbol_logp(REDUNDANCY_POSITION_LOGP)? != 0;
        let redundant_len = if explicit {
            (range_decoder.decode_uint(HYBRID_REDUNDANCY_LEN_RADIX)? as usize)
                + HYBRID_REDUNDANCY_MIN_BYTES
        } else {
            bytes.len().saturating_sub(whole_bytes(range_decoder.tell()))
        };

        let active_len = bytes.len().saturating_sub(redundant_len);
        if bits_in(active_len) < range_decoder.tell() {
            return Ok(Self::without_redundancy(&bytes[..0]));
        }
        range_decoder.shrink_storage(redundant_len);

        let active = &bytes[..active_len];
        let redundant = &bytes[active_len..(active_len + redundant_len)];
        let redundancy = if at_start {
            Redundancy::CeltToSilk(redundant)
        } else {
            Redundancy::SilkToCelt(redundant)
        };
        return Ok(Self { active, redundancy });
    }
}

/// Decoder-side handling of a mode switch that carries no redundancy
/// ([RFC 6716 Section 4.5.3, Figure 19](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.3>)):
/// 5 ms of audio concealed from the outgoing decoder is cross-lapped with the start of the new
/// frame. Switches between SILK-only and Hybrid, and within one mode, need no cross-fade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transition {
    /// No cross-fade.
    None,
    /// SILK-only or Hybrid to CELT-only: conceal from the outgoing state before the frame is
    /// decoded.
    IntoCelt,
    /// CELT-only to SILK-only or Hybrid: conceal from the CELT state once the SILK layer has
    /// been decoded.
    OutOfCelt,
}

impl Transition {
    /// Classifies the switch from `last` to a frame in `mode`. A switch into CELT-only that the
    /// previous frame announced with a [`Redundancy::SilkToCelt`] frame needs no concealment
    /// ([RFC 6716 Section 4.5.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.2>)).
    fn between(last: Option<LastFrame>, mode: AudioMode) -> Self {
        let Some(last) = last else {
            return Self::None;
        };
        return match (last.config.mode, mode) {
            (AudioMode::Silk | AudioMode::Hybrid, AudioMode::Celt) => {
                if last.silk_to_celt_redundancy { Self::None } else { Self::IntoCelt }
            }
            (AudioMode::Celt, AudioMode::Silk | AudioMode::Hybrid) => Self::OutOfCelt,
            (AudioMode::Celt, AudioMode::Celt)
            | (AudioMode::Silk | AudioMode::Hybrid, AudioMode::Silk | AudioMode::Hybrid) => {
                Self::None
            }
        };
    }

    /// A redundant frame makes the transition normative and forbids concealment
    /// ([RFC 6716 Section 4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5>):
    /// "PLC MUST NOT be applied during any normative transition").
    const fn unless_redundant(self, redundancy: &Redundancy<'_>) -> Self {
        if redundancy.is_present() {
            return Self::None;
        }
        return self;
    }
}

/// Elementary Opus stream decoder pairing a SILK decoder and a CELT decoder for a mono
/// (`output_channels == 1`) or coupled stereo (`output_channels == 2`) stream
/// ([RFC 6716 Section 4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4>)).
pub(crate) struct StreamDecoder {
    output_channels: usize,
    sample_rate: SampleRate,
    silk_decoder: silk::Decoder,
    celt_decoder: celt::Decoder,
    /// `None` until the first frame has been decoded; a loss before that produces silence.
    last: Option<LastFrame>,
    range_final: u32,
}

impl StreamDecoder {
    /// Creates a new [`StreamDecoder`] for `sample_rate` and `output_channels` (`1` or `2`).
    pub(crate) fn new(sample_rate: SampleRate, output_channels: usize) -> Result<Self> {
        let mut silk_decoder = silk::Decoder::new();
        silk_decoder.set_output_sample_rate(sample_rate.hz());
        let celt_decoder = celt::Decoder::new(sample_rate, output_channels)?;

        return Ok(Self {
            output_channels,
            sample_rate,
            silk_decoder,
            celt_decoder,
            last: None,
            range_final: 0,
        });
    }

    /// Resets the elementary stream decoder state across stream discontinuities.
    pub(crate) fn reset(&mut self) {
        self.silk_decoder.reset();
        self.celt_decoder.reset();
        self.last = None;
        self.range_final = 0;
        return;
    }

    /// Whether the stream decodes into two output planes.
    #[inline]
    fn is_stereo(&self) -> bool {
        return self.output_channels == 2;
    }

    /// Decodes every frame of `packet` back to back into the `scratch` rows and returns the
    /// samples written per channel.
    fn decode_packet(
        &mut self,
        packet: &FramePacket<'_>,
        scratch: &mut [[f32; MAX_PACKET_SAMPLES_48K]; 2],
    ) -> Result<usize> {
        let config = FrameConfig::from(packet);
        let samples_per_frame = self.sample_rate.samples_per(config.duration);
        let mut pcm = Planes::from_rows(scratch, self.is_stereo());

        return packet.frames.iter().try_fold(0usize, |offset, &frame| {
            let written = self.decode_frame(
                Some(frame),
                config,
                pcm.range(offset..(offset + samples_per_frame)),
            )?;
            return Ok(offset + written);
        });
    }

    /// Conceals a lost packet of `duration` into the `scratch` rows
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    fn conceal_lost_packet(
        &mut self,
        duration: FrameDuration,
        scratch: &mut [[f32; MAX_PACKET_SAMPLES_48K]; 2],
    ) -> Result<usize> {
        let samples = self.sample_rate.samples_per(duration);
        let mut pcm = Planes::from_rows(scratch, self.is_stereo());
        return self.conceal_frame(duration, pcm.range(..samples));
    }

    /// Conceals a lost or DTX frame of `duration`
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    /// Durations above 20 ms, the longest frame the SILK and CELT concealment produce, are
    /// concealed in 20 ms steps.
    fn conceal_frame(&mut self, duration: FrameDuration, mut out: Planes<'_>) -> Result<usize> {
        let samples = self.sample_rate.samples_per(duration);
        let f20 = self.sample_rate.samples_per(FrameDuration::Ms20);
        if samples <= f20 {
            return self.conceal_up_to_20ms(duration, out);
        }
        for offset in (0..samples).step_by(f20) {
            let step = f20.min(samples - offset);
            self.conceal_up_to_20ms(FrameDuration::Ms20, out.range(offset..(offset + step)))?;
        }
        return Ok(samples);
    }

    /// Conceals one lost frame of at most 20 ms with the decoders of the last frame
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)):
    /// LPC extrapolation in the SILK layer, pitch repetition or noise in the CELT layer, both
    /// for a Hybrid frame. Before the first decoded frame the output is silence.
    fn conceal_up_to_20ms(
        &mut self,
        duration: FrameDuration,
        mut out: Planes<'_>,
    ) -> Result<usize> {
        let samples = self.sample_rate.samples_per(duration);
        let mut out = out.range(..samples);
        out.fill(0.0);
        self.range_final = 0;

        let Some(last) = self.last.as_mut() else {
            return Ok(samples);
        };
        // A redundant frame bridges two consecutive decoded frames (RFC 6716 Section 4.5.1):
        // a loss in between ends the SILK-to-CELT hand-over it announced.
        last.silk_to_celt_redundancy = false;
        let config = last.config;

        if has_silk_layer(config.mode) {
            self.conceal_silk(config, duration, out.reborrow())?;
        }
        if has_celt_layer(config.mode) {
            self.conceal_celt(config, duration, out)?;
        }
        return Ok(samples);
    }

    /// Conceals `duration` of SILK output with the last frame's SILK configuration. SILK's
    /// shortest frame is 10 ms
    /// ([RFC 6716 Section 4.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.2>)),
    /// so a shorter loss is concealed as a 10 ms frame and truncated.
    fn conceal_silk(
        &mut self,
        config: FrameConfig,
        duration: FrameDuration,
        mut out: Planes<'_>,
    ) -> Result<()> {
        let samples = self.sample_rate.samples_per(duration);
        let f10 = self.sample_rate.samples_per(FrameDuration::Ms10);

        if samples >= f10 {
            self.silk_decoder.configure(
                coded_channels(config.stereo),
                duration,
                config.bandwidth,
            )?;
            self.silk_decoder.conceal_frame_planar(config.stereo, out)?;
            return Ok(());
        }

        let mut full = [[0.0_f32; MAX_20MS_SAMPLES_48K]; 2];
        let mut full_planes = Planes::from_rows(&mut full, self.is_stereo());
        self.silk_decoder.configure(
            coded_channels(config.stereo),
            FrameDuration::Ms10,
            config.bandwidth,
        )?;
        self.silk_decoder.conceal_frame_planar(config.stereo, full_planes.range(..f10))?;
        out.each_mut().zip(&full).for_each(|(plane, full_plane)| {
            plane.copy_from_slice(&full_plane[..samples]);
        });
        return Ok(());
    }

    /// Conceals `duration` of CELT output
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)),
    /// summed onto the SILK concealment when the last frame was Hybrid.
    fn conceal_celt(
        &mut self,
        config: FrameConfig,
        duration: FrameDuration,
        out: Planes<'_>,
    ) -> Result<()> {
        let layout = self.celt_layout(config, duration)?;
        self.celt_decoder.decode_frame(FrameInput::Lost, layout, out, celt_mix(config.mode))?;
        return Ok(());
    }

    /// Layout of the CELT layer of a frame of `config` and `duration`: the frame's block size
    /// (at most 20 ms), its start band and the TOC bandwidth and channel count
    /// ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)).
    fn celt_layout(&self, config: FrameConfig, duration: FrameDuration) -> Result<FrameLayout> {
        let frame_size_48k = SampleRate::Hz48000.samples_per(duration).min(MAX_20MS_SAMPLES_48K);
        return Ok(FrameLayout {
            block: BlockSize::from_frame_size_48k(frame_size_48k)?,
            start_band: celt_start_band(config.mode),
            bandwidth: config.bandwidth,
            stream_channels: coded_channels(config.stereo),
        });
    }

    /// Conceals the first 5 ms of the frame (or all of a 2.5 ms frame) from the outgoing
    /// decoders into `concealed`: the `P` of
    /// [RFC 6716 Section 4.5.3, Figure 19](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.3>).
    fn conceal_transition_tail(
        &mut self,
        samples: usize,
        concealed: &mut [[f32; MAX_5MS_SAMPLES_48K]; 2],
    ) -> Result<()> {
        let f2_5 = self.sample_rate.samples_per(FrameDuration::Ms2_5);
        let f5 = self.sample_rate.samples_per(FrameDuration::Ms5);
        let duration = if samples == f2_5 { FrameDuration::Ms2_5 } else { FrameDuration::Ms5 };
        let mut out = Planes::from_rows(concealed, self.is_stereo());
        self.conceal_up_to_20ms(duration, out.range(..f5.min(samples)))?;
        return Ok(());
    }

    /// Decodes one Opus frame of `config`, or conceals it when `frame` is absent, into `out`
    /// and returns the samples written per channel
    /// ([RFC 6716 Section 4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4>)). The
    /// SILK layer, the redundancy side information, the CELT layer and the redundant frame are
    /// decoded in bitstream order, then the cross-laps of
    /// [Section 4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5>) are applied.
    fn decode_frame(
        &mut self,
        frame: Option<&[u8]>,
        config: FrameConfig,
        mut out: Planes<'_>,
    ) -> Result<usize> {
        let samples = self.sample_rate.samples_per(config.duration);
        // RFC 6716 Appendix A: a frame of at most one byte has no payload and is concealed.
        let Some(bytes) = frame.filter(|b| b.len() > 1) else {
            return self.conceal_frame(config.duration, out);
        };
        let mut out = out.range(..samples);
        out.fill(0.0);

        let transition = Transition::between(self.last, config.mode);
        let mut concealed = [[0.0_f32; MAX_5MS_SAMPLES_48K]; 2];
        if transition == Transition::IntoCelt {
            self.conceal_transition_tail(samples, &mut concealed)?;
        }

        let mut range_decoder = entropy::Decoder::new(bytes);
        self.decode_silk_layer(&mut range_decoder, config, out.reborrow())?;
        let payload = FramePayload::split(config.mode, &mut range_decoder, bytes)?;
        let transition = transition.unless_redundant(&payload.redundancy);
        if transition == Transition::OutOfCelt {
            self.conceal_transition_tail(samples, &mut concealed)?;
        }

        let mut redundant = [[0.0_f32; MAX_5MS_SAMPLES_48K]; 2];
        let (layer_range, redundant_range) = self.decode_celt_layers(
            &mut range_decoder,
            &payload,
            config,
            &mut redundant,
            out.reborrow(),
        )?;
        self.crossfade_leading_redundancy(&payload.redundancy, &redundant, out.reborrow(), samples);
        if transition != Transition::None {
            self.crossfade_prefix(&concealed, out, samples);
        }

        // RFC 6716 Section 6: the final range state is part of conformance. A frame left
        // without a decodable payload reports zero, like a lost frame.
        self.range_final =
            if payload.active.len() <= 1 { 0 } else { layer_range ^ redundant_range };
        self.last = Some(LastFrame {
            config,
            silk_to_celt_redundancy: matches!(payload.redundancy, Redundancy::SilkToCelt(_)),
        });
        return Ok(samples);
    }

    /// Decodes the SILK layer of a SILK-only or Hybrid frame
    /// ([RFC 6716 Section 4.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2>))
    /// into `out`; a CELT-only frame has none. The SILK state is reset first when the previous
    /// frame was CELT-only
    /// ([RFC 6716 Section 4.5.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.2>)).
    fn decode_silk_layer(
        &mut self,
        range_decoder: &mut entropy::Decoder<'_>,
        config: FrameConfig,
        out: Planes<'_>,
    ) -> Result<()> {
        if !has_silk_layer(config.mode) {
            return Ok(());
        }
        if self.last.is_some_and(|last| last.config.mode == AudioMode::Celt) {
            self.silk_decoder.reset();
        }
        self.silk_decoder.configure(
            coded_channels(config.stereo),
            config.duration,
            config.bandwidth,
        )?;
        self.silk_decoder.decode_frame_planar(range_decoder, config.stereo, out)?;
        return Ok(());
    }

    /// Decodes the CELT side of the frame in bitstream order: a leading redundant frame
    /// ([`Redundancy::CeltToSilk`]), the frame's own CELT layer or overlap flush, then a
    /// trailing redundant frame ([`Redundancy::SilkToCelt`]) cross-lapped with the frame's end
    /// ([RFC 6716 Section 4.5.1.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.4>)).
    /// Returns the range coder states of the layer and of the redundant frame (`0` without one).
    fn decode_celt_layers(
        &mut self,
        range_decoder: &mut entropy::Decoder<'_>,
        payload: &FramePayload<'_>,
        config: FrameConfig,
        redundant: &mut [[f32; MAX_5MS_SAMPLES_48K]; 2],
        mut out: Planes<'_>,
    ) -> Result<(u32, u32)> {
        let leading_range = match payload.redundancy {
            Redundancy::CeltToSilk(frame) => {
                self.decode_redundant_frame(frame, config, redundant)?
            }
            Redundancy::None | Redundancy::SilkToCelt(_) => 0,
        };

        let layer_range = match config.mode {
            AudioMode::Celt | AudioMode::Hybrid => self.decode_active_celt_layer(
                range_decoder,
                payload.active,
                config,
                out.reborrow(),
            )?,
            AudioMode::Silk => self.flush_hybrid_celt_overlap(
                range_decoder,
                &payload.redundancy,
                config,
                out.reborrow(),
            )?,
        };

        let redundant_range = match payload.redundancy {
            Redundancy::SilkToCelt(frame) => {
                self.decode_trailing_redundancy(frame, config, redundant, out)?
            }
            Redundancy::None | Redundancy::CeltToSilk(_) => leading_range,
        };
        return Ok((layer_range, redundant_range));
    }

    /// Decodes the 5 ms redundant CELT frame `frame` into the `redundant` rows as a CELT-only
    /// frame without a TOC byte, with the channel count and bandwidth of the current frame
    /// ([RFC 6716 Section 4.5.1.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.4>)).
    /// Returns its range coder state.
    fn decode_redundant_frame(
        &mut self,
        frame: &[u8],
        config: FrameConfig,
        redundant: &mut [[f32; MAX_5MS_SAMPLES_48K]; 2],
    ) -> Result<u32> {
        let f5 = self.sample_rate.samples_per(FrameDuration::Ms5);
        let layout = FrameLayout {
            block: BlockSize::from_frame_size_48k(MAX_5MS_SAMPLES_48K)?,
            start_band: 0,
            bandwidth: config.bandwidth,
            stream_channels: coded_channels(config.stereo),
        };
        let mut out = Planes::from_rows(redundant, self.is_stereo());
        self.celt_decoder.decode_frame(
            FrameInput::new(Some(frame), None),
            layout,
            out.range(..f5),
            Mix::Overwrite,
        )?;
        return Ok(self.celt_decoder.final_range());
    }

    /// Decodes the CELT layer of a CELT-only frame, or of a Hybrid frame summed onto the SILK
    /// output ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)),
    /// from the shared range decoder. The CELT state is reset on a mode change unless the
    /// previous frame's trailing redundancy already primed it
    /// ([RFC 6716 Section 4.5.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.2>)).
    /// A frame left without active bytes by an invalid redundancy size is concealed. Returns
    /// the range coder state after the layer.
    fn decode_active_celt_layer(
        &mut self,
        range_decoder: &mut entropy::Decoder<'_>,
        active: &[u8],
        config: FrameConfig,
        out: Planes<'_>,
    ) -> Result<u32> {
        if self
            .last
            .is_some_and(|last| last.config.mode != config.mode && !last.silk_to_celt_redundancy)
        {
            self.celt_decoder.reset();
        }

        let layout = self.celt_layout(config, config.duration)?;
        let input = FrameInput::new(Some(active), Some(range_decoder));
        self.celt_decoder.decode_frame(input, layout, out, celt_mix(config.mode))?;
        return Ok(self.celt_decoder.final_range());
    }

    /// Adds the CELT overlap buffer left by a preceding Hybrid frame to the first 2.5 ms of a
    /// SILK-only frame by decoding a 2.5 ms silence frame
    /// ([RFC 6716 Section 4.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5>),
    /// "Hybrid to WB SILK"). Skipped when the frame's leading redundancy continues the previous
    /// frame's trailing redundancy
    /// ([Figure 18](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.3>), "Hybrid to
    /// NB or MB SILK with Redundancy"): the redundant frames carry the hand-over. Returns the
    /// range coder state of the frame, which the flush leaves untouched.
    fn flush_hybrid_celt_overlap(
        &mut self,
        range_decoder: &entropy::Decoder<'_>,
        redundancy: &Redundancy<'_>,
        config: FrameConfig,
        mut out: Planes<'_>,
    ) -> Result<u32> {
        let f2_5 = self.sample_rate.samples_per(FrameDuration::Ms2_5);
        let after_hybrid = self.last.is_some_and(|last| last.config.mode == AudioMode::Hybrid);
        let redundant_handover = matches!(redundancy, Redundancy::CeltToSilk(_))
            && self.last.is_some_and(|last| last.silk_to_celt_redundancy);

        if after_hybrid && !redundant_handover {
            let layout = FrameLayout {
                block: BlockSize::from_frame_size_48k(OVERLAP_FLUSH_SAMPLES_48K)?,
                start_band: 0,
                bandwidth: config.bandwidth,
                stream_channels: coded_channels(config.stereo),
            };
            self.celt_decoder.decode_frame(
                FrameInput::Silence,
                layout,
                out.range(..f2_5),
                Mix::Accumulate,
            )?;
        }
        return Ok(range_decoder.final_range());
    }

    /// Decodes the redundant frame at the end of a SILK-only or Hybrid frame after resetting the
    /// CELT state
    /// ([RFC 6716 Section 4.5.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.2>))
    /// and cross-laps its second half with the last 2.5 ms of the frame
    /// ([Section 4.5.1.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.4>)).
    /// Returns the redundant frame's range coder state.
    fn decode_trailing_redundancy(
        &mut self,
        frame: &[u8],
        config: FrameConfig,
        redundant: &mut [[f32; MAX_5MS_SAMPLES_48K]; 2],
        mut out: Planes<'_>,
    ) -> Result<u32> {
        self.celt_decoder.reset();
        let range = self.decode_redundant_frame(frame, config, redundant)?;

        let f2_5 = self.sample_rate.samples_per(FrameDuration::Ms2_5);
        let f5 = self.sample_rate.samples_per(FrameDuration::Ms5);
        let samples = out.left.len();
        crossfade_planes(
            out.range((samples - f2_5)..samples),
            [&redundant[0][f2_5..f5], &redundant[1][f2_5..f5]],
            FadeDirection::Out,
            self.sample_rate.downsample_from_48k(),
        );
        return Ok(range);
    }

    /// Mixes a leading redundant frame into the start of the frame
    /// ([RFC 6716 Section 4.5.1.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.4>)).
    /// After a plain SILK-only frame the redundant frame is decoded but left out:
    /// [Figure 18](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.3>) only mixes it
    /// when it follows a CELT-only frame or the trailing redundancy of the previous frame.
    fn crossfade_leading_redundancy(
        &self,
        redundancy: &Redundancy<'_>,
        redundant: &[[f32; MAX_5MS_SAMPLES_48K]; 2],
        out: Planes<'_>,
        samples: usize,
    ) {
        let Redundancy::CeltToSilk(_) = redundancy else {
            return;
        };
        let bridges_celt = self
            .last
            .is_none_or(|last| last.config.mode != AudioMode::Silk || last.silk_to_celt_redundancy);
        if bridges_celt {
            self.crossfade_prefix(redundant, out, samples);
        }
        return;
    }

    /// Replaces the first 2.5 ms of `out` with `src` and cross-laps the next 2.5 ms, or
    /// cross-laps the whole of a 2.5 ms frame
    /// ([RFC 6716 Section 4.5.1.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.4>)).
    /// The same window blends concealed audio into a transition without redundancy
    /// ([Section 4.5.3, Figure 19](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.3>)).
    fn crossfade_prefix(
        &self,
        src: &[[f32; MAX_5MS_SAMPLES_48K]; 2],
        mut out: Planes<'_>,
        samples: usize,
    ) {
        let f2_5 = self.sample_rate.samples_per(FrameDuration::Ms2_5);
        let f5 = self.sample_rate.samples_per(FrameDuration::Ms5);
        let downsample = self.sample_rate.downsample_from_48k();

        if samples < f5 {
            crossfade_planes(
                out.range(..f2_5),
                [&src[0][..f2_5], &src[1][..f2_5]],
                FadeDirection::In,
                downsample,
            );
            return;
        }
        out.each_mut().zip(src).for_each(|(plane, src_plane)| {
            plane[..f2_5].copy_from_slice(&src_plane[..f2_5]);
        });
        crossfade_planes(
            out.range(f2_5..f5),
            [&src[0][f2_5..f5], &src[1][f2_5..f5]],
            FadeDirection::In,
            downsample,
        );
        return;
    }
}

/// The elementary stream packets of a multi-stream Opus packet, in stream order: `count - 1`
/// self-delimited packets
/// ([RFC 6716 Appendix B](<https://datatracker.ietf.org/doc/html/rfc6716#appendix-B>)) followed
/// by one undelimited packet that runs to the end of the data
/// ([RFC 7845 Section 3](<https://datatracker.ietf.org/doc/html/rfc7845#section-3>)).
struct StreamPackets<'a> {
    data: &'a [u8],
    remaining: usize,
}

impl<'a> StreamPackets<'a> {
    /// Splits `data` into `count` elementary stream packets.
    const fn new(data: &'a [u8], count: usize) -> Self {
        return Self { data, remaining: count };
    }
}

impl<'a> Iterator for StreamPackets<'a> {
    type Item = Result<FramePacket<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        self.remaining = self.remaining.checked_sub(1)?;
        if self.remaining == 0 {
            return Some(FramePacket::new(self.data).map_err(SymphoniaError::from));
        }
        let parsed = FramePacket::parse_self_delimited(self.data).map_err(SymphoniaError::from);
        return Some(parsed.map(|(packet, consumed)| {
            self.data = &self.data[consumed..];
            return packet;
        }));
    }
}

/// Where each decoded stream channel goes
/// ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)):
/// coupled stream `i` feeds the output channels whose mapping entry is `2i` (left) or `2i + 1`
/// (right); uncoupled stream `i` feeds the entries equal to `coupled_count + i`. Samples are
/// scaled by the output gain on the way
/// ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)).
#[derive(Clone, Copy)]
struct OutputRouting<'a> {
    channel_mapping: &'a [u8],
    coupled_count: usize,
    /// Linear output gain, `None` when the `output_gain` field is zero.
    gain: Option<f32>,
}

impl OutputRouting<'_> {
    /// Copies `total_samples` of stream `stream_i` from the `scratch` rows into its output planes.
    fn route(
        &self,
        stream_i: usize,
        total_samples: usize,
        scratch: &[[f32; MAX_PACKET_SAMPLES_48K]; 2],
        buffer: &mut AudioBuffer<f32>,
    ) -> Result<()> {
        let (first, count) = if stream_i < self.coupled_count {
            (2 * stream_i, 2)
        } else {
            (self.coupled_count + stream_i, 1)
        };

        for (ch, &map) in self.channel_mapping.iter().enumerate() {
            let Some(k) = usize::from(map).checked_sub(first).filter(|k| *k < count) else {
                continue;
            };
            let src = &scratch[k][..total_samples];
            let dst = buffer
                .plane_mut(ch)
                .ok_or(SymphoniaError::DecodeError("opus: missing output plane"))?;
            let dst = &mut dst[..total_samples];
            match self.gain {
                Some(scale) => dst.iter_mut().zip(src).for_each(|(d, &s)| *d = s * scale),
                None => dst.copy_from_slice(src),
            }
        }
        return Ok(());
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
/// It owns the top-level codec parameters, the multi-stream `StreamDecoder` instances, the
/// `OpusHead` channel mapping and gain state, and the output [`AudioBuffer<f32>`].
pub struct OpusDecoder {
    params: AudioCodecParameters,
    sample_rate: SampleRate,
    buffer: AudioBuffer<f32>,
    streams: Vec<StreamDecoder>,
    scratch: [[f32; MAX_PACKET_SAMPLES_48K]; 2],
    coupled_count: usize,
    channel_mapping: Vec<u8>,
    pre_skip: u16,
    output_gain: i16,
    /// Linear output gain, `None` when `output_gain` is zero.
    gain: Option<f32>,
    range_final: u32,
}

impl OpusDecoder {
    /// Builds a single-stream RTP (Family 0) [`StreamLayout`] for `1` or `2` output channels
    /// ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)).
    fn build_rtp_stream_layout(
        sample_rate: SampleRate,
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

    /// Parses the [`StreamLayout`] from `params.extra_data` (`OpusHead`,
    /// [RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)) or
    /// defaults to Family 0.
    fn parse_stream_layout(
        params: &AudioCodecParameters,
        sample_rate: SampleRate,
        num_output_channels: usize,
    ) -> Result<StreamLayout> {
        let Some(extra) = params.extra_data.as_deref().filter(|b| !b.is_empty()) else {
            if num_output_channels > 2 {
                return unsupported_error("opus: more than 2 channels need an OpusHead mapping");
            }
            return Self::build_rtp_stream_layout(sample_rate, num_output_channels, 0, 0);
        };

        let id = header::IdentificationHeader::parse(BufReader::new(extra))
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
                    .map(|i| StreamDecoder::new(sample_rate, coded_channels(i < coupled_count)))
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

        let sample_rate = SampleRate::try_from(
            params.sample_rate.ok_or(SymphoniaError::Unsupported("opus: missing sample rate"))?,
        )?;
        let channels = params
            .channels
            .clone()
            .ok_or(SymphoniaError::Unsupported("opus: missing channel layout"))?;
        let num_output_channels = channels.count();
        if num_output_channels == 0 || num_output_channels > MAX_OUTPUT_CHANNELS {
            return unsupported_error("opus: unsupported output channel count");
        }

        let StreamLayout { streams, coupled_count, channel_mapping, pre_skip, output_gain } =
            Self::parse_stream_layout(params, sample_rate, num_output_channels)?;

        // One packet carries at most 120 ms (RFC 6716 Section 3.2.5).
        let spec = AudioSpec::new(sample_rate.hz(), channels);
        let buffer =
            AudioBuffer::new(spec, MAX_PACKET_SAMPLES_48K / sample_rate.downsample_from_48k());

        return Ok(Self {
            params: params.to_owned(),
            sample_rate,
            buffer,
            streams,
            scratch: [[0.0; MAX_PACKET_SAMPLES_48K]; 2],
            coupled_count,
            channel_mapping,
            pre_skip,
            output_gain,
            gain: (output_gain != 0).then(|| output_gain_scale(output_gain)),
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

    /// Returns the final range decoder state of the last decoded packet, XOR-combined across
    /// elementary streams
    /// ([RFC 6716 Section 6](<https://datatracker.ietf.org/doc/html/rfc6716#section-6>)); zero
    /// after a lost or concealed packet.
    #[inline]
    pub fn final_range(&self) -> u32 {
        return self.range_final;
    }

    /// Conceals a lost packet across all elementary streams with the duration of the last
    /// decoded frame
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)) and
    /// renders the result into the output buffer.
    pub fn decode_lost_packet(&mut self) -> Result<GenericAudioBufferRef<'_>> {
        let duration = self
            .streams
            .first()
            .and_then(|stream| stream.last)
            .map_or(FrameDuration::Ms20, |last| last.config.duration);
        let total_samples = self.sample_rate.samples_per(duration);
        self.prepare_output_buffer(total_samples)?;

        let routing = OutputRouting {
            channel_mapping: &self.channel_mapping,
            coupled_count: self.coupled_count,
            gain: self.gain,
        };
        for (i, stream) in self.streams.iter_mut().enumerate() {
            stream.conceal_lost_packet(duration, &mut self.scratch)?;
            routing.route(i, total_samples, &self.scratch, &mut self.buffer)?;
        }

        self.range_final = 0;
        return Ok(self.buffer.as_generic_audio_buffer_ref());
    }

    /// Clears the output buffer, renders `total_samples` frames and silences the output
    /// channels whose mapping entry is [`UNMAPPED_CHANNEL_INDEX`]
    /// ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)).
    fn prepare_output_buffer(&mut self, total_samples: usize) -> Result<()> {
        self.buffer.clear();
        self.buffer.render_uninit(Some(total_samples));
        let unmapped = self
            .channel_mapping
            .iter()
            .enumerate()
            .filter(|&(_, &map)| map == UNMAPPED_CHANNEL_INDEX)
            .map(|(ch, _)| ch);
        for ch in unmapped {
            let plane = self
                .buffer
                .plane_mut(ch)
                .ok_or(SymphoniaError::DecodeError("opus: missing output plane"))?;
            plane[..total_samples].fill(0.0);
        }
        return Ok(());
    }
}

impl AudioDecoder for OpusDecoder {
    fn reset(&mut self) {
        self.streams.iter_mut().for_each(StreamDecoder::reset);
        self.scratch.iter_mut().for_each(|row| row.fill(0.0));
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

        let mut packets = StreamPackets::new(packet.data, self.streams.len());
        let first = packets
            .next()
            .ok_or(SymphoniaError::DecodeError("opus: decoder has no elementary streams"))??;
        // RFC 7845 Section 3: every stream packet of a multi-stream packet has the duration of
        // the first one; any other is treated as a malformed TOC sequence.
        let shape = (first.frames.len(), first.frame_duration);
        let total_samples = shape.0 * self.sample_rate.samples_per(shape.1);
        self.prepare_output_buffer(total_samples)?;

        let routing = OutputRouting {
            channel_mapping: &self.channel_mapping,
            coupled_count: self.coupled_count,
            gain: self.gain,
        };
        let packets = std::iter::once(Ok(first)).chain(packets);
        let mut combined_range = 0u32;
        for (i, (stream, stream_packet)) in self.streams.iter_mut().zip(packets).enumerate() {
            let stream_packet = stream_packet?;
            if (stream_packet.frames.len(), stream_packet.frame_duration) != shape {
                self.buffer.clear();
                return Err(SymphoniaError::DecodeError(
                    "opus: inconsistent frame count or frame duration across elementary streams",
                ));
            }
            stream.decode_packet(&stream_packet, &mut self.scratch)?;
            combined_range ^= stream.range_final;
            routing.route(i, total_samples, &self.scratch, &mut self.buffer)?;
        }

        self.range_final = combined_range;
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
    // Fixtures: packets and expected values recorded from libopus 1.6.1. Decoding of received
    // frames is normative (RFC 6716 Section 4) and is verified against the RFC 8251 test
    // vectors by `tests/rfc6716_conformance.rs`; the concealment steps are regression pins for
    // behaviour that RFC 6716 Section 4.4 leaves to the implementation.
    use super::*;
    use crate::test_vectors::*;
    use symphonia_core::audio::{Audio, Channels, Position};
    use symphonia_core::codecs::audio::well_known::CODEC_ID_VORBIS;
    use symphonia_core::units::{Duration, Timestamp};

    /// The 48 kHz codec parameters of an output with the channels `positions`.
    fn params(positions: Position) -> AudioCodecParameters {
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(positions));
        return params;
    }

    fn mono_params() -> AudioCodecParameters {
        return params(Position::FRONT_CENTER);
    }

    fn stereo_params() -> AudioCodecParameters {
        return params(Position::FRONT_LEFT | Position::FRONT_RIGHT);
    }

    fn decoder(params: &AudioCodecParameters) -> OpusDecoder {
        return OpusDecoder::try_new(params, &AudioDecoderOptions::default()).unwrap();
    }

    /// Decodes `packet` as one packet of `samples` samples per channel.
    fn decode(dec: &mut OpusDecoder, packet: &[u8], samples: usize) {
        let pkt = PacketRef::new(0, Timestamp::from(0u32), Duration::from(samples as u64), packet);
        dec.decode_ref(&pkt).unwrap();
        return;
    }

    fn quantize_i16_24(slice: &[f32]) -> [i16; 24] {
        return std::array::from_fn(|i| (slice[i] * 32768.0).round() as i16);
    }

    /// Checks an output plane of `samples` samples against its recorded head, tail and RMS.
    fn assert_plane_matches_recorded(
        plane: &[f32],
        samples: usize,
        recorded: &RecordedPlane,
        tol: f32,
    ) {
        assert_eq!(plane.len(), samples);
        for (i, (&actual, &exp)) in plane[..24].iter().zip(recorded.head.iter()).enumerate() {
            let diff = (actual - exp).abs();
            assert!(
                diff <= tol,
                "head sample {i} mismatch: actual={actual}, expected={exp}, diff={diff}, tol={tol}"
            );
        }
        for (i, (&actual, &exp)) in
            plane[samples - 24..].iter().zip(recorded.tail.iter()).enumerate()
        {
            let diff = (actual - exp).abs();
            assert!(
                diff <= tol,
                "tail sample {i} mismatch: actual={actual}, expected={exp}, diff={diff}, tol={tol}"
            );
        }
        let rms = (plane.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>() / (samples as f64))
            .sqrt() as f32;
        let expected_rms = recorded.rms;
        let rms_diff = (rms - expected_rms).abs();
        assert!(
            rms_diff <= tol,
            "rms mismatch: actual={rms}, expected={expected_rms}, diff={rms_diff}, tol={tol}"
        );
        return;
    }

    /// Decodes every packet of `stream` with `dec` and checks every output plane against its
    /// recorded head, tail and RMS.
    fn assert_stream_decodes<const C: usize, const P: usize>(
        dec: &mut OpusDecoder,
        stream: &RecordedStream<RecordedPacket<C>, P>,
    ) {
        for recorded in &stream.packets {
            decode(dec, recorded.packet, stream.samples);
            for (c, plane) in recorded.planes.iter().enumerate() {
                let output = dec.buffer.plane(c).unwrap();
                assert_plane_matches_recorded(output, stream.samples, plane, 1e-4);
            }
        }
        return;
    }

    /// Decodes every packet of `stream` with `dec` and checks the 16-bit tail, and the head when
    /// recorded, of every output plane.
    fn assert_pcm_stream_decodes<const C: usize, const P: usize>(
        dec: &mut OpusDecoder,
        stream: &RecordedStream<RecordedPcmPacket<C>, P>,
    ) {
        let samples = stream.samples;
        for recorded in &stream.packets {
            decode(dec, recorded.packet, samples);
            assert_eq!(dec.buffer.frames(), samples);
            for (c, &tail) in recorded.tails.iter().enumerate() {
                let plane = dec.buffer.plane(c).unwrap();
                assert_eq!(&quantize_i16_24(&plane[samples - 24..]), tail);
                if let Some(heads) = recorded.heads {
                    assert_eq!(&quantize_i16_24(&plane[..24]), heads[c]);
                }
            }
        }
        return;
    }

    #[test]
    fn opus_decoder_initialization() {
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(Position::FRONT_LEFT | Position::FRONT_RIGHT));

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
    fn silk_mono_and_stereo_packets_decode_to_recorded_values() {
        assert_pcm_stream_decodes(&mut decoder(&mono_params()), &MONO_WB);
        assert_pcm_stream_decodes(&mut decoder(&stereo_params()), &STEREO_MB);
        return;
    }

    #[test]
    fn hybrid_mono_and_stereo_packets_decode_to_recorded_values() {
        assert_stream_decodes(&mut decoder(&mono_params()), &HYBRID_MONO_SWB_10MS);
        assert_stream_decodes(&mut decoder(&mono_params()), &HYBRID_MONO_FB_20MS);
        assert_stream_decodes(&mut decoder(&stereo_params()), &HYBRID_STEREO_SWB_20MS);
        assert_stream_decodes(&mut decoder(&stereo_params()), &HYBRID_STEREO_FB_20MS);
        return;
    }

    #[test]
    fn celt_only_packets_of_every_frame_size_decode_to_recorded_values() {
        let mono = [&CELT_MONO_FB_2_5MS, &CELT_MONO_FB_5MS, &CELT_MONO_FB_10MS, &CELT_MONO_FB_20MS];
        for stream in mono {
            assert_stream_decodes(&mut decoder(&mono_params()), stream);
        }
        for stream in [&CELT_STEREO_FB_10MS, &CELT_STEREO_FB_20MS] {
            assert_stream_decodes(&mut decoder(&stereo_params()), stream);
        }
        return;
    }

    #[test]
    fn mode_transitions_and_redundancy_decode_to_recorded_values() {
        assert_stream_decodes(&mut decoder(&mono_params()), &MODE_TRANSITIONS);
        return;
    }

    /// RFC 7845 Section 5.1 accepts any `OpusHead` version of 15 or less. An ISO/MP4 `dOps` box
    /// carries version 0, so a version-0 header must configure the decoder like a version-1 one.
    #[test]
    fn version_0_opus_head_from_isomp4_configures_the_decoder() {
        // "OpusHead" | version=0 | channels=2 | pre_skip=312 LE | rate=48000 LE
        // | output_gain=-256 LE (-1.0 dB) | mapping_family=0
        let opus_head_v0: [u8; 19] = [
            b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', 0, 2, 0x38, 0x01, 0x80, 0xbb, 0x00,
            0x00, 0x00, 0xff, 0,
        ];
        let mut params = stereo_params();
        params.with_extra_data(Box::from(opus_head_v0));

        let dec = decoder(&params);
        assert_eq!(dec.pre_skip(), 312);
        assert_eq!(dec.output_gain(), -256);

        let mut rejected = stereo_params();
        let mut opus_head_v16 = opus_head_v0;
        opus_head_v16[8] = 16;
        rejected.with_extra_data(Box::from(opus_head_v16));
        assert!(OpusDecoder::try_new(&rejected, &AudioDecoderOptions::default()).is_err());
        return;
    }

    #[test]
    fn multistream_family_1_decodes_to_recorded_values() {
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

        assert_stream_decodes(&mut dec, &MULTISTREAM_FAMILY_1);

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
            MULTISTREAM_FAMILY_1.packets[0].packet,
        );
        gain_dec.decode_ref(&pkt0).unwrap();
        let muted_plane = gain_dec.buffer.plane(1).unwrap();
        assert!(muted_plane.iter().all(|&s| s == 0.0));
        let expected_scale = 10.0_f32.powf(256.0 / (20.0 * 256.0));
        let p0_scaled = gain_dec.buffer.plane(0).unwrap();
        assert!(
            (p0_scaled[960 - 1]
                - MULTISTREAM_FAMILY_1.packets[0].planes[0].tail[23] * expected_scale)
                .abs()
                < 1e-4
        );
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
            HYBRID_MONO_FB_20MS.packets[0].packet,
        );
        dec.decode_ref(&active_hybrid).unwrap();
        assert_eq!(dec.buffer.frames(), 960);

        let empty_pkt = PacketRef::new(0, Timestamp::from(960u32), Duration::from(960u64), &[]);
        dec.decode_ref(&empty_pkt).unwrap();
        assert_eq!(dec.buffer.frames(), 960);
        assert!(dec.buffer.plane(0).unwrap().iter().all(|s| s.is_finite()));

        // 1-byte Code 0 DTX packet (Hybrid FB 20 ms TOC = 0x78)
        let dtx_code0 =
            PacketRef::new(0, Timestamp::from(1920u32), Duration::from(960u64), &[0x78]);
        dec.decode_ref(&dtx_code0).unwrap();
        assert_eq!(dec.buffer.frames(), 960);
        assert!(dec.buffer.plane(0).unwrap().iter().all(|s| s.is_finite()));

        // 2. Decode active CELT 10 ms frame, then conceal lost packet, then 1-byte Code 1 DTX packet (2 x 10 ms = 960 samples)
        let active_celt = PacketRef::new(
            0,
            Timestamp::from(2880u32),
            Duration::from(480u64),
            CELT_MONO_FB_10MS.packets[0].packet,
        );
        dec.decode_ref(&active_celt).unwrap();
        assert_eq!(dec.buffer.frames(), 480);

        dec.decode_lost_packet().unwrap();
        assert_eq!(dec.buffer.frames(), 480);
        assert!(dec.buffer.plane(0).unwrap().iter().all(|s| s.is_finite()));

        // 1-byte Code 1 DTX packet (CELT FB 10 ms Code 1 TOC = 0xf1)
        let dtx_code1 =
            PacketRef::new(0, Timestamp::from(3840u32), Duration::from(960u64), &[0xf1]);
        dec.decode_ref(&dtx_code1).unwrap();
        assert_eq!(dec.buffer.frames(), 960);
        assert!(dec.buffer.plane(0).unwrap().iter().all(|s| s.is_finite()));
        return;
    }

    /// RFC 6716 Appendix A (`opus_decode_frame`): a frame of one byte carries no payload and is
    /// decoded exactly like a lost frame, so its output must equal explicit loss concealment from
    /// the same state and its final range state must be zero.
    #[test]
    fn one_byte_frame_is_concealed_like_a_lost_frame() {
        let mut mono_params = AudioCodecParameters::new();
        mono_params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(Position::FRONT_CENTER));
        let active = PacketRef::new(
            0,
            Timestamp::from(0u32),
            Duration::from(480u64),
            CELT_MONO_FB_10MS.packets[0].packet,
        );

        let mut lost = OpusDecoder::try_new(&mono_params, &AudioDecoderOptions::default()).unwrap();
        lost.decode_ref(&active).unwrap();
        lost.decode_lost_packet().unwrap();
        let concealed: Vec<f32> = lost.buffer.plane(0).unwrap().to_vec();

        let mut one_byte =
            OpusDecoder::try_new(&mono_params, &AudioDecoderOptions::default()).unwrap();
        one_byte.decode_ref(&active).unwrap();
        let one_byte_frame = [CELT_MONO_FB_10MS.packets[0].packet[0], 0x00];
        let pkt =
            PacketRef::new(0, Timestamp::from(480u32), Duration::from(480u64), &one_byte_frame[..]);
        one_byte.decode_ref(&pkt).unwrap();

        assert_eq!(one_byte.buffer.frames(), 480);
        assert_eq!(one_byte.final_range(), 0);
        assert_eq!(one_byte.buffer.plane(0).unwrap(), &concealed[..]);
        return;
    }
}
