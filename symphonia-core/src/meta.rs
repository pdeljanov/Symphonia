// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The `meta` module defines basic metadata elements, and management structures.
//!
//! # Tags
//!
//! Within the context of media, a tag is single piece of metadata about the media as a whole, or a
//! track within the media. The storage of tags, their structure, and organization, varies based on
//! the metadata/tagging format.
//!
//! The [`Tag`] structure represents a single tag, and abstracts over the differences in tagging
//! formats. `Tag` is a composition of a mandatory raw tag, and an optional standard tag.
//!
//! ## Raw Tags
//!
//! A [`RawTag`] stores a tag in a data format that matches as closely as possible to the format of
//! the tag as it was written. The data format depends on the tagging format, and the writer of the
//! tag.
//!
//! A `RawTag` consists of a mandatory key-value pair. For most tagging formats, this is sufficient
//! to faithfully represent the original tag, however, for some more structured tagging formats, a
//! set of additional key-value pairs ([`RawTagSubField`]) may be populated.
//!
//! The meaning of the tag can be derived from its key, however, the key may be named differently
//! based on the underlying tagging format and the writer of the tag.
//!
//! Raw tags can be ignored by most tag consumers. Instead, standard tags should be preferred.
//!
//! ## Standard Tags
//!
//! A [`StandardTag`] is a parsed representation of a tag. Unlike a raw tag, a standard tag has a
//! well-defined data type and meaning.
//!
//! A metadata reader will assign a `StandardTag` to a `Tag` if it is able to identify the meaning
//! of the `RawTag`, and parse its value. If the `RawTag` maps to multiple `StandardTag`s, then
//! the `Tag` (along with the `RawTag`) will be duplicated for each `StandardTag` with each instance
//! being assigned one `StandardTag`.
//!
//! An end-user should prefer consuming standard tags over raw tags.
//!
//! ## Storage Efficiency
//!
//! In many cases, the value of a `RawTag` will be the same as the `StandardTag`. Since a value may
//! be large, duplicating it could be wasteful. For this reason, string and binary data values are
//! stored using an [`Arc`].

use std::borrow::Cow;
use std::collections::VecDeque;
use std::convert::From;
use std::fmt;
use std::num::NonZeroU8;
use std::sync::Arc;

use crate::common::FourCc;
use crate::errors::Result;
use crate::io::MediaSourceStream;
use crate::units::Time;

/// A `MetadataType` is a unique identifier used to identify a metadata format.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct MetadataId(u32);

impl MetadataId {
    /// Create a new metadata ID from a FourCC.
    pub const fn new(cc: FourCc) -> MetadataId {
        // A FourCc always only contains ASCII characters. Therefore, the upper bits are always 0.
        Self(0x8000_0000 | u32::from_be_bytes(cc.get()))
    }
}

impl From<FourCc> for MetadataId {
    fn from(value: FourCc) -> Self {
        MetadataId::new(value)
    }
}

impl fmt::Display for MetadataId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

/// Null metadata format
pub const METADATA_ID_NULL: MetadataId = MetadataId(0x0);

/// Basic information about a metadata format.
#[derive(Copy, Clone)]
pub struct MetadataInfo {
    /// The `MetadataType` identifier.
    pub metadata: MetadataId,
    /// A short ASCII-only string identifying the format.
    pub short_name: &'static str,
    /// A longer, more descriptive, string identifying the format.
    pub long_name: &'static str,
}

/// `Limit` defines an upper-bound on how much of a resource should be allocated when the amount to
/// be allocated is specified by the media stream, which is untrusted. A limit will place an
/// upper-bound on this allocation at the risk of breaking potentially valid streams. Limits are
/// used to prevent denial-of-service attacks.
///
/// All limits can be defaulted to a reasonable value specific to the situation. These defaults will
/// generally not break any normal streams.
#[derive(Copy, Clone, Debug)]
pub enum Limit {
    /// Do not impose any limit.
    None,
    /// Use the a reasonable default specified by the `FormatReader` or `Decoder` implementation.
    Default,
    /// Specify the upper limit of the resource. Units are case specific.
    Maximum(usize),
}

