// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

pub mod default {
    //! The `default` module provides common convenience functions to get an implementer
    //! up-and-running as quickly as possible, and to reduce boiler-plate. Using the `default` module
    //! is completely optional and incurs no overhead unless actually used.

    use lazy_static::lazy_static;

    use sonata_core::probe::Probe;
    use sonata_core::codecs::CodecRegistry;

    lazy_static! {
        static ref CODEC_REGISTRY: CodecRegistry = {
            #[cfg(feature = "flac")]
            use sonata_codec_flac::FlacDecoder;
            #[cfg(feature = "mp3")]
            use sonata_codec_mp3::Mp3Decoder;
            #[cfg(feature = "pcm")]
            use sonata_codec_pcm::PcmDecoder;

            let mut registry = CodecRegistry::new();

            #[cfg(feature = "flac")]
            registry.register_all::<FlacDecoder>(0);

            #[cfg(feature = "mp3")]
            registry.register_all::<Mp3Decoder>(0);

            #[cfg(feature = "pcm")]
            registry.register_all::<PcmDecoder>(0);

            registry
        };
    }

    lazy_static! {
        static ref PROBE: Probe = {
            #[cfg(feature = "flac")]
            use sonata_codec_flac::FlacReader;
            #[cfg(feature = "mp3")]
            use sonata_codec_mp3::Mp3Reader;
            #[cfg(feature = "wav")]
            use sonata_format_wav::WavReader;
            #[cfg(feature = "ogg")]
            use sonata_format_ogg::OggReader;

            use sonata_metadata::id3v2::Id3v2Reader;

            let mut registry: Probe = Default::default();

            #[cfg(feature = "flac")]
            registry.register_all::<FlacReader>();

            #[cfg(feature = "mp3")]
            registry.register_all::<Mp3Reader>();

            #[cfg(feature = "wav")]
            registry.register_all::<WavReader>();

            #[cfg(feature = "ogg")]
            registry.register_all::<OggReader>();

            registry.register_all::<Id3v2Reader>();

            registry
        };
    }

    /// Gets the default `CodecRegistry`. This registry pre-registers all the codecs selected by the
    /// `feature` flags in the includer's `Cargo.toml`. If `features` is not set, the default set of
    /// Sonata codecs is registered.
    ///
    /// This function does not instantiate the `CodecRegistry` until the first call to this function.
    pub fn get_codecs() -> &'static CodecRegistry {
        &CODEC_REGISTRY
    }

    /// Gets the default `Probe`. This registry pre-registers all the formats selected by the
    /// `feature` flags in the includer's `Cargo.toml`. If `features` is not set, the default set of
    /// Sonata formats is registered.
    ///
    /// This function does not instantiate the `Probe` until the first call to this function.
    pub fn get_probe() -> &'static Probe {
        &PROBE
    }

}

pub use sonata_core as core;