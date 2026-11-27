// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Sony Wave64 (W64), the 64-bit extension of WAVE. The chunks have the same payloads as those of
//! WAVE, but are identified by 16-byte GUIDs, have 64-bit sizes that include the 24-byte chunk
//! header, and are padded to 8 bytes.

use std::io::{Seek, SeekFrom};

use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::errors::{Error, Result, SeekErrorKind};
use symphonia_core::errors::{decode_error, seek_error, unsupported_error};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_W64;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::support_format;

use crate::common::{
    FormatData, PacketInfo, ParseChunk, append_data_params, append_format_params, next_packet,
};
use crate::wave::chunks::WaveFormatChunk;

const W64_FORMAT_INFO: FormatInfo =
    FormatInfo { format: FORMAT_ID_W64, short_name: "w64", long_name: "Sony Wave64" };

/// The GUID of the top-level chunk, which is the file marker.
const GUID_RIFF: [u8; 16] = [
    0x72, 0x69, 0x66, 0x66, 0x2e, 0x91, 0xcf, 0x11, 0xa5, 0xd6, 0x28, 0xdb, 0x04, 0xc1, 0x00, 0x00,
];

/// The suffix that the GUIDs of the WAVE form, and of its `fmt `, `fact`, and `data` chunks share,
/// which follows the tag of the chunk.
const GUID_SUFFIX: [u8; 12] =
    [0xf3, 0xac, 0xd3, 0x11, 0x8c, 0xd1, 0x00, 0xc0, 0x4f, 0x8e, 0xdb, 0x8a];

/// The longest format chunk that is read.
const MAX_FORMAT_LEN: u64 = 4096;

/// The size of the header of a chunk: a GUID and a size.
const CHUNK_HEADER_LEN: u64 = 24;

fn is_wave_guid(guid: &[u8; 16], tag: &[u8; 4]) -> bool {
    guid[..4] == *tag && guid[4..] == GUID_SUFFIX
}

/// Sony Wave64 format reader.
pub struct W64Reader<'s> {
    reader: MediaSourceStream<'s>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    chapters: Option<ChapterGroup>,
    metadata: MetadataLog,
    packet_info: PacketInfo,
    data_start_pos: u64,
    data_end_pos: u64,
}

impl<'s> W64Reader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let mut guid = [0u8; 16];
        mss.read_buf_exact(&mut guid)?;

        if guid != GUID_RIFF {
            return unsupported_error("w64: missing riff guid");
        }

        // The size of the file, and the GUID of the WAVE form.
        mss.ignore_bytes(8)?;
        mss.read_buf_exact(&mut guid)?;

        if !is_wave_guid(&guid, b"wave") {
            return unsupported_error("w64: form is not wave");
        }

        let mut format = None;

        loop {
            let start = mss.pos();

            if mss.read_buf_exact(&mut guid).is_err() {
                return unsupported_error("w64: missing data chunk");
            }
            let size = mss.read_u64()?;

            if size < CHUNK_HEADER_LEN {
                return decode_error("w64: invalid chunk size");
            }
            let payload_len = size - CHUNK_HEADER_LEN;
            let Some(padded_end) =
                size.checked_next_multiple_of(8).and_then(|padded| start.checked_add(padded))
            else {
                return decode_error("w64: invalid chunk size");
            };

            if is_wave_guid(&guid, b"fmt ") {
                if payload_len > MAX_FORMAT_LEN {
                    return decode_error("w64: invalid format chunk");
                }

                // Some writers include the padding in the size of the chunk, which the parser of
                // the format cannot tell apart from additional data, so try the unpadded lengths.
                let body = mss.read_boxed_slice_exact(payload_len as usize)?;
                let mut parsed = None;
                for pad in 0..8.min(body.len() as u32) {
                    let len = body.len() as u32 - pad;
                    if let Ok(f) = WaveFormatChunk::parse(&mut BufReader::new(&body), *b"fmt ", len)
                    {
                        parsed = Some(f);
                        break;
                    }
                }
                format = parsed;
                if format.is_none() {
                    return decode_error("w64: invalid format chunk");
                }
            }
            else if is_wave_guid(&guid, b"data") {
                let Some(format) = format
                else {
                    return decode_error("w64: missing format chunk");
                };

                if matches!(format.format_data, FormatData::Mpeg(_) | FormatData::Framed(_)) {
                    return unsupported_error("w64: unsupported format");
                }

                let packet_info = format.packet_info()?;

                let mut codec_params = AudioCodecParameters::new();
                codec_params
                    .with_max_frames_per_packet(packet_info.max_frames_per_packet.get())
                    .with_frames_per_block(packet_info.frames_per_block.get());
                append_format_params(&mut codec_params, format.format_data, format.sample_rate);

                let data_start_pos = mss.pos();
                let data_end_pos = data_start_pos + payload_len;

                let mut track = Track::new(0);
                track.with_codec_params(CodecParameters::Audio(codec_params));
                append_data_params(&mut track, payload_len, &packet_info);

                return Ok(W64Reader {
                    reader: mss,
                    media_info: MediaInfo::from_track(&track),
                    tracks: vec![track],
                    chapters: opts.external_data.chapters,
                    metadata: opts.external_data.metadata.unwrap_or_default(),
                    packet_info,
                    data_start_pos,
                    data_end_pos,
                });
            }

            // Skip the rest of the chunk, which is padded.
            let pos = mss.pos();
            if padded_end < pos {
                return decode_error("w64: invalid chunk size");
            }
            mss.ignore_bytes(padded_end - pos)?;
        }
    }
}

