use symphonia_core::errors::{Error, Result};
use symphonia_core::io::ReadBytes;

use crate::ebml::{Element, ElementData, ElementHeader};
use crate::element_ids::ElementType;

#[derive(Debug)]
pub(crate) struct SegmentElement {
    seek: Option<SeekHeadElement>,
    pub(crate) tracks: Box<[TrackElement]>,
    info: Option<InfoElement>,
    pub(crate) cues: Option<CuesElement>,
    pub(crate) first_cluster_pos: u64,
    pub(crate) duration: Option<u64>,
    pub(crate) timestamp_scale: u64,
}

impl Element for SegmentElement {
    const ID: ElementType = ElementType::Segment;

    fn read<R: ReadBytes>(mut reader: &mut R, header: ElementHeader) -> Result<Self> {
        let mut seek_head = None;
        let mut tracks = None;
        let mut info = None;
        let mut cues = None;
        let mut first_cluster_pos = None;
        let mut duration = None;
        let mut timestamp_scale = None;

        let mut it = header.children(&mut *reader);
        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::SeekHead => {
                    seek_head = Some(it.read_element_data::<SeekHeadElement>()?);
                }
                ElementType::Tracks => {
                    tracks = Some(it.read_element_data::<TracksElement>()?);
                }
                ElementType::Info => {
                    info = Some(it.read_element_data::<InfoElement>()?);
                }
                ElementType::Cues => {
                    cues = Some(it.read_element_data::<CuesElement>()?);
                }
                ElementType::TimestampScale => {
                    timestamp_scale = Some(it.read_u64()?);
                }
                ElementType::Duration => {
                    duration = Some(it.read_u64()?);
                }
                ElementType::Cluster => {
                    first_cluster_pos = Some(reader.pos() - (header.len - header.data_len));
                    break;
                }
                other => {
                    log::warn!("ignored element {:?}", other);
                }
            }
        }

        Ok(Self {
            seek: seek_head,
            tracks: tracks.map(|t| t.tracks).unwrap_or_default(),
            info,
            cues,
            first_cluster_pos: first_cluster_pos.unwrap(),
            timestamp_scale: timestamp_scale.unwrap_or(1_000_000),
            duration: duration,
        })
    }
}

#[derive(Debug)]
pub(crate) struct TrackElement {
    pub(crate) id: u64,
    pub(crate) language: Option<String>,
    pub(crate) codec_id: String,
    pub(crate) codec_private: Option<Box<[u8]>>,
    pub(crate) audio: Option<AudioElement>,
    pub(crate) default_duration: Option<u64>,
}

impl Element for TrackElement {
    const ID: ElementType = ElementType::TrackEntry;

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut track_number = None;
        let mut language = None;
        let mut audio = None;
        let mut codec_private = None;
        let mut codec_id = None;
        let mut default_duration = None;

        let mut it = header.children(reader);
        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::TrackNumber => {
                    track_number = Some(it.read_u64()?);
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
                ElementType::DefaultDuration => {
                    default_duration = Some(it.read_u64()?);
                }
                other => {
                    log::warn!("ignored element {:?}", other);
                }
            }
        }

        Ok(Self {
            id: track_number.ok_or_else(|| Error::DecodeError("mkv: missing track number"))?,
            language,
            codec_id: codec_id.ok_or_else(|| Error::DecodeError("mkv: missing codec id"))?,
            codec_private,
            audio,
            default_duration,
        })
    }
}

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
                    log::warn!("ignored element {:?}", other);
                }
            }
        }

        Ok(Self {
            sampling_frequency: sampling_frequency.unwrap_or(8000.0),
            output_sampling_frequency: output_sampling_frequency,
            channels: channels.unwrap_or(1),
            bit_depth: bit_depth,
        })
    }
}

#[derive(Debug)]
struct SeekHeadElement {
    seeks: Box<[SeekElement]>,
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
                    log::warn!("ignored element {:?}", other);
                }
            }
        }

        Ok(Self { seeks: seeks.into_boxed_slice() })
    }
}


#[derive(Debug)]
struct SeekElement {
    id: u64,
    position: u64,
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
                    log::warn!("ignored element {:?}", other);
                }
            }
        }

        Ok(Self {
            id: seek_id.ok_or_else(|| Error::DecodeError("mkv: missing seek track id"))?,
            position: seek_position.ok_or_else(|| Error::DecodeError("mkv: missing seek track pos"))?,
        })
    }
}

#[derive(Debug)]
struct TracksElement {
    tracks: Box<[TrackElement]>,
}

impl Element for TracksElement {
    const ID: ElementType = ElementType::Tracks;

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut it = header.children(reader);
        Ok(Self {
            tracks: it.read_elements()?,
        })
    }
}

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
                    log::warn!("ignored element {:?}", other);
                }
            }
        }

        Ok(Self {
            version: version.unwrap_or(1),
            read_version: read_version.unwrap_or(1),
            max_id_length: max_id_length.unwrap_or(4),
            max_size_length: max_size_length.unwrap_or(8),
            doc_type: doc_type.ok_or_else(|| Error::Unsupported("mkv: invalid ebml file"))?,
            doc_type_version: doc_type_version.unwrap_or(1),
            doc_type_read_version: doc_type_version.unwrap_or(1),
        })
    }
}

#[derive(Debug)]
struct InfoElement {
    elements: Box<[(ElementHeader, Option<ElementData>)]>,
}

impl Element for InfoElement {
    const ID: ElementType = ElementType::Info;

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut elements = Vec::new();

        let mut it = header.children(reader);
        while let Some(header) = it.read_header()? {
            elements.push((header, it.try_read_data(header)?));
        }

        Ok(Self { elements: elements.into_boxed_slice() })
    }
}

#[derive(Debug)]
pub(crate) struct CuesElement {
    pub(crate) points: Box<[CuePointElement]>,
}

impl Element for CuesElement {
    const ID: ElementType = ElementType::Cues;

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut it = header.children(reader);
        Ok(Self {
            points: it.read_elements()?,
        })
    }
}

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
                ElementType::CueTime => {
                    time = Some(it.read_u64()?)
                }
                ElementType::CueTrackPositions => {
                    pos = Some(it.read_element_data()?);
                }
                other => {
                    log::warn!("ignored element {:?}", other);
                }
            }
        }

        Ok(Self {
            time: time.ok_or_else(|| Error::DecodeError("mkv: missing time in cue"))?,
            positions: pos.ok_or_else(|| Error::DecodeError("mkv: missing positions in cue"))?,
        })
    }
}

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
                    log::warn!("ignored element {:?}", other);
                }
            }
        }
        Ok(Self {
            track: track.ok_or_else(|| Error::DecodeError("mkv: missing track in cue track positions"))?,
            cluster_position: pos.ok_or_else(|| Error::DecodeError("mkv: missing position in cue track positions"))?,
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
                    let nanos = it.read_data()?;
                }
                ElementType::Block => {
                    data = Some(it.read_boxed_slice()?);
                }
                ElementType::BlockDuration => {
                    block_duration = Some(it.read_u64()?);
                }
                other => {
                    log::warn!("ignored element {:?}", other);
                }
            }
        }
        Ok(Self {
            data: data.ok_or_else(|| Error::DecodeError("mkv: missing block inside block group"))?,
            duration: block_duration,
        })
    }
}
