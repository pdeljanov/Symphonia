//! The Symphonia Opus decoder
//! ([RFC 6716 Section 4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4>),
//! [RFC 7845 Section 5](<https://datatracker.ietf.org/doc/html/rfc7845#section-5>)).
//!
//! [`OpusDecoder`] implements the Symphonia [`AudioDecoder`] trait: it resolves the output
//! format and the stream layout from the codec parameters and the `OpusHead` identification
//! header, drives one [`StreamDecoder`] per elementary stream ([`crate::stream`]) and routes the
//! decoded channels of each stream to the output planes ([`crate::routing`]).

use std::sync::LazyLock;

use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, Channels, GenericAudioBufferRef,
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

use crate::header::{
    ChannelMapping, ChannelMappingTable, IdentificationHeader, OutputGain, SILENT_CHANNEL,
};
use crate::layout::Layout;
use crate::routing::{OutputRouting, StreamPackets};
use crate::sample_rate::SampleRate;
use crate::stream::{MAX_PACKET_SAMPLES_48K, StreamDecoder};
use crate::toc::FrameDuration;

/// Largest output channel count an `OpusHead` can describe: the channel count is one byte
/// ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)).
const MAX_OUTPUT_CHANNELS: usize = u8::MAX as usize;

/// Static Opus codec info.
static CODEC_INFO: LazyLock<CodecInfo> = LazyLock::new(|| CodecInfo {
    short_name: "opus",
    long_name: "Opus Audio Codec",
    profiles: &[],
});

/// Static supported codecs array.
static SUPPORTED_CODECS: LazyLock<[SupportedAudioCodec; 1]> =
    LazyLock::new(|| [SupportedAudioCodec { id: CODEC_ID_OPUS, info: *CODEC_INFO }]);

/// The elementary streams of a packet and how their decoded channels reach the output channels
/// ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)).
struct StreamLayout {
    streams: Vec<StreamDecoder>,
    coupled_count: usize,
    channel_mapping: Vec<u8>,
}

impl StreamLayout {
    /// Family 0, the RTP mapping: one stream decoding straight to 1 or 2 output channels
    /// ([RFC 7845 Section 5.1.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1.1>)).
    fn rtp(sample_rate: SampleRate, num_output_channels: usize) -> Result<Self> {
        let Some(layout) = Layout::from_channels(num_output_channels)
        else {
            return unsupported_error("opus: more than 2 channels need an OpusHead mapping");
        };
        let channel_mapping = match layout {
            Layout::Mono => vec![0],
            Layout::Stereo => vec![0, 1],
        };
        return Ok(Self {
            streams: vec![StreamDecoder::new(sample_rate, layout)?],
            coupled_count: usize::from(layout.is_stereo()),
            channel_mapping,
        });
    }

    /// `N` streams, `M` of them coupled, routed to the output channels by the mapping table
    /// ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)).
    fn from_table(sample_rate: SampleRate, table: &ChannelMappingTable) -> Result<Self> {
        let stream_count = usize::from(table.stream_count);
        let coupled_count = usize::from(table.coupled_count);
        let streams = (0..stream_count)
            .map(|i| StreamDecoder::new(sample_rate, Layout::from_stereo_flag(i < coupled_count)))
            .collect::<Result<Vec<_>>>()?;
        return Ok(Self { streams, coupled_count, channel_mapping: table.channel_mapping.clone() });
    }

    /// Selects the layout the header describes, or the RTP mapping when there is no header.
    fn resolve(
        id: Option<&IdentificationHeader>,
        sample_rate: SampleRate,
        num_output_channels: usize,
    ) -> Result<Self> {
        let Some(id) = id
        else {
            return Self::rtp(sample_rate, num_output_channels);
        };
        if usize::from(id.channel_count) != num_output_channels {
            return Err(SymphoniaError::DecodeError(
                "opus: OpusHead channel count does not match AudioCodecParameters",
            ));
        }
        return match &id.channel_mapping {
            ChannelMapping::Rtp => Self::rtp(sample_rate, num_output_channels),
            ChannelMapping::Vorbis(table) => Self::from_table(sample_rate, table),
            ChannelMapping::Reserved(..) | ChannelMapping::Undefined(_) => {
                unsupported_error("opus: unsupported channel mapping family")
            }
        };
    }
}

/// The output sample rate and channel layout the decoder produces.
struct OutputFormat {
    sample_rate: SampleRate,
    channels: Channels,
}

