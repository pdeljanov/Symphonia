//! Opus Decoder
//!
//! The Opus decoder consists of two main blocks: the SILK decoder and
//! the CELT decoder. At any given time, one or both of the SILK and
//! CELT decoders may be active. The output of the Opus decode is the
//! sum of the outputs from the SILK and CELT decoders with proper sample
//! rate conversion and delay compensation on the SILK side, and optional
//! decimation (when decoding to sample rates less than 48 kHz) on the
//! CELT side, as illustrated in the block diagram below.
//! ```text
//!
//!                          +---------+    +------------+
//!                          |  SILK   |    |   Sample   |
//!                       +->| Decoder |--->|    Rate    |----+
//! Bit-    +---------+   |  |         |    | Conversion |    v
//! stream  |  Range  |---+  +---------+    +------------+  /---\  Audio
//! ------->| Decoder |                                     | + |------>
//!         |         |---+  +---------+    +------------+  \---/
//!         +---------+   |  |  CELT   |    | Decimation |    ^
//!                       +->| Decoder |--->| (Optional) |----+
//!                          |         |    |            |
//!                          +---------+    +------------+
//! ```
//! <https://datatracker.ietf.org/doc/html/rfc6716#section-4>
use crate::silk;
use std::sync::LazyLock;
use symphonia_core::audio::GenericAudioBufferRef;
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{Result, unsupported_error};
use symphonia_core::packet::PacketRef;

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
        id: CODEC_ID_OPUS,
        info: *CODEC_INFO,
    }]
});

/// The OpusDecoder struct implements the Symphonia AudioDecoder trait.
/// It currently supports only SILK mode.
/// CELT and Hybrid modes are placeholders for future implementation.
pub struct OpusDecoder {
    silk_decoder: silk::Decoder,
}

impl OpusDecoder {
    pub fn try_new(params: &AudioCodecParameters, _options: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_OPUS {
            return unsupported_error("opus: invalid codec");
        }

        let silk_decoder = silk::Decoder::try_new(params.to_owned())?;

        return Ok(Self { silk_decoder });
    }
}

impl AudioDecoder for OpusDecoder {
    fn reset(&mut self) {
        self.silk_decoder.reset();
    }

    fn codec_info(&self) -> &CodecInfo {
        return &CODEC_INFO;
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        return self.silk_decoder.codec_params();
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        // TODO: Implement all decoder modes.
        return self.silk_decoder.decode(packet);
    }

    fn finalize(&mut self) -> FinalizeResult {
        return self.silk_decoder.finalize();
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        return self.silk_decoder.last_decoded();
    }
}

impl RegisterableAudioDecoder for OpusDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>> {
        return Ok(Box::new(Self::try_new(params, opts)?));
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        return &SUPPORTED_CODECS[..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symphonia_core::audio::{Channels, Position};
    use symphonia_core::codecs::audio::well_known::CODEC_ID_VORBIS;

    #[test]
    fn opus_decoder_initialization() {
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_OPUS)
            .with_sample_rate(48000)
            .with_channels(Channels::Positioned(
                Position::FRONT_LEFT | Position::FRONT_RIGHT,
            ));

        let mut decoder = OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();

        assert_eq!(decoder.codec_info().short_name, "opus");
        assert_eq!(decoder.codec_params().codec, CODEC_ID_OPUS);
        assert_eq!(decoder.last_decoded().frames(), 0);
        assert!(decoder.finalize().verify_ok.is_none());

        decoder.reset();

        assert_eq!(OpusDecoder::supported_codecs().len(), 1);
        assert_eq!(OpusDecoder::supported_codecs()[0].id, CODEC_ID_OPUS);
    }

    #[test]
    fn opus_decoder_invalid_codec() {
        let mut params = AudioCodecParameters::new();
        params.for_codec(CODEC_ID_VORBIS);

        let result = OpusDecoder::try_new(&params, &AudioDecoderOptions::default());
        assert!(result.is_err());
    }
}