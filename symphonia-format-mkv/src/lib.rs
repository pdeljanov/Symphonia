use std::io::{Seek, SeekFrom};

use symphonia_core::audio::{Channels, Layout, SampleBuffer};
use symphonia_core::codecs::{CODEC_TYPE_OPUS, CodecParameters};
use symphonia_core::errors::{Error, Result};
use symphonia_core::formats::{Cue, FormatOptions, FormatReader, Packet, SeekedTo, SeekMode, SeekTo, Track};
use symphonia_core::io::{MediaSourceStream, ReadBytes};
use symphonia_core::meta::Metadata;
use symphonia_core::probe::{Descriptor, QueryDescriptor};
use symphonia_core::probe::Instantiate;
use symphonia_core::sample::SampleFormat;
use symphonia_core::support_format;

use crate::codecs::codec_id_to_type;
use crate::ebml::{EbmlElement, Element, ElementData, ElementHeader, ElementIterator, get_data};
use crate::element_ids::ElementType;
use crate::segment::{BlockGroupElement, EbmlHeaderElement, SegmentElement};

mod codecs;
mod element_ids;
mod ebml;
mod segment;

pub struct TrackState {
    /// Codec parameters.
    codec_params: CodecParameters,
    /// The track number.
    track_num: u32,
    /// The current segment.
    cur_seg: usize,
    /// The current sample index relative to the track.
    next_sample: u32,
    /// The current sample byte position relative to the start of the track.
    next_sample_pos: u64,
}

pub struct MkvReader {
    /// Iterator over EBML element headers
    iter: ElementIterator<MediaSourceStream>,
    tracks: Vec<Track>,
    track_states: Vec<TrackState>,
    current_cluster: Option<ClusterState>,
}

fn print_all(mut reader: &mut MediaSourceStream) -> Result<()> {
    let mut it = ElementIterator::new(&mut reader);
    let header = it.read_element_data::<EbmlHeaderElement>()?;
    let hdr = ElementHeader::read(&mut reader)?;

    visit(&mut reader, hdr, 0)?;
    Ok(())
}

fn read_children<B: ReadBytes>(source: &mut B, header: ElementHeader) -> Result<Box<[ElementHeader]>> {
    let mut it = header.children(source);
    Ok(std::iter::from_fn(|| it.read_header().transpose()).collect::<Result<Vec<_>>>()?.into_boxed_slice())
}

fn visit<B: ReadBytes + Seek>(mut source: &mut B, element: ElementHeader, level: usize) -> Result<()> {
    println!("{}{:?} [{}]", "  ".repeat(level), element.etype, element.data_len);
    for element in read_children(&mut source, element)?.into_vec() {
        if let Some(val) = get_data(&mut source, element)? {
            print!("{}{:?} [{}]", "  ".repeat(level + 1), element.etype, element.data_len);
            match val {
                ElementData::Binary(value) => {
                    if value.len() < 16 {
                        println!("{:02x?}", &value);
                    } else {
                        println!("{:02x?}", &value[..16]);
                    }
                }
                ElementData::Boolean(_) => {}
                ElementData::Float(value) => {
                    println!("{}", value);
                }
                ElementData::SignedInt(value) | ElementData::Date(value) => {
                    println!("{}", value);
                }
                ElementData::String(value) => {
                    println!("{}", value);
                }
                ElementData::UnsignedInt(value) => {
                    println!("{}", value);
                }
            }
        } else {
            visit(source, element, level + 1)?;
        }
    }
    Ok(())
}

struct ClusterState {
    timestamp: Option<u64>,
}

impl FormatReader for MkvReader {
    fn try_new(mut reader: MediaSourceStream, options: &FormatOptions) -> Result<Self>
        where
            Self: Sized
    {
        let mut it = ElementIterator::new(reader);
        let header = it.read_element::<EbmlElement>()?;
        let segment = it.read_element::<SegmentElement>()?;
        reader = it.into_inner();
        reader.seek(SeekFrom::Start(segment.clusters_offset))?;
        let it = ElementIterator::new_at(reader, segment.clusters_offset);

        let tracks: Vec<_> = segment.tracks.into_vec().into_iter().map(|track| {
            let mut codec_params = CodecParameters::new();
            if let Some(codec_type) = codec_id_to_type(&track) {
                codec_params.for_codec(codec_type);
            }

            if let Some(audio) = track.audio {
                codec_params.with_sample_rate(audio.sampling_frequency.round() as u32);
                let format = audio.bit_depth.and_then(|bits| match bits {
                    8 => Some(SampleFormat::S8),
                    16 => Some(SampleFormat::S16),
                    24 => Some(SampleFormat::S24),
                    32 => Some(SampleFormat::S32),
                    _ => None,
                });
                if let Some(format) = format {
                    codec_params.with_sample_format(format);
                }
                if let Some(bits) = audio.bit_depth {
                    codec_params.with_bits_per_sample(bits as u32);
                }
                codec_params.with_channel_layout(match audio.channels {
                    1 => Layout::Mono,
                    2 => Layout::Stereo,
                    _ => unimplemented!(),
                });
                if let Some(data) = track.codec_private {
                    codec_params.with_extra_data(data);
                }
            }

            Track {
                id: track.id as u32,
                codec_params,
                language: None,
            }
        }).collect();

        let track_states = tracks.iter().map(|track| TrackState {
            codec_params: track.codec_params.clone(),
            track_num: 0,
            cur_seg: 0,
            next_sample: 0,
            next_sample_pos: 0,
        }).collect();

        Ok(Self {
            iter: it,
            tracks,
            track_states,
            current_cluster: None,
        })
    }

    fn cues(&self) -> &[Cue] {
        todo!()
    }

    fn metadata(&mut self) -> Metadata<'_> {
        todo!()
    }

    fn seek(&mut self, mode: SeekMode, to: SeekTo) -> symphonia_core::errors::Result<SeekedTo> {
        todo!()
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> Result<Packet> {
        loop {
            let header = self.iter
                .read_child_header()?
                .ok_or_else(|| Error::DecodeError("mkv: invalid header"))?;

            match header.etype {
                ElementType::Cluster => {
                    self.current_cluster = Some(ClusterState {
                        timestamp: None,
                    });
                }
                ElementType::Timestamp => {
                    if let Some(cluster) = &mut self.current_cluster {
                        cluster.timestamp = self.iter.read_data()?.to_u64();
                    }
                }
                ElementType::SimpleBlock => {
                    match self.iter.read_data()? {
                        ElementData::Binary(b) => {
                            return Ok(Packet::new_from_boxed_slice(0, 0, 20, b));
                        }
                        _ => unreachable!(),
                    }
                }
                ElementType::BlockGroup => {
                    let x = self.iter.read_element_data::<BlockGroupElement>()?;
                    return Ok(Packet::new_from_boxed_slice(0, 0, 20, x.data));
                }
                _ => todo!("{:?}", header),
            }
        }
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.iter.into_inner()
    }
}

impl QueryDescriptor for MkvReader {
    fn query() -> &'static [Descriptor] {
        &[
            support_format!(
                "matroska",
                "Matroska / WebM",
                &[ "webm", "mkv" ],
                &[ "video/webm", "video/x-matroska" ],
                &[ b"\x1A\x45\xDF\xA3" ] // Top-level element Ebml element
            ),
        ]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}