//! Opus header parsing implementation.
//!
//! This module provides functionality to parse Opus headers as specified in RFC 7845.
//! It includes structures and methods to handle both the identification header and the comment header.
//!
//! References:
//! - RFC 7845: Ogg Encapsulation for the Opus Audio Codec (https://tools.ietf.org/html/rfc7845)
//! - RFC 6716: Definition of the Opus Audio Codec (https://tools.ietf.org/html/rfc6716)

use std::convert::TryFrom;
use std::io::{Read, Cursor};
use byteorder::{ReadBytesExt, LittleEndian};
use thiserror::Error;

/// Errors that can occur during Opus header parsing.
#[derive(Debug, Error)]
pub enum Error {
    #[error("Invalid magic signature")]
    InvalidMagicSignature,

    #[error("Unsupported version: {0}")]
    UnsupportedVersion(u8),

    #[error("Invalid channel count: {0}")]
    InvalidChannelCount(u8),

    #[error("Invalid pre-skip value: {0}")]
    InvalidPreSkip(u16),

    #[error("Invalid input sample rate: {0}")]
    InvalidInputSampleRate(u32),

    #[error("Invalid output gain: {0}")]
    InvalidOutputGain(i16),

    #[error("Invalid channel mapping family: {0}")]
    InvalidChannelMappingFamily(u8),

    #[error("Invalid stream count: {0}")]
    InvalidStreamCount(u8),

    #[error("Invalid coupled count: {0}")]
    InvalidCoupledCount(u8),

    #[error("Invalid channel mapping")]
    InvalidChannelMapping,

    #[error("Unexpected end of input")]
    UnexpectedEof,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("UTF-8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

/// Channel mapping family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMappingFamily {
    /// RTP mapping (mono or stereo).
    Rtp,
    /// Vorbis channel order.
    Vorbis,
    /// No defined channel meaning.
    Undefined,
    /// Reserved for future use.
    Reserved(u8),
}

impl TryFrom<u8> for ChannelMappingFamily {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Rtp),
            1 => Ok(Self::Vorbis),
            2..=254 => Ok(Self::Reserved(value)),
            255 => Ok(Self::Undefined),
            _ => Err(Error::InvalidChannelMappingFamily(value)) // unreachable!(), 
        }
    }
}

/// Identification Header
///```text 
///       0                   1                   2                   3
///       0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      |      'O'      |      'p'      |      'u'      |      's'      |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      |      'H'      |      'e'      |      'a'      |      'd'      |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      |  Version = 1  | Channel Count |           Pre-skip            |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      |                     Input Sample Rate (Hz)                    |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      |   Output Gain (Q7.8 in dB)    | Mapping Family|               |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+               :
///      |                                                               |
///      :               Optional Channel Mapping Table...               :
///      |                                                               |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// 
///                         Figure 2: ID Header Packet 
///```
/// 
/// https://datatracker.ietf.org/doc/html/rfc7845#section-5.1
#[derive(Debug, Clone, PartialEq)]
pub struct IdentificationHeader {
    pub version: u8,
    pub channel_count: u8,
    pub pre_skip: u16,
    pub input_sample_rate: u32,
    pub output_gain: i16,
    pub channel_mapping_family: ChannelMappingFamily,
    pub channel_mapping_table: Option<Vec<u8>>,
}


impl IdentificationHeader {
    const MAGIC_SIGNATURE: &'static [u8] = b"OpusHead";

    /// Parse Opus identification header from a byte stream.
    pub fn parse<R: Read>(mut reader: R) -> Result<Self, Error> {
        let _ = Self::parse_magic_signature(&mut reader)?;
        let version = Self::parse_version(&mut reader)?;
        let channel_count = Self::parse_channel_count(&mut reader)?;
        let pre_skip = Self::parse_pre_skip(&mut reader)?;
        let input_sample_rate = Self::parse_input_sample_rate(&mut reader)?;
        let output_gain = Self::parse_output_gain(&mut reader)?;
        let channel_mapping_family = Self::parse_channel_mapping_family(&mut reader)?;
        let channel_mapping_table = Self::parse_channel_mapping_table(&mut reader, &channel_mapping_family, channel_count)?;

        return Ok(Self {
            version,
            channel_count,
            pre_skip,
            input_sample_rate,
            output_gain,
            channel_mapping_family,
            channel_mapping_table,
        });
    }

