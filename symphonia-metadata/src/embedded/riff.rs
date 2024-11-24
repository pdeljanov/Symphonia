// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! RIFF-based metadata formats reading.

use std::collections::HashMap;

use lazy_static::lazy_static;

use symphonia_core::errors::Result;
use symphonia_core::io::ReadBytes;
use symphonia_core::meta::{MetadataBuilder, MetadataSideData, RawTag};

use crate::utils::std_tag::*;

lazy_static! {
    static ref RIFF_INFO_MAP: RawTagParserMap = {
        let mut m: RawTagParserMap = HashMap::new();
        m.insert("ages", parse_rating);
        m.insert("cmnt", parse_comment);
        m.insert("comm", parse_comment); // TODO: Same as "cmnt"
        m.insert("dtim", parse_original_date);
        m.insert("genr", parse_genre);
        m.insert("iart", parse_artist);
        m.insert("icmt", parse_comment); // TODO: Same as "cmnt"?
        m.insert("icnt", parse_release_country);
        m.insert("icop", parse_copyright);
        m.insert("icrd", parse_date);
        m.insert("idit", parse_original_date);
        m.insert("ienc", parse_encoded_by);
        m.insert("ieng", parse_engineer);
        m.insert("ifrm", parse_track_total);
        m.insert("ignr", parse_genre);
        m.insert("ilng", parse_language);
        m.insert("imed", parse_media_format);
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
        m.insert("itoc", parse_cdtoc);
        m.insert("itrk", parse_track_number_exclusive);
        m.insert("iwri", parse_writer);
        m.insert("lang", parse_language);
        m.insert("prt1", parse_part_number_exclusive);
        m.insert("prt2", parse_part_total);
        m.insert("titl", parse_track_title); // TODO: Same as "inam"?
        m.insert("torg", parse_label);
        m.insert("trck", parse_track_number_exclusive);
        m.insert("tver", parse_version);
        m.insert("year", parse_date);
        m
    };
}

/// Parse the RIFF INFO block into a `Tag` using the block's identifier tag and a slice
/// containing the block's contents.
pub fn parse_riff_info_block(
    tag: [u8; 4],
    buf: &[u8],
    builder: &mut MetadataBuilder,
) -> Result<()> {
    // TODO: Key should be checked that it only contains ASCII characters.
    let key = String::from_utf8_lossy(&tag);
    let value = String::from_utf8_lossy(buf);

    let raw = RawTag::new(key, value);

    builder.add_mapped_tags(raw, &RIFF_INFO_MAP);
    Ok(())
}

/// Read a RIFF ID3 block.
pub fn read_riff_id3_block<B: ReadBytes>(
    reader: &mut B,
    builder: &mut MetadataBuilder,
    side_data: &mut Vec<MetadataSideData>,
) -> Result<()> {
    crate::id3v2::read_id3v2(reader, builder, side_data)
}
