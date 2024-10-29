// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::sync::Arc;

use symphonia_core::errors::{decode_error, Error, Result};
use symphonia_core::io::{BufReader, ReadBytes};
use symphonia_core::meta::{
    MetadataBuilder, MetadataRevision, StandardTag, StandardVisualKey, Tag,
};
use symphonia_core::meta::{RawValue, Visual};
use symphonia_core::util::bits;
use symphonia_metadata::utils::images::try_get_image_info;
use symphonia_metadata::{id3v1, itunes};

use crate::atoms::{Atom, AtomHeader, AtomIterator, AtomType};

use encoding_rs::{SHIFT_JIS, UTF_16BE};
use log::warn;

/// Data type enumeration for metadata value atoms as defined in the QuickTime File Format standard.
#[derive(Debug, Copy, Clone)]
pub enum DataType {
    AffineTransformF64,
    Bmp,
    DimensionsF32,
    Float32,
    Float64,
    Jpeg,
    /// The data type is implicit to the atom.
    NoType,
    Png,
    PointF32,
    QuickTimeMetadata,
    RectF32,
    ShiftJis,
    SignedInt16,
    SignedInt32,
    SignedInt64,
    SignedInt8,
    SignedIntVariable,
    UnsignedInt16,
    UnsignedInt32,
    UnsignedInt64,
    UnsignedInt8,
    UnsignedIntVariable,
    Utf16,
    Utf16Sort,
    Utf8,
    Utf8Sort,
    #[allow(dead_code)]
    Unknown(u32),
}

impl From<u32> for DataType {
    fn from(value: u32) -> Self {
        match value {
            0 => DataType::NoType,
            1 => DataType::Utf8,
            2 => DataType::Utf16,
            3 => DataType::ShiftJis,
            4 => DataType::Utf8Sort,
            5 => DataType::Utf16Sort,
            13 => DataType::Jpeg,
            14 => DataType::Png,
            21 => DataType::SignedIntVariable,
            22 => DataType::UnsignedIntVariable,
            23 => DataType::Float32,
            24 => DataType::Float64,
            27 => DataType::Bmp,
            28 => DataType::QuickTimeMetadata,
            65 => DataType::SignedInt8,
            66 => DataType::SignedInt16,
            67 => DataType::SignedInt32,
            70 => DataType::PointF32,
            71 => DataType::DimensionsF32,
            72 => DataType::RectF32,
            74 => DataType::SignedInt64,
            75 => DataType::UnsignedInt8,
            76 => DataType::UnsignedInt16,
            77 => DataType::UnsignedInt32,
            78 => DataType::UnsignedInt64,
            79 => DataType::AffineTransformF64,
            _ => DataType::Unknown(value),
        }
    }
}

fn parse_no_type(data: &[u8]) -> Option<RawValue> {
    // Latin1, potentially null-terminated.
    let end = data.iter().position(|&c| c == b'\0').unwrap_or(data.len());
    let text = String::from_utf8_lossy(&data[..end]);
    Some(RawValue::from(text))
}

fn parse_utf8(data: &[u8]) -> Option<RawValue> {
    // UTF8, no null-terminator or count.
    let text = String::from_utf8_lossy(data);
    Some(RawValue::from(text))
}

fn parse_utf16(data: &[u8]) -> Option<RawValue> {
    // UTF16 BE
    let text = UTF_16BE.decode(data).0;
    Some(RawValue::from(text))
}

fn parse_shift_jis(data: &[u8]) -> Option<RawValue> {
    // Shift-JIS
    let text = SHIFT_JIS.decode(data).0;
    Some(RawValue::from(text))
}

fn parse_signed_int8(data: &[u8]) -> Option<RawValue> {
    match data.len() {
        1 => {
            let s = bits::sign_extend_leq8_to_i8(data[0], 8);
            Some(RawValue::from(s))
        }
        _ => None,
    }
}

fn parse_signed_int16(data: &[u8]) -> Option<RawValue> {
    match data.len() {
        2 => {
            let u = BufReader::new(data).read_be_u16().ok()?;
            let s = bits::sign_extend_leq16_to_i16(u, 16);
            Some(RawValue::from(s))
        }
        _ => None,
    }
}

