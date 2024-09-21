//! All the heavy lifting is done symphonia-format-ogg which
//! already handles parsing the ID Header and Comment Header during OGG demuxing,
//! however it omits Input Sample Rate (it is just metadata),
//! and Output Gain that should be applied during decoding.
//! and The channel mapping table for complex setups (e.g., 5.1 surround)
//! The extra_data field in CodecParameters after demuxing ogg
//! is storing the raw Opus Identification Header packet that could be used by this module,
//! or it could be integrated into the demuxing process and be used during meatadata parsing.
//!
//! Opus header parsing implementation.
//!
//! This module provides functionality to parse Opus headers as specified in RFC 7845.
//! It includes structures and methods to handle both the identification header and the comment header.
//!
//! References:
//! - RFC 7845: Ogg Encapsulation for the Opus Audio Codec (https://tools.ietf.org/html/rfc7845)
//! - RFC 6716: Definition of the Opus Audio Codec (https://tools.ietf.org/html/rfc6716)
///
/// Packet Organization of opus stream
/// ```text
///
///         Page 0         Pages 1 ... n        Pages (n+1) ...
///      +------------+ +---+ +---+ ... +---+ +-----------+ +---------+ +--
///      |            | |   | |   |     |   | |           | |         | |
///      |+----------+| |+-----------------+| |+-------------------+ +-----
///      |||ID Header|| ||  Comment Header || ||Audio Data Packet 1| | ...
///      |+----------+| |+-----------------+| |+-------------------+ +-----
///      |            | |   | |   |     |   | |           | |         | |
///      +------------+ +---+ +---+ ... +---+ +-----------+ +---------+ +--
///      ^      ^                           ^
///      |      |                           |
///      |      |                           Mandatory Page Break
///      |      |
///      |      ID header is contained on a single page
///      |
///      'Beginning Of Stream'
///
///     Figure 1: Example Packet Organization for a Logical Ogg Opus Stream
///```
///
/// https://datatracker.ietf.org/doc/html/rfc7845#section-3

use symphonia_core::io::ReadBytes;

use std::convert::TryFrom;
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
pub struct ID {
    pub version: u8,
    pub channel_count: u8,
    pub pre_skip: u16,
    pub input_sample_rate: u32,
    pub output_gain: i16,
    pub mapping_family: ChannelMappingFamily,
    pub channel_mapping_table: Option<ChannelMappingTable>,
}


impl ID {
    const MAGIC_SIGNATURE: &'static [u8] = b"OpusHead";

    /// Parse Opus identification header from a byte stream.
    pub fn parse<R: ReadBytes>(mut reader: R) -> Result<Self, Error> {
        let _ = Self::parse_magic_signature(&mut reader)?;
        let version = Self::parse_version(&mut reader)?;
        let channel_count = Self::parse_channel_count(&mut reader)?;
        let pre_skip = Self::parse_pre_skip(&mut reader)?;
        let input_sample_rate = Self::parse_input_sample_rate(&mut reader)?;
        let output_gain = Self::parse_output_gain(&mut reader)?;
        let channel_mapping_family = Self::parse_channel_mapping_family(&mut reader)?;
        let channel_mapping_table = ChannelMappingTable::parse(&mut reader, channel_count, &channel_mapping_family)?;

        return Ok(Self {
            version,
            channel_count,
            pre_skip,
            input_sample_rate,
            output_gain,
            mapping_family: channel_mapping_family,
            channel_mapping_table,
        });
    }

    fn parse_magic_signature<R: ReadBytes>(reader: &mut R) -> Result<[u8; 8], Error> {
        let mut magic = [0u8; Self::MAGIC_SIGNATURE.len()];
        reader.read_buf_exact(&mut magic)?;
        if magic != Self::MAGIC_SIGNATURE {
            return Err(Error::InvalidMagicSignature);
        }

        return Ok(magic);
    }

    fn parse_version<R: ReadBytes>(reader: &mut R) -> Result<u8, Error> {
        let version = reader.read_u8()?;
        if version != 1 {
            return Err(Error::UnsupportedVersion(version));
        }

        return Ok(version);
    }

    fn parse_channel_count<R: ReadBytes>(reader: &mut R) -> Result<u8, Error> {
        let channel_count = reader.read_u8()?;
        if channel_count == 0 {
            return Err(Error::InvalidChannelCount(channel_count));
        }

        return Ok(channel_count);
    }

    fn parse_pre_skip<R: ReadBytes>(reader: &mut R) -> Result<u16, Error> {
        return Ok(reader.read_u16()?);
    }

