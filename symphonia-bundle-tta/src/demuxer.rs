// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{Seek, SeekFrom};

use symphonia_core::audio::{Channels, Position};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::codecs::audio::well_known::CODEC_ID_TTA;
use symphonia_core::errors::{
    Error, Result, SeekErrorKind, decode_error, seek_error, unsupported_error,
};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_TTA;
use symphonia_core::io::{MediaSource, MediaSourceStream, ReadBytes, ScopedStream};
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::support_format;

const TTA_FORMAT_INFO: FormatInfo =
    FormatInfo { format: FORMAT_ID_TTA, short_name: "tta", long_name: "True Audio" };

const TTA_MARKER: [u8; 4] = *b"TTA1";

/// The format code of a simple (unencrypted) stream.
const TTA_FORMAT_SIMPLE: u16 = 1;

/// The length of the frame, in seconds, as a ratio: 256 / 245.
const FRAME_LEN_NUM: u64 = 256;
const FRAME_LEN_DEN: u64 = 245;

/// The size of the checksum that terminates each frame.
const FRAME_CRC_LEN: u64 = 4;

/// The maximum allowed size of a single frame.
const MAX_FRAME_LEN: u32 = 64 * 1024 * 1024;

fn make_channels(count: u16) -> Channels {
    use symphonia_core::audio::layouts;

    match count {
        1 => layouts::CHANNEL_LAYOUT_MONO,
        2 => layouts::CHANNEL_LAYOUT_STEREO,
        n if n <= 32 => {
            Channels::Positioned(Position::from_bits_truncate(u64::MAX >> (64 - u32::from(n))))
        }
        n => Channels::Discrete(n),
    }
}

/// True Audio (TTA) demuxer.
pub struct TtaReader<'s> {
    reader: MediaSourceStream<'s>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    chapters: Option<ChapterGroup>,
    frame_len: u64,
    total_samples: u64,
    /// The size in bytes, including the checksum, of every frame.
    frame_sizes: Vec<u32>,
    /// The position of the first frame in the stream.
    data_start: u64,
    next_frame: usize,
}

impl<'s> TtaReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        if mss.read_quad_bytes()? != TTA_MARKER {
            return unsupported_error("tta: missing stream marker");
        }

        let format = mss.read_u16()?;
        let channels = mss.read_u16()?;
        let bits_per_sample = mss.read_u16()?;
        let sample_rate = mss.read_u32()?;
        let total_samples = u64::from(mss.read_u32()?);
        let _header_crc = mss.read_u32()?;

        if format != TTA_FORMAT_SIMPLE {
            return unsupported_error("tta: encrypted streams are not supported");
        }

        if channels == 0 || sample_rate == 0 || !(1..=32).contains(&bits_per_sample) {
            return decode_error("tta: invalid header");
        }

        let frame_len = u64::from(sample_rate) * FRAME_LEN_NUM / FRAME_LEN_DEN;

        if frame_len == 0 {
            return decode_error("tta: invalid sample rate");
        }

        let num_frames = total_samples.div_ceil(frame_len) as usize;

        let mut frame_sizes = Vec::with_capacity(num_frames);

        for _ in 0..num_frames {
            let size = mss.read_u32()?;

            if size as u64 <= FRAME_CRC_LEN || size > MAX_FRAME_LEN {
                return decode_error("tta: invalid frame size");
            }

            frame_sizes.push(size);
        }

        // The seek table checksum.
        let _seek_crc = mss.read_u32()?;

        let data_start = mss.pos();

        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_TTA)
            .with_sample_rate(sample_rate)
            .with_channels(make_channels(channels))
            .with_bits_per_sample(u32::from(bits_per_sample))
            .with_max_frames_per_packet(frame_len);

        let mut track = Track::new(0);
        track
            .with_codec_params(CodecParameters::Audio(params))
            .with_time_base(
                TimeBase::try_new(1, sample_rate)
                    .ok_or(Error::DecodeError("tta: invalid sample rate"))?,
            )
            .with_num_frames(total_samples)
            .with_duration(Duration::from(total_samples));

        Ok(TtaReader {
            reader: mss,
            media_info: MediaInfo::from_track(&track),
            tracks: vec![track],
            metadata: opts.external_data.metadata.unwrap_or_default(),
            chapters: opts.external_data.chapters,
            frame_len,
            total_samples,
            frame_sizes,
            data_start,
            next_frame: 0,
        })
    }

    fn frame_offset(&self, frame: usize) -> u64 {
        self.data_start + self.frame_sizes[..frame].iter().map(|&s| u64::from(s)).sum::<u64>()
    }
}

impl Scoreable for TtaReader<'_> {
    fn score(_src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        Ok(Score::Supported(255))
    }
}

impl ProbeableFormat<'_> for TtaReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(TtaReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(TTA_FORMAT_INFO, &["tta"], &["audio/x-tta", "audio/tta"], &[b"TTA1"])]
    }
}

impl FormatReader for TtaReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &TTA_FORMAT_INFO
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

        if required_ts.get() < 0 || required_ts.get() as u64 >= self.total_samples {
            return seek_error(SeekErrorKind::OutOfRange);
        }

        let frame = (required_ts.get() as u64 / self.frame_len) as usize;

        self.reader.seek(SeekFrom::Start(self.frame_offset(frame)))?;
        self.next_frame = frame;

        let actual_ts = Timestamp::new((frame as u64 * self.frame_len) as i64);

        Ok(SeekedTo { track_id: 0, actual_ts, required_ts })
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        let Some(&size) = self.frame_sizes.get(self.next_frame)
        else {
            return Ok(None);
        };

        let first_sample = self.next_frame as u64 * self.frame_len;
        let dur = self.frame_len.min(self.total_samples - first_sample);

        let data = self.reader.read_boxed_slice_exact(size as usize)?;

        self.next_frame += 1;

        Ok(Some(Packet::new(0, Timestamp::new(first_sample as i64), Duration::from(dur), data)))
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}