fn parse_signed_int32(data: &[u8]) -> Option<RawValue> {
    match data.len() {
        4 => {
            let u = BufReader::new(data).read_be_u32().ok()?;
            let s = bits::sign_extend_leq32_to_i32(u, 32);
            Some(RawValue::from(s))
        }
        _ => None,
    }
}

fn parse_signed_int64(data: &[u8]) -> Option<RawValue> {
    match data.len() {
        8 => {
            let u = BufReader::new(data).read_be_u64().ok()?;
            let s = bits::sign_extend_leq64_to_i64(u, 64);
            Some(RawValue::from(s))
        }
        _ => None,
    }
}

fn parse_var_signed_int(data: &[u8]) -> Option<RawValue> {
    match data.len() {
        1 => parse_signed_int8(data),
        2 => parse_signed_int16(data),
        4 => parse_signed_int32(data),
        _ => None,
    }
}

fn parse_unsigned_int8(data: &[u8]) -> Option<RawValue> {
    match data.len() {
        1 => Some(RawValue::from(data[0])),
        _ => None,
    }
}

fn parse_unsigned_int16(data: &[u8]) -> Option<RawValue> {
    match data.len() {
        2 => {
            let u = BufReader::new(data).read_be_u16().ok()?;
            Some(RawValue::from(u))
        }
        _ => None,
    }
}

fn parse_unsigned_int32(data: &[u8]) -> Option<RawValue> {
    match data.len() {
        4 => {
            let u = BufReader::new(data).read_be_u32().ok()?;
            Some(RawValue::from(u))
        }
        _ => None,
    }
}

fn parse_unsigned_int64(data: &[u8]) -> Option<RawValue> {
    match data.len() {
        8 => {
            let u = BufReader::new(data).read_be_u64().ok()?;
            Some(RawValue::from(u))
        }
        _ => None,
    }
}

fn parse_var_unsigned_int(data: &[u8]) -> Option<RawValue> {
    match data.len() {
        1 => parse_unsigned_int8(data),
        2 => parse_unsigned_int16(data),
        4 => parse_unsigned_int32(data),
        _ => None,
    }
}

fn parse_float32(data: &[u8]) -> Option<RawValue> {
    match data.len() {
        4 => {
            let f = BufReader::new(data).read_be_f32().ok()?;
            Some(RawValue::Float(f64::from(f)))
        }
        _ => None,
    }
}

fn parse_float64(data: &[u8]) -> Option<RawValue> {
    match data.len() {
        8 => {
            let f = BufReader::new(data).read_be_f64().ok()?;
            Some(RawValue::Float(f))
        }
        _ => None,
    }
}

fn parse_tag_value(data_type: DataType, data: &[u8]) -> Option<RawValue> {
    match data_type {
        DataType::NoType => parse_no_type(data),
        DataType::Utf8 | DataType::Utf8Sort => parse_utf8(data),
        DataType::Utf16 | DataType::Utf16Sort => parse_utf16(data),
        DataType::ShiftJis => parse_shift_jis(data),
        DataType::UnsignedInt8 => parse_unsigned_int8(data),
        DataType::UnsignedInt16 => parse_unsigned_int16(data),
        DataType::UnsignedInt32 => parse_unsigned_int32(data),
        DataType::UnsignedInt64 => parse_unsigned_int64(data),
        DataType::UnsignedIntVariable => parse_var_unsigned_int(data),
        DataType::SignedInt8 => parse_signed_int8(data),
        DataType::SignedInt16 => parse_signed_int16(data),
        DataType::SignedInt32 => parse_signed_int32(data),
        DataType::SignedInt64 => parse_signed_int64(data),
        DataType::SignedIntVariable => parse_var_signed_int(data),
        DataType::Float32 => parse_float32(data),
        DataType::Float64 => parse_float64(data),
        _ => None,
    }
}

