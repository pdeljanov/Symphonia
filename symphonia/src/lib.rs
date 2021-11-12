// Symphonia
// Copyright (c) 2019-2021 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

//! # Project Symphonia
//!
//! Symphonia is a 100% pure Rust audio decoding and multimedia format demuxing framework.
//!
//! # Support
//!
//! Supported formats, codecs, and metadata tags are listed below. By default Symphonia only enables
//! royalty-free open standard media formats and codecs. Other formats and codecs must be enabled
//! via. a feature flag.
//!
//! ## Formats
//!
//! The following container formats are supported.
//!
//! | Format   | Feature Flag | Default |
//! |----------|--------------|---------|
//! | ISO/MP4  | `isomp4`     | No      |
//! | OGG      | `ogg`        | Yes     |
//! | Wave     | `wav`        | Yes     |
//!
//! ## Codecs
//!
//! The following codecs are supported.
//!
//! | Codec    | Feature Flag | Default |
//! |----------|--------------|---------|
//! | AAC-LC   | `aac`        | No      |
//! | ALAC     | `alac`       | No      |
//! | FLAC     | `flac`       | Yes     |
//! | MP3      | `mp3`        | No      |
//! | PCM      | `pcm`        | Yes     |
//! | Vorbis   | `vorbis`     | Yes     |
//!
//! ## Metadata
//!
//! The following metadata tagging formats are supported. These are always enabled.
//!
//! * ID3v1
//! * ID3v2
//! * ISO/MP4
//! * RIFF
//! * Vorbis Comment (in OGG & FLAC)
//!
//! # Usage
//!
//! The following steps describe a basic usage of Symphonia:
//!
//! 1.  Instantiate a [`CodecRegistry`][core::codecs::CodecRegistry] and register all the codecs
//!     that are of interest. Alternatively, you may use [`default::get_codecs`] to get the default
//!     registry with all the enabled codecs pre-registered. The registry will be used to
//!     instantiate a [`Decoder`][core::codecs::Decoder] later.
//! 2.  Instantiate a [`Probe`][core::probe::Probe] and register all the formats that are of
//!     interest. Alternatively, you may use [`default::get_probe`] to get a default format probe
//!     with all the enabled formats pre-registered. The probe will be used to automatically detect
//!     the media format and instantiate a compatible [`FormatReader`][core::formats::FormatReader].
//! 3.  Make sure the [`MediaSource`][core::io::MediaSource] trait is implemented for whatever
//!     source you are using. This trait is already implemented for `std::fs::File` and
//!     `std::io::Cursor`.
//! 4.  Instantiate a [`MediaSourceStream`][core::io::MediaSourceStream] with the `MediaSource`
//!     above.
//! 5.  Using the `Probe`, call [`format`][core::probe::Probe::format] and pass it the
//!    `MediaSourceStream`.
//! 6.  If the probe successfully detects a compatible format, a `FormatReader` will be returned.
//!     This is an instance of a demuxer that can read and demux the provided source into 
//!     [`Packet`][core::formats::Packet]s.
//! 7.  At this point it is possible to interrogate the `FormatReader` for general information about
//!     the media and metadata. Examine the [`Track`][core::formats::Track] listing using
//!     [`tracks`][core::formats::FormatReader::tracks] and select one or more tracks of interest to
//!     decode.
//! 8.  To instantiate a `Decoder` for a selected `Track`, call the `CodecRegistry`'s
//!     [`make`][core::codecs::CodecRegistry::make] function and pass it
//!     the [`CodecParameters`][core::codecs::CodecParameters] for that track. This step is repeated
//!     once per selected track.
//! 9.  To decode a track, obtain a packet from the `FormatReader` by
//!     calling [`next_packet`][`core::formats::FormatReader::next_packet`] and then pass the
//!     `Packet` to the `Decoder` for that track. The [`decode`][core::codecs::Decoder::decode]
//!     function will read a packet and return an [`AudioBufferRef`][core::audio::AudioBufferRef]
//!     (an "any-type" [`AudioBuffer`][core::audio::AudioBuffer]).
//! 10. The `AudioBufferRef` may be used to access the decoded audio samples directly, or it can be
//!     copied into a [`SampleBuffer`][core::audio::SampleBuffer] or
//!     [`RawSampleBuffer`][core::audio::RawSampleBuffer] to export the audio out of Symphonia.
//! 11. Repeat step 9 and 10 until the end-of-stream error is returned.
//!
//! An example implementation of a simple audio player (symphonia-play) can be found in the
//! Project Symphonia git repository.
//!
//! # Adding support for new formats and codecs
//!
//! Simply implement the [`Decoder`][core::codecs::Decoder] trait for a decoder or the
//! [`FormatReader`][core::formats::FormatReader] trait for a demuxer trait and register with
//! the appropriate registry or probe!

pub mod default {
    //! The `default` module provides convenience functions and registries to get an implementer
    //! up-and-running as quickly as possible, and to reduce boiler-plate. Using the `default`
    //! module is completely optional and incurs no overhead unless actually used.

