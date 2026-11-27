//! Reading the test vector files: the packet records of a `.bit` file and the reference PCM.

use super::*;
use std::path::{Path, PathBuf};

/// One `opus_demo` record: the encoder final range and the packet payload.
pub(crate) struct BitRecord<'a> {
    pub(crate) enc_final_range: u32,
    pub(crate) payload: &'a [u8],
}

/// Splits a `.bit` file into records the way `opus_demo -d` reads it: per packet a 32-bit
/// big-endian payload length, a 32-bit big-endian encoder final range and the payload. A partial
/// length field at the end of the file is end of input (`feof`); an invalid length or a truncated
/// record is an error.
pub(crate) fn parse_bit_records(bytes: &[u8]) -> Result<Vec<BitRecord<'_>>, String> {
    let mut records = Vec::new();
    let mut rest = bytes;
    while let Some((len_field, tail)) = rest.split_first_chunk::<RECORD_FIELD_BYTES>() {
        let i = records.len();
        let len = u32::from_be_bytes(*len_field) as usize;
        if len > MAX_PACKET {
            return Err(format!("record {i}: invalid payload length {len}"));
        }
        let Some((range_field, tail)) = tail.split_first_chunk::<RECORD_FIELD_BYTES>()
        else {
            return Err(format!("record {i}: truncated encoder final range"));
        };
        let Some((payload, tail)) = tail.split_at_checked(len)
        else {
            return Err(format!("record {i}: ran out of input, expecting {len} bytes"));
        };
        records.push(BitRecord { enc_final_range: u32::from_be_bytes(*range_field), payload });
        rest = tail;
    }
    return Ok(records);
}

/// Reads the vector directory from `OPUS_TESTVECTORS_DIR`; a conformance run without vectors
/// must fail loudly rather than pass.
pub(crate) fn vectors_dir() -> PathBuf {
    let Some(dir) = std::env::var_os(VECTORS_DIR_ENV)
    else {
        panic!(
            "{VECTORS_DIR_ENV} is not set: point it at the unpacked RFC 8251 test vectors \
             (testvector01.bit .. testvector12.bit, testvectorNN.dec, testvectorNNm.dec), see \
             <https://datatracker.ietf.org/doc/html/rfc8251#section-11>"
        );
    };
    return PathBuf::from(dir);
}

pub(crate) fn read_file(path: &Path) -> Vec<u8> {
    return std::fs::read(path)
        .unwrap_or_else(|err| panic!("cannot read test vector file {}: {err}", path.display()));
}

/// Reads a 16-bit little-endian PCM file as written by `opus_demo`.
pub(crate) fn read_pcm16(bytes: &[u8]) -> Vec<i16> {
    return bytes.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]])).collect();
}
