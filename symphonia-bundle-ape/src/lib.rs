// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]
// The following lints are allowed in all Symphonia crates. Please see clippy.toml for their
// justification.
#![allow(clippy::comparison_chain)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_range_contains)]

use std::io::{Read, Seek, SeekFrom};

use ape_decoder::ApeError;
use ape_decoder::decoder::FrameDecoder;
use ape_decoder::format::{ApeFileInfo, parse as parse_ape};

use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, Channels, GenericAudioBufferRef,
    layouts,
};
use symphonia_core::codecs::audio::well_known::CODEC_ID_MONKEYS_AUDIO;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::codecs::{CodecInfo, CodecParameters};
use symphonia_core::errors::{
    Error, Result, SeekErrorKind, decode_error, seek_error, unsupported_error,
};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_APE;
use symphonia_core::io::{MediaSource, MediaSourceStream, ScopedStream};
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::packet::PacketRef;
use symphonia_core::{support_audio_codec, support_format};

const APE_FORMAT_INFO: FormatInfo =
    FormatInfo { format: FORMAT_ID_APE, short_name: "ape", long_name: "Monkey's Audio" };

const APE_CODEC_INFO: CodecInfo =
    CodecInfo { short_name: "ape", long_name: "Monkey's Audio", profiles: &[] };

/// The maximum size of a single compressed frame accepted by the demuxer.
const MAX_FRAME_BYTES: u64 = 64 * 1024 * 1024;

/// The size of the little-endian `seek_remainder` that prefixes the data of each packet.
const PACKET_PREFIX_LEN: usize = 4;

/// The length of the extra data: version, compression level, and bits per sample.
const EXTRA_DATA_LEN: usize = 6;

fn map_ape_error(err: ApeError) -> Error {
    match err {
        ApeError::Io(err) => Error::IoError(err),
        ApeError::InvalidFormat(msg) | ApeError::DecodingError(msg) => Error::DecodeError(msg),
        ApeError::InvalidChecksum => Error::DecodeError("ape: frame crc mismatch"),
        ApeError::UnsupportedVersion(_) => Error::Unsupported("ape: unsupported version"),
        _ => Error::DecodeError("ape: unknown error"),
    }
}

fn make_channels(count: u16) -> Channels {
    match count {
        1 => layouts::CHANNEL_LAYOUT_MONO,
        2 => layouts::CHANNEL_LAYOUT_STEREO,
        3 => layouts::CHANNEL_LAYOUT_3P0,
        4 => layouts::CHANNEL_LAYOUT_4P0_QUAD,
        5 => layouts::CHANNEL_LAYOUT_5P0,
        6 => layouts::CHANNEL_LAYOUT_5P1,
        n => Channels::Discrete(n),
    }
}

/// Monkey's Audio (APE) demuxer.
pub struct ApeReader<'s> {
    reader: MediaSourceStream<'s>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    chapters: Option<ChapterGroup>,
    info: ApeFileInfo,
    next_frame: u32,
}

