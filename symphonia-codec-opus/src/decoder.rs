use once_cell::sync::Lazy;
use symphonia_core::audio::{AudioBuffer, AudioBufferRef, SignalSpec};
use symphonia_core::codecs::{CodecDescriptor, CodecParameters, Decoder, DecoderOptions, FinalizeResult, CODEC_TYPE_OPUS};
use symphonia_core::formats::Packet;
use crate::{celt, entropy, silk, toc};
use thiserror::Error;
use symphonia_core::errors::Error;
use crate::toc::{AudioMode, Toc};

const OPUS_FRAME_SIZES: [usize; 5] = [120, 240, 480, 960, 1920];

const SILK_INTERNAL_SAMPLE_RATE: u32 = 16000;
const CELT_INTERNAL_SAMPLE_RATE: u32 = 48000;
const DEFAULT_FRAME_LENGTH_MS: usize = 20;

static OPUS_CODEC_DESCRIPTOR: Lazy<CodecDescriptor> = Lazy::new(|| {
    CodecDescriptor {
        codec: CODEC_TYPE_OPUS,
        short_name: "opus",
        long_name: "Opus Audio Codec",
        inst_func: |params: &CodecParameters, options: &DecoderOptions| -> symphonia_core::errors::Result<Box<dyn Decoder>> {
            Ok(Box::new(OpusDecoder::try_new(params, options)?))
        },
    }
});

/// Register the Opus decoder with Symphonia
pub fn get_codecs() -> &'static [CodecDescriptor] {
    return std::slice::from_ref(&*OPUS_CODEC_DESCRIPTOR);
}

pub struct OpusDecoder {
    buf: Option<AudioBuffer<f32>>, 
    silk_decoder: silk::Decoder,
    celt_decoder: celt::Decoder,
}


impl Decoder for OpusDecoder {
    fn try_new(params: &CodecParameters, _options: &DecoderOptions) -> symphonia_core::errors::Result<Self>
    where
        Self: Sized,
    {
        let silk_decoder = silk::Decoder::try_new(params,)?;
        let celt_decoder = celt::Decoder::new();

        return Ok(Self {
            buf: None,
            silk_decoder,
            celt_decoder,
        });
    }

    fn supported_codecs() -> &'static [CodecDescriptor]
    where
        Self: Sized,
    {
        unimplemented!()
    }

    fn reset(&mut self) {
        unimplemented!()
    }

    fn codec_params(&self) -> &CodecParameters {
        unimplemented!()
    }

    fn decode(&mut self, packet: &Packet) -> symphonia_core::errors::Result<AudioBufferRef> {
        return self.silk_decoder.decode(packet);
    }

    fn finalize(&mut self) -> FinalizeResult {
        unimplemented!()
    }

    fn last_decoded(&self) -> AudioBufferRef {
        unimplemented!()
    }
}
