use symphonia_core::errors::{decode_error, unsupported_error, Result};
use symphonia_core::io::{MediaSourceStream, ReadBytes};

const DSD_CHUNK_HEADER: [u8; 4] = *b"DSD ";
const FMT_CHUNK_HEADER: [u8; 4] = *b"fmt ";
const DATA_CHUNK_HEADER: [u8; 4] = *b"data";

pub struct DSDChunk {
    pub file_size: u64,
    pub metadata_offset: u64,
}

impl DSDChunk {
    pub fn read(source: &mut MediaSourceStream) -> Result<DSDChunk> {
        let header = source.read_quad_bytes()?;
        if header != DSD_CHUNK_HEADER {
            return unsupported_error("dsf: missing DSD chunk marker");
        }

        let chunk_size = source.read_u64()?;
        if chunk_size != 28 {
            return decode_error("dsf: invalid DSD chunk size");
        }

        let file_size = source.read_u64()?;
        let metadata_offset = source.read_u64()?;

        Ok(DSDChunk { file_size, metadata_offset })
    }
}

pub struct FmtChunk {
    pub channel_type: u32,
    pub channel_num: u32,
    pub sampling_frequency: u32,
    pub bits_per_sample: u32,
    pub sample_count: u64,
    pub block_size_per_channel: u32,
}

impl FmtChunk {
    pub fn read(source: &mut MediaSourceStream) -> Result<FmtChunk> {
        let header = source.read_quad_bytes()?;
        if header != FMT_CHUNK_HEADER {
            return unsupported_error("dsf: missing fmt chunk marker");
        }

        let chunk_size = source.read_u64()?;
        // chunk_size can be variable if ID3v2 is present? No, standard says 52 usually?
        // Code checked 52.
        if chunk_size < 52 {
            return decode_error("dsf: fmt chunk too small");
        }

        let format_version = source.read_u32()?;
        if format_version != 1 {
            return unsupported_error("dsf: unsupported format version");
        }

        let format_id = source.read_u32()?;
        if format_id != 0 {
            return unsupported_error("dsf: unsupported format id");
        }

        let channel_type = source.read_u32()?;
        let channel_num = source.read_u32()?;
        let sampling_frequency = source.read_u32()?;
        let bits_per_sample = source.read_u32()?;
        let sample_count = source.read_u64()?;
        let block_size_per_channel = source.read_u32()?;
        let reserved = source.read_u32()?;

        if reserved != 0 {
            // log warning?
        }

        // Skip any extra bytes if chunk_size > 52
        if chunk_size > 52 {
            source.ignore_bytes(chunk_size - 52)?;
        }

        Ok(FmtChunk {
            channel_type,
            channel_num,
            sampling_frequency,
            bits_per_sample,
            sample_count,
            block_size_per_channel,
        })
    }
}

pub struct DataChunk {
    pub chunk_size: u64,
}

impl DataChunk {
    pub fn read(source: &mut MediaSourceStream) -> Result<DataChunk> {
        let header = source.read_quad_bytes()?;
        if header != DATA_CHUNK_HEADER {
            return unsupported_error("dsf: missing data chunk marker");
        }

        let chunk_size = source.read_u64()?;

        Ok(DataChunk { chunk_size })
    }
}

pub struct DSFMetadata {
    pub dsd_chunk: DSDChunk,
    pub fmt_chunk: FmtChunk,
    pub data_chunk: DataChunk,
}

impl DSFMetadata {
    pub fn read(source: &mut MediaSourceStream) -> Result<DSFMetadata> {
        let dsd_chunk = DSDChunk::read(source)?;
        let fmt_chunk = FmtChunk::read(source)?;
        let data_chunk = DataChunk::read(source)?;
        Ok(DSFMetadata { dsd_chunk, fmt_chunk, data_chunk })
    }
}
