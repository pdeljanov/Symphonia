#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::{CODEC_TYPE_MP3, CodecParameters, Decoder};
use symphonia::core::formats::Packet;
use symphonia::default::codecs::Mp3Decoder;

fuzz_target!(|data: Vec<u8>| {
    let mut codec_params = CodecParameters::new();
    codec_params.for_codec(CODEC_TYPE_MP3);

    let mut decoder = Mp3Decoder::try_new(&codec_params, &Default::default()).unwrap();

    let packet = Packet::new_from_boxed_slice(0, 0, 0, data.into_boxed_slice());
    let _ = decoder.decode(&packet);
});
