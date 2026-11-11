//! Ogg Opus header packets: the identification header `OpusHead` and its channel mapping
//! ([RFC 7845 Section 5](<https://datatracker.ietf.org/doc/html/rfc7845#section-5>)).
//!
//! A logical Ogg Opus stream opens with the identification header, followed by the comment
//! header and then the audio data packets
//! ([RFC 7845 Section 3](<https://datatracker.ietf.org/doc/html/rfc7845#section-3>)). The Ogg
//! demuxer reads both header packets and hands the identification header to the decoder as codec
//! extra data, so this module parses that packet: the output channel count, pre-skip, output gain
//! and the channel mapping that routes the decoded channels of a multistream packet to output
//! channels. The comment header `OpusTags` carries only metadata and is not parsed here.
//!
//! ```text
//!         Page 0         Pages 1 ... n        Pages (n+1) ...
//!      +------------+ +---+ +---+ ... +---+ +-----------+ +---------+ +--
//!      |            | |   | |   |     |   | |           | |         | |
//!      |+----------+| |+-----------------+| |+-------------------+ +-----
//!      |||ID Header|| ||  Comment Header || ||Audio Data Packet 1| | ...
//!      |+----------+| |+-----------------+| |+-------------------+ +-----
//!      |            | |   | |   |     |   | |           | |         | |
//!      +------------+ +---+ +---+ ... +---+ +-----------+ +---------+ +--
//!      ^      ^                           ^
//!      |      |                           |
//!      |      |                           Mandatory Page Break
//!      |      |
//!      |      ID header is contained on a single page
//!      |
//!      'Beginning Of Stream'
//!
//!     Figure 1: Example Packet Organization for a Logical Ogg Opus Stream
//! ```

use symphonia_core::audio::{Channels, Position};
use symphonia_core::io::ReadBytes;

use std::convert::TryFrom;
use thiserror::Error;

/// Highest `Version` field value this parser accepts
/// ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)).
///
/// The upper four bits of the version are the major version and the lower four bits the minor
/// version. An implementation SHOULD accept any stream with a version number of 15 or less and
/// assume that a stream with a greater version is incompatible. This also admits version 0, which
/// the ISO Base Media File Format `dOps` box uses for the same fields.
const MAX_SUPPORTED_VERSION: u8 = 15;
/// Channel mapping entry that marks an output channel as silent
/// ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)).
const SILENT_CHANNEL: u8 = 255;

/// Ways an identification header can violate
/// [RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>).
#[derive(Debug, Error)]
pub(crate) enum Error {
    /// The packet does not start with the magic signature `OpusHead`.
    #[error("Invalid magic signature")]
    InvalidMagicSignature,

    /// The version field is above [`MAX_SUPPORTED_VERSION`]: its major version is not 0.
    #[error("Unsupported version: {0}")]
    UnsupportedVersion(u8),

    /// The output channel count is zero or outside the range of its channel mapping family.
    #[error("Invalid channel count: {0}")]
    InvalidChannelCount(u8),

    /// The channel mapping table signals zero streams.
    #[error("Invalid stream count: {0}")]
    InvalidStreamCount(u8),

    /// The channel mapping table signals more coupled streams than streams.
    #[error("Invalid coupled count: {0}")]
    InvalidCoupledCount(u8),

    /// The stream count plus the coupled stream count does not fit in 255 decoded channels.
    #[error("Stream count plus coupled count exceeds 255")]
    ExcessiveDecodedChannels,

    /// A channel mapping entry names a decoded channel that does not exist.
    #[error("Invalid channel mapping")]
    InvalidChannelMapping,

