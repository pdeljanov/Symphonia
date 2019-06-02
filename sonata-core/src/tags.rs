// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::fmt;

/// `StandardVisualKey` is an enumation providing standardized keys for common visual dispositions.
/// A demuxer may assign a `StandardVisualKey` to a `Visual` if the disposition of the attached 
/// visual is known and can be mapped to a standard key.
///
/// The visual types listed here are derived from, though do not entirely cover, the ID3v2 APIC 
/// frame specification.
#[derive(Copy, Clone, Debug)]
pub enum StandardVisualKey {
    FileIcon,
    OtherIcon,
    FrontCover,
    BackCover,
    Leaflet,
    Media,
    LeadArtistPerformerSoloist,
    ArtistPerformer,
    Conductor,
    BandOrchestra,
    Composer,
    Lyricist,
    RecordingLocation,
    RecordingSession,
    Performance,
    ScreenCapture,
    Illustration,
    BandArtistLogo,
    PublisherStudioLogo,
}

/// `StandardTagKey` is an enumation providing standardized keys for common tag types.
/// A tag reader may assign a `StandardTagKey` to a `Tag` if the tag's key is generally
/// accepted to map to a specific usage.
#[derive(Copy, Clone, Debug)]
pub enum StandardTagKey {
    Album,
    AlbumArtist,
    Arranger,
    Artist,
    Bpm,
    Comment,
    Compilation,
    Composer,
    Conductor,
    ContentGroup,
    Copyright,
    Date,
    Description,
    DiscNumber,
    DiscSubtitle,
    DiscTotal,
    EncodedBy,
    Encoder,
    EncoderSettings,
    Engineer,
    Ensemble,
    Genre,
    IdentBarcode,
    IdentCatalogNumber,
    IdentEanUpn,
    IdentIsrc,
    IdentPn,
    IdentUpc,
    Label,
    Language,
    License,
    Lyricist,
    Lyrics,
    MediaFormat,
    MixDj,
    MixEngineer,
    Mood,
    MusicBrainzAlbumArtistId,
    MusicBrainzAlbumId,
    MusicBrainzArtistId,
    MusicBrainzDiscId,
    MusicBrainzGenreId,
    MusicBrainzLabelId,
    MusicBrainzOriginalAlbumId,
    MusicBrainzOriginalArtistId,
    MusicBrainzRecordingId,
    MusicBrainzReleaseGroupId,
    MusicBrainzReleaseTrackId,
    MusicBrainzTrackId,
    MusicBrainzWorkId,
    Opus,
    OriginalDate,
    Part,
    PartTotal,
    Performer,
    Producer,
    Rating,
    ReleaseCountry,
    Remixer,
    ReplayGainAlbumGain,
    ReplayGainAlbumPeak,
    ReplayGainTrackGain,
    ReplayGainTrackPeak,
    Script,
    SortAlbum,
    SortAlbumArtist,
    SortArtist,
    SortTrackTitle,
    TrackNumber,
    TrackSubtitle,
    TrackTitle,
    TrackTotal,
    Version,
    Writer,
}

/// A `Tag` encapsulates a key-value pair of metadata.
pub struct Tag {
    /// If the `Tag`'s key string is commonly associated with a typical type, meaning, or purpose, then if recognized a 
    /// `StandardTagKey` will be assigned to this `Tag`. 
    /// 
    /// This is a best effort guess since not all metadata formats have a well defined or specified mapping. However, it
    /// is recommended that user's use `std_key` over `key` if provided.
    pub std_key: Option<StandardTagKey>,
    /// A key string indicating the type, meaning, or purpose of the `Tag`s value.
    pub key: String,
    /// The value of the `Tag`.
    pub value: String,
}

impl Tag {
    /// Create a new `Tag`.
    pub fn new(std_key: Option<StandardTagKey>, key: &str, value: &str) -> Tag {
        Tag {
            std_key: std_key,
            key: key.to_string(),
            value: value.to_string()
        }
    }

    /// Returns true if the `Tag`'s key string was recognized and a `StandardTagKey` was assigned, otherwise false is 
    /// returned.
    pub fn is_known(&self) -> bool {
        self.std_key.is_some()
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.std_key {
            Some(ref std_key) => write!(f, "{{ std_key={:?}, key=\"{}\", value=\"{}\" }}", std_key, self.key, self.value),
            None => write!(f, "{{ key=\"{}\", value=\"{}\" }}", self.key, self.value),
        }
    }
}

pub mod vorbis {
    //! Parsers and helper functions for reading, writing, and manipulating Vorbis Comment tags.

    use std::collections::HashMap;
    use lazy_static::lazy_static;
    use super::{Tag, StandardTagKey};

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
    pub fn parse(tag: &str) -> Tag {
        // Vorbis Comments (aka tags) are stored as <key>=<value> where <key> is
        // a reduced ASCII-only identifier and <value> is a UTF8 value.
        //
        // <Key> must only contain ASCII 0x20 through 0x7D, with 0x3D ('=') excluded.
        // ASCII 0x41 through 0x5A inclusive (A-Z) is to be considered equivalent to
        // ASCII 0x61 through 0x7A inclusive (a-z) for tag matching.

        let field: Vec<&str> = tag.splitn(2, "=").collect();

        // Attempt to assign a standardized tag key.
        let std_tag = match VORBIS_COMMENT_MAP.get(field[0].to_lowercase().as_str()) {
            Some(&tag) => Some(tag),
            None => None,
        };

        // The value field was empty so only the key field exists. Create an empty tag for the given key field.
        if field.len() == 1 {
            return Tag::new(std_tag, field[0], "");
        }

        Tag::new(std_tag, field[0], field[1])
    }
}

