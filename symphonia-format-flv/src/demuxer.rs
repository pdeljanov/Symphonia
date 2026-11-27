// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{ErrorKind, Seek, SeekFrom};

use symphonia_core::audio::layouts::{CHANNEL_LAYOUT_MONO, CHANNEL_LAYOUT_STEREO};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::codecs::audio::well_known::{
    CODEC_ID_AAC, CODEC_ID_ADPCM_SWF, CODEC_ID_MP3, CODEC_ID_NELLYMOSER, CODEC_ID_PCM_ALAW,
    CODEC_ID_PCM_MULAW, CODEC_ID_PCM_S8, CODEC_ID_PCM_S16LE, CODEC_ID_PCM_U8,
};
use symphonia_core::errors::{
    Error, Result, SeekErrorKind, decode_error, seek_error, unsupported_error,
};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_FLV;
use symphonia_core::io::{MediaSource, MediaSourceStream, ReadBytes, ScopedStream};
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::support_format;

use symphonia_common::mpeg::audio::AudioSpecificConfig;

const FLV_FORMAT_INFO: FormatInfo =
    FormatInfo { format: FORMAT_ID_FLV, short_name: "flv", long_name: "Flash Video" };

const TAG_AUDIO: u8 = 8;

const FORMAT_PCM_BE: u8 = 0;
const FORMAT_ADPCM: u8 = 1;
const FORMAT_NELLYMOSER_16K: u8 = 4;
const FORMAT_NELLYMOSER_8K: u8 = 5;
const FORMAT_NELLYMOSER: u8 = 6;
const FORMAT_PCM_LE: u8 = 3;
const FORMAT_MP3: u8 = 2;
const FORMAT_MP3_8K: u8 = 14;
const FORMAT_ALAW: u8 = 7;
const FORMAT_MULAW: u8 = 8;
const FORMAT_AAC: u8 = 10;

const SAMPLE_RATES: [u32; 4] = [5512, 11025, 22050, 44100];

/// The longest tag that is read.
const MAX_TAG_LEN: usize = 16 * 1024 * 1024;

struct Tag {
    kind: u8,
    len: usize,
}

fn read_tag_header(reader: &mut MediaSourceStream<'_>) -> Result<Option<Tag>> {
    // The size of the previous tag precedes every tag.
    match reader.read_be_u32() {
        Ok(_) => (),
        Err(err) if err.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err.into()),
    }

    let mut head = [0u8; 11];
    match reader.read_buf_exact(&mut head) {
        Ok(()) => (),
        Err(err) if err.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err.into()),
    }

    let len = (usize::from(head[1]) << 16) | (usize::from(head[2]) << 8) | usize::from(head[3]);
    if len > MAX_TAG_LEN {
        return decode_error("flv: tag is too long");
    }

    Ok(Some(Tag { kind: head[0] & 0x1f, len }))
}

/// The number of samples that a frame of an MP3 stream decodes to, given its header.
fn mp3_frame_samples(header: &[u8]) -> u64 {
    if header.len() < 4 || header[0] != 0xff || header[1] & 0xe0 != 0xe0 {
        return 1152;
    }
    let version = (header[1] >> 3) & 3;
    let layer = (header[1] >> 1) & 3;
    match (version, layer) {
        (_, 3) => 384,
        (_, 2) => 1152,
        (3, _) => 1152,
        _ => 576,
    }
}

/// Flash Video (FLV) demuxer.
pub struct FlvReader<'s> {
    reader: MediaSourceStream<'s>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    chapters: Option<ChapterGroup>,
    sound_format: u8,
    channels: u64,
    bytes_per_sample: u64,
    /// The position of the first tag, which is that of the size of the tag before it.
    first_tag_pos: u64,
    next_ts: u64,
    /// The first packet, which was read to find the codec.
    pending: Option<(Box<[u8]>, u64)>,
}

