// Symphonia APE demuxer
// Copyright (c) 2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::HashMap;
use std::io::{Seek, SeekFrom};

use lazy_static::lazy_static;

use symphonia_core::codecs::{CodecParameters, CODEC_TYPE_MONKEYS_AUDIO};
use symphonia_core::errors::{seek_error, Result, SeekErrorKind};
use symphonia_core::formats::{
    Cue, FormatOptions, FormatReader, Packet, SeekMode, SeekTo, SeekedTo, Track,
};
use symphonia_core::io::{MediaSource, MediaSourceStream};
use symphonia_core::meta::{MetadataBuilder, MetadataLog, Metadata, StandardTagKey, Tag, Value};
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};
use symphonia_core::support_format;
use symphonia_core::units::TimeBase;

use ape_decoder::format::ApeFileInfo;

// ---------------------------------------------------------------------------
// APE tag field name → StandardTagKey mapping
// ---------------------------------------------------------------------------

lazy_static! {
    static ref APE_TAG_MAP: HashMap<&'static str, StandardTagKey> = {
        let mut m = HashMap::new();
        m.insert("album artist",          StandardTagKey::AlbumArtist);
        m.insert("album",                 StandardTagKey::Album);
        m.insert("artist",                StandardTagKey::Artist);
        m.insert("bpm",                   StandardTagKey::Bpm);
        m.insert("comment",               StandardTagKey::Comment);
        m.insert("composer",              StandardTagKey::Composer);
        m.insert("conductor",             StandardTagKey::Conductor);
        m.insert("copyright",             StandardTagKey::Copyright);
        m.insert("disc",                  StandardTagKey::DiscNumber);
        m.insert("genre",                 StandardTagKey::Genre);
        m.insert("keywords",              StandardTagKey::Description);
        m.insert("lyrics",                StandardTagKey::Lyrics);
        m.insert("notes",                 StandardTagKey::Comment);
        m.insert("publisher",             StandardTagKey::Label);
        m.insert("rating",                StandardTagKey::Rating);
        m.insert("replay gain (album)",   StandardTagKey::ReplayGainAlbumGain);
        m.insert("replay gain (radio)",   StandardTagKey::ReplayGainTrackGain);
        m.insert("title",                 StandardTagKey::TrackTitle);
        m.insert("tool name",             StandardTagKey::Encoder);
        m.insert("track",                 StandardTagKey::TrackNumber);
        m.insert("year",                  StandardTagKey::Date);
        m.insert("artist url",            StandardTagKey::UrlArtist);
        m.insert("buy url",               StandardTagKey::UrlPurchase);
        m.insert("copyright url",         StandardTagKey::UrlCopyright);
        m
    };
}

// ---------------------------------------------------------------------------
// ApeReader
// ---------------------------------------------------------------------------

/// Monkey's Audio (APE) format reader (demuxer).
pub struct ApeReader {
    reader: MediaSourceStream,
    metadata: MetadataLog,
    tracks: Vec<Track>,
    cues: Vec<Cue>,
    file_info: ApeFileInfo,
    current_frame: u32,
}

impl QueryDescriptor for ApeReader {
    fn query() -> &'static [Descriptor] {
        &[support_format!(
            "ape",
            "Monkey's Audio",
            &["ape"],
            &["audio/x-ape", "audio/ape"],
            &[b"MAC "]
        )]
    }

    fn score(context: &[u8]) -> u8 {
        if context.len() >= 4 && &context[..4] == b"MAC " {
            255
        }
        else {
            0
        }
    }
}

/// Map an APE channel count to Symphonia's `Channels` bitmask.
fn channels_from_count(count: u16) -> symphonia_core::audio::Channels {
    use symphonia_core::audio::{Channels, Layout};

    match count {
        1 => Layout::Mono.into_channels(),
        2 => Layout::Stereo.into_channels(),
        3 => Channels::FRONT_LEFT | Channels::FRONT_RIGHT | Channels::FRONT_CENTRE,
        4 => {
            Channels::FRONT_LEFT
                | Channels::FRONT_RIGHT
                | Channels::REAR_LEFT
                | Channels::REAR_RIGHT
        }
        5 => Layout::FivePointOne.into_channels() & !Channels::LFE1,
        6 => Layout::FivePointOne.into_channels(),
        _ => {
            // For 7+ channels, map to standard surround positions (capped at 8).
            // APE supports up to 32 channels but > 8 is extremely rare.
            let all = [
                Channels::FRONT_LEFT,
                Channels::FRONT_RIGHT,
                Channels::FRONT_CENTRE,
                Channels::LFE1,
                Channels::REAR_LEFT,
                Channels::REAR_RIGHT,
                Channels::SIDE_LEFT,
                Channels::SIDE_RIGHT,
            ];
            let n = (count as usize).min(all.len());
            let mut ch = Channels::empty();
            for &flag in &all[..n] {
                ch |= flag;
            }
            ch
        }
    }
}