    fn parse_input_sample_rate<R: ReadBytes>(reader: &mut R) -> Result<u32, Error> {
        return Ok(reader.read_u32()?);
    }

    fn parse_output_gain<R: ReadBytes>(reader: &mut R) -> Result<i16, Error> {
        return Ok(reader.read_i16()?);
    }

    fn parse_channel_mapping_family<R: ReadBytes>(reader: &mut R) -> Result<ChannelMappingFamily, Error> {
        return ChannelMappingFamily::try_from(reader.read_u8()?);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMappingFamily {
    /// Family 0: Mono or stereo (1 or 2 channels).
    Rtp,
    /// Family 1: Vorbis mapping (1-8 channels with Vorbis channel order).
    Vorbis,
    /// Reserved values for future use (2-254).
    Reserved(u8),
    /// Family 255: Reserved for undefined mappings (unidentified channels).
    Undefined,
}

impl TryFrom<u8> for ChannelMappingFamily {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Rtp),
            1 => Ok(Self::Vorbis),
            2..=254 => Ok(Self::Reserved(value)),
            255 => Ok(Self::Undefined),
            _ => unreachable!(),
        }
    }
}

/// Channel Mapping
///```text
///    An Ogg Opus stream allows mapping one number of Opus streams (N) to a
///    possibly larger number of decoded channels (M + N) to yet another
///    number of output channels (C), which might be larger or smaller than
///    the number of decoded channels.  The order and meaning of these
///    channels are defined by a channel mapping, which consists of the
///    'channel mapping family' octet and, for channel mapping families
///    other than family 0, a 'channel mapping table', as illustrated
///    in Figure 3.
///
///       0                   1                   2                   3
///       0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
///                                                      +-+-+-+-+-+-+-+-+
///                                                      | Stream Count  |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      | Coupled Count |              Channel Mapping...               :
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///
///                       Figure 3: Channel Mapping Table
///```
///
///https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1
///
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelMappingTable {
    pub stream_count: u8,
    pub coupled_count: u8,
    pub channel_mapping: Vec<u8>,
}

impl ChannelMappingTable {
    pub fn parse<R: ReadBytes>(
        reader: &mut R,
        channel_count: u8,
        channel_mapping_family: &ChannelMappingFamily,
    ) -> Result<Option<Self>, Error> {
        match channel_mapping_family {
            // MUST be omitted when the channel mapping family is 0, but is REQUIRED otherwise,
            // however Reserved and Undefined have no meaningful channel mapping.
            ChannelMappingFamily::Rtp
            | ChannelMappingFamily::Reserved(_)
            | ChannelMappingFamily::Undefined => Ok(None),

            ChannelMappingFamily::Vorbis => Self::parse_rtp(reader, channel_count).map(Some),
        }
    }

    fn parse_rtp<R: ReadBytes>(reader: &mut R, channel_count: u8) -> Result<Self, Error> {
        let stream_count = reader.read_u8()?;
        if stream_count == 0 {
            return Err(Error::InvalidStreamCount(stream_count));
        }

        let coupled_count = reader.read_u8()?;
        if coupled_count > stream_count {
            return Err(Error::InvalidCoupledCount(coupled_count));
        }

        let mut channel_mapping = vec![0u8; channel_count as usize];
        reader.read_buf_exact(&mut channel_mapping)?;

        for &i in &channel_mapping {
            if i != 255 && i >= (stream_count + coupled_count) {
                return Err(Error::InvalidChannelMapping);
            }
        }

        return Ok(Self { stream_count, coupled_count, channel_mapping });
    }

    pub fn interpret_channel(&self, i: u8) -> ChannelInterpretation {
        return ChannelInterpretation::new(self, i);
    }
}

#[derive(Debug, PartialEq)]
pub enum ChannelInterpretation {
    Silence,
    Stereo { stream: u8, is_right: bool },
    Mono { stream: u8 },
}

impl ChannelInterpretation {
    fn new(table: &ChannelMappingTable, i: u8) -> Self {
        return match i {
            255 => Self::Silence,
            i if i < 2 * table.coupled_count => Self::Stereo { stream: i / 2, is_right: i % 2 != 0 },
            i => Self::Mono { stream: i - table.coupled_count }
        };
    }
}

/// Comment Header
/// ```text
///       0                   1                   2                   3
///       0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      |      'O'      |      'p'      |      'u'      |      's'      |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      |      'T'      |      'a'      |      'g'      |      's'      |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      |                     Vendor String Length                      |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      |                                                               |
///      :                        Vendor String...                       :
///      |                                                               |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      |                   User Comment List Length                    |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      |                 User Comment #0 String Length                 |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      |                                                               |
///      :                   User Comment #0 String...                   :
///      |                                                               |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      |                 User Comment #1 String Length                 |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      :                                                               :
///
///                      Figure 10: Comment Header Packet
///```
///
/// https://datatracker.ietf.org/doc/html/rfc7845#section-5.2
#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    pub vendor_string: String,
    pub user_comments: Vec<String>,
}