impl OutputFormat {
    /// Resolves the format from the codec parameters, falling back to the header.
    ///
    /// Every Opus stream decodes at 48 kHz; the `Input Sample Rate` of the header is not a
    /// playback rate
    /// ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)). A
    /// declared rate selects a downsampled output only when it is one the decoder can produce;
    /// an absent or foreign rate (such as a container's original recording rate) decodes at
    /// 48 kHz. The channel layout comes from the parameters, else from the header's channel
    /// count and mapping family
    /// ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)).
    fn resolve(params: &AudioCodecParameters, id: Option<&IdentificationHeader>) -> Result<Self> {
        let sample_rate = match params.sample_rate {
            Some(hz) => SampleRate::try_from(hz).unwrap_or_else(|_| {
                log::warn!("opus: declared sample rate {hz} Hz is not an Opus output rate, decoding at 48 kHz");
                return SampleRate::Hz48000;
            }),
            None => SampleRate::Hz48000,
        };
        let channels = params
            .channels
            .clone()
            .or_else(|| id.map(IdentificationHeader::channels))
            .ok_or(SymphoniaError::Unsupported("opus: missing channel layout"))?;
        let count = channels.count();
        if count == 0 || count > MAX_OUTPUT_CHANNELS {
            return unsupported_error("opus: unsupported output channel count");
        }
        return Ok(Self { sample_rate, channels });
    }
}

/// Pure-Rust Symphonia [`AudioDecoder`] for the Opus audio codec
/// ([RFC 6716](<https://datatracker.ietf.org/doc/html/rfc6716>),
/// [RFC 7845](<https://datatracker.ietf.org/doc/html/rfc7845>),
/// [RFC 8251](<https://datatracker.ietf.org/doc/html/rfc8251>)).
///
/// `OpusDecoder` decodes SILK-only, Hybrid, and CELT-only packets across all 32 TOC configurations
/// (`2.5 ms` to `60 ms` frames, up to `120 ms` per packet) into planar 32-bit floating-point
/// (`f32`) audio buffers.
///
/// # Configuration from [`AudioCodecParameters`]
///
/// - **Container streams with `OpusHead` (`params.extra_data`)**: Ogg Opus, Matroska / WebM, and
///   ISO/MP4 streams supply an `OpusHead` identification header in `params.extra_data`. The
///   decoder validates the header and configures the output channel count, channel mapping family
///   (`0` for mono/stereo RTP or `1` for `1..=8` channel Vorbis surround), [`Self::pre_skip`], and
///   Q7.8 dB [`Self::output_gain`].
/// - **Headerless / RTP streams (`params.extra_data == None`)**: When no `extra_data` is present,
///   `params.channels` must describe `1` (mono) or `2` (stereo) channels, which are decoded using
///   Mapping Family `0` with `pre_skip = 0` and `0.0 dB` unity gain.
/// - **Output sample rate (`params.sample_rate`)**: Opus decodes natively at `48_000` Hz and also
///   supports direct output at `24_000`, `16_000`, `12_000`, or `8_000` Hz. If `params.sample_rate`
///   is `None` or specifies a non-Opus container rate (such as `44_100` Hz), decoding automatically
///   falls back to `48_000` Hz per [RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)
///   and updates [`AudioDecoder::codec_params`] accordingly.
///
/// # Examples
///
/// ```rust
/// use symphonia_codec_opus::OpusDecoder;
/// use symphonia_core::audio::{Audio, Channels, Position};
/// use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
/// use symphonia_core::codecs::audio::well_known::CODEC_ID_OPUS;
/// use symphonia_core::packet::PacketRef;
/// use symphonia_core::units::{Duration, Timestamp};
///
/// let mut params = AudioCodecParameters::new();
/// params
///     .for_codec(CODEC_ID_OPUS)
///     .with_sample_rate(48_000)
///     .with_channels(Channels::Positioned(Position::FRONT_LEFT | Position::FRONT_RIGHT));
///
/// let mut decoder = OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();
///
/// // Decode a 20 ms Fullband stereo CELT silence packet (960 samples per channel at 48 kHz):
/// let frame = [0xFC, 0xFF, 0xFE];
/// let packet = PacketRef::new(0, Timestamp::from(0u32), Duration::from(960u64), &frame);
/// let out = decoder.decode_ref(&packet).unwrap();
/// assert_eq!(out.frames(), 960);
/// assert_eq!(out.spec().rate(), 48_000);
/// ```
pub struct OpusDecoder {
    params: AudioCodecParameters,
    sample_rate: SampleRate,
    buffer: AudioBuffer<f32>,
    streams: Vec<StreamDecoder>,
    scratch: [[f32; MAX_PACKET_SAMPLES_48K]; 2],
    coupled_count: usize,
    channel_mapping: Vec<u8>,
    pre_skip: u16,
    output_gain: OutputGain,
}