    /// The packet ended before a field was complete.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// The identification header `OpusHead`, the first packet of an Ogg Opus stream
/// ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)).
///
/// ```text
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
/// ```
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IdentificationHeader {
    /// The `Version` field, 0 to 15 for an accepted header; 1 for RFC 7845 streams.
    pub(crate) version: u8,
    /// The `Output Channel Count` field `C`, the number of output channels.
    pub(crate) channel_count: u8,
    /// The `Pre-skip` field: samples at 48 kHz to discard from the decoder output at the start
    /// of playback.
    pub(crate) pre_skip: u16,
    /// The `Input Sample Rate` field in Hz, the rate of the original input. It is metadata only
    /// and does not change the decoder output rate.
    pub(crate) input_sample_rate: u32,
    /// The `Output Gain` field, a Q7.8 gain in dB applied to the decoded output.
    pub(crate) output_gain: i16,
    /// The `Channel Mapping Family` field.
    pub(crate) mapping_family: ChannelMappingFamily,
    /// The channel mapping table, present for every family except family 0.
    pub(crate) channel_mapping_table: Option<ChannelMappingTable>,
}

impl IdentificationHeader {
    /// The `Magic Signature` field that opens the packet.
    const MAGIC_SIGNATURE: &'static [u8] = b"OpusHead";

    /// Parses an identification header from `reader`, validating every field against
    /// [RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>).
    pub(crate) fn parse<R: ReadBytes>(mut reader: R) -> Result<Self, Error> {
        Self::parse_magic_signature(&mut reader)?;
        let version = Self::parse_version(&mut reader)?;
        let channel_count = Self::parse_channel_count(&mut reader)?;
        let pre_skip = reader.read_u16()?;
        let input_sample_rate = reader.read_u32()?;
        let output_gain = reader.read_i16()?;
        let mapping_family = ChannelMappingFamily::try_from(reader.read_u8()?)?;
        Self::check_channel_count_for_family(channel_count, &mapping_family)?;
        let channel_mapping_table =
            ChannelMappingTable::parse(&mut reader, channel_count, &mapping_family)?;

        return Ok(Self {
            version,
            channel_count,
            pre_skip,
            input_sample_rate,
            output_gain,
            mapping_family,
            channel_mapping_table,
        });
    }

    /// The output channel layout of the channel mapping family
    /// ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)):
    /// mono or left/right stereo for family 0
    /// ([Section 5.1.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1.1>)), the
    /// Vorbis channel order for family 1
    /// ([Section 5.1.1.2](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1.2>)), and
    /// discrete channels without defined positions for every other family
    /// ([Section 5.1.1.3](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1.3>),
    /// [Section 5.1.1.4](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1.4>)).
    ///
    /// ```text
    ///      Channels | Vorbis channel order (family 1)
    ///      ---------+---------------------------------------------------------------
    ///          1    | mono
    ///          2    | left, right
    ///          3    | left, center, right
    ///          4    | front left, front right, rear left, rear right
    ///          5    | front left, center, front right, rear left, rear right
    ///          6    | front left, center, front right, rear left, rear right, LFE
    ///          7    | front left, center, front right, side left, side right, rear
    ///               | center, LFE
    ///          8    | front left, center, front right, side left, side right, rear
    ///               | left, rear right, LFE
    /// ```
    pub(crate) fn channels(&self) -> Channels {
        use ChannelMappingFamily::{Rtp, Vorbis};

        let positions = match (self.mapping_family, self.channel_count) {
            (Rtp | Vorbis, 1) => Position::FRONT_LEFT,
            (Rtp | Vorbis, 2) => Position::FRONT_LEFT | Position::FRONT_RIGHT,
            (Vorbis, 3) => Position::FRONT_LEFT | Position::FRONT_CENTER | Position::FRONT_RIGHT,
            (Vorbis, 4) => {
                Position::FRONT_LEFT
                    | Position::FRONT_RIGHT
                    | Position::REAR_LEFT
                    | Position::REAR_RIGHT
            }
            (Vorbis, 5) => {
                Position::FRONT_LEFT
                    | Position::FRONT_CENTER
                    | Position::FRONT_RIGHT
                    | Position::REAR_LEFT
                    | Position::REAR_RIGHT
            }
            (Vorbis, 6) => {
                Position::FRONT_LEFT
                    | Position::FRONT_CENTER
                    | Position::FRONT_RIGHT
                    | Position::REAR_LEFT
                    | Position::REAR_RIGHT
                    | Position::LFE1
            }
            (Vorbis, 7) => {
                Position::FRONT_LEFT
                    | Position::FRONT_CENTER
                    | Position::FRONT_RIGHT
                    | Position::SIDE_LEFT
                    | Position::SIDE_RIGHT
                    | Position::REAR_CENTER
                    | Position::LFE1
            }
            (Vorbis, 8) => {
                Position::FRONT_LEFT
                    | Position::FRONT_CENTER
                    | Position::FRONT_RIGHT
                    | Position::SIDE_LEFT
                    | Position::SIDE_RIGHT
                    | Position::REAR_LEFT
                    | Position::REAR_RIGHT
                    | Position::LFE1
            }
            _ => return Channels::Discrete(u16::from(self.channel_count)),
        };
        return Channels::Positioned(positions);
    }