    pub mod codecs {
        //! The `codecs` module re-exports all enabled Symphonia decoders.

        #[cfg(feature = "aac")]
        pub use symphonia_codec_aac::AacDecoder as AacDecoder;
        #[cfg(feature = "alac")]
        pub use symphonia_codec_alac::AlacDecoder as AlacDecoder;
        #[cfg(feature = "flac")]
        pub use symphonia_bundle_flac::FlacDecoder as FlacDecoder;
        #[cfg(feature = "mp3")]
        pub use symphonia_bundle_mp3::Mp3Decoder as Mp3Decoder;
        #[cfg(feature = "pcm")]
        pub use symphonia_codec_pcm::PcmDecoder as PcmDecoder;
        #[cfg(feature = "vorbis")]
        pub use symphonia_codec_vorbis::VorbisDecoder as VorbisDecoder;
    }

    pub mod formats {
        //! The `formats` module re-exports all enabled Symphonia format readers.

        #[cfg(feature = "aac")]
        pub use symphonia_codec_aac::AdtsReader as AdtsReader;
        #[cfg(feature = "flac")]
        pub use symphonia_bundle_flac::FlacReader as FlacReader;
        #[cfg(feature = "isomp4")]
        pub use symphonia_format_isomp4::IsoMp4Reader as IsoMp4Reader;
        #[cfg(feature = "mp3")]
        pub use symphonia_bundle_mp3::Mp3Reader as Mp3Reader;
        #[cfg(feature = "wav")]
        pub use symphonia_format_wav::WavReader as WavReader;
        #[cfg(feature = "ogg")]
        pub use symphonia_format_ogg::OggReader as OggReader;
    }

    use lazy_static::lazy_static;

    use symphonia_core::probe::Probe;
    use symphonia_core::codecs::CodecRegistry;

    lazy_static! {
        static ref CODEC_REGISTRY: CodecRegistry = {
            let mut registry = CodecRegistry::new();
            register_enabled_codecs(&mut registry);
            registry
        };
    }

    lazy_static! {
        static ref PROBE: Probe = {
            let mut probe: Probe = Default::default();
            register_enabled_formats(&mut probe);
            probe
        };
    }

    /// Gets the default `CodecRegistry`. This registry pre-registers all the codecs selected by the
    /// `feature` flags in the includer's `Cargo.toml`. If `features` is not set, the default set of
    /// Symphonia codecs is registered.
    ///
    /// This function is lazy and does not instantiate the `CodecRegistry` until the first call to
    /// this function.
    pub fn get_codecs() -> &'static CodecRegistry {
        &CODEC_REGISTRY
    }

    /// Gets the default `Probe`. This registry pre-registers all the formats selected by the
    /// `feature` flags in the includer's `Cargo.toml`. If `features` is not set, the default set of
    /// Symphonia formats is registered.
    ///
    /// This function is lazy and does not instantiate the `Probe` until the first call to this
    /// function.
    pub fn get_probe() -> &'static Probe {
        &PROBE
    }

    /// Registers all the codecs selected by the `feature` flags in the includer's `Cargo.toml` on
    /// the provided `CodecRegistry`. If `features` is not set, the default set of Symphonia codecs
    /// is registered.
    ///
    /// Use this function to easily populate a custom registry with all enabled codecs.
    pub fn register_enabled_codecs(registry: &mut CodecRegistry) {
        #[cfg(feature = "aac")]
        registry.register_all::<codecs::AacDecoder>();

        #[cfg(feature = "alac")]
        registry.register_all::<codecs::AlacDecoder>();

        #[cfg(feature = "flac")]
        registry.register_all::<codecs::FlacDecoder>();

        #[cfg(feature = "mp3")]
        registry.register_all::<codecs::Mp3Decoder>();

        #[cfg(feature = "pcm")]
        registry.register_all::<codecs::PcmDecoder>();

        #[cfg(feature = "vorbis")]
        registry.register_all::<codecs::VorbisDecoder>();
    }

    /// Registers all the formats selected by the `feature` flags in the includer's `Cargo.toml` on
    /// the provided `Probe`. If `features` is not set, the default set of Symphonia formats is
    /// registered.
    ///
    /// Use this function to easily populate a custom probe with all enabled formats.
    pub fn register_enabled_formats(probe: &mut Probe) {
        use symphonia_metadata::id3v2::Id3v2Reader;

        // Formats
        #[cfg(feature = "aac")]
        probe.register_all::<formats::AdtsReader>();

        #[cfg(feature = "flac")]
        probe.register_all::<formats::FlacReader>();

        #[cfg(feature = "isomp4")]
        probe.register_all::<formats::IsoMp4Reader>();

        #[cfg(feature = "mp3")]
        probe.register_all::<formats::Mp3Reader>();

        #[cfg(feature = "wav")]
        probe.register_all::<formats::WavReader>();

        #[cfg(feature = "ogg")]
        probe.register_all::<formats::OggReader>();

        // Metadata
        probe.register_all::<Id3v2Reader>();
    }
}

pub use symphonia_core as core;