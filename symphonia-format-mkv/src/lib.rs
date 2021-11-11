use std::io::{Read, Seek, SeekFrom};

use symphonia_core::audio::{Channels, Layout};
use symphonia_core::codecs::{CODEC_TYPE_OPUS, CodecParameters};
use symphonia_core::errors::{Error, Result};
use symphonia_core::formats::{Cue, FormatOptions, FormatReader, Packet, SeekedTo, SeekMode, SeekTo, Track};
use symphonia_core::io::{MediaSourceStream, ReadBytes};
use symphonia_core::meta::{Metadata, Value};
use symphonia_core::probe::{Descriptor, QueryDescriptor};
use symphonia_core::probe::Instantiate;
use symphonia_core::sample::SampleFormat;
use symphonia_core::support_format;

use crate::ebml::{Element, ElementHeader, get_value, ElementIterator, EbmlElement};
use crate::element_ids::ElementType;
use crate::segment::{BlockGroupElement, EbmlHeaderElement, SegmentElement};

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
        if let Some(val) = get_value(&mut source, element)? {
            print!("{}{:?} [{}]", "  ".repeat(level + 1), element.etype, element.data_len);
            match val {
                Value::Binary(value) => {
                    if value.len() < 16 {
                        println!("{:02x?}", &value);
                    } else {
                        println!("{:02x?}", &value[..16]);
                    }
                }
                Value::Boolean(_) | Value::Flag => {}
                Value::Float(value) => {
                    println!("{}", value);
                }
                Value::SignedInt(value) => {
                    println!("{}", value);
                }
                Value::String(value) => {
                    println!("{}", value);
                }
                Value::UnsignedInt(value) => {
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

        let tracks: Vec<_> = segment.tracks.into_vec().into_iter().map(|track| Track {
            id: track.id as u32,
            codec_params: CodecParameters {
                codec: CODEC_TYPE_OPUS,
                sample_rate: track.audio.map(|it| it.sampling_frequency.round() as u32),
                time_base: None,
                n_frames: None,
                start_ts: 0,
                sample_format: Some(SampleFormat::S16),
                bits_per_sample: None,
                bits_per_coded_sample: None,
                channels: Some(Channels::SIDE_LEFT | Channels::SIDE_RIGHT),
                channel_layout: Some(Layout::Stereo),
                leading_padding: None,
                trailing_padding: None,
                max_frames_per_packet: None,
                packet_data_integrity: false,
                verification_check: None,
                extra_data: track.codec_private,
            },
            language: None,
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
                        cluster.timestamp = match self.iter.read_value()? {
                            Value::UnsignedInt(x) => Some(x),
                            other => todo!(),
                        };
                    }
                }
                ElementType::SimpleBlock => {
                    match self.iter.read_value()? {
                        Value::Binary(b) => {
                            return Ok(Packet::new_from_boxed_slice(0, 0, 20, b));
                        }
                        _ => unreachable!(),
                    }
                }
                ElementType::BlockGroup => {
                    let x = self.iter.read_element_data::<BlockGroupElement>()?;
                    return Ok(Packet::new_from_boxed_slice(0, 0, 20, x.data))
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