impl Limit {
    /// Gets the numeric limit of the limit, or default value. If there is no limit, None is
    /// returned.
    pub fn limit_or_default(&self, default: usize) -> Option<usize> {
        match self {
            Limit::None => None,
            Limit::Default => Some(default),
            Limit::Maximum(max) => Some(*max),
        }
    }
}

impl Default for Limit {
    fn default() -> Self {
        Limit::Default
    }
}

/// `MetadataOptions` is a common set of options that all metadata readers use.
#[derive(Copy, Clone, Debug, Default)]
pub struct MetadataOptions {
    /// The maximum size limit in bytes that a tag may occupy in memory once decoded. Tags exceeding
    /// this limit will be skipped by the demuxer. Take note that tags in-memory are stored as UTF-8
    /// and therefore may occupy more than one byte per character.
    pub limit_metadata_bytes: Limit,

    /// The maximum size limit in bytes that a visual (picture) may occupy.
    pub limit_visual_bytes: Limit,
}

/// `StandardVisualKey` is an enumeration providing standardized keys for common visual dispositions.
/// A demuxer may assign a `StandardVisualKey` to a `Visual` if the disposition of the attached
/// visual is known and can be mapped to a standard key.
///
/// The visual types listed here are derived from, though do not entirely cover, the ID3v2 APIC
/// frame specification.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
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
    Other,
}