pub mod riff {
    //! Parsers and helper functions for reading, writing, and manipulating RIFF INFO tags.

    use std::collections::HashMap;
    use lazy_static::lazy_static;
    use super::{Tag, StandardTagKey};

    lazy_static! {
        static ref RIFF_INFO_MAP: HashMap<&'static str, StandardTagKey> = {
            let mut m = HashMap::new();
            m.insert("ages", StandardTagKey::Rating);
            m.insert("cmnt", StandardTagKey::Comment);
            // Is this the same as a cmnt?
            m.insert("comm", StandardTagKey::Comment);
            m.insert("dtim", StandardTagKey::OriginalDate);
            m.insert("genr", StandardTagKey::Genre);
            m.insert("iart", StandardTagKey::Artist);
            // Is this also  the same as cmnt?
            m.insert("icmt", StandardTagKey::Comment);
            m.insert("icop", StandardTagKey::Copyright);
            m.insert("icrd", StandardTagKey::Date);
            m.insert("idit", StandardTagKey::OriginalDate);
            m.insert("ienc", StandardTagKey::EncodedBy);
            m.insert("ieng", StandardTagKey::Engineer);
            m.insert("ifrm", StandardTagKey::TrackTotal);
            m.insert("ignr", StandardTagKey::Genre);
            m.insert("ilng", StandardTagKey::Language);
            m.insert("imus", StandardTagKey::Composer);
            m.insert("inam", StandardTagKey::TrackTitle);
            m.insert("iprd", StandardTagKey::Album);
            m.insert("ipro", StandardTagKey::Producer);
            m.insert("iprt", StandardTagKey::TrackNumber);
            m.insert("irtd", StandardTagKey::Rating);
            m.insert("isft", StandardTagKey::Encoder);
            m.insert("isgn", StandardTagKey::Genre);
            m.insert("isrf", StandardTagKey::MediaFormat);
            m.insert("itch", StandardTagKey::EncodedBy);
            m.insert("iwri", StandardTagKey::Writer);
            m.insert("lang", StandardTagKey::Language);
            m.insert("prt1", StandardTagKey::TrackNumber);
            m.insert("prt2", StandardTagKey::TrackTotal);
            // Same as inam?
            m.insert("titl", StandardTagKey::TrackTitle);
            m.insert("torg", StandardTagKey::Label);
            m.insert("trck", StandardTagKey::TrackNumber);
            m.insert("tver", StandardTagKey::Version);
            m.insert("year", StandardTagKey::Date);
            m
        };
    }

    /// Parse the RIFF INFO block into a `Tag` using the block's identifier tag and a slice containing the block's 
    /// contents.
    pub fn parse(tag: [u8; 4], buf: &[u8]) -> Tag {
        // TODO: Key should be checked that it only contains ASCII characters.
        let key = String::from_utf8_lossy(&tag);
        let value = String::from_utf8_lossy(buf);

        // Attempt to assign a standardized tag key.
        let std_tag = match RIFF_INFO_MAP.get(key.to_lowercase().as_str()) {
            Some(&tag) => Some(tag),
            None => None,
        };

        Tag::new(std_tag, &key, &value)
    }
}

pub mod id3v2 {
    //! Parsers and helper functions for reading, writing, and manipulating ID3v2 tags.

    use super::StandardVisualKey;

    /// Try to get a `StandardVisualKey` from the given APIC block identifier.
    pub fn visual_key_from_apic(apic: u32) -> Option<StandardVisualKey> {
        match apic {
            0x01 => Some(StandardVisualKey::FileIcon),
            0x02 => Some(StandardVisualKey::OtherIcon),
            0x03 => Some(StandardVisualKey::FrontCover),
            0x04 => Some(StandardVisualKey::BackCover),
            0x05 => Some(StandardVisualKey::Leaflet),
            0x06 => Some(StandardVisualKey::Media),
            0x07 => Some(StandardVisualKey::LeadArtistPerformerSoloist),
            0x08 => Some(StandardVisualKey::ArtistPerformer),
            0x09 => Some(StandardVisualKey::Conductor),
            0x0a => Some(StandardVisualKey::BandOrchestra),
            0x0b => Some(StandardVisualKey::Composer),
            0x0c => Some(StandardVisualKey::Lyricist),
            0x0d => Some(StandardVisualKey::RecordingLocation),
            0x0e => Some(StandardVisualKey::RecordingSession),
            0x0f => Some(StandardVisualKey::Performance),
            0x10 => Some(StandardVisualKey::ScreenCapture),
            0x12 => Some(StandardVisualKey::Illustration),
            0x13 => Some(StandardVisualKey::BandArtistLogo),
            0x14 => Some(StandardVisualKey::PublisherStudioLogo),
            _ => None,
        }
    }

}