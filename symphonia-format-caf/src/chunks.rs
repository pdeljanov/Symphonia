use log::{debug, error, info};
use std::{mem::size_of, str};
use symphonia_core::{
    audio::{Channels, Layout},
    codecs::*,
    errors::{decode_error, unsupported_error, Result},
    io::{MediaSourceStream, ReadBytes},
};

#[derive(Debug)]
pub enum Chunk {
    AudioDescription(AudioDescription),
    AudioData(AudioData),
    ChannelLayout(ChannelLayout),
    Free,
}

impl Chunk {
    /// Reads a chunk
    ///
    /// After calling this function the reader's position will be:
    ///   - at the start of the next chunk,
    ///   - or, at the end of the file,
    ///   - or, if the chunk is the audio data chunk and the size is unknown,
    ///     then at the start of the audio data.
    pub fn read(mut reader: &mut MediaSourceStream) -> Result<Option<Self>> {
        let chunk_type = reader.read_quad_bytes()?;
        let chunk_size = reader.read_be_i64()?;

        let result = match &chunk_type {
            b"desc" => Chunk::AudioDescription(AudioDescription::read(&mut reader)?),
            b"data" => Chunk::AudioData(AudioData::read(&mut reader, chunk_size)?),
            b"chan" => Chunk::ChannelLayout(ChannelLayout::read(&mut reader)?),
            b"free" => {
                if chunk_size < 0 {
                    error!("invalid Free chunk size ({chunk_size})");
                    return decode_error("invalid Free chunk size");
                }
                reader.ignore_bytes(chunk_size as u64)?;
                Chunk::Free
            }
            other => {
                // Log unsupported chunk types but don't return an error
                info!(
                    "unsupported chunk type ('{}')",
                    str::from_utf8(other.as_slice()).unwrap_or("????")
                );
                return Ok(None);
            }
        };

        debug!("chunk: {result:?} - size: {chunk_size}");
        Ok(Some(result))
    }
}

#[derive(Debug)]
pub struct AudioDescription {
    pub sample_rate: f64,
    pub format_id: AudioDescriptionFormatId,
    pub bytes_per_packet: u32,
    pub frames_per_packet: u32,
    pub channels_per_frame: u32,
    pub bits_per_channel: u32,
}

impl AudioDescription {
    pub fn read(reader: &mut MediaSourceStream) -> Result<Self> {
        let sample_rate = reader.read_be_f64()?;
        let format_id = AudioDescriptionFormatId::read(reader)?;

        Ok(Self {
            sample_rate,
            format_id,
            bytes_per_packet: reader.read_be_u32()?,
            frames_per_packet: reader.read_be_u32()?,
            channels_per_frame: reader.read_be_u32()?,
            bits_per_channel: reader.read_be_u32()?,
        })
    }

    pub fn codec_type(&self) -> Result<CodecType> {
        use AudioDescriptionFormatId::*;

        let result = match &self.format_id {
            LinearPCM { floating_point, little_endian } => {
                if *floating_point {
                    match (self.bits_per_channel, *little_endian) {
                        (32, true) => CODEC_TYPE_PCM_F32LE,
                        (32, false) => CODEC_TYPE_PCM_F32BE,
                        (64, true) => CODEC_TYPE_PCM_F64LE,
                        (64, false) => CODEC_TYPE_PCM_F64BE,
                        (bits, _) => {
                            error!("unsupported PCM floating point format (bits: {bits})");
                            return unsupported_error("unsupported bits per channel");
                        }
                    }
                } else {
                    match (self.bits_per_channel, *little_endian) {
                        (16, true) => CODEC_TYPE_PCM_S16LE,
                        (16, false) => CODEC_TYPE_PCM_S16BE,
                        (24, true) => CODEC_TYPE_PCM_S24LE,
                        (24, false) => CODEC_TYPE_PCM_S24BE,
                        (32, true) => CODEC_TYPE_PCM_S32LE,
                        (32, false) => CODEC_TYPE_PCM_S32BE,
                        (bits, _) => {
                            error!("unsupported PCM integer format (bits: {bits})");
                            return unsupported_error("unsupported bits per channel");
                        }
                    }
                }
            }
            AppleIMA4 => CODEC_TYPE_ADPCM_IMA_WAV,
            MPEG4AAC => CODEC_TYPE_AAC,
            ULaw => CODEC_TYPE_PCM_MULAW,
            ALaw => CODEC_TYPE_PCM_ALAW,
            MPEGLayer1 => CODEC_TYPE_MP1,
            MPEGLayer2 => CODEC_TYPE_MP2,
            MPEGLayer3 => CODEC_TYPE_MP3,
            AppleLossless => CODEC_TYPE_ALAC,
            unsupported => {
                error!("unsupported codec ({unsupported:?})");
                return unsupported_error("unsupported codec");
            }
        };

        Ok(result)
    }