    /// Reads the 8-byte magic signature and rejects anything other than `OpusHead`.
    fn parse_magic_signature<R: ReadBytes>(reader: &mut R) -> Result<(), Error> {
        let mut magic = [0u8; Self::MAGIC_SIGNATURE.len()];
        reader.read_buf_exact(&mut magic)?;
        if magic != Self::MAGIC_SIGNATURE {
            return Err(Error::InvalidMagicSignature);
        }
        return Ok(());
    }

    /// Reads the version and rejects every version above [`MAX_SUPPORTED_VERSION`].
    fn parse_version<R: ReadBytes>(reader: &mut R) -> Result<u8, Error> {
        let version = reader.read_u8()?;
        return (version <= MAX_SUPPORTED_VERSION)
            .then_some(version)
            .ok_or(Error::UnsupportedVersion(version));
    }

    /// Reads the output channel count, which MUST NOT be zero.
    fn parse_channel_count<R: ReadBytes>(reader: &mut R) -> Result<u8, Error> {
        let channel_count = reader.read_u8()?;
        if channel_count == 0 {
            return Err(Error::InvalidChannelCount(channel_count));
        }
        return Ok(channel_count);
    }

    /// Checks the channel count against the family's allowed range: 1 or 2 for family 0
    /// ([RFC 7845 Section 5.1.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1.1>)),
    /// 1 to 8 for family 1
    /// ([RFC 7845 Section 5.1.1.2](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1.2>)),
    /// and any non-zero count otherwise.
    fn check_channel_count_for_family(
        channel_count: u8,
        family: &ChannelMappingFamily,
    ) -> Result<(), Error> {
        let allowed = match family {
            ChannelMappingFamily::Rtp => 1..=2,
            ChannelMappingFamily::Vorbis => 1..=8,
            ChannelMappingFamily::Reserved(_) | ChannelMappingFamily::Undefined => 1..=u8::MAX,
        };
        if !allowed.contains(&channel_count) {
            return Err(Error::InvalidChannelCount(channel_count));
        }
        return Ok(());
    }
}

/// The `Channel Mapping Family` field: how the decoded channels map to output channels and
/// what each output channel means
/// ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelMappingFamily {
    /// Family 0, the RTP mapping: one stream carrying mono or left/right stereo, with no
    /// channel mapping table
    /// ([RFC 7845 Section 5.1.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1.1>)).
    Rtp,
    /// Family 1: 1 to 8 channels in Vorbis channel order, from mono up to 7.1 surround
    /// ([RFC 7845 Section 5.1.1.2](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1.2>)).
    Vorbis,
    /// Families 2 to 254, reserved; a decoder treats them as family 255
    /// ([RFC 7845 Section 5.1.1.4](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1.4>)).
    Reserved(u8),
    /// Family 255: any number of channels with no defined meaning or order
    /// ([RFC 7845 Section 5.1.1.3](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1.3>)).
    Undefined,
}

impl TryFrom<u8> for ChannelMappingFamily {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        return match value {
            0 => Ok(Self::Rtp),
            1 => Ok(Self::Vorbis),
            2..=254 => Ok(Self::Reserved(value)),
            255 => Ok(Self::Undefined),
        };
    }
}

