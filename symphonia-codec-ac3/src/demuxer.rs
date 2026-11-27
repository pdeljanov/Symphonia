// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{Seek, SeekFrom};

use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::codecs::audio::well_known::{CODEC_ID_AC3, CODEC_ID_EAC3};
use symphonia_core::errors::{Error, Result, SeekErrorKind, seek_error};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_AC3;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::support_format;

use crate::frame::{FrameHeader, MIN_HEADER_LEN, SAMPLES_PER_BLOCK, SAMPLES_PER_FRAME, SYNC_WORD};

const AC3_FORMAT_INFO: FormatInfo = FormatInfo {
    format: FORMAT_ID_AC3,
    short_name: "ac3",
    long_name: "Dolby Digital (AC-3) Elementary Stream",
};

/// The number of header bytes read to determine the length of a frame.
const PEEK_LEN: usize = 256;

/// Reads the frame header at the current position, resynchronizing to the next sync word. On
/// success, the reader is positioned at the start of the frame.
fn sync_to_frame<B: ReadBytes + SeekBuffered>(reader: &mut B) -> Result<FrameHeader> {
    let mut sync = 0u16;

    loop {
        sync = (sync << 8) | u16::from(reader.read_u8()?);

        if sync != SYNC_WORD {
            continue;
        }

        let mut buf = [0u8; PEEK_LEN];
        buf[..2].copy_from_slice(&SYNC_WORD.to_be_bytes());

        let mut len = 2;
        while len < PEEK_LEN {
            match reader.read_u8() {
                Ok(b) => buf[len] = b,
                Err(_) => break,
            }
            len += 1;
        }

        // Rewind to the start of the candidate frame.
        reader.seek_buffered_rev(len);

        if len >= MIN_HEADER_LEN + 3 {
            if let Ok(header) = FrameHeader::parse(&buf[..len]) {
                return Ok(header);
            }
        }

        // Not a frame, continue searching after the false sync word.
        reader.ignore_bytes(2)?;
        sync = 0;
    }
}

/// A reader of raw AC-3 elementary streams.
pub struct Ac3Reader<'s> {
    reader: MediaSourceStream<'s>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    chapters: Option<ChapterGroup>,
    metadata: MetadataLog,
    first_frame_pos: u64,
    next_packet_ts: Timestamp,
}

impl<'s> Ac3Reader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let header = sync_to_frame(&mut mss)?;
        let first_frame_pos = mss.pos();

        let mut codec_params = AudioCodecParameters::new();
        codec_params
            .for_codec(if header.eac3.is_some() { CODEC_ID_EAC3 } else { CODEC_ID_AC3 })
            .with_sample_rate(header.sample_rate)
            .with_channels(header.channels())
            .with_bits_per_coded_sample(0)
            .with_max_frames_per_packet(SAMPLES_PER_FRAME as u64);

        let mut track = Track::new(0);
        track.with_codec_params(CodecParameters::Audio(codec_params));

        if let Some(len) = mss.byte_len() {
            let num_frames = (len.saturating_sub(first_frame_pos) / header.frame_len as u64)
                * SAMPLES_PER_FRAME as u64;
            track.with_num_frames(num_frames);
            track.with_duration(Duration::from(num_frames));
        }

        Ok(Ac3Reader {
            reader: mss,
            media_info: MediaInfo::from_track(&track),
            tracks: vec![track],
            chapters: opts.external_data.chapters,
            metadata: opts.external_data.metadata.unwrap_or_default(),
            first_frame_pos,
            next_packet_ts: Timestamp::new(0),
        })
    }
}

impl Scoreable for Ac3Reader<'_> {
    fn score(mut src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        // A raw stream starts with a frame, and is followed by another one unless it is a single
        // frame stream.
        let mut head = [0u8; PEEK_LEN];
        let mut len = 0;
        while len < PEEK_LEN {
            match src.read_u8() {
                Ok(b) => head[len] = b,
                Err(_) => break,
            }
            len += 1;
        }
        let head = &head[..len];

        if len < MIN_HEADER_LEN + 3 {
            return Ok(Score::Unsupported);
        }

        if u16::from_be_bytes([head[0], head[1]]) != SYNC_WORD {
            return Ok(Score::Unsupported);
        }

        let Ok(header) = FrameHeader::parse(head)
        else {
            return Ok(Score::Unsupported);
        };

        src.ignore_bytes((header.frame_len - len.min(header.frame_len)) as u64)?;

        match src.read_be_u16() {
            Ok(SYNC_WORD) => Ok(Score::Supported(255)),
            Err(_) => Ok(Score::Supported(64)),
            Ok(_) => Ok(Score::Unsupported),
        }
    }
}

impl ProbeableFormat<'_> for Ac3Reader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(Ac3Reader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(
            AC3_FORMAT_INFO,
            &["ac3"],
            &["audio/ac3", "audio/vnd.dolby.dd-raw"],
            &[&[0x0b, 0x77]]
        )]
    }
}

impl FormatReader for Ac3Reader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &AC3_FORMAT_INFO
    }

    fn media_info(&self) -> &MediaInfo {
        &self.media_info
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        let header = match sync_to_frame(&mut self.reader) {
            Ok(header) => header,
            Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(None);
            }
            Err(err) => return Err(err),
        };

        let mut data = Vec::with_capacity(header.frame_len);
        let mut frame_len = header.frame_len;

        // A packet is a frame with all the substreams that are decoded along with it.
        loop {
            let start = data.len();
            data.resize(start + frame_len, 0);
            match self.reader.read_buf_exact(&mut data[start..]) {
                Ok(()) => (),
                Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(err) => return Err(err.into()),
            }

            match sync_to_frame(&mut self.reader) {
                Ok(next) if next.is_additional_substream() => frame_len = next.frame_len,
                _ => break,
            }
        }

        let duration = Duration::from((header.num_blocks * SAMPLES_PER_BLOCK) as u64);
        let ts = self.next_packet_ts;

        self.next_packet_ts = match ts.checked_add(duration) {
            Some(ts) => ts,
            None => return Ok(None),
        };

        Ok(Some(Packet::new(0, ts, duration, data.into_boxed_slice())))
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
            let header = match sync_to_frame(&mut self.reader) {
                Ok(header) => header,
                Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return seek_error(SeekErrorKind::OutOfRange);
                }
                Err(err) => return Err(err),
            };

            let duration = Duration::from((header.num_blocks * SAMPLES_PER_BLOCK) as u64);

            match self.next_packet_ts.checked_add(duration) {
                Some(ts) if ts <= required_ts => {
                    self.reader.ignore_bytes(header.frame_len as u64)?;
                    self.next_packet_ts = ts;

                    // Skip the substreams that belong to the frame.
                    while let Ok(next) = sync_to_frame(&mut self.reader) {
                        if !next.is_additional_substream() {
                            break;
                        }
                        self.reader.ignore_bytes(next.frame_len as u64)?;
                    }
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