/// A standard tag is an enumeration of well-defined and well-known tags with parsed values.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum StandardTag {
    AccurateRipCount(Arc<String>),
    AccurateRipCountAllOffsets(Arc<String>),
    AccurateRipCountWithOffset(Arc<String>),
    AccurateRipCrc(Arc<String>),
    AccurateRipDiscId(Arc<String>),
    AccurateRipId(Arc<String>),
    AccurateRipOffset(Arc<String>),
    AccurateRipResult(Arc<String>),
    AccurateRipTotal(Arc<String>),
    AcoustIdFingerprint(Arc<String>),
    AcoustIdId(Arc<String>),
    Album(Arc<String>),
    AlbumArtist(Arc<String>),
    Arranger(Arc<String>),
    Artist(Arc<String>),
    Bpm(u64),
    CdToc(Arc<String>),
    Comment(Arc<String>),
    Compilation,
    Composer(Arc<String>),
    Conductor(Arc<String>),
    Copyright(Arc<String>),
    CueToolsDbDiscConfidence(Arc<String>),
    CueToolsDbTrackConfidence(Arc<String>),
    Date(Arc<String>),
    Description(Arc<String>),
    DiscNumber(u64),
    DiscSubtitle(Arc<String>),
    DiscTotal(u64),
    EncodedBy(Arc<String>),
    Encoder(Arc<String>),
    EncoderSettings(Arc<String>),
    EncodingDate(Arc<String>),
    Engineer(Arc<String>),
    Ensemble(Arc<String>),
    Genre(Arc<String>),
    Grouping(Arc<String>),
    IdentAsin(Arc<String>),
    IdentBarcode(Arc<String>),
    IdentCatalogNumber(Arc<String>),
    IdentEanUpn(Arc<String>),
    IdentIsbn(Arc<String>),
    IdentIsrc(Arc<String>),
    IdentPn(Arc<String>),
    IdentPodcast(Arc<String>),
    IdentUpc(Arc<String>),
    IndexNumber(u8),
    InitialKey(Arc<String>),
    InternetRadioName(Arc<String>),
    InternetRadioOwner(Arc<String>),
    Label(Arc<String>),
    LabelCode(Arc<String>),
    Language(Arc<String>),
    License(Arc<String>),
    Lyricist(Arc<String>),
    Lyrics(Arc<String>),
    MediaFormat(Arc<String>),
    MixDj(Arc<String>),
    MixEngineer(Arc<String>),
    Mood(Arc<String>),
    MovementName(Arc<String>),
    MovementNumber(u64),
    MovementTotal(u64),
    Mp3GainAlbumMinMax(Arc<String>),
    Mp3GainMinMax(Arc<String>),
    Mp3GainUndo(Arc<String>),
    MusicBrainzAlbumArtistId(Arc<String>),
    MusicBrainzAlbumId(Arc<String>),
    MusicBrainzArtistId(Arc<String>),
    MusicBrainzDiscId(Arc<String>),
    MusicBrainzGenreId(Arc<String>),
    MusicBrainzLabelId(Arc<String>),
    MusicBrainzOriginalAlbumId(Arc<String>),
    MusicBrainzOriginalArtistId(Arc<String>),
    MusicBrainzRecordingId(Arc<String>),
    MusicBrainzReleaseGroupId(Arc<String>),
    MusicBrainzReleaseStatus(Arc<String>),
    MusicBrainzReleaseTrackId(Arc<String>),
    MusicBrainzReleaseType(Arc<String>),
    MusicBrainzTrackId(Arc<String>),
    MusicBrainzTrmId(Arc<String>),
    MusicBrainzWorkId(Arc<String>),
    Opus(Arc<String>),
    OriginalAlbum(Arc<String>),
    OriginalArtist(Arc<String>),
    OriginalDate(Arc<String>),
    OriginalFile(Arc<String>),
    OriginalWriter(Arc<String>),
    OriginalYear(u16),
    Owner(Arc<String>),
    PartNumber(u64),
    Part(Arc<String>),
    PartTotal(u64),
    Performer(Arc<String>),
    Podcast,
    PodcastCategory(Arc<String>),
    PodcastDescription(Arc<String>),
    PodcastKeywords(Arc<String>),
    Producer(Arc<String>),
    PurchaseDate(Arc<String>),
    Rating(Arc<String>),
    RecordingDate(Arc<String>),
    RecordingLocation(Arc<String>),
    RecordingTime(Arc<String>),
    ReleaseCountry(Arc<String>),
    ReleaseDate(Arc<String>),
    Remixer(Arc<String>),
    ReplayGainAlbumGain(Arc<String>),
    ReplayGainAlbumPeak(Arc<String>),
    ReplayGainAlbumRange(Arc<String>),
    ReplayGainReferenceLoudness(Arc<String>),
    ReplayGainTrackGain(Arc<String>),
    ReplayGainTrackPeak(Arc<String>),
    ReplayGainTrackRange(Arc<String>),
    Script(Arc<String>),
    SortAlbum(Arc<String>),
    SortAlbumArtist(Arc<String>),
    SortArtist(Arc<String>),
    SortComposer(Arc<String>),
    SortTrackTitle(Arc<String>),
    TaggingDate(Arc<String>),
    TrackNumber(u64),
    TrackSubtitle(Arc<String>),
    TrackTitle(Arc<String>),
    TrackTotal(u64),
    TvEpisode(u64),
    TvEpisodeTitle(Arc<String>),
    TvNetwork(Arc<String>),
    TvSeason(u64),
    TvShowTitle(Arc<String>),
    Url(Arc<String>),
    UrlArtist(Arc<String>),
    UrlCopyright(Arc<String>),
    UrlInternetRadio(Arc<String>),
    UrlLabel(Arc<String>),
    UrlOfficial(Arc<String>),
    UrlPayment(Arc<String>),
    UrlPodcast(Arc<String>),
    UrlPurchase(Arc<String>),
    UrlSource(Arc<String>),
    Version(Arc<String>),
    Work(Arc<String>),
    Writer(Arc<String>),
}

/// The value of a [`RawTag`].
///
/// Note: The data types in this enumeration are an abstraction. Depending on the particular tagging
/// format, the actual data type of a specific tag may have a lesser width or different encoding
/// than the data type stored here.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum RawValue {
    /// A binary buffer.
    Binary(Arc<Box<[u8]>>),
    /// A boolean value.
    Boolean(bool),
    /// A flag or indicator. A flag carries no data, but the presence of the tag has an implicit
    /// meaning.
    Flag,
    /// A floating point number.
    Float(f64),
    /// A signed integer.
    SignedInt(i64),
    /// A string. This is also the catch-all type for tags with unconventional data types.
    String(Arc<String>),
    /// An unsigned integer.
    UnsignedInt(u64),
}

macro_rules! impl_from_for_value {
    ($value:ident, $from:ty, $conv:expr) => {
        impl From<$from> for RawValue {
            fn from($value: $from) -> Self {
                $conv
            }
        }
    };
}

