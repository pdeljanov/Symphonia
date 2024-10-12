// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! An APEv1 and APEv2 metadata reader.

use std::collections::HashMap;
use std::io::{Seek, SeekFrom};

use symphonia_core::errors::{decode_error, unsupported_error, Result};
use symphonia_core::formats::probe::{
    Anchors, ProbeMetadataData, ProbeableMetadata, Score, Scoreable,
};
use symphonia_core::io::{MediaSourceStream, ReadBytes, ScopedStream, SeekBuffered};
use symphonia_core::meta::well_known::{METADATA_ID_APEV1, METADATA_ID_APEV2};
use symphonia_core::meta::{
    MetadataBuilder, MetadataInfo, MetadataOptions, MetadataReader, MetadataRevision,
    StandardTagKey, Tag, Value,
};
use symphonia_core::support_metadata;

use lazy_static::lazy_static;

lazy_static! {
    static ref APE_TAG_MAP: HashMap<&'static str, StandardTagKey> = {
        let mut m = HashMap::new();
        m.insert("accurateripdiscid"           , StandardTagKey::AccurateRipDiscId);
        m.insert("accurateripresult"           , StandardTagKey::AccurateRipResult);
        m.insert("acoustid_fingerprint"        , StandardTagKey::AcoustIdFingerprint);
        m.insert("acoustid_id"                 , StandardTagKey::AcoustId);
        m.insert("album artist"                , StandardTagKey::AlbumArtist);
        m.insert("album"                       , StandardTagKey::Album);
        m.insert("albumartistsort"             , StandardTagKey::SortAlbumArtist);
        m.insert("albumsort"                   , StandardTagKey::SortAlbum);
        m.insert("arranger"                    , StandardTagKey::Arranger);
        m.insert("artist"                      , StandardTagKey::Artist);
        m.insert("artistsort"                  , StandardTagKey::SortArtist);
        m.insert("asin"                        , StandardTagKey::IdentAsin);
        m.insert("bpm"                         , StandardTagKey::Bpm);
        m.insert("catalog"                     , StandardTagKey::IdentCatalogNumber);
        m.insert("catalognumber"               , StandardTagKey::IdentCatalogNumber);
        m.insert("comment"                     , StandardTagKey::Comment);
        m.insert("compilation"                 , StandardTagKey::Compilation);
        m.insert("composer"                    , StandardTagKey::Composer);
        m.insert("composersort"                , StandardTagKey::SortComposer);
        m.insert("conductor"                   , StandardTagKey::Conductor);
        m.insert("copyright"                   , StandardTagKey::Copyright);
        // Disc Number or Disc Number/Total Discs
        m.insert("disc"                        , StandardTagKey::DiscNumber);
        m.insert("djmixer"                     , StandardTagKey::MixDj);
        // EAN-13/UPC-A
        m.insert("ean/upc"                     , StandardTagKey::IdentEanUpn);
        m.insert("encodedby"                   , StandardTagKey::EncodedBy);
        m.insert("encoder settings"            , StandardTagKey::EncoderSettings);
        m.insert("encoder"                     , StandardTagKey::Encoder);
        m.insert("engineer"                    , StandardTagKey::Engineer);
        m.insert("file"                        , StandardTagKey::OriginalFile);
        m.insert("genre"                       , StandardTagKey::Genre);
        m.insert("isbn"                        , StandardTagKey::IdentIsbn);
        m.insert("isrc"                        , StandardTagKey::IdentIsrc);
        m.insert("label"                       , StandardTagKey::Label);
        m.insert("labelcode"                   , StandardTagKey::LabelCode);
        m.insert("language"                    , StandardTagKey::Language);
        m.insert("lyricist"                    , StandardTagKey::Lyricist);
        m.insert("lyrics"                      , StandardTagKey::Lyrics);
        m.insert("media"                       , StandardTagKey::MediaFormat);
        m.insert("mixer"                       , StandardTagKey::MixEngineer);
        m.insert("mood"                        , StandardTagKey::Mood);
        m.insert("movement"                    , StandardTagKey::MovementTotal);
        m.insert("movementname"                , StandardTagKey::MovementName);
        m.insert("movementtotal"               , StandardTagKey::Mood);
        m.insert("mp3gain_album_minmax"        , StandardTagKey::Mp3GainAlbumMinMax);
        m.insert("mp3gain_minmax"              , StandardTagKey::Mp3GainMinMax);
        m.insert("mp3gain_undo"                , StandardTagKey::Mp3GainUndo);
        m.insert("musicbrainz_albumartistid"   , StandardTagKey::MusicBrainzAlbumArtistId);
        m.insert("musicbrainz_albumid"         , StandardTagKey::MusicBrainzAlbumId);
        m.insert("musicbrainz_artistid"        , StandardTagKey::MusicBrainzArtistId);
        m.insert("musicbrainz_discid"          , StandardTagKey::AccurateRipDiscId);
        m.insert("musicbrainz_originalalbumid" , StandardTagKey::MusicBrainzOriginalAlbumId);
        m.insert("musicbrainz_originalartistid", StandardTagKey::MusicBrainzOriginalArtistId);
        m.insert("musicbrainz_releasegroupid"  , StandardTagKey::MusicBrainzReleaseGroupId);
        m.insert("musicbrainz_releasetrackid"  , StandardTagKey::MusicBrainzReleaseTrackId);
        m.insert("musicbrainz_trackid"         , StandardTagKey::MusicBrainzTrackId);
        m.insert("musicbrainz_trmid"           , StandardTagKey::MusicBrainzTrmId);
        m.insert("musicbrainz_workid"          , StandardTagKey::MusicBrainzWorkId);
        m.insert("original artist"             , StandardTagKey::OriginalArtist);
        m.insert("originalyear"                , StandardTagKey::OriginalDate);
        m.insert("publisher"                   , StandardTagKey::Label);
        m.insert("record date"                 , StandardTagKey::RecordingDate);
        m.insert("record location"             , StandardTagKey::RecordingLocation);
        m.insert("related"                     , StandardTagKey::Url);
        m.insert("replaygain_album_gain"       , StandardTagKey::ReplayGainAlbumGain);
        m.insert("replaygain_album_peak"       , StandardTagKey::ReplayGainAlbumPeak);
        m.insert("replaygain_track_gain"       , StandardTagKey::ReplayGainTrackGain);
        m.insert("replaygain_track_peak"       , StandardTagKey::ReplayGainTrackPeak);
        m.insert("subtitle"                    , StandardTagKey::TrackSubtitle);
        m.insert("title"                       , StandardTagKey::TrackTitle);
        m.insert("titlesort"                   , StandardTagKey::SortTrackTitle);
        // Track Number or Track Number/Total Tracks
        m.insert("track"                       , StandardTagKey::TrackNumber);
        m.insert("writer"                      , StandardTagKey::Writer);
        m.insert("year"                        , StandardTagKey::ReleaseDate);
        // TODO: Debut Album
        // TODO: Publicationright
        // TODO: Abstract
        // TODO: Bibliography

        // No mappings for: Index, Introplay, Dummy
        m
    };
}

