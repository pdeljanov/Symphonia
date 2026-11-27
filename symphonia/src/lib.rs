// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
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
//! using feature flags.
//!
//! **Tip:** All format, codec, and metadata support can be enabled with the `all` feature flag.
//!
//! ## Formats
//!
//! The following container formats are supported.
//!
//! | Format   | Feature Flag | Gapless* | Default |
//! |----------|--------------|----------|---------|
//! | AC-3     | `ac3`        | No       | No      |
//! | AIFF     | `aiff`       | Yes      | No      |
//! | APE      | `mac`        | No       | No      |
//! | CAF      | `caf`        | No       | No      |
//! | DTS      | `dca`        | No       | No      |
//! | ISO/MP4  | `isomp4`     | Yes      | No      |
//! | FLV      | `flv`        | No       | No      |
//! | MPEG-TS  | `ts`         | No       | No      |
//! | MKV/WebM | `mkv`        | No       | Yes     |
//! | OGG      | `ogg`        | Yes      | Yes     |
//! | TTA      | `tta`        | No       | No      |
//! | ADX      | `adx`        | No       | No      |
//! | SBC      | `sbc`        | No       | No      |
//! | TrueHD   | `truehd`     | No       | No      |
//! | Wave     | `wav`        | Yes      | Yes     |
//! | Wave64   | `wav`        | No       | Yes     |
//! | WavPack  | `wavpack`    | No       | Yes     |
//!
//! \* Gapless playback requires support from both the demuxer and decoder.
//!
//! **Tip:** All formats can be enabled with the `all-codecs` feature flag.
//!
//! ## Codecs
//!
//! The following codecs are supported.
//!
//! | Codec                    | Feature Flag | Gapless | Default |
//! |--------------------------|--------------|---------|---------|
//! | AAC-LC, Main, HE-AAC(v2) | `aac`        | Yes     | No      |
//! | ADPCM                    | `adpcm`      | Yes     | Yes     |
//! | AC-3 (Dolby Digital)     | `ac3`        | No      | No      |
//! | E-AC-3 (no ECPL)         | `ac3`        | No      | No      |
//! | ALAC                     | `alac`       | Yes     | No      |
//! | DTS (core, ES 6.1)       | `dca`        | No      | No      |
//! | FLAC                     | `flac`       | Yes     | Yes     |
//! | Monkey's Audio (APE)     | `mac`        | No      | No      |
//! | MP1                      | `mp1`, `mpa` | No      | No      |
//! | MP2                      | `mp2`, `mpa` | No      | No      |
//! | MP3                      | `mp3`, `mpa` | Yes     | No      |
//! | Opus                     | `opus`       | Yes     | Yes     |
//! | PCM                      | `pcm`        | Yes     | Yes     |
//! | G.723.1                  | `g723`       | No      | No      |
//! | Nellymoser               | `nellymoser` | No      | No      |
//! | Speex                    | `speex`      | No      | No      |
//! | TTA                      | `tta`        | No      | No      |
//! | ADX (CRI ADPCM)          | `adx`        | No      | No      |
//! | DFPWM                    | `dfpwm`      | No      | No      |
//! | TrueHD, MLP              | `truehd`     | No      | No      |
//! | SBC (Bluetooth)          | `sbc`        | No      | No      |
//! | Vorbis                   | `vorbis`     | Yes     | Yes     |
//! | WavPack                  | `wavpack`    | No      | Yes     |
//!
//! **Tip:** All codecs can be enabled with the `all-codecs` feature flag. Similarly, all MPEG
//! audio codecs can be enabled with the `mpa` feature flag.
//!
//! ## Metadata
//!
//! For metadata formats that are standalone and not part of the media container, a feature flag may
//! be used to toggle support.
//!
//! | Format                | Feature Flag | Default |
//! |-----------------------|--------------|---------|
//! | APEv1                 | `ape`        | Yes     |
//! | APEv2                 | `ape`        | Yes     |
//! | ID3v1                 | `id3v1`      | Yes     |
//! | ID3v2                 | `id3v2`      | Yes     |
//! | ISO/MP4               | N/A          | N/A     |
//! | RIFF                  | N/A          | N/A     |
//! | Vorbis comment (FLAC) | N/A          | N/A     |
//! | Vorbis comment (OGG)  | N/A          | N/A     |
//!
//! **Tip:** All metadata formats can be enabled with the `all-meta` feature flag.
//!
//! ## Optimizations
//!
//! SIMD optimizations are enabled by default. Precise control over which SIMD instruction sets are
//! supported may be controlled using the following feature flags. Enabling any SIMD support feature
//! flag will pull in the `rustfft` dependency.
//!
//! | Instruction Set | Feature Flag    | Default |
//! |-----------------|-----------------|---------|
//! | SSE             | `opt-simd-sse`  | Yes     |
//! | AVX             | `opt-simd-avx`  | Yes     |
//! | Neon            | `opt-simd-neon` | Yes     |
//!
//! **Tip:** All SIMD optimizations can be enabled with the `opt-simd` feature flag.
//!
//! # Experimental Features
//!
//! Previews of experimental new features may be enabled by using feature flags. Experimental
//! features should be used for development purposes only. Before using an experimental feature,
//! please observe the warnings below. Never use experimental features in a production application.
//!
//! | Experimental Feature   | Feature Flag          |
//! |------------------------|-----------------------|
//! | Subtitle codec support | `exp-subtitle-codecs` |
//! | Video codec support    | `exp-video-codecs`    |
//!
//! ## Warnings
//!
//! * SemVer compatibilty is **not** guaranteed. Be prepared for build failures.
//! * Experimental features and their associated feature flags **may be removed at any time.**
//! * Functionality **may change or break at any time.**
//! * Again, **never** use in any production application.
//!
//! # Usage
//!
//! An example implementation of a simple audio player (symphonia-play) can be found in the
//! Project Symphonia git repository.
//!
//! # Adding new formats and codecs
//!
//! Simply implement the [`AudioDecoder`][core::codecs::audio::AudioDecoder] trait for an audio
//! decoder or the [`FormatReader`][core::formats::FormatReader] trait for a demuxer trait and
//! register with the appropriate registry or probe!