    fn parse_magic_signature<R: Read>(reader: &mut R) -> Result<[u8; 8], Error> {
        let mut magic = [0u8; 8];
        reader.read_exact(&mut magic)?;
        if magic != Self::MAGIC_SIGNATURE {
            return Err(Error::InvalidMagicSignature);
        }

        return Ok(magic);
    }

    fn parse_version<R: Read>(reader: &mut R) -> Result<u8, Error> {
        let version = reader.read_u8()?;
        if version != 1 {
            return Err(Error::UnsupportedVersion(version));
        }

        return Ok(version);
    }

    fn parse_channel_count<R: Read>(reader: &mut R) -> Result<u8, Error> {
        let channel_count = reader.read_u8()?;
        if channel_count == 0 {
            return Err(Error::InvalidChannelCount(channel_count));
        }

        return Ok(channel_count);
    }

    fn parse_pre_skip<R: Read>(reader: &mut R) -> Result<u16, Error> {
        return Ok(reader.read_u16::<LittleEndian>()?);
    }

    fn parse_input_sample_rate<R: Read>(reader: &mut R) -> Result<u32, Error> {
        return Ok(reader.read_u32::<LittleEndian>()?);
    }

    fn parse_output_gain<R: Read>(reader: &mut R) -> Result<i16, Error> {
        return Ok(reader.read_i16::<LittleEndian>()?);
    }

    fn parse_channel_mapping_family<R: Read>(reader: &mut R) -> Result<ChannelMappingFamily, Error> {
        return ChannelMappingFamily::try_from(reader.read_u8()?);
    }

    fn parse_channel_mapping_table<R: Read>(
        reader: &mut R,
        channel_mapping_family: &ChannelMappingFamily,
        channel_count: u8,
    ) -> Result<Option<Vec<u8>>, Error> {
        return match channel_mapping_family {
            ChannelMappingFamily::Rtp => Ok(None),
            _ => {
                let stream_count = reader.read_u8()?;
                if stream_count == 0 {
                    return Err(Error::InvalidStreamCount(stream_count));
                }

                let coupled_count = reader.read_u8()?;
                if coupled_count > stream_count {
                    return Err(Error::InvalidCoupledCount(coupled_count));
                }

                let mut table = vec![0u8; channel_count as usize];
                reader.read_exact(&mut table)?;

                if table.iter().any(|&x| x >= stream_count) {
                    return Err(Error::InvalidChannelMapping);
                }

                Ok(Some(table))
            }
        };
    }
}

/// Opus comment header structure.
#[derive(Debug, Clone, PartialEq)]
pub struct OpusCommentHeader {
    pub vendor_string: String,
    pub user_comments: Vec<String>,
}

impl OpusCommentHeader {
    const MAGIC_SIGNATURE: &'static [u8] = b"OpusTags";

    /// Parse Opus comment header from a byte stream.
    pub fn parse<R: Read>(mut reader: R) -> Result<Self, Error> {
        Self::validate_magic_signature(&mut reader)?;
        let vendor_string = Self::parse_vendor_string(&mut reader)?;
        let user_comments = Self::parse_user_comments(&mut reader)?;

        return Ok(Self {
            vendor_string,
            user_comments,
        });
    }

    fn validate_magic_signature<R: Read>(reader: &mut R) -> Result<(), Error> {
        let mut magic = [0u8; 8];
        reader.read_exact(&mut magic)?;
        if magic != Self::MAGIC_SIGNATURE {
            return Err(Error::InvalidMagicSignature);
        }
        Ok(())
    }

    fn parse_vendor_string<R: Read>(reader: &mut R) -> Result<String, Error> {
        let vendor_length = reader.read_u32::<LittleEndian>()? as usize;
        let mut vendor_string = vec![0u8; vendor_length];
        reader.read_exact(&mut vendor_string)?;
        Ok(String::from_utf8(vendor_string)?)
    }

