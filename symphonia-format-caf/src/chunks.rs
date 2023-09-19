use log::{debug, error, info};
use std::{mem::size_of, str};
use symphonia_core::{
    codecs::*,
    errors::{decode_error, unsupported_error, Result},
    io::{MediaSourceStream, ReadBytes},
};

#[derive(Debug)]
pub enum Chunk {
    AudioDescription(AudioDescription),
    AudioData(AudioData),
    Free,
}

impl Chunk {
    /// Reads a chunk
    ///
    /// After calling this function the reader's position will be:
    ///   - at the start of the next chunk,
    ///   - or at the end of the file,
    ///   - or, if the chunk is the audio data chunk and the size is unknown,
    ///     then at the start of the audio data.
    pub fn read(mut reader: &mut MediaSourceStream) -> Result<Self> {
        let chunk_type = reader.read_quad_bytes()?;
        let chunk_size = reader.read_be_i64()?;

        let result = match &chunk_type {
            b"data" => Chunk::AudioData(AudioData::read(&mut reader, chunk_size)?),
            b"desc" => Chunk::AudioDescription(AudioDescription::read(&mut reader)?),
            b"free" => {
                if chunk_size < 0 {
                    error!("invalid Free chunk size ({chunk_size})");
                    return decode_error("invalid Free chunk size");
                }
                reader.ignore_bytes(chunk_size as u64)?;
                Chunk::Free
            }
            other => {
                info!(
                    "unsupported chunk type ('{}')",
                    str::from_utf8(other.as_slice()).unwrap_or("????")
                );
                return unsupported_error("unsupported chunk type");
            }
        };

        debug!("chunk: {result:?} - size: {chunk_size}");
        Ok(result)
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
