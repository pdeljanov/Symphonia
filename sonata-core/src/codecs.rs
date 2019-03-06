// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This library is free software; you can redistribute it and/or
// modify it under the terms of the GNU Lesser General Public
// License as published by the Free Software Foundation; either
// version 2.1 of the License, or (at your option) any later version.
//
// This library is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
// Lesser General Public License for more details.
//
// You should have received a copy of the GNU Lesser General Public
// License along with this library; if not, write to the Free Software
// Foundation, Inc., 51 Franklin Street, Fifth Floor, Boston, MA  02110-1301 USA

use crate::audio::{AudioBuffer, Channel, Layout, SignalSpec};
use crate::io::Bytestream;
use crate::errors::Result;
use crate::formats::Packet;


/// A `CodecType` is a unique identifier used to identify a specific codec. `CodecType` is mainly used for matching 
/// a format's stream to a specific `Decoder`. Decoders advertisting support for a specific `CodecType` should be 
/// interchangeable in regards to their ability to consume packets from a packet stream. This means that while support 
/// for codec features and quality may differ, all Decoders will identically advance the packet stream.
#[derive(Copy, Clone)]
pub struct CodecType(u32);

/// Null decoder, simply discards all data.
pub const CODEC_TYPE_NULL: CodecType = CodecType(0);

/// Free Lossless Audio Codec (FLAC)
pub const CODEC_TYPE_FLAC: CodecType = CodecType(1);

/// MPEG Layer 1, 2, and 3 (MP1, MP2, MP3)
pub const CODEC_TYPE_MP3: CodecType = CodecType(2);

/// Advanced Audio Coding (AAC)
pub const CODEC_TYPE_AAC: CodecType = CodecType(3);

/// Vorbis 
pub const CODEC_TYPE_VORBIS: CodecType = CodecType(4);

/// Opus
pub const CODEC_TYPE_OPUS: CodecType = CodecType(5);

/// Wave (WAV)
pub const CODEC_TYPE_WAVE: CodecType = CodecType(6);

/// WavPack
pub const CODEC_TYPE_WAVPACK: CodecType = CodecType(7);

/// Native Hardware Decoder
pub const CODEC_TYPE_HWDEC: CodecType = CodecType(128);

/// Codec parameters stored in a container format's headers and metadata may be passed to a codec using the 
/// `CodecParameters` structure. All fields in this structure are optional.
#[derive(Clone)]
pub struct CodecParameters {
    pub codec: CodecType,

    /// The sample rate of the audio in Hz.
    pub sample_rate: Option<u32>,

    /// The length of the encoded stream in number of frames.
    pub n_frames: Option<u64>,

    /// The number of bits per one decoded audio sample.
    pub bits_per_sample: Option<u32>,

    /// The number of bits per one coded audio sample.
    pub bits_per_coded_sample: Option<u32>,

    /// The number of audio channels.
    pub n_channels: Option<u32>,

    /// A list of in-order channels.
    pub channels: Option<Vec<Channel>>,

    /// The channel layout.
    pub channel_layout: Option<Layout>,

    /// The number of leading samples inserted by the encoder for padding that should be skipped during playback.
    pub leading_padding: Option<u32>,

    /// The number of trailing samples inserted by the encoder for padding that should be skipped during playback.
    pub trailing_padding: Option<u32>,

    /// The maximum number of samples a packet will contain.
    pub max_frames_per_packet: Option<u64>,
}

impl CodecParameters {
    pub fn new(codec: CodecType) -> CodecParameters {
        CodecParameters {
            codec,
            sample_rate: None,
            n_frames: None,
            bits_per_sample: None,
            bits_per_coded_sample: None,
            n_channels: None,
            channels: None,
            channel_layout: None,
            leading_padding: None,
            trailing_padding: None,
            max_frames_per_packet: None,
        }
    }

    pub fn with_sample_rate(&mut self, sample_rate: u32) -> &mut Self {
        self.sample_rate = Some(sample_rate);
        self
    }

    pub fn with_n_frames(&mut self, n_frames: u64) -> &mut Self {
        self.n_frames = Some(n_frames);
        self
    }

    pub fn with_bits_per_sample(&mut self, bits_per_sample: u32) -> &mut Self {
        self.bits_per_sample = Some(bits_per_sample);
        self
    }

    pub fn with_bits_per_coded_sample(&mut self, bits_per_coded_sample: u32) -> &mut Self {
        self.bits_per_coded_sample = Some(bits_per_coded_sample);
        self
    }

    pub fn with_channels(&mut self, channels: &Vec<Channel>) -> &mut Self {
        self.channels = Some(channels.clone());
        self
    }

    pub fn with_channel_layout(&mut self, channel_layout: Layout) -> &mut Self {
        self.channel_layout = Some(channel_layout);
        self
    }

    pub fn with_leading_padding(&mut self, padding: u32) -> &mut Self {
        self.leading_padding = Some(padding);
        self
    }

    pub fn with_trailing_padding(&mut self, padding: u32) -> &mut Self {
        self.trailing_padding = Some(padding);
        self
    }

    pub fn with_max_frames_per_packet(&mut self, len: u64) -> &mut Self {
        self.max_frames_per_packet = Some(len);
        self
    }

}

/// A `Decoder` implements a Codec's decode process. It consumes `Packet`'s and produces `AudioBuffers`.
pub trait Decoder {

    /// Instantiates the Decoder will the provided `CodecParameters`.
    fn new(params: &CodecParameters) -> Self;

    fn codec_params(&self) -> &CodecParameters;

    fn spec(&self) -> Option<SignalSpec>;

    fn decode<B: Bytestream>(&mut self, packet: &mut Packet<'_, B>, buf: &mut AudioBuffer<i32>) -> Result<()>;
 
    // fn decode_from<B: Bytestream>(&mut self, packet: &mut Packet<'_, B>, buf: &mut AudioBuffer<i32>) -> Result<()>;

}