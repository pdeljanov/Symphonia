// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A RIFF INFO metadata reader.

use std::collections::HashMap;

use lazy_static::lazy_static;

use symphonia_core::errors::Result;
use symphonia_core::meta::{MetadataBuilder, RawTag};

use crate::utils::std_tag::*;

lazy_static! {
    static ref RIFF_INFO_MAP: RawTagParserMap = {
        let mut m: RawTagParserMap = HashMap::new();
        m.insert("ages", parse_rating);
        m.insert("cmnt", parse_comment);
        // Is this the same as a cmnt?
        m.insert("comm", parse_comment);
        m.insert("dtim", parse_original_date);
        m.insert("genr", parse_genre);
        m.insert("iart", parse_artist);
        // Is this also  the same as cmnt?
        m.insert("icmt", parse_comment);
        m.insert("icop", parse_copyright);
        m.insert("icrd", parse_date);
        m.insert("idit", parse_original_date);
        m.insert("ienc", parse_encoded_by);
        m.insert("ieng", parse_engineer);
        m.insert("ifrm", parse_track_total);
        m.insert("ignr", parse_genre);
        m.insert("ilng", parse_language);
        m.insert("imus", parse_composer);
        m.insert("inam", parse_track_title);
        m.insert("iprd", parse_album);
        m.insert("ipro", parse_producer);
        m.insert("iprt", parse_track_number_exclusive);
        m.insert("irtd", parse_rating);
        m.insert("isft", parse_encoder);
        m.insert("isgn", parse_genre);
        m.insert("isrf", parse_media_format);
        m.insert("itch", parse_encoded_by);
        m.insert("itrk", parse_track_number_exclusive);
        m.insert("iwri", parse_writer);
        m.insert("lang", parse_language);
        m.insert("prt1", parse_part_number_exclusive);
        m.insert("prt2", parse_part_total);
        // Same as inam?
        m.insert("titl", parse_track_title);
        m.insert("torg", parse_label);
        m.insert("trck", parse_track_number_exclusive);
        m.insert("tver", parse_version);
        m.insert("year", parse_date);
        m
    };
}

/// Parse the RIFF INFO block into a `Tag` using the block's identifier tag and a slice
/// containing the block's contents.
pub fn read_riff_info_block(tag: [u8; 4], buf: &[u8], builder: &mut MetadataBuilder) -> Result<()> {
    // TODO: Key should be checked that it only contains ASCII characters.
    let key = String::from_utf8_lossy(&tag);
    let value = String::from_utf8_lossy(buf);

    let raw = RawTag::new(key, value);

    builder.add_mapped_tags(raw, &RIFF_INFO_MAP);
    Ok(())
}
