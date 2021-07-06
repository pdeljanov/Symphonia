// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{Result, decode_error};
use symphonia_core::io::ReadBytes;

use crate::atoms::{Atom, AtomHeader};

/// Type of track.
#[derive(Debug, PartialEq)]
pub enum TrackType {
    /// Video track.
    Video,
    /// Audio track.
    Sound,
    /// Subtitle track.
    Subtitle,
    /// Metadata track.
    Metadata,
    /// Text track.
    Text,
}

/// Handler atom.
#[derive(Debug)]
pub struct HdlrAtom {
    /// Atom header.
    header: AtomHeader,
    /// Track type.
    pub track_type: TrackType,
    /// Name of component.
    pub name: String,
}

impl Atom for HdlrAtom {
    fn header(&self) -> AtomHeader {
        self.header
    }

    fn read<B: ReadBytes>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        let (_, _) = AtomHeader::read_extra(reader)?;

        // Ignore the component type.
        let _ = reader.read_quad_bytes()?;

        let track_type = match &reader.read_quad_bytes()? {
            b"vide" => TrackType::Video,
            b"soun" => TrackType::Sound,
            b"meta" => TrackType::Metadata,
            b"subt" => TrackType::Subtitle,
            b"text" => TrackType::Text,
            _ => {
                return decode_error("illegal track type")
            }
        };

        // Ignore component manufacturer, flags, and flags mask.
        reader.ignore_bytes(4 * 3)?;

        // Component name occupies the remainder of the atom.
        let mut buf = vec![0; (header.data_len - 24) as usize];
        reader.read_buf_exact(&mut buf)?;

        let name = String::from_utf8(buf).unwrap_or(String::from("(err)"));
        
        Ok(HdlrAtom {
            header,
            track_type,
            name,
        })
    }

}
