use std::fmt;
use std::marker::PhantomData;

use sonata_core::errors::{Result, decode_error, unsupported_error};
use sonata_core::io::Bytestream;
use sonata_core::tags::{Tag, RiffTag};

/// `ParseChunkTag` implements `parse_tag` to map between the 4-byte chunk identifier and the enumeration 
pub trait ParseChunkTag : Sized {
    fn parse_tag(tag: &[u8; 4], len: u32) -> Option<Self>;
}

enum NullChunks {}

impl ParseChunkTag for NullChunks {
    fn parse_tag(_tag: &[u8; 4], _len: u32) -> Option<Self> { None }
}

/// `ChunksReader` reads chunks from a `ByteStream`. It is generic across a type, usually an enum, implementing the 
/// `ParseChunkTag` trait. When a new chunk is encountered in the stream, `parse_tag` on T is called to return an 
/// object capable of parsing/reading that chunk or `None`. This makes reading the actual chunk data lazy in that the 
/// chunk is not read until the object is consumed.
pub struct ChunksReader<T: ParseChunkTag> {
    len: u32,
    consumed: u32,
    phantom: PhantomData<T>,
}

impl<T: ParseChunkTag> ChunksReader<T> {
    pub fn new(len: u32) -> Self {
        ChunksReader { 
            len, 
            consumed: 0, 
            phantom: PhantomData
        }
    }

    pub fn next<B: Bytestream>(&mut self, reader: &mut B) -> Result<Option<T>> {
        // Loop until a chunk is recognized and returned, or the end of stream is reached.
        loop {
            // Align to the next 2-byte boundary if not currently aligned..
            if self.consumed & 0x1 == 1 {
                reader.read_u8()?;
                self.consumed += 1;
            }

            // Check if there are enough bytes for another chunk, if not, there are no more chunks.
            if self.consumed + 8 > self.len {
                return Ok(None);
            }

            // Read tag and len, the chunk header.
            let tag = reader.read_quad_bytes()?;
            let len = reader.read_u32()?;
            self.consumed += 8;

            // Check if the chunk length will exceed the parent chunk.
            if self.consumed + len > self.len {
                return decode_error("Info chunk length exceeds parent List chunk length.");
            }

            // "Consume" the chunk.
            self.consumed += len;

            match T::parse_tag(&tag, len) {
                Some(chunk) => return Ok(Some(chunk)),
                None => {
                    // As per the RIFF spec, unknown chunks are to be ignored.
                    eprintln!("Ignoring unknown chunk: tag={}, len={}.", String::from_utf8_lossy(&tag), len);
                    reader.ignore_bytes(len as u64)?
                }
            }
        }
    }

    pub fn finish<B: Bytestream>(&mut self, reader: &mut B) -> Result<()>{
        // If data is remaining in this chunk, skip it.
        if self.consumed < self.len {
            let remaining = self.len - self.consumed;
            reader.ignore_bytes(remaining as u64)?;
            self.consumed += remaining;
        }

        // Pad the chunk to the next 2-byte boundary.
        if self.len & 0x1 == 1 {
            reader.read_u8()?;
        }

        Ok(())
    }
}

/// Common trait implemented for all chunks that are parsed by a `ChunkParser`.
pub trait ParseChunk : Sized {
    fn parse<B: Bytestream>(reader: &mut B, tag: [u8; 4], len: u32) -> Result<Self>;
}

/// `ChunkParser` is a utility struct for unifying the parsing of chunks.
pub struct ChunkParser<P: ParseChunk> {
    tag: [u8; 4],
    len: u32,
    phantom: PhantomData<P>,
}

impl<P: ParseChunk> ChunkParser<P> {
    fn new(tag: [u8; 4], len: u32) -> Self {
        ChunkParser {
            tag,
            len,
            phantom: PhantomData,
        }
    }

    pub fn parse<B: Bytestream>(&self, reader: &mut B) -> Result<P> {
        P::parse(reader, self.tag, self.len)
    }
}

pub enum WaveFormatData {
    Pcm(WaveFormatPcm),
    IeeeFloat,
    Extensible(WaveFormatExtensible),
}

pub struct WaveFormatPcm {
    /// The number of bits per sample. In the PCM format, this is always a multiple of 8-bits.
    pub bits_per_sample: u16,
}

