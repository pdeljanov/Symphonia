// Symphonia
// Copyright (c) 2019-2021 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The `codec` module provides the traits and support structures necessary to implement audio codec
//! decoders.

use std::collections::HashMap;
use std::default::Default;
use std::fmt;

use crate::audio::{AudioBufferRef, Channels, Layout};
use crate::errors::{Result, unsupported_error};
use crate::formats::Packet;
use crate::sample::SampleFormat;
use crate::units::TimeBase;

/// A `CodecType` is a unique identifier used to identify a specific codec. `CodecType` is mainly
/// used for matching a format's stream to a specific `Decoder`. Decoders advertisting support for a
/// specific `CodecType` should be interchangeable in regards to their ability to consume packets
/// from a packet stream. This means that while support for codec features and quality may differ,
/// all Decoders will identically advance the packet stream.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct CodecType(u32);

/// Null decoder, simply discards all data.
pub const CODEC_TYPE_NULL: CodecType             = CodecType(0x0);

// Uncompressed PCM audio codecs
//------------------------------

/// PCM signed 32-bit little-endian interleaved
pub const CODEC_TYPE_PCM_S32LE: CodecType        = CodecType(0x100);
/// PCM signed 32-bit little-endian planar
pub const CODEC_TYPE_PCM_S32LE_PLANAR: CodecType = CodecType(0x101);
/// PCM signed 32-bit big-endian interleaved
pub const CODEC_TYPE_PCM_S32BE: CodecType        = CodecType(0x102);
/// PCM signed 32-bit big-endian planar
pub const CODEC_TYPE_PCM_S32BE_PLANAR: CodecType = CodecType(0x103);
/// PCM signed 24-bit little-endian interleaved
pub const CODEC_TYPE_PCM_S24LE: CodecType        = CodecType(0x104);
/// PCM signed 24-bit little-endian planar
pub const CODEC_TYPE_PCM_S24LE_PLANAR: CodecType = CodecType(0x105);
/// PCM signed 24-bit big-endian interleaved
pub const CODEC_TYPE_PCM_S24BE: CodecType        = CodecType(0x106);
/// PCM signed 24-bit big-endian planar
pub const CODEC_TYPE_PCM_S24BE_PLANAR: CodecType = CodecType(0x107);
/// PCM signed 16-bit little-endian interleaved
pub const CODEC_TYPE_PCM_S16LE: CodecType        = CodecType(0x108);
/// PCM signed 16-bit little-endian planar
pub const CODEC_TYPE_PCM_S16LE_PLANAR: CodecType = CodecType(0x109);
/// PCM signed 16-bit big-endian interleaved
pub const CODEC_TYPE_PCM_S16BE: CodecType        = CodecType(0x10a);
/// PCM signed 16-bit big-endian planar
pub const CODEC_TYPE_PCM_S16BE_PLANAR: CodecType = CodecType(0x10b);
/// PCM signed 8-bit interleaved
pub const CODEC_TYPE_PCM_S8: CodecType           = CodecType(0x10c);
/// PCM signed 8-bit planar
pub const CODEC_TYPE_PCM_S8_PLANAR: CodecType    = CodecType(0x10d);
/// PCM unsigned 32-bit little-endian interleaved
pub const CODEC_TYPE_PCM_U32LE: CodecType        = CodecType(0x10e);
/// PCM unsigned 32-bit little-endian planar
pub const CODEC_TYPE_PCM_U32LE_PLANAR: CodecType = CodecType(0x10f);
/// PCM unsigned 32-bit big-endian interleaved
pub const CODEC_TYPE_PCM_U32BE: CodecType        = CodecType(0x110);
/// PCM unsigned 32-bit big-endian planar
pub const CODEC_TYPE_PCM_U32BE_PLANAR: CodecType = CodecType(0x111);
/// PCM unsigned 24-bit little-endian interleaved
pub const CODEC_TYPE_PCM_U24LE: CodecType        = CodecType(0x112);
/// PCM unsigned 24-bit little-endian planar
pub const CODEC_TYPE_PCM_U24LE_PLANAR: CodecType = CodecType(0x113);
/// PCM unsigned 24-bit big-endian interleaved
pub const CODEC_TYPE_PCM_U24BE: CodecType        = CodecType(0x114);
/// PCM unsigned 24-bit big-endian planar
pub const CODEC_TYPE_PCM_U24BE_PLANAR: CodecType = CodecType(0x115);
/// PCM unsigned 16-bit little-endian interleaved
pub const CODEC_TYPE_PCM_U16LE: CodecType        = CodecType(0x116);
/// PCM unsigned 16-bit little-endian planar
pub const CODEC_TYPE_PCM_U16LE_PLANAR: CodecType = CodecType(0x117);
/// PCM unsigned 16-bit big-endian interleaved
pub const CODEC_TYPE_PCM_U16BE: CodecType        = CodecType(0x118);
/// PCM unsigned 16-bit big-endian planar
pub const CODEC_TYPE_PCM_U16BE_PLANAR: CodecType = CodecType(0x119);
/// PCM unsigned 8-bit interleaved
pub const CODEC_TYPE_PCM_U8: CodecType           = CodecType(0x11a);
/// PCM unsigned 8-bit planar
pub const CODEC_TYPE_PCM_U8_PLANAR: CodecType    = CodecType(0x11b);
/// PCM 32-bit little-endian floating point interleaved
pub const CODEC_TYPE_PCM_F32LE: CodecType        = CodecType(0x11c);
/// PCM 32-bit little-endian floating point planar
pub const CODEC_TYPE_PCM_F32LE_PLANAR: CodecType = CodecType(0x11d);
/// PCM 32-bit big-endian floating point interleaved
pub const CODEC_TYPE_PCM_F32BE: CodecType        = CodecType(0x11e);
/// PCM 32-bit big-endian floating point planar
pub const CODEC_TYPE_PCM_F32BE_PLANAR: CodecType = CodecType(0x11f);
/// PCM 64-bit little-endian floating point interleaved
pub const CODEC_TYPE_PCM_F64LE: CodecType        = CodecType(0x120);
/// PCM 64-bit little-endian floating point planar
pub const CODEC_TYPE_PCM_F64LE_PLANAR: CodecType = CodecType(0x121);
/// PCM 64-bit big-endian floating point interleaved
pub const CODEC_TYPE_PCM_F64BE: CodecType        = CodecType(0x122);
/// PCM 64-bit big-endian floating point planar
pub const CODEC_TYPE_PCM_F64BE_PLANAR: CodecType = CodecType(0x123);
/// PCM A-law
pub const CODEC_TYPE_PCM_ALAW: CodecType         = CodecType(0x124);
/// PCM Mu-law
pub const CODEC_TYPE_PCM_MULAW: CodecType        = CodecType(0x125);