impl OpusDecoder {
    /// Parses the `OpusHead` identification header
    /// ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)) from
    /// `params.extra_data`, or returns `None` when the parameters carry no extra data.
    fn parse_identification_header(
        params: &AudioCodecParameters,
    ) -> Result<Option<IdentificationHeader>> {
        return params
            .extra_data
            .as_deref()
            .filter(|extra| !extra.is_empty())
            .map(|extra| {
                IdentificationHeader::parse(BufReader::new(extra)).map_err(SymphoniaError::from)
            })
            .transpose();
    }

    /// Creates a new [`OpusDecoder`] from `params` and `_options`.
    ///
    /// # Errors
    ///
    /// Returns:
    /// - [`Error::Unsupported`](symphonia_core::errors::Error::Unsupported) if `params.codec` is
    ///   not [`CODEC_ID_OPUS`], if no channel count can be resolved from either `params.channels`
    ///   or `params.extra_data`, if more than 2 channels are requested without an `OpusHead`
    ///   mapping table, or if the `OpusHead` requests an unsupported channel mapping family
    ///   (`2..=255`).
    /// - [`Error::DecodeError`](symphonia_core::errors::Error::DecodeError) if `params.extra_data`
    ///   is present and violates [RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)
    ///   (invalid `OpusHead` magic, version `> 15`, invalid stream/coupled counts, out-of-range
    ///   mapping index, or channel count mismatch with `params.channels`).
    pub fn try_new(params: &AudioCodecParameters, _options: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_OPUS {
            return unsupported_error("opus: invalid codec");
        }

        let id = Self::parse_identification_header(params)?;
        let format = OutputFormat::resolve(params, id.as_ref())?;
        let layout =
            StreamLayout::resolve(id.as_ref(), format.sample_rate, format.channels.count())?;
        let (pre_skip, output_gain) =
            id.map_or((0, OutputGain::UNITY), |id| (id.pre_skip, id.output_gain));

        // One packet carries at most 120 ms (RFC 6716 Section 3.2.5).
        let spec = AudioSpec::new(format.sample_rate.hz(), format.channels.clone());
        let buffer = AudioBuffer::new(
            spec,
            MAX_PACKET_SAMPLES_48K / format.sample_rate.downsample_from_48k(),
        );

        // Report the effective output format through `codec_params`.
        let mut params = params.to_owned();
        params.with_sample_rate(format.sample_rate.hz()).with_channels(format.channels);

        return Ok(Self {
            params,
            sample_rate: format.sample_rate,
            buffer,
            streams: layout.streams,
            scratch: [[0.0; MAX_PACKET_SAMPLES_48K]; 2],
            coupled_count: layout.coupled_count,
            channel_mapping: layout.channel_mapping,
            pre_skip,
            output_gain,
        });
    }

    /// Returns the `pre_skip` sample count (at 48 kHz) parsed from the `OpusHead` identification
    /// header, or `0` for headerless streams.
    ///
    /// Per [RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>),
    /// `pre_skip` indicates how many 48 kHz samples should be discarded from the start of playback
    /// to account for encoder lookahead (scale by `sample_rate / 48_000` when decoding at reduced
    /// rates). Symphonia's Ogg and Matroska demuxers already expose this delay on the track's
    /// `CodecParameters` for gapless trimming, so normal Symphonia playback does not need to trim
    /// `pre_skip` manually.
    #[inline]
    pub fn pre_skip(&self) -> u16 {
        return self.pre_skip;
    }

    /// Returns the signed Q7.8 fixed-point `output_gain` (in `1 / 256` dB steps) parsed from the
    /// `OpusHead` identification header, or `0` (`0.0 dB`) for headerless streams.
    ///
    /// [`OpusDecoder`] **automatically applies** this gain (`10^(output_gain / (20 * 256))`)
    /// to every decoded and concealed sample written to the output buffer
    /// ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>));
    /// callers should not apply it a second time unless inspecting header metadata.
    #[inline]
    pub fn output_gain(&self) -> i16 {
        return self.output_gain.q7_8_db();
    }

