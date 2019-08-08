use sonata_core::audio::{AudioBuffer, Signal, SignalSpec};
use sonata_core::codecs::{CODEC_TYPE_MP3, CodecParameters, CodecDescriptor, Decoder, DecoderOptions};
use sonata_core::conv::{IntoSample};
use sonata_core::errors::Result;
use sonata_core::formats::Packet;
use sonata_core::support_codec;

use super::bitstream::{State, next_frame};

pub struct Mp3Decoder {
    params: CodecParameters,
    state: State,
}

impl Decoder for Mp3Decoder {

    fn new(params: &CodecParameters, options: &DecoderOptions) -> Self {
        Mp3Decoder {
            params: params.clone(),
            state: State::new(),
        }
    }

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[ support_codec!(CODEC_TYPE_MP3, "mp3", "MPEG Audio Layer 3") ]
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
        let mut reader = packet.into_stream();

        next_frame(&mut reader, &mut self.state)?;

        buf.render_reserved(Some(1152));
        let (l, r) = buf.chan_pair_mut(0, 1);

        for i in 0..576 {
            l[i +   0] = self.state.samples[0][0][i].into_sample();
            l[i + 576] = self.state.samples[1][0][i].into_sample();
            r[i +   0] = self.state.samples[0][1][i].into_sample();
            r[i + 576] = self.state.samples[1][1][i].into_sample();
        }

        Ok(())
    }

    fn close(&mut self) {

    }

}
