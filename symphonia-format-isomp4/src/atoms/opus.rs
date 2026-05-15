// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::audio::{Channels, Position};
use symphonia_core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia_core::errors::Error;

use crate::atoms::stsd::AudioSampleEntry;
use crate::atoms::{
    Atom, AtomHeader, AtomIterator, ReadAtom, Result, decode_error, unsupported_error,
};

/// Opus atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct OpusAtom {
    /// Audio channels
    channels: Channels,
    /// Pre skip
    pre_skip: u16,
}

impl Atom for OpusAtom {
    fn read<R: ReadAtom>(reader: &mut AtomIterator<R>, header: &AtomHeader) -> Result<Self> {
        const OPUS_MAGIC: &[u8] = b"OpusHead";
        const OPUS_MAGIC_LEN: usize = OPUS_MAGIC.len();

        const MIN_OPUS_EXTRA_DATA_SIZE: usize = OPUS_MAGIC_LEN + 11;
        const MAX_OPUS_EXTRA_DATA_SIZE: usize = MIN_OPUS_EXTRA_DATA_SIZE + 257;

        // The dops atom contains an Opus identification header excluding the OpusHead magic
        // signature. Therefore, the atom data length should be atleast as long as the shortest
        // Opus identification header.
        let data_len = header
            .data_size()
            .ok_or(Error::DecodeError("isomp4 (opus): expected atom size to be known"))?
            as usize;

        if data_len < MIN_OPUS_EXTRA_DATA_SIZE - OPUS_MAGIC_LEN {
            return decode_error("isomp4 (opus): opus identification header too short");
        }

        if data_len > MAX_OPUS_EXTRA_DATA_SIZE - OPUS_MAGIC_LEN {
            return decode_error("isomp4 (opus): opus identification header too large");
        }

        let version = reader.read_byte()?;

        // Verify the version number is 0.
        if version != 0 {
            return unsupported_error("isomp4 (opus): unsupported opus version");
        }

        let channel_count = reader.read_byte()?;

        if channel_count == 0 {
            return decode_error("isomp4 (opus): Invalid channel count");
        }

        let pre_skip = u16::from_be_bytes(reader.read_double_bytes()?);

        let _input_sample_rate = u32::from_be_bytes(reader.read_quad_bytes()?);

        let _output_gain = i16::from_be_bytes(reader.read_double_bytes()?);

        let channel_mapping = reader.read_byte()?;

        let positions = match channel_mapping {
            // RTP Mapping
            0 if channel_count == 1 => Position::FRONT_LEFT,
            0 if channel_count == 2 => Position::FRONT_LEFT | Position::FRONT_RIGHT,
            // Vorbis Mapping
            1 => match channel_count {
                1 => Position::FRONT_LEFT,
                2 => Position::FRONT_LEFT | Position::FRONT_RIGHT,
                3 => Position::FRONT_LEFT | Position::FRONT_CENTER | Position::FRONT_RIGHT,
                4 => {
                    Position::FRONT_LEFT
                        | Position::FRONT_RIGHT
                        | Position::REAR_LEFT
                        | Position::REAR_RIGHT
                }
                5 => {
                    Position::FRONT_LEFT
                        | Position::FRONT_CENTER
                        | Position::FRONT_RIGHT
                        | Position::REAR_LEFT
                        | Position::REAR_RIGHT
                }
                6 => {
                    Position::FRONT_LEFT
                        | Position::FRONT_CENTER
                        | Position::FRONT_RIGHT
                        | Position::REAR_LEFT
                        | Position::REAR_RIGHT
                        | Position::LFE1
                }
                7 => {
                    Position::FRONT_LEFT
                        | Position::FRONT_CENTER
                        | Position::FRONT_RIGHT
                        | Position::SIDE_LEFT
                        | Position::SIDE_RIGHT
                        | Position::REAR_CENTER
                        | Position::LFE1
                }
                8 => {
                    Position::FRONT_LEFT
                        | Position::FRONT_CENTER
                        | Position::FRONT_RIGHT
                        | Position::SIDE_LEFT
                        | Position::SIDE_RIGHT
                        | Position::REAR_LEFT
                        | Position::REAR_RIGHT
                        | Position::LFE1
                }
                _ => return decode_error("isomp4 (opus): Invalid channel mapping"),
            },
            // Reserved, and should NOT be supported for playback.
            _ => return decode_error("isomp4 (opus): Invalid channel mapping"),
        };

        Ok(OpusAtom { channels: Channels::Positioned(positions), pre_skip })
    }
}

impl OpusAtom {
    pub fn fill_audio_sample_entry(self, entry: &mut AudioSampleEntry) {
        entry.codec_id = CODEC_ID_OPUS;
        entry.channels = Some(self.channels);
        entry.sample_rate = 48_000.0;
    }
}
