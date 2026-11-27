// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{Seek, SeekFrom};

use symphonia_core::audio::{Channels, Position};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::codecs::audio::well_known::CODEC_ID_DFPWM;
use symphonia_core::errors::{Error, Result, SeekErrorKind, decode_error, seek_error};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_DFPWM;
use symphonia_core::io::{MediaSource, MediaSourceStream, ReadBytes, ScopedStream};
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::support_format;

const DFPWM_FORMAT_INFO: FormatInfo =
    FormatInfo { format: FORMAT_ID_DFPWM, short_name: "dfpwm", long_name: "DFPWM" };

/// The sample rate that is assumed for a file, which has no header.
const DEFAULT_SAMPLE_RATE: u32 = 48000;

/// The number of bytes of a packet for every channel.
const PACKET_BYTES_PER_CHANNEL: u64 = 1024;

fn make_channels(count: u8) -> Channels {
    use symphonia_core::audio::layouts;

    match count {
        1 => layouts::CHANNEL_LAYOUT_MONO,
        2 => layouts::CHANNEL_LAYOUT_STEREO,
        n => Channels::Positioned(Position::from_bits_truncate(u64::MAX >> (64 - u32::from(n)))),
    }
}

/// Raw DFPWM demuxer.
///
/// The file has no header, so the stream is mono at 48000 Hz unless the reader is created with
/// [`DfpwmReader::try_new_with`]. The format has no marker either, so the reader is never selected
/// by the probe.
pub struct DfpwmReader<'s> {
    reader: MediaSourceStream<'s>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    chapters: Option<ChapterGroup>,
    channels: u64,
    packet_bytes: u64,
    packet_frames: u64,
    start: u64,
    total_frames: Option<u64>,
    next_ts: u64,
}

impl<'s> DfpwmReader<'s> {
    pub fn try_new(mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        Self::try_new_with(mss, opts, DEFAULT_SAMPLE_RATE, 1)
    }

    pub fn try_new_with(
        mss: MediaSourceStream<'s>,
        opts: FormatOptions,
        sample_rate: u32,
        channels: u8,
    ) -> Result<Self> {
        if channels == 0 || channels > 8 || sample_rate == 0 {
            return decode_error("dfpwm: invalid sample rate or channels");
        }

        let packet_bytes = PACKET_BYTES_PER_CHANNEL * u64::from(channels);
        let packet_frames = packet_bytes * 8 / u64::from(channels);

        let start = mss.pos();
        let total_frames =
            mss.byte_len().map(|len| len.saturating_sub(start) * 8 / u64::from(channels));

        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_DFPWM)
            .with_sample_rate(sample_rate)
            .with_channels(make_channels(channels))
            .with_max_frames_per_packet(packet_frames);

        let mut track = Track::new(0);
        track.with_codec_params(CodecParameters::Audio(params)).with_time_base(
            TimeBase::try_new(1, sample_rate)
                .ok_or(Error::DecodeError("dfpwm: invalid sample rate"))?,
        );
        if let Some(frames) = total_frames {
            track.with_num_frames(frames).with_duration(Duration::from(frames));
        }

        Ok(DfpwmReader {
            reader: mss,
            media_info: MediaInfo::from_track(&track),
            tracks: vec![track],
            metadata: opts.external_data.metadata.unwrap_or_default(),
            chapters: opts.external_data.chapters,
            channels: u64::from(channels),
            packet_bytes,
            packet_frames,
            start,
            total_frames,
            next_ts: 0,
        })
    }
}

impl Scoreable for DfpwmReader<'_> {
    fn score(_src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        Ok(Score::Supported(0))
    }
}

impl ProbeableFormat<'_> for DfpwmReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(DfpwmReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(DFPWM_FORMAT_INFO, &["dfpwm"], &["audio/x-dfpwm"], &[])]
    }
}

impl FormatReader for DfpwmReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &DFPWM_FORMAT_INFO
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

        let Some(total) = self.total_frames
        else {
            return seek_error(SeekErrorKind::Unseekable);
        };
        if required_ts.get() < 0 || required_ts.get() as u64 >= total {
            return seek_error(SeekErrorKind::OutOfRange);
        }

        // The decoder state is not stored, so it restarts from the start of the packet.
        let packet = required_ts.get() as u64 / self.packet_frames;

        self.reader.seek(SeekFrom::Start(self.start + packet * self.packet_bytes))?;
        self.next_ts = packet * self.packet_frames;

        Ok(SeekedTo { track_id: 0, actual_ts: Timestamp::new(self.next_ts as i64), required_ts })
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        let mut data = vec![0u8; self.packet_bytes as usize];
        let mut len = 0;
        while len < data.len() {
            match self.reader.read_buf(&mut data[len..]) {
                Ok(0) => break,
                Ok(n) => len += n,
                Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(err) => return Err(err.into()),
            }
        }

        let whole = len - len % self.channels as usize;
        if whole == 0 {
            return Ok(None);
        }
        data.truncate(whole);

        let dur = whole as u64 * 8 / self.channels;
        let ts = self.next_ts;
        self.next_ts += dur;

        Ok(Some(Packet::new(0, Timestamp::new(ts as i64), Duration::from(dur), data)))
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}