/// Reads and parses a `MetaTagAtom` from the provided iterator and adds it to the `MetadataBuilder`
/// if there are no errors.
fn add_generic_tag<B: ReadBytes>(
    iter: &mut AtomIterator<B>,
    builder: &mut MetadataBuilder,
    map: fn(&RawValue) -> Option<StandardTag>,
) -> Result<()> {
    let tag = iter.read_atom::<MetaTagAtom>()?;

    for value_atom in tag.values.iter() {
        // Parse the value atom data into a string, if possible.
        if let Some(value) = parse_tag_value(value_atom.data_type, &value_atom.data) {
            let std_tag = map(&value);
            builder.add_tag(Tag::new_from_parts("", value, std_tag));
        }
        else {
            warn!("unsupported data type {:?} for {:?} tag", value_atom.data_type, tag.atom_type);
        }
    }

    Ok(())
}

fn add_var_unsigned_int_tag<B: ReadBytes>(
    iter: &mut AtomIterator<B>,
    builder: &mut MetadataBuilder,
    map: fn(&RawValue) -> Option<StandardTag>,
) -> Result<()> {
    let tag = iter.read_atom::<MetaTagAtom>()?;

    if let Some(value_atom) = tag.values.first() {
        if let Some(value) = parse_var_unsigned_int(&value_atom.data) {
            let std_tag = map(&value);
            builder.add_tag(Tag::new_from_parts("", value, std_tag));
        }
        else {
            warn!("got unexpected data for {:?} tag", tag.atom_type);
        }
    }

    Ok(())
}

// fn add_var_signed_int_tag<B: ReadBytes>(
//     iter: &mut AtomIterator<B>,
//     builder: &mut MetadataBuilder,
//     map: fn(&Value) -> Option<StandardTag>,
// ) -> Result<()> {
//     let tag = iter.read_atom::<MetaTagAtom>()?;

//     if let Some(value_atom) = tag.values.first() {
//         if let Some(value) = parse_var_signed_int(&value_atom.data) {
//             builder.add_tag(Tag::new_from_parts("", value, map(&value)));
//         }
//         else {
//             warn!("got unexpected data for {:?} tag", std_key);
//         }
//     }

//     Ok(())
// }

fn add_flag_tag<B: ReadBytes>(
    iter: &mut AtomIterator<B>,
    builder: &mut MetadataBuilder,
    map: fn() -> Option<StandardTag>,
) -> Result<()> {
    let tag = iter.read_atom::<MetaTagAtom>()?;

    // There should only be 1 value.
    if let Some(value) = tag.values.first() {
        // Only add the tag if the boolean value is true (1).
        if let Some(bool_value) = value.data.first() {
            if *bool_value == 1 {
                builder.add_tag(Tag::new_from_parts("", true, map()));
            }
        }
    }

    Ok(())
}

fn add_m_of_n_tag<B: ReadBytes>(
    iter: &mut AtomIterator<B>,
    builder: &mut MetadataBuilder,
    map_m: fn(&RawValue) -> Option<StandardTag>,
    map_n: fn(&RawValue) -> Option<StandardTag>,
) -> Result<()> {
    let tag = iter.read_atom::<MetaTagAtom>()?;

    // There should only be 1 value.
    if let Some(value) = tag.values.first() {
        // The trkn and disk atoms contains an 8 byte value buffer, where the 4th and 6th bytes
        // indicate the track/disk number and total number of tracks/disks, respectively. Odd.
        if value.data.len() == 8 {
            let m = RawValue::from(value.data[3]);
            let n = RawValue::from(value.data[5]);

            builder.add_tag(Tag::new_from_parts("", m.clone(), map_m(&m)));
            builder.add_tag(Tag::new_from_parts("", n.clone(), map_n(&n)));
        }
    }

    Ok(())
}

fn add_visual_tag<B: ReadBytes>(
    iter: &mut AtomIterator<B>,
    builder: &mut MetadataBuilder,
) -> Result<()> {
    let tag = iter.read_atom::<MetaTagAtom>()?;

    // There could be more than one attached image.
    for value in tag.values {
        let image_info = try_get_image_info(&value.data);

        builder.add_visual(Visual {
            media_type: image_info.as_ref().map(|info| info.media_type.clone()),
            dimensions: image_info.as_ref().map(|info| info.dimensions),
            color_mode: image_info.as_ref().map(|info| info.color_mode),
            usage: Some(StandardVisualKey::FrontCover),
            tags: Default::default(),
            data: value.data,
        });
    }

    Ok(())
}

