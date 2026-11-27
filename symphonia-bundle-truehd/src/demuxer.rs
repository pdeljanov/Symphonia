// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{ErrorKind, Seek, SeekFrom};

use symphonia_core::audio::{Channels, Position};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::codecs::audio::well_known::{CODEC_ID_MLP, CODEC_ID_TRUEHD};
use symphonia_core::errors::{
    Error, Result, SeekErrorKind, decode_error, seek_error, unsupported_error,
};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_TRUEHD;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::support_format;

use crate::mlp::{AU_HEADER_LEN, MlpCore, SYNC_FBA, SYNC_FBB, access_unit_len, major_sync};

const TRUEHD_FORMAT_INFO: FormatInfo =
    FormatInfo { format: FORMAT_ID_TRUEHD, short_name: "truehd", long_name: "Dolby TrueHD / MLP" };

/// The signature that follows the format info of a major sync.
const MAJOR_SYNC_SIGNATURE: u16 = 0xb752;

const SYNC_FBA_MARKER: [u8; 4] = SYNC_FBA.to_be_bytes();
const SYNC_FBB_MARKER: [u8; 4] = SYNC_FBB.to_be_bytes();

fn make_channels(count: usize) -> Channels {
    use symphonia_core::audio::layouts;

    match count {
        1 => layouts::CHANNEL_LAYOUT_MONO,
        2 => layouts::CHANNEL_LAYOUT_STEREO,
        n => Channels::Positioned(Position::from_bits_truncate(u64::MAX >> (64 - n as u32))),
    }
}

/// Dolby TrueHD and MLP elementary stream demuxer.
pub struct TrueHdReader<'s> {
    reader: MediaSourceStream<'s>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    chapters: Option<ChapterGroup>,
    first_pos: u64,
    frames_per_au: u64,
    next_ts: u64,
    pending: Option<Vec<u8>>,
    /// The timestamp and the position of every access unit with a major sync that was read.
    sync_points: Vec<(u64, u64)>,
}

impl<'s> TrueHdReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let mut head = [0u8; 4];
        mss.read_buf_exact(&mut head)?;
        mss.seek_buffered_rev(head.len());

        // The probe stops at the major sync, which follows the header of the access unit.
        if head == SYNC_FBA_MARKER || head == SYNC_FBB_MARKER {
            mss.seek_buffered_rev(AU_HEADER_LEN);
        }

        let first_pos = mss.pos();
        let first =
            read_access_unit(&mut mss)?.ok_or(Error::DecodeError("truehd: empty stream"))?;

        if major_sync(&first).is_none() {
            return unsupported_error("truehd: the stream does not start with a major sync");
        }

        let mut core = MlpCore::default();
        let mut frames = Vec::new();
        if core.decode(&first, &mut frames).is_err() {
            return unsupported_error("truehd: unsupported stream");
        }
        let (Some(info), channels) = (core.info(), core.channels())
        else {
            return unsupported_error("truehd: unsupported stream");
        };
        if channels == 0 || channels > 14 {
            return unsupported_error("truehd: unsupported channel count");
        }

        let codec = if info.is_fba { CODEC_ID_TRUEHD } else { CODEC_ID_MLP };

        let mut params = AudioCodecParameters::new();
        params
            .for_codec(codec)
            .with_sample_rate(info.sample_rate)
            .with_bits_per_sample(24)
            .with_channels(make_channels(channels))
            .with_max_frames_per_packet(info.frames_per_au as u64);

        let mut track = Track::new(0);
        track.with_codec_params(CodecParameters::Audio(params)).with_time_base(
            TimeBase::try_new(1, info.sample_rate)
                .ok_or(Error::DecodeError("truehd: invalid sample rate"))?,
        );

        Ok(TrueHdReader {
            reader: mss,
            media_info: MediaInfo::from_track(&track),
            tracks: vec![track],
            metadata: opts.external_data.metadata.unwrap_or_default(),
            chapters: opts.external_data.chapters,
            first_pos,
            frames_per_au: info.frames_per_au as u64,
            next_ts: 0,
            pending: Some(first),
            sync_points: Vec::new(),
        })
    }

    fn read_next(&mut self) -> Result<Option<(u64, Vec<u8>)>> {
        let pos = if self.pending.is_some() { self.first_pos } else { self.reader.pos() };
        let data = match self.pending.take() {
            Some(data) => data,
            None => match read_access_unit(&mut self.reader)? {
                Some(data) => data,
                None => return Ok(None),
            },
        };

        let ts = self.next_ts;
        self.next_ts += self.frames_per_au;

        let is_new = self.sync_points.last().is_none_or(|&(last, _)| ts > last);
        if is_new && major_sync(&data).is_some() {
            self.sync_points.push((ts, pos));
        }

        Ok(Some((ts, data)))
    }
}

/// Reads an access unit, or returns `None` at the end of the stream.
fn read_access_unit(reader: &mut MediaSourceStream<'_>) -> Result<Option<Vec<u8>>> {
    let mut header = [0u8; AU_HEADER_LEN];
    match reader.read_buf_exact(&mut header) {
        Ok(()) => (),
        Err(err) if err.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err.into()),
    }

    let len = access_unit_len(&header).unwrap_or(0);
    if len < AU_HEADER_LEN + 4 {
        return decode_error("truehd: invalid access unit length");
    }

    let mut data = vec![0u8; len];
    data[..AU_HEADER_LEN].copy_from_slice(&header);
    match reader.read_buf_exact(&mut data[AU_HEADER_LEN..]) {
        Ok(()) => Ok(Some(data)),
        Err(err) if err.kind() == ErrorKind::UnexpectedEof => Ok(None),
        Err(err) => Err(err.into()),
    }
}

impl Scoreable for TrueHdReader<'_> {
    fn score(mut src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        let mut head = [0u8; 10];
        src.read_buf_exact(&mut head)?;

        let sync = [head[0], head[1], head[2], head[3]];
        let signature = u16::from_be_bytes([head[8], head[9]]);

        let valid = (sync == SYNC_FBA_MARKER || sync == SYNC_FBB_MARKER)
            && signature == MAJOR_SYNC_SIGNATURE;

        Ok(if valid { Score::Supported(255) } else { Score::Unsupported })
    }
}

impl ProbeableFormat<'_> for TrueHdReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(TrueHdReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(
            TRUEHD_FORMAT_INFO,
            &["thd", "truehd", "mlp"],
            &["audio/vnd.dolby.mlp"],
            &[&SYNC_FBA_MARKER, &SYNC_FBB_MARKER]
        )]
    }
}

impl FormatReader for TrueHdReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &TRUEHD_FORMAT_INFO
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

        if required_ts.get() < 0 {
            return seek_error(SeekErrorKind::OutOfRange);
        }
        let required = required_ts.get() as u64;

        while self.next_ts <= required {
            if self.read_next()?.is_none() {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        }

        // The state of the decoder is only complete after a major sync, so restart from the last
        // one before the required timestamp.
        let index = self.sync_points.partition_point(|&(ts, _)| ts <= required);
        let (ts, pos) = self.sync_points[index.saturating_sub(1)];

        self.reader.seek(SeekFrom::Start(pos))?;
        self.pending = None;
        self.next_ts = ts;

        Ok(SeekedTo { track_id: 0, actual_ts: Timestamp::new(ts as i64), required_ts })
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        Ok(self.read_next()?.map(|(ts, data)| {
            Packet::new(0, Timestamp::new(ts as i64), Duration::from(self.frames_per_au), data)
        }))
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}
