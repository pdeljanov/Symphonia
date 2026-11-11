//! Elementary Opus stream decoding: the SILK and CELT layers of one frame, packet loss
//! concealment and the configuration-switching rules
//! ([RFC 6716 Section 4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4>)).
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

use symphonia_core::errors::Result;

use crate::celt;
use crate::celt::BlockSize;
use crate::celt::constant::CELT_WINDOW;
use crate::celt::decoder::{FrameInput, FrameLayout, Mix};
use crate::entropy;
use crate::layout::Layout;
use crate::packet::FramePacket;
use crate::planes::Planes;
use crate::sample_rate::SampleRate;
use crate::silk;
use crate::toc::{AudioMode, Bandwidth, FrameDuration};

/// Maximum samples per channel in one Opus packet: 120 ms at 48 kHz (`5760`,
/// [RFC 6716 Section 3.2.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.5>)).
pub(crate) const MAX_PACKET_SAMPLES_48K: usize = 5760;

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
const OVERLAP_FLUSH_SAMPLES_48K: usize = celt::OVERLAP;

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

/// Bit count of `bytes` whole bytes, in the `i32` domain of [`entropy::Decoder::tell`].
const fn bits_in(bytes: usize) -> i32 {
    return (bytes as i32) * BITS_PER_BYTE;
}

/// Whole bytes that hold `bits` bits, rounded up to the next byte boundary.
const fn whole_bytes(bits: i32) -> usize {
    return (bits as u32).div_ceil(u8::BITS) as usize;
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

/// Final range coder states of one frame's CELT work: the active layer and the 5 ms redundant
/// frame, zero without one
/// ([RFC 6716 Section 4.5.1.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1.4>)).
#[derive(Debug, Clone, Copy)]
struct CeltRanges {
    layer: u32,
    redundant: u32,
}

impl CeltRanges {
    /// The frame's contribution to the final range: both states XOR-combined
    /// ([RFC 6716 Section 6](<https://datatracker.ietf.org/doc/html/rfc6716#section-6>)).
    const fn combined(self) -> u32 {
        return self.layer ^ self.redundant;
    }
}

/// Elementary Opus stream decoder pairing a SILK decoder and a CELT decoder for a mono or
/// coupled stereo stream
/// ([RFC 6716 Section 4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4>)).
pub(crate) struct StreamDecoder {
    layout: Layout,
    sample_rate: SampleRate,
    silk_decoder: silk::Decoder,
    celt_decoder: celt::Decoder,
    /// `None` until the first frame has been decoded; a loss before that produces silence.
    last: Option<LastFrame>,
    /// Final range coder state of the last decoded frame, zero after a loss
    /// ([RFC 6716 Section 6](<https://datatracker.ietf.org/doc/html/rfc6716#section-6>)).
    range_final: u32,
}

impl StreamDecoder {
    /// Creates a new [`StreamDecoder`] producing `layout` output at `sample_rate`.
    pub(crate) fn new(sample_rate: SampleRate, layout: Layout) -> Result<Self> {
        return Ok(Self {
            layout,
            sample_rate,
            silk_decoder: silk::Decoder::new(sample_rate),
            celt_decoder: celt::Decoder::new(sample_rate, layout.channels())?,
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

    /// The final range coder state of the last decoded frame, zero after a loss.
    #[inline]
    pub(crate) fn final_range(&self) -> u32 {
        return self.range_final;
    }

    /// The duration of the last decoded frame, `None` before the first one.
    #[inline]
    pub(crate) fn last_duration(&self) -> Option<FrameDuration> {
        return self.last.map(|last| last.config.duration);
    }

    /// Whether the stream decodes into two output planes.
    #[inline]
    fn is_stereo(&self) -> bool {
        return self.layout.is_stereo();
    }

    /// Decodes every frame of `packet` back to back into the `scratch` rows and returns the
    /// samples written per channel.
    pub(crate) fn decode_packet(
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
    pub(crate) fn conceal_lost_packet(
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
                Layout::from_stereo_flag(config.stereo).channels(),
                duration,
                config.bandwidth,
            )?;
            self.silk_decoder.conceal_frame_planar(config.stereo, out)?;
            return Ok(());
        }

        let mut full = [[0.0_f32; MAX_20MS_SAMPLES_48K]; 2];
        let mut full_planes = Planes::from_rows(&mut full, self.is_stereo());
        self.silk_decoder.configure(
            Layout::from_stereo_flag(config.stereo).channels(),
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
            stream_channels: Layout::from_stereo_flag(config.stereo).channels(),
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
        let ranges = self.decode_celt_layers(
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
        self.range_final = match payload.active.len() {
            0 | 1 => 0,
            _ => ranges.combined(),
        };
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
            Layout::from_stereo_flag(config.stereo).channels(),
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
    ) -> Result<CeltRanges> {
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
        return Ok(CeltRanges { layer: layer_range, redundant: redundant_range });
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
            stream_channels: Layout::from_stereo_flag(config.stereo).channels(),
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
                stream_channels: Layout::from_stereo_flag(config.stereo).channels(),
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
