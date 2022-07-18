// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! iTunes metadata support.

use symphonia_core::meta::StandardTagKey;

static ITUNES_TAG_MAP: phf::Map<&'static str, StandardTagKey> = phf::phf_map! {
    "com.apple.iTunes:ARTISTS" => StandardTagKey::Artist,
    "com.apple.iTunes:ASIN" => StandardTagKey::IdentAsin,
    "com.apple.iTunes:BARCODE" => StandardTagKey::IdentBarcode,
    "com.apple.iTunes:CATALOGNUMBER" => StandardTagKey::IdentCatalogNumber,
    "com.apple.iTunes:CONDUCTOR" => StandardTagKey::Conductor,
    "com.apple.iTunes:DISCSUBTITLE" => StandardTagKey::DiscSubtitle,
    "com.apple.iTunes:DJMIXER" => StandardTagKey::MixDj,
    "com.apple.iTunes:ENGINEER" => StandardTagKey::Engineer,
    "com.apple.iTunes:ISRC" => StandardTagKey::IdentIsrc,
    "com.apple.iTunes:LABEL" => StandardTagKey::Label,
    "com.apple.iTunes:LANGUAGE" => StandardTagKey::Language,
    "com.apple.iTunes:LICENSE" => StandardTagKey::License,
    "com.apple.iTunes:LYRICIST" => StandardTagKey::Lyricist,
    "com.apple.iTunes:MEDIA" => StandardTagKey::MediaFormat,
    "com.apple.iTunes:MIXER" => StandardTagKey::MixEngineer,
    "com.apple.iTunes:MOOD" => StandardTagKey::Mood,
    "com.apple.iTunes:MusicBrainz Album Artist Id" => StandardTagKey::MusicBrainzAlbumArtistId,
    "com.apple.iTunes:MusicBrainz Album Id" => StandardTagKey::MusicBrainzAlbumId,
    "com.apple.iTunes:MusicBrainz Album Release Country" => StandardTagKey::ReleaseCountry,
    "com.apple.iTunes:MusicBrainz Album Status" => StandardTagKey::MusicBrainzReleaseStatus,
    "com.apple.iTunes:MusicBrainz Album Type" => StandardTagKey::MusicBrainzReleaseType,
    "com.apple.iTunes:MusicBrainz Artist Id" => StandardTagKey::MusicBrainzArtistId,
    "com.apple.iTunes:MusicBrainz Release Group Id" => StandardTagKey::MusicBrainzReleaseGroupId,
    "com.apple.iTunes:MusicBrainz Release Track Id" => StandardTagKey::MusicBrainzReleaseTrackId,
    "com.apple.iTunes:MusicBrainz Track Id" => StandardTagKey::MusicBrainzTrackId,
    "com.apple.iTunes:MusicBrainz Work Id" => StandardTagKey::MusicBrainzWorkId,
    "com.apple.iTunes:originaldate" => StandardTagKey::OriginalDate,
    "com.apple.iTunes:PRODUCER" => StandardTagKey::Producer,
    "com.apple.iTunes:REMIXER" => StandardTagKey::Remixer,
    "com.apple.iTunes:SCRIPT" => StandardTagKey::Script,
    "com.apple.iTunes:SUBTITLE" => StandardTagKey::TrackSubtitle
};

/// Try to map the iTunes `tag` name to a `StandardTagKey`.
pub fn std_key_from_tag(key: &str) -> Option<StandardTagKey> {
    ITUNES_TAG_MAP.get(key).copied()
}
