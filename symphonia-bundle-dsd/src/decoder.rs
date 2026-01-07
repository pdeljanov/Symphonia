use symphonia_core::audio::{AudioBuffer, AudioBufferRef, Signal, SignalSpec};
use symphonia_core::codecs::{
    CodecDescriptor, CodecParameters, Decoder, DecoderOptions, FinalizeResult, CODEC_TYPE_DSD_LSBF,
    CODEC_TYPE_DSD_MSBF,
};
use symphonia_core::errors::Result;
use symphonia_core::formats::Packet;
use symphonia_core::support_codec;

pub struct DsdDecoder {
    params: CodecParameters,
    buf: AudioBuffer<u32>,
}

impl DsdDecoder {
    fn decode_inner(&mut self, packet: &Packet) {
        let channels = self.params.channels.unwrap().count();
        let src = packet.buf();

        let block_size = src.len() / channels;
        // We pack 4 bytes (32 bits) into 1 u32.
        let samples_out_per_channel = block_size / 4;

        self.buf.clear();
        self.buf.render_reserved(Some(samples_out_per_channel));

        let mut planes = self.buf.planes_mut();
        let raw_planes = planes.planes();

        // Check if we need to reverse bits.
        // CODEC_TYPE_DSD_LSBF means the file is LSBF.
        // Most hardware expects MSBF (native DSD).
        let reverse_bits = self.params.codec == CODEC_TYPE_DSD_LSBF;

        for c in 0..channels {
            let channel_block = &src[c * block_size..(c + 1) * block_size];
            let plane = &mut raw_planes[c];

            for (i, chunk) in channel_block.chunks(4).enumerate() {
                if i < plane.len() && chunk.len() == 4 {
                    // Pack 4 bytes into u32.
                    // u32 from LE bytes to preserve byte order A, B, C, D.
                    let mut bytes = [chunk[0], chunk[1], chunk[2], chunk[3]];

                    if reverse_bits {
                        bytes[0] = bytes[0].reverse_bits();
                        bytes[1] = bytes[1].reverse_bits();
                        bytes[2] = bytes[2].reverse_bits();
                        bytes[3] = bytes[3].reverse_bits();
                    }

                    let val = u32::from_le_bytes(bytes);
                    plane[i] = val;
                }
            }
        }
    }
}

impl Decoder for DsdDecoder {
    fn try_new(params: &CodecParameters, _options: &DecoderOptions) -> Result<Self> {
        let sample_rate = params.sample_rate.unwrap_or(2822400);
        // Pack 32 bits -> rate / 32
        let out_rate = sample_rate / 32;

        let channels = params.channels.unwrap_or_default();
        let frames_in = params.frames_per_block.unwrap_or(32768);
        let capacity = frames_in / 32;

        let spec = SignalSpec::new(out_rate, channels);
        let buf = AudioBuffer::new(capacity, spec);

        Ok(DsdDecoder { params: params.clone(), buf })
    }

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[
            support_codec!(CODEC_TYPE_DSD_LSBF, "dsd_lsbf", "DSD (Least Significant Bit First)"),
            support_codec!(CODEC_TYPE_DSD_MSBF, "dsd_msbf", "DSD (Most Significant Bit First)"),
        ]
    }

    fn reset(&mut self) {
        // No state in this simple decoder
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: &Packet) -> Result<AudioBufferRef<'_>> {
        self.decode_inner(packet);
        Ok(AudioBufferRef::U32(std::borrow::Cow::Borrowed(&self.buf)))
    }

    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }

    fn last_decoded(&self) -> AudioBufferRef<'_> {
        AudioBufferRef::U32(std::borrow::Cow::Borrowed(&self.buf))
    }
}
