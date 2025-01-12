// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::rc::Rc;
use std::sync::Arc;

use symphonia_core::codecs::video::well_known::extra_data::{
    VIDEO_EXTRA_DATA_ID_DOLBY_VISION_CONFIG, VIDEO_EXTRA_DATA_ID_DOLBY_VISION_EL_HEVC,
};
use symphonia_core::codecs::video::VideoExtraData;
use symphonia_core::errors::{decode_error, Error, Result};
use symphonia_core::formats::{Attachment, FileAttachment, TrackFlags};
use symphonia_core::io::{BufReader, ReadBytes};
use symphonia_core::meta::{
    Chapter, ChapterGroup, ChapterGroupItem, MetadataBuilder, MetadataRevision, RawTag,
    RawTagSubField, StandardTag, Tag,
};
use symphonia_core::units::Time;

use crate::ebml::{
    read_unsigned_vint, Element, ElementData, ElementHeader, ElementIterator, ElementReader,
};
use crate::element_ids::ElementType;
use crate::lacing::calc_abs_block_timestamp;
use crate::sub_fields::*;
use crate::tags::{make_raw_tags, map_std_tag, TagContext, Target};

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct TrackElement {
    pub(crate) number: u64,
    pub(crate) uid: NonZeroU64,
    pub(crate) lang: String,
    pub(crate) lang_bcp47: Option<String>,
    pub(crate) codec_id: String,
    pub(crate) codec_private: Option<Box<[u8]>>,
    pub(crate) block_addition_mappings: Vec<BlockAdditionMappingElement>,
    pub(crate) audio: Option<AudioElement>,
    pub(crate) video: Option<VideoElement>,
    pub(crate) default_duration: Option<u64>,
    pub(crate) flags: TrackFlags,
}

impl Element for TrackElement {
    const ID: ElementType = ElementType::TrackEntry;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut number = None;
        let mut uid = None;
        let mut lang = None;
        let mut lang_bcp47 = None;
        let mut audio = None;
        let mut video = None;
        let mut codec_private = None;
        let mut block_addition_mappings = Vec::new();
        let mut codec_id = None;
        let mut default_duration = None;
        let mut flags = Default::default();

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::TrackNumber => {
                    // TODO: 0 is invalid.
                    number = Some(it.read_u64()?);
                }
                ElementType::TrackUid => {
                    uid = match NonZeroU64::new(it.read_u64()?) {
                        None => return decode_error("mkv: invalid track uid"),
                        uid => uid,
                    };
                }
                ElementType::Language => {
                    lang = Some(it.read_string()?);
                }
                ElementType::LanguageBcp47 => {
                    lang_bcp47 = Some(it.read_string()?);
                }
                ElementType::CodecId => {
                    codec_id = Some(it.read_string()?);
                }
                ElementType::CodecPrivate => {
                    codec_private = Some(it.read_boxed_slice()?);
                }
                ElementType::Audio => {
                    audio = Some(it.read_element_data()?);
                }
                ElementType::Video => {
                    video = Some(it.read_element_data()?);
                }
                ElementType::BlockAdditionMapping => {
                    block_addition_mappings.push(it.read_element_data()?);
                }
                ElementType::DefaultDuration => {
                    default_duration = Some(it.read_u64()?);
                }
                ElementType::FlagDefault => {
                    if it.read_u64()? == 1 {
                        flags |= TrackFlags::DEFAULT;
                    }
                }
                ElementType::FlagForced => {
                    if it.read_u64()? == 1 {
                        flags |= TrackFlags::FORCED;
                    }
                }
                ElementType::FlagHearingImpaired => {
                    if it.read_u64()? == 1 {
                        flags |= TrackFlags::HEARING_IMPAIRED;
                    }
                }
                ElementType::FlagVisualImpaired => {
                    if it.read_u64()? == 1 {
                        flags |= TrackFlags::VISUALLY_IMPAIRED;
                    }
                }
                ElementType::FlagTextDescriptions => {
                    if it.read_u64()? == 1 {
                        flags |= TrackFlags::TEXT_DESCRIPTIONS;
                    }
                }
                ElementType::FlagOriginal => {
                    if it.read_u64()? == 1 {
                        flags |= TrackFlags::ORIGINAL_LANGUAGE;
                    }
                }
                ElementType::FlagCommentary => {
                    if it.read_u64()? == 1 {
                        flags |= TrackFlags::COMMENTARY;
                    }
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self {
            number: number.ok_or(Error::DecodeError("mkv: missing track number"))?,
            uid: uid.ok_or(Error::DecodeError("mkv: missing track UID"))?,
            lang: lang.unwrap_or_else(|| "eng".into()),
            lang_bcp47,
            codec_id: codec_id.ok_or(Error::DecodeError("mkv: missing codec id"))?,
            codec_private,
            block_addition_mappings,
            audio,
            video,
            default_duration,
            flags,
        })
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct AudioElement {
    pub(crate) sampling_frequency: f64,
    pub(crate) output_sampling_frequency: Option<f64>,
    pub(crate) channels: u64,
    pub(crate) bit_depth: Option<u64>,
}

impl Element for AudioElement {
    const ID: ElementType = ElementType::Audio;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut sampling_frequency = None;
        let mut output_sampling_frequency = None;
        let mut channels = None;
        let mut bit_depth = None;

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::SamplingFrequency => {
                    sampling_frequency = Some(it.read_f64()?);
                }
                ElementType::OutputSamplingFrequency => {
                    output_sampling_frequency = Some(it.read_f64()?);
                }
                ElementType::Channels => {
                    channels = Some(it.read_u64()?);
                }
                ElementType::BitDepth => {
                    bit_depth = Some(it.read_u64()?);
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self {
            sampling_frequency: sampling_frequency.unwrap_or(8000.0),
            output_sampling_frequency,
            channels: channels.unwrap_or(1),
            bit_depth,
        })
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct VideoElement {
    pub(crate) pixel_width: u16,
    pub(crate) pixel_height: u16,
}

impl Element for VideoElement {
    const ID: ElementType = ElementType::Video;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut pixel_width = None;
        let mut pixel_height = None;

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::PixelWidth => {
                    pixel_width = Some(it.read_u64()? as u16);
                }
                ElementType::PixelHeight => {
                    pixel_height = Some(it.read_u64()? as u16);
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self { pixel_width: pixel_width.unwrap_or(0), pixel_height: pixel_height.unwrap_or(0) })
    }
}

#[derive(Debug)]
pub(crate) struct BlockAdditionMappingElement {
    pub(crate) extra_data: Option<VideoExtraData>,
}

impl Element for BlockAdditionMappingElement {
    const ID: ElementType = ElementType::BlockAdditionMapping;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        // There can be many BlockAdditionMapping elements with DolbyVisionConfiguration in a single track
        // BlockAddIdType FourCC string allows to determine the type of DolbyVisionConfiguration extra data
        let mut extra_data = None;
        let mut block_add_id_type = String::new();

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::BlockAddIdType => {
                    block_add_id_type = it.read_string()?;
                }
                ElementType::DolbyVisionConfiguration => match block_add_id_type.as_str() {
                    "dvcC" | "dvvC" => {
                        extra_data = Some(VideoExtraData {
                            id: VIDEO_EXTRA_DATA_ID_DOLBY_VISION_CONFIG,
                            data: it.read_boxed_slice()?,
                        });
                    }
                    "hvcE" => {
                        extra_data = Some(VideoExtraData {
                            id: VIDEO_EXTRA_DATA_ID_DOLBY_VISION_EL_HEVC,
                            data: it.read_boxed_slice()?,
                        });
                    }
                    _ => {}
                },
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self { extra_data })
    }
}