pub mod default {
    //! The `default` module provides convenience functions and registries to get an implementer
    //! up-and-running as quickly as possible, and to reduce boiler-plate. Using the `default`
    //! module is completely optional and incurs no overhead unless actually used.

    pub mod codecs {
        //! The `codecs` module re-exports all enabled Symphonia decoders.

        #[cfg(feature = "mac")]
        pub use symphonia_bundle_ape::ApeDecoder;
        #[cfg(feature = "flac")]
        pub use symphonia_bundle_flac::FlacDecoder;
        #[cfg(any(feature = "mp1", feature = "mp2", feature = "mp3"))]
        pub use symphonia_bundle_mp3::MpaDecoder;
        #[cfg(feature = "tta")]
        pub use symphonia_bundle_tta::TtaDecoder;

        #[cfg(feature = "adx")]
        pub use symphonia_bundle_adx::AdxDecoder;

        #[cfg(feature = "dfpwm")]
        pub use symphonia_bundle_dfpwm::DfpwmDecoder;

        #[cfg(feature = "truehd")]
        pub use symphonia_bundle_truehd::TrueHdDecoder;

        #[cfg(feature = "sbc")]
        pub use symphonia_bundle_sbc::SbcDecoder;
        #[cfg(feature = "aac")]
        pub use symphonia_codec_aac::AacDecoder;
        #[cfg(feature = "ac3")]
        pub use symphonia_codec_ac3::Ac3Decoder;
        #[cfg(feature = "adpcm")]
        pub use symphonia_codec_adpcm::{AdpcmDecoder, SwfAdpcmDecoder};
        #[cfg(feature = "alac")]
        pub use symphonia_codec_alac::AlacDecoder;
        #[cfg(feature = "dca")]
        pub use symphonia_codec_dca::DcaDecoder;

        #[cfg(feature = "opus")]
        pub use symphonia_codec_opus::OpusDecoder;
        #[cfg(feature = "pcm")]
        pub use symphonia_codec_pcm::PcmDecoder;
        #[cfg(feature = "speex")]
        pub use symphonia_codec_speex::SpeexDecoder;

        #[cfg(feature = "g723")]
        pub use symphonia_codec_g723::G723Decoder;

        #[cfg(feature = "nellymoser")]
        pub use symphonia_codec_nellymoser::NellymoserDecoder;
        #[cfg(feature = "vorbis")]
        pub use symphonia_codec_vorbis::VorbisDecoder;
        #[cfg(feature = "wavpack")]
        pub use symphonia_codec_wavpack::WavPackDecoder;