fn add_advisory_tag<B: ReadBytes>(
    _iter: &mut AtomIterator<B>,
    _builder: &mut MetadataBuilder,
) -> Result<()> {
    Ok(())
}

fn add_media_type_tag<B: ReadBytes>(
    iter: &mut AtomIterator<B>,
    builder: &mut MetadataBuilder,
) -> Result<()> {
    let tag = iter.read_atom::<MetaTagAtom>()?;

    // There should only be 1 value.
    if let Some(value) = tag.values.first() {
        if let Some(media_type_value) = value.data.first() {
            let media_type = match media_type_value {
                0 => "Movie",
                1 => "Normal",
                2 => "Audio Book",
                5 => "Whacked Bookmark",
                6 => "Music Video",
                9 => "Short Film",
                10 => "TV Show",
                11 => "Booklet",
                _ => "Unknown",
            };

            let media = Arc::new(String::from(media_type));
            let tag = Tag::new_from_parts("", media.clone(), Some(StandardTag::MediaFormat(media)));
            builder.add_tag(tag);
        }
    }

    Ok(())
}

fn add_id3v1_genre_tag<B: ReadBytes>(
    iter: &mut AtomIterator<B>,
    builder: &mut MetadataBuilder,
) -> Result<()> {
    let tag = iter.read_atom::<MetaTagAtom>()?;

    // There should only be 1 value.
    if let Some(value) = tag.values.first() {
        // The ID3v1 genre is stored as a unsigned 16-bit big-endian integer.
        let index = BufReader::new(&value.data).read_be_u16()?;

        // The stored index uses 1-based indexing, but the ID3v1 genre list is 0-based.
        if index > 0 && index <= 255 {
            if let Some(genre) = id3v1::util::genre_name((index - 1) as u8) {
                let genre = Arc::new(genre);
                let tag = Tag::new_from_parts("", genre.clone(), Some(StandardTag::Genre(genre)));
                builder.add_tag(tag);
            }
        }
    }

    Ok(())
}

fn add_freeform_tag<B: ReadBytes>(
    iter: &mut AtomIterator<B>,
    builder: &mut MetadataBuilder,
) -> Result<()> {
    let tag = iter.read_atom::<MetaTagAtom>()?;

    // A user-defined tag should only have 1 value.
    for value_atom in tag.values.iter() {
        // Parse the value atom data into a string, if possible.
        if let Some(value) = parse_tag_value(value_atom.data_type, &value_atom.data) {
            // Try to map iTunes freeform tags to standard tag keys.
            itunes::parse_as_itunes_tag(tag.full_name(), value, builder)?;
        }
        else {
            warn!("unsupported data type {:?} for free-form tag", value_atom.data_type);
        }
    }

    Ok(())
}

/// Metadata tag data atom.
#[allow(dead_code)]
pub struct MetaTagDataAtom {
    /// Tag data.
    pub data: Box<[u8]>,
    /// The data type contained in buf.
    pub data_type: DataType,
}

impl Atom for MetaTagDataAtom {
    fn read<B: ReadBytes>(reader: &mut B, mut header: AtomHeader) -> Result<Self> {
        let (version, flags) = header.read_extended_header(reader)?;

        // For the mov brand, this a data type indicator and must always be 0 (well-known type). It
        // specifies the table in which the next 24-bit integer specifying the actual data type
        // indexes. For iso/mp4, this is a version, and there is only one version, 0. Therefore,
        // flags are interpreted as the actual data type index.
        if version != 0 {
            return decode_error("isomp4: invalid data atom version");
        }

        let data_type = DataType::from(flags);

        // For the mov brand, the next four bytes are country and languages code. However, for
        // iso/mp4 these codes should be ignored.
        let _country = reader.read_be_u16()?;
        let _language = reader.read_be_u16()?;

        // The data payload is the remainder of the atom.
        // TODO: Apply a limit.
        let data = {
            let size = header.data_unread_at(reader.pos()).ok_or_else(|| {
                Error::DecodeError("isomp4 (ilst): expected atom size to be known")
            })?;

            reader.read_boxed_slice_exact(size as usize)?
        };

        Ok(MetaTagDataAtom { data, data_type })
    }
}

