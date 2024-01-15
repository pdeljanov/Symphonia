// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use log::{debug, error, info, warn};
use std::{convert::TryFrom, fmt, mem::size_of, str};
use symphonia_core::{
    audio::{Channels, Layout},
    codecs::*,
    errors::{decode_error, unsupported_error, Error, Result},
    io::{MediaSourceStream, ReadBytes},
};

#[derive(Debug)]
pub enum Chunk {
    AudioDescription(AudioDescription),
    AudioData(AudioData),
    ChannelLayout(ChannelLayout),
    PacketTable(PacketTable),
    MagicCookie(Box<[u8]>),
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
    ///
    /// The first chunk read will be the AudioDescription chunk. Once it's been read, the caller
    /// should pass it in to subsequent read calls.
    pub fn read(
        reader: &mut MediaSourceStream,
        audio_description: &Option<AudioDescription>,
    ) -> Result<Option<Self>> {
        let chunk_type = reader.read_quad_bytes()?;
        let chunk_size = reader.read_be_i64()?;

        let result = match &chunk_type {
            b"desc" => Chunk::AudioDescription(AudioDescription::read(reader, chunk_size)?),
            b"data" => Chunk::AudioData(AudioData::read(reader, chunk_size)?),
            b"chan" => Chunk::ChannelLayout(ChannelLayout::read(reader, chunk_size)?),
            b"pakt" => {
                Chunk::PacketTable(PacketTable::read(reader, audio_description, chunk_size)?)
            }
            b"kuki" => {
                if let Ok(chunk_size) = usize::try_from(chunk_size) {
                    Chunk::MagicCookie(reader.read_boxed_slice_exact(chunk_size)?)
                }
                else {
                    return invalid_chunk_size_error("Magic Cookie", chunk_size);
                }
            }
            b"free" => {
                if chunk_size < 0 {
                    return invalid_chunk_size_error("Free", chunk_size);
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

                if chunk_size >= 0 {
                    reader.ignore_bytes(chunk_size as u64)?;
                    return Ok(None);
                }
                else {
                    return invalid_chunk_size_error("unsupported", chunk_size);
                }
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
    pub fn read(reader: &mut MediaSourceStream, chunk_size: i64) -> Result<Self> {
        if chunk_size != 32 {
            return invalid_chunk_size_error("Audio Description", chunk_size);
        }

        let sample_rate = reader.read_be_f64()?;
        if sample_rate == 0.0 {
            return decode_error("caf: sample rate must be not be zero");
        }

        let format_id = AudioDescriptionFormatId::read(reader)?;

        let bytes_per_packet = reader.read_be_u32()?;
        let frames_per_packet = reader.read_be_u32()?;

        let channels_per_frame = reader.read_be_u32()?;
        if channels_per_frame == 0 {
            return decode_error("caf: channels per frame must be not be zero");
        }

        let bits_per_channel = reader.read_be_u32()?;

        Ok(Self {
            sample_rate,
            format_id,
            bytes_per_packet,
            frames_per_packet,
            channels_per_frame,
            bits_per_channel,
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
                            error!("unsupported PCM floating point format (bits: {})", bits);
                            return unsupported_error("caf: unsupported bits per channel");
                        }
                    }
                }
                else {
                    match (self.bits_per_channel, *little_endian) {
                        (16, true) => CODEC_TYPE_PCM_S16LE,
                        (16, false) => CODEC_TYPE_PCM_S16BE,
                        (24, true) => CODEC_TYPE_PCM_S24LE,
                        (24, false) => CODEC_TYPE_PCM_S24BE,
                        (32, true) => CODEC_TYPE_PCM_S32LE,
                        (32, false) => CODEC_TYPE_PCM_S32BE,
                        (bits, _) => {
                            error!("unsupported PCM integer format (bits: {})", bits);
                            return unsupported_error("caf: unsupported bits per channel");
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
            Flac => CODEC_TYPE_FLAC,
            Opus => CODEC_TYPE_OPUS,
            unsupported => {
                error!("unsupported codec ({:?})", unsupported);
                return unsupported_error("caf: unsupported codec");
            }
        };

        Ok(result)
    }

    pub fn format_is_compressed(&self) -> bool {
        self.bits_per_channel == 0
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
        let edit_count_offset = size_of::<u32>() as i64;

        if chunk_size != -1 && chunk_size < edit_count_offset {
            return invalid_chunk_size_error("Audio Data", chunk_size);
        }

        let edit_count = reader.read_be_u32()?;
        let start_pos = reader.pos();

        if chunk_size == -1 {
            return Ok(Self { _edit_count: edit_count, start_pos, data_len: None });
        }

        let data_len = (chunk_size - edit_count_offset) as u64;
        debug!("data_len: {}", data_len);
        reader.ignore_bytes(data_len)?;
        Ok(Self { _edit_count: edit_count, start_pos, data_len: Some(data_len) })
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
    Flac,
    Opus,
}

impl AudioDescriptionFormatId {
    pub fn read(reader: &mut MediaSourceStream) -> Result<Self> {
        use AudioDescriptionFormatId::*;

        let format_id = reader.read_quad_bytes()?;
        let format_flags = reader.read_be_u32()?;

        let result = match &format_id {
            // Formats mentioned in the spec
            b"lpcm" => {
                let floating_point = format_flags & (1 << 0) != 0;
                let little_endian = format_flags & (1 << 1) != 0;
                return Ok(LinearPCM { floating_point, little_endian });
            }
            b"ima4" => AppleIMA4,
            b"aac " => {
                if format_flags != 2 {
                    warn!("undocumented AAC object type ({})", format_flags);
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
            // Additional formats from CoreAudioBaseTypes.h
            b"flac" => Flac,
            b"opus" => Opus,
            other => {
                error!("unsupported format id ({:?})", other);
                return unsupported_error("caf: unsupported format id");
            }
        };

        if format_flags != 0 {
            info!("non-zero format flags ({})", format_flags);
        }

        Ok(result)
    }
}

#[derive(Debug)]
pub struct ChannelLayout {
    pub channel_layout: u32,
    pub channel_bitmap: u32,
    pub channel_descriptions: Vec<ChannelDescription>,
}

impl ChannelLayout {
    pub fn read(reader: &mut MediaSourceStream, chunk_size: i64) -> Result<Self> {
        if chunk_size < 12 {
            return invalid_chunk_size_error("Channel Layout", chunk_size);
        }

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
                            info!("unsupported channel label: {}", unsupported);
                            return None;
                        }
                    }
                }
                return Channels::from_bits(channels);
            }
            // Use the channel bitmap
            LAYOUT_TAG_USE_CHANNEL_BITMAP => return Channels::from_bits(self.channel_bitmap),
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
                debug!("unsupported channel layout: {}", unsupported);
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

const LAYOUT_TAG_USE_CHANNEL_BITMAP: u32 = 1 << 16;
// Layout tags from the CAF spec that match the first N channels of a standard layout
const LAYOUT_TAG_MONO: u32 = (100 << 16) | 1;
const LAYOUT_TAG_STEREO: u32 = (101 << 16) | 2;
const LAYOUT_TAG_STEREO_HEADPHONES: u32 = (102 << 16) | 2;
const LAYOUT_TAG_MPEG_3_0_A: u32 = (113 << 16) | 3; // L R C
const LAYOUT_TAG_MPEG_5_1_A: u32 = (121 << 16) | 6; // L R C LFE Ls Rs
const LAYOUT_TAG_MPEG_7_1_A: u32 = (126 << 16) | 8; // L R C LFE Ls Rs Lc Rc
const LAYOUT_TAG_DVD_10: u32 = (136 << 16) | 4; // L R C LFE

pub struct PacketTable {
    pub valid_frames: i64,
    pub priming_frames: i32,
    pub remainder_frames: i32,
    pub packets: Vec<CafPacket>,
}

impl PacketTable {
    pub fn read(
        reader: &mut MediaSourceStream,
        desc: &Option<AudioDescription>,
        chunk_size: i64,
    ) -> Result<Self> {
        if chunk_size < 24 {
            return invalid_chunk_size_error("Packet Table", chunk_size);
        }

        let desc = desc.as_ref().ok_or_else(|| {
            error!("missing audio description");
            Error::DecodeError("caf: missing audio descripton")
        })?;

        let total_packets = reader.read_be_i64()?;
        if total_packets < 0 {
            error!("invalid number of packets in the packet table ({})", total_packets);
            return decode_error("caf: invalid number of packets in the packet table");
        }

        let valid_frames = reader.read_be_i64()?;
        if valid_frames < 0 {
            error!("invalid number of frames in the packet table ({})", valid_frames);
            return decode_error("caf: invalid number of frames in the packet table");
        }

        let priming_frames = reader.read_be_i32()?;
        let remainder_frames = reader.read_be_i32()?;

        let mut packets = Vec::with_capacity(total_packets as usize);
        let mut current_frame = 0;
        let mut packet_offset = 0;

        match (desc.bytes_per_packet, desc.frames_per_packet) {
            // Variable bytes per packet, variable number of frames
            (0, 0) => {
                for _ in 0..total_packets {
                    let size = read_variable_length_integer(reader)?;
                    let frames = read_variable_length_integer(reader)?;
                    packets.push(CafPacket {
                        size,
                        frames,
                        start_frame: current_frame,
                        data_offset: packet_offset,
                    });
                    current_frame += frames;
                    packet_offset += size;
                }
            }
            // Variable bytes per packet, constant number of frames
            (0, frames_per_packet) => {
                for _ in 0..total_packets {
                    let size = read_variable_length_integer(reader)?;
                    let frames = frames_per_packet as u64;
                    packets.push(CafPacket {
                        size,
                        frames,
                        start_frame: current_frame,
                        data_offset: packet_offset,
                    });
                    current_frame += frames;
                    packet_offset += size;
                }
            }
            // Constant bytes per packet, variable number of frames
            (bytes_per_packet, 0) => {
                for _ in 0..total_packets {
                    let size = bytes_per_packet as u64;
                    let frames = read_variable_length_integer(reader)?;
                    packets.push(CafPacket {
                        size,
                        frames,
                        start_frame: current_frame,
                        data_offset: packet_offset,
                    });
                    current_frame += frames;
                    packet_offset += size;
                }
            }
            // Constant bit rate format
            (_, _) => {
                if total_packets > 0 {
                    error!(
                        "unexpected packet table for constant bit rate ({} packets)",
                        total_packets
                    );
                    return decode_error(
                        "caf: unexpected packet table for constant bit rate format",
                    );
                }
            }
        }

        Ok(Self { valid_frames, priming_frames, remainder_frames, packets })
    }
}

impl fmt::Debug for PacketTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PacketTable")?;
        write!(
            f,
            "{{ valid_frames: {}, priming_frames: {}, remainder_frames: {}, packet count: {}}}",
            self.valid_frames,
            self.priming_frames,
            self.remainder_frames,
            self.packets.len()
        )
    }
}

#[derive(Debug)]
pub struct CafPacket {
    // The packet's offset in bytes from the start of the data
    pub data_offset: u64,
    // The index of the first frame in the packet
    pub start_frame: u64,
    // The number of frames in the packet
    // For files with a constant frames per packet this value will match frames_per_packet
    pub frames: u64,
    // The size in bytes of the packet
    // For constant bit-rate files this value will match bytes_per_packet
    pub size: u64,
}

fn invalid_chunk_size_error<T>(chunk_type: &str, chunk_size: i64) -> Result<T> {
    error!("invalid {} chunk size ({})", chunk_type, chunk_size);
    decode_error("caf: invalid chunk size")
}

fn read_variable_length_integer(reader: &mut MediaSourceStream) -> Result<u64> {
    let mut result = 0;

    for _ in 0..9 {
        let byte = reader.read_byte()?;

        result |= (byte & 0x7f) as u64;

        if byte & 0x80 == 0 {
            return Ok(result);
        }

        result <<= 7;
    }

    decode_error("caf: unterminated variable-length integer")
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn variable_length_integer_test(bytes: &[u8], expected: u64) -> Result<()> {
        let cursor = Cursor::new(Vec::from(bytes));
        let mut source = MediaSourceStream::new(Box::new(cursor), Default::default());

        assert_eq!(read_variable_length_integer(&mut source)?, expected);

        Ok(())
    }

    #[test]
    fn variable_length_integers() -> Result<()> {
        variable_length_integer_test(&[0x01], 1)?;
        variable_length_integer_test(&[0x11], 17)?;
        variable_length_integer_test(&[0x7f], 127)?;
        variable_length_integer_test(&[0x81, 0x00], 128)?;
        variable_length_integer_test(&[0x81, 0x02], 130)?;
        variable_length_integer_test(&[0x82, 0x01], 257)?;
        variable_length_integer_test(&[0xff, 0x7f], 16383)?;
        variable_length_integer_test(&[0x81, 0x80, 0x00], 16384)?;
        Ok(())
    }

    #[test]
    fn unterminated_variable_length_integer() {
        let cursor = Cursor::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        let mut source = MediaSourceStream::new(Box::new(cursor), Default::default());

        assert!(read_variable_length_integer(&mut source).is_err());
    }
}
