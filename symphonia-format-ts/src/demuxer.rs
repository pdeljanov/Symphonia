// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{ErrorKind, Seek, SeekFrom};

use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia_core::errors::{
    Error, Result, SeekErrorKind, decode_error, seek_error, unsupported_error,
};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_TS;
use symphonia_core::io::{MediaSource, MediaSourceStream, ReadBytes, ScopedStream, SeekBuffered};
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::support_format;

use symphonia_common::frames::{AudioFrame, parse_ac3, parse_dts, parse_mpeg_audio};
use symphonia_common::mpeg::audio::AudioSpecificConfig;

const TS_FORMAT_INFO: FormatInfo =
    FormatInfo { format: FORMAT_ID_TS, short_name: "ts", long_name: "MPEG Transport Stream" };

const PACKET_LEN: usize = 188;
const SYNC_BYTE: u8 = 0x47;

/// The number of packets that are searched for the tables and the first frame of the stream.
const MAX_PROBE_PACKETS: usize = 50_000;

/// The kind of audio stream that is read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Adts,
    Mpeg,
    Ac3,
    Dts,
}

fn kind_of_stream_type(stream_type: u8, descriptors: &[u8]) -> Option<Kind> {
    match stream_type {
        0x0f => Some(Kind::Adts),
        0x03 | 0x04 => Some(Kind::Mpeg),
        0x81 => Some(Kind::Ac3),
        0x82 | 0x85 | 0x8a => Some(Kind::Dts),
        // Private data identifies the codec by a descriptor.
        0x06 => {
            let mut rest = descriptors;
            while rest.len() >= 2 {
                let (tag, len) = (rest[0], usize::from(rest[1]));
                if rest.len() < 2 + len {
                    break;
                }
                match tag {
                    0x6a => return Some(Kind::Ac3),
                    0x7b => return Some(Kind::Dts),
                    _ => (),
                }
                rest = &rest[2 + len..];
            }
            None
        }
        _ => None,
    }
}

/// The header of an ADTS frame: the length of the frame and of its header, and the audio specific
/// config of the stream.
struct AdtsFrame {
    len: usize,
    header_len: usize,
    config: [u8; 2],
    samples: u64,
}

fn parse_adts(buf: &[u8]) -> Option<AdtsFrame> {
    if buf.len() < 7 || buf[0] != 0xff || buf[1] & 0xf6 != 0xf0 {
        return None;
    }

    let protection_absent = buf[1] & 1 != 0;
    let profile = (buf[2] >> 6) & 3;
    let sfi = (buf[2] >> 2) & 0xf;
    let channels = ((buf[2] & 1) << 2) | (buf[3] >> 6);
    let len =
        (usize::from(buf[3] & 3) << 11) | (usize::from(buf[4]) << 3) | usize::from(buf[5] >> 5);
    let blocks = u64::from(buf[6] & 3) + 1;
    let header_len = if protection_absent { 7 } else { 9 };

    if sfi >= 13 || len < header_len {
        return None;
    }

    let object_type = u16::from(profile) + 1;
    let config =
        ((object_type << 11) | (u16::from(sfi) << 7) | (u16::from(channels) << 3)).to_be_bytes();

    Some(AdtsFrame { len, header_len, config, samples: 1024 * blocks })
}

/// A parsed frame of the stream: its bytes in the stream, the packet data, and the number of
/// samples.
struct Frame {
    len: usize,
    data_range: (usize, usize),
    samples: u64,
}

