use std::collections::VecDeque;
use std::io::{Seek, SeekFrom};

use symphonia_core::audio::Layout;
use symphonia_core::codecs::{CODEC_TYPE_FLAC, CODEC_TYPE_VORBIS, CodecParameters};
use symphonia_core::errors::{Error, Result, unsupported_error};
use symphonia_core::formats::{Cue, FormatOptions, FormatReader, Packet, SeekedTo, SeekMode, SeekTo, Track};
use symphonia_core::io::{BufReader, MediaSourceStream, ReadBytes};
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::probe::{Descriptor, QueryDescriptor};
use symphonia_core::probe::Instantiate;
use symphonia_core::sample::SampleFormat;
use symphonia_core::support_format;
use symphonia_core::units::TimeBase;
use symphonia_utils_xiph::flac::metadata::{MetadataBlockHeader, MetadataBlockType};

use crate::codecs::codec_id_to_type;
use crate::ebml::{EbmlElement, Element, ElementIterator};
use crate::element_ids::ElementType;
use crate::lacing::{extract_frames, Frame, read_xiph_sizes};
use crate::segment::{BlockGroupElement, SegmentElement};

pub struct TrackState {
    /// Codec parameters.
    codec_params: CodecParameters,
    /// The track number.
    track_num: u32,
}

pub struct MkvReader {
    /// Iterator over EBML element headers
    iter: ElementIterator<MediaSourceStream>,
    tracks: Vec<Track>,
    track_states: Vec<TrackState>,
    current_cluster: Option<ClusterState>,
    metadata: MetadataLog,
    cues: Vec<Cue>,
    frames: VecDeque<Frame>,
    timestamp_scale: u64,
}

struct ClusterState {
    timestamp: Option<u64>,
    end: u64,
}


fn convert_vorbis_data(extra: &[u8]) -> Result<Box<[u8]>> {
    const VORBIS_PACKET_TYPE_IDENTIFICATION: u8 = 1;
    const VORBIS_PACKET_TYPE_SETUP: u8 = 5;

    // Private Data for this codec has the following layout:
    // - 1 byte that represents number of packets minus one;
    // - Xiph coded lengths of packets, length of the last packet must be deduced (as in Xiph lacing)
    // - packets in order:
    //    - The Vorbis identification header
    //    - Vorbis comment header
    //    - codec setup header

    let mut reader = BufReader::new(&extra);
    let packet_count = reader.read_byte()? as usize;
    let packet_lengths = read_xiph_sizes(&mut reader, packet_count)?;

    let mut packets = Vec::new();
    for length in packet_lengths {
        packets.push(reader.read_boxed_slice_exact(length as usize)?);
    }

    let last_packet_length = extra.len() - reader.pos() as usize;
    packets.push(reader.read_boxed_slice_exact(last_packet_length)?);

    let mut ident_header = None;
    let mut setup_header = None;

    for packet in packets {
        match packet[0] {
            VORBIS_PACKET_TYPE_IDENTIFICATION => {
                ident_header = Some(packet);
            }
            VORBIS_PACKET_TYPE_SETUP => {
                setup_header = Some(packet);
            }
            other => {
                log::debug!("unsupported vorbis packet type");
            }
        }
    }

    // This is layout expected currently by Vorbis codec.
    Ok([
        ident_header.ok_or_else(|| Error::DecodeError("mkv: missing vorbis identification packet"))?,
        setup_header.ok_or_else(|| Error::DecodeError("mkv: missing vorbis setup packet"))?,
    ].concat().into_boxed_slice())
}

fn get_stream_info_from_codec_private(codec_private: &[u8]) -> Result<Box<[u8]>> {
    let mut reader = BufReader::new(codec_private);

    let marker = reader.read_quad_bytes()?;
    if marker != *b"fLaC" {
        return unsupported_error("mkv (flac): missing flac stream marker");
    }

    let header = MetadataBlockHeader::read(&mut reader)?;

    loop {
        match header.block_type {
            MetadataBlockType::StreamInfo => {
                break Ok(reader.read_boxed_slice_exact(header.block_len as usize)?);
            }
            _ => reader.ignore_bytes(u64::from(header.block_len))?,
        }
    }
}