impl_from_for_value!(v, &[u8], RawValue::Binary(Arc::new(Box::from(v))));
impl_from_for_value!(v, Box<[u8]>, RawValue::Binary(Arc::new(v)));
impl_from_for_value!(v, Arc<Box<[u8]>>, RawValue::Binary(v));
impl_from_for_value!(v, bool, RawValue::Boolean(v));
impl_from_for_value!(v, f32, RawValue::Float(f64::from(v)));
impl_from_for_value!(v, f64, RawValue::Float(v));
impl_from_for_value!(v, i8, RawValue::SignedInt(i64::from(v)));
impl_from_for_value!(v, i16, RawValue::SignedInt(i64::from(v)));
impl_from_for_value!(v, i32, RawValue::SignedInt(i64::from(v)));
impl_from_for_value!(v, i64, RawValue::SignedInt(v));
impl_from_for_value!(v, u8, RawValue::UnsignedInt(u64::from(v)));
impl_from_for_value!(v, u16, RawValue::UnsignedInt(u64::from(v)));
impl_from_for_value!(v, u32, RawValue::UnsignedInt(u64::from(v)));
impl_from_for_value!(v, u64, RawValue::UnsignedInt(v));
impl_from_for_value!(v, &str, RawValue::String(Arc::new(v.to_string())));
impl_from_for_value!(v, String, RawValue::String(Arc::new(v)));
impl_from_for_value!(v, Arc<String>, RawValue::String(v));
impl_from_for_value!(v, Cow<'_, str>, RawValue::String(Arc::new(v.into_owned())));

fn buffer_to_hex_string(buf: &[u8]) -> String {
    let mut output = String::with_capacity(5 * buf.len());

    for ch in buf {
        let u = (ch & 0xf0) >> 4;
        let l = ch & 0x0f;
        output.push_str("\\0x");
        output.push(if u < 10 { (b'0' + u) as char } else { (b'a' + u - 10) as char });
        output.push(if l < 10 { (b'0' + l) as char } else { (b'a' + l - 10) as char });
    }

    output
}

impl fmt::Display for RawValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Implement default formatters for each type.
        match self {
            RawValue::Binary(ref buf) => f.write_str(&buffer_to_hex_string(buf)),
            RawValue::Boolean(boolean) => fmt::Display::fmt(boolean, f),
            RawValue::Flag => write!(f, "<flag>"),
            RawValue::Float(float) => fmt::Display::fmt(float, f),
            RawValue::SignedInt(int) => fmt::Display::fmt(int, f),
            RawValue::String(ref string) => fmt::Display::fmt(string, f),
            RawValue::UnsignedInt(uint) => fmt::Display::fmt(uint, f),
        }
    }
}

/// A key-value pair of supplementary data that can be attached to a raw tag.
#[derive(Clone, Debug)]
pub struct RawTagSubField {
    /// The name of the sub-field.
    pub field: String,
    /// The value of the sub-field.
    pub value: RawValue,
}

impl RawTagSubField {
    /// Create a new sub-field from the provided field and value. Consumes the inputs.
    pub fn new<F, V>(field: F, value: V) -> Self
    where
        F: Into<String>,
        V: Into<RawValue>,
    {
        RawTagSubField { field: field.into(), value: value.into() }
    }
}

/// A raw tag represents a tag in a data format that matches, as closely as possible, to the data
/// format that the tag was written in.
#[derive(Clone, Debug)]
pub struct RawTag {
    /// The name of the tag's key.
    pub key: String,
    /// The value of the tag.
    pub value: RawValue,
    /// The tag's sub-fields, if any.
    pub sub_fields: Option<Box<[RawTagSubField]>>,
}

impl RawTag {
    /// Create a new raw tag from the provided key and value, with no sub-fields. Consumes the
    /// inputs.
    pub fn new<K, V>(key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Into<RawValue>,
    {
        RawTag { key: key.into(), value: value.into(), sub_fields: None }
    }

    /// Create a new raw tag with sub-fields from the provided key, value, and sub-fields. Consumes
    /// the inputs.
    pub fn new_with_sub_fields<K, V>(key: K, value: V, sub_fields: Box<[RawTagSubField]>) -> Self
    where
        K: Into<String>,
        V: Into<RawValue>,
    {
        RawTag { key: key.into(), value: value.into(), sub_fields: Some(sub_fields) }
    }
}

/// A tag encapsulates a single piece of metadata.
#[derive(Clone, Debug)]
pub struct Tag {
    /// The raw tag.
    pub raw: RawTag,
    /// An optional standard tag.
    pub std: Option<StandardTag>,
}

impl Tag {
    /// Create a new tag from a raw tag. Consumes the inputs.
    pub fn new(raw: RawTag) -> Self {
        Tag { raw, std: None }
    }