// Compressed lossy audio codecs
//------------------------------

/// Vorbis
pub const CODEC_TYPE_VORBIS: CodecType           = CodecType(0x1000);
/// MPEG Layer 1 (MP1)
pub const CODEC_TYPE_MP1: CodecType              = CodecType(0x1001);
/// MPEG Layer 2 (MP2)
pub const CODEC_TYPE_MP2: CodecType              = CodecType(0x1002);
/// MPEG Layer 3 (MP3)
pub const CODEC_TYPE_MP3: CodecType              = CodecType(0x1003);
/// Advanced Audio Coding (AAC)
pub const CODEC_TYPE_AAC: CodecType              = CodecType(0x1004);
/// Opus
pub const CODEC_TYPE_OPUS: CodecType             = CodecType(0x1005);
/// Musepack
pub const CODEC_TYPE_MUSEPACK: CodecType         = CodecType(0x1006);

// Compressed lossless audio codecs
//---------------------------------

/// Free Lossless Audio Codec (FLAC)
pub const CODEC_TYPE_FLAC: CodecType             = CodecType(0x2000);
/// WavPack
pub const CODEC_TYPE_WAVPACK: CodecType          = CodecType(0x2001);
/// Monkey's Audio (APE)
pub const CODEC_TYPE_MONKEYS_AUDIO: CodecType    = CodecType(0x2002);
/// Apple Lossless Audio Codec (ALAC)
pub const CODEC_TYPE_ALAC: CodecType             = CodecType(0x2003);


impl fmt::Display for CodecType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A method and expected value to perform verification on the decoded audio.
#[derive(Copy, Clone)]
pub enum VerificationCheck {
    /// CRC8 of interleaved PCM audio samples.
    Crc8(u8),
    /// CRC16 of interleaved PCM audio samples.
    Crc16([u8; 2]),
    /// CRC32 of interleaved PCM audio samples.
    Crc32([u8; 4]),
    /// MD5 of interleaved PCM audio samples.
    Md5([u8; 16]),
    /// Codec defined, up-to 16-byte code.
    Other([u8; 16]),
}

/// Codec parameters stored in a container format's headers and metadata may be passed to a codec
/// using the `CodecParameters` structure.
#[derive(Clone)]
pub struct CodecParameters {
    /// The codec type.
    pub codec: CodecType,

    /// The sample rate of the audio in Hz.
    pub sample_rate: Option<u32>,

    /// The `TimeBase`.
    pub time_base: Option<TimeBase>,

    /// The length of the encoded stream in number of frames.
    pub n_frames: Option<u64>,

    /// The timestamp of the first frame.
    pub start_ts: u64,

    /// The sample format of an audio sample.
    pub sample_format: Option<SampleFormat>,