const APEV1_METADATA_INFO: MetadataInfo =
    MetadataInfo { metadata: METADATA_ID_APEV1, short_name: "apev1", long_name: "APEv1" };
const APEV2_METADATA_INFO: MetadataInfo =
    MetadataInfo { metadata: METADATA_ID_APEV2, short_name: "apev2", long_name: "APEv2" };

/// The APE tag version.
#[derive(PartialEq, Eq)]
enum ApeVersion {
    /// Version 1, maps to 1000.
    V1,
    /// Version 2, maps to 2000.
    V2,
}

struct ApeHeader {
    version: ApeVersion,
    num_items: u32,
    size: u32,
    is_header: bool,
    has_header: bool,
    has_footer: bool,
}

impl ApeHeader {
    /// Read and verify the APE tag preamble and version.
    fn read_identity<B: ReadBytes>(reader: &mut B) -> Result<ApeVersion> {
        let mut preamble = [0; 8];
        reader.read_buf_exact(&mut preamble)?;

        if preamble != *b"APETAGEX" {
            return decode_error("ape: invalid preamble");
        }

        // Read the version. 1000 for APEv1, 2000 for APEv2, and so on...
        let version = match reader.read_u32()? {
            1000 => ApeVersion::V1,
            2000 => ApeVersion::V2,
            _ => return unsupported_error("ape: unsupported version"),
        };

        Ok(version)
    }

