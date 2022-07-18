// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A Vorbic COMMENT metadata reader for FLAC or OGG formats.

use symphonia_core::errors::Result;
use symphonia_core::io::ReadBytes;
use symphonia_core::meta::{MetadataBuilder, StandardTagKey, Tag, Value};

static VORBIS_COMMENT_MAP: phf::Map<&'static str, StandardTagKey> = phf::phf_map! {
    "album artist"                => StandardTagKey::AlbumArtist,
    "album"                       => StandardTagKey::Album,
    "albumartist"                 => StandardTagKey::AlbumArtist,
    "albumartistsort"             => StandardTagKey::SortAlbumArtist,
    "albumsort"                   => StandardTagKey::SortAlbum,
    "arranger"                    => StandardTagKey::Arranger,
    "artist"                      => StandardTagKey::Artist,
    "artistsort"                  => StandardTagKey::SortArtist,
    // TODO: Is Author a synonym for Writer?
    "author"                      => StandardTagKey::Writer,
    "barcode"                     => StandardTagKey::IdentBarcode,
    "bpm"                         => StandardTagKey::Bpm,
    "catalog #"                   => StandardTagKey::IdentCatalogNumber,
    "catalog"                     => StandardTagKey::IdentCatalogNumber,
    "catalognumber"               => StandardTagKey::IdentCatalogNumber,
    "catalogue #"                 => StandardTagKey::IdentCatalogNumber,
    "comment"                     => StandardTagKey::Comment,
    "compileation"                => StandardTagKey::Compilation,
    "composer"                    => StandardTagKey::Composer,
    "conductor"                   => StandardTagKey::Conductor,
    "copyright"                   => StandardTagKey::Copyright,
    "date"                        => StandardTagKey::Date,
    "description"                 => StandardTagKey::Description,
    "disc"                        => StandardTagKey::DiscNumber,
    "discnumber"                  => StandardTagKey::DiscNumber,
    "discsubtitle"                => StandardTagKey::DiscSubtitle,
    "disctotal"                   => StandardTagKey::DiscTotal,
    "disk"                        => StandardTagKey::DiscNumber,
    "disknumber"                  => StandardTagKey::DiscNumber,
    "disksubtitle"                => StandardTagKey::DiscSubtitle,
    "disktotal"                   => StandardTagKey::DiscTotal,
    "djmixer"                     => StandardTagKey::MixDj,
    "ean/upn"                     => StandardTagKey::IdentEanUpn,
    "encoded-by"                  => StandardTagKey::EncodedBy,
    "encoder settings"            => StandardTagKey::EncoderSettings,
    "encoder"                     => StandardTagKey::Encoder,
    "encoding"                    => StandardTagKey::EncoderSettings,
    "engineer"                    => StandardTagKey::Engineer,
    "ensemble"                    => StandardTagKey::Ensemble,
    "genre"                       => StandardTagKey::Genre,
    "isrc"                        => StandardTagKey::IdentIsrc,
    "language"                    => StandardTagKey::Language,
    "label"                       => StandardTagKey::Label,
    "license"                     => StandardTagKey::License,
    "lyricist"                    => StandardTagKey::Lyricist,
    "lyrics"                      => StandardTagKey::Lyrics,
    "media"                       => StandardTagKey::MediaFormat,
    "mixer"                       => StandardTagKey::MixEngineer,
    "mood"                        => StandardTagKey::Mood,
    "musicbrainz_albumartistid"   => StandardTagKey::MusicBrainzAlbumArtistId,
    "musicbrainz_albumid"         => StandardTagKey::MusicBrainzAlbumId,
    "musicbrainz_artistid"        => StandardTagKey::MusicBrainzArtistId,
    "musicbrainz_discid"          => StandardTagKey::MusicBrainzDiscId,
    "musicbrainz_originalalbumid" => StandardTagKey::MusicBrainzOriginalAlbumId,
    "musicbrainz_originalartistid"=> StandardTagKey::MusicBrainzOriginalArtistId,
    "musicbrainz_recordingid"     => StandardTagKey::MusicBrainzRecordingId,
    "musicbrainz_releasegroupid"  => StandardTagKey::MusicBrainzReleaseGroupId,
    "musicbrainz_releasetrackid"  => StandardTagKey::MusicBrainzReleaseTrackId,
    "musicbrainz_trackid"         => StandardTagKey::MusicBrainzTrackId,
    "musicbrainz_workid"          => StandardTagKey::MusicBrainzWorkId,
    "opus"                        => StandardTagKey::Opus,
    "organization"                => StandardTagKey::Label,
    "originaldate"                => StandardTagKey::OriginalDate,
    "part"                        => StandardTagKey::Part,
    "performer"                   => StandardTagKey::Performer,
    "producer"                    => StandardTagKey::Producer,
    "productnumber"               => StandardTagKey::IdentPn,
    // TODO: Is Publisher a synonym for Label?
    "publisher"                   => StandardTagKey::Label,
    "rating"                      => StandardTagKey::Rating,
    "releasecountry"              => StandardTagKey::ReleaseCountry,
    "remixer"                     => StandardTagKey::Remixer,
    "replaygain_album_gain"       => StandardTagKey::ReplayGainAlbumGain,
    "replaygain_album_peak"       => StandardTagKey::ReplayGainAlbumPeak,
    "replaygain_track_gain"       => StandardTagKey::ReplayGainTrackGain,
    "replaygain_track_peak"       => StandardTagKey::ReplayGainTrackPeak,
    "script"                      => StandardTagKey::Script,
    "subtitle"                    => StandardTagKey::TrackSubtitle,
    "title"                       => StandardTagKey::TrackTitle,
    "titlesort"                   => StandardTagKey::SortTrackTitle,
    "totaldiscs"                  => StandardTagKey::DiscTotal,
    "totaltracks"                 => StandardTagKey::TrackTotal,
    "tracknumber"                 => StandardTagKey::TrackNumber,
    "tracktotal"                  => StandardTagKey::TrackTotal,
    "upc"                         => StandardTagKey::IdentUpc,
    "version"                     => StandardTagKey::Version,
    "writer"                      => StandardTagKey::Writer,
    "year"                        => StandardTagKey::Date,
};

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