impl<'s> FlvReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let mut header = [0u8; 9];
        mss.read_buf_exact(&mut header)?;

        if &header[..3] != b"FLV" || header[3] != 1 {
            return unsupported_error("flv: missing signature");
        }
        if header[4] & 4 == 0 {
            return unsupported_error("flv: no audio");
        }

        let offset = u64::from(u32::from_be_bytes([header[5], header[6], header[7], header[8]]));
        if offset < 9 {
            return decode_error("flv: invalid header");
        }
        mss.ignore_bytes(offset - 9)?;
        let first_tag_pos = mss.pos();

        let mut params = AudioCodecParameters::new();
        let sound_format;
        let channels;
        let mut bytes_per_sample;
        let pending;

        // The codec is described by the first audio tag, and AAC by the tag after it.
        loop {
            let Some(tag) = read_tag_header(&mut mss)?
            else {
                return unsupported_error("flv: no audio");
            };

            if tag.kind != TAG_AUDIO || tag.len < 2 {
                mss.ignore_bytes(tag.len as u64)?;
                continue;
            }

            let body = mss.read_boxed_slice_exact(tag.len)?;
            let flags = body[0];
            let format = flags >> 4;
            let rate = SAMPLE_RATES[usize::from((flags >> 2) & 3)];
            let stereo = flags & 1 != 0;
            let wide = flags & 2 != 0;
            let mut count = if stereo { 2 } else { 1 };
            bytes_per_sample = if wide { 2 } else { 1 };
            let layout = if stereo { CHANNEL_LAYOUT_STEREO } else { CHANNEL_LAYOUT_MONO };

            match format {
                FORMAT_AAC => {
                    // The first packet holds the audio specific configuration.
                    if body[1] != 0 {
                        continue;
                    }
                    let config = body[2..].to_vec();
                    let asc = AudioSpecificConfig::read(&config)?;
                    params.for_codec(CODEC_ID_AAC).with_sample_rate(asc.sample_rate);
                    if let Some(c) = asc.channels {
                        count = c.count() as u64;
                        params.with_channels(c);
                    }
                    params.with_extra_data(config.into_boxed_slice());
                }
                FORMAT_MP3 | FORMAT_MP3_8K => {
                    let rate = if format == FORMAT_MP3_8K { 8000 } else { rate };
                    params.for_codec(CODEC_ID_MP3).with_sample_rate(rate).with_channels(layout);
                }
                FORMAT_PCM_LE | FORMAT_PCM_BE => {
                    let codec = match (format, wide) {
                        (FORMAT_PCM_LE, true) => CODEC_ID_PCM_S16LE,
                        (FORMAT_PCM_LE, false) => CODEC_ID_PCM_U8,
                        (_, false) => CODEC_ID_PCM_S8,
                        _ => return unsupported_error("flv: big endian PCM is not supported"),
                    };
                    params
                        .for_codec(codec)
                        .with_sample_rate(rate)
                        .with_channels(layout)
                        .with_bits_per_sample(bytes_per_sample as u32 * 8)
                        .with_bits_per_coded_sample(bytes_per_sample as u32 * 8);
                }
                FORMAT_ADPCM => {
                    params
                        .for_codec(CODEC_ID_ADPCM_SWF)
                        .with_sample_rate(rate)
                        .with_channels(layout);
                }
                FORMAT_NELLYMOSER | FORMAT_NELLYMOSER_8K | FORMAT_NELLYMOSER_16K => {
                    let rate = match format {
                        FORMAT_NELLYMOSER_8K => 8000,
                        FORMAT_NELLYMOSER_16K => 16000,
                        _ => rate,
                    };
                    params
                        .for_codec(CODEC_ID_NELLYMOSER)
                        .with_sample_rate(rate)
                        .with_channels(CHANNEL_LAYOUT_MONO);
                    count = 1;
                    bytes_per_sample = 1;
                }
                FORMAT_ALAW | FORMAT_MULAW => {
                    let codec =
                        if format == FORMAT_ALAW { CODEC_ID_PCM_ALAW } else { CODEC_ID_PCM_MULAW };
                    params.for_codec(codec).with_sample_rate(rate).with_channels(layout);
                    bytes_per_sample = 1;
                }
                _ => return unsupported_error("flv: unsupported audio codec"),
            }

            sound_format = format;
            channels = count;
            pending = audio_packet(&body, format, count * bytes_per_sample, count as usize);
            break;
        }

        let rate = params.sample_rate.ok_or(Error::DecodeError("flv: missing sample rate"))?;
        params.with_max_frames_per_packet(2048);

        let mut track = Track::new(0);
        track.with_codec_params(CodecParameters::Audio(params)).with_time_base(
            TimeBase::try_new(1, rate).ok_or(Error::DecodeError("flv: invalid sample rate"))?,
        );

        let reader = FlvReader {
            reader: mss,
            media_info: MediaInfo::from_track(&track),
            tracks: vec![track],
            metadata: opts.external_data.metadata.unwrap_or_default(),
            chapters: opts.external_data.chapters,
            sound_format,
            channels,
            bytes_per_sample,
            first_tag_pos,
            next_ts: 0,
            pending,
        };

        Ok(reader)
    }

    fn rewind(&mut self) -> Result<()> {
        self.reader.seek(SeekFrom::Start(self.first_tag_pos))?;
        self.pending = None;
        self.next_ts = 0;
        Ok(())
    }

    /// Reads the next packet of audio, and returns its data and the number of samples.
    fn read_audio(&mut self) -> Result<Option<(Box<[u8]>, u64)>> {
        if let Some(packet) = self.pending.take() {
            return Ok(Some(packet));
        }

        loop {
            let Some(tag) = read_tag_header(&mut self.reader)?
            else {
                return Ok(None);
            };

            if tag.kind != TAG_AUDIO || tag.len < 1 {
                self.reader.ignore_bytes(tag.len as u64)?;
                continue;
            }

            let body = self.reader.read_boxed_slice_exact(tag.len)?;

            if let Some(packet) = audio_packet(
                &body,
                self.sound_format,
                self.channels * self.bytes_per_sample,
                self.channels as usize,
            ) {
                return Ok(Some(packet));
            }
        }
    }
}