impl Comment {
    const MAGIC_SIGNATURE: &'static [u8] = b"OpusTags";

    /// Parse Opus comment header from a byte stream.
    pub fn parse<R: ReadBytes>(mut reader: R) -> Result<Self, Error> {
        Self::validate_magic_signature(&mut reader)?;
        let vendor_string = Self::parse_vendor_string(&mut reader)?;
        let user_comments = Self::parse_user_comments(&mut reader)?;

        return Ok(Self {
            vendor_string,
            user_comments,
        });
    }

    fn validate_magic_signature<R: ReadBytes>(reader: &mut R) -> Result<(), Error> {
        let mut magic = [0u8; Self::MAGIC_SIGNATURE.len()];
        reader.read_buf_exact(&mut magic)?;
        if magic != Self::MAGIC_SIGNATURE {
            return Err(Error::InvalidMagicSignature);
        }

        return Ok(());
    }

    fn parse_vendor_string<R: ReadBytes>(reader: &mut R) -> Result<String, Error> {
        let vendor_length = reader.read_u32()? as usize;
        let mut vendor_string = vec![0u8; vendor_length];
        reader.read_buf_exact(&mut vendor_string)?;

        return Ok(String::from_utf8(vendor_string)?);
    }

    fn parse_user_comments<R: ReadBytes>(reader: &mut R) -> Result<Vec<String>, Error> {
        let user_comment_list_length = reader.read_u32()? as usize;
        let mut user_comments = Vec::with_capacity(user_comment_list_length);

        for _ in 0..user_comment_list_length {
            let comment = Self::parse_single_comment(reader)?;
            user_comments.push(comment);
        }

        return Ok(user_comments);
    }

