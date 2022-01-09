// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A Vorbic COMMENT metadata reader for FLAC or OGG formats.

use lazy_static::lazy_static;
use std::collections::HashMap;
use symphonia_core::errors::Result;
use symphonia_core::io::ReadBytes;
use symphonia_core::meta::{MetadataBuilder, StandardTagKey, Tag, Value};

lazy_static! {
    static ref VORBIS_COMMENT_MAP: HashMap<&'static str, StandardTagKey> = {
        let mut m = HashMap::new();
        m.insert("album artist"                , StandardTagKey::AlbumArtist);
        m.insert("album"                       , StandardTagKey::Album);
        m.insert("albumartist"                 , StandardTagKey::AlbumArtist);
        m.insert("albumartistsort"             , StandardTagKey::SortAlbumArtist);
        m.insert("albumsort"                   , StandardTagKey::SortAlbum);
        m.insert("arranger"                    , StandardTagKey::Arranger);
        m.insert("artist"                      , StandardTagKey::Artist);
        m.insert("artistsort"                  , StandardTagKey::SortArtist);
        // TODO: Is Author a synonym for Writer?
        m.insert("author"                      , StandardTagKey::Writer);
        m.insert("barcode"                     , StandardTagKey::IdentBarcode);
        m.insert("bpm"                         , StandardTagKey::Bpm);
        m.insert("catalog #"                   , StandardTagKey::IdentCatalogNumber);
        m.insert("catalog"                     , StandardTagKey::IdentCatalogNumber);
        m.insert("catalognumber"               , StandardTagKey::IdentCatalogNumber);
        m.insert("catalogue #"                 , StandardTagKey::IdentCatalogNumber);
        m.insert("comment"                     , StandardTagKey::Comment);
        m.insert("compileation"                , StandardTagKey::Compilation);
        m.insert("composer"                    , StandardTagKey::Composer);
        m.insert("conductor"                   , StandardTagKey::Conductor);
        m.insert("copyright"                   , StandardTagKey::Copyright);
        m.insert("date"                        , StandardTagKey::Date);
        m.insert("description"                 , StandardTagKey::Description);
        m.insert("disc"                        , StandardTagKey::DiscNumber);
        m.insert("discnumber"                  , StandardTagKey::DiscNumber);
        m.insert("discsubtitle"                , StandardTagKey::DiscSubtitle);
        m.insert("disctotal"                   , StandardTagKey::DiscTotal);
        m.insert("disk"                        , StandardTagKey::DiscNumber);
        m.insert("disknumber"                  , StandardTagKey::DiscNumber);
        m.insert("disksubtitle"                , StandardTagKey::DiscSubtitle);
        m.insert("disktotal"                   , StandardTagKey::DiscTotal);
        m.insert("djmixer"                     , StandardTagKey::MixDj);
        m.insert("ean/upn"                     , StandardTagKey::IdentEanUpn);
        m.insert("encoded-by"                  , StandardTagKey::EncodedBy);
        m.insert("encoder settings"            , StandardTagKey::EncoderSettings);
        m.insert("encoder"                     , StandardTagKey::Encoder);
        m.insert("encoding"                    , StandardTagKey::EncoderSettings);
        m.insert("engineer"                    , StandardTagKey::Engineer);
        m.insert("ensemble"                    , StandardTagKey::Ensemble);
        m.insert("genre"                       , StandardTagKey::Genre);
        m.insert("isrc"                        , StandardTagKey::IdentIsrc);
        m.insert("language"                    , StandardTagKey::Language);
        m.insert("label"                       , StandardTagKey::Label);
        m.insert("license"                     , StandardTagKey::License);
        m.insert("lyricist"                    , StandardTagKey::Lyricist);
        m.insert("lyrics"                      , StandardTagKey::Lyrics);
        m.insert("media"                       , StandardTagKey::MediaFormat);
        m.insert("mixer"                       , StandardTagKey::MixEngineer);
        m.insert("mood"                        , StandardTagKey::Mood);
        m.insert("musicbrainz_albumartistid"   , StandardTagKey::MusicBrainzAlbumArtistId);
        m.insert("musicbrainz_albumid"         , StandardTagKey::MusicBrainzAlbumId);
        m.insert("musicbrainz_artistid"        , StandardTagKey::MusicBrainzArtistId);
        m.insert("musicbrainz_discid"          , StandardTagKey::MusicBrainzDiscId);
        m.insert("musicbrainz_originalalbumid" , StandardTagKey::MusicBrainzOriginalAlbumId);
        m.insert("musicbrainz_originalartistid", StandardTagKey::MusicBrainzOriginalArtistId);
        m.insert("musicbrainz_recordingid"     , StandardTagKey::MusicBrainzRecordingId);
        m.insert("musicbrainz_releasegroupid"  , StandardTagKey::MusicBrainzReleaseGroupId);
        m.insert("musicbrainz_releasetrackid"  , StandardTagKey::MusicBrainzReleaseTrackId);
        m.insert("musicbrainz_trackid"         , StandardTagKey::MusicBrainzTrackId);
        m.insert("musicbrainz_workid"          , StandardTagKey::MusicBrainzWorkId);
        m.insert("opus"                        , StandardTagKey::Opus);
        m.insert("organization"                , StandardTagKey::Label);
        m.insert("originaldate"                , StandardTagKey::OriginalDate);
        m.insert("part"                        , StandardTagKey::Part);
        m.insert("performer"                   , StandardTagKey::Performer);
        m.insert("producer"                    , StandardTagKey::Producer);
        m.insert("productnumber"               , StandardTagKey::IdentPn);
        // TODO: Is Publisher a synonym for Label?
        m.insert("publisher"                   , StandardTagKey::Label);
        m.insert("rating"                      , StandardTagKey::Rating);
        m.insert("releasecountry"              , StandardTagKey::ReleaseCountry);
        m.insert("remixer"                     , StandardTagKey::Remixer);
        m.insert("replaygain_album_gain"       , StandardTagKey::ReplayGainAlbumGain);
        m.insert("replaygain_album_peak"       , StandardTagKey::ReplayGainAlbumPeak);
        m.insert("replaygain_track_gain"       , StandardTagKey::ReplayGainTrackGain);
        m.insert("replaygain_track_peak"       , StandardTagKey::ReplayGainTrackPeak);
        m.insert("script"                      , StandardTagKey::Script);
        m.insert("subtitle"                    , StandardTagKey::TrackSubtitle);
        m.insert("title"                       , StandardTagKey::TrackTitle);
        m.insert("titlesort"                   , StandardTagKey::SortTrackTitle);
        m.insert("totaldiscs"                  , StandardTagKey::DiscTotal);
        m.insert("totaltracks"                 , StandardTagKey::TrackTotal);
        m.insert("tracknumber"                 , StandardTagKey::TrackNumber);
        m.insert("tracktotal"                  , StandardTagKey::TrackTotal);
        m.insert("upc"                         , StandardTagKey::IdentUpc);
        m.insert("version"                     , StandardTagKey::Remixer);
        m.insert("version"                     , StandardTagKey::Version);
        m.insert("writer"                      , StandardTagKey::Writer);
        m.insert("year"                        , StandardTagKey::Date);
        m
    };
}