/// The channel mapping table: `N` streams, `M` of them coupled stereo, decoded into `N + M`
/// channels and routed to the `C` output channels
/// ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)).
///
/// ```text
///       0                   1                   2                   3
///       0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
///                                                      +-+-+-+-+-+-+-+-+
///                                                      | Stream Count  |
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///      | Coupled Count |              Channel Mapping...               :
///      +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
///
///                       Figure 3: Channel Mapping Table
/// ```
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChannelMappingTable {
    /// The `Stream Count` field `N`, the number of Opus streams in each Ogg packet.
    pub(crate) stream_count: u8,
    /// The `Coupled Stream Count` field `M`, the number of streams that decode to two channels.
    pub(crate) coupled_count: u8,
    /// The `Channel Mapping` field: for each output channel, the decoded channel it plays, or
    /// 255 for silence.
    pub(crate) channel_mapping: Vec<u8>,
}

impl ChannelMappingTable {
    /// Reads the table when `channel_mapping_family` requires one: it MUST be omitted for
    /// family 0 and is REQUIRED for every other family
    /// ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)).
    pub(crate) fn parse<R: ReadBytes>(
        reader: &mut R,
        channel_count: u8,
        channel_mapping_family: &ChannelMappingFamily,
    ) -> Result<Option<Self>, Error> {
        return match channel_mapping_family {
            ChannelMappingFamily::Rtp => Ok(None),
            ChannelMappingFamily::Vorbis
            | ChannelMappingFamily::Reserved(_)
            | ChannelMappingFamily::Undefined => Self::parse_table(reader, channel_count).map(Some),
        };
    }