/// The number of samples of a channel in a packet of Shockwave Flash ADPCM, which holds blocks of up
/// to 4096 samples, each with a header of 2 bits and 22 bits for every channel.
fn swf_adpcm_samples(data: &[u8], channels: usize) -> u64 {
    let total = data.len() * 8;
    let (mut pos, mut samples) = (0, 0);

    while total - pos >= 2 + channels * 22 {
        let bit = |at: usize| usize::from((data[at / 8] >> (7 - at % 8)) & 1);
        let code_size = (bit(pos) << 1 | bit(pos + 1)) + 2;
        pos += 2 + channels * 22;
        let coded = ((total - pos) / (channels * code_size)).min(4095);
        pos += coded * channels * code_size;
        samples += 1 + coded as u64;
    }

    samples
}

/// Extracts the data of a packet and its number of samples from the body of an audio tag. Returns
/// `None` if the tag does not hold a packet of the stream.
fn audio_packet(
    body: &[u8],
    sound_format: u8,
    frame_len: u64,
    channels: usize,
) -> Option<(Box<[u8]>, u64)> {
    if body[0] >> 4 != sound_format {
        return None;
    }

    match sound_format {
        FORMAT_AAC => {
            // The packet types other than 1 are sequence headers.
            if body.len() < 2 || body[1] != 1 {
                return None;
            }
            Some((body[2..].into(), 1024))
        }
        FORMAT_MP3 | FORMAT_MP3_8K => Some((body[1..].into(), mp3_frame_samples(&body[1..]))),
        FORMAT_NELLYMOSER | FORMAT_NELLYMOSER_8K | FORMAT_NELLYMOSER_16K => {
            Some((body[1..].into(), (body.len() as u64 - 1) / 64 * 256))
        }
        FORMAT_ADPCM => Some((body[1..].into(), swf_adpcm_samples(&body[1..], channels))),
        _ => Some((body[1..].into(), (body.len() as u64 - 1) / frame_len.max(1))),
    }
}

impl Scoreable for FlvReader<'_> {
    fn score(mut src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        let mut header = [0u8; 9];
        src.read_buf_exact(&mut header)?;

        Ok(if &header[..3] == b"FLV" && header[3] == 1 && header[5..9] == [0, 0, 0, 9] {
            Score::Supported(255)
        }
        else {
            Score::Unsupported
        })
    }
}

impl ProbeableFormat<'_> for FlvReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(FlvReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(
            FLV_FORMAT_INFO,
            &["flv"],
            &["video/x-flv", "audio/x-flv"],
            &[b"FLV\x01"]
        )]
    }
}