pub struct WaveFormatExtensible {
    /// The number of bits per sample rounded up to the nearest 8-bits.
    pub bits_per_sample: u16,
    /// The number of bits per sample.
    pub bits_per_coded_sample: u16,
    /// Mask of channels.
    pub channel_mask: u32,
    /// Globally unique identifier of the format.
    pub sub_format_guid: [u8; 16],
}

pub struct WaveFormatChunk {
    /// The number of channels.
    pub n_channels: u16,
    /// The sample rate in Hz. For non-PCM formats, this value must be interpreted as per the format's specifications.
    pub sample_rate: u32,
    /// The required average data rate required in bytes/second. For non-PCM formats, this value must be interpreted as 
    /// per the format's specifications.
    pub avg_bytes_per_sec: u32,
    /// The byte alignment of one audio frame. For PCM formats, this is equal to 
    /// `(n_channels * extra_data.bits_per_sample) / 8`. For non-PCM formats, this value must be interpreted as per the 
    /// format's specifications.
    pub block_align: u16,
    /// Extra data associated with the format block conditional upon the format tag.
    pub format_data: WaveFormatData,
}

impl WaveFormatChunk {

    fn read_pcm_fmt<B: Bytestream>(reader: &mut B, bits_per_sample: u16, len: u32) -> Result<WaveFormatData> {
        // WaveFormat for a PCM format /may/ be extended with an extra data length parameter followed by the 
        // extra data itself. Use the chunk length to determine if the format chunk is extended.
        let is_extended = match len {
            // Minimal WavFormat struct, no extension.
            16 => false,
            // WaveFormatEx with exta data length field present, but not extra data.
            18 => true,
            // WaveFormatEx with extra data length field and extra data.
            40 => true,
            _ => return decode_error("Malformed PCM fmt chunk."),
        };

        // If there is extra data, read the length, and discard the extra data.
        if is_extended {
            let extra_size = reader.read_u16()?; 

            if extra_size > 0 {
                reader.ignore_bytes(extra_size as u64)?;
            }
        }

        // Bits per sample for PCM is both the decoded width, and actual sample width. Strictly, this must 
        // either be 8 or 16 bits, but there is no reason why 24 and 32 bits can't be supported. Since these 
        // files do exist, allow 8/16/24/32-bit, but error if not a multiple of 8 or greater than 32-bits.
        if (bits_per_sample > 32) || (bits_per_sample & 0x7 != 0) {
            return decode_error("Bits per sample for PCM Wave Format must either be 8 or 16 bits.");
        }

        Ok(WaveFormatData::Pcm(WaveFormatPcm { bits_per_sample }))
    }

    fn read_ieee_fmt<B: Bytestream>(reader: &mut B, bits_per_sample: u16, len: u32) -> Result<WaveFormatData> {
        // WaveFormat for a IEEE format should not be extended, but it may still have an extra data length 
        // parameter.
        if len == 18 {
            let extra_size = reader.read_u16()?; 
            if extra_size != 0 {
                return decode_error("Extra data not expected for IEEE fmt chunk.");
            }
        }
        else if len > 16 {
            return decode_error("Malformed IEEE fmt chunk.");
        }

        // Officially, only 32-bit floats are supported, but Sonata can handle 64-bit floats.
        if bits_per_sample != 32 || bits_per_sample != 64 {
            return decode_error("Bits per sample for IEEE Wave Format must be 32-bits.");
        }

        Ok(WaveFormatData::IeeeFloat)
    }