    /// Create a new tag from a raw tag with a standard tag. Consumes the inputs.
    pub fn new_std(raw: RawTag, std: StandardTag) -> Self {
        Tag { raw, std: Some(std) }
    }

    /// Create a new tag from its constituent parts: a key, value, and optional standard tag.
    /// Consumes the inputs.
    pub fn new_from_parts<K, V>(key: K, value: V, std: Option<StandardTag>) -> Self
    where
        K: Into<String>,
        V: Into<RawValue>,
    {
        Tag { raw: RawTag { key: key.into(), value: value.into(), sub_fields: None }, std }
    }

    /// Returns `true` if the tag was recognized as a well-known tag and has a standard tag
    /// assigned.
    pub fn has_std_tag(&self) -> bool {
        self.std.is_some()
    }
}

/// A 2-dimensional (width and height) size type.
#[derive(Copy, Clone, Debug, Default)]
pub struct Size {
    /// The width in pixels.
    pub width: u32,
    /// The height in pixels.
    pub height: u32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
/// A color model describes how a color is represented.
#[non_exhaustive]
pub enum ColorModel {
    /// Grayscale (1 channel: `Y`), of the indicated bit depth.
    Y(NonZeroU8),
    /// Grayscale with alpha (2 channels: `Y`, `A`), of the indicated bit depth.
    YA(NonZeroU8),
    /// RGB (3 channels: `R`,`G`,`B`), of the indicated bit depth.
    RGB(NonZeroU8),
    /// RGBA (4 channels: `R`,`G`,`B`,`A`), of the indicated bit depth.
    RGBA(NonZeroU8),
    /// CMYK (4 channels: `C`,`M`,`Y`,`K`), of the indicated bit depth.
    CMYK(NonZeroU8),
}

impl ColorModel {
    /// Gets the bits/pixel.
    pub fn bits_per_pixel(&self) -> u32 {
        match self {
            ColorModel::Y(bits) => u32::from(bits.get()),
            ColorModel::YA(bits) => 2 * u32::from(bits.get()),
            ColorModel::RGB(bits) => 3 * u32::from(bits.get()),
            ColorModel::RGBA(bits) => 4 * u32::from(bits.get()),
            ColorModel::CMYK(bits) => 4 * u32::from(bits.get()),
        }
    }

    /// Returns if the color model contains an alpha channel.
    pub fn has_alpha_channel(&self) -> bool {
        matches!(self, ColorModel::YA(_) | ColorModel::RGBA(_))
    }
}

/// A description of the color palette for indexed color mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ColorPaletteInfo {
    /// The number of bits per pixel used to index the palette.
    pub bits_per_pixel: NonZeroU8,
    /// The color model of the entries in the palette.
    pub color_model: ColorModel,
}

/// Indicates how colors are represented in the image.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ColorMode {
    /// Direct colour mode. Each pixel in the image stores the value of each color model primary.
    ///
    /// For example, in the RGB color model, each pixel will store a value for the red, green, and
    /// blue color primaries.
    Direct(ColorModel),
    /// Indexed colour mode. Each pixel in the image stores an index into a color map (the palette)
    /// that stores the actual color.
    Indexed(ColorPaletteInfo),
}

/// A `Visual` is any 2 dimensional graphic.
#[derive(Clone, Debug)]
pub struct Visual {
    /// The Media Type (MIME Type) used to encode the `Visual`.
    pub media_type: Option<String>,
    /// The dimensions of the `Visual`.
    ///
    /// Note: This value may not be accurate as it comes from metadata, not the embedded graphic
    /// itself. Consider it only a hint.
    pub dimensions: Option<Size>,
    /// The color mode of the `Visual`.
    ///
    /// Note: This value may not be accurate as it comes from metadata, not the embedded graphic
    /// itself. Consider it only a hint.
    pub color_mode: Option<ColorMode>,
    /// The usage and/or content of the `Visual`.
    pub usage: Option<StandardVisualKey>,
    /// Any tags associated with the `Visual`.
    pub tags: Vec<Tag>,
    /// The data of the `Visual`, encoded as per `media_type`.
    pub data: Box<[u8]>,
}