impl<'s> ApeReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let info = parse_ape(&mut mss).map_err(map_ape_error)?;
        let header = &info.header;

        if header.channels == 0 || header.sample_rate == 0 || header.blocks_per_frame == 0 {
            return decode_error("ape: invalid header");
        }

        // The decoder crate subtracts neighbouring seek table entries, so they must not decrease.
        let seek_table_ok = info.seek_table.len() as u64 >= u64::from(header.total_frames)
            && info.seek_table.windows(2).all(|w| w[0] <= w[1])
            && info.seek_table.last().is_none_or(|&last| last < 1 << 62);
        if !seek_table_ok {
            return decode_error("ape: invalid seek table");
        }

        let mut extra_data = Vec::with_capacity(EXTRA_DATA_LEN);
        extra_data.extend_from_slice(&info.descriptor.version.to_le_bytes());
        extra_data.extend_from_slice(&header.compression_level.to_le_bytes());
        extra_data.extend_from_slice(&header.bits_per_sample.to_le_bytes());

        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_MONKEYS_AUDIO)
            .with_sample_rate(header.sample_rate)
            .with_channels(make_channels(header.channels))
            .with_bits_per_sample(u32::from(header.bits_per_sample))
            .with_bits_per_coded_sample(u32::from(header.bits_per_sample))
            .with_max_frames_per_packet(u64::from(header.blocks_per_frame))
            .with_extra_data(extra_data.into_boxed_slice());

        let mut track = Track::new(0);
        track.with_codec_params(CodecParameters::Audio(params));
        track.with_time_base(
            TimeBase::try_new(1, header.sample_rate)
                .ok_or(Error::DecodeError("ape: invalid sample rate"))?,
        );

        if let Ok(total) = u64::try_from(info.total_blocks) {
            track.with_num_frames(total).with_duration(Duration::from(total));
        }

        let media_info = MediaInfo::from_track(&track);

        Ok(ApeReader {
            reader: mss,
            media_info,
            tracks: vec![track],
            metadata: opts.external_data.metadata.unwrap_or_default(),
            chapters: opts.external_data.chapters,
            info,
            next_frame: 0,
        })
    }

    /// The number of bytes between the 32-bit aligned position of the frame and the frame itself.
    fn seek_remainder(&self, frame: u32) -> u32 {
        ((self.info.seek_byte(frame) - self.info.seek_byte(0)) % 4) as u32
    }
}

impl Scoreable for ApeReader<'_> {
    fn score(_src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        Ok(Score::Supported(255))
    }
}

impl ProbeableFormat<'_> for ApeReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(ApeReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(
            APE_FORMAT_INFO,
            &["ape", "mac"],
            &["audio/x-ape", "audio/ape"],
            &[b"MAC "]
        )]
    }
}