#[derive(Debug)]
pub(crate) struct SeekHeadElement {
    pub(crate) seeks: Box<[SeekElement]>,
}

impl Element for SeekHeadElement {
    const ID: ElementType = ElementType::SeekHead;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut seeks = Vec::new();

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::Seek => {
                    seeks.push(it.read_element_data()?);
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self { seeks: seeks.into_boxed_slice() })
    }
}

#[derive(Debug)]
pub(crate) struct SeekElement {
    pub(crate) id: u64,
    pub(crate) position: u64,
}

impl Element for SeekElement {
    const ID: ElementType = ElementType::Seek;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut seek_id = None;
        let mut seek_position = None;

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::SeekId => {
                    seek_id = Some(it.read_u64()?);
                }
                ElementType::SeekPosition => {
                    seek_position = Some(it.read_u64()?);
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self {
            id: seek_id.ok_or(Error::DecodeError("mkv: missing seek track id"))?,
            position: seek_position.ok_or(Error::DecodeError("mkv: missing seek track pos"))?,
        })
    }
}

#[derive(Debug)]
pub(crate) struct TracksElement {
    pub(crate) tracks: Box<[TrackElement]>,
}

impl Element for TracksElement {
    const ID: ElementType = ElementType::Tracks;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, _parent: ElementHeader) -> Result<Self> {
        Ok(Self { tracks: it.read_elements()? })
    }
}

