// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A RIFF INFO metadata reader.

use symphonia_core::meta::{StandardTagKey, Tag, Value};

static RIFF_INFO_MAP: phf::Map<&'static str, StandardTagKey> = phf::phf_map! {
    "ages" => StandardTagKey::Rating,
    "cmnt" => StandardTagKey::Comment,
    // Is this the same as a cmnt?
    "comm" => StandardTagKey::Comment,
    "dtim" => StandardTagKey::OriginalDate,
    "genr" => StandardTagKey::Genre,
    "iart" => StandardTagKey::Artist,
    // Is this also  the same as cmnt?
    "icmt" => StandardTagKey::Comment,
    "icop" => StandardTagKey::Copyright,
    "icrd" => StandardTagKey::Date,
    "idit" => StandardTagKey::OriginalDate,
    "ienc" => StandardTagKey::EncodedBy,
    "ieng" => StandardTagKey::Engineer,
    "ifrm" => StandardTagKey::TrackTotal,
    "ignr" => StandardTagKey::Genre,
    "ilng" => StandardTagKey::Language,
    "imus" => StandardTagKey::Composer,
    "inam" => StandardTagKey::TrackTitle,
    "iprd" => StandardTagKey::Album,
    "ipro" => StandardTagKey::Producer,
    "iprt" => StandardTagKey::TrackNumber,
    "irtd" => StandardTagKey::Rating,
    "isft" => StandardTagKey::Encoder,
    "isgn" => StandardTagKey::Genre,
    "isrf" => StandardTagKey::MediaFormat,
    "itch" => StandardTagKey::EncodedBy,
    "iwri" => StandardTagKey::Writer,
    "lang" => StandardTagKey::Language,
    "prt1" => StandardTagKey::TrackNumber,
    "prt2" => StandardTagKey::TrackTotal,
    // Same as inam?
    "titl" => StandardTagKey::TrackTitle,
    "torg" => StandardTagKey::Label,
    "trck" => StandardTagKey::TrackNumber,
    "tver" => StandardTagKey::Version,
    "year" => StandardTagKey::Date
};

/// Parse the RIFF INFO block into a `Tag` using the block's identifier tag and a slice
/// containing the block's contents.
pub fn parse(tag: [u8; 4], buf: &[u8]) -> Tag {
    // TODO: Key should be checked that it only contains ASCII characters.
    let key = String::from_utf8_lossy(&tag);
    let value = String::from_utf8_lossy(buf);

    // Attempt to assign a standardized tag key.
    let std_tag = RIFF_INFO_MAP.get(key.to_lowercase().as_str()).copied();

    Tag::new(std_tag, &key, Value::from(value))
}