impl FormatReader for ApeReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &APE_FORMAT_INFO
    }

    fn media_info(&self) -> &MediaInfo {
        &self.media_info
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn chapters(&self) -> Option<&ChapterGroup> {
        self.chapters.as_ref()
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        if !self.reader.is_seekable() {
            return seek_error(SeekErrorKind::Unseekable);
        }

        let required_ts = match to {
            SeekTo::Timestamp { ts, .. } => ts,
            SeekTo::Time { time, .. } => {
                let tb =
                    self.tracks[0].time_base.ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
                tb.calc_timestamp(time).ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?
            }
        };

        let total = u64::try_from(self.info.total_blocks).unwrap_or(0);

        if required_ts.get() < 0 || required_ts.get() as u64 >= total {
            return seek_error(SeekErrorKind::OutOfRange);
        }

        let blocks_per_frame = u64::from(self.info.header.blocks_per_frame);
        let frame = (required_ts.get() as u64 / blocks_per_frame) as u32;

        self.next_frame = frame;

        let actual_ts = Timestamp::new((u64::from(frame) * blocks_per_frame) as i64);

        Ok(SeekedTo { track_id: 0, actual_ts, required_ts })
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        let frame = self.next_frame;

        if frame >= self.info.header.total_frames {
            return Ok(None);
        }

        let frame_bytes = self.info.frame_byte_count(frame);

        if frame_bytes > MAX_FRAME_BYTES {
            return decode_error("ape: frame is too large");
        }

        let remainder = self.seek_remainder(frame);
        let blocks = u64::from(self.info.frame_block_count(frame));

        // The decoder reads a few bytes past the end of the frame.
        let read_len = frame_bytes as usize + remainder as usize + 4;

        self.reader.seek(SeekFrom::Start(
            self.info.seek_byte(frame).saturating_sub(u64::from(remainder)),
        ))?;

        let mut data = vec![0u8; PACKET_PREFIX_LEN + read_len];
        data[..PACKET_PREFIX_LEN].copy_from_slice(&remainder.to_le_bytes());

        let mut filled = PACKET_PREFIX_LEN;

        while filled < data.len() {
            let n = self.reader.read(&mut data[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }

        data.truncate(filled);

        self.next_frame += 1;

        let ts = Timestamp::new(
            (u64::from(frame) * u64::from(self.info.header.blocks_per_frame)) as i64,
        );

        Ok(Some(Packet::new(0, ts, Duration::from(blocks), data)))
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}

/// Monkey's Audio (APE) decoder.
pub struct ApeDecoder {
    params: AudioCodecParameters,
    frame_decoder: FrameDecoder,
    bits_per_sample: u16,
    channels: usize,
    buf: AudioBuffer<i32>,
}

impl ApeDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_MONKEYS_AUDIO {
            return unsupported_error("ape: invalid codec");
        }

        let Some(extra) =
            params.extra_data.as_deref().filter(|extra| extra.len() >= EXTRA_DATA_LEN)
        else {
            return unsupported_error("ape: missing extra data");
        };

        let version = u16::from_le_bytes([extra[0], extra[1]]);
        let compression_level = u16::from_le_bytes([extra[2], extra[3]]);
        let bits_per_sample = u16::from_le_bytes([extra[4], extra[5]]);

        let Some(rate) = params.sample_rate
        else {
            return unsupported_error("ape: sample rate is required");
        };

        let Some(channels) = params.channels.clone()
        else {
            return unsupported_error("ape: channels or channel layout is required");
        };

        let num_channels = channels.count();

        let frame_decoder =
            FrameDecoder::new(version, num_channels as u16, bits_per_sample, compression_level)
                .map_err(map_ape_error)?;

        let max_frames = params.max_frames_per_packet.unwrap_or(73728 * 4) as usize;

        if !symphonia_core::audio::is_reasonable_capacity(max_frames, num_channels) {
            return unsupported_error("ape: maximum frames per packet is too large");
        }

        Ok(ApeDecoder {
            params: params.clone(),
            frame_decoder,
            bits_per_sample,
            channels: num_channels,
            buf: AudioBuffer::new(AudioSpec::new(rate, channels), max_frames),
        })
    }

    fn decode_inner(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        self.buf.clear();

        let Some((prefix, frame)) = packet.data.split_at_checked(PACKET_PREFIX_LEN)
        else {
            return decode_error("ape: packet is too short");
        };

        let remainder = u32::from_le_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]);
        if remainder > 3 {
            return decode_error("ape: invalid seek remainder");
        }
        let blocks = packet.dur.get() as usize;

        let pcm =
            self.frame_decoder.decode_frame(frame, remainder, blocks).map_err(map_ape_error)?;

        let bytes_per_sample = usize::from(self.bits_per_sample / 8);

        if pcm.len() < blocks * self.channels * bytes_per_sample {
            return decode_error("ape: decoded frame is too short");
        }

        let shift = 32 - u32::from(self.bits_per_sample);
        let channels = self.channels;
        let bits = self.bits_per_sample;

        self.buf.render_uninit(Some(blocks));

        for ch in 0..channels {
            let plane = self.buf.plane_mut(ch).ok_or(Error::DecodeError("ape: missing plane"))?;

            for (i, out) in plane.iter_mut().enumerate() {
                let off = (i * channels + ch) * bytes_per_sample;
                let sample = match bits {
                    // The stored 8-bit samples are unsigned.
                    8 => i32::from(pcm[off]) - 128,
                    16 => i32::from(i16::from_le_bytes([pcm[off], pcm[off + 1]])),
                    24 => i32::from_le_bytes([pcm[off], pcm[off + 1], pcm[off + 2], 0]) << 8 >> 8,
                    _ => i32::from_le_bytes([pcm[off], pcm[off + 1], pcm[off + 2], pcm[off + 3]]),
                };
                *out = sample.wrapping_shl(shift);
            }
        }

        if self.params_gapless_trim(packet) {
            self.buf.trim(packet.trim_start.get() as usize, packet.trim_end.get() as usize);
        }

        Ok(())
    }

    fn params_gapless_trim(&self, packet: &PacketRef<'_>) -> bool {
        packet.trim_start.get() != 0 || packet.trim_end.get() != 0
    }
}

impl AudioDecoder for ApeDecoder {
    fn reset(&mut self) {
        // Every frame is decoded independently.
    }

    fn codec_info(&self) -> &CodecInfo {
        &APE_CODEC_INFO
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

impl RegisterableAudioDecoder for ApeDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(ApeDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(CODEC_ID_MONKEYS_AUDIO, "ape", "Monkey's Audio")]
    }
}