        #[deprecated = "use `default::codecs::MpaDecoder` instead"]
        #[cfg(any(feature = "mp1", feature = "mp2", feature = "mp3"))]
        pub type Mp3Decoder = MpaDecoder;
    }

    pub mod formats {
        //! The `formats` module re-exports all enabled Symphonia format readers.

        #[cfg(feature = "mac")]
        pub use symphonia_bundle_ape::ApeReader;
        #[cfg(feature = "flac")]
        pub use symphonia_bundle_flac::FlacReader;
        #[cfg(any(feature = "mp1", feature = "mp2", feature = "mp3"))]
        pub use symphonia_bundle_mp3::MpaReader;
        #[cfg(feature = "tta")]
        pub use symphonia_bundle_tta::TtaReader;

        #[cfg(feature = "adx")]
        pub use symphonia_bundle_adx::AdxReader;

        #[cfg(feature = "dfpwm")]
        pub use symphonia_bundle_dfpwm::DfpwmReader;

        #[cfg(feature = "truehd")]
        pub use symphonia_bundle_truehd::TrueHdReader;

        #[cfg(feature = "sbc")]
        pub use symphonia_bundle_sbc::SbcReader;
        #[cfg(feature = "aac")]
        pub use symphonia_codec_aac::AdtsReader;
        #[cfg(feature = "ac3")]
        pub use symphonia_codec_ac3::Ac3Reader;
        #[cfg(feature = "dca")]
        pub use symphonia_codec_dca::DcaReader;
        #[cfg(feature = "wavpack")]
        pub use symphonia_codec_wavpack::WavPackReader;
        #[cfg(feature = "caf")]
        pub use symphonia_format_caf::CafReader;
        #[cfg(feature = "flv")]
        pub use symphonia_format_flv::FlvReader;
        #[cfg(feature = "isomp4")]
        pub use symphonia_format_isomp4::IsoMp4Reader;
        #[cfg(feature = "mkv")]
        pub use symphonia_format_mkv::MkvReader;
        #[cfg(feature = "ogg")]
        pub use symphonia_format_ogg::OggReader;
        #[cfg(feature = "aiff")]
        pub use symphonia_format_riff::AiffReader;
        #[cfg(feature = "wav")]
        pub use symphonia_format_riff::WavReader;
        #[cfg(feature = "ts")]
        pub use symphonia_format_ts::TsReader;

        #[cfg(feature = "wav")]
        pub use symphonia_format_riff::W64Reader;

        #[deprecated = "use `default::formats::MpaReader` instead"]
        #[cfg(any(feature = "mp1", feature = "mp2", feature = "mp3"))]
        pub type Mp3Reader<'s> = MpaReader<'s>;
    }

    pub mod meta {
        //! The `meta` module re-exports all enabled Symphonia metadata readers.

        #[cfg(feature = "ape")]
        pub use symphonia_metadata::ape::ApeReader;
        #[cfg(feature = "id3v1")]
        pub use symphonia_metadata::id3v1::Id3v1Reader;
        #[cfg(feature = "id3v2")]
        pub use symphonia_metadata::id3v2::Id3v2Reader;

        pub use symphonia_metadata::embedded;
    }

    use lazy_static::lazy_static;

    use symphonia_core::codecs::registry::CodecRegistry;
    use symphonia_core::formats::probe::Probe;

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
        registry.register_audio_decoder::<codecs::AacDecoder>();

        #[cfg(feature = "adpcm")]
        registry.register_audio_decoder::<codecs::AdpcmDecoder>();

        #[cfg(feature = "adpcm")]
        registry.register_audio_decoder::<codecs::SwfAdpcmDecoder>();

        #[cfg(feature = "alac")]
        registry.register_audio_decoder::<codecs::AlacDecoder>();

        #[cfg(feature = "ac3")]
        registry.register_audio_decoder::<codecs::Ac3Decoder>();

        #[cfg(feature = "dca")]
        registry.register_audio_decoder::<codecs::DcaDecoder>();

        #[cfg(feature = "speex")]
        registry.register_audio_decoder::<codecs::SpeexDecoder>();

        #[cfg(feature = "g723")]
        registry.register_audio_decoder::<codecs::G723Decoder>();

        #[cfg(feature = "nellymoser")]
        registry.register_audio_decoder::<codecs::NellymoserDecoder>();

