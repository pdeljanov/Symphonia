use std::convert::TryFrom;

use symphonia_core::{
    audio::Layout,
    codecs::CodecType,
    codecs::{self, CodecParameters, CODEC_TYPE_FLAC, CODEC_TYPE_VORBIS},
    errors::{decode_error, Error, Result},
    formats::Track,
    io::{BufReader, ReadBytes},
    sample::SampleFormat,
    units::TimeBase,
};
use symphonia_utils_xiph::flac::metadata::{MetadataBlockHeader, MetadataBlockType};
use webm_iterable::matroska_spec::{Master, MatroskaSpec};

use super::audio::Audio;
use super::super::compression::Compression;

#[derive(Debug)]
pub(crate) struct TrackEntry {
    pub(crate) number: u64,
    pub(crate) language: Option<String>,
    pub(crate) codec_id: String,
    pub(crate) codec_private: Option<Box<[u8]>>,
    pub(crate) audio: Option<Audio>,
    pub(crate) default_duration: Option<u64>,
    pub(crate) compression: Option<(Compression, Box<[u8]>)>,
}

impl TryFrom<Vec<MatroskaSpec>> for TrackEntry {
    type Error = Error;
    fn try_from(tags: Vec<MatroskaSpec>) -> Result<Self> {
        let mut number = None;
        let mut language = None;
        let mut audio = None;
        let mut codec_private = None;
        let mut codec_id = None;
        let mut default_duration = None;
        let mut compression = None;

        for tag in tags {
            match tag {
                MatroskaSpec::TrackNumber(val) => {
                    number = Some(val);
                },
                MatroskaSpec::Language(val) => {
                    language = Some(val);
                },
                MatroskaSpec::CodecID(val) => {
                    codec_id = Some(val);
                },
                MatroskaSpec::CodecPrivate(val) => {
                    codec_private = Some(val.into_boxed_slice());
                },
                MatroskaSpec::Audio(val) => {
                    if let Master::Full(val) = val {
                        audio = Some(Audio::try_from(val)?);
                    }
                },
                MatroskaSpec::DefaultDuration(val) => {
                    default_duration = Some(val);
                },
                MatroskaSpec::ContentEncodings(tag) => {
                    compression = read_compression(tag);
                },
                other => {
                    log::debug!("ignored element {:?}", other);
                }
            }
        }

        Ok(Self {
            number: number.ok_or(Error::DecodeError("mkv: missing track number"))?,
            language,
            codec_id: codec_id.ok_or(Error::DecodeError("mkv: missing codec id"))?,
            codec_private,
            audio,
            default_duration,
            compression,
        })
    }
}

fn read_compression(tag: Master<MatroskaSpec>) -> Option<(Compression, Box<[u8]>)> {
    let mut compression_type = None;
    let mut compression_data = None;

    for tag in tag.get_children() {
        if let MatroskaSpec::ContentEncoding(Master::Full(tags)) = tag {
            for tag in tags {
                if let MatroskaSpec::ContentCompression(Master::Full(tags)) = tag {
                    for tag in tags {
                        if let MatroskaSpec::ContentCompAlgo(id) = tag {
                            match id {
                                0 => { compression_type = Some(Compression::Zlib); },
                                1 => { compression_type = Some(Compression::Bzlib); },
                                2 => { compression_type = Some(Compression::Lzo1x); },
                                3 => { compression_type = Some(Compression::HeaderStripping); },
                                _ => { log::warn!("mkv: unknown compression algorithm {id}"); }
                            }
                        }
                        if let MatroskaSpec::ContentCompSettings(data) = tag {
                            compression_data = Some(data.into_boxed_slice());
                        }
                    }
                }
            }
        }
    }

    match (compression_type, compression_data) {
        (Some(compression_type), Some(compression_data)) => Some((compression_type, compression_data)),
        _ => None
    }

}