fn parse_frame(buf: &[u8], kind: Kind) -> Option<(Frame, Option<AudioFrame>)> {
    match kind {
        Kind::Adts => {
            let f = parse_adts(buf)?;
            Some((
                Frame { len: f.len, data_range: (f.header_len, f.len), samples: f.samples },
                None,
            ))
        }
        Kind::Mpeg => {
            if buf.len() < 4 {
                return None;
            }
            let f = parse_mpeg_audio(u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]))?;
            Some((
                Frame { len: f.len, data_range: (0, f.len), samples: f.samples_per_frame },
                Some(f),
            ))
        }
        Kind::Ac3 => {
            if buf.len() < 16 {
                return None;
            }
            let f = parse_ac3(buf)?;
            Some((
                Frame { len: f.len, data_range: (0, f.len), samples: f.samples_per_frame },
                Some(f),
            ))
        }
        Kind::Dts => {
            if buf.len() < 16 {
                return None;
            }
            let f = parse_dts(buf)?;
            Some((
                Frame { len: f.len, data_range: (0, f.len), samples: f.samples_per_frame },
                Some(f),
            ))
        }
    }
}

/// MPEG transport stream demuxer.
pub struct TsReader<'s> {
    reader: MediaSourceStream<'s>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    chapters: Option<ChapterGroup>,
    pid: u16,
    kind: Kind,
    /// The position of the first packet of the stream.
    first_pos: u64,
    /// The bytes of the elementary stream that are not part of a packet yet.
    es: Vec<u8>,
    ended: bool,
    next_ts: u64,
    /// A frame that was read by a seek, and is the next one.
    pending: Option<(Box<[u8]>, u64)>,
}

