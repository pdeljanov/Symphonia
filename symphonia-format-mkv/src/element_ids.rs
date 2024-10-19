// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::HashMap;

use lazy_static::lazy_static;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum Type {
    Master,
    Unsigned,
    Signed,
    Binary,
    String,
    Float,
    Date,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ElementType {
    Ebml,
    EbmlVersion,
    EbmlReadVersion,
    EbmlMaxIdLength,
    EbmlMaxSizeLength,
    DocType,
    DocTypeVersion,
    DocTypeReadVersion,
    Crc32,
    Void,
    Segment,
    SeekHead,
    Seek,
    SeekId,
    SeekPosition,
    Info,
    TimestampScale,
    Duration,
    DateUtc,
    Title,
    MuxingApp,
    WritingApp,
    Cluster,
    Timestamp,
    PrevSize,
    SimpleBlock,
    BlockGroup,
    Block,
    BlockAdditions,
    BlockMore,
    BlockAddId,
    BlockAddIdType,
    BlockAdditional,
    BlockAdditionMapping,
    BlockDuration,
    ReferenceBlock,
    DiscardPadding,
    Tracks,
    TrackEntry,
    TrackNumber,
    TrackUid,
    TrackType,
    FlagEnabled,
    FlagDefault,
    FlagForced,
    FlagHearingImpaired,
    FlagVisualImpaired,
    FlagTextDescriptions,
    FlagOriginal,
    FlagCommentary,
    FlagLacing,
    DefaultDuration,
    Name,
    Language,
    CodecId,
    CodecPrivate,
    CodecName,
    CodecDelay,
    SeekPreRoll,
    Video,
    FlagInterlaced,
    StereoMode,
    AlphaMode,
    PixelWidth,
    PixelHeight,
    PixelCropBottom,
    PixelCropTop,
    PixelCropLeft,
    PixelCropRight,
    DisplayWidth,
    DisplayHeight,
    DisplayUnit,
    AspectRatioType,
    DolbyVisionConfiguration,
    Audio,
    SamplingFrequency,
    OutputSamplingFrequency,
    Channels,
    BitDepth,
    ContentEncodings,
    ContentEncoding,
    ContentEncodingOrder,
    ContentEncodingScope,
    ContentEncodingType,
    ContentEncryption,
    ContentEncAlgo,
    ContentEncKeyId,
    ContentEncAesSettings,
    AesSettingsCipherMode,
    Colour,
    MatrixCoefficients,
    BitsPerChannel,
    ChromaSubsamplingHorz,
    ChromaSubsamplingVert,
    CbSubsamplingHorz,
    CbSubsamplingVert,
    ChromaSitingHorz,
    ChromaSitingVert,
    Range,
    TransferCharacteristics,
    Primaries,
    MaxCll,
    MaxFall,
    MasteringMetadata,
    PrimaryRChromaticityX,
    PrimaryRChromaticityY,
    PrimaryGChromaticityX,
    PrimaryGChromaticityY,
    PrimaryBChromaticityX,
    PrimaryBChromaticityY,
    WhitePointChromaticityX,
    WhitePointChromaticityY,
    LuminanceMax,
    LuminanceMin,
    Cues,
    CuePoint,
    CueTime,
    CueTrackPositions,
    CueTrack,
    CueClusterPosition,
    CueRelativePosition,
    CueDuration,
    CueBlockNumber,
    Chapters,
    EditionEntry,
    ChapterAtom,
    ChapterUid,
    ChapterStringUid,
    ChapterTimeStart,
    ChapterTimeEnd,
    ChapterDisplay,
    ChapString,
    ChapLanguage,
    ChapLanguageIetf,
    ChapCountry,
    Tags,
    Tag,
    Targets,
    TargetTypeValue,
    TargetType,
    TagTrackUid,
    SimpleTag,
    TagName,
    TagLanguage,
    TagDefault,
    TagString,
    TagBinary,
    /// Special type for unknown tags.
    Unknown,
}

impl ElementType {
    pub(crate) fn is_top_level(&self) -> bool {
        matches!(
            self,
            ElementType::Cluster
                | ElementType::Cues
                | ElementType::Info
                | ElementType::SeekHead
                | ElementType::Tags
                | ElementType::Tracks
        )
    }
}

lazy_static! {
    pub(crate) static ref ELEMENTS: HashMap<u32, (Type, ElementType)> = {
        let mut elems = HashMap::new();
        elems.insert(0x1A45DFA3, (Type::Master, ElementType::Ebml));
        elems.insert(0x4286, (Type::Unsigned, ElementType::EbmlVersion));
        elems.insert(0x42F7, (Type::Unsigned, ElementType::EbmlReadVersion));
        elems.insert(0x42F2, (Type::Unsigned, ElementType::EbmlMaxIdLength));
        elems.insert(0x42F3, (Type::Unsigned, ElementType::EbmlMaxSizeLength));
        elems.insert(0x4282, (Type::String, ElementType::DocType));
        elems.insert(0x4287, (Type::Unsigned, ElementType::DocTypeVersion));
        elems.insert(0x4285, (Type::Unsigned, ElementType::DocTypeReadVersion));
        elems.insert(0xBF, (Type::Binary, ElementType::Crc32));
        elems.insert(0xEC, (Type::Binary, ElementType::Void));
        elems.insert(0x18538067, (Type::Master, ElementType::Segment));
        elems.insert(0x114D9B74, (Type::Master, ElementType::SeekHead));
        elems.insert(0x4DBB, (Type::Master, ElementType::Seek));
        elems.insert(0x53AB, (Type::Unsigned, ElementType::SeekId));
        elems.insert(0x53AC, (Type::Unsigned, ElementType::SeekPosition));
        elems.insert(0x1549A966, (Type::Master, ElementType::Info));
        elems.insert(0x2AD7B1, (Type::Unsigned, ElementType::TimestampScale));
        elems.insert(0x4489, (Type::Float, ElementType::Duration));
        elems.insert(0x4461, (Type::Date, ElementType::DateUtc));
        elems.insert(0x7BA9, (Type::String, ElementType::Title));
        elems.insert(0x4D80, (Type::String, ElementType::MuxingApp));
        elems.insert(0x5741, (Type::String, ElementType::WritingApp));
        elems.insert(0x1F43B675, (Type::Master, ElementType::Cluster));
        elems.insert(0xE7, (Type::Unsigned, ElementType::Timestamp));
        elems.insert(0xAB, (Type::Unsigned, ElementType::PrevSize));
        elems.insert(0xA3, (Type::Binary, ElementType::SimpleBlock));
        elems.insert(0xA0, (Type::Master, ElementType::BlockGroup));
        elems.insert(0xA1, (Type::Binary, ElementType::Block));
        elems.insert(0x75A1, (Type::Master, ElementType::BlockAdditions));
        elems.insert(0xA6, (Type::Master, ElementType::BlockMore));
        elems.insert(0xEE, (Type::Unsigned, ElementType::BlockAddId));
        elems.insert(0x41E7, (Type::String, ElementType::BlockAddIdType));
        elems.insert(0xA5, (Type::Binary, ElementType::BlockAdditional));
        elems.insert(0x41E4, (Type::Binary, ElementType::BlockAdditionMapping));
        elems.insert(0x9B, (Type::Unsigned, ElementType::BlockDuration));
        elems.insert(0xFB, (Type::Signed, ElementType::ReferenceBlock));
        elems.insert(0x75A2, (Type::Signed, ElementType::DiscardPadding));
        elems.insert(0x1654AE6B, (Type::Master, ElementType::Tracks));
        elems.insert(0xAE, (Type::Master, ElementType::TrackEntry));
        elems.insert(0xD7, (Type::Unsigned, ElementType::TrackNumber));
        elems.insert(0x73C5, (Type::Unsigned, ElementType::TrackUid));
        elems.insert(0x83, (Type::Unsigned, ElementType::TrackType));
        elems.insert(0xB9, (Type::Unsigned, ElementType::FlagEnabled));
        elems.insert(0x88, (Type::Unsigned, ElementType::FlagDefault));
        elems.insert(0x55AA, (Type::Unsigned, ElementType::FlagForced));
        elems.insert(0x55AB, (Type::Unsigned, ElementType::FlagHearingImpaired));
        elems.insert(0x55AC, (Type::Unsigned, ElementType::FlagVisualImpaired));
        elems.insert(0x55AD, (Type::Unsigned, ElementType::FlagTextDescriptions));
        elems.insert(0x55AE, (Type::Unsigned, ElementType::FlagOriginal));
        elems.insert(0x55AF, (Type::Unsigned, ElementType::FlagCommentary));
        elems.insert(0x9C, (Type::Unsigned, ElementType::FlagLacing));
        elems.insert(0x23E383, (Type::Unsigned, ElementType::DefaultDuration));
        elems.insert(0x536E, (Type::String, ElementType::Name));
        elems.insert(0x22B59C, (Type::String, ElementType::Language));
        elems.insert(0x86, (Type::String, ElementType::CodecId));
        elems.insert(0x63A2, (Type::Binary, ElementType::CodecPrivate));
        elems.insert(0x258688, (Type::String, ElementType::CodecName));
        elems.insert(0x56AA, (Type::Unsigned, ElementType::CodecDelay));
        elems.insert(0x56BB, (Type::Unsigned, ElementType::SeekPreRoll));
        elems.insert(0xE0, (Type::Master, ElementType::Video));
        elems.insert(0x9A, (Type::Unsigned, ElementType::FlagInterlaced));
        elems.insert(0x53B8, (Type::Unsigned, ElementType::StereoMode));
        elems.insert(0x53C0, (Type::Unsigned, ElementType::AlphaMode));
        elems.insert(0xB0, (Type::Unsigned, ElementType::PixelWidth));
        elems.insert(0xBA, (Type::Unsigned, ElementType::PixelHeight));
        elems.insert(0x54AA, (Type::Unsigned, ElementType::PixelCropBottom));
        elems.insert(0x54BB, (Type::Unsigned, ElementType::PixelCropTop));
        elems.insert(0x54CC, (Type::Unsigned, ElementType::PixelCropLeft));
        elems.insert(0x54DD, (Type::Unsigned, ElementType::PixelCropRight));
        elems.insert(0x54B0, (Type::Unsigned, ElementType::DisplayWidth));
        elems.insert(0x54BA, (Type::Unsigned, ElementType::DisplayHeight));
        elems.insert(0x54B2, (Type::Unsigned, ElementType::DisplayUnit));
        elems.insert(0x54B3, (Type::Unsigned, ElementType::AspectRatioType));
        elems.insert(0x41ED, (Type::Binary, ElementType::DolbyVisionConfiguration));
        elems.insert(0xE1, (Type::Master, ElementType::Audio));
        elems.insert(0xB5, (Type::Float, ElementType::SamplingFrequency));
        elems.insert(0x78B5, (Type::Float, ElementType::OutputSamplingFrequency));
        elems.insert(0x9F, (Type::Unsigned, ElementType::Channels));
        elems.insert(0x6264, (Type::Unsigned, ElementType::BitDepth));
        elems.insert(0x6D80, (Type::Master, ElementType::ContentEncodings));
        elems.insert(0x6240, (Type::Master, ElementType::ContentEncoding));
        elems.insert(0x5031, (Type::Unsigned, ElementType::ContentEncodingOrder));
        elems.insert(0x5032, (Type::Unsigned, ElementType::ContentEncodingScope));
        elems.insert(0x5033, (Type::Unsigned, ElementType::ContentEncodingType));
        elems.insert(0x5035, (Type::Master, ElementType::ContentEncryption));
        elems.insert(0x47E1, (Type::Unsigned, ElementType::ContentEncAlgo));
        elems.insert(0x47E2, (Type::Unsigned, ElementType::ContentEncKeyId));
        elems.insert(0x47E7, (Type::Master, ElementType::ContentEncAesSettings));
        elems.insert(0x47E8, (Type::Unsigned, ElementType::AesSettingsCipherMode));
        elems.insert(0x55B0, (Type::Master, ElementType::Colour));
        elems.insert(0x55B1, (Type::Unsigned, ElementType::MatrixCoefficients));
        elems.insert(0x55B2, (Type::Unsigned, ElementType::BitsPerChannel));
        elems.insert(0x55B3, (Type::Unsigned, ElementType::ChromaSubsamplingHorz));
        elems.insert(0x55B4, (Type::Unsigned, ElementType::ChromaSubsamplingVert));
        elems.insert(0x55B5, (Type::Unsigned, ElementType::CbSubsamplingHorz));
        elems.insert(0x55B6, (Type::Unsigned, ElementType::CbSubsamplingVert));
        elems.insert(0x55B7, (Type::Unsigned, ElementType::ChromaSitingHorz));
        elems.insert(0x55B8, (Type::Unsigned, ElementType::ChromaSitingVert));
        elems.insert(0x55B9, (Type::Unsigned, ElementType::Range));
        elems.insert(0x55BA, (Type::Unsigned, ElementType::TransferCharacteristics));
        elems.insert(0x55BB, (Type::Unsigned, ElementType::Primaries));
        elems.insert(0x55BC, (Type::Unsigned, ElementType::MaxCll));
        elems.insert(0x55BD, (Type::Unsigned, ElementType::MaxFall));
        elems.insert(0x55D0, (Type::Master, ElementType::MasteringMetadata));
        elems.insert(0x55D1, (Type::Float, ElementType::PrimaryRChromaticityX));
        elems.insert(0x55D2, (Type::Float, ElementType::PrimaryRChromaticityY));
        elems.insert(0x55D3, (Type::Float, ElementType::PrimaryGChromaticityX));
        elems.insert(0x55D4, (Type::Float, ElementType::PrimaryGChromaticityY));
        elems.insert(0x55D5, (Type::Float, ElementType::PrimaryBChromaticityX));
        elems.insert(0x55D6, (Type::Float, ElementType::PrimaryBChromaticityY));
        elems.insert(0x55D7, (Type::Float, ElementType::WhitePointChromaticityX));
        elems.insert(0x55D8, (Type::Float, ElementType::WhitePointChromaticityY));
        elems.insert(0x55D9, (Type::Float, ElementType::LuminanceMax));
        elems.insert(0x55DA, (Type::Float, ElementType::LuminanceMin));
        elems.insert(0x1C53BB6B, (Type::Master, ElementType::Cues));
        elems.insert(0xBB, (Type::Master, ElementType::CuePoint));
        elems.insert(0xB3, (Type::Unsigned, ElementType::CueTime));
        elems.insert(0xB7, (Type::Master, ElementType::CueTrackPositions));
        elems.insert(0xF7, (Type::Unsigned, ElementType::CueTrack));
        elems.insert(0xF1, (Type::Unsigned, ElementType::CueClusterPosition));
        elems.insert(0xF0, (Type::Unsigned, ElementType::CueRelativePosition));
        elems.insert(0xB2, (Type::Unsigned, ElementType::CueDuration));
        elems.insert(0x5378, (Type::Unsigned, ElementType::CueBlockNumber));
        elems.insert(0x1043A770, (Type::Master, ElementType::Chapters));
        elems.insert(0x45B9, (Type::Master, ElementType::EditionEntry));
        elems.insert(0xB6, (Type::Master, ElementType::ChapterAtom));
        elems.insert(0x73C4, (Type::Unsigned, ElementType::ChapterUid));
        elems.insert(0x5654, (Type::String, ElementType::ChapterStringUid));
        elems.insert(0x91, (Type::Unsigned, ElementType::ChapterTimeStart));
        elems.insert(0x92, (Type::Unsigned, ElementType::ChapterTimeEnd));
        elems.insert(0x80, (Type::Master, ElementType::ChapterDisplay));
        elems.insert(0x85, (Type::String, ElementType::ChapString));
        elems.insert(0x437C, (Type::String, ElementType::ChapLanguage));
        elems.insert(0x437D, (Type::String, ElementType::ChapLanguageIetf));
        elems.insert(0x437E, (Type::String, ElementType::ChapCountry));
        elems.insert(0x1254C367, (Type::Master, ElementType::Tags));
        elems.insert(0x7373, (Type::Master, ElementType::Tag));
        elems.insert(0x63C0, (Type::Master, ElementType::Targets));
        elems.insert(0x68CA, (Type::Unsigned, ElementType::TargetTypeValue));
        elems.insert(0x63CA, (Type::String, ElementType::TargetType));
        elems.insert(0x63C5, (Type::Unsigned, ElementType::TagTrackUid));
        elems.insert(0x67C8, (Type::Master, ElementType::SimpleTag));
        elems.insert(0x45A3, (Type::String, ElementType::TagName));
        elems.insert(0x447A, (Type::String, ElementType::TagLanguage));
        elems.insert(0x4484, (Type::Unsigned, ElementType::TagDefault));
        elems.insert(0x4487, (Type::String, ElementType::TagString));
        elems.insert(0x4485, (Type::Binary, ElementType::TagBinary));
        elems
    };
}
