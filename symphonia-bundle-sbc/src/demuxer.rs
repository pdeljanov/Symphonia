// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{Seek, SeekFrom};

use symphonia_core::audio::layouts;
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::codecs::audio::well_known::CODEC_ID_SBC;
use symphonia_core::errors::{Error, Result, SeekErrorKind, seek_error};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_SBC;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::support_format;

use crate::frame::{ChannelMode, FrameHeader, HEADER_LEN, MAX_FRAME_LEN, SYNC_BYTE};

const SBC_FORMAT_INFO: FormatInfo =
    FormatInfo { format: FORMAT_ID_SBC, short_name: "sbc", long_name: "SBC (Bluetooth A2DP)" };

// The second header byte holds the sample rate, the blocks, and the mode, so every value of it may
// follow the sync byte.
static MARKER_BYTES: [[u8; 2]; 256] = {
    let mut table = [[SYNC_BYTE, 0]; 256];
    let mut i = 0;
    while i < 256 {
        table[i][1] = i as u8;
        i += 1;
    }
    table
};

static MARKERS: [&[u8]; 256] = {
    let mut table: [&[u8]; 256] = [&[]; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = &MARKER_BYTES[i];
        i += 1;
    }
    table
};

static PROBE_DATA: [ProbeFormatData; 1] =
    [support_format!(SBC_FORMAT_INFO, &["sbc"], &["audio/x-sbc", "audio/sbc"], &MARKERS)];

/// Reads until the buffer is full or the stream ends, and returns the number of bytes read.
fn fill<B: ReadBytes>(reader: &mut B, buf: &mut [u8]) -> usize {
    let mut len = 0;
    while len < buf.len() {
        match reader.read_buf(&mut buf[len..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => len += n,
        }
    }
    len
}

/// Reads the frame header at the current position, resynchronizing to the next valid frame. On
/// success, the reader is positioned at the start of the frame.
fn sync_to_frame<B: ReadBytes + SeekBuffered>(reader: &mut B) -> Result<FrameHeader> {
    loop {
        if reader.read_u8()? != SYNC_BYTE {
            continue;
        }

        let mut buf = [0u8; MAX_FRAME_LEN];
        buf[0] = SYNC_BYTE;
        let len = 1 + fill(reader, &mut buf[1..]);

        // Rewind to the start of the candidate frame.
        reader.seek_buffered_rev(len);

        if let Some(header) = FrameHeader::parse(&buf[..len]) {
            if header.check_crc(&buf[..len]) {
                return Ok(header);
            }
        }

        // Not a frame, continue searching after the false sync byte.
        reader.ignore_bytes(1)?;
    }
}

/// A reader of raw SBC streams.
pub struct SbcReader<'s> {
    reader: MediaSourceStream<'s>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    chapters: Option<ChapterGroup>,
    metadata: MetadataLog,
    first_frame_pos: u64,
    next_packet_ts: Timestamp,
}

impl<'s> SbcReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let header = sync_to_frame(&mut mss)?;
        let first_frame_pos = mss.pos();

        let mut codec_params = AudioCodecParameters::new();
        codec_params
            .for_codec(CODEC_ID_SBC)
            .with_sample_rate(header.sample_rate)
            .with_channels(if header.mode == ChannelMode::Mono {
                layouts::CHANNEL_LAYOUT_MONO
            }
            else {
                layouts::CHANNEL_LAYOUT_STEREO
            })
            .with_max_frames_per_packet(header.samples_per_frame() as u64);

        let mut track = Track::new(0);
        track.with_codec_params(CodecParameters::Audio(codec_params));

        if let Some(len) = mss.byte_len() {
            let num_frames = (len.saturating_sub(first_frame_pos) / header.frame_len as u64)
                * header.samples_per_frame() as u64;
            track.with_num_frames(num_frames);
            track.with_duration(Duration::from(num_frames));
        }

        if let Some(time_base) = TimeBase::try_new(1, header.sample_rate) {
            track.with_time_base(time_base);
        }

        Ok(SbcReader {
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

impl Scoreable for SbcReader<'_> {
    fn score(mut src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        let mut buf = [0u8; 2 * MAX_FRAME_LEN];
        let len = fill(&mut src, &mut buf);
        let buf = &buf[..len];

        let Some(header) = FrameHeader::parse(buf)
        else {
            return Ok(Score::Unsupported);
        };
        if !header.check_crc(buf) {
            return Ok(Score::Unsupported);
        }

        // A raw stream is followed by another frame unless it is a single frame stream.
        let next = &buf[header.frame_len..];
        if next.len() < HEADER_LEN {
            return Ok(Score::Supported(64));
        }

        match FrameHeader::parse(next) {
            Some(next) if next.sample_rate == header.sample_rate => Ok(Score::Supported(255)),
            _ => Ok(Score::Unsupported),
        }
    }
}

impl ProbeableFormat<'_> for SbcReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(SbcReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &PROBE_DATA
    }
}

impl FormatReader for SbcReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &SBC_FORMAT_INFO
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

        let mut data = vec![0u8; header.frame_len];
        match self.reader.read_buf_exact(&mut data) {
            Ok(()) => (),
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(err) => return Err(err.into()),
        }

        let duration = Duration::from(header.samples_per_frame() as u64);
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

            let duration = Duration::from(header.samples_per_frame() as u64);

            match self.next_packet_ts.checked_add(duration) {
                Some(ts) if ts <= required_ts => {
                    self.reader.ignore_bytes(header.frame_len as u64)?;
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
