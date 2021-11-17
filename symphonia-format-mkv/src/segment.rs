use symphonia_core::codecs::CodecType;
use symphonia_core::errors::{Error, Result};
use symphonia_core::io::ReadBytes;

use crate::{Element, ElementData, ElementHeader, ElementType, read_children};
use crate::codecs::codec_id_to_type;

#[derive(Debug)]
pub(crate) struct SegmentElement {
    seek: Option<SeekHeadElement>,
    pub(crate) tracks: Box<[TrackElement]>,
    info: Option<InfoElement>,
    pub(crate) cues: Option<CuesElement>,
    pub(crate) clusters_offset: u64,
}

impl Element for SegmentElement {
    const ID: ElementType = ElementType::Segment;

    fn read<R: ReadBytes>(mut reader: &mut R, header: ElementHeader) -> Result<Self> {
        let mut seek_head = None;
        let mut tracks = None;
        let mut info = None;
        let mut cues = None;
        let mut clusters_offset = None;

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
                ElementType::Cluster => {
                    clusters_offset = Some(reader.pos() - (header.element_len - header.data_len));
                    break;
                }
                other => {
                    log::warn!("mkv: ignored element {:?}", other);
                }
            }
        }

        Ok(Self {
            seek: seek_head,
            tracks: tracks.map(|t| t.tracks).unwrap_or_default(),
            info,
            cues,
            clusters_offset: clusters_offset.unwrap(),
        })
    }
}

#[derive(Debug)]
pub(crate) struct TrackElement {
    pub(crate) id: u64,
    pub(crate) codec_id: String,
    pub(crate) codec_private: Option<Box<[u8]>>,
    pub(crate) audio: Option<AudioElement>,
}

impl Element for TrackElement {
    const ID: ElementType = ElementType::TrackEntry;

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut codec_private = None;
        let mut track_number = None;
        let mut audio = None;
        let mut codec_id = None;

        let mut it = header.children(reader);
        while let Some(header) = it.read_header()? {
            match header.etype {
                ElementType::TrackNumber => {
                    track_number = Some(it.read_u64()?);
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
                other => {
                    log::warn!("mkv: unexpected element {:?}", other);
                }
            }
        }

        Ok(Self {
            id: track_number.unwrap(),
            codec_id: codec_id.unwrap(),
            codec_private: codec_private,
            audio: audio,
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
                    log::warn!("mkv: unexpected element {:?}", other);
                }
            }
        }

        Ok(Self {
            sampling_frequency: sampling_frequency.unwrap_or(8000.0),
            output_sampling_frequency: output_sampling_frequency,
            channels: channels.unwrap_or(1),
            bit_depth: bit_depth
        })
    }
}

#[derive(Debug)]
struct SeekHeadElement {
    seeks: Box<[SeekElement]>,
}

impl Element for SeekHeadElement {
    const ID: ElementType = ElementType::SeekHead;

    fn read<B: ReadBytes>(source: &mut B, header: ElementHeader) -> Result<Self> {
        let mut it = header.children(source);
        // TODO
        let seeks = it.read_elements()?;
        Ok(Self { seeks })
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
                    log::warn!("mkv: unexpected element {:?}", other);
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

    fn read<B: ReadBytes>(source: &mut B, header: ElementHeader) -> Result<Self> {
        let mut it = header.children(source);
        Ok(Self {
            tracks: it.read_elements()?,
        })
    }
}

#[derive(Debug)]
pub(crate) struct EbmlHeaderElement {
    children: Box<[ElementHeader]>,
}

impl Element for EbmlHeaderElement {
    const ID: ElementType = ElementType::Ebml;
    fn read<B: ReadBytes>(source: &mut B, header: ElementHeader) -> Result<Self> {
        // FIXME
        let children = read_children(source, header)?;
        Ok(Self { children })
    }
}

#[derive(Debug)]
struct InfoElement {
    elements: Box<[ElementHeader]>,
}

impl Element for InfoElement {
    const ID: ElementType = ElementType::Info;

    fn read<B: ReadBytes>(source: &mut B, header: ElementHeader) -> Result<Self> {
        // FIXME
        let elements = read_children(source, header)?;
        Ok(Self { elements })
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
                    assert!(pos.is_none());
                    time = Some(it.read_u64()?)
                }
                ElementType::CueTrackPositions => {
                    assert!(pos.is_none());
                    pos = Some(it.read_element_data()?);
                }
                _ => todo!(),
            }
        }

        Ok(Self {
            time: time.unwrap().into(),
            positions: pos.unwrap(),
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
                _ => todo!(),
            }
        }
        Ok(Self {
            track: track.unwrap(),
            cluster_position: pos.unwrap(),
        })
    }
}

#[derive(Debug)]
pub(crate) struct BlockGroupElement {
    pub(crate) data: Box<[u8]>,
    pub(crate) duration: u64,
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
                _ => todo!("{:?}", header),
            }
        }
        Ok(Self {
            data: data.unwrap(),
            duration: block_duration.unwrap(),
        })
    }
}