    fn read_ext_fmt<B: Bytestream>(reader: &mut B, bits_per_sample: u16, len: u32) -> Result<WaveFormatData> {
        // WaveFormat for the extensible format must be extended to 40 bytes in length.
        if len < 40 {
            return decode_error("Malformed Extensible fmt chunk.");
        }

        let extra_size = reader.read_u16()?; 

        // The size of the extra data for the Extensible format is exactly 22 bytes.
        if extra_size != 22 {
            return decode_error("Extra data size not 22 bytes for Extensible fmt chunk.");
        }

        // Bits per sample for extensible formats is the decoded "container" width per sample. This must be 
        // a multiple of 8.
        if bits_per_sample % 8 > 0 {
            return decode_error("Bits per sample for Extensible Wave Format must be a multiple of 8 bits.");
        }
        
        let bits_per_coded_sample = reader.read_u16()?;
        let channel_mask = reader.read_u32()?;
        let mut sub_format_guid = [0u8; 16];

        reader.read_buf_bytes(&mut sub_format_guid)?;

        // These GUIDs identifiy the format of the data chunks. These definitions can be found in ksmedia.h of the 
        // Microsoft Windows Platform SDK.
        const KSDATAFORMAT_SUBTYPE_PCM: [u8; 16] = 
            [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71];
        // const KSDATAFORMAT_SUBTYPE_ADPCM: [u8; 16] = 
        //     [0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x10, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71];
        const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: [u8; 16] = 
            [0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71];
        // const KSDATAFORMAT_SUBTYPE_ALAW: [u8; 16] = 
        //     [0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x10, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71];
        // const KSDATAFORMAT_SUBTYPE_MULAW: [u8; 16] = 
        //     [0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x10, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71];

        // Verify support based on the format GUID.
        match sub_format_guid {
            KSDATAFORMAT_SUBTYPE_PCM => {}
            KSDATAFORMAT_SUBTYPE_IEEE_FLOAT => {},
            _ => return unsupported_error("Unsupported Wave Format."),
        };

        Ok(WaveFormatData::Extensible(WaveFormatExtensible { 
            bits_per_sample, bits_per_coded_sample, channel_mask, sub_format_guid }))
    }
}

impl ParseChunk for WaveFormatChunk {
    fn parse<B: Bytestream>(reader: &mut B, _tag: [u8; 4], len: u32) -> Result<WaveFormatChunk> {
        // WaveFormat has a minimal length of 16 bytes. This may be extended with format specific data later.
        if len < 16 {
            return decode_error("Malformed fmt chunk.");
        }

        let format = reader.read_u16()?;
        let n_channels = reader.read_u16()?;
        let sample_rate = reader.read_u32()?;
        let avg_bytes_per_sec = reader.read_u32()?;
        let block_align = reader.read_u16()?;
        let bits_per_sample = reader.read_u16()?;

        // The definition of these format identifiers can be found in mmreg.h of the Microsoft Windows Platform SDK.
        const WAVE_FORMAT_PCM: u16        = 0x0001;
        // const WAVE_FORMAT_ADPCM: u16        = 0x0002;
        const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
        // const WAVE_FORMAT_ALAW: u16       = 0x0006;
        // const WAVE_FORMAT_MULAW: u16      = 0x0007;
        const WAVE_FORMAT_EXTENSIBLE: u16 = 0xfffe;

        let format_data = match format {
            // The PCM Wave Format
            WAVE_FORMAT_PCM => Self::read_pcm_fmt(reader, bits_per_sample, len),
            // The IEEE Float Wave Format
            WAVE_FORMAT_IEEE_FLOAT => Self::read_ieee_fmt(reader, bits_per_sample, len),
            // The Extensible Wave Format
            WAVE_FORMAT_EXTENSIBLE => Self::read_ext_fmt(reader, bits_per_sample, len),
            // Unsupported format.
            _ => unsupported_error("Unsupported Wave Format."),
        }?;

        Ok(WaveFormatChunk { n_channels, sample_rate, avg_bytes_per_sec, block_align, format_data })
    }
}

impl fmt::Display for WaveFormatChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "WaveFormatChunk {{")?;
        writeln!(f, "\tn_channels: {},", self.n_channels)?;
        writeln!(f, "\tsample_rate: {} Hz,", self.sample_rate)?;
        writeln!(f, "\tavg_bytes_per_sec: {},", self.avg_bytes_per_sec)?;
        writeln!(f, "\tblock_align: {},", self.block_align)?;

        match self.format_data {
            WaveFormatData::Pcm(ref data) => {
                writeln!(f, "\tformat_data: Pcm {{")?;
                writeln!(f, "\t\tbits_per_sample: {},", data.bits_per_sample)?;
            },
            WaveFormatData::IeeeFloat => {
                writeln!(f, "\tformat_data: IeeeFloat {{")?;
            },
            WaveFormatData::Extensible(ref data) => {
                writeln!(f, "\tformat_data: Extensible {{")?;
                writeln!(f, "\t\tbits_per_sample: {},", data.bits_per_sample)?;
                writeln!(f, "\t\tbits_per_coded_sample: {},", data.bits_per_coded_sample)?;
                writeln!(f, "\t\tchannel_maske: {},", data.channel_mask)?;
                writeln!(f, "\t\tsub_format_guid: {:?},", &data.sub_format_guid)?;
            },
        };

        writeln!(f, "\t}}")?;
        writeln!(f, "}}")
    }
}

