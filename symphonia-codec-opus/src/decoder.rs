//! Opus Decoder
///
/// The Opus decoder consists of two main blocks: the SILK decoder and
/// the CELT decoder. At any given time, one or both of the SILK and
/// CELT decoders may be active.  The output of the Opus decode is the
/// sum of the outputs from the SILK and CELT decoders with proper sample
/// rate conversion and delay compensation on the SILK side, and optional
/// decimation (when decoding to sample rates less than 48 kHz) on the
/// CELT side, as illustrated in the block diagram below.
///```text
///
///                          +---------+    +------------+
///                          |  SILK   |    |   Sample   |
///                       +->| Decoder |--->|    Rate    |----+
/// Bit-    +---------+   |  |         |    | Conversion |    v
/// stream  |  Range  |---+  +---------+    +------------+  /---\  Audio
/// ------->| Decoder |                                     | + |------>
///         |         |---+  +---------+    +------------+  \---/
///         +---------+   |  |  CELT   |    | Decimation |    ^
///                       +->| Decoder |--->| (Optional) |----+
///                          |         |    |            |
///                          +---------+    +------------+
/// ```
/// https://datatracker.ietf.org/doc/html/rfc6716#section-4
use crate::silk;
use std::sync::LazyLock;
use symphonia_core::audio::GenericAudioBufferRef;
use symphonia_core::codecs::audio::{
    AudioCodecId, AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::CodecParameters;
use symphonia_core::common::FourCc;
use symphonia_core::formats::Packet;

/// Opus codec ID as a FourCC: 'OPUS'
pub const CODEC_TYPE_OPUS: AudioCodecId = AudioCodecId::new(FourCc::new(*b"OPUS"));

/// Static Opus codec info.
static CODEC_INFO: LazyLock<CodecInfo> = LazyLock::new(|| {
    CodecInfo {
        short_name: "opus",
        long_name: "Opus Audio Codec",
        profiles: &[],
    }
});

/// Static supported codecs array.
static SUPPORTED_CODECS: LazyLock<[SupportedAudioCodec; 1]> = LazyLock::new(|| {
    [SupportedAudioCodec {
        id: CODEC_TYPE_OPUS,
        info: (*CODEC_INFO).clone(),
    }]
});

/// The OpusDecoder struct implements the Symphonia Decoder trait.
/// It currently supports only SILK mode.
/// CELT and Hybrid modes are placeholders for future implementation.
pub struct OpusDecoder {
    silk_decoder: silk::Decoder,
    codec_params: AudioCodecParameters,
}

impl AudioDecoder for OpusDecoder {
    fn reset(&mut self) {
        self.silk_decoder.reset();
    }

    fn codec_info(&self) -> &CodecInfo {
        &CODEC_INFO
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.codec_params
    }

    fn decode(&mut self, packet: &Packet) -> symphonia_core::errors::Result<GenericAudioBufferRef> {
        // TODO: Implement all decoder modes.
        self.silk_decoder.decode(packet)
    }

    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef {
        // Return the last decoded buffer from the silk decoder
        self.silk_decoder.last_decoded()
    }
}

impl RegisterableAudioDecoder for OpusDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        _opts: &AudioDecoderOptions,
    ) -> symphonia_core::errors::Result<Box<dyn AudioDecoder>> {
        let params_codec = CodecParameters::Audio(params.to_owned());
        let decoder = Self {
            silk_decoder: silk::Decoder::try_new(params_codec)?,
            codec_params: params.to_owned(),
        };
        Ok(Box::new(decoder))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &SUPPORTED_CODECS[..]
    }
}