    pub fn format_is_compressed(&self) -> bool {
        !matches!(self.format_id, AudioDescriptionFormatId::LinearPCM { .. })
    }
}

#[derive(Debug)]
pub struct AudioData {
    pub _edit_count: u32,
    pub start_pos: u64,
    pub data_len: Option<u64>,
}

impl AudioData {
    pub fn read(reader: &mut MediaSourceStream, chunk_size: i64) -> Result<Self> {
        let edit_count = reader.read_be_u32()?;
        let edit_count_offset = size_of::<u32>() as u64;
        let start_pos = reader.pos();

        if chunk_size == -1 {
            return Ok(Self { _edit_count: edit_count, start_pos, data_len: None });
        }

        let chunk_size = chunk_size as u64;
        if chunk_size < edit_count_offset {
            error!("invalid audio data chunk size ({})", chunk_size);
            decode_error("invalid audio data chunk size")
        } else {
            let data_len = chunk_size - edit_count_offset;
            debug!("data_len: {data_len}");
            reader.ignore_bytes(data_len)?;
            Ok(Self { _edit_count: edit_count, start_pos, data_len: Some(data_len) })
        }
    }
}

#[derive(Debug)]
pub enum AudioDescriptionFormatId {
    LinearPCM { floating_point: bool, little_endian: bool },
    AppleIMA4,
    MPEG4AAC,
    MACE3,
    MACE6,
    ULaw,
    ALaw,
    MPEGLayer1,
    MPEGLayer2,
    MPEGLayer3,
    AppleLossless,
}

impl AudioDescriptionFormatId {
    pub fn read(reader: &mut MediaSourceStream) -> Result<Self> {
        use AudioDescriptionFormatId::*;

        let format_id = reader.read_quad_bytes()?;
        let format_flags = reader.read_be_u32()?;

        let result = match &format_id {
            b"lpcm" => {
                let floating_point = format_flags & (1 << 0) != 0;
                let little_endian = format_flags & (1 << 1) != 0;
                return Ok(LinearPCM { floating_point, little_endian });
            }
            b"ima4" => AppleIMA4,
            b"aac " => {
                if format_flags != 2 {
                    error!("unsupported AAC object type ({format_flags})");
                    return unsupported_error("unsupported AAC object type");
                }
                return Ok(MPEG4AAC);
            }
            b"MAC3" => MACE3,
            b"MAC6" => MACE6,
            b"ulaw" => ULaw,
            b"alaw" => ALaw,
            b".mp1" => MPEGLayer1,
            b".mp2" => MPEGLayer2,
            b".mp3" => MPEGLayer3,
            b"alac" => AppleLossless,
            other => {
                error!("unsupported format id ({other:?})");
                return unsupported_error("unsupported format id");
            }
        };

        if format_flags == 0 {
            Ok(result)
        } else {
            error!("format flags should be zero ({format_flags})");
            decode_error("non-zero format flags")
        }
    }
}

#[derive(Debug)]
pub struct ChannelLayout {
    pub channel_layout: u32,
    pub channel_bitmap: u32,
    pub channel_descriptions: Vec<ChannelDescription>,
}

impl ChannelLayout {
    pub fn read(reader: &mut MediaSourceStream) -> Result<Self> {
        let channel_layout = reader.read_be_u32()?;
        let channel_bitmap = reader.read_be_u32()?;
        let channel_description_count = reader.read_be_u32()?;
        let channel_descriptions: Vec<ChannelDescription> = (0..channel_description_count)
            .map(|_| ChannelDescription::read(reader))
            .collect::<Result<_>>()?;

        Ok(Self { channel_layout, channel_bitmap, channel_descriptions })
    }

