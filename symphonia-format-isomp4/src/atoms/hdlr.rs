// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::common::FourCc;
use symphonia_core::errors::{Error, Result};
use symphonia_core::io::ReadBytes;

use crate::atoms::{Atom, AtomHeader};

use log::warn;

/// Handler type.
#[derive(Debug, PartialEq, Eq)]
pub enum HandlerType {
    /// Video handler.
    Video,
    /// Audio handler.
    Sound,
    /// Subtitle handler.
    Subtitle,
    /// Metadata handler.
    Metadata,
    /// Text handler.
    Text,
    /// Unknown handler type.
    Other([u8; 4]),
}

/// Handler atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct HdlrAtom {
    /// Handler type.
    pub handler_type: HandlerType,
    /// Human-readable handler name.
    pub name: String,
}

impl Atom for HdlrAtom {
    fn read<B: ReadBytes>(reader: &mut B, mut header: AtomHeader) -> Result<Self> {
        let (_, _) = header.read_extended_header(reader)?;

        // Always 0 for MP4, but for Quicktime this contains the component type.
        let _ = reader.read_quad_bytes()?;

        let handler_type = match &reader.read_quad_bytes()? {
            b"vide" => HandlerType::Video,
            b"soun" => HandlerType::Sound,
            b"meta" => HandlerType::Metadata,
            b"subt" => HandlerType::Subtitle,
            b"text" => HandlerType::Text,
            &hdlr => {
                warn!("unknown handler type {:?}", FourCc::new(hdlr));
                HandlerType::Other(hdlr)
            }
        };

        // These bytes are reserved for MP4, but for QuickTime they contain the component
        // manufacturer, flags, and flags mask.
        reader.ignore_bytes(4 * 3)?;

        // Human readable UTF-8 string of the track type.
        let name = {
            let size = header.data_unread_at(reader.pos()).ok_or_else(|| {
                Error::DecodeError("isomp4 (hdlr): expected atom size to be known")
            })?;
            let buf = reader.read_boxed_slice_exact(size as usize)?;
            String::from_utf8_lossy(&buf).to_string()
        };

        Ok(HdlrAtom { handler_type, name })
    }
}
