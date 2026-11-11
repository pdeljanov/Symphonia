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
use symphonia_core::errors::Error as SymphoniaError;
use symphonia_core::io::ReadBytes;

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
pub(crate) const SILENT_CHANNEL: u8 = 255;
/// Units of the Q7.8 `Output Gain` field per decibel
/// ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)).
const OUTPUT_GAIN_UNITS_PER_DB: f64 = 256.0;
/// Decibels per decade of amplitude: a gain of `g` dB scales samples by `10^(g / 20)`.
const DB_PER_AMPLITUDE_DECADE: f64 = 20.0;

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

impl Error {
    /// The message the Symphonia error carries for this header error.
    fn message(&self) -> &'static str {
        return match self {
            Self::InvalidMagicSignature => "opus: OpusHead has an invalid magic signature",
            Self::UnsupportedVersion(_) => "opus: unsupported OpusHead version",
            Self::InvalidChannelCount(_) => "opus: invalid OpusHead channel count",
            Self::InvalidStreamCount(_) => "opus: invalid OpusHead stream count",
            Self::InvalidCoupledCount(_) => "opus: invalid OpusHead coupled stream count",
            Self::ExcessiveDecodedChannels => "opus: OpusHead streams exceed 255 decoded channels",
            Self::InvalidChannelMapping => "opus: invalid OpusHead channel mapping",
            Self::Io(_) => "opus: truncated OpusHead",
        };
    }
}

impl From<Error> for SymphoniaError {
    fn from(error: Error) -> Self {
        return SymphoniaError::DecodeError(error.message());
    }
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
    /// The `Output Gain` field applied to the decoded output.
    pub(crate) output_gain: OutputGain,
    /// The `Channel Mapping Family` field with its channel mapping table.
    pub(crate) channel_mapping: ChannelMapping,
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
        let output_gain = OutputGain(reader.read_i16()?);
        let channel_mapping = ChannelMapping::parse(&mut reader, channel_count)?;