impl TrackEntry {
    pub(crate) fn get_codec_type(&self) -> Option<CodecType> {
        let bit_depth = self.audio.as_ref().and_then(|a| a.bit_depth);

        match self.codec_id.as_str() {
            "A_MPEG/L1" => Some(codecs::CODEC_TYPE_MP1),
            "A_MPEG/L2" => Some(codecs::CODEC_TYPE_MP2),
            "A_MPEG/L3" => Some(codecs::CODEC_TYPE_MP3),
            "A_FLAC" => Some(codecs::CODEC_TYPE_FLAC),
            "A_OPUS" => Some(codecs::CODEC_TYPE_OPUS),
            "A_VORBIS" => Some(codecs::CODEC_TYPE_VORBIS),
            "A_AAC/MPEG2/MAIN" | "A_AAC/MPEG2/LC" | "A_AAC/MPEG2/LC/SBR" | "A_AAC/MPEG2/SSR"
            | "A_AAC/MPEG4/MAIN" | "A_AAC/MPEG4/LC" | "A_AAC/MPEG4/LC/SBR" | "A_AAC/MPEG4/SSR"
            | "A_AAC/MPEG4/LTP" | "A_AAC" => Some(codecs::CODEC_TYPE_AAC),
            "A_PCM/INT/BIG" => match bit_depth? {
                16 => Some(codecs::CODEC_TYPE_PCM_S16BE),
                24 => Some(codecs::CODEC_TYPE_PCM_S24BE),
                32 => Some(codecs::CODEC_TYPE_PCM_S32BE),
                _ => None,
            },
            "A_PCM/INT/LIT" => match bit_depth? {
                16 => Some(codecs::CODEC_TYPE_PCM_S16LE),
                24 => Some(codecs::CODEC_TYPE_PCM_S24LE),
                32 => Some(codecs::CODEC_TYPE_PCM_S32LE),
                _ => None,
            },
            "A_PCM/FLOAT/IEEE" => match bit_depth? {
                32 => Some(codecs::CODEC_TYPE_PCM_F32LE),
                64 => Some(codecs::CODEC_TYPE_PCM_F64LE),
                _ => None,
            },
            _ => {
                log::info!("unknown codec: {}", &self.codec_id);
                None
            }
        }
    }

    pub(crate) fn to_core_track(
        &self,
        time_base: TimeBase,
        duration: Option<u64>,
    ) -> Result<Track> {
        let codec_type = self.get_codec_type();

        let mut codec_params = CodecParameters::new();
        codec_params.with_time_base(time_base);

        if let Some(duration) = duration {
            codec_params.with_n_frames(duration);
        }

        if let Some(audio) = &self.audio {
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
                    log::warn!("track #{} has custom number of channels: {}", self.number, other);
                    None
                }
            };

            if let Some(layout) = layout {
                codec_params.with_channel_layout(layout);
            }

            if let Some(codec_type) = codec_type {
                codec_params.for_codec(codec_type);
                if let Some(codec_private) = self.codec_private.clone() {
                    let extra_data = match codec_type {
                        CODEC_TYPE_VORBIS => vorbis_extra_data_from_codec_private(&codec_private)?,
                        CODEC_TYPE_FLAC => flac_extra_data_from_codec_private(&codec_private)?,
                        _ => codec_private,
                    };
                    codec_params.with_extra_data(extra_data);
                }
            }
        }

        let track_id = self.number as u32;
        Ok(Track { id: track_id, codec_params, language: self.language.clone() })
    }
}

fn read_xiph_sizes<R: ReadBytes>(mut reader: R, frames: usize) -> Result<Vec<u64>> {
    let mut prefixes = 0;
    let mut sizes = Vec::new();
    while sizes.len() < frames {
        let byte = reader.read_byte()? as u64;
        if byte == 255 {
            prefixes += 1;
        }
        else {
            let size = prefixes * 255 + byte;
            prefixes = 0;
            sizes.push(size);
        }
    }

    Ok(sizes)
}

fn vorbis_extra_data_from_codec_private(extra: &[u8]) -> Result<Box<[u8]>> {
    const VORBIS_PACKET_TYPE_IDENTIFICATION: u8 = 1;
    const VORBIS_PACKET_TYPE_SETUP: u8 = 5;

    // Private Data for this codec has the following layout:
    // - 1 byte that represents number of packets minus one;
    // - Xiph coded lengths of packets, length of the last packet must be deduced (as in Xiph lacing)
    // - packets in order:
    //    - The Vorbis identification header
    //    - Vorbis comment header
    //    - codec setup header

    let mut reader = BufReader::new(extra);
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
        match packet.first().copied() {
            Some(VORBIS_PACKET_TYPE_IDENTIFICATION) => {
                ident_header = Some(packet);
            }
            Some(VORBIS_PACKET_TYPE_SETUP) => {
                setup_header = Some(packet);
            }
            _ => {
                log::debug!("unsupported vorbis packet type");
            }
        }
    }

    // This is layout expected currently by Vorbis codec.
    Ok([
        ident_header.ok_or(Error::DecodeError("mkv: missing vorbis identification packet"))?,
        setup_header.ok_or(Error::DecodeError("mkv: missing vorbis setup packet"))?,
    ]
    .concat()
    .into_boxed_slice())
}

fn flac_extra_data_from_codec_private(codec_private: &[u8]) -> Result<Box<[u8]>> {
    let mut reader = BufReader::new(codec_private);

    let marker = reader.read_quad_bytes()?;
    if marker != *b"fLaC" {
        return decode_error("mkv (flac): missing flac stream marker");
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