/// `VendorData` is any binary metadata that is proprietary to a certain application or vendor.
#[derive(Clone, Debug)]
pub struct VendorData {
    /// A text representation of the vendor's application identifier.
    pub ident: String,
    /// The vendor data.
    pub data: Box<[u8]>,
}

/// A group of chapters and/or other chapter groups.
#[derive(Clone, Debug)]
pub struct ChapterGroup {
    /// A list of chapters and/or chapter groups.
    pub items: Vec<ChapterGroupItem>,
    /// The tags associated with the group of chapters.
    pub tags: Vec<Tag>,
    /// The visuals associated with the group of chapters.
    pub visuals: Vec<Visual>,
}

/// A chapter is a labelled section of a piece of media with a defined start time.
#[derive(Clone, Debug)]
pub struct Chapter {
    /// The offset from the beginning of the media to the start of the chapter.
    pub start_time: Time,
    /// The offset from the beginning of the media to the end of the chapter.
    pub end_time: Option<Time>,
    /// The byte position from the beginning of the media source to the first byte of the first
    /// frame in the chapter.
    pub start_byte: Option<u64>,
    /// The byte position from the beginning of the media source to the first byte of the frame
    /// following the end of the chapter.
    pub end_byte: Option<u64>,
    /// The tags associated with the chapter.
    pub tags: Vec<Tag>,
    /// The visuals associated with the chapter.
    pub visuals: Vec<Visual>,
}

/// A chapter group item is either a chapter or chapter group.
#[derive(Clone, Debug)]
pub enum ChapterGroupItem {
    /// The item is a chapter group.
    Group(ChapterGroup),
    /// The item is a chapter.
    Chapter(Chapter),
}

/// A metadata revision is a container for a single discrete revision of media metadata.
///
/// A metadata revision contains what a user typically associates with metadata: tags, pictures,
/// etc.
#[derive(Clone, Debug, Default)]
pub struct MetadataRevision {
    /// Key-value pairs of metadata.
    tags: Vec<Tag>,
    /// Attached pictures.
    visuals: Vec<Visual>,
    /// Vendor-specific data.
    vendor_data: Vec<VendorData>,
}

impl MetadataRevision {
    /// Gets an immutable slice to the `Tag`s in this revision.
    ///
    /// If a tag read from the source contained multiple values, then there will be one `Tag` item
    /// per value, with each item having the same key and standard key.
    pub fn tags(&self) -> &[Tag] {
        &self.tags
    }

    /// Gets an immutable slice to the `Visual`s in this revision.
    pub fn visuals(&self) -> &[Visual] {
        &self.visuals
    }

    /// Gets an immutable slice to the `VendorData` in this revision.
    pub fn vendor_data(&self) -> &[VendorData] {
        &self.vendor_data
    }
}

/// `MetadataBuilder` is the builder for [`Metadata`] revisions.
#[derive(Clone, Debug, Default)]
pub struct MetadataBuilder {
    metadata: MetadataRevision,
}

impl MetadataBuilder {
    /// Instantiate a new `MetadataBuilder`.
    pub fn new() -> Self {
        MetadataBuilder { metadata: Default::default() }
    }

    /// Add a `Tag` to the metadata.
    pub fn add_tag(&mut self, tag: Tag) -> &mut Self {
        self.metadata.tags.push(tag);
        self
    }

    /// Add a `Visual` to the metadata.
    pub fn add_visual(&mut self, visual: Visual) -> &mut Self {
        self.metadata.visuals.push(visual);
        self
    }

    /// Add `VendorData` to the metadata.
    pub fn add_vendor_data(&mut self, vendor_data: VendorData) -> &mut Self {
        self.metadata.vendor_data.push(vendor_data);
        self
    }

    /// Yield the constructed `Metadata` revision.
    pub fn metadata(self) -> MetadataRevision {
        self.metadata
    }
}