impl FormatReader for FlvReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &FLV_FORMAT_INFO
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
        let required_ts = match to {
            SeekTo::Timestamp { ts, .. } => ts,
            SeekTo::Time { time, .. } => {
                let tb =
                    self.tracks[0].time_base.ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
                tb.calc_timestamp(time).ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?
            }
        };
        let required = u64::try_from(required_ts.get())
            .map_err(|_| Error::SeekError(SeekErrorKind::OutOfRange))?;

        if required < self.next_ts {
            if !self.reader.is_seekable() {
                return seek_error(SeekErrorKind::ForwardOnly);
            }
            self.rewind()?;
        }

        // There is no index, so read the tags up to the one that holds the sample.
        loop {
            let Some((data, samples)) = self.read_audio()?
            else {
                return seek_error(SeekErrorKind::OutOfRange);
            };

            if required < self.next_ts + samples {
                self.pending = Some((data, samples));
                break;
            }
            self.next_ts += samples;
        }

        Ok(SeekedTo { track_id: 0, actual_ts: Timestamp::new(self.next_ts as i64), required_ts })
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        let Some((data, samples)) = self.read_audio()?
        else {
            return Ok(None);
        };

        let ts = self.next_ts;
        self.next_ts += samples;

        Ok(Some(Packet::new(0, Timestamp::new(ts as i64), Duration::from(samples), data)))
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn tag(kind: u8, ts: u32, body: &[u8]) -> Vec<u8> {
        let len = body.len() as u32;
        let mut out = vec![kind];
        out.extend_from_slice(&len.to_be_bytes()[1..]);
        out.extend_from_slice(&ts.to_be_bytes()[1..]);
        out.push((ts >> 24) as u8);
        out.extend_from_slice(&[0, 0, 0]);
        out.extend_from_slice(body);
        out.extend_from_slice(&(11 + len).to_be_bytes());
        out
    }

    /// A file with a video tag and tags of 16-bit mono PCM of 8 samples each.
    fn file(tags: usize) -> Vec<u8> {
        let mut out = b"FLV\x01\x05\0\0\0\x09\0\0\0\0".to_vec();
        out.extend(tag(9, 0, &[0x17, 0, 0, 0, 0]));
        for i in 0..tags {
            // Little endian PCM, 22 kHz, 16 bits, mono.
            let mut body = vec![0x3a];
            body.extend((0..8).flat_map(|n| ((i * 8 + n) as i16).to_le_bytes()));
            out.extend(tag(TAG_AUDIO, i as u32, &body));
        }
        out
    }

    fn reader(tags: usize) -> FlvReader<'static> {
        let mss = MediaSourceStream::new(Box::new(Cursor::new(file(tags))), Default::default());
        FlvReader::try_new(mss, FormatOptions::default()).unwrap()
    }

    #[test]
    fn reads_pcm_packets() {
        let mut reader = reader(3);

        let Some(CodecParameters::Audio(params)) = reader.tracks()[0].codec_params.clone()
        else {
            panic!("not an audio track");
        };
        assert_eq!(params.codec, CODEC_ID_PCM_S16LE);
        assert_eq!(params.sample_rate, Some(22050));

        for i in 0..3 {
            let packet = reader.next_packet().unwrap().unwrap();
            assert_eq!(packet.pts.get(), i * 8);
            assert_eq!(packet.dur.get(), 8);
            assert_eq!(packet.data.len(), 16);
            assert_eq!(i16::from_le_bytes([packet.data[0], packet.data[1]]) as i64, i * 8);
        }
        assert!(reader.next_packet().unwrap().is_none());
    }

    #[test]
    fn seeks_to_the_tag_with_the_sample() {
        let mut reader = reader(4);
        let to = SeekTo::Timestamp { ts: Timestamp::new(19), track_id: 0 };
        let seeked = reader.seek(SeekMode::Accurate, to).unwrap();
        assert_eq!(seeked.actual_ts.get(), 16);

        let packet = reader.next_packet().unwrap().unwrap();
        assert_eq!(packet.pts.get(), 16);

        // And back to the start.
        let to = SeekTo::Timestamp { ts: Timestamp::new(0), track_id: 0 };
        assert_eq!(reader.seek(SeekMode::Accurate, to).unwrap().actual_ts.get(), 0);
        assert_eq!(reader.next_packet().unwrap().unwrap().pts.get(), 0);
    }

    #[test]
    fn rejects_other_files() {
        let mss = MediaSourceStream::new(Box::new(Cursor::new(vec![0u8; 64])), Default::default());
        assert!(FlvReader::try_new(mss, FormatOptions::default()).is_err());
    }
}
