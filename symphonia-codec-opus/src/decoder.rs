use symphonia_core::audio::{AudioBuffer, AudioBufferRef};
use symphonia_core::codecs::{CodecDescriptor, CodecParameters, Decoder, DecoderOptions, FinalizeResult};
use symphonia_core::formats::Packet;
use crate::{celt, silk};

// Opus-specific constants
const OPUS_FRAME_SIZES: [usize; 5] = [120, 240, 480, 960, 1920];
const MAX_FRAME_SIZE_MS: usize = 60;
const MAX_PACKET_DURATION_MS: usize = 120;
const SILK_INTERNAL_SAMPLE_RATE: u32 = 16000;
const CELT_INTERNAL_SAMPLE_RATE: u32 = 48000;


#[derive(Debug, Clone, Copy)]
enum Mode {
    Silk,
    Celt,
    Hybrid,
}

#[derive(Debug, Clone, Copy)]
enum Bandwidth {
    NarrowBand,
    MediumBand,
    WideBand,
    SuperWideBand,
    FullBand,
}

struct Frame {
    mode: Mode,
    bandwidth: Bandwidth,
    frame_size: usize,
    data: Vec<u8>,
}

pub struct OpusDecoder {
    params: CodecParameters,
    // TODO: extend if needed according to https://datatracker.ietf.org/doc/html/rfc6716
    buf: AudioBuffer<f32>,
    silk_decoder: Option<silk::Decoder>,
    celt_decoder: Option<celt::Decoder>,
}


impl Decoder for OpusDecoder {
    fn try_new(params: &CodecParameters, options: &DecoderOptions) -> symphonia_core::errors::Result<Self>
    where
        Self: Sized,
    {
        todo!()
    }

    fn supported_codecs() -> &'static [CodecDescriptor]
    where
        Self: Sized,
    {
        todo!()
    }

    fn reset(&mut self) {
        todo!()
    }

    fn codec_params(&self) -> &CodecParameters {
        todo!()
    }

    fn decode(&mut self, packet: &Packet) -> symphonia_core::errors::Result<AudioBufferRef> {
        todo!()
    }

    fn finalize(&mut self) -> FinalizeResult {
        todo!()
    }

    fn last_decoded(&self) -> AudioBufferRef {
        todo!()
    }
}