/// Build the extra_data blob that the decoder needs to reconstruct its state.
/// Layout (12 bytes, all little-endian):
///   [0..2]  version (u16)
///   [2..4]  compression_level (u16)
///   [4..6]  bits_per_sample (u16)
///   [6..8]  channels (u16)
///   [8..10] format_flags (u16)
///   [10..12] padding (u16, reserved)
fn build_extra_data(info: &ApeFileInfo) -> Box<[u8]> {
    let mut buf = vec![0u8; 12];
    buf[0..2].copy_from_slice(&info.descriptor.version.to_le_bytes());
    buf[2..4].copy_from_slice(&info.header.compression_level.to_le_bytes());
    buf[4..6].copy_from_slice(&info.header.bits_per_sample.to_le_bytes());
    buf[6..8].copy_from_slice(&info.header.channels.to_le_bytes());
    buf[8..10].copy_from_slice(&info.header.format_flags.to_le_bytes());
    buf.into_boxed_slice()
}

/// Read APE tags and ID3v2 tags from the source, converting them to Symphonia metadata.
fn read_metadata(reader: &mut MediaSourceStream, metadata: &mut MetadataLog) {
    // Read APEv2 tags (located at end of file).
    if let Ok(Some(tag)) = ape_decoder::read_tag(reader) {
        let mut builder = MetadataBuilder::new();

        for field in &tag.fields {
            if field.field_type() == ape_decoder::TagFieldType::TextUtf8 {
                if let Some(text) = field.value_as_str() {
                    let key_lower = field.name.to_lowercase();
                    let std_key = APE_TAG_MAP.get(key_lower.as_str()).copied();
                    builder.add_tag(Tag::new(std_key, &field.name, Value::from(text)));
                }
            }
        }

        let revision = builder.metadata();
        metadata.push(revision);
    }

    // Read ID3v2 tags (prepended before APE header, in junk region).
    if let Ok(Some(id3)) = ape_decoder::read_id3v2(reader) {
        let mut builder = MetadataBuilder::new();

        let known: &[(&str, StandardTagKey, Option<String>)] = &[
            ("TIT2", StandardTagKey::TrackTitle, id3.title()),
            ("TPE1", StandardTagKey::Artist, id3.artist()),
            ("TALB", StandardTagKey::Album, id3.album()),
            ("TRCK", StandardTagKey::TrackNumber, id3.track()),
            ("TDRC", StandardTagKey::Date, id3.year()),
            ("TCON", StandardTagKey::Genre, id3.genre()),
            ("COMM", StandardTagKey::Comment, id3.comment()),
        ];

        for (key, std_key, value) in known {
            if let Some(text) = value {
                builder.add_tag(Tag::new(Some(*std_key), key, Value::from(text.as_str())));
            }
        }

        let revision = builder.metadata();
        metadata.push(revision);
    }
}

