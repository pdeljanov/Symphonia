// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! An ID3v1 metadata reader.

use std::borrow::Cow;
use std::sync::Arc;

use symphonia_core::errors::{unsupported_error, Result};
use symphonia_core::formats::probe::{
    Anchors, ProbeMetadataData, ProbeableMetadata, Score, Scoreable,
};
use symphonia_core::io::{MediaSourceStream, ReadBytes, ScopedStream};
use symphonia_core::meta::well_known::METADATA_ID_ID3V1;
use symphonia_core::meta::{
    MetadataBuffer, MetadataBuilder, MetadataInfo, MetadataOptions, MetadataReader, StandardTag,
    Tag,
};
use symphonia_core::support_metadata;

use encoding_rs::WINDOWS_1252;

use crate::utils::id3v1::get_genre_name;

fn read_id3v1<B: ReadBytes>(reader: &mut B, builder: &mut MetadataBuilder) -> Result<()> {
    // Read the "TAG" header.
    let marker = reader.read_triple_bytes()?;

    if marker != *b"TAG" {
        return unsupported_error("id3v1: Not an ID3v1 tag");
    }

    let mut buf = [0u8; 125];
    reader.read_buf_exact(&mut buf)?;

    if let Some(title) = decode_iso8859_buf(&buf[0..30]) {
        let tag = Tag::new_from_parts("TITLE", title.clone(), Some(StandardTag::TrackTitle(title)));
        builder.add_tag(tag);
    }

    if let Some(artist) = decode_iso8859_buf(&buf[30..60]) {
        let tag = Tag::new_from_parts("ARTIST", artist.clone(), Some(StandardTag::Artist(artist)));
        builder.add_tag(tag);
    }

    if let Some(album) = decode_iso8859_buf(&buf[60..90]) {
        let tag = Tag::new_from_parts("ALBUM", album.clone(), Some(StandardTag::Album(album)));
        builder.add_tag(tag);
    }

    if let Some(year) = decode_iso8859_buf(&buf[90..94]) {
        let tag = Tag::new_from_parts("DATE", year.clone(), Some(StandardTag::Date(year)));
        builder.add_tag(tag);
    }

    // If the second-last byte of the comment field is 0 (indicating the remaining characters are
    // also 0), then the last byte of the comment field is the track number.
    let comment = if buf[122] == 0 {
        // The last byte of the comment field is the track number.
        let track = u64::from(buf[123]);

        builder.add_tag(Tag::new_from_parts("TRACK", track, Some(StandardTag::TrackNumber(track))));

        decode_iso8859_buf(&buf[94..122])
    }
    else {
        decode_iso8859_buf(&buf[94..124])
    };

    if let Some(comment) = comment {
        let tag =
            Tag::new_from_parts("COMMENT", comment.clone(), Some(StandardTag::Comment(comment)));
        builder.add_tag(tag);
    }

    // Convert the genre index to an actual genre name using the GENRES lookup table.
    if let Some(genre) = get_genre_name(buf[124]).map(Arc::new) {
        let tag = Tag::new_from_parts("GENRE", genre.clone(), Some(StandardTag::Genre(genre)));
        builder.add_tag(tag);
    }

    Ok(())
}

fn decode_iso8859_buf(buf: &[u8]) -> Option<Arc<String>> {
    // Trim the buffer upto a null-terminator.
    let buf = match buf.iter().position(|&b| b == b'\0') {
        Some(i) => &buf[..i],
        None => buf,
    };

    // Decode as Windows code page 1252 (a superset of ISO-8859-1).
    let text = WINDOWS_1252.decode(buf).0;

    // Replace all control characters that should not be there for ISO-8859-1 with the Unicode
    // replacement character (U+FFFD).
    let text = if text.chars().any(|c| c.is_ascii_control()) {
        let replaced: String = text
            .chars()
            .map(|c| {
                if c.is_ascii_control() {
                    '\u{FFFD}'
                }
                else {
                    c
                }
            })
            .collect();
        Cow::Owned(replaced)
    }
    else {
        // No control characters found. Return the original string.
        text
    };

    // Do not return an empty string.
    if !text.is_empty() {
        Some(Arc::new(text.into_owned()))
    }
    else {
        None
    }
}

const ID3V1_METADATA_INFO: MetadataInfo =
    MetadataInfo { metadata: METADATA_ID_ID3V1, short_name: "id3v1", long_name: "ID3v1" };

/// ID3v1 tag reader.
pub struct Id3v1Reader<'s> {
    reader: MediaSourceStream<'s>,
}

impl<'s> Id3v1Reader<'s> {
    pub fn try_new(mss: MediaSourceStream<'s>, _opts: MetadataOptions) -> Result<Self> {
        Ok(Self { reader: mss })
    }
}

impl Scoreable for Id3v1Reader<'_> {
    fn score(_src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        Ok(Score::Supported(255))
    }
}

impl ProbeableMetadata<'_> for Id3v1Reader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: MetadataOptions,
    ) -> Result<Box<dyn MetadataReader + '_>>
    where
        Self: Sized,
    {
        Ok(Box::new(Id3v1Reader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeMetadataData] {
        &[support_metadata!(ID3V1_METADATA_INFO, &[], &[], &[b"TAG"], Anchors::Exclusive(&[128]))]
    }
}

impl MetadataReader for Id3v1Reader<'_> {
    fn metadata_info(&self) -> &MetadataInfo {
        &ID3V1_METADATA_INFO
    }

    fn read_all(&mut self) -> Result<MetadataBuffer> {
        let mut builder = MetadataBuilder::new();
        read_id3v1(&mut self.reader, &mut builder)?;
        Ok(MetadataBuffer { revision: builder.metadata(), side_data: Vec::new() })
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}