    /// Read an APE tag header.
    fn read<B: ReadBytes>(reader: &mut B) -> Result<ApeHeader> {
        let version = ApeHeader::read_identity(reader)?;

        // The size of the tag excluding any header.
        let size = reader.read_u32()?;
        let num_items = reader.read_u32()?;
        let flags = reader.read_u32()?;
        let _reserved = reader.read_u64()?;

        // Interpret the flags and size based on version.
        let (size, has_footer, has_header, is_header) = match version {
            ApeVersion::V1 => {
                // Flags should be ignored reading an APEv1 tag. However, an APEv1 tag always has a
                // footer.
                (size, true, false, false)
            }
            ApeVersion::V2 => {
                let has_header = flags & 0x8000_0000 != 0;
                let has_footer = flags & 0x4000_0000 != 0;
                let is_header = flags & 0x2000_0000 != 0;

                // The header size is not included in the size written to the tag.
                let real_size = size + if has_header { 32 } else { 0 };

                (real_size, has_footer, has_header, is_header)
            }
        };

        Ok(ApeHeader { version, num_items, size, is_header, has_header, has_footer })
    }
}

/// The value of an APE tag item.
enum ApeItemValue {
    String(String),
    Binary(Box<[u8]>),
    Locator(String),
}

/// An APE tag item.
struct ApeItem {
    key: String,
    value: ApeItemValue,
}

impl ApeItem {
    /// Try to read and return an APE tag item.
    fn read<B: ReadBytes>(reader: &mut B, header: &ApeHeader) -> Result<ApeItem> {
        // The length of the value in bytes.
        let len = reader.read_u32()? as usize;

        // Read flags.
        let flags = match header.version {
            ApeVersion::V1 => {
                // Ignore item flags for APEv1. The value type is always text.
                reader.read_u32()?;
                0
            }
            ApeVersion::V2 => reader.read_u32()?,
        };

        // Read the null-terminated key.
        let key = read_key(reader)?;

        // Read the value.
        let value = match (flags >> 1) & 0x3 {
            // UTF-8
            0 => ApeItemValue::String(read_utf8_value(reader, len)?),
            // Binary
            1 => ApeItemValue::Binary(reader.read_boxed_slice_exact(len)?),
            // Locator
            2 => ApeItemValue::Locator(read_utf8_value(reader, len)?),
            // Reserved
            3 => return decode_error("ape: reserved item value type"),
            _ => unreachable!(),
        };

        Ok(ApeItem { key, value })
    }
}

/// APEv1 and APEv2 tag reader.
pub struct ApeReader<'s> {
    reader: MediaSourceStream<'s>,
    version: ApeVersion,
}

impl<'s> ApeReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, _opts: MetadataOptions) -> Result<Self> {
        // Read and verify the APE tag preamble and version.
        let version = ApeHeader::read_identity(&mut mss)?;
        mss.seek_buffered_rel(-12);

        Ok(Self { reader: mss, version })
    }
}

impl Scoreable for ApeReader<'_> {
    fn score(_src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        Ok(Score::Supported(255))
    }
}