    /// Returns the 32-bit final range decoder state of the last decoded packet, XOR-combined
    /// across elementary streams
    /// ([RFC 6716 Section 6](<https://datatracker.ietf.org/doc/html/rfc6716#section-6>)), or `0`
    /// after a lost, empty, or DTX-concealed packet.
    ///
    /// Conformance suites compare `final_range()` against the encoder's recorded `enc_final_range`
    /// on every packet to verify bit-exact entropy consumption across SILK and CELT.
    pub fn final_range(&self) -> u32 {
        return self.streams.iter().fold(0, |range, stream| range ^ stream.final_range());
    }

    /// Conceals one lost packet across all elementary streams using the duration of the most
    /// recently decoded frame (or `20 ms` if no packet has been decoded yet)
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)) and
    /// returns a reference to the synthesized output buffer.
    ///
    /// Passing an empty slice (`&[]`) to [`AudioDecoder::decode_ref`] delegates to this method as
    /// well. Calling `decode_lost_packet` directly is convenient when a real-time RTP or container
    /// reader detects a sequence-number or timestamp gap without constructing a dummy packet.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use symphonia_codec_opus::OpusDecoder;
    /// use symphonia_core::audio::{Audio, Channels, Position};
    /// use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
    /// use symphonia_core::codecs::audio::well_known::CODEC_ID_OPUS;
    /// use symphonia_core::packet::PacketRef;
    /// use symphonia_core::units::{Duration, Timestamp};
    ///
    /// let mut params = AudioCodecParameters::new();
    /// params
    ///     .for_codec(CODEC_ID_OPUS)
    ///     .with_sample_rate(48_000)
    ///     .with_channels(Channels::Positioned(Position::FRONT_CENTER));
    ///
    /// let mut decoder = OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();
    ///
    /// // Decode a 10 ms mono CELT frame (480 samples at 48 kHz), then conceal a lost packet:
    /// let active = PacketRef::new(0, Timestamp::from(0u32), Duration::from(480u64), &[0xF0, 0xFF, 0xFE]);
    /// decoder.decode_ref(&active).unwrap();
    ///
    /// let concealed = decoder.decode_lost_packet().unwrap();
    /// assert_eq!(concealed.frames(), 480); // Matches the 10 ms duration of the prior frame
    /// assert_eq!(decoder.final_range(), 0);
    /// ```
    pub fn decode_lost_packet(&mut self) -> Result<GenericAudioBufferRef<'_>> {
        let duration = self
            .streams
            .first()
            .and_then(StreamDecoder::last_duration)
            .unwrap_or(FrameDuration::Ms20);
        let total_samples = self.sample_rate.samples_per(duration);
        log::debug!("opus: concealing a lost packet of {total_samples} samples per channel");
        self.prepare_output_buffer(total_samples)?;

        let routing = OutputRouting {
            channel_mapping: &self.channel_mapping,
            coupled_count: self.coupled_count,
            gain: self.output_gain.scale(),
        };
        for (i, stream) in self.streams.iter_mut().enumerate() {
            stream.conceal_lost_packet(duration, &mut self.scratch)?;
            routing.route(i, total_samples, &self.scratch, &mut self.buffer)?;
        }

        return Ok(self.buffer.as_generic_audio_buffer_ref());
    }

    /// Clears the output buffer, renders `total_samples` frames and silences the output
    /// channels whose mapping entry is [`SILENT_CHANNEL`]
    /// ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)).
    fn prepare_output_buffer(&mut self, total_samples: usize) -> Result<()> {
        self.buffer.clear();
        self.buffer.render_uninit(Some(total_samples));
        let unmapped = self
            .channel_mapping
            .iter()
            .enumerate()
            .filter(|&(_, &map)| map == SILENT_CHANNEL)
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
            gain: self.output_gain.scale(),
        };
        let packets = std::iter::once(Ok(first)).chain(packets);
        for (i, (stream, stream_packet)) in self.streams.iter_mut().zip(packets).enumerate() {
            let stream_packet = stream_packet?;
            if (stream_packet.frames.len(), stream_packet.frame_duration) != shape {
                self.buffer.clear();
                return Err(SymphoniaError::DecodeError(
                    "opus: inconsistent frame count or frame duration across elementary streams",
                ));
            }
            stream.decode_packet(&stream_packet, &mut self.scratch)?;
            routing.route(i, total_samples, &self.scratch, &mut self.buffer)?;
        }

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
    use crate::header::test_support::OpusHeadBuilder;
    use crate::test_vectors::*;
    use symphonia_core::audio::{Audio, Channels, Position};
    use symphonia_core::codecs::audio::well_known::CODEC_ID_VORBIS;
    use symphonia_core::units::{Duration, Timestamp};

    /// The 48 kHz codec parameters of an output with the channels `positions`.
    /// Parameters carrying only `OpusHead` extra data; rate and channels come from the fallbacks.
    fn head_params(head: OpusHeadBuilder) -> AudioCodecParameters {
        let mut params = AudioCodecParameters::new();
        params.for_codec(CODEC_ID_OPUS).with_extra_data(head.build().into_boxed_slice());
        return params;
    }

    /// Family 1 three-channel `OpusHead`: 2 streams, 1 coupled, channel mapping `[0, 2, 1]`.
    fn three_channel_head() -> OpusHeadBuilder {
        return OpusHeadBuilder::new().channels(3).family(1).table(2, 1, &[0, 2, 1]);
    }

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
        let mut params = stereo_params();
        let head = OpusHeadBuilder::new().version(0).output_gain(-256).build();
        params.with_extra_data(head.into_boxed_slice());

        let dec = decoder(&params);
        assert_eq!(dec.pre_skip(), 312);
        assert_eq!(dec.output_gain(), -256);

        let mut rejected = stereo_params();
        let head = OpusHeadBuilder::new().version(16).build();
        rejected.with_extra_data(head.into_boxed_slice());
        assert!(OpusDecoder::try_new(&rejected, &AudioDecoderOptions::default()).is_err());
        return;
    }

    /// RFC 7845 Section 5.1: every Opus stream decodes at 48 kHz and the header's input sample
    /// rate is not a playback rate. A missing or foreign declared rate (a container's original
    /// recording rate) decodes at 48 kHz; a declared Opus output rate still selects that rate.
    #[test]
    fn missing_or_foreign_sample_rate_decodes_at_48_khz() {
        for declared in [None, Some(44_100)] {
            let mut params = head_params(OpusHeadBuilder::new());
            declared.into_iter().for_each(|hz| {
                params.with_sample_rate(hz);
            });
            let dec = decoder(&params);
            assert_eq!(dec.buffer.spec().rate(), 48_000);
            assert_eq!(dec.codec_params().sample_rate, Some(48_000));
        }

        let mut params = head_params(OpusHeadBuilder::new());
        params.with_sample_rate(16_000);
        assert_eq!(decoder(&params).buffer.spec().rate(), 16_000);
        return;
    }

    /// RFC 7845 Section 5.1.1: when the parameters carry no channel layout, the `OpusHead` channel
    /// count and mapping family define it. Without either, the decoder cannot be created.
    #[test]
    fn missing_channels_come_from_the_opus_head() {
        let dec = decoder(&head_params(OpusHeadBuilder::new()));
        let left_right = Channels::Positioned(Position::FRONT_LEFT | Position::FRONT_RIGHT);
        assert_eq!(dec.buffer.spec().channels(), &left_right);
        assert_eq!(dec.codec_params().channels, Some(left_right));

        let dec = decoder(&head_params(three_channel_head()));
        let three = Position::FRONT_LEFT | Position::FRONT_CENTER | Position::FRONT_RIGHT;
        assert_eq!(dec.buffer.spec().channels(), &Channels::Positioned(three));
        assert_eq!(dec.streams.len(), 2);

        let mut params = AudioCodecParameters::new();
        params.for_codec(CODEC_ID_OPUS).with_sample_rate(48_000);
        assert!(OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).is_err());
        return;
    }

    #[test]
    fn multistream_family_1_decodes_to_recorded_values() {
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(
                Position::FRONT_LEFT | Position::FRONT_CENTER | Position::FRONT_RIGHT,
            ))
            .with_extra_data(three_channel_head().build().into_boxed_slice());

        let mut dec = OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();
        assert_eq!(dec.pre_skip(), 312);
        assert_eq!(dec.output_gain(), 0);

        assert_stream_decodes(&mut dec, &MULTISTREAM_FAMILY_1);

        // Also verify non-zero output_gain (+256 Q7.8 = +1.0 dB) and muted channel mapping (255)
        let gain_mute = OpusHeadBuilder::new()
            .channels(3)
            .pre_skip(0)
            .output_gain(256)
            .family(1)
            .table(2, 1, &[0, 255, 1]);
        let mut gain_params = AudioCodecParameters::new();
        gain_params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(
                Position::FRONT_LEFT | Position::FRONT_CENTER | Position::FRONT_RIGHT,
            ))
            .with_extra_data(gain_mute.build().into_boxed_slice());
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