impl FormatReader for MkvReader {
    fn try_new(mut reader: MediaSourceStream, options: &FormatOptions) -> Result<Self>
        where
            Self: Sized
    {
        let mut it = ElementIterator::new(reader);
        let ebml = it.read_element::<EbmlElement>()?;
        log::warn!("ebml header: {:#?}", ebml.header);

        if !matches!(ebml.header.doc_type.as_str(), "matroska" | "webm") {
            return unsupported_error("mkv: not a matroska / webm file");
        }

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
        reader.seek(SeekFrom::Start(segment.first_cluster_pos))?;
        let it = ElementIterator::new_at(reader, segment.first_cluster_pos);

        let time_base = TimeBase::new(1000, segment.timestamp_scale as u32);

        let mut tracks = Vec::new();
        let mut states = Vec::new();
        for track in segment.tracks.into_vec() {
            let codec_type = codec_id_to_type(&track);

            let mut codec_params = CodecParameters::new();
            codec_params.with_time_base(time_base);

            if let Some(duration) = segment.duration {
                codec_params.with_n_frames(duration);
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

                let layout = match audio.channels {
                    1 => Some(Layout::Mono),
                    2 => Some(Layout::Stereo),
                    3 => Some(Layout::TwoPointOne),
                    6 => Some(Layout::FivePointOne),
                    other => {
                        log::warn!("track #{} has custom number of channels: {}", track.id, other);
                        None
                    }
                };

                if let Some(layout) = layout {
                    codec_params.with_channel_layout(layout);
                }

                if let Some(codec_type) = codec_type {
                    codec_params.for_codec(codec_type);
                    if let Some(codec_private) = track.codec_private {
                        let extra_data = match codec_type {
                            CODEC_TYPE_VORBIS => convert_vorbis_data(&codec_private)?,
                            CODEC_TYPE_FLAC => get_stream_info_from_codec_private(&codec_private)?,
                            _ => codec_private,
                        };
                        codec_params.with_extra_data(extra_data);
                    }
                }
            }

            tracks.push(Track {
                id: track.id as u32,
                codec_params: codec_params.clone(),
                language: track.language,
            });

            states.push(TrackState {
                codec_params: codec_params,
                track_num: track.id as u32,
            });
        }

        Ok(Self {
            iter: it,
            tracks,
            track_states: states,
            current_cluster: None,
            metadata: MetadataLog::default(),
            cues: Vec::new(),
            frames: VecDeque::new(),
            timestamp_scale: segment.timestamp_scale,
        })
    }

    fn cues(&self) -> &[Cue] {
        &self.cues
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn seek(&mut self, mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        todo!()
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> Result<Packet> {
        loop {
            if let Some(frame) = self.frames.pop_front() {
                let timestamp = self.current_cluster.as_ref()
                    .and_then(|c| c.timestamp)
                    .map(|ts| (ts as i64 + frame.timestamp as i64) as u64)
                    .unwrap_or(0);
                return Ok(Packet::new_from_boxed_slice(
                    frame.track as u32, timestamp, 0, frame.data));
            }

            let header = self.iter
                .read_child_header()?
                .ok_or_else(|| Error::DecodeError("mkv: end of stream"))?;

            if let Some(state) = &self.current_cluster {
                if self.iter.pos() >= state.end {
                    log::debug!("ended cluster");
                    self.current_cluster = None;
                }
            }

            match header.etype {
                ElementType::Cluster => {
                    self.current_cluster = Some(ClusterState {
                        timestamp: None,
                        end: header.pos + header.len,
                    });
                }
                ElementType::Timestamp => {
                    match self.current_cluster.as_mut() {
                        Some(cluster) => {
                            cluster.timestamp = Some(self.iter.read_u64()?);
                        }
                        None => {
                            self.iter.ignore_data()?;
                            return decode_error("mkv: timestamp element outside of a cluster");
                        }
                    }
                }
                ElementType::SimpleBlock => {
                    if self.current_cluster.is_none() {
                        self.iter.ignore_data()?;
                        return decode_error("mkv: simple block element outside of a cluster");
                    }

                    let data = self.iter.read_boxed_slice()?;
                    extract_frames(&data, &mut self.frames)?;
                }
                ElementType::BlockGroup => {
                    if self.current_cluster.is_none() {
                        self.iter.ignore_data()?;
                        return decode_error("mkv: block group element outside of a cluster");
                    }

                    let group = self.iter.read_element_data::<BlockGroupElement>()?;
                    extract_frames(&group.data, &mut self.frames)?;
                }
                _ => {
                    self.iter.ignore_data()?;
                }
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