impl<'s> TsReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let first_pos = mss.pos();

        let mut pmt_pid = None;
        let mut selected = None;

        // Find the first audio stream of the first program.
        for _ in 0..MAX_PROBE_PACKETS {
            let Some(packet) = read_packet(&mut mss)?
            else {
                break;
            };

            let Some(payload) = packet.payload()
            else {
                continue;
            };

            if packet.pid == 0 && pmt_pid.is_none() && packet.unit_start {
                pmt_pid = parse_pat(payload);
            }
            else if Some(packet.pid) == pmt_pid && packet.unit_start {
                selected = parse_pmt(payload);
                if selected.is_some() {
                    break;
                }
            }
        }

        let Some((pid, kind)) = selected
        else {
            return unsupported_error("ts: no supported audio stream");
        };

        let mut reader = TsReader {
            reader: mss,
            media_info: MediaInfo::from_track(&Track::new(0)),
            tracks: Vec::new(),
            metadata: opts.external_data.metadata.unwrap_or_default(),
            chapters: opts.external_data.chapters,
            pid,
            kind,
            first_pos,
            es: Vec::new(),
            ended: false,
            next_ts: 0,
            pending: None,
        };

        // The first frame gives the parameters of the stream.
        let mut params = AudioCodecParameters::new();
        loop {
            reader.sync_es();
            if let Some((frame, audio)) = parse_frame(&reader.es, kind) {
                if frame.len <= reader.es.len() {
                    match (kind, audio) {
                        (Kind::Adts, _) => {
                            let adts = parse_adts(&reader.es)
                                .ok_or(Error::DecodeError("ts: invalid adts"))?;
                            let asc = AudioSpecificConfig::read(&adts.config)?;
                            params.for_codec(CODEC_ID_AAC).with_sample_rate(asc.sample_rate);
                            if let Some(c) = asc.channels {
                                params.with_channels(c);
                            }
                            params.with_extra_data(Box::from(&adts.config[..]));
                        }
                        (_, Some(audio)) => {
                            params
                                .for_codec(audio.codec)
                                .with_sample_rate(audio.sample_rate)
                                .with_channels(audio.channels);
                        }
                        _ => return decode_error("ts: missing audio parameters"),
                    }
                    break;
                }
            }
            if !reader.fill()? {
                return unsupported_error("ts: no audio frame");
            }
        }

        let rate = params.sample_rate.ok_or(Error::DecodeError("ts: missing sample rate"))?;
        params.with_max_frames_per_packet(4096);

        let mut track = Track::new(0);
        track.with_codec_params(CodecParameters::Audio(params)).with_time_base(
            TimeBase::try_new(1, rate).ok_or(Error::DecodeError("ts: invalid sample rate"))?,
        );

        reader.media_info = MediaInfo::from_track(&track);
        reader.tracks = vec![track];

        Ok(reader)
    }

    /// Discards the bytes in front of the next frame header.
    fn sync_es(&mut self) {
        let kind = self.kind;
        let at = (0..self.es.len()).find(|&i| match kind {
            Kind::Adts => parse_adts(&self.es[i..]).is_some(),
            Kind::Mpeg => {
                self.es.len() - i >= 4
                    && parse_mpeg_audio(u32::from_be_bytes([
                        self.es[i],
                        self.es[i + 1],
                        self.es[i + 2],
                        self.es[i + 3],
                    ]))
                    .is_some()
            }
            Kind::Ac3 | Kind::Dts => {
                self.es.len() - i >= 16 && parse_frame(&self.es[i..], kind).is_some()
            }
        });

        match at {
            Some(i) => {
                self.es.drain(..i);
            }
            // Keep the end, which may hold the start of a header.
            None => {
                let keep = self.es.len().min(15);
                let cut = self.es.len() - keep;
                self.es.drain(..cut);
            }
        }
    }

    /// Reads packets until the elementary stream gets more bytes. Returns `false` at the end.
    fn fill(&mut self) -> Result<bool> {
        while !self.ended {
            let Some(packet) = read_packet(&mut self.reader)?
            else {
                self.ended = true;
                break;
            };

            if packet.pid != self.pid {
                continue;
            }

            let Some(payload) = packet.payload()
            else {
                continue;
            };

            let data = if packet.unit_start { pes_payload(payload) } else { payload };
            if !data.is_empty() {
                self.es.extend_from_slice(data);
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn pending_or_read(&mut self) -> Result<Option<(Box<[u8]>, u64)>> {
        match self.pending.take() {
            Some(frame) => Ok(Some(frame)),
            None => self.read_frame(),
        }
    }

    /// Reads the next frame of the stream, and returns its packet data and its number of samples.
    fn read_frame(&mut self) -> Result<Option<(Box<[u8]>, u64)>> {
        loop {
            self.sync_es();

            if let Some((frame, _)) = parse_frame(&self.es, self.kind) {
                if frame.len <= self.es.len() {
                    let data = self.es[frame.data_range.0..frame.data_range.1].to_vec();
                    self.es.drain(..frame.len);
                    return Ok(Some((data.into_boxed_slice(), frame.samples)));
                }
            }

            if !self.fill()? {
                return Ok(None);
            }
        }
    }
}

struct TsPacket {
    pid: u16,
    unit_start: bool,
    buf: [u8; PACKET_LEN],
    payload_at: Option<usize>,
}

impl TsPacket {
    fn payload(&self) -> Option<&[u8]> {
        self.payload_at.map(|at| &self.buf[at..])
    }
}

fn read_packet(reader: &mut MediaSourceStream<'_>) -> Result<Option<TsPacket>> {
    let mut buf = [0u8; PACKET_LEN];

    loop {
        match reader.read_buf_exact(&mut buf) {
            Ok(()) => (),
            Err(err) if err.kind() == ErrorKind::UnexpectedEof => return Ok(None),
            Err(err) => return Err(err.into()),
        }

        if buf[0] == SYNC_BYTE {
            break;
        }

        // Lost sync: try again one byte later.
        let pos = reader.pos() - PACKET_LEN as u64;
        reader.seek_buffered(pos + 1);
    }

    let unit_start = buf[1] & 0x40 != 0;
    let pid = (u16::from(buf[1] & 0x1f) << 8) | u16::from(buf[2]);
    let control = (buf[3] >> 4) & 3;

    let mut at = 4;
    if control & 2 != 0 {
        at += 1 + usize::from(buf[4]);
    }
    let payload_at = if control & 1 != 0 && at < PACKET_LEN { Some(at) } else { None };

    Ok(Some(TsPacket { pid, unit_start, buf, payload_at }))
}

/// Returns the payload of a packet that carries a section, after its pointer field.
fn section(payload: &[u8]) -> Option<&[u8]> {
    let pointer = usize::from(*payload.first()?);
    payload.get(1 + pointer..)
}

/// Returns the PID of the program map table of the first program.
fn parse_pat(payload: &[u8]) -> Option<u16> {
    let sec = section(payload)?;
    if sec.len() < 12 || sec[0] != 0 {
        return None;
    }
    let len = ((usize::from(sec[1] & 0xf) << 8) | usize::from(sec[2])).min(sec.len() - 3);
    // The program entries follow the 5 bytes of the section header, and precede the checksum.
    let entries = sec.get(8..3 + len.checked_sub(4)?)?;
    entries.chunks_exact(4).find_map(|e| {
        let program = (u16::from(e[0]) << 8) | u16::from(e[1]);
        (program != 0).then(|| (u16::from(e[2] & 0x1f) << 8) | u16::from(e[3]))
    })
}

/// Returns the PID and kind of the first audio stream of a program map table.
fn parse_pmt(payload: &[u8]) -> Option<(u16, Kind)> {
    let sec = section(payload)?;
    if sec.len() < 16 || sec[0] != 2 {
        return None;
    }
    let len = ((usize::from(sec[1] & 0xf) << 8) | usize::from(sec[2])).min(sec.len() - 3);
    let end = 3 + len.checked_sub(4)?;
    let program_info = (usize::from(sec[10] & 0xf) << 8) | usize::from(sec[11]);

    let mut at = 12 + program_info;
    while at + 5 <= end {
        let stream_type = sec[at];
        let pid = (u16::from(sec[at + 1] & 0x1f) << 8) | u16::from(sec[at + 2]);
        let info_len = (usize::from(sec[at + 3] & 0xf) << 8) | usize::from(sec[at + 4]);
        let descriptors = sec.get(at + 5..(at + 5 + info_len).min(end))?;

        if let Some(kind) = kind_of_stream_type(stream_type, descriptors) {
            return Some((pid, kind));
        }
        at += 5 + info_len;
    }

    None
}

/// Returns the elementary stream bytes of the start of a PES packet.
fn pes_payload(payload: &[u8]) -> &[u8] {
    if payload.len() >= 9 && payload[..3] == [0, 0, 1] {
        let header_len = usize::from(payload[8]);
        payload.get(9 + header_len..).unwrap_or(&[])
    }
    else {
        payload
    }
}

impl Scoreable for TsReader<'_> {
    fn score(mut src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        let mut buf = [0u8; 3 * PACKET_LEN + 1];
        src.read_buf_exact(&mut buf)?;

        let valid =
            [0, PACKET_LEN, 2 * PACKET_LEN, 3 * PACKET_LEN].iter().all(|&i| buf[i] == SYNC_BYTE);

        Ok(if valid { Score::Supported(255) } else { Score::Unsupported })
    }
}

impl ProbeableFormat<'_> for TsReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(TsReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(
            TS_FORMAT_INFO,
            &["ts", "m2t", "mts"],
            &["video/mp2t", "audio/mp2t"],
            // Streams start with the program association table, which has PID 0, or with the
            // service description table.
            &[&[SYNC_BYTE, 0x40, 0x00], &[SYNC_BYTE, 0x40, 0x11], &[SYNC_BYTE, 0x60, 0x00]]
        )]
    }
}