impl TracksElement {
    pub(crate) fn get_target_uids(&self, target_tags: &mut TargetTagsMap) {
        self.tracks.iter().for_each(|track| {
            target_tags.insert(TargetUid::Track(track.uid.get()), Default::default());
        })
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct EbmlHeaderElement {
    pub(crate) version: u64,
    pub(crate) read_version: u64,
    pub(crate) max_id_length: u64,
    pub(crate) max_size_length: u64,
    pub(crate) doc_type: String,
    pub(crate) doc_type_version: u64,
    pub(crate) doc_type_read_version: u64,
}

impl Element for EbmlHeaderElement {
    const ID: ElementType = ElementType::Ebml;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut version = None;
        let mut read_version = None;
        let mut max_id_length = None;
        let mut max_size_length = None;
        let mut doc_type = None;
        let mut doc_type_version = None;
        let mut doc_type_read_version = None;

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::EbmlVersion => {
                    version = Some(it.read_u64()?);
                }
                ElementType::EbmlReadVersion => {
                    read_version = Some(it.read_u64()?);
                }
                ElementType::EbmlMaxIdLength => {
                    max_id_length = Some(it.read_u64()?);
                }
                ElementType::EbmlMaxSizeLength => {
                    max_size_length = Some(it.read_u64()?);
                }
                ElementType::DocType => {
                    doc_type = Some(it.read_string()?);
                }
                ElementType::DocTypeVersion => {
                    doc_type_version = Some(it.read_u64()?);
                }
                ElementType::DocTypeReadVersion => {
                    doc_type_read_version = Some(it.read_u64()?);
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self {
            version: version.unwrap_or(1),
            read_version: read_version.unwrap_or(1),
            max_id_length: max_id_length.unwrap_or(4),
            max_size_length: max_size_length.unwrap_or(8),
            doc_type: doc_type.ok_or(Error::Unsupported("mkv: invalid ebml file"))?,
            doc_type_version: doc_type_version.unwrap_or(1),
            doc_type_read_version: doc_type_read_version.unwrap_or(1),
        })
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct InfoElement {
    pub(crate) timestamp_scale: u64,
    pub(crate) duration: Option<f64>,
    title: Option<Box<str>>,
    muxing_app: Box<str>,
    writing_app: Box<str>,
}

impl Element for InfoElement {
    const ID: ElementType = ElementType::Info;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut duration = None;
        let mut timestamp_scale = None;
        let mut title = None;
        let mut muxing_app = None;
        let mut writing_app = None;

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::TimestampScale => {
                    timestamp_scale = Some(it.read_u64()?);
                }
                ElementType::Duration => {
                    duration = Some(it.read_f64()?);
                }
                ElementType::Title => {
                    title = Some(it.read_string()?);
                }
                ElementType::MuxingApp => {
                    muxing_app = Some(it.read_string()?);
                }
                ElementType::WritingApp => {
                    writing_app = Some(it.read_string()?);
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self {
            timestamp_scale: timestamp_scale.unwrap_or(1_000_000),
            duration,
            title: title.map(|it| it.into_boxed_str()),
            muxing_app: muxing_app.unwrap_or_default().into_boxed_str(),
            writing_app: writing_app.unwrap_or_default().into_boxed_str(),
        })
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct CuesElement {
    pub(crate) points: Box<[CuePointElement]>,
}

impl Element for CuesElement {
    const ID: ElementType = ElementType::Cues;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, _parent: ElementHeader) -> Result<Self> {
        Ok(Self { points: it.read_elements()? })
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct CuePointElement {
    pub(crate) time: u64,
    pub(crate) positions: CueTrackPositionsElement,
}

impl Element for CuePointElement {
    const ID: ElementType = ElementType::CuePoint;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut time = None;
        let mut pos = None;
        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::CueTime => time = Some(it.read_u64()?),
                ElementType::CueTrackPositions => {
                    pos = Some(it.read_element_data()?);
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self {
            time: time.ok_or(Error::DecodeError("mkv: missing time in cue"))?,
            positions: pos.ok_or(Error::DecodeError("mkv: missing positions in cue"))?,
        })
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct CueTrackPositionsElement {
    pub(crate) track: u64,
    pub(crate) cluster_position: u64,
}

impl Element for CueTrackPositionsElement {
    const ID: ElementType = ElementType::CueTrackPositions;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut track = None;
        let mut pos = None;
        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::CueTrack => {
                    track = Some(it.read_u64()?);
                }
                ElementType::CueClusterPosition => {
                    pos = Some(it.read_u64()?);
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }
        Ok(Self {
            track: track.ok_or(Error::DecodeError("mkv: missing track in cue track positions"))?,
            cluster_position: pos
                .ok_or(Error::DecodeError("mkv: missing position in cue track positions"))?,
        })
    }
}

#[derive(Debug)]
pub(crate) struct BlockGroupElement {
    pub(crate) data: Box<[u8]>,
    pub(crate) duration: Option<u64>,
}

impl Element for BlockGroupElement {
    const ID: ElementType = ElementType::BlockGroup;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut data = None;
        let mut block_duration = None;
        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::DiscardPadding => {
                    let _nanos = it.read_data()?;
                }
                ElementType::Block => {
                    data = Some(it.read_boxed_slice()?);
                }
                ElementType::BlockDuration => {
                    block_duration = Some(it.read_u64()?);
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }
        Ok(Self {
            data: data.ok_or(Error::DecodeError("mkv: missing block inside block group"))?,
            duration: block_duration,
        })
    }
}

#[derive(Debug)]
pub(crate) struct BlockElement {
    pub(crate) track: u64,
    pub(crate) timestamp: u64,
    pub(crate) pos: u64,
}

#[derive(Debug)]
pub(crate) struct ClusterElement {
    pub(crate) timestamp: u64,
    pub(crate) pos: u64,
    pub(crate) end: Option<u64>,
    pub(crate) blocks: Box<[BlockElement]>,
}

impl Element for ClusterElement {
    const ID: ElementType = ElementType::Cluster;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let pos = it.pos();
        let mut timestamp = None;
        let mut blocks = Vec::new();
        let has_size = parent.end().is_some();

        fn read_block(data: &[u8], timestamp: u64, offset: u64) -> Result<BlockElement> {
            let mut reader = BufReader::new(data);
            let track = read_unsigned_vint(&mut reader)?;
            let rel_ts = reader.read_be_u16()? as i16;
            let timestamp = calc_abs_block_timestamp(timestamp, rel_ts);
            Ok(BlockElement { track, timestamp, pos: offset })
        }

        fn get_timestamp(timestamp: Option<u64>) -> Result<u64> {
            timestamp.ok_or(Error::DecodeError("mkv: missing timestamp for a cluster"))
        }

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::Timestamp => {
                    timestamp = Some(it.read_u64()?);
                }
                ElementType::BlockGroup => {
                    let group = it.read_element_data::<BlockGroupElement>()?;
                    blocks.push(read_block(&group.data, get_timestamp(timestamp)?, header.pos)?);
                }
                ElementType::SimpleBlock => {
                    let data = it.read_boxed_slice()?;
                    blocks.push(read_block(&data, get_timestamp(timestamp)?, header.pos)?);
                }
                _ if header.etype.is_top_level() && !has_size => break,
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(ClusterElement {
            timestamp: get_timestamp(timestamp)?,
            blocks: blocks.into_boxed_slice(),
            pos,
            end: parent.end(),
        })
    }
}

#[derive(Debug)]
pub(crate) struct TagsElement {
    pub(crate) tags: Box<[TagElement]>,
}

impl Element for TagsElement {
    const ID: ElementType = ElementType::Tags;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut tags = Vec::new();

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::Tag => {
                    tags.push(it.read_element_data::<TagElement>()?);
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self { tags: tags.into_boxed_slice() })
    }
}

/// Map of a target-specific UID to a vector of tags.
pub type TargetTagsMap = HashMap<TargetUid, Vec<Tag>>;

impl TagsElement {
    pub(crate) fn into_metadata(
        mut self,
        target_tags: &mut TargetTagsMap,
        is_video: bool,
    ) -> MetadataRevision {
        /// UID -> (Last tag context, Tag vector)
        type UidMap = HashMap<u64, (TagContext, Vec<Tag>)>;

        /// Append tags to the item in the UID map for the given UID.
        fn append_to_map(map: &mut UidMap, uid: u64, raw_tags: &Vec<RawTag>, target: &Target) {
            if let Some(item) = map.get_mut(&uid) {
                // Attempt to generate a standard tag for each raw tag using the last context for
                // the current item, and push a new tag.
                for raw in raw_tags {
                    item.1.push(Tag { raw: raw.clone(), std: map_std_tag(raw, &item.0) });
                }
                // Update the last target of the context for the given item. Note, this is a cheap
                // clone (internal RC).
                item.0.target = Some(target.clone());
            }
        }

        /// Append tags to all items in the UID map.
        fn append_to_map_all(map: &mut UidMap, raw_tags: &Vec<RawTag>, target: &Target) {
            for (_, item) in map.iter_mut() {
                // Attempt to generate a standard tag for each raw tag using the last context for
                // the current item, and push a new tag.
                for raw in raw_tags {
                    item.1.push(Tag { raw: raw.clone(), std: map_std_tag(raw, &item.0) });
                }
                // Update the last target of the context for the given item. Note, this is a cheap
                // clone (internal RC).
                item.0.target = Some(target.clone());
            }
        }

        /// Append tags to media.
        fn append_to_media(
            builder: &mut MetadataBuilder,
            raw_tags: &Vec<RawTag>,
            target: &Option<Target>,
            ctx: &mut TagContext,
        ) {
            // Attempt to generate a standard tag for each raw tag using the last media context, and
            // push a new tag.
            for raw in raw_tags {
                builder.add_tag(Tag { raw: raw.clone(), std: map_std_tag(raw, ctx) });
            }
            // Update the last target of the media tag context. Note, this is a cheap clone
            // (internal RC).
            ctx.target = target.clone();
        }

        let mut builder = MetadataBuilder::new();
        let mut media_target = TagContext { is_video, target: None };

        let mut tracks: UidMap = Default::default();
        let mut editions: UidMap = Default::default();
        let mut chapters: UidMap = Default::default();
        let mut attachments: UidMap = Default::default();

        // Pre-populate maps using known track, edition, chapter, and attachment UIDs.
        for (uid, _) in target_tags.iter() {
            let default = (media_target.clone(), Vec::new());
            match uid {
                TargetUid::Track(uid) => tracks.insert(*uid, default),
                TargetUid::Edition(uid) => editions.insert(*uid, default),
                TargetUid::Chapter(uid) => chapters.insert(*uid, default),
                TargetUid::Attachment(uid) => attachments.insert(*uid, default),
            };
        }

        // Sort all tag elements in order of ascending target level. Tag elements without a target
        // or target level should be last. This sort is stable so tag elements at the same target
        // level will be in the same relative position as they were read.
        self.tags.sort_by_key(|tag| {
            tag.targets.as_ref().map(|targets| targets.target_type_value).unwrap_or(u64::MAX)
        });

        for tag in self.tags {
            // Tag context for the current tag element.
            let ctx = TagContext {
                is_video,
                target: tag.targets.as_ref().map(|t| Target {
                    value: t.target_type_value,
                    name: t.target_type.clone().map(Rc::new),
                }),
            };

            // Generate a vector of raw tags from simple tag elements. This vector will be appended
            // to the appropriate targets or media.
            let mut raw_tags = Vec::with_capacity(tag.simple_tags.len());

            for tag in tag.simple_tags {
                make_raw_tags(tag, &ctx, &mut raw_tags);
            }

            // Append tags to targets.
            if let Some(targets) = tag.targets {
                if !targets.uids.is_empty() {
                    let target = ctx.target.unwrap();

                    // Append tags to specific to tracks, editions, chapters, or attachments.
                    for uid in targets.uids {
                        match uid {
                            TargetUid::Track(uid) if !targets.all_tracks => {
                                append_to_map(&mut tracks, uid, &raw_tags, &target);
                            }
                            TargetUid::Edition(uid) if !targets.all_editions => {
                                append_to_map(&mut editions, uid, &raw_tags, &target);
                            }
                            TargetUid::Chapter(uid) if !targets.all_chapters => {
                                append_to_map(&mut chapters, uid, &raw_tags, &target);
                            }
                            TargetUid::Attachment(uid) if !targets.all_attachments => {
                                append_to_map(&mut attachments, uid, &raw_tags, &target);
                            }
                            _ => (),
                        }
                    }

                    // Append tags to all tracks.
                    if targets.all_tracks {
                        append_to_map_all(&mut tracks, &raw_tags, &target);
                    }
                    // Append tags to all editions.
                    if targets.all_editions {
                        append_to_map_all(&mut editions, &raw_tags, &target);
                    }
                    // Append tags to all chapters.
                    if targets.all_chapters {
                        append_to_map_all(&mut chapters, &raw_tags, &target);
                    }
                    // Append tags to all attachments.
                    if targets.all_attachments {
                        append_to_map_all(&mut attachments, &raw_tags, &target);
                    }
                }
                else {
                    // No target UID(s). Append tags to the entire media.
                    append_to_media(&mut builder, &raw_tags, &ctx.target, &mut media_target);
                }
            }
            else {
                // No targets. Append tags to the entire media.
                append_to_media(&mut builder, &raw_tags, &None, &mut media_target);
            }
        }

        // SAFETY: There needs to be sane limits (e.g., 1024 each category).

        // Return edition target tags.
        for (uid, mut value) in editions {
            let tags = target_tags.entry(TargetUid::Edition(uid)).or_default();
            tags.append(&mut value.1);
        }
        // Return chapter target tags.
        for (uid, mut value) in chapters {
            let tags = target_tags.entry(TargetUid::Chapter(uid)).or_default();
            tags.append(&mut value.1);
        }
        // Return attachment target tags.
        for (uid, mut value) in attachments {
            let tags = target_tags.entry(TargetUid::Attachment(uid)).or_default();
            tags.append(&mut value.1);
        }

        // Return media and track-level metadata.
        builder.metadata()
    }
}

#[derive(Debug)]
pub(crate) struct TagElement {
    pub(crate) simple_tags: Box<[SimpleTagElement]>,
    pub(crate) targets: Option<TargetsElement>,
}

impl Element for TagElement {
    const ID: ElementType = ElementType::Tag;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut simple_tags = Vec::new();
        let mut targets = None;

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::Targets => {
                    targets = Some(it.read_element_data::<TargetsElement>()?);
                }
                ElementType::SimpleTag => {
                    simple_tags.push(it.read_element_data::<SimpleTagElement>()?);
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self { simple_tags: simple_tags.into_boxed_slice(), targets })
    }
}

#[derive(Debug, Hash, PartialEq, Eq)]
pub(crate) enum TargetUid {
    Track(u64),
    Edition(u64),
    Chapter(u64),
    Attachment(u64),
}

#[derive(Debug)]
pub(crate) struct TargetsElement {
    pub(crate) target_type_value: u64,
    pub(crate) target_type: Option<Box<str>>,
    pub(crate) uids: Vec<TargetUid>,
    pub(crate) all_tracks: bool,
    pub(crate) all_editions: bool,
    pub(crate) all_chapters: bool,
    pub(crate) all_attachments: bool,
}

impl Element for TargetsElement {
    const ID: ElementType = ElementType::Targets;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut target_type_value = None;
        let mut target_type = None;
        let mut uids = Vec::new();
        let mut all_tracks = false;
        let mut all_editions = false;
        let mut all_chapters = false;
        let mut all_attachments = false;

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::TargetTypeValue => {
                    target_type_value = Some(it.read_u64()?);
                }
                ElementType::TargetType => {
                    target_type = Some(it.read_string()?);
                }
                ElementType::TagTrackUid => {
                    let uid = it.read_u64()?;
                    uids.push(TargetUid::Track(uid));
                    // If the UID is 0, then all tracks are targets.
                    if uid == 0 {
                        all_tracks = true;
                    }
                }
                ElementType::TagEditionUid => {
                    let uid = it.read_u64()?;
                    uids.push(TargetUid::Edition(uid));
                    // If the UID is 0, then all editions are targets.
                    if uid == 0 {
                        all_editions = true;
                    }
                }
                ElementType::TagChapterUid => {
                    let uid = it.read_u64()?;
                    uids.push(TargetUid::Chapter(uid));
                    // If the UID is 0, then all chapters are targets.
                    if uid == 0 {
                        all_chapters = true;
                    }
                }
                ElementType::TagAttachmentUid => {
                    let uid = it.read_u64()?;
                    uids.push(TargetUid::Attachment(uid));
                    // If the UID is 0, then all attachments are targets.
                    if uid == 0 {
                        all_attachments = true;
                    }
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self {
            target_type_value: target_type_value.unwrap_or(50),
            target_type: target_type.map(|t| t.into_boxed_str()),
            uids,
            all_tracks,
            all_editions,
            all_chapters,
            all_attachments,
        })
    }
}

