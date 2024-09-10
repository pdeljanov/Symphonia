mod common;
mod range;
mod decoder;
mod header;
mod silk;
mod celt;
mod parameters;

use symphonia_core::codecs::*;
use symphonia_core::errors::Result;
use symphonia_core::io::*;
use symphonia_core::audio::*;

use decoder::OpusDecoder;

use once_cell::sync::Lazy;

/// Opus codec descriptor 
static OPUS_CODEC_DESCRIPTOR: Lazy<CodecDescriptor> = Lazy::new(|| {
    CodecDescriptor {
        codec: CODEC_TYPE_OPUS,
        short_name: "opus",
        long_name: "Opus Audio Codec",
        inst_func: |params: &CodecParameters, options: &DecoderOptions| -> Result<Box<dyn Decoder>> {
            Ok(Box::new(OpusDecoder::try_new(params, options)?))
        },
    }
});

/// Register the Opus decoder with Symphonia
pub fn get_codecs() -> &'static [CodecDescriptor] {
   return std::slice::from_ref(&*OPUS_CODEC_DESCRIPTOR)
}