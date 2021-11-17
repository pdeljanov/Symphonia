use std::collections::VecDeque;
use std::io::{Seek, SeekFrom};

use symphonia_core::audio::{Channels, Layout, SampleBuffer};
use symphonia_core::codecs::{CODEC_TYPE_OPUS, CodecParameters};
use symphonia_core::errors::{Error, Result};
use symphonia_core::formats::{Cue, CuePoint, FormatOptions, FormatReader, Packet, SeekedTo, SeekMode, SeekTo, Track};
use symphonia_core::io::{BufReader, MediaSourceStream, ReadBytes};
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::probe::{Descriptor, QueryDescriptor};
use symphonia_core::probe::Instantiate;
use symphonia_core::sample::SampleFormat;
use symphonia_core::support_format;

use crate::codecs::codec_id_to_type;
use crate::ebml::{EbmlElement, Element, ElementData, ElementHeader, ElementIterator, get_data, read_vint, read_vint_signed};
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
    metadata: MetadataLog,
    cues: Vec<Cue>,
    frames: VecDeque<(u32, Box<[u8]>)>,
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

enum Lacing {
    None,
    Xiph,
    FixedSize,
    Ebml,
}

fn parse_flags(flags: u8) -> Result<Lacing> {
    match (flags >> 1) & 0b11 {
        0b00 => Ok(Lacing::None),
        0b01 => Ok(Lacing::Xiph),
        0b10 => Ok(Lacing::FixedSize),
        0b11 => Ok(Lacing::Ebml),
        _ => unreachable!(),
    }
}

fn read_ebml_sizes<R: ReadBytes>(mut reader: R, frames: usize) -> Result<Vec<u64>> {
    let mut sizes = Vec::new();
    for _ in 0..frames {
        if let Some(last_size) = sizes.last().copied() {
            let delta = read_vint_signed(&mut reader)?;
            sizes.push((last_size as i64 + delta) as u64)
        } else {
            let size = read_vint::<_, true>(&mut reader)?;
            sizes.push(size);
        }
    }

    Ok(sizes)
}

fn read_xiph_sizes<R: ReadBytes>(mut reader: R, frames: usize) -> Result<Vec<u64>> {
    let mut prefixes = 0;
    let mut sizes = Vec::new();
    while sizes.len() < frames as usize + 1 {
        let byte = reader.read_byte()? as u64;
        if byte == 255 {
            prefixes += 1;
        } else {
            let size = prefixes * 255 + byte;
            prefixes = 0;
            sizes.push(size);
        }
    }

    Ok(sizes)
}

fn extract_frames(block: &[u8], buffer: &mut VecDeque<(u32, Box<[u8]>)>) -> Result<()> {
    let mut reader = BufReader::new(&block);
    let track = read_vint::<_, true>(&mut reader)? as u32;
    let timestamp = reader.read_be_u16()? as i16;
    let flags = reader.read_byte()?;
    let lacing = parse_flags(flags)?;
    match lacing {
        Lacing::None => {
            let frame = reader.read_boxed_slice_exact(block.len() - reader.pos() as usize)?;
            buffer.push_back((track, frame));
        }
        Lacing::Xiph | Lacing::Ebml => {
            // Read number of stored sizes which is actually `number of frames` - 1
            // since size of the last frame is deduced from block size.
            let frames = reader.read_byte()? as usize;
            let sizes = match lacing {
                Lacing::Xiph => read_xiph_sizes(&mut reader, frames)?,
                Lacing::Ebml => read_ebml_sizes(&mut reader, frames)?,
                _ => unreachable!(),
            };

            for frame_size in sizes {
                buffer.push_back((track, reader.read_boxed_slice_exact(frame_size as usize)?));
            }

            // Size of last frame is not provided so we read to the end of the block.
            let size = block.len() - reader.pos() as usize;
            buffer.push_back((track, reader.read_boxed_slice_exact(size)?));
        }
        Lacing::FixedSize => {
            let frames = reader.read_byte()? as usize;
            let total_size = block.len() - reader.pos() as usize;
            if total_size % frames != 0 {
                return Err(Error::DecodeError("mkv: invalid block size"));
            }

            let frame_size = total_size / frames;
            for _ in 0..frames {
                buffer.push_back((track, reader.read_boxed_slice_exact(frame_size)?));
            }
        }
        _ => unreachable!(),
    }

    Ok(())
}

impl FormatReader for MkvReader {
    fn try_new(mut reader: MediaSourceStream, options: &FormatOptions) -> Result<Self>
        where
            Self: Sized
    {
        let mut it = ElementIterator::new(reader);
        let header = it.read_element::<EbmlElement>()?;

        let segment = loop {
            if let Some(header) = it.read_header()? {
                match header.etype {
                    ElementType::Segment => break it.read_element_data::<SegmentElement>()?,
                    ElementType::Crc32 => {
                        // TODO: ignore crc for now
                        continue;
                    }
                    _ => todo!(),
                }
            } else {
                todo!();
            }
        };

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
            metadata: MetadataLog::default(),
            cues: Vec::new(),
            frames: VecDeque::new(),
        })
    }

    fn cues(&self) -> &[Cue] {
        &self.cues
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn seek(&mut self, mode: SeekMode, to: SeekTo) -> symphonia_core::errors::Result<SeekedTo> {
        todo!()
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> Result<Packet> {
        loop {
            if let Some((track, frame)) = self.frames.pop_front() {
                return Ok(Packet::new_from_boxed_slice(track as u32, 0, 0, frame));
            }

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
                        cluster.timestamp = Some(self.iter.read_u64()?);
                    }
                }
                ElementType::SimpleBlock => {
                    let data = self.iter.read_boxed_slice()?;
                    extract_frames(&data, &mut self.frames)?;
                }
                ElementType::BlockGroup => {
                    let group = self.iter.read_element_data::<BlockGroupElement>()?;
                    extract_frames(&group.data, &mut self.frames)?;
                }
                ElementType::Void => continue,
                _ => log::warn!("mkv: unsupported element: {:?}", header),
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