    fn parse_user_comments<R: Read>(reader: &mut R) -> Result<Vec<String>, Error> {
        let user_comment_list_length = reader.read_u32::<LittleEndian>()? as usize;
        let mut user_comments = Vec::with_capacity(user_comment_list_length);

        for _ in 0..user_comment_list_length {
            let comment = Self::parse_single_comment(reader)?;
            user_comments.push(comment);
        }

        Ok(user_comments)
    }

    fn parse_single_comment<R: Read>(reader: &mut R) -> Result<String, Error> {
        let comment_length = reader.read_u32::<LittleEndian>()? as usize;
        let mut comment = vec![0u8; comment_length];
        reader.read_exact(&mut comment)?;
        Ok(String::from_utf8(comment)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod id_header {
        use super::*;

        fn create_valid_id_header() -> Vec<u8> {
            return vec![
                0x4F, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, // Magic signature "OpusHead"
                0x01, // Version
                0x02, // Channel count
                0x38, 0x01, // Pre-skip
                0x80, 0xBB, 0x00, 0x00, // Input sample rate (48000 Hz)
                0x00, 0x00, // Output gain
                0x00, // Channel mapping family (RTP)
            ];
        }

        #[test]
        fn check_valid_id_header_parsing() {
            let id_header = create_valid_id_header();
            let parsed_header = IdentificationHeader::parse(Cursor::new(&id_header)).unwrap();

            assert_eq!(parsed_header.version, 1);
            assert_eq!(parsed_header.channel_count, 2);
            assert_eq!(parsed_header.pre_skip, 312);
            assert_eq!(parsed_header.input_sample_rate, 48000);
            assert_eq!(parsed_header.output_gain, 0);
            assert_eq!(parsed_header.channel_mapping_family, ChannelMappingFamily::Rtp);
            assert!(parsed_header.channel_mapping_table.is_none());
        }

        #[test]
        fn check_invalid_magic_signature() {
            let mut invalid_header = create_valid_id_header();
            invalid_header[7] = b's'; // Change last byte of signature

            let result = IdentificationHeader::parse(Cursor::new(&invalid_header));
            assert!(matches!(result, Err(Error::InvalidMagicSignature)));
        }

        #[test]
        fn check_unsupported_version() {
            let mut invalid_header = create_valid_id_header();
            invalid_header[8] = 2; // Change version to 2

            let result = IdentificationHeader::parse(Cursor::new(&invalid_header));
            assert!(matches!(result, Err(Error::UnsupportedVersion(2))));
        }

        #[test]
        fn check_invalid_channel_count() {
            let mut invalid_header = create_valid_id_header();
            invalid_header[9] = 0; // Change channel count to 0

            let result = IdentificationHeader::parse(Cursor::new(&invalid_header));
            assert!(matches!(result, Err(Error::InvalidChannelCount(0))));
        }
        #[test]
        fn check_vorbis_channel_mapping() {
            let mut header = create_valid_id_header();
            header[18] = 1; // Change channel mapping family to Vorbis
            header.extend_from_slice(&[2, 1, 0, 1]); // Stream count, coupled count, channel mapping

            let parsed_header = IdentificationHeader::parse(Cursor::new(&header)).unwrap();

            assert_eq!(parsed_header.channel_mapping_family, ChannelMappingFamily::Vorbis);
            assert_eq!(parsed_header.channel_mapping_table, Some(vec![0, 1]));
        }

        #[test]
        fn check_invalid_stream_count() {
            let mut header = create_valid_id_header();
            header[18] = 1; // Change channel mapping family to Vorbis
            header.extend_from_slice(&[0, 0, 0, 1]); // Invalid stream count (0)

            let result = IdentificationHeader::parse(Cursor::new(&header));
            assert!(matches!(result, Err(Error::InvalidStreamCount(0))));
        }

        #[test]
        fn check_invalid_coupled_count() {
            let mut header = create_valid_id_header();
            header[18] = 1; // Change channel mapping family to Vorbis
            header.extend_from_slice(&[2, 3, 0, 1]); // Invalid coupled count (3 > 2)

            let result = IdentificationHeader::parse(Cursor::new(&header));
            assert!(matches!(result, Err(Error::InvalidCoupledCount(3))));
        }

        #[test]
        fn check_invalid_channel_mapping() {
            let mut header = create_valid_id_header();
            header[18] = 1; // Change channel mapping family to Vorbis
            header[9] = 3;  // Set channel count to 3
            header.extend_from_slice(&[2, 1, 0, 1, 2]); // Stream count, coupled count, channel mapping

            let result = IdentificationHeader::parse(Cursor::new(&header));
            assert!(matches!(result, Err(Error::InvalidChannelMapping)));
        }
    }

    mod comment_header {
        use super::*;

        fn create_valid_comment_header() -> Vec<u8> {
            vec![
                0x4F, 0x70, 0x75, 0x73, 0x54, 0x61, 0x67, 0x73, // Magic signature "OpusTags"
                0x0B, 0x00, 0x00, 0x00, // Vendor string length
                0x53, 0x79, 0x6D, 0x70, 0x68, 0x6F,
                0x6E, 0x69, 0x61, 0x2D, 0x30, // Vendor string "Symphonia-0"
                0x02, 0x00, 0x00, 0x00, // User comment list length
                0x09, 0x00, 0x00, 0x00, // Comment 1 length
                0x41, 0x52, 0x54, 0x49, 0x53, 0x54, 0x3D, 0x4D, 0x65, // Comment 1 "ARTIST=Me"
                0x0A, 0x00, 0x00, 0x00, // Comment 2 length
                0x54, 0x49, 0x54, 0x4C, 0x45, 0x3D, 0x53, 0x6F, 0x6E, 0x67, // Comment 2 "TITLE=Song"
            ]
        }

        #[test]
        fn check_valid_comment_header_parsing() {
            let comment_header = create_valid_comment_header();
            let parsed_header = OpusCommentHeader::parse(Cursor::new(&comment_header)).unwrap();

            assert_eq!(parsed_header.vendor_string, "Symphonia-0");
            assert_eq!(parsed_header.user_comments.len(), 2);
            assert_eq!(parsed_header.user_comments[0], "ARTIST=Me");
            assert_eq!(parsed_header.user_comments[1], "TITLE=Song");
        }

        #[test]
        fn check_invalid_magic_signature() {
            let mut invalid_header = create_valid_comment_header();
            invalid_header[7] = b'S'; // Change last byte of signature

            let result = OpusCommentHeader::parse(Cursor::new(&invalid_header));
            assert!(matches!(result, Err(Error::InvalidMagicSignature)));
        }

        #[test]
        fn check_empty_vendor_string() {
            let mut header = create_valid_comment_header();
            header[8..12].copy_from_slice(&[0, 0, 0, 0]); // Set vendor string length to 0
            header.drain(12..23); // Remove vendor string

            let parsed_header = OpusCommentHeader::parse(Cursor::new(&header)).unwrap();
            assert_eq!(parsed_header.vendor_string, "");
        }

        #[test]
        fn check_no_user_comments() {
            let mut header = create_valid_comment_header();
            header[23..27].copy_from_slice(&[0, 0, 0, 0]); // Set user comment list length to 0
            header.truncate(27); // Remove user comments

            let parsed_header = OpusCommentHeader::parse(Cursor::new(&header)).unwrap();
            assert!(parsed_header.user_comments.is_empty());
        }

        #[test]
        fn check_invalid_utf8() {
            let mut header = create_valid_comment_header();
            header[12] = 0xFF; // Replace first byte of vendor string with invalid UTF-8

            let result = OpusCommentHeader::parse(Cursor::new(&header));
            assert!(matches!(result, Err(Error::Utf8(_))));
        }

        #[test]
        fn check_unexpected_eof() {
            let header = create_valid_comment_header();
            let truncated_header = &header[..header.len() - 1]; // Remove last byte

            let result = OpusCommentHeader::parse(Cursor::new(truncated_header));
            assert!(matches!(result, Err(Error::Io(_))));
        }
    }
}