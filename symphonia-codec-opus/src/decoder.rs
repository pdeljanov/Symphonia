use crate::silk;
use std::sync::LazyLock;
use symphonia_core::audio::AudioBufferRef;
use symphonia_core::codecs::{CodecDescriptor, CodecParameters, Decoder, DecoderOptions, FinalizeResult, CODEC_TYPE_OPUS};
use symphonia_core::formats::Packet;

/// Static Opus Codec Descriptor.
static OPUS_CODEC_DESCRIPTOR: LazyLock<CodecDescriptor> = LazyLock::new(|| {
    CodecDescriptor {
        codec: CODEC_TYPE_OPUS,
        short_name: "opus",
        long_name: "Opus Audio Codec",
        inst_func: |params: &CodecParameters, options: &DecoderOptions| -> symphonia_core::errors::Result<Box<dyn Decoder>> {
            Ok(Box::new(OpusDecoder::try_new(params, options)?))
        },
    }
});

/// Register the Opus decoder with Symphonia.
pub fn get_codecs() -> &'static [CodecDescriptor] {
    return std::slice::from_ref(&*OPUS_CODEC_DESCRIPTOR);
}

/// The OpusDecoder struct implements the Symphonia Decoder trait.
/// It currently supports only SILK mode. 
/// CELT and Hybrid modes are placeholders for future implementation.
pub struct OpusDecoder {
    silk_decoder: silk::Decoder,
}


impl Decoder for OpusDecoder {
    fn try_new(params: &CodecParameters, _options: &DecoderOptions) -> symphonia_core::errors::Result<Self>
    where
        Self: Sized,
    {
        let silk_decoder = silk::Decoder::try_new(params.to_owned())?;

        return Ok(Self { silk_decoder });
    }

    fn supported_codecs() -> &'static [CodecDescriptor]
    where
        Self: Sized,
    {
        return get_codecs();
    }

    fn reset(&mut self) {
        self.silk_decoder.reset();
    }

    fn codec_params(&self) -> &CodecParameters {
        return self.silk_decoder.codec_params();
    }

    fn decode(&mut self, packet: &Packet) -> symphonia_core::errors::Result<AudioBufferRef> {
        // TODO: Implement all decoder modes.
        return self.silk_decoder.decode(packet);
    }

    fn finalize(&mut self) -> FinalizeResult {
        unimplemented!()
    }

    fn last_decoded(&self) -> AudioBufferRef {
        unimplemented!()
    }
}