    /// The number of bits per one decoded audio sample.
    pub bits_per_sample: Option<u32>,

    /// The number of bits per one encoded audio sample.
    pub bits_per_coded_sample: Option<u32>,

    /// A bitmask of all channels in the stream.
    pub channels: Option<Channels>,

    /// The channel layout.
    pub channel_layout: Option<Layout>,

    /// The number of leading frames inserted by the encoder for padding that should be skipped
    /// during playback.
    pub leading_padding: Option<u32>,

    /// The number of trailing frames inserted by the encoder for padding that should be skipped
    /// during playback.
    pub trailing_padding: Option<u32>,

    /// The maximum number of frames a packet will contain.
    pub max_frames_per_packet: Option<u64>,

    /// The demuxer guarantees packet data integrity.
    pub packet_data_integrity: bool,

    /// A method and expected value that may be used to perform verification on the decoded audio.
    pub verification_check: Option<VerificationCheck>,

    /// Extra data (defined by the codec).
    pub extra_data: Option<Box<[u8]>>,
}

impl CodecParameters {
    pub fn new() -> CodecParameters {
        CodecParameters {
            codec: CODEC_TYPE_NULL,
            sample_rate: None,
            time_base: None,
            n_frames: None,
            start_ts: 0,
            sample_format: None,
            bits_per_sample: None,
            bits_per_coded_sample: None,
            channels: None,
            channel_layout: None,
            leading_padding: None,
            trailing_padding: None,
            max_frames_per_packet: None,
            packet_data_integrity: false,
            verification_check: None,
            extra_data: None,
        }
    }

    /// Provide the `CodecType`.
    pub fn for_codec(&mut self, codec: CodecType) -> &mut Self {
        self.codec = codec;
        self
    }

    /// Provide the sample rate in Hz.
    pub fn with_sample_rate(&mut self, sample_rate: u32) -> &mut Self {
        self.sample_rate = Some(sample_rate);
        self
    }

    /// Provide the `TimeBase`.
    pub fn with_time_base(&mut self, time_base: TimeBase) -> &mut Self {
        self.time_base = Some(time_base);
        self
    }

    /// Provide the total number of frames.
    pub fn with_n_frames(&mut self, n_frames: u64) -> &mut Self {
        self.n_frames = Some(n_frames);
        self
    }

    /// Provide the timestamp of the first frame.
    pub fn with_start_ts(&mut self, start_ts: u64) -> &mut Self {
        self.start_ts = start_ts;
        self
    }

    /// Provide the codec's decoded audio sample format.
    pub fn with_sample_format(&mut self, sample_format: SampleFormat) -> &mut Self {
        self.sample_format = Some(sample_format);
        self
    }

    /// Provide the bit per sample of a decoded audio sample.
    pub fn with_bits_per_sample(&mut self, bits_per_sample: u32) -> &mut Self {
        self.bits_per_sample = Some(bits_per_sample);
        self
    }

    /// Provide the bits per sample of an encoded audio sample.
    pub fn with_bits_per_coded_sample(&mut self, bits_per_coded_sample: u32) -> &mut Self {
        self.bits_per_coded_sample = Some(bits_per_coded_sample);
        self
    }

    /// Provide the channel map.
    pub fn with_channels(&mut self, channels: Channels) -> &mut Self {
        self.channels = Some(channels);
        self
    }

    /// Provide the channel layout.
    pub fn with_channel_layout(&mut self, channel_layout: Layout) -> &mut Self {
        self.channel_layout = Some(channel_layout);
        self
    }

    /// Provide the number of leading frames to discard.
    pub fn with_leading_padding(&mut self, padding: u32) -> &mut Self {
        self.leading_padding = Some(padding);
        self
    }

    /// Provide the number of trailing frames to discard.
    pub fn with_trailing_padding(&mut self, padding: u32) -> &mut Self {
        self.trailing_padding = Some(padding);
        self
    }

    /// Provide the maximum number of frames per packet.
    pub fn with_max_frames_per_packet(&mut self, len: u64) -> &mut Self {
        self.max_frames_per_packet = Some(len);
        self
    }

    /// Specify if the packet's data integrity was guaranteed.
    pub fn with_packet_data_integrity(&mut self, integrity: bool) -> &mut Self {
        self.packet_data_integrity = integrity;
        self
    }

    /// Provide codec extra data.
    pub fn with_extra_data(&mut self, data: Box<[u8]>) -> &mut Self {
        self.extra_data = Some(data);
        self
    }

    /// Provide a verification code of the final decoded audio.
    pub fn with_verification_code(&mut self, code: VerificationCheck) -> &mut Self {
        self.verification_check = Some(code);
        self
    }
}

impl Default for CodecParameters {
    fn default() -> Self {
        Self::new()
    }
}

