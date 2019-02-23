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
/// A demuxer may assign a `StandardTagKey` to a `Tag` if the tag's key is generally
/// mapped to a specific disposition.
#[derive(Debug)]
pub enum StandardTagKey {
    TrackTitle,
    Artist,
    Release,
    TrackNumber,
    Genre,
    Rating,
    Language,
    Date,
    Composer,
    Lyricist,
    Writer,
    Conductor,
    Performer,
    Remixer,
    Arranger,
    Engineer,
    Producer,
    MixDJ,
    MixEngineer,
    Label,
    MusicBrainzTrackID,
    MusicBrainzReleaseID,
    MusicBrainzGenreID,
    MusicBrainzLabelID,
    MusicBrainzArtistID,
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
            Some(ref std_key) => write!(f, "{{ \"{}\": \"{}\" [{:?}] }}", self.key, self.value, std_key),
            None =>  write!(f, "{{ \"{}\": \"{}\" }}", self.key, self.value)
        }
    }
}

pub struct TagCollection {

}