    pub fn channels(&self) -> Option<Channels> {
        let channels = match self.channel_layout {
            // Use channel descriptions
            0 => {
                let mut channels: u32 = 0;
                for channel in self.channel_descriptions.iter() {
                    match channel.channel_label {
                        1 => channels |= Channels::FRONT_LEFT.bits(),
                        2 => channels |= Channels::FRONT_RIGHT.bits(),
                        3 => channels |= Channels::FRONT_CENTRE.bits(),
                        4 => channels |= Channels::LFE1.bits(),
                        5 => channels |= Channels::REAR_LEFT.bits(),
                        6 => channels |= Channels::REAR_RIGHT.bits(),
                        7 => channels |= Channels::FRONT_LEFT_CENTRE.bits(),
                        8 => channels |= Channels::FRONT_RIGHT_CENTRE.bits(),
                        9 => channels |= Channels::REAR_CENTRE.bits(),
                        10 => channels |= Channels::SIDE_LEFT.bits(),
                        11 => channels |= Channels::SIDE_RIGHT.bits(),
                        12 => channels |= Channels::TOP_CENTRE.bits(),
                        13 => channels |= Channels::TOP_FRONT_LEFT.bits(),
                        14 => channels |= Channels::TOP_FRONT_CENTRE.bits(),
                        15 => channels |= Channels::TOP_FRONT_RIGHT.bits(),
                        16 => channels |= Channels::TOP_REAR_LEFT.bits(),
                        17 => channels |= Channels::TOP_REAR_CENTRE.bits(),
                        18 => channels |= Channels::TOP_REAR_RIGHT.bits(),
                        unsupported => {
                            info!("Unsupported channel label: {unsupported}");
                            return None;
                        }
                    }
                }
                return Channels::from_bits(channels);
            }
            // Use the channel bitmap
            1 => return Channels::from_bits(self.channel_bitmap),
            // Layout tags which have channel roles that match the standard channel layout
            LAYOUT_TAG_MONO => Layout::Mono.into_channels(),
            LAYOUT_TAG_STEREO | LAYOUT_TAG_STEREO_HEADPHONES => Layout::Stereo.into_channels(),
            LAYOUT_TAG_MPEG_3_0_A => {
                Channels::FRONT_LEFT | Channels::FRONT_RIGHT | Channels::FRONT_CENTRE
            }
            LAYOUT_TAG_MPEG_5_1_A => Layout::FivePointOne.into_channels(),
            LAYOUT_TAG_MPEG_7_1_A => {
                Channels::FRONT_LEFT
                    | Channels::FRONT_RIGHT
                    | Channels::FRONT_CENTRE
                    | Channels::LFE1
                    | Channels::REAR_LEFT
                    | Channels::REAR_RIGHT
                    | Channels::FRONT_LEFT_CENTRE
                    | Channels::FRONT_RIGHT_CENTRE
            }
            LAYOUT_TAG_DVD_10 => {
                Channels::FRONT_LEFT
                    | Channels::FRONT_RIGHT
                    | Channels::FRONT_CENTRE
                    | Channels::LFE1
            }
            unsupported => {
                debug!("Unsupported channel layout: {unsupported}");
                return None;
            }
        };

        Some(channels)
    }
}

#[derive(Debug)]
pub struct ChannelDescription {
    pub channel_label: u32,
    pub channel_flags: u32,
    pub coordinates: [f32; 3],
}

impl ChannelDescription {
    pub fn read(reader: &mut MediaSourceStream) -> Result<Self> {
        Ok(Self {
            channel_label: reader.read_be_u32()?,
            channel_flags: reader.read_be_u32()?,
            coordinates: [reader.read_be_f32()?, reader.read_be_f32()?, reader.read_be_f32()?],
        })
    }
}

// Layout tags from the CAF spec that match the first N channels of a standard layout
const LAYOUT_TAG_MONO: u32 = (100 << 16) | 1;
const LAYOUT_TAG_STEREO: u32 = (101 << 16) | 2;
const LAYOUT_TAG_STEREO_HEADPHONES: u32 = (102 << 16) | 2;
const LAYOUT_TAG_MPEG_3_0_A: u32 = (113 << 16) | 3; // L R C
const LAYOUT_TAG_MPEG_5_1_A: u32 = (121 << 16) | 6; // L R C LFE Ls Rs
const LAYOUT_TAG_MPEG_7_1_A: u32 = (126 << 16) | 8; // L R C LFE Ls Rs Lc Rc
const LAYOUT_TAG_DVD_10: u32 = (136 << 16) | 4; // L R C LFE