/// `FinalizeResult` contains optional information that can only be found, calculated, or
/// determined after decoding is complete.
#[derive(Default)]
pub struct FinalizeResult {
    /// If verification is enabled and supported by the decoder, provides the verification result
    /// if available.
    pub verify_ok: Option<bool>,
}

/// `DecoderOptions` is a common set of options that all decoders use.
pub struct DecoderOptions {
    /// The decoded audio should be verified if possible during the decode process.
    pub verify: bool,
}

impl Default for DecoderOptions {
    fn default() -> Self {
        DecoderOptions {
            verify: false,
        }
    }
}

/// A `Decoder` implements a codec's decode algorithm. It consumes `Packet`s and produces
/// `AudioBuffer`s.
pub trait Decoder: Send {
    /// Attempts to instantiates a `Decoder` using the provided `CodecParameters`.
    fn try_new(params: &CodecParameters, options: &DecoderOptions) -> Result<Self>
    where
        Self: Sized;

    /// Gets a list of codec descriptors for the codecs supported by this Decoder.
    fn supported_codecs() -> &'static [CodecDescriptor]
    where
        Self: Sized;

    /// Reset the `Decoder`.
    ///
    /// A decoder must be reset when the next packet is discontinuous with respect to the last
    /// decoded packet. Most notably, this occurs after a seek.
    ///
    /// For codecs that do a lot of pre-computation, reset should only reset the absolute minimum
    /// amount of state.
    fn reset(&mut self);

    /// Gets a reference to parameters the `Decoder` was instantiated with.
    fn codec_params(&self) -> &CodecParameters;

    /// Decodes a `Packet` of audio data and returns a copy-on-write generic (untyped) audio buffer
    /// of the decoded audio.
    fn decode(&mut self, packet: &Packet) -> Result<AudioBufferRef>;

    /// Optionally, obtain post-decode information such as the verification status.
    fn finalize(&mut self) -> FinalizeResult;
}

/// A `CodecDescriptor` stores a description of a single logical codec. Common information such as
/// the `CodecType`, a short name, and a long name are provided. The `CodecDescriptor` also provides
/// an instantiation function. When the instantiation function is called, a `Decoder` for the codec
/// is returned.
#[derive(Copy, Clone)]
pub struct CodecDescriptor {
    /// The `CodecType` identifier.
    pub codec: CodecType,
    /// A short ASCII-only string identifying the codec.
    pub short_name: &'static str,
    /// A longer, more descriptive, string identifying the codec.
    pub long_name: &'static str,
    // An instantiation function for the codec.
    pub inst_func: fn(&CodecParameters, &DecoderOptions) -> Result<Box<dyn Decoder>>,
}

/// A `CodecRegistry` allows the registration of codecs, and provides a method to instantiate a
/// `Decoder` given a `CodecParameters` object.
pub struct CodecRegistry {
    codecs: HashMap<CodecType, CodecDescriptor>,
}

impl CodecRegistry {
    /// Instantiate a new `CodecRegistry`.
    pub fn new() -> Self {
        CodecRegistry {
            codecs: HashMap::new(),
        }
    }

    /// Gets the `CodecDescriptor` for a registered codec.
    pub fn get_codec(&self, codec: CodecType) -> Option<&CodecDescriptor> {
        self.codecs.get(&codec)
    }

    /// Registers all codecs supported by `Decoder`. If a supported codec was previously registered
    /// by another `Decoder` it will be replaced within the registry.
    pub fn register_all<D: Decoder>(&mut self) {
        for descriptor in D::supported_codecs() {
            self.register(descriptor);
        }
    }

    /// Register a single codec. If the codec was previously registered it will be replaced within
    /// the registry.
    pub fn register(&mut self, descriptor: &CodecDescriptor) {
        self.codecs.insert(descriptor.codec, *descriptor);
    }

    /// Searches the registry for a `Decoder` that supports the codec. If one is found, it will be
    /// instantiated with the provided `CodecParameters` and returned. If a `Decoder` could not be
    /// found, or the `CodecParameters` are either insufficient or invalid for the `Decoder`, an
    /// error will be returned.
    pub fn make(&self, params: &CodecParameters, options: &DecoderOptions)
        -> Result<Box<dyn Decoder>> {

        if let Some(descriptor) = self.codecs.get(&params.codec) {
            Ok((descriptor.inst_func)(params, options)?)
        }
        else {
            unsupported_error("core (codec):unsupported codec")
        }
    }
}

impl Default for CodecRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience macro for declaring a `CodecDescriptor`.
#[macro_export]
macro_rules! support_codec {
    ($type:expr, $short_name:expr, $long_name:expr) => {
        CodecDescriptor {
            codec: $type,
            short_name: $short_name,
            long_name: $long_name,
            inst_func: |params, opt| {
                Ok(Box::new(Self::try_new(&params, &opt)?))
            }
        }
    };
}