pub struct FactChunk {
    n_frames: u32,
}

impl ParseChunk for FactChunk {
    fn parse<B: Bytestream>(reader: &mut B, _tag: [u8; 4], len: u32) -> Result<Self> {
        // A Fact chunk is exactly 4 bytes long, though there is some mystery as to whether there can be more fields
        // in the chunk.
        if len != 4 {
            return decode_error("Malformed fact chunk.");
        }

        Ok(FactChunk{ n_frames: reader.read_u32()? })
    }
}

impl fmt::Display for FactChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "FactChunk {{")?;
        writeln!(f, "\tn_frames: {},", self.n_frames)?;
        writeln!(f, "}}")
    }
}

pub struct ListChunk {
    pub form: [u8; 4],
    pub len: u32, 
}

impl ListChunk {
    pub fn skip<B: Bytestream>(&self, reader: &mut B) -> Result<()> {
        ChunksReader::<NullChunks>::new(self.len).finish(reader)
    }
}

impl ParseChunk for ListChunk {
    fn parse<B: Bytestream>(reader: &mut B, _tag: [u8; 4], len: u32) -> Result<Self> {
        // A List chunk must contain atleast the list/form identifier. However, an empty list (len = 4) is permissible.
        if len < 4 {
            return decode_error("Malformed list chunk.");
        }

        Ok(ListChunk{ 
            form: reader.read_quad_bytes()?,
            len: len - 4
        })
    }
}

impl fmt::Display for ListChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "ListChunk {{")?;
        writeln!(f, "\tform: {},", String::from_utf8_lossy(&self.form))?;
        writeln!(f, "\tlen: {},", self.len)?;
        writeln!(f, "}}")
    }
}

pub struct InfoChunk {
    pub tag: Tag,
}

impl ParseChunk for InfoChunk {
    fn parse<B: Bytestream>(reader: &mut B, tag: [u8; 4], len: u32) -> Result<InfoChunk> {
        let mut value_buf = vec![0u8; len as usize];
        reader.read_buf_bytes(&mut value_buf)?;

        let value = String::from_utf8_lossy(&value_buf);

        Ok(InfoChunk {
            tag: RiffTag::parse(tag, &value)
        })
    }
}

pub enum RiffWaveChunks {
    Format(ChunkParser<WaveFormatChunk>),
    List(ChunkParser<ListChunk>),
    Fact(ChunkParser<FactChunk>),
    Data
}

macro_rules! parser {
    ($class:expr, $result:ty, $tag:expr, $len:expr) => {
        Some($class(ChunkParser::<$result>::new($tag, $len)))
    };
}

impl ParseChunkTag for RiffWaveChunks {
    fn parse_tag(tag: &[u8; 4], len: u32) -> Option<Self> {
        match tag {
            b"fmt " => parser!(RiffWaveChunks::Format, WaveFormatChunk, *tag, len),
            b"LIST" => parser!(RiffWaveChunks::List, ListChunk, *tag, len),
            b"fact" => parser!(RiffWaveChunks::Fact, FactChunk, *tag, len),
            b"data" => Some(RiffWaveChunks::Data),
            _ => None,
        }
    }
}

pub enum RiffInfoListChunks {
    Info(ChunkParser<InfoChunk>),
}

impl ParseChunkTag for RiffInfoListChunks {
    fn parse_tag(tag: &[u8; 4], len: u32) -> Option<Self> {
        // Right now it is assumed all list chunks are INFO chunks, but that's not really guaranteed.
        // TODO: Actually validate that the chunk is an info chunk.
        parser!(RiffInfoListChunks::Info, InfoChunk, *tag, len)
    }
}