impl ProbeableMetadata<'_> for ApeReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: MetadataOptions,
    ) -> Result<Box<dyn MetadataReader + '_>>
    where
        Self: Sized,
    {
        Ok(Box::new(ApeReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeMetadataData] {
        &[
            // APEv1
            support_metadata!(
                APEV1_METADATA_INFO,
                &[],
                &[],
                &[b"APETAGEX\xe8\x03\x00\x00"],
                // APEv1 tags are only appended to the end of the stream.
                Anchors::Exclusive(&[
                    32,  // APE tag at end of stream.
                    160  // APE tag before ID3v1 tag.
                ])
            ),
            // APEv2
            support_metadata!(
                APEV2_METADATA_INFO,
                &[],
                &[],
                &[b"APETAGEX\xd0\x07\x00\x00"],
                // APEv2 tags can be appended to the end of the stream, or be at the start.
                Anchors::Supplemental(&[
                    32,  // APE tag at end of stream.
                    160  // APE tag before ID3v1 tag.
                ])
            ),
        ]
    }
}

impl MetadataReader for ApeReader<'_> {
    fn metadata_info(&self) -> &MetadataInfo {
        match self.version {
            ApeVersion::V1 => &APEV1_METADATA_INFO,
            ApeVersion::V2 => &APEV2_METADATA_INFO,
        }
    }

    fn read_all(&mut self) -> Result<MetadataRevision> {
        let mut builder = MetadataBuilder::new();

        // Read the tag header. This may actually be the header OR the footer.
        let header = ApeHeader::read(&mut self.reader)?;

        // If the header was actually a footer. Seek to the start of the APE tag.
        if !header.is_header {
            // The current position is the first byte after the APE footer. After the seek, the
            // reader will be at the header (if the tag contains one), or the first item.
            self.reader.seek(SeekFrom::Current(-(i64::from(header.size))))?;

            // If the APE tag contains a header, read it and do some verification checks. All header
            // and footer fields should match other than the `is_header` flag.
            if header.has_header {
                let real_header = ApeHeader::read(&mut self.reader)?;

                if header.has_footer != real_header.has_footer
                    || header.has_header != real_header.has_header
                    || header.is_header == real_header.is_header
                    || header.num_items != real_header.num_items
                    || header.size != real_header.size
                    || header.version != real_header.version
                {
                    return decode_error("ape: header and footer mismatch");
                }
            }
        }

        // Read APE tag items.
        for _ in 0..header.num_items {
            let item = ApeItem::read(&mut self.reader, &header)?;

            // Map APE tag item values.
            let value = match item.value {
                ApeItemValue::String(str) => Value::String(str),
                ApeItemValue::Locator(loc) => Value::String(loc),
                ApeItemValue::Binary(bin) => Value::Binary(bin),
            };

            // Try to find standard tag key.
            let std_key = APE_TAG_MAP.get(item.key.to_lowercase().as_str()).copied();

            // TODO: Detect visuals. Will need to be able to detect common image formats to do this
            // robustly.

            builder.add_tag(Tag::new(std_key, &item.key, value));
        }

        // Read the footer.
        let footer = ApeHeader::read(&mut self.reader)?;

        // If the initial header was the actual header, then this checks the entire APE tag was
        // read, and the footer matches the header. If the initial header was actually the footer,
        // or there was no header, then this only checks the entire tag APE tag was read. However,
        // if there was a header, then the header and footer was checked to match earlier above.
        if header.has_footer != footer.has_footer
            || header.has_header != footer.has_header
            || header.num_items != footer.num_items
            || header.size != footer.size
            || header.version != footer.version
        {
            return decode_error("ape: header and footer mismatch");
        }

        Ok(builder.metadata())
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}

fn read_key<B: ReadBytes>(reader: &mut B) -> Result<String> {
    let mut buf = Vec::new();

    loop {
        let byte = reader.read_u8()?;

        // Break at the null-terminator. Do not add it to the string buffer.
        if byte == 0 {
            break;
        }

        // Can only contain ASCII characters from 0x20 ' ' up to 0x7E '~'.
        if byte < 0x20 || byte > 0x7e {
            return decode_error("ape: invalid character in item key");
        }

        buf.push(byte);
    }

    // Safety: Only printable ASCII characters are pushed onto the vector.
    Ok(String::from_utf8(buf).unwrap())
}

fn read_utf8_value<B: ReadBytes>(reader: &mut B, len: usize) -> Result<String> {
    match String::from_utf8(reader.read_boxed_slice_exact(len)?.into_vec()) {
        Ok(value) => Ok(value),
        Err(_) => decode_error("ape: item value is not utf-8"),
    }
}
