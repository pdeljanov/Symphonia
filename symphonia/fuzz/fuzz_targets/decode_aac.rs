#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder};
use symphonia::core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia::core::formats::Packet;
use symphonia::default::codecs::AacDecoder;

fuzz_target!(|data: Vec<u8>| {
    let mut codec_params = AudioCodecParameters::new();
    codec_params.for_codec(CODEC_ID_AAC);

    if let Ok(mut decoder) = AacDecoder::try_new(&codec_params, &Default::default()) {
        let packet = Packet::new_from_boxed_slice(0, 0, 0, data.into_boxed_slice());
        decoder.decode(&packet).ok();
    }
});