#[derive(Debug)]
pub(crate) struct SimpleTagElement {
    pub(crate) name: Box<str>,
    pub(crate) value: Option<ElementData>,
    pub(crate) is_default: bool,
    pub(crate) lang: String,
    pub(crate) lang_bcp47: Option<String>,
    pub(crate) sub_tags: Vec<SimpleTagElement>,
}

impl Element for SimpleTagElement {
    const ID: ElementType = ElementType::SimpleTag;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut name = None;
        let mut value = None;
        let mut lang = None;
        let mut lang_bcp47 = None;
        let mut is_default = true;
        let mut sub_tags = Vec::new();

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::TagName => {
                    name = Some(it.read_string()?);
                }
                ElementType::TagString | ElementType::TagBinary => {
                    value = Some(it.read_data()?);
                }
                ElementType::TagLanguage => {
                    lang = Some(it.read_string()?);
                }
                ElementType::TagLanguageBcp47 => {
                    lang_bcp47 = Some(it.read_string()?);
                }
                ElementType::TagDefault => {
                    is_default = it.read_u64()? == 1;
                }
                ElementType::SimpleTag => {
                    // Simple tag elements exist at a depth >= 3. Only support 3 levels of nesting
                    // as this is enough to support Matroska's standardized tagging scheme.
                    if parent.depth < 6 {
                        sub_tags.push(it.read_element_data::<SimpleTagElement>()?);
                    }
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self {
            name: name.ok_or(Error::DecodeError("mkv: missing tag name"))?.into_boxed_str(),
            value,
            lang: lang.unwrap_or_else(|| "und".into()),
            lang_bcp47,
            is_default,
            sub_tags,
        })
    }
}