impl FormatReader for ApeReader {
    fn try_new(source: MediaSourceStream, _options: &FormatOptions) -> Result<Self>
    where
        Self: Sized,
    {
        let mut reader = source;

        // Parse the APE file header, descriptor, and seek table.
        let file_info = ape_decoder::format::parse(&mut reader).map_err(|e| {
            symphonia_core::errors::Error::DecodeError(match e {
                ape_decoder::ApeError::InvalidFormat(msg) => msg,
                ape_decoder::ApeError::UnsupportedVersion(_) => "ape: unsupported version",
                ape_decoder::ApeError::InvalidChecksum => "ape: invalid checksum",
                ape_decoder::ApeError::DecodingError(msg) => msg,
                ape_decoder::ApeError::Io(_) => "ape: I/O error during header parse",
                _ => "ape: unknown error during header parse",
            })
        })?;

        let header = &file_info.header;

        // Reject unsupported channel counts. Our channel mapping supports up to 8.
        if header.channels == 0 || header.channels > 8 {
            return Err(symphonia_core::errors::Error::Unsupported(
                "ape: unsupported channel count (must be 1-8)",
            ));
        }

        // Read metadata (APE tags and ID3v2 tags). Errors are silently ignored
        // since metadata is non-essential for playback.
        let mut metadata = MetadataLog::default();
        read_metadata(&mut reader, &mut metadata);

        // Build codec parameters.
        let mut codec_params = CodecParameters::new();

        codec_params
            .for_codec(CODEC_TYPE_MONKEYS_AUDIO)
            .with_sample_rate(header.sample_rate)
            .with_time_base(TimeBase::new(1, header.sample_rate))
            .with_bits_per_sample(header.bits_per_sample as u32)
            .with_channels(channels_from_count(header.channels))
            .with_n_frames(file_info.total_blocks as u64)
            .with_max_frames_per_packet(header.blocks_per_frame as u64)
            .with_packet_data_integrity(true)
            .with_extra_data(build_extra_data(&file_info));

        let tracks = vec![Track::new(0, codec_params)];

        Ok(ApeReader {
            reader,
            metadata,
            tracks,
            cues: Vec::new(),
            file_info,
            current_frame: 0,
        })
    }

    fn cues(&self) -> &[Cue] {
        &self.cues
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        if self.tracks.is_empty() || !self.reader.is_seekable() {
            return seek_error(SeekErrorKind::Unseekable);
        }

        let params = &self.tracks[0].codec_params;

        // Convert SeekTo to a timestamp (sample number).
        let ts = match to {
            SeekTo::TimeStamp { ts, .. } => ts,
            SeekTo::Time { time, .. } => {
                if let Some(sr) = params.sample_rate {
                    TimeBase::new(1, sr).calc_timestamp(time)
                }
                else {
                    return seek_error(SeekErrorKind::Unseekable);
                }
            }
        };

        let total = self.file_info.total_blocks as u64;
        if ts >= total {
            return seek_error(SeekErrorKind::OutOfRange);
        }

        // Calculate the frame containing the target sample.
        let bpf = self.file_info.header.blocks_per_frame as u64;
        let frame_idx = (ts / bpf) as u32;
        let actual_ts = frame_idx as u64 * bpf;

        self.current_frame = frame_idx;

        Ok(SeekedTo { track_id: 0, required_ts: ts, actual_ts })
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> Result<Packet> {
        let frame_idx = self.current_frame;

        if frame_idx >= self.file_info.header.total_frames {
            return Err(symphonia_core::errors::Error::IoError(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "ape: end of stream",
            )));
        }

        let seek_byte = self.file_info.seek_byte(frame_idx);
        let seek_remainder = self.seek_remainder(frame_idx);
        let frame_bytes = self.file_info.frame_byte_count(frame_idx);
        let frame_blocks = self.file_info.frame_block_count(frame_idx) as u64;

        if frame_bytes > 64 * 1024 * 1024 {
            return Err(symphonia_core::errors::Error::DecodeError(
                "ape: frame data exceeds 64 MB",
            ));
        }

        let read_bytes = (frame_bytes as u32 + seek_remainder + 4) as usize;

        // Seek to frame data (accounting for alignment).
        self.reader.seek(SeekFrom::Start(seek_byte - seek_remainder as u64))?;

        // Read frame data.
        let mut frame_data = vec![0u8; read_bytes];
        let mut total_read = 0;
        while total_read < read_bytes {
            let n = std::io::Read::read(&mut self.reader, &mut frame_data[total_read..])?;
            if n == 0 {
                break;
            }
            total_read += n;
        }
        frame_data.truncate(total_read);

        // Prepend seek_remainder as a 4-byte LE header so the decoder knows the alignment.
        let mut packet_data = Vec::with_capacity(4 + frame_data.len());
        packet_data.extend_from_slice(&seek_remainder.to_le_bytes());
        packet_data.extend_from_slice(&frame_data);

        let ts = frame_idx as u64 * self.file_info.header.blocks_per_frame as u64;

        self.current_frame += 1;

        Ok(Packet::new_from_boxed_slice(
            0,
            ts,
            frame_blocks,
            packet_data.into_boxed_slice(),
        ))
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.reader
    }
}

impl ApeReader {
    fn seek_remainder(&self, frame_idx: u32) -> u32 {
        let seek_byte = self.file_info.seek_byte(frame_idx);
        let seek_byte_0 = self.file_info.seek_byte(0);
        ((seek_byte - seek_byte_0) % 4) as u32
    }
}