/// Metadata tag name and mean atom.
#[allow(dead_code)]
pub struct MetaTagNamespaceAtom {
    /// For 'mean' atoms, this is the key namespace. For 'name' atom, this is the key name.
    pub value: String,
}

impl Atom for MetaTagNamespaceAtom {
    fn read<B: ReadBytes>(reader: &mut B, mut header: AtomHeader) -> Result<Self> {
        let (_, _) = header.read_extended_header(reader)?;

        let size = header
            .data_len()
            .ok_or_else(|| Error::DecodeError("isomp4 (ilst): expected atom size to be known"))?;

        let buf = reader.read_boxed_slice_exact(size as usize)?;

        // Do a lossy conversion because metadata should not prevent the demuxer from working.
        let value = String::from_utf8_lossy(&buf).to_string();

        Ok(MetaTagNamespaceAtom { value })
    }
}

/// A generic metadata tag atom.
#[allow(dead_code)]
pub struct MetaTagAtom {
    /// The atom type for the tag.
    pub atom_type: AtomType,
    /// Tag value(s).
    pub values: Vec<MetaTagDataAtom>,
    /// Optional, tag key namespace.
    pub mean: Option<MetaTagNamespaceAtom>,
    /// Optional, tag key name.
    pub name: Option<MetaTagNamespaceAtom>,
}

impl MetaTagAtom {
    pub fn full_name(&self) -> String {
        let mut full_name = String::new();

        if self.mean.is_some() || self.name.is_some() {
            // full_name.push_str("----:");

            if let Some(mean) = &self.mean {
                full_name.push_str(&mean.value);
            }

            full_name.push(':');

            if let Some(name) = &self.name {
                full_name.push_str(&name.value);
            }
        }

        full_name
    }
}

impl Atom for MetaTagAtom {
    fn read<B: ReadBytes>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        let atom_type = header.atom_type();

        let mut iter = AtomIterator::new(reader, header);

        let mut mean = None;
        let mut name = None;
        let mut values = Vec::new();

        while let Some(header) = iter.next()? {
            match header.atom_type {
                AtomType::MetaTagData => {
                    values.push(iter.read_atom::<MetaTagDataAtom>()?);
                }
                AtomType::MetaTagName => {
                    name = Some(iter.read_atom::<MetaTagNamespaceAtom>()?);
                }
                AtomType::MetaTagMeaning => {
                    mean = Some(iter.read_atom::<MetaTagNamespaceAtom>()?);
                }
                _ => (),
            }
        }

        Ok(MetaTagAtom { atom_type, values, mean, name })
    }
}

macro_rules! map_std_str {
    ($std:path) => {
        |value: &RawValue| match value {
            RawValue::String(s) => Some($std(s.clone())),
            _ => None,
        }
    };
}

macro_rules! map_std_uint {
    ($std:path) => {
        |value: &RawValue| match value {
            RawValue::UnsignedInt(v) => Some($std(*v)),
            _ => None,
        }
    };
}

/// User data atom.
#[allow(dead_code)]
pub struct IlstAtom {
    /// Metadata revision.
    pub metadata: MetadataRevision,
}

