// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{Seek, SeekFrom};

use symphonia_core::errors::{Error, SeekErrorKind, seek_error};
use symphonia_core::support_format;

use crate::header::{SYNC_14_BE, SYNC_14_LE, SYNC_16_BE, SYNC_16_LE, peek_header};

use symphonia_core::audio::Channels;
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::codecs::audio::well_known::CODEC_ID_DCA;
use symphonia_core::errors::Result;
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_DCA;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};

const DCA_FORMAT_INFO: FormatInfo =
    FormatInfo { format: FORMAT_ID_DCA, short_name: "dca", long_name: "DTS Coherent Acoustics" };

pub struct DcaReader<'s> {
    reader: MediaSourceStream<'s>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    chapters: Option<ChapterGroup>,
    metadata: MetadataLog,
    first_frame_pos: u64,
    next_packet_ts: Timestamp,
}

impl<'s> DcaReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let header = DcaHeader::read(&mut mss)?;

        // Rewind back to the start of the frame.
        mss.seek_buffered_rev(header.header_len as usize);
        let first_frame_pos = mss.pos();

        let mut codec_params = AudioCodecParameters::new();
        codec_params.for_codec(CODEC_ID_DCA).with_sample_rate(header.sample_rate);

        codec_params.with_channels(header.channels.clone()).with_max_frames_per_packet(4096);

        let mut track = Track::new(0);
        track.with_codec_params(CodecParameters::Audio(codec_params));

        if let Some(len) = mss.byte_len() {
            let frames = len.saturating_sub(first_frame_pos) / u64::from(header.frame_len);
            let num_frames = frames * u64::from(header.samples_per_frame);
            track.with_num_frames(num_frames);
            track.with_duration(Duration::from(num_frames));
        }

        let media_info = MediaInfo::from_track(&track);

        Ok(DcaReader {
            reader: mss,
            media_info,
            tracks: vec![track],
            chapters: opts.external_data.chapters,
            metadata: opts.external_data.metadata.unwrap_or_default(),
            first_frame_pos,
            next_packet_ts: Timestamp::new(0),
        })
    }
}

impl Scoreable for DcaReader<'_> {
    fn score(mut src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        // A raw stream starts with a core frame, which is followed by another one unless extension
        // data or padding is in between.
        let mut first = [0u8; 4];
        src.read_buf_exact(&mut first)?;
        if !is_sync(u32::from_be_bytes(first)) {
            return Ok(Score::Unsupported);
        }
        src.seek_buffered_rev(first.len());

        let header = DcaHeader::read(&mut src)?;
        src.seek_buffered_rev(header.header_len as usize);
        src.ignore_bytes(u64::from(header.frame_len))?;

        let mut next = [0u8; 4];
        match src.read_buf_exact(&mut next) {
            Ok(()) if is_sync(u32::from_be_bytes(next)) => Ok(Score::Supported(255)),
            _ => Ok(Score::Supported(128)),
        }
    }
}

fn is_sync(word: u32) -> bool {
    matches!(word, SYNC_16_BE | SYNC_16_LE | SYNC_14_BE | SYNC_14_LE)
}

const HEADER_PEEK: usize = 16;

pub struct DcaHeader {
    pub sample_rate: u32,
    pub channels: Channels,
    pub frame_len: u32,
    pub header_len: u32,
    pub samples_per_frame: u32,
}

impl DcaHeader {
    pub fn read<B: ReadBytes>(reader: &mut B) -> Result<Self> {
        let mut sync = 0u32;

        loop {
            sync = (sync << 8) | u32::from(reader.read_u8()?);
            if matches!(sync, SYNC_16_BE | SYNC_16_LE | SYNC_14_BE | SYNC_14_LE) {
                break;
            }
        }

        let mut buf = [0u8; HEADER_PEEK];
        buf[..4].copy_from_slice(&sync.to_be_bytes());
        reader.read_buf_exact(&mut buf[4..])?;

        let (hdr, stored_len) = peek_header(&buf)?;

        Ok(DcaHeader {
            sample_rate: hdr.sample_rate,
            channels: hdr.channels()?,
            frame_len: stored_len as u32,
            header_len: HEADER_PEEK as u32,
            samples_per_frame: hdr.samples_per_frame() as u32,
        })
    }
}

impl ProbeableFormat<'_> for DcaReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(DcaReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(
            DCA_FORMAT_INFO,
            &["dts", "dca"],
            &["audio/dts", "audio/vnd.dts"],
            &[&[0x7f, 0xfe, 0x80, 0x01]]
        )]
    }
}

impl FormatReader for DcaReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &DCA_FORMAT_INFO
    }
    fn media_info(&self) -> &MediaInfo {
        &self.media_info
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        let header = match DcaHeader::read(&mut self.reader) {
            Ok(header) => header,
            Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(None);
            }
            Err(err) => return Err(err),
        };

        // Rewind back to the start of the frame (including sync word).
        self.reader.seek_buffered_rev(header.header_len as usize);

        let ts = self.next_packet_ts;
        let duration = Duration::from(u64::from(header.samples_per_frame));

        self.next_packet_ts = match self.next_packet_ts.checked_add(duration) {
            Some(ts) => ts,
            None => return Ok(None),
        };

        Ok(Some(Packet::new(
            0,
            ts,
            duration,
            self.reader.read_boxed_slice_exact(header.frame_len as usize)?,
        )))
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn chapters(&self) -> Option<&ChapterGroup> {
        self.chapters.as_ref()
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        let required_ts = match to {
            SeekTo::Timestamp { ts, .. } => ts,
            SeekTo::Time { time, .. } => {
                let tb =
                    self.tracks[0].time_base.ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
                tb.calc_timestamp(time).ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?
            }
        };

        if required_ts < self.next_packet_ts {
            if !self.reader.is_seekable() {
                return seek_error(SeekErrorKind::ForwardOnly);
            }

            let pos = self.reader.seek(SeekFrom::Start(self.first_frame_pos))?;
            if pos != self.first_frame_pos {
                return seek_error(SeekErrorKind::Unseekable);
            }

            self.next_packet_ts = Timestamp::from(0);
        }

        loop {
            let header = match DcaHeader::read(&mut self.reader) {
                Ok(header) => header,
                Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return seek_error(SeekErrorKind::OutOfRange);
                }
                Err(err) => return Err(err),
            };
            self.reader.seek_buffered_rev(header.header_len as usize);

            let duration = Duration::from(u64::from(header.samples_per_frame));

            match self.next_packet_ts.checked_add(duration) {
                Some(ts) if ts <= required_ts => {
                    self.reader.ignore_bytes(u64::from(header.frame_len))?;
                    self.next_packet_ts = ts;
                }
                _ => break,
            }
        }

        Ok(SeekedTo { track_id: 0, required_ts, actual_ts: self.next_packet_ts })
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}
