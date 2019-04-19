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

pub mod default {
    //! The `default` module provides common convenience functions to get an implementer up-and-running as quickly as 
    //! possible and reduce boiler-plate. Using the `default` modules is completely optional and incurs no overhead 
    //! unless actually used.

    use lazy_static::lazy_static;

    use sonata_core::formats::FormatRegistry;
    use sonata_core::codecs::CodecRegistry;

    lazy_static! {
        static ref CODEC_REGISTRY: CodecRegistry = {
            #[cfg(feature = "flac")]
            use sonata_codec_flac::FlacDecoder;
            #[cfg(feature = "pcm")]
            use sonata_codec_pcm::PcmDecoder;

            let mut registry = CodecRegistry::new();

            #[cfg(feature = "flac")]
            registry.register_all::<FlacDecoder>(0);

            #[cfg(feature = "pcm")]
            registry.register_all::<PcmDecoder>(0);

            registry
        };
    }

    lazy_static! {
        static ref FORMAT_REGISTRY: FormatRegistry = {
            #[cfg(feature = "flac")]
            use sonata_codec_flac::FlacReader;
            #[cfg(feature = "wav")]
            use sonata_format_wav::WavReader;

            let mut registry = FormatRegistry::new();

            #[cfg(feature = "flac")]
            registry.register_all::<FlacReader>(0);

            #[cfg(feature = "wav")]
            registry.register_all::<WavReader>(0);

            registry
        };
    }

    /// Gets the default `CodecRegistry`. This registry pre-registers all the codecs selected by the `feature` flags in 
    /// the includer's `Cargo.toml`. If `features` is not set, the default set of Sonata codecs is registered. 
    /// 
    /// This function does not create the `CodecRegistry` until the first call to this function.
    pub fn get_codecs() -> &'static CodecRegistry {
        &CODEC_REGISTRY
    }

    /// Gets the default `FormatRegistry`. This registry pre-registers all the formats selected by the `feature` flags 
    /// in the includer's `Cargo.toml`. If `features` is not set, the default set of Sonata formats is registered. 
    /// 
    /// This function does not create the `FormatRegistry` until the first call to this function.
    pub fn get_formats() -> &'static FormatRegistry {
        &FORMAT_REGISTRY
    }

}

pub use sonata_core as core;