        return Ok(Self {
            version,
            channel_count,
            pre_skip,
            input_sample_rate,
            output_gain,
            channel_mapping,
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
        use ChannelMapping::{Rtp, Vorbis};

        let positions = match (&self.channel_mapping, self.channel_count) {
            (Rtp | Vorbis(_), 1) => Position::FRONT_LEFT,
            (Rtp | Vorbis(_), 2) => Position::FRONT_LEFT | Position::FRONT_RIGHT,
            (Vorbis(_), 3) => Position::FRONT_LEFT | Position::FRONT_CENTER | Position::FRONT_RIGHT,
            (Vorbis(_), 4) => {
                Position::FRONT_LEFT
                    | Position::FRONT_RIGHT
                    | Position::REAR_LEFT
                    | Position::REAR_RIGHT
            }
            (Vorbis(_), 5) => {
                Position::FRONT_LEFT
                    | Position::FRONT_CENTER
                    | Position::FRONT_RIGHT
                    | Position::REAR_LEFT
                    | Position::REAR_RIGHT
            }
            (Vorbis(_), 6) => {
                Position::FRONT_LEFT
                    | Position::FRONT_CENTER
                    | Position::FRONT_RIGHT
                    | Position::REAR_LEFT
                    | Position::REAR_RIGHT
                    | Position::LFE1
            }
            (Vorbis(_), 7) => {
                Position::FRONT_LEFT
                    | Position::FRONT_CENTER
                    | Position::FRONT_RIGHT
                    | Position::SIDE_LEFT
                    | Position::SIDE_RIGHT
                    | Position::REAR_CENTER
                    | Position::LFE1
            }
            (Vorbis(_), 8) => {
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
        return (channel_count != 0)
            .then_some(channel_count)
            .ok_or(Error::InvalidChannelCount(channel_count));
    }
}

/// The `Output Gain` field: a Q7.8 gain in dB applied to the decoded output
/// ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct OutputGain(pub(crate) i16);

impl OutputGain {
    /// Unity gain, the value a stream without a header decodes with.
    pub(crate) const UNITY: Self = Self(0);

    /// The raw Q7.8 dB value of the field.
    pub(crate) const fn q7_8_db(self) -> i16 {
        return self.0;
    }

    /// Linear scale of the gain, `10^(gain / (20 * 256))`, the formula RFC 7845 gives for
    /// applying it; `None` at unity gain so the output can be copied unscaled.
    pub(crate) fn scale(self) -> Option<f32> {
        let gain_db = f64::from(self.0) / OUTPUT_GAIN_UNITS_PER_DB;
        return (self != Self::UNITY)
            .then(|| 10.0_f64.powf(gain_db / DB_PER_AMPLITUDE_DECADE) as f32);
    }
}

/// The `Channel Mapping Family` field together with the channel mapping table that every family
/// other than 0 carries: how the decoded channels map to output channels and what each output
/// channel means
/// ([RFC 7845 Section 5.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1>)).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ChannelMapping {
    /// Family 0, the RTP mapping: one stream carrying mono or left/right stereo, with no
    /// channel mapping table
    /// ([RFC 7845 Section 5.1.1.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1.1>)).
    Rtp,
    /// Family 1: 1 to 8 channels in Vorbis channel order, from mono up to 7.1 surround
    /// ([RFC 7845 Section 5.1.1.2](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1.2>)).
    Vorbis(ChannelMappingTable),
    /// Families 2 to 254, reserved; a decoder treats them as family 255
    /// ([RFC 7845 Section 5.1.1.4](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1.4>)).
    Reserved(u8, ChannelMappingTable),
    /// Family 255: any number of channels with no defined meaning or order
    /// ([RFC 7845 Section 5.1.1.3](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1.3>)).
    Undefined(ChannelMappingTable),
}

impl ChannelMapping {
    /// Reads the family byte and, for every family but 0, the table that follows it, after
    /// checking `channel_count` against the family's allowed range: 1 or 2 for family 0, 1 to 8
    /// for family 1, and any non-zero count otherwise.
    fn parse<R: ReadBytes>(reader: &mut R, channel_count: u8) -> Result<Self, Error> {
        let family = reader.read_u8()?;
        let allowed = match family {
            0 => 1..=2,
            1 => 1..=8,
            _ => 1..=u8::MAX,
        };
        if !allowed.contains(&channel_count) {
            return Err(Error::InvalidChannelCount(channel_count));
        }

        return match family {
            0 => Ok(Self::Rtp),
            1 => Ok(Self::Vorbis(ChannelMappingTable::parse(reader, channel_count)?)),
            255 => Ok(Self::Undefined(ChannelMappingTable::parse(reader, channel_count)?)),
            _ => Ok(Self::Reserved(family, ChannelMappingTable::parse(reader, channel_count)?)),
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
    /// [`SILENT_CHANNEL`] for silence.
    pub(crate) channel_mapping: Vec<u8>,
}

impl ChannelMappingTable {
    /// Reads the stream counts and one mapping entry per output channel, rejecting an entry
    /// that names a decoded channel beyond `N + M`.
    fn parse<R: ReadBytes>(reader: &mut R, channel_count: u8) -> Result<Self, Error> {
        let stream_count = reader.read_u8()?;
        if stream_count == 0 {
            return Err(Error::InvalidStreamCount(stream_count));
        }

        let coupled_count = reader.read_u8()?;
        if coupled_count > stream_count {
            return Err(Error::InvalidCoupledCount(coupled_count));
        }

        let mut channel_mapping = vec![0u8; usize::from(channel_count)];
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

/// Builders of `OpusHead` packets for the tests of this crate.
#[cfg(test)]
pub(crate) mod test_support {
    /// Builds an `OpusHead` packet from RFC 7845 Section 5.1 defaults (version 1, stereo,
    /// pre-skip 312, 48 kHz input, unity gain, family 0) with one setter per field.
    pub(crate) struct OpusHeadBuilder {
        magic: [u8; 8],
        version: u8,
        channel_count: u8,
        pre_skip: u16,
        input_sample_rate: u32,
        output_gain: i16,
        family: u8,
        table: Vec<u8>,
        trailing: Vec<u8>,
    }

    impl OpusHeadBuilder {
        pub(crate) fn new() -> Self {
            return Self {
                magic: *b"OpusHead",
                version: 1,
                channel_count: 2,
                pre_skip: 312,
                input_sample_rate: 48_000,
                output_gain: 0,
                family: 0,
                table: Vec::new(),
                trailing: Vec::new(),
            };
        }

        pub(crate) fn magic(mut self, magic: &[u8; 8]) -> Self {
            self.magic = *magic;
            return self;
        }

        pub(crate) fn version(mut self, version: u8) -> Self {
            self.version = version;
            return self;
        }

        pub(crate) fn channels(mut self, channel_count: u8) -> Self {
            self.channel_count = channel_count;
            return self;
        }

        pub(crate) fn pre_skip(mut self, pre_skip: u16) -> Self {
            self.pre_skip = pre_skip;
            return self;
        }

        pub(crate) fn output_gain(mut self, output_gain: i16) -> Self {
            self.output_gain = output_gain;
            return self;
        }

        pub(crate) fn family(mut self, family: u8) -> Self {
            self.family = family;
            return self;
        }

        /// Appends a channel mapping table: `N` streams, `M` of them coupled, and one mapping
        /// entry per output channel.
        pub(crate) fn table(
            mut self,
            stream_count: u8,
            coupled_count: u8,
            channel_mapping: &[u8],
        ) -> Self {
            self.table = [&[stream_count, coupled_count], channel_mapping].concat();
            return self;
        }

        /// Appends bytes after the known fields.
        pub(crate) fn trailing(mut self, bytes: &[u8]) -> Self {
            self.trailing = bytes.to_vec();
            return self;
        }

        pub(crate) fn build(self) -> Vec<u8> {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&self.magic);
            bytes.push(self.version);
            bytes.push(self.channel_count);
            bytes.extend_from_slice(&self.pre_skip.to_le_bytes());
            bytes.extend_from_slice(&self.input_sample_rate.to_le_bytes());
            bytes.extend_from_slice(&self.output_gain.to_le_bytes());
            bytes.push(self.family);
            bytes.extend_from_slice(&self.table);
            bytes.extend_from_slice(&self.trailing);
            return bytes;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::OpusHeadBuilder;
    use super::*;
    use symphonia_core::io::BufReader;

    fn parse(header: &[u8]) -> Result<IdentificationHeader, Error> {
        return IdentificationHeader::parse(BufReader::new(header));
    }

    fn table(stream_count: u8, coupled_count: u8, channel_mapping: &[u8]) -> ChannelMappingTable {
        return ChannelMappingTable {
            stream_count,
            coupled_count,
            channel_mapping: channel_mapping.to_vec(),
        };
    }

    #[test]
    fn valid_id_header_parsing() {
        let parsed = parse(&OpusHeadBuilder::new().build()).unwrap();
        let expected = IdentificationHeader {
            version: 1,
            channel_count: 2,
            pre_skip: 312,
            input_sample_rate: 48_000,
            output_gain: OutputGain::UNITY,
            channel_mapping: ChannelMapping::Rtp,
        };
        assert_eq!(parsed, expected);
    }

    /// A header that ends inside its fixed fields reports the failed read
    /// ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)).
    #[test]
    fn truncated_header_reports_the_read_error() {
        let result = parse(&OpusHeadBuilder::new().build()[..12]);
        let eof = std::io::ErrorKind::UnexpectedEof;
        assert!(matches!(result, Err(Error::Io(ref error)) if error.kind() == eof));
    }

    #[test]
    fn invalid_magic_signature() {
        let result = parse(&OpusHeadBuilder::new().magic(b"OpusHeas").build());
        assert!(matches!(result, Err(Error::InvalidMagicSignature)));
    }

    /// Every header error converts to a Symphonia decode error with a crate-prefixed message.
    #[test]
    fn errors_convert_to_symphonia_decode_errors() {
        let error = parse(&OpusHeadBuilder::new().version(16).build()).unwrap_err();
        let converted = SymphoniaError::from(error);
        assert!(
            matches!(converted, SymphoniaError::DecodeError(message) if message.starts_with("opus: "))
        );
    }

    /// RFC 7845 Section 5.1.1: family 0 and family 1 carry positioned channels in Vorbis
    /// order; families 2 to 255 carry discrete channels without defined positions.
    #[test]
    fn channel_layout_follows_the_mapping_family() {
        let mono = parse(&OpusHeadBuilder::new().channels(1).build()).unwrap();
        assert_eq!(mono.channels(), Channels::Positioned(Position::FRONT_LEFT));

        let stereo = parse(&OpusHeadBuilder::new().build()).unwrap();
        let left_right = Position::FRONT_LEFT | Position::FRONT_RIGHT;
        assert_eq!(stereo.channels(), Channels::Positioned(left_right));

        let surround =
            OpusHeadBuilder::new().channels(6).family(1).table(4, 2, &[0, 4, 1, 2, 3, 5]);
        let layout_5_1 = Position::FRONT_LEFT
            | Position::FRONT_CENTER
            | Position::FRONT_RIGHT
            | Position::REAR_LEFT
            | Position::REAR_RIGHT
            | Position::LFE1;
        assert_eq!(parse(&surround.build()).unwrap().channels(), Channels::Positioned(layout_5_1));

        let undefined = OpusHeadBuilder::new().family(255).table(2, 0, &[0, 1]);
        assert_eq!(parse(&undefined.build()).unwrap().channels(), Channels::Discrete(2));
    }

    /// RFC 7845 Section 5.1: the linear scale of the Q7.8 dB gain is `10^(gain / (20 * 256))`,
    /// and unity gain needs no scaling at all.
    #[test]
    fn output_gain_scale_follows_the_rfc_formula() {
        assert_eq!(OutputGain::UNITY.scale(), None);
        let plus_one_db = OutputGain(256).scale().unwrap();
        assert!((plus_one_db - 1.122_018_5).abs() < 1e-6);
        let minus_one_db = OutputGain(-256).scale().unwrap();
        assert!((minus_one_db - 0.891_250_9).abs() < 1e-6);
    }

    /// RFC 7845 Section 5.1: an implementation SHOULD accept any stream with a version number
    /// of 15 or less. Version 0 is what the ISO Base Media File Format `dOps` box carries.
    #[test]
    fn versions_up_to_15_are_accepted() {
        for version in [0, 1, 2, 15] {
            let parsed = parse(&OpusHeadBuilder::new().version(version).build()).unwrap();
            assert_eq!(parsed.version, version);
        }
    }

    /// RFC 7845 Section 5.1: a header with a compatible major version but a larger minor
    /// version MUST NOT be treated as invalid for containing additional data not specified
    /// there. The known fields parse unchanged and the trailing bytes are left unread.
    #[test]
    fn trailing_data_after_the_known_fields_is_tolerated() {
        let trailing = [0xde, 0xad, 0xbe, 0xef];

        let family_zero = parse(&OpusHeadBuilder::new().version(2).build()).unwrap();
        let with_trailing = OpusHeadBuilder::new().version(2).trailing(&trailing).build();
        assert_eq!(parse(&with_trailing).unwrap(), family_zero);

        let family_one = OpusHeadBuilder::new().version(2).family(1).table(2, 1, &[0, 1]);
        let expected = parse(&family_one.build()).unwrap();
        let with_trailing = OpusHeadBuilder::new()
            .version(2)
            .family(1)
            .table(2, 1, &[0, 1])
            .trailing(&trailing)
            .build();
        let parsed = parse(&with_trailing).unwrap();
        assert_eq!(parsed, expected);
        assert_eq!(parsed.channel_mapping, ChannelMapping::Vorbis(table(2, 1, &[0, 1])));
    }

    /// RFC 7845 Section 5.1: a version above 15 has a different major version and the stream
    /// is assumed to be incompatible.
    #[test]
    fn versions_above_15_are_rejected() {
        for version in [16, 0x1f, 0xff] {
            let result = parse(&OpusHeadBuilder::new().version(version).build());
            assert!(matches!(result, Err(Error::UnsupportedVersion(v)) if v == version));
        }
    }

    #[test]
    fn invalid_channel_count() {
        let result = parse(&OpusHeadBuilder::new().channels(0).build());
        assert!(matches!(result, Err(Error::InvalidChannelCount(0))));
    }

    #[test]
    fn family_one_channel_mapping() {
        let parsed = parse(&OpusHeadBuilder::new().family(1).table(2, 1, &[0, 1]).build()).unwrap();
        assert_eq!(parsed.channel_mapping, ChannelMapping::Vorbis(table(2, 1, &[0, 1])));
    }

    /// RFC 7845 Section 5.1.1: `N + M` decoded channels must fit in a byte; a crafted header
    /// must produce an error, never an arithmetic overflow.
    #[test]
    fn stream_and_coupled_counts_summing_past_255_are_rejected() {
        let result = parse(&OpusHeadBuilder::new().family(1).table(200, 100, &[0, 1]).build());
        assert!(matches!(result, Err(Error::ExcessiveDecodedChannels)));
    }

    /// RFC 7845 Section 5.1.1.3 and Section 5.1.1.4: family 255 and the reserved families carry
    /// a channel mapping table like every family other than 0.
    #[test]
    fn families_other_than_0_carry_a_channel_mapping_table() {
        let undefined = parse(&OpusHeadBuilder::new().family(255).table(2, 0, &[0, 1]).build());
        assert_eq!(
            undefined.unwrap().channel_mapping,
            ChannelMapping::Undefined(table(2, 0, &[0, 1]))
        );

        let reserved = parse(&OpusHeadBuilder::new().family(7).table(2, 0, &[0, 1]).build());
        assert_eq!(
            reserved.unwrap().channel_mapping,
            ChannelMapping::Reserved(7, table(2, 0, &[0, 1]))
        );
    }

    /// RFC 7845 Section 5.1.1.1 (family 0: 1 or 2 channels) and Section 5.1.1.2 (family 1:
    /// 1 to 8 channels).
    #[test]
    fn channel_count_outside_the_family_range_is_rejected() {
        let result = parse(&OpusHeadBuilder::new().channels(3).build());
        assert!(matches!(result, Err(Error::InvalidChannelCount(3))));

        let nine =
            OpusHeadBuilder::new().channels(9).family(1).table(5, 4, &[0, 1, 2, 3, 4, 5, 6, 7, 8]);
        let result = parse(&nine.build());
        assert!(matches!(result, Err(Error::InvalidChannelCount(9))));
    }

    #[test]
    fn invalid_stream_count() {
        let result = parse(&OpusHeadBuilder::new().family(1).table(0, 0, &[0, 1]).build());
        assert!(matches!(result, Err(Error::InvalidStreamCount(0))));
    }

    #[test]
    fn invalid_coupled_count() {
        let result = parse(&OpusHeadBuilder::new().family(1).table(2, 3, &[0, 1]).build());
        assert!(matches!(result, Err(Error::InvalidCoupledCount(3))));
    }

    #[test]
    fn valid_channel_mapping() {
        let three = OpusHeadBuilder::new().channels(3).family(1).table(2, 1, &[0, 1, 2]);
        let parsed = parse(&three.build()).unwrap();
        assert_eq!(parsed.channel_count, 3);
        assert_eq!(parsed.channel_mapping, ChannelMapping::Vorbis(table(2, 1, &[0, 1, 2])));
    }

    #[test]
    fn invalid_channel_mapping() {
        let three = OpusHeadBuilder::new().channels(3).family(1).table(2, 1, &[0, 1, 3]);
        let result = parse(&three.build());
        assert!(matches!(result, Err(Error::InvalidChannelMapping)));
    }
}
