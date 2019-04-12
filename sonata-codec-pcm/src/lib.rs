#![warn(rust_2018_idioms)]

use sonata_core::support_codec;

use sonata_core::audio::{AudioBuffer, Signal, SignalSpec};
use sonata_core::codecs::{CodecParameters, CodecDescriptor, Decoder, DecoderOptions};
use sonata_core::codecs::{CODEC_TYPE_PCM_S8, CODEC_TYPE_PCM_S16LE, CODEC_TYPE_PCM_S24LE, CODEC_TYPE_PCM_S32LE};
use sonata_core::errors::{Result, unsupported_error};
use sonata_core::formats::Packet;
use sonata_core::io::Bytestream;

macro_rules! read_pcm_signed {
    ($buf:ident, $read:expr, $shift:expr) => {
        $buf.fill(| audio_planes, idx | -> Result<()> {
            for plane in audio_planes.planes() {
                plane[idx] = (($read as u32) << (32 - $shift)) as i32;
            }
            Ok(()) 
        })
    };
}

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

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[
            support_codec!(CODEC_TYPE_PCM_S16LE, "pcm_s16le", "PCM Signed 16-bit Little-Endian Interleaved"),
            support_codec!(CODEC_TYPE_PCM_S24LE, "pcm_s24le", "PCM Signed 24-bit Little-Endian Interleaved"),
            support_codec!(CODEC_TYPE_PCM_S32LE, "pcm_s32le", "PCM Signed 32-bit Little-Endian Interleaved"),
        ]
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn spec(&self) -> Option<SignalSpec> {
        if let Some(rate) = self.params.sample_rate {
            // Prefer the channel layout over a list of channels.
            if let Some(layout) = self.params.channel_layout {
                return Some(SignalSpec::new_with_layout(rate, layout));
            }
            else if let Some(channels) = self.params.channels {
                return Some(SignalSpec::new(rate, channels));
            }
        }
        None
    }

    fn decode(&mut self, packet: Packet<'_>, buf: &mut AudioBuffer<i32>) -> Result<()> {
        let mut stream = packet.into_stream();

        match self.params.codec {
            CODEC_TYPE_PCM_S8 => 
                read_pcm_signed!(buf, stream.read_u8()?,  self.params.bits_per_coded_sample.unwrap_or( 8)),
            CODEC_TYPE_PCM_S16LE => 
                read_pcm_signed!(buf, stream.read_u16()?, self.params.bits_per_coded_sample.unwrap_or(16)),
            CODEC_TYPE_PCM_S24LE => 
                read_pcm_signed!(buf, stream.read_u24()?, self.params.bits_per_coded_sample.unwrap_or(24)),
            CODEC_TYPE_PCM_S32LE => 
                read_pcm_signed!(buf, stream.read_u32()?, self.params.bits_per_coded_sample.unwrap_or(32)),
            _ => 
                unsupported_error("PCM codec unsupported.")
        }
    }
}



#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}