impl Atom for IlstAtom {
    fn read<B: ReadBytes>(reader: &mut B, header: AtomHeader) -> Result<Self> {
        let mut iter = AtomIterator::new(reader, header);

        let mut mb = MetadataBuilder::new();

        while let Some(header) = iter.next()? {
            // Ignore standard atoms, check if other is a metadata atom.
            match &header.atom_type {
                AtomType::AdvisoryTag => add_advisory_tag(&mut iter, &mut mb)?,
                AtomType::AlbumArtistTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::AlbumArtist))?
                }
                AtomType::AlbumTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::Album))?
                }
                AtomType::ArtistLowerTag => (),
                AtomType::ArtistTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::Artist))?
                }
                AtomType::CategoryTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::PodcastCategory))?
                }
                AtomType::CommentTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::Comment))?
                }
                AtomType::CompilationTag => {
                    add_flag_tag(&mut iter, &mut mb, || Some(StandardTag::Compilation))?
                }
                AtomType::ComposerTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::Composer))?
                }
                AtomType::CopyrightTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::Copyright))?
                }
                AtomType::CoverTag => add_visual_tag(&mut iter, &mut mb)?,
                AtomType::CustomGenreTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::Genre))?
                }
                AtomType::DateTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::Date))?
                }
                AtomType::DescriptionTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::Description))?
                }
                AtomType::DiskNumberTag => add_m_of_n_tag(
                    &mut iter,
                    &mut mb,
                    map_std_uint!(StandardTag::DiscNumber),
                    map_std_uint!(StandardTag::DiscTotal),
                )?,
                AtomType::EncodedByTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::EncodedBy))?
                }
                AtomType::EncoderTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::Encoder))?
                }
                AtomType::GaplessPlaybackTag => {
                    // TODO: Need standard tag key for gapless playback.
                    // add_boolean_tag(&mut iter, &mut mb, )?
                }
                AtomType::GenreTag => add_id3v1_genre_tag(&mut iter, &mut mb)?,
                AtomType::GroupingTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::Grouping))?
                }
                AtomType::HdVideoTag => (),
                AtomType::IdentPodcastTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::IdentPodcast))?
                }
                AtomType::KeywordTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::PodcastKeywords))?
                }
                AtomType::LongDescriptionTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::Description))?
                }
                AtomType::LyricsTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::Lyrics))?
                }
                AtomType::MediaTypeTag => add_media_type_tag(&mut iter, &mut mb)?,
                AtomType::OwnerTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::Owner))?
                }
                AtomType::PodcastTag => {
                    add_flag_tag(&mut iter, &mut mb, || Some(StandardTag::Podcast))?
                }
                AtomType::PurchaseDateTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::PurchaseDate))?
                }
                AtomType::RatingTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::Rating))?
                }
                AtomType::SortAlbumArtistTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::SortAlbumArtist))?
                }
                AtomType::SortAlbumTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::SortAlbum))?
                }
                AtomType::SortArtistTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::SortArtist))?
                }
                AtomType::SortComposerTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::SortComposer))?
                }
                AtomType::SortNameTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::SortTrackTitle))?
                }
                AtomType::TempoTag => {
                    add_var_unsigned_int_tag(&mut iter, &mut mb, map_std_uint!(StandardTag::Bpm))?
                }
                AtomType::TrackNumberTag => add_m_of_n_tag(
                    &mut iter,
                    &mut mb,
                    map_std_uint!(StandardTag::TrackNumber),
                    map_std_uint!(StandardTag::TrackTotal),
                )?,
                AtomType::TrackTitleTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::TrackTitle))?
                }
                AtomType::TvEpisodeNameTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::TvEpisodeTitle))?
                }
                AtomType::TvEpisodeNumberTag => add_var_unsigned_int_tag(
                    &mut iter,
                    &mut mb,
                    map_std_uint!(StandardTag::TvEpisode),
                )?,
                AtomType::TvNetworkNameTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::TvNetwork))?
                }
                AtomType::TvSeasonNumberTag => add_var_unsigned_int_tag(
                    &mut iter,
                    &mut mb,
                    map_std_uint!(StandardTag::TvSeason),
                )?,
                AtomType::TvShowNameTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::TvShowTitle))?
                }
                AtomType::UrlPodcastTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::UrlPodcast))?
                }
                AtomType::WorkTag => {
                    add_generic_tag(&mut iter, &mut mb, map_std_str!(StandardTag::Work))?
                }
                AtomType::FreeFormTag => add_freeform_tag(&mut iter, &mut mb)?,
                _ => (),
            }
        }

        Ok(IlstAtom { metadata: mb.metadata() })
    }
}
