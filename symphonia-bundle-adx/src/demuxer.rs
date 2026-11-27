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
use symphonia_core::codecs::audio::well_known::CODEC_ID_ADPCM_ADX;
use symphonia_core::errors::{
    Error, Result, SeekErrorKind, decode_error, seek_error, unsupported_error,
};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_ADX;
use symphonia_core::io::{MediaSource, MediaSourceStream, ReadBytes, ScopedStream};
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::support_format;

use crate::decoder::HEADER_PREFIX_LEN;

const ADX_FORMAT_INFO: FormatInfo =
    FormatInfo { format: FORMAT_ID_ADX, short_name: "adx", long_name: "CRI ADX" };

/// The encoding of the standard ADX with a fixed predictor.
const ENCODING_FIXED: u8 = 3;
/// The encoding of the ADX with a fixed predictor and an exponential scale.
const ENCODING_FIXED_2: u8 = 2;

const BLOCK_LEN: u64 = 18;
const BLOCK_SAMPLES: u64 = 32;

/// The number of frames of every packet.
const FRAMES_PER_PACKET: u64 = 64;

fn make_channels(count: u8) -> Channels {
    use symphonia_core::audio::layouts;

    match count {
        1 => layouts::CHANNEL_LAYOUT_MONO,
        2 => layouts::CHANNEL_LAYOUT_STEREO,
        n => Channels::Positioned(Position::from_bits_truncate(u64::MAX >> (64 - u32::from(n)))),
    }
}

/// CRI ADX demuxer.
pub struct AdxReader<'s> {
    reader: MediaSourceStream<'s>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    chapters: Option<ChapterGroup>,
    frame_len: u64,
    total_samples: u64,
    data_start: u64,
    next_ts: u64,
}

impl<'s> AdxReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let mut header = [0u8; HEADER_PREFIX_LEN];
        mss.read_buf_exact(&mut header)?;

        if header[0] != 0x80 || header[1] != 0 {
            return unsupported_error("adx: missing header marker");
        }

        let data_start = u64::from(u16::from_be_bytes([header[2], header[3]])) + 4;
        let (encoding, block_len, bits, channels) = (header[4], header[5], header[6], header[7]);
        let sample_rate = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);
        let total_samples =
            u64::from(u32::from_be_bytes([header[12], header[13], header[14], header[15]]));

        if encoding != ENCODING_FIXED && encoding != ENCODING_FIXED_2 {
            return unsupported_error("adx: unsupported encoding");
        }
        if u64::from(block_len) != BLOCK_LEN || bits != 4 {
            return unsupported_error("adx: unsupported block layout");
        }
        if channels == 0 || sample_rate == 0 || data_start < HEADER_PREFIX_LEN as u64 {
            return decode_error("adx: invalid header");
        }
        if channels > 64 {
            return unsupported_error("adx: too many channels");
        }

        mss.ignore_bytes(data_start - HEADER_PREFIX_LEN as u64)?;

        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_ADPCM_ADX)
            .with_sample_rate(sample_rate)
            .with_channels(make_channels(channels))
            .with_extra_data(Box::from(&header[..]))
            .with_max_frames_per_packet(FRAMES_PER_PACKET * BLOCK_SAMPLES);

        let mut track = Track::new(0);
        track
            .with_codec_params(CodecParameters::Audio(params))
            .with_time_base(
                TimeBase::try_new(1, sample_rate)
                    .ok_or(Error::DecodeError("adx: invalid sample rate"))?,
            )
            .with_num_frames(total_samples)
            .with_duration(Duration::from(total_samples));

        Ok(AdxReader {
            reader: mss,
            media_info: MediaInfo::from_track(&track),
            tracks: vec![track],
            metadata: opts.external_data.metadata.unwrap_or_default(),
            chapters: opts.external_data.chapters,
            frame_len: BLOCK_LEN * u64::from(channels),
            total_samples,
            data_start,
            next_ts: 0,
        })
    }
}

impl Scoreable for AdxReader<'_> {
    fn score(mut src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        let mut head = [0u8; 8];
        src.read_buf_exact(&mut head)?;

        // The marker is only two bytes, so the encoding and the block layout have to match too.
        let valid = head[0] == 0x80
            && head[1] == 0
            && (head[4] == ENCODING_FIXED || head[4] == ENCODING_FIXED_2)
            && head[5] == BLOCK_LEN as u8
            && head[6] == 4
            && head[7] != 0;

        Ok(if valid { Score::Supported(255) } else { Score::Unsupported })
    }
}

impl ProbeableFormat<'_> for AdxReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(AdxReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(ADX_FORMAT_INFO, &["adx"], &["audio/x-adx"], &[&[0x80, 0x00]])]
    }
}

impl FormatReader for AdxReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &ADX_FORMAT_INFO
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

        // The prediction of a block continues from the previous one, so seek to the start of a
        // packet and let the decoder run from there.
        let packet = required_ts.get() as u64 / (FRAMES_PER_PACKET * BLOCK_SAMPLES);
        let frame = packet * FRAMES_PER_PACKET;

        self.reader.seek(SeekFrom::Start(self.data_start + frame * self.frame_len))?;
        self.next_ts = frame * BLOCK_SAMPLES;

        Ok(SeekedTo { track_id: 0, actual_ts: Timestamp::new(self.next_ts as i64), required_ts })
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        if self.next_ts >= self.total_samples {
            return Ok(None);
        }

        let remaining = self.total_samples - self.next_ts;
        let dur = remaining.min(FRAMES_PER_PACKET * BLOCK_SAMPLES);
        let frames = dur.div_ceil(BLOCK_SAMPLES);

        let mut data = vec![0u8; (frames * self.frame_len) as usize];
        let mut len = 0;
        while len < data.len() {
            match self.reader.read_buf(&mut data[len..]) {
                Ok(0) => break,
                Ok(n) => len += n,
                Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(err) => return Err(err.into()),
            }
        }

        let whole = len - len % self.frame_len as usize;
        if whole == 0 {
            return Ok(None);
        }
        data.truncate(whole);

        let dur = dur.min(whole as u64 / self.frame_len * BLOCK_SAMPLES);
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