impl Scoreable for W64Reader<'_> {
    fn score(mut src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        let mut guid = [0u8; 16];
        src.read_buf_exact(&mut guid)?;

        if guid != GUID_RIFF {
            return Ok(Score::Unsupported);
        }

        src.ignore_bytes(8)?;
        src.read_buf_exact(&mut guid)?;

        Ok(if is_wave_guid(&guid, b"wave") { Score::Supported(255) } else { Score::Unsupported })
    }
}

impl ProbeableFormat<'_> for W64Reader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(W64Reader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(W64_FORMAT_INFO, &["w64"], &["audio/x-w64", "audio/w64"], &[&GUID_RIFF])]
    }
}

impl FormatReader for W64Reader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &W64_FORMAT_INFO
    }

    fn media_info(&self) -> &MediaInfo {
        &self.media_info
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        next_packet(
            &mut self.reader,
            &self.packet_info,
            &self.tracks,
            self.data_start_pos,
            self.data_end_pos,
        )
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
        let track = &self.tracks[0];

        let required_ts = match to {
            SeekTo::Timestamp { ts, .. } => ts,
            SeekTo::Time { time, .. } => {
                let tb = track.time_base.ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
                tb.calc_timestamp(time).ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?
            }
        };

        if required_ts.is_negative() {
            return seek_error(SeekErrorKind::OutOfRange);
        }

        if let Some(num_frames) = track.num_frames {
            if required_ts.get() as u64 > num_frames {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        }

        // Seek to a packet boundary, so that the packets have the same timestamps after a seek.
        let actual_ts = self.packet_info.get_actual_ts(required_ts);
        let block_idx = actual_ts.get() as u64 / self.packet_info.frames_per_block;
        let seek_pos = self.data_start_pos + block_idx * self.packet_info.block_size.get();

        if self.reader.is_seekable() {
            self.reader.seek(SeekFrom::Start(seek_pos))?;
        }
        else {
            let current_pos = self.reader.pos();
            if seek_pos >= current_pos {
                self.reader.ignore_bytes(seek_pos - current_pos)?;
            }
            else {
                return seek_error(SeekErrorKind::ForwardOnly);
            }
        }

        Ok(SeekedTo { track_id: 0, actual_ts, required_ts })
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

    fn chunk(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(tag);
        out.extend_from_slice(&GUID_SUFFIX);
        out.extend_from_slice(&(24 + payload.len() as u64).to_le_bytes());
        out.extend_from_slice(payload);
        out.resize(out.len().next_multiple_of(8), 0);
        out
    }

    fn w64(samples: &[i16]) -> Vec<u8> {
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1u16.to_le_bytes());
        fmt.extend_from_slice(&1u16.to_le_bytes());
        fmt.extend_from_slice(&8000u32.to_le_bytes());
        fmt.extend_from_slice(&16000u32.to_le_bytes());
        fmt.extend_from_slice(&2u16.to_le_bytes());
        fmt.extend_from_slice(&16u16.to_le_bytes());

        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();

        let mut body = Vec::new();
        body.extend_from_slice(b"wave");
        body.extend_from_slice(&GUID_SUFFIX);
        body.extend(chunk(b"fmt ", &fmt));
        body.extend(chunk(b"data", &data));

        let mut out = GUID_RIFF.to_vec();
        out.extend_from_slice(&(24 + body.len() as u64).to_le_bytes());
        out.extend(body);
        out
    }

    fn reader(samples: &[i16]) -> W64Reader<'static> {
        let mss = MediaSourceStream::new(Box::new(Cursor::new(w64(samples))), Default::default());
        W64Reader::try_new(mss, FormatOptions::default()).unwrap()
    }

    #[test]
    fn reads_pcm_samples() {
        let mut reader = reader(&[1, -2, 3, -4, 5]);

        let track = &reader.tracks()[0];
        assert_eq!(track.num_frames, Some(5));

        let packet = reader.next_packet().unwrap().unwrap();
        let samples: Vec<i16> =
            packet.data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        assert_eq!(samples, [1, -2, 3, -4, 5]);
        assert!(reader.next_packet().unwrap().is_none());
    }

    #[test]
    fn seeks_to_a_frame() {
        let mut reader = reader(&[1, 2, 3, 4, 5, 6]);
        let to = SeekTo::Timestamp { ts: Timestamp::new(4), track_id: 0 };
        let seeked = reader.seek(SeekMode::Accurate, to).unwrap();

        // The seek lands on a packet boundary.
        let packet = reader.next_packet().unwrap().unwrap();
        assert_eq!(packet.pts, seeked.actual_ts);
        assert_eq!(packet.data.len() as i64, (6 - seeked.actual_ts.get()) * 2);
    }

    #[test]
    fn rejects_other_files() {
        let mss = MediaSourceStream::new(Box::new(Cursor::new(vec![0u8; 64])), Default::default());
        assert!(W64Reader::try_new(mss, FormatOptions::default()).is_err());
    }
}
