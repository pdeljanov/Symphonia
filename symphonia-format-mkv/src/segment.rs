// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::codecs::video::well_known::extra_data::{
    VIDEO_EXTRA_DATA_ID_DOLBY_VISION_CONFIG, VIDEO_EXTRA_DATA_ID_DOLBY_VISION_EL_HEVC,
};
use symphonia_core::codecs::video::VideoExtraData;
use symphonia_core::errors::{Error, Result};
use symphonia_core::io::{BufReader, ReadBytes};
use symphonia_core::meta::{MetadataBuilder, MetadataRevision, Tag, Value};

use crate::ebml::{read_unsigned_vint, Element, ElementData, ElementHeader};
use crate::element_ids::ElementType;
use crate::lacing::calc_abs_block_timestamp;

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct TrackElement {
    pub(crate) number: u64,
    pub(crate) uid: u64,
    pub(crate) language: Option<String>,
    pub(crate) codec_id: String,
    pub(crate) codec_private: Option<Box<[u8]>>,
    pub(crate) block_addition_mappings: Vec<BlockAdditionMappingElement>,
    pub(crate) audio: Option<AudioElement>,
    pub(crate) video: Option<VideoElement>,
    pub(crate) default_duration: Option<u64>,
}

impl Element for TrackElement {
    const ID: ElementType = ElementType::TrackEntry;

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut number = None;
        let mut uid = None;
        let mut language = None;
        let mut audio = None;
        let mut video = None;
        let mut codec_private = None;
        let mut block_addition_mappings = Vec::new();
        let mut codec_id = None;
        let mut default_duration = None;

        let mut it = header.children(reader);
        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::TrackNumber => {
                    number = Some(it.read_u64()?);
                }
                ElementType::TrackUid => {
                    uid = Some(it.read_u64()?);
                }
                ElementType::Language => {
                    language = Some(it.read_string()?);
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
                other => {
                    log::debug!("ignored element {:?}", other);
                }
            }
        }

        Ok(Self {
            number: number.ok_or(Error::DecodeError("mkv: missing track number"))?,
            uid: uid.ok_or(Error::DecodeError("mkv: missing track UID"))?,
            language,
            codec_id: codec_id.ok_or(Error::DecodeError("mkv: missing codec id"))?,
            codec_private,
            block_addition_mappings,
            audio,
            video,
            default_duration,
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

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut sampling_frequency = None;
        let mut output_sampling_frequency = None;
        let mut channels = None;
        let mut bit_depth = None;

        let mut it = header.children(reader);
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
                    log::debug!("ignored element {:?}", other);
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

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut pixel_width = None;
        let mut pixel_height = None;

        let mut it = header.children(reader);
        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::PixelWidth => {
                    pixel_width = Some(it.read_u64()? as u16);
                }
                ElementType::PixelHeight => {
                    pixel_height = Some(it.read_u64()? as u16);
                }
                other => {
                    log::debug!("ignored element {:?}", other);
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

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        // There can be many BlockAdditionMapping elements with DolbyVisionConfiguration in a single track
        // BlockAddIdType FourCC string allows to determine the type of DolbyVisionConfiguration extra data
        let mut extra_data = None;
        let mut block_add_id_type = String::new();

        let mut it = header.children(reader);
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
                    log::debug!("ignored element {:?}", other);
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

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut seeks = Vec::new();

        let mut it = header.children(reader);
        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::Seek => {
                    seeks.push(it.read_element_data()?);
                }
                other => {
                    log::debug!("ignored element {:?}", other);
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

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut seek_id = None;
        let mut seek_position = None;

        let mut it = header.children(reader);
        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::SeekId => {
                    seek_id = Some(it.read_u64()?);
                }
                ElementType::SeekPosition => {
                    seek_position = Some(it.read_u64()?);
                }
                other => {
                    log::debug!("ignored element {:?}", other);
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

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut it = header.children(reader);
        Ok(Self { tracks: it.read_elements()? })
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

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut version = None;
        let mut read_version = None;
        let mut max_id_length = None;
        let mut max_size_length = None;
        let mut doc_type = None;
        let mut doc_type_version = None;
        let mut doc_type_read_version = None;

        let mut it = header.children(reader);
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
                    log::debug!("ignored element {:?}", other);
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

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut duration = None;
        let mut timestamp_scale = None;
        let mut title = None;
        let mut muxing_app = None;
        let mut writing_app = None;

        let mut it = header.children(reader);
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
                    log::debug!("ignored element {:?}", other);
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

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut it = header.children(reader);
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

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut it = header.children(reader);

        let mut time = None;
        let mut pos = None;
        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::CueTime => time = Some(it.read_u64()?),
                ElementType::CueTrackPositions => {
                    pos = Some(it.read_element_data()?);
                }
                other => {
                    log::debug!("ignored element {:?}", other);
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

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut it = header.children(reader);

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
                    log::debug!("ignored element {:?}", other);
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

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut it = header.children(reader);

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
                    log::debug!("ignored element {:?}", other);
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

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let pos = reader.pos();
        let mut timestamp = None;
        let mut blocks = Vec::new();
        let has_size = header.end().is_some();

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

        let mut it = header.children(reader);
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
                    log::debug!("ignored element {:?}", other);
                }
            }
        }

        Ok(ClusterElement {
            timestamp: get_timestamp(timestamp)?,
            blocks: blocks.into_boxed_slice(),
            pos,
            end: header.end(),
        })
    }
}

#[derive(Debug)]
pub(crate) struct TagsElement {
    pub(crate) tags: Box<[TagElement]>,
}

impl Element for TagsElement {
    const ID: ElementType = ElementType::Tags;

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut tags = Vec::new();

        let mut it = header.children(reader);
        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::Tag => {
                    tags.push(it.read_element_data::<TagElement>()?);
                }
                other => {
                    log::debug!("ignored element {:?}", other);
                }
            }
        }

        Ok(Self { tags: tags.into_boxed_slice() })
    }
}