        #[cfg(feature = "flac")]
        registry.register_audio_decoder::<codecs::FlacDecoder>();

        #[cfg(any(feature = "mp1", feature = "mp2", feature = "mp3"))]
        registry.register_audio_decoder::<codecs::MpaDecoder>();

        #[cfg(feature = "mac")]
        registry.register_audio_decoder::<codecs::ApeDecoder>();

        #[cfg(feature = "tta")]
        registry.register_audio_decoder::<codecs::TtaDecoder>();

        #[cfg(feature = "adx")]
        registry.register_audio_decoder::<codecs::AdxDecoder>();

        #[cfg(feature = "dfpwm")]
        registry.register_audio_decoder::<codecs::DfpwmDecoder>();

        #[cfg(feature = "truehd")]
        registry.register_audio_decoder::<codecs::TrueHdDecoder>();

        #[cfg(feature = "sbc")]
        registry.register_audio_decoder::<codecs::SbcDecoder>();

        #[cfg(feature = "opus")]
        registry.register_audio_decoder::<codecs::OpusDecoder>();

        #[cfg(feature = "pcm")]
        registry.register_audio_decoder::<codecs::PcmDecoder>();

        #[cfg(feature = "vorbis")]
        registry.register_audio_decoder::<codecs::VorbisDecoder>();

        #[cfg(feature = "wavpack")]
        registry.register_audio_decoder::<codecs::WavPackDecoder>();
    }

    /// Registers all the formats selected by the `feature` flags in the includer's `Cargo.toml` on
    /// the provided `Probe`. If `features` is not set, the default set of Symphonia formats is
    /// registered.
    ///
    /// Use this function to easily populate a custom probe with all enabled formats.
    pub fn register_enabled_formats(probe: &mut Probe) {
        // Formats
        #[cfg(feature = "aac")]
        probe.register_format::<formats::AdtsReader<'_>>();

        #[cfg(feature = "mac")]
        probe.register_format::<formats::ApeReader<'_>>();
        #[cfg(feature = "ac3")]
        probe.register_format::<formats::Ac3Reader<'_>>();
        #[cfg(feature = "dca")]
        probe.register_format::<formats::DcaReader<'_>>();

        #[cfg(feature = "tta")]
        probe.register_format::<formats::TtaReader<'_>>();

        #[cfg(feature = "adx")]
        probe.register_format::<formats::AdxReader<'_>>();

        #[cfg(feature = "sbc")]
        probe.register_format::<formats::SbcReader<'_>>();

        #[cfg(feature = "truehd")]
        probe.register_format::<formats::TrueHdReader<'_>>();

        #[cfg(feature = "caf")]
        probe.register_format::<formats::CafReader<'_>>();

        #[cfg(feature = "flac")]
        probe.register_format::<formats::FlacReader<'_>>();

        #[cfg(feature = "flv")]
        probe.register_format::<formats::FlvReader<'_>>();

        #[cfg(feature = "ts")]
        probe.register_format::<formats::TsReader<'_>>();

        #[cfg(feature = "isomp4")]
        probe.register_format::<formats::IsoMp4Reader<'_>>();

        #[cfg(any(feature = "mp1", feature = "mp2", feature = "mp3"))]
        probe.register_format::<formats::MpaReader<'_>>();

        #[cfg(feature = "aiff")]
        probe.register_format::<formats::AiffReader<'_>>();

        #[cfg(feature = "wav")]
        probe.register_format::<formats::WavReader<'_>>();

        #[cfg(feature = "wav")]
        probe.register_format::<formats::W64Reader<'_>>();

        #[cfg(feature = "wavpack")]
        probe.register_format::<formats::WavPackReader<'_>>();

        #[cfg(feature = "ogg")]
        probe.register_format::<formats::OggReader<'_>>();

        #[cfg(feature = "mkv")]
        probe.register_format::<formats::MkvReader<'_>>();

        // Metadata
        #[cfg(feature = "ape")]
        probe.register_metadata::<meta::ApeReader<'_>>();

        #[cfg(feature = "id3v1")]
        probe.register_metadata::<meta::Id3v1Reader<'_>>();

        #[cfg(feature = "id3v2")]
        probe.register_metadata::<meta::Id3v2Reader<'_>>();
    }
}

pub use symphonia_core as core;
