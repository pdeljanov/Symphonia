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
    AcoustidFingerprint,
    AcoustidId,
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
    EncodingDate,
    Engineer,
    Ensemble,
    Genre,
    IdentBarcode,
    IdentCatalogNumber,
    IdentEanUpn,
    IdentIsrc,
    IdentPn,
    IdentPodcast,
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
    MovementName,
    MovementNumber,
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
    OriginalAlbum,
    OriginalArtist,
    OriginalDate,
    OriginalFile,
    OriginalWriter,
    Part,
    PartTotal,
    Performer,
    Podcast,
    PodcastCategory,
    PodcastDescription,
    PodcastKeywords,
    Producer,
    Rating,
    ReleaseCountry,
    ReleaseDate,
    Remixer,
    ReplayGainAlbumGain,
    ReplayGainAlbumPeak,
    ReplayGainTrackGain,
    ReplayGainTrackPeak,
    Script,
    SortAlbum,
    SortAlbumArtist,
    SortArtist,
    SortComposer,
    SortTrackTitle,
    TaggingDate,
    TrackNumber,
    TrackSubtitle,
    TrackTitle,
    TrackTotal,
    Url,
    UrlArtist,
    UrlCopyright,
    UrlInternetRadio,
    UrlLabel,
    UrlOfficial,
    UrlPayment,
    UrlPodcast,
    UrlPurchase,
    UrlSource,
    Version,
    Writer,
}

/// A `Tag` encapsulates a key-value pair of metadata.
pub struct Tag {
    /// If the `Tag`'s key string is commonly associated with a typical type, meaning, or purpose,
    /// then if recognized a `StandardTagKey` will be assigned to this `Tag`. 
    /// 
    /// This is a best effort guess since not all metadata formats have a well defined or specified
    /// mapping. However, it is recommended that user's use `std_key` over `key` if provided.
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
            std_key,
            key: key.to_string(),
            value: value.to_string()
        }
    }

    /// Returns true if the `Tag`'s key string was recognized and a `StandardTagKey` was assigned,
    /// otherwise false is returned.
    pub fn is_known(&self) -> bool {
        self.std_key.is_some()
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.std_key {
            Some(ref std_key) =>
                write!(
                    f,
                    "{{ std_key={:?}, key=\"{}\", value=\"{}\" }}",
                    std_key,
                    self.key,
                    self.value
                ),
            None => write!(f, "{{ key=\"{}\", value=\"{}\" }}", self.key, self.value),
        }
    }
}