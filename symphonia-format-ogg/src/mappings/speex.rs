// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::common::SideData;

use super::{MapResult, Mapper, PacketParser};

use symphonia_core::audio::{Channels, Position};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::codecs::audio::well_known::CODEC_ID_SPEEX;
use symphonia_core::errors::Result;
use symphonia_core::formats::Track;
use symphonia_core::io::BufReader;
use symphonia_core::meta::MetadataBuilder;
use symphonia_core::units::{Duration, Timestamp};
use symphonia_metadata::embedded::vorbis::{self, VORBIS_COMMENT_METADATA_INFO};

use log::warn;

/// The length of the Speex header packet.
const OGG_SPEEX_HEADER_LEN: usize = 80;

/// The signature of the Speex header packet.
const OGG_SPEEX_SIGNATURE: &[u8] = b"Speex   ";

pub fn detect(serial: u32, buf: &[u8]) -> Result<Option<Box<dyn Mapper>>> {
    if buf.len() < OGG_SPEEX_HEADER_LEN || !buf.starts_with(OGG_SPEEX_SIGNATURE) {
        return Ok(None);
    }

    let word = |at: usize| u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);

    let sample_rate = word(36);
    let channels = word(48);
    let frame_size = word(56);
    let frames_per_packet = word(64).max(1);

    if sample_rate == 0 || !(1..=2).contains(&channels) || frame_size == 0 {
        return Ok(None);
    }

    let positions = if channels == 2 {
        Position::FRONT_LEFT | Position::FRONT_RIGHT
    }
    else {
        Position::FRONT_CENTER
    };

    let mut codec_params = AudioCodecParameters::new();

    codec_params
        .for_codec(CODEC_ID_SPEEX)
        .with_sample_rate(sample_rate)
        .with_channels(Channels::Positioned(positions))
        .with_extra_data(Box::from(&buf[..OGG_SPEEX_HEADER_LEN]));

    let mut track = Track::new(serial);
    track.with_codec_params(CodecParameters::Audio(codec_params));

    let dur = u64::from(frame_size) * u64::from(frames_per_packet);

    // The granule position of the first page excludes the samples of the decoder delay.
    let lookahead = match word(40) {
        0 => 40,
        1 => 143,
        _ => 349,
    };

    Ok(Some(Box::new(SpeexMapper { track, dur, lookahead, need_comment: true })))
}

struct SpeexPacketParser {
    dur: u64,
}

impl PacketParser for SpeexPacketParser {
    fn parse_next_packet_dur(&mut self, _: &[u8]) -> (Duration, Duration) {
        (Duration::new(self.dur), Duration::ZERO)
    }
}

struct SpeexMapper {
    track: Track,
    dur: u64,
    lookahead: i64,
    need_comment: bool,
}

impl Mapper for SpeexMapper {
    fn name(&self) -> &'static str {
        "speex"
    }

    fn reset(&mut self) {
        // Nothing to do!
    }

    fn track(&self) -> &Track {
        &self.track
    }

    fn track_mut(&mut self) -> &mut Track {
        &mut self.track
    }

    fn absgp_to_ts(&self, absgp: u64) -> Timestamp {
        Timestamp::from((absgp as i64).saturating_add(self.lookahead))
    }

    fn make_parser(&self) -> Option<Box<dyn PacketParser>> {
        Some(Box::new(SpeexPacketParser { dur: self.dur }))
    }

    fn map_packet(&mut self, packet: &[u8]) -> Result<MapResult> {
        if !self.need_comment {
            return Ok(MapResult::StreamData {
                dur: Duration::new(self.dur),
                discard: Duration::ZERO,
            });
        }

        // The packet that follows the header is a Vorbis comment without a signature.
        self.need_comment = false;

        let mut reader = BufReader::new(packet);
        let mut builder = MetadataBuilder::new(VORBIS_COMMENT_METADATA_INFO);
        let mut side_data = Default::default();

        match vorbis::read_vorbis_comment(&mut reader, &mut builder, &mut side_data) {
            Ok(()) => {
                let rev = builder.build();
                Ok(MapResult::SideData { data: SideData::Metadata { rev, side_data } })
            }
            Err(err) => {
                warn!("ogg (speex): invalid comment packet: {err}");
                Ok(MapResult::Unknown)
            }
        }
    }
}