/// Parse the given Vorbis Comment string into a `Tag`.
fn parse(tag: &str) -> Tag {
    // Vorbis Comments (aka tags) are stored as <key>=<value> where <key> is
    // a reduced ASCII-only identifier and <value> is a UTF8 value.
    //
    // <Key> must only contain ASCII 0x20 through 0x7D, with 0x3D ('=') excluded.
    // ASCII 0x41 through 0x5A inclusive (A-Z) is to be considered equivalent to
    // ASCII 0x61 through 0x7A inclusive (a-z) for tag matching.

    let field: Vec<&str> = tag.splitn(2, '=').collect();

    // Attempt to assign a standardized tag key.
    let std_tag = VORBIS_COMMENT_MAP.get(field[0].to_lowercase().as_str()).copied();

    // The value field was empty so only the key field exists. Create an empty tag for the given
    // key field.
    if field.len() == 1 {
        return Tag::new(std_tag, field[0], Value::from(""));
    }

    Tag::new(std_tag, field[0], Value::from(field[1]))
}

pub fn read_comment_no_framing<B: ReadBytes>(
    reader: &mut B,
    metadata: &mut MetadataBuilder,
) -> Result<()> {
    // Read the vendor string length in bytes.
    let vendor_length = reader.read_u32()?;

    // Ignore the vendor string.
    reader.ignore_bytes(u64::from(vendor_length))?;

    // Read the number of comments.
    let n_comments = reader.read_u32()? as usize;

    for _ in 0..n_comments {
        // Read the comment string length in bytes.
        let comment_length = reader.read_u32()?;

        // Read the comment string.
        let mut comment_byte = vec![0; comment_length as usize];
        reader.read_buf_exact(&mut comment_byte)?;

        // Parse the comment string into a Tag and insert it into the parsed tag list.
        metadata.add_tag(parse(&String::from_utf8_lossy(&comment_byte)));
    }

    Ok(())
}
