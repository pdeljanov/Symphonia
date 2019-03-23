#![warn(rust_2018_idioms)]

use sonata_core::audio::{AudioBuffer, SignalSpec};
use sonata_core::codecs::{CodecParameters, Decoder, DecoderOptions};
use sonata_core::errors::Result;
use sonata_core::formats::Packet;
use sonata_core::io::Bytestream;

use sonata_core::codecs::{CODEC_TYPE_PCM_S32LE};

/// `PcmDecoder` implements a decoder all raw PCM bitstreams.
pub struct PcmDecoder {
    params: CodecParameters,
}

impl Decoder for PcmDecoder {

    fn new(params: &CodecParameters, options: &DecoderOptions) -> Self {
        PcmDecoder {
            params: params.clone(),
        }
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn spec(&self) -> Option<SignalSpec> {
        None
    }

    fn decode(&mut self, packet: Packet<'_>, buf: &mut AudioBuffer<i32>) -> Result<()> {
        let mut stream = packet.into_stream();

        Ok(())
    }
}

fn read_pcm_s32le<B: Bytestream>(reader: &mut B, buf: &mut AudioBuffer<i32>, shift: u32) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