#[derive(Debug)]
pub(crate) struct AttachedFileElement {
    pub(crate) uid: NonZeroU64,
    pub(crate) name: String,
    pub(crate) desc: Option<String>,
    pub(crate) media_type: String,
    pub(crate) data: Box<[u8]>,
}

impl Element for AttachedFileElement {
    const ID: ElementType = ElementType::AttachedFile;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut uid = None;
        let mut name = None;
        let mut desc = None;
        let mut media_type = None;
        let mut data = None;

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::FileDescription => {
                    desc = Some(it.read_string()?);
                }
                ElementType::FileName => {
                    name = Some(it.read_string()?);
                }
                ElementType::FileMediaType => {
                    media_type = Some(it.read_string()?);
                }
                ElementType::FileData => {
                    data = Some(it.read_boxed_slice()?);
                }
                ElementType::FileUid => {
                    uid = match NonZeroU64::new(it.read_u64()?) {
                        None => return decode_error("mkv: invalid file uid"),
                        uid => uid,
                    };
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self {
            uid: uid.ok_or(Error::DecodeError("mkv: missing attached file uid"))?,
            name: name.ok_or(Error::DecodeError("mkv: missing attached file name"))?,
            desc,
            media_type: media_type
                .ok_or(Error::DecodeError("mkv: missing attached file media-type"))?,
            data: data.ok_or(Error::DecodeError("mkv: missing attached file data"))?,
        })
    }
}