/// A reference to the metadata inside of a [`MetadataLog`].
#[derive(Debug)]
pub struct Metadata<'a> {
    revisions: &'a mut VecDeque<MetadataRevision>,
}

impl<'a> Metadata<'a> {
    /// Returns `true` if the current metadata revision is the newest, `false` otherwise.
    pub fn is_latest(&self) -> bool {
        self.revisions.len() <= 1
    }

    /// Gets an immutable reference to the current, and therefore oldest, revision of the metadata.
    pub fn current(&self) -> Option<&MetadataRevision> {
        self.revisions.front()
    }

    /// Skips to, and gets an immutable reference to the latest, and therefore newest, revision of
    /// the metadata.
    pub fn skip_to_latest(&mut self) -> Option<&MetadataRevision> {
        loop {
            if self.pop().is_none() {
                break;
            }
        }
        self.current()
    }

    /// If there are newer `Metadata` revisions, advances the `MetadataLog` by discarding the
    /// current revision and replacing it with the next revision, returning the discarded
    /// `Metadata`. When there are no newer revisions, `None` is returned. As such, `pop` will never
    /// completely empty the log.
    pub fn pop(&mut self) -> Option<MetadataRevision> {
        if self.revisions.len() > 1 {
            self.revisions.pop_front()
        }
        else {
            None
        }
    }
}

/// `MetadataLog` is a container for time-ordered [`Metadata`] revisions.
#[derive(Clone, Debug, Default)]
pub struct MetadataLog {
    revisions: VecDeque<MetadataRevision>,
}

impl MetadataLog {
    /// Returns a reference to the metadata revisions inside the log.
    pub fn metadata(&mut self) -> Metadata<'_> {
        Metadata { revisions: &mut self.revisions }
    }

    /// Push a new metadata revision to the end of the log.
    pub fn push(&mut self, rev: MetadataRevision) {
        self.revisions.push_back(rev);
    }

    /// Moves all metadata revisions from another metadata log to the end of this log.
    pub fn append(&mut self, other: &mut MetadataLog) {
        self.revisions.append(&mut other.revisions);
    }

    /// Push a metadata revision to the front of the log.
    pub fn push_front(&mut self, rev: MetadataRevision) {
        self.revisions.push_front(rev);
    }

    /// Moves all metadata revisions from another metadata log to the front of this log.
    pub fn append_front(&mut self, other: &mut MetadataLog) {
        // Maintain the relative ordering.
        while let Some(revision) = other.revisions.pop_back() {
            self.revisions.push_front(revision)
        }
    }
}

/// Enumeration of types of metadata side data.
#[non_exhaustive]
pub enum MetadataSideData {
    /// Chapter information.
    Chapters(ChapterGroup),
}

/// The decoded contents of read metadata.
pub struct MetadataBuffer {
    /// The revision of metadata containing tags, visuals, and vendor-specific metadata buffers.
    pub revision: MetadataRevision,
    /// Additional pieces of data stored in the metadata, but not part of a metadata revision. These
    /// pieces of data are usually passed to a format reader to support its function.
    pub side_data: Vec<MetadataSideData>,
}

/// A `MetadataReader` reads and decodes metadata and produces a revision of that decoded metadata.
pub trait MetadataReader: Send + Sync {
    /// Get basic information about the metadata format.
    fn metadata_info(&self) -> &MetadataInfo;

    /// Read all metadata and return it if successful.
    fn read_all(&mut self) -> Result<MetadataBuffer>;

    /// Consumes the `MetadataReader` and returns the underlying media source stream
    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's;
}

/// IDs for well-known metadata formats.
pub mod well_known {
    use super::MetadataId;

    // ID3 tags
    //---------

    /// ID3
    pub const METADATA_ID_ID3: MetadataId = MetadataId(0x100);
    /// ID3v2
    pub const METADATA_ID_ID3V2: MetadataId = MetadataId(0x101);

    // APE tags
    //---------

    /// APEv1
    pub const METADATA_ID_APEV1: MetadataId = MetadataId(0x200);
    /// APEv2
    pub const METADATA_ID_APEV2: MetadataId = MetadataId(0x201);
}