    /// Reads the stream counts and one mapping entry per output channel, rejecting an entry
    /// that names a decoded channel beyond `N + M`.
    fn parse_table<R: ReadBytes>(reader: &mut R, channel_count: u8) -> Result<Self, Error> {
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

        let decoded_channels =
            stream_count.checked_add(coupled_count).ok_or(Error::ExcessiveDecodedChannels)?;
        let out_of_range = channel_mapping.iter().any(|&decoded_channel| {
            decoded_channel != SILENT_CHANNEL && decoded_channel >= decoded_channels
        });
        if out_of_range {
            return Err(Error::InvalidChannelMapping);
        }

        return Ok(Self { stream_count, coupled_count, channel_mapping });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod id {
        use super::*;
        use symphonia_core::io::BufReader;

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
            let parsed_header = IdentificationHeader::parse(reader).unwrap();

            assert_eq!(parsed_header.version, 1);
            assert_eq!(parsed_header.channel_count, 2);
            assert_eq!(parsed_header.pre_skip, 312);
            assert_eq!(parsed_header.input_sample_rate, 48000);
            assert_eq!(parsed_header.output_gain, 0);
            assert_eq!(parsed_header.mapping_family, ChannelMappingFamily::Rtp);
            assert!(parsed_header.channel_mapping_table.is_none());
        }

        /// A header that ends inside its fixed fields reports the failed read
        /// ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)).
        #[test]
        fn truncated_header_reports_the_read_error() {
            let header = create_valid_id_header();
            let result = IdentificationHeader::parse(BufReader::new(&header[..12]));
            let eof = std::io::ErrorKind::UnexpectedEof;
            assert!(matches!(result, Err(Error::Io(ref error)) if error.kind() == eof));
        }

        #[test]
        fn invalid_magic_signature() {
            let mut invalid_header = create_valid_id_header();
            invalid_header[7] = b's'; // Change last byte of signature

            let reader = BufReader::new(&invalid_header);
            let result = IdentificationHeader::parse(reader);
            assert!(matches!(result, Err(Error::InvalidMagicSignature)));
        }

        /// RFC 7845 Section 5.1.1: family 0 and family 1 carry positioned channels in Vorbis
        /// order; families 2 to 255 carry discrete channels without defined positions.
        #[test]
        fn channel_layout_follows_the_mapping_family() {
            let mut mono = create_valid_id_header();
            mono[9] = 1;
            let parsed = IdentificationHeader::parse(BufReader::new(&mono)).unwrap();
            assert_eq!(parsed.channels(), Channels::Positioned(Position::FRONT_LEFT));

            let stereo = create_valid_id_header();
            let parsed = IdentificationHeader::parse(BufReader::new(&stereo)).unwrap();
            let left_right = Position::FRONT_LEFT | Position::FRONT_RIGHT;
            assert_eq!(parsed.channels(), Channels::Positioned(left_right));

            let mut surround = create_valid_id_header();
            surround[9] = 6;
            surround[18] = 1;
            surround.extend_from_slice(&[4, 2, 0, 4, 1, 2, 3, 5]);
            let parsed = IdentificationHeader::parse(BufReader::new(&surround)).unwrap();
            let layout_5_1 = Position::FRONT_LEFT
                | Position::FRONT_CENTER
                | Position::FRONT_RIGHT
                | Position::REAR_LEFT
                | Position::REAR_RIGHT
                | Position::LFE1;
            assert_eq!(parsed.channels(), Channels::Positioned(layout_5_1));

            let mut undefined = create_valid_id_header();
            undefined[18] = 255;
            undefined.extend_from_slice(&[2, 0, 0, 1]);
            let parsed = IdentificationHeader::parse(BufReader::new(&undefined)).unwrap();
            assert_eq!(parsed.channels(), Channels::Discrete(2));
        }

        /// RFC 7845 Section 5.1: an implementation SHOULD accept any stream with a version number
        /// of 15 or less. Version 0 is what the ISO Base Media File Format `dOps` box carries.
        #[test]
        fn versions_up_to_15_are_accepted() {
            for version in [0, 1, 2, 15] {
                let mut header = create_valid_id_header();
                header[8] = version;
                let parsed = IdentificationHeader::parse(BufReader::new(&header)).unwrap();
                assert_eq!(parsed.version, version);
            }
        }

        /// RFC 7845 Section 5.1: a version above 15 has a different major version and the stream
        /// is assumed to be incompatible.
        #[test]
        fn versions_above_15_are_rejected() {
            for version in [16, 0x1f, 0xff] {
                let mut header = create_valid_id_header();
                header[8] = version;
                let result = IdentificationHeader::parse(BufReader::new(&header));
                assert!(matches!(result, Err(Error::UnsupportedVersion(v)) if v == version));
            }
        }

        #[test]
        fn invalid_channel_count() {
            let mut invalid_header = create_valid_id_header();
            invalid_header[9] = 0; // Change channel count to 0

            let reader = BufReader::new(&invalid_header);
            let result = IdentificationHeader::parse(reader);
            assert!(matches!(result, Err(Error::InvalidChannelCount(0))));
        }
        #[test]
        fn family_one_channel_mapping() {
            let mut header = create_valid_id_header();
            header[18] = 1; // Change channel mapping family to Vorbis
            let (stream_count, coupled_count, channel_mapping) = (2, 1, vec![0, 1]);
            header.extend_from_slice(&[
                stream_count,
                coupled_count,
                channel_mapping[0],
                channel_mapping[1],
            ]);

            let reader = BufReader::new(&header);
            let parsed_header = IdentificationHeader::parse(reader).unwrap();
            assert_eq!(parsed_header.mapping_family, ChannelMappingFamily::Vorbis);

            let table = ChannelMappingTable { stream_count, coupled_count, channel_mapping };
            assert_eq!(parsed_header.channel_mapping_table, Some(table));
        }

        /// RFC 7845 Section 5.1.1: `N + M` decoded channels must fit in a byte; a crafted header
        /// must produce an error, never an arithmetic overflow.
        #[test]
        fn stream_and_coupled_counts_summing_past_255_are_rejected() {
            let mut header = create_valid_id_header();
            header[18] = 1;
            header.extend_from_slice(&[200, 100, 0, 1]);
            let result = IdentificationHeader::parse(BufReader::new(&header));
            assert!(matches!(result, Err(Error::ExcessiveDecodedChannels)));
        }

        /// RFC 7845 Section 5.1.1.3: family 255 carries a channel mapping table like every
        /// family other than 0.
        #[test]
        fn family_255_carries_a_channel_mapping_table() {
            let mut header = create_valid_id_header();
            header[18] = 255;
            header.extend_from_slice(&[2, 0, 0, 1]);
            let parsed = IdentificationHeader::parse(BufReader::new(&header)).unwrap();
            assert_eq!(parsed.mapping_family, ChannelMappingFamily::Undefined);
            let table = ChannelMappingTable {
                stream_count: 2,
                coupled_count: 0,
                channel_mapping: vec![0, 1],
            };
            assert_eq!(parsed.channel_mapping_table, Some(table));
        }

        /// RFC 7845 Section 5.1.1.1 (family 0: 1 or 2 channels) and Section 5.1.1.2 (family 1:
        /// 1 to 8 channels).
        #[test]
        fn channel_count_outside_the_family_range_is_rejected() {
            let mut family_zero = create_valid_id_header();
            family_zero[9] = 3;
            let result = IdentificationHeader::parse(BufReader::new(&family_zero));
            assert!(matches!(result, Err(Error::InvalidChannelCount(3))));

            let mut family_one = create_valid_id_header();
            family_one[9] = 9;
            family_one[18] = 1;
            family_one.extend_from_slice(&[5, 4, 0, 1, 2, 3, 4, 5, 6, 7, 8]);
            let result = IdentificationHeader::parse(BufReader::new(&family_one));
            assert!(matches!(result, Err(Error::InvalidChannelCount(9))));
        }

        #[test]
        fn invalid_stream_count() {
            let mut header = create_valid_id_header();
            header[18] = 1; // Change channel mapping family to Vorbis
            header.extend_from_slice(&[0, 0, 0, 1]); // Invalid stream count (0)

            let reader = BufReader::new(&header);
            let result = IdentificationHeader::parse(reader);
            assert!(matches!(result, Err(Error::InvalidStreamCount(0))));
        }

        #[test]
        fn invalid_coupled_count() {
            let mut header = create_valid_id_header();
            header[18] = 1; // Change channel mapping family to Vorbis
            header.extend_from_slice(&[2, 3, 0, 1]); // Invalid coupled count (3 > 2)

            let reader = BufReader::new(&header);
            let result = IdentificationHeader::parse(reader);
            assert!(matches!(result, Err(Error::InvalidCoupledCount(3))));
        }

        #[test]
        fn valid_channel_mapping() {
            let mut header = create_valid_id_header();
            header[18] = 1; // Change channel mapping family to Vorbis
            header[9] = 3; // Set channel count to 3
            header.extend_from_slice(&[2, 1, 0, 1, 2]); // Stream count, coupled count, channel mapping

            let reader = BufReader::new(&header);
            let result = IdentificationHeader::parse(reader);
            assert!(result.is_ok());

            if let Ok(parsed) = result {
                assert_eq!(parsed.channel_count, 3);
                assert_eq!(parsed.mapping_family, ChannelMappingFamily::Vorbis);
                assert_eq!(
                    parsed.channel_mapping_table,
                    Some(ChannelMappingTable {
                        stream_count: 2,
                        coupled_count: 1,
                        channel_mapping: vec![0, 1, 2],
                    })
                );
            }
        }
        #[test]
        fn invalid_channel_mapping() {
            let mut header = create_valid_id_header();
            header[18] = 1; // Change channel mapping family to Vorbis
            header[9] = 3; // Set channel count to 3
            header.extend_from_slice(&[2, 1, 0, 1, 3]); // Stream count, coupled count, channel mapping

            let reader = BufReader::new(&header);
            let result = IdentificationHeader::parse(reader);
            assert!(matches!(result, Err(Error::InvalidChannelMapping)));
        }
    }
}