impl FormatReader for TsReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &TS_FORMAT_INFO
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
            self.reader.seek(SeekFrom::Start(self.first_pos))?;
            self.es.clear();
            self.pending = None;
            self.ended = false;
            self.next_ts = 0;
        }

        // The timestamps come from the frames, so count them up to the one with the sample.
        loop {
            let Some((data, samples)) = self.pending_or_read()?
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
        let Some((data, samples)) = self.pending_or_read()?
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

    /// A packet that is padded to its length by stuffing in an adaptation field.
    fn packet(pid: u16, unit_start: bool, counter: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![SYNC_BYTE, (u8::from(unit_start) << 6) | (pid >> 8) as u8, pid as u8];
        let stuffing = 184 - payload.len();
        if stuffing == 0 {
            out.push(0x10 | counter);
        }
        else {
            out.push(0x30 | counter);
            out.push((stuffing - 1) as u8);
            if stuffing > 1 {
                out.push(0);
                out.resize(out.len() + stuffing - 2, 0xff);
            }
        }
        out.extend_from_slice(payload);
        assert_eq!(out.len(), PACKET_LEN);
        out
    }

    fn section(table_id: u8, body: &[u8]) -> Vec<u8> {
        let mut out =
            vec![0, table_id, 0xb0 | ((body.len() + 9) >> 8) as u8, (body.len() + 9) as u8];
        out.extend_from_slice(&[0, 1, 0xc1, 0, 0]);
        out.extend_from_slice(body);
        out.extend_from_slice(&[0; 4]);
        out
    }

    /// An ADTS frame of 16-bit mono AAC-LC at 44.1 kHz with an arbitrary payload.
    fn adts_frame(seed: u8) -> Vec<u8> {
        let len = 7 + 20;
        let mut out = vec![
            0xff,
            0xf1,
            0x50,
            0x40 | (len >> 11) as u8,
            (len >> 3) as u8,
            ((len & 7) << 5) as u8 | 0x1f,
            0xfc,
        ];
        out.extend((0..20).map(|i| seed.wrapping_add(i)));
        out
    }

    fn stream(frames: usize) -> Vec<u8> {
        let mut out = packet(0, true, 0, &section(0, &[0, 1, 0xe1, 0x00]));
        out.extend(packet(
            0x100,
            true,
            0,
            &section(2, &[0xe1, 0x01, 0xf0, 0, 0x0f, 0xe1, 0x01, 0xf0, 0]),
        ));

        let mut es = vec![0, 0, 1, 0xc0, 0, 0, 0x80, 0, 0];
        for i in 0..frames {
            es.extend(adts_frame(i as u8));
        }
        for (i, chunk) in es.chunks(184).enumerate() {
            out.extend(packet(0x101, i == 0, i as u8 & 15, chunk));
        }
        out
    }

    fn reader(frames: usize) -> TsReader<'static> {
        let mss = MediaSourceStream::new(Box::new(Cursor::new(stream(frames))), Default::default());
        TsReader::try_new(mss, FormatOptions::default()).unwrap()
    }

    #[test]
    fn reads_adts_frames_across_packets() {
        let mut reader = reader(20);

        let Some(CodecParameters::Audio(params)) = reader.tracks()[0].codec_params.clone()
        else {
            panic!("not an audio track");
        };
        assert_eq!(params.codec, CODEC_ID_AAC);
        assert_eq!(params.sample_rate, Some(44100));

        for i in 0..20 {
            let packet = reader.next_packet().unwrap().unwrap();
            assert_eq!(packet.pts.get(), i * 1024);
            assert_eq!(&*packet.data, &adts_frame(i as u8)[7..]);
        }
        assert!(reader.next_packet().unwrap().is_none());
    }

    #[test]
    fn seeks_to_the_frame_with_the_sample() {
        let mut reader = reader(20);
        let to = SeekTo::Timestamp { ts: Timestamp::new(7 * 1024 + 5), track_id: 0 };
        let seeked = reader.seek(SeekMode::Accurate, to).unwrap();
        assert_eq!(seeked.actual_ts.get(), 7 * 1024);
        assert_eq!(reader.next_packet().unwrap().unwrap().pts.get(), 7 * 1024);

        let to = SeekTo::Timestamp { ts: Timestamp::new(0), track_id: 0 };
        assert_eq!(reader.seek(SeekMode::Accurate, to).unwrap().actual_ts.get(), 0);
    }

    #[test]
    fn rejects_other_files() {
        let mss =
            MediaSourceStream::new(Box::new(Cursor::new(vec![0u8; 1000])), Default::default());
        assert!(TsReader::try_new(mss, FormatOptions::default()).is_err());
    }
}