impl TagsElement {
    pub(crate) fn to_metadata(&self) -> MetadataRevision {
        let mut metadata = MetadataBuilder::new();
        for tag in self.tags.iter() {
            for simple_tag in tag.simple_tags.iter() {
                // TODO: support std_key
                metadata.add_tag(Tag::new(
                    None,
                    &simple_tag.name,
                    match &simple_tag.value {
                        ElementData::Binary(b) => Value::Binary(b.clone()),
                        ElementData::String(s) => Value::String(s.clone()),
                        _ => unreachable!(),
                    },
                ));
            }
        }
        metadata.metadata()
    }
}

#[derive(Debug)]
pub(crate) struct TagElement {
    pub(crate) simple_tags: Box<[SimpleTagElement]>,
}

impl Element for TagElement {
    const ID: ElementType = ElementType::Tag;

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut simple_tags = Vec::new();

        let mut it = header.children(reader);
        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::SimpleTag => {
                    simple_tags.push(it.read_element_data::<SimpleTagElement>()?);
                }
                other => {
                    log::debug!("ignored element {:?}", other);
                }
            }
        }

        Ok(Self { simple_tags: simple_tags.into_boxed_slice() })
    }
}

#[derive(Debug)]
pub(crate) struct SimpleTagElement {
    pub(crate) name: Box<str>,
    pub(crate) value: ElementData,
}

impl Element for SimpleTagElement {
    const ID: ElementType = ElementType::SimpleTag;

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut name = None;
        let mut value = None;

        let mut it = header.children(reader);
        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::TagName => {
                    name = Some(it.read_string()?);
                }
                ElementType::TagString | ElementType::TagBinary => {
                    value = Some(it.read_data()?);
                }
                other => {
                    log::debug!("ignored element {:?}", other);
                }
            }
        }

        Ok(Self {
            name: name.ok_or(Error::DecodeError("mkv: missing tag name"))?.into_boxed_str(),
            value: value.ok_or(Error::DecodeError("mkv: missing tag value"))?,
        })
    }
}