    fn parse_single_comment<R: ReadBytes>(reader: &mut R) -> Result<String, Error> {
        let comment_length = reader.read_u32()? as usize;
        let mut comment = vec![0u8; comment_length];
        reader.read_buf_exact(&mut comment)?;

        return Ok(String::from_utf8(comment)?);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod id {
        use symphonia_core::io::BufReader;
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
        fn valid_id_header_parsing() {
            let id_header = create_valid_id_header();
            let reader = BufReader::new(&id_header);
            let parsed_header = ID::parse(reader).unwrap();

            assert_eq!(parsed_header.version, 1);
            assert_eq!(parsed_header.channel_count, 2);
            assert_eq!(parsed_header.pre_skip, 312);
            assert_eq!(parsed_header.input_sample_rate, 48000);
            assert_eq!(parsed_header.output_gain, 0);
            assert_eq!(parsed_header.mapping_family, ChannelMappingFamily::Rtp);
            assert!(parsed_header.channel_mapping_table.is_none());
        }

        #[test]
        fn invalid_magic_signature() {
            let mut invalid_header = create_valid_id_header();
            invalid_header[7] = b's'; // Change last byte of signature

            let reader = BufReader::new(&invalid_header);
            let result = ID::parse(reader);
            assert!(matches!(result, Err(Error::InvalidMagicSignature)));
        }

        #[test]
        fn unsupported_version() {
            let mut invalid_header = create_valid_id_header();
            invalid_header[8] = 2; // Change version to 2

            let reader = BufReader::new(&invalid_header);
            let result = ID::parse(reader);
            assert!(matches!(result, Err(Error::UnsupportedVersion(2))));
        }

        #[test]
        fn invalid_channel_count() {
            let mut invalid_header = create_valid_id_header();
            invalid_header[9] = 0; // Change channel count to 0

            let reader = BufReader::new(&invalid_header);
            let result = ID::parse(reader);
            assert!(matches!(result, Err(Error::InvalidChannelCount(0))));
        }
        #[test]
        fn family_one_channel_mapping() {
            let mut header = create_valid_id_header();
            header[18] = 1; // Change channel mapping family to Vorbis
            let (stream_count, coupled_count, channel_mapping) = (2, 1, vec![0, 1]);
            header.extend_from_slice(&[stream_count, coupled_count, channel_mapping[0], channel_mapping[1]]);

            let reader = BufReader::new(&header);
            let parsed_header = ID::parse(reader).unwrap();
            assert_eq!(parsed_header.mapping_family, ChannelMappingFamily::Vorbis);

            let table = ChannelMappingTable { stream_count, coupled_count, channel_mapping };
            assert_eq!(parsed_header.channel_mapping_table, Some(table));
        }

        #[test]
        fn invalid_stream_count() {
            let mut header = create_valid_id_header();
            header[18] = 1; // Change channel mapping family to Vorbis
            header.extend_from_slice(&[0, 0, 0, 1]); // Invalid stream count (0)

            let reader = BufReader::new(&header);
            let result = ID::parse(reader);
            assert!(matches!(result, Err(Error::InvalidStreamCount(0))));
        }

        #[test]
        fn invalid_coupled_count() {
            let mut header = create_valid_id_header();
            header[18] = 1; // Change channel mapping family to Vorbis
            header.extend_from_slice(&[2, 3, 0, 1]); // Invalid coupled count (3 > 2)

            let reader = BufReader::new(&header);
            let result = ID::parse(reader);
            assert!(matches!(result, Err(Error::InvalidCoupledCount(3))));
        }

        #[test]
        fn valid_channel_mapping() {
            let mut header = create_valid_id_header();
            header[18] = 1; // Change channel mapping family to Vorbis
            header[9] = 3;  // Set channel count to 3
            header.extend_from_slice(&[2, 1, 0, 1, 2]); // Stream count, coupled count, channel mapping

            let reader = BufReader::new(&header);
            let result = ID::parse(reader);
            assert!(result.is_ok());

            if let Ok(parsed) = result {
                assert_eq!(parsed.channel_count, 3);
                assert_eq!(parsed.mapping_family, ChannelMappingFamily::Vorbis);
                assert_eq!(parsed.channel_mapping_table, Some(ChannelMappingTable {
                    stream_count: 2,
                    coupled_count: 1,
                    channel_mapping: vec![0, 1, 2],
                }));
            }
        }
        #[test]
        fn invalid_channel_mapping() {
            let mut header = create_valid_id_header();
            header[18] = 1; // Change channel mapping family to Vorbis
            header[9] = 3;  // Set channel count to 3
            header.extend_from_slice(&[2, 1, 0, 1, 3]); // Stream count, coupled count, channel mapping

            let reader = BufReader::new(&header);
            let result = ID::parse(reader);
            assert!(matches!(result, Err(Error::InvalidChannelMapping)));
        }
    }

    mod comment {
        use symphonia_core::io::BufReader;
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
        fn valid_comment_header_parsing() {
            let comment_header = create_valid_comment_header();
            let reader = BufReader::new(&comment_header);
            let parsed_header = Comment::parse(reader).unwrap();

            assert_eq!(parsed_header.vendor_string, "Symphonia-0");
            assert_eq!(parsed_header.user_comments.len(), 2);
            assert_eq!(parsed_header.user_comments[0], "ARTIST=Me");
            assert_eq!(parsed_header.user_comments[1], "TITLE=Song");
        }

        #[test]
        fn invalid_magic_signature() {
            let mut invalid_header = create_valid_comment_header();
            invalid_header[7] = b'S'; // Change last byte of signature

            let reader = BufReader::new(&invalid_header);
            let result = Comment::parse(reader);
            assert!(matches!(result, Err(Error::InvalidMagicSignature)));
        }

        #[test]
        fn empty_vendor_string() {
            let mut header = create_valid_comment_header();
            header[8..12].copy_from_slice(&[0, 0, 0, 0]); // Set vendor string length to 0
            header.drain(12..23); // Remove vendor string

            let reader = BufReader::new(&header);
            let parsed_header = Comment::parse(reader).unwrap();
            assert_eq!(parsed_header.vendor_string, "");
        }

        #[test]
        fn no_user_comments() {
            let mut header = create_valid_comment_header();
            header[23..27].copy_from_slice(&[0, 0, 0, 0]); // Set user comment list length to 0
            header.truncate(27); // Remove user comments

            let reader = BufReader::new(&header);
            let parsed_header = Comment::parse(reader).unwrap();
            assert!(parsed_header.user_comments.is_empty());
        }

        #[test]
        fn invalid_utf8() {
            let mut header = create_valid_comment_header();
            header[12] = 0xFF; // Replace first byte of vendor string with invalid UTF-8

            let reader = BufReader::new(&header);
            let result = Comment::parse(reader);
            assert!(matches!(result, Err(Error::Utf8(_))));
        }

        #[test]
        fn unexpected_eof() {
            let header = create_valid_comment_header();
            let truncated_header = &header[..header.len() - 1]; // Remove last byte

            let reader = BufReader::new(truncated_header);
            let result = Comment::parse(reader);
            assert!(matches!(result, Err(Error::Io(_))));
        }
    }
}