#[derive(Debug)]
pub(crate) struct AttachmentsElement {
    pub(crate) attached_files: Box<[AttachedFileElement]>,
}

impl Element for AttachmentsElement {
    const ID: ElementType = ElementType::Attachments;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut attached_files = Vec::new();

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::AttachedFile => {
                    attached_files.push(it.read_element_data::<AttachedFileElement>()?);
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self { attached_files: attached_files.into_boxed_slice() })
    }
}

impl AttachmentsElement {
    pub(crate) fn get_target_uids(&self, target_tags: &mut TargetTagsMap) {
        self.attached_files.iter().for_each(|file| {
            target_tags.insert(TargetUid::Attachment(file.uid.get()), Default::default());
        })
    }

    pub(crate) fn into_attachments(self, _target_tags: &mut TargetTagsMap) -> Vec<Attachment> {
        self.attached_files
            .into_vec()
            .into_iter()
            .map(|file| {
                Attachment::File(FileAttachment {
                    name: file.name,
                    description: file.desc,
                    media_type: Some(file.media_type),
                    data: file.data,
                })
            })
            .collect()
    }
}

#[derive(Debug)]
pub(crate) struct ChaptersElement {
    pub(crate) editions: Box<[EditionEntryElement]>,
}

impl Element for ChaptersElement {
    const ID: ElementType = ElementType::Chapters;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut editions = Vec::new();

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::EditionEntry => {
                    editions.push(it.read_element_data::<EditionEntryElement>()?);
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self { editions: editions.into_boxed_slice() })
    }
}

impl ChaptersElement {
    pub(crate) fn get_target_uids(&self, target_tags: &mut TargetTagsMap) {
        self.editions.iter().for_each(|edition| edition.get_target_uids(target_tags));
    }

    /// Builds a chapter group containing the chapters of the first non-hidden default edition, or,
    /// if one doesn't exist, the first non-hidden edition.
    pub(crate) fn into_chapter_group(
        self,
        target_tags: &mut TargetTagsMap,
    ) -> Option<ChapterGroup> {
        // Find the first non-hidden default edition. Or, the first non-hidden edition.
        let index = self
            .editions
            .iter()
            .position(|e| !e.is_hidden && e.is_default)
            .or_else(|| self.editions.iter().position(|e| !e.is_hidden));

        // Convert to chapter group.
        index.map(|index| {
            self.editions.into_vec().swap_remove(index).into_chapter_group(target_tags)
        })
    }
}

#[derive(Debug)]
pub(crate) struct EditionEntryElement {
    pub(crate) uid: NonZeroU64,
    pub(crate) is_hidden: bool,
    pub(crate) is_default: bool,
    #[allow(dead_code)]
    pub(crate) is_ordered: bool,
    pub(crate) display: Box<[EditionDisplayElement]>,
    pub(crate) chapters: Box<[ChapterAtomElement]>,
}

impl Element for EditionEntryElement {
    const ID: ElementType = ElementType::EditionEntry;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut uid = None;
        let mut is_hidden = false;
        let mut is_default = false;
        let mut is_ordered = false;
        let mut display = Vec::new();
        let mut chapters = Vec::new();

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::EditionUid => {
                    uid = match NonZeroU64::new(it.read_u64()?) {
                        None => return decode_error("mkv: invalid edition uid"),
                        uid => uid,
                    };
                }
                ElementType::EditionFlagHidden => {
                    is_hidden = it.read_u64()? == 1;
                }
                ElementType::EditionFlagDefault => {
                    is_default = it.read_u64()? == 1;
                }
                ElementType::EditionFlagOrdered => {
                    is_ordered = it.read_u64()? == 1;
                }
                ElementType::EditionDisplay => {
                    display.push(it.read_element_data::<EditionDisplayElement>()?)
                }
                ElementType::ChapterAtom => {
                    chapters.push(it.read_element_data::<ChapterAtomElement>()?)
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self {
            uid: uid.ok_or(Error::DecodeError("mkv: missing edition uid"))?,
            is_hidden,
            is_default,
            is_ordered,
            display: display.into_boxed_slice(),
            chapters: chapters.into_boxed_slice(),
        })
    }
}

impl EditionEntryElement {
    pub(crate) fn get_target_uids(&self, target_tags: &mut TargetTagsMap) {
        // Append edition UID.
        target_tags.insert(TargetUid::Edition(self.uid.get()), Default::default());
        // Append chapter UIDs.
        self.chapters.iter().for_each(|chapter| chapter.get_target_uids(target_tags));
    }

    pub(crate) fn into_chapter_group(self, target_tags: &mut TargetTagsMap) -> ChapterGroup {
        // Take the vector of tags associated with this edition, if any.
        let mut tags = target_tags
            .remove(&TargetUid::Edition(self.uid.get()))
            .unwrap_or_else(|| Vec::with_capacity(self.display.len()));

        // Edition title tags.
        for display in self.display {
            let mut sub_fields = Vec::with_capacity(1);

            // Edition language sub-field.
            if let Some(lang) = display.lang_bcp47 {
                // BCP47 language code is present.
                sub_fields.push(RawTagSubField::new(EDITION_TITLE_LANGUAGE_BCP47, lang));
            }

            let title = Arc::new(display.name);

            let raw = RawTag::new_with_sub_fields(
                "ChapString",
                title.clone(),
                sub_fields.into_boxed_slice(),
            );

            tags.push(Tag::new_std(raw, StandardTag::ChapterTitle(title)));
        }

        // Edition chapters.
        let mut items = Vec::with_capacity(self.chapters.len());

        for chapter in self.chapters {
            if !chapter.is_hidden {
                items.push(chapter.into_chapter_group_item(target_tags));
            }
        }

        ChapterGroup { items, tags, visuals: vec![] }
    }
}

#[derive(Debug)]
pub(crate) struct EditionDisplayElement {
    pub(crate) name: String,
    pub(crate) lang_bcp47: Option<String>,
}

impl Element for EditionDisplayElement {
    const ID: ElementType = ElementType::EditionDisplay;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut name = None;
        let mut lang_bcp47 = None;

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::EditionString => {
                    name = Some(it.read_string()?);
                }
                ElementType::EditionLanguageBcp47 => {
                    lang_bcp47 = Some(it.read_string()?);
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self {
            name: name.ok_or(Error::DecodeError("mkv: missing edition display name"))?,
            lang_bcp47,
        })
    }
}

#[derive(Debug)]
pub(crate) struct ChapterAtomElement {
    pub(crate) uid: NonZeroU64,
    #[allow(dead_code)]
    pub(crate) is_enabled: bool,
    pub(crate) is_hidden: bool,
    pub(crate) time_start: u64,
    pub(crate) time_end: Option<u64>,
    pub(crate) skip_type: Option<u8>,
    pub(crate) display: Box<[ChapterDisplayElement]>,
    pub(crate) chapters: Box<[ChapterAtomElement]>,
}

impl Element for ChapterAtomElement {
    const ID: ElementType = ElementType::ChapterAtom;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut uid = None;
        let mut is_enabled = false;
        let mut is_hidden = false;
        let mut time_start = None;
        let mut time_end = None;
        let mut skip_type = None;
        let mut display = Vec::new();
        let mut chapters = Vec::new();

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::ChapterUid => {
                    uid = match NonZeroU64::new(it.read_u64()?) {
                        None => return decode_error("mkv: invalid chapter uid"),
                        uid => uid,
                    };
                }
                ElementType::ChapterStringUid => {}
                ElementType::ChapterTimeStart => {
                    time_start = Some(it.read_u64()?);
                }
                ElementType::ChapterTimeEnd => {
                    time_end = Some(it.read_u64()?);
                }
                ElementType::ChapterFlagEnabled => {
                    is_enabled = it.read_u64()? == 1;
                }
                ElementType::ChapterFlagHidden => {
                    is_hidden = it.read_u64()? == 1;
                }
                ElementType::ChapterDisplay => {
                    display.push(it.read_element_data::<ChapterDisplayElement>()?);
                }
                ElementType::ChapterSkipType => {
                    skip_type = match it.read_u64()? {
                        value @ 0..=6 => Some(value as u8),
                        _ => None,
                    };
                }
                ElementType::ChapterAtom => {
                    chapters.push(it.read_element_data::<ChapterAtomElement>()?);
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self {
            uid: uid.ok_or(Error::DecodeError("mkv: missing chapter uid"))?,
            is_enabled,
            is_hidden,
            time_start: time_start
                .ok_or(Error::DecodeError("mkv: missing chapter atom time start"))?,
            time_end,
            skip_type,
            display: display.into_boxed_slice(),
            chapters: chapters.into_boxed_slice(),
        })
    }
}

impl ChapterAtomElement {
    pub(crate) fn get_target_uids(&self, target_tags: &mut TargetTagsMap) {
        // Append chapter UID.
        target_tags.insert(TargetUid::Chapter(self.uid.get()), Default::default());
        // Append chapter UIDs.
        self.chapters.iter().for_each(|chapter| chapter.get_target_uids(target_tags));
    }

    pub(crate) fn into_chapter_group_item(
        self,
        target_tags: &mut TargetTagsMap,
    ) -> ChapterGroupItem {
        // Take the vector of any tags associated with this chapter, if any.
        let mut tags = target_tags
            .remove(&TargetUid::Chapter(self.uid.get()))
            .unwrap_or_else(|| Vec::with_capacity(self.display.len()));

        // Chapter title tags.
        for display in self.display {
            let mut sub_fields = Vec::with_capacity(if display.country.is_some() { 2 } else { 1 });

            // Chapter language sub-field.
            if let Some(lang) = display.lang_bcp47 {
                // BCP47 language code is present, prefer it over the ISO 639-2 chapter language
                // and county elements.
                sub_fields.push(RawTagSubField::new(CHAPTER_TITLE_LANGUAGE_BCP47, lang));
            }
            else {
                // ISO 639-2 language code.
                sub_fields.push(RawTagSubField::new(CHAPTER_TITLE_LANGUAGE, display.lang));

                // Chapter country sub-field.
                if let Some(country) = display.country {
                    sub_fields.push(RawTagSubField::new(CHAPTER_TITLE_COUNTRY, country));
                }
            }

            let title = Arc::new(display.name);

            let raw = RawTag::new_with_sub_fields(
                "ChapString",
                title.clone(),
                sub_fields.into_boxed_slice(),
            );

            tags.push(Tag::new_std(raw, StandardTag::ChapterTitle(title)));
        }

        // Chapter skip-type tag.
        if let Some(skip_type) = self.skip_type {
            tags.push(Tag::new_from_parts("CHAPTER_SKIP_TYPE", skip_type, None));
        }

        let chapter = Chapter {
            start_time: Time::from_ns(self.time_start),
            end_time: self.time_end.map(Time::from_ns),
            start_byte: None,
            end_byte: None,
            tags,
            visuals: vec![],
        };

        if self.chapters.is_empty() {
            // This chapter atom does not have nested chapters. Return a chapter item.
            ChapterGroupItem::Chapter(chapter)
        }
        else {
            // This chapter atom has nested chapters. Return a group containing 1 chapter
            // (this one), and a group of nested chapters.
            let mut items = Vec::with_capacity(self.chapters.len());

            for chapter in self.chapters {
                if !chapter.is_hidden {
                    items.push(chapter.into_chapter_group_item(target_tags));
                }
            }

            ChapterGroupItem::Group(
                // The parent chapter group.
                ChapterGroup {
                    items: vec![
                        // The parent chapter as the first item in the chapter group.
                        ChapterGroupItem::Chapter(chapter),
                        // The nested chapters as a chapter group as the second item.
                        ChapterGroupItem::Group(ChapterGroup {
                            items,
                            tags: vec![],
                            visuals: vec![],
                        }),
                    ],
                    tags: vec![],
                    visuals: vec![],
                },
            )
        }
    }
}

#[derive(Debug)]
pub(crate) struct ChapterDisplayElement {
    pub(crate) name: String,
    pub(crate) lang: String,
    pub(crate) lang_bcp47: Option<String>,
    pub(crate) country: Option<String>,
}

impl Element for ChapterDisplayElement {
    const ID: ElementType = ElementType::ChapterDisplay;

    fn read<R: ElementReader>(mut it: ElementIterator<R>, parent: ElementHeader) -> Result<Self> {
        let mut name = None;
        let mut lang = None;
        let mut lang_bcp47 = None;
        let mut country = None;

        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::ChapString => {
                    name = Some(it.read_string()?);
                }
                ElementType::ChapLanguage => {
                    lang = Some(it.read_string()?);
                }
                ElementType::ChapLanguageBcp47 => {
                    lang_bcp47 = Some(it.read_string()?);
                }
                ElementType::ChapCountry => {
                    country = Some(it.read_string()?);
                }
                other => {
                    log::debug!("ignored {:?} child element {:?}", parent.etype, other);
                }
            }
        }

        Ok(Self {
            name: name.ok_or(Error::DecodeError("mkv: missing chapter display name"))?,
            lang: lang.ok_or(Error::DecodeError("mkv: missing chapter display language"))?,
            lang_bcp47,
            country,
        })
    }
}
