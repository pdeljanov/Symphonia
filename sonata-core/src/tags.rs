// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This library is free software; you can redistribute it and/or
// modify it under the terms of the GNU Lesser General Public
// License as published by the Free Software Foundation; either
// version 2.1 of the License, or (at your option) any later version.
//
// This library is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
// Lesser General Public License for more details.
//
// You should have received a copy of the GNU Lesser General Public
// License along with this library; if not, write to the Free Software
// Foundation, Inc., 51 Franklin Street, Fifth Floor, Boston, MA  02110-1301 USA
use std::fmt;
use std::collections::HashMap;

use lazy_static::lazy_static;

/// `StandardVisualKey` is an enumation providing standardized keys for common visual dispositions.
/// A demuxer may assign a `StandardVisualKey` to a `Visual` if the disposition of the attached 
/// visual is known and can be mapped to a standard key.
///
/// The visual types listed here are derived from, though do not entirely cover, the ID3v2 APIC 
/// frame specification.
#[derive(Debug)]
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
    MusicBrainzArtistID,
    MusicBrainzGenreID,
    MusicBrainzLabelID,
    MusicBrainzReleaseID,
    MusicBrainzTrackID,
    Opus,
    OriginalDate,
    Part,
    PartTotal,
    Performer,
    Producer,
    Rating,
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

/// A `Tag` encapsulates the key-value pair of a media stream's metadata tag.
pub struct Tag {
    std_key: Option<StandardTagKey>,
    key: String,
    value: String,
}

impl Tag {
    pub fn new(std_key: Option<StandardTagKey>, key: &str, value: &str) -> Tag {
        Tag {
            std_key: std_key,
            key: key.to_string(),
            value: value.to_string()
        }
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

pub struct TagCollection {

}

lazy_static! {
    static ref RIFF_COMMENT_MAP: HashMap<&'static str, StandardTagKey> = {
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
        m.insert("ilng", StandardTagKey::Language);
        m.insert("imus", StandardTagKey::Composer);
        m.insert("inam", StandardTagKey::TrackTitle);
        m.insert("iprd", StandardTagKey::Album);
        m.insert("ipro", StandardTagKey::Producer);
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

lazy_static! {
    static ref VORBIS_COMMENT_MAP: HashMap<&'static str, StandardTagKey> = {
        let mut m = HashMap::new();
        m.insert("album artist"         , StandardTagKey::AlbumArtist);
        m.insert("album"                , StandardTagKey::Album);
        m.insert("albumartist"          , StandardTagKey::AlbumArtist);
        m.insert("albumartistsort"      , StandardTagKey::SortAlbumArtist);
        m.insert("albumsort"            , StandardTagKey::SortAlbum);
        m.insert("arranger"             , StandardTagKey::Arranger);
        m.insert("artist"               , StandardTagKey::Artist);
        m.insert("artistsort"           , StandardTagKey::SortArtist);
        // TODO: Is Author a synonym for Writer?
        m.insert("author"               , StandardTagKey::Writer);
        m.insert("barcode"              , StandardTagKey::IdentBarcode);
        m.insert("bpm"                  , StandardTagKey::Bpm);
        m.insert("catalog #"            , StandardTagKey::IdentCatalogNumber);
        m.insert("catalog"              , StandardTagKey::IdentCatalogNumber);
        m.insert("catalognumber"        , StandardTagKey::IdentCatalogNumber);
        m.insert("catalogue #"          , StandardTagKey::IdentCatalogNumber);
        m.insert("comment"              , StandardTagKey::Comment);
        m.insert("compileation"         , StandardTagKey::Compilation);
        m.insert("composer"             , StandardTagKey::Composer);
        m.insert("conductor"            , StandardTagKey::Conductor);
        m.insert("copyright"            , StandardTagKey::Copyright);
        m.insert("date"                 , StandardTagKey::Date);
        m.insert("description"          , StandardTagKey::Description);
        m.insert("disc"                 , StandardTagKey::DiscNumber);
        m.insert("discnumber"           , StandardTagKey::DiscNumber);
        m.insert("discsubtitle"         , StandardTagKey::DiscSubtitle);
        m.insert("disctotal"            , StandardTagKey::DiscTotal);
        m.insert("djmixer"              , StandardTagKey::MixDj);
        m.insert("ean/upn"              , StandardTagKey::IdentEanUpn);
        m.insert("encoded-by"           , StandardTagKey::EncodedBy);
        m.insert("encoder settings"     , StandardTagKey::EncoderSettings);
        m.insert("encoder"              , StandardTagKey::Encoder);
        m.insert("encoding"             , StandardTagKey::EncoderSettings);
        m.insert("engineer"             , StandardTagKey::Engineer);
        m.insert("ensemble"             , StandardTagKey::Ensemble);
        m.insert("genre"                , StandardTagKey::Genre);
        m.insert("isrc"                 , StandardTagKey::IdentIsrc);
        m.insert("language"             , StandardTagKey::Language);
        m.insert("license"              , StandardTagKey::License);
        m.insert("lyricist"             , StandardTagKey::Lyricist);
        m.insert("lyrics"               , StandardTagKey::Lyrics);
        m.insert("media"                , StandardTagKey::MediaFormat);
        m.insert("mixer"                , StandardTagKey::MixEngineer);
        m.insert("mood"                 , StandardTagKey::Mood);
        m.insert("opus"                 , StandardTagKey::Opus);
        m.insert("organization"         , StandardTagKey::Label);
        m.insert("originaldate"         , StandardTagKey::OriginalDate);
        m.insert("part"                 , StandardTagKey::Part);
        m.insert("performer"            , StandardTagKey::Performer);
        m.insert("producer"             , StandardTagKey::Producer);
        m.insert("productnumber"        , StandardTagKey::IdentPn);
        // TODO: Is Publisher a synonym for Label?
        m.insert("publisher"            , StandardTagKey::Label);
        m.insert("rating"               , StandardTagKey::Rating);
        m.insert("remixer"              , StandardTagKey::Remixer);
        m.insert("replaygain_album_gain", StandardTagKey::ReplayGainAlbumGain);
        m.insert("replaygain_album_peak", StandardTagKey::ReplayGainAlbumPeak);
        m.insert("replaygain_track_gain", StandardTagKey::ReplayGainTrackGain);
        m.insert("replaygain_track_peak", StandardTagKey::ReplayGainTrackPeak);
        m.insert("script"               , StandardTagKey::Script);
        m.insert("subtitle"             , StandardTagKey::TrackSubtitle);
        m.insert("title"                , StandardTagKey::TrackTitle);
        m.insert("titlesort"            , StandardTagKey::SortTrackTitle);
        m.insert("totaldiscs"           , StandardTagKey::DiscTotal);
        m.insert("totaltracks"          , StandardTagKey::TrackTotal);
        m.insert("tracknumber"          , StandardTagKey::TrackNumber);
        m.insert("tracktotal"           , StandardTagKey::TrackTotal);
        m.insert("upc"                  , StandardTagKey::IdentUpc);
        m.insert("version"              , StandardTagKey::Remixer);
        m.insert("version"              , StandardTagKey::Version);
        m.insert("writer"               , StandardTagKey::Writer);
        m.insert("year"                 , StandardTagKey::Date);
        m
    };
}

pub struct VorbisTag;

impl VorbisTag {
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
