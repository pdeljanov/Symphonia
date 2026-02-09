#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, well_known::CODEC_ID_MP3,
};
use symphonia::core::packet::Packet;
use symphonia::core::units::{Duration, Timestamp};
use symphonia::default::codecs::MpaDecoder;

fuzz_target!(|data: Vec<u8>| {
    let mut codec_params = AudioCodecParameters::new();
    codec_params.for_codec(CODEC_ID_MP3);

    let mut decoder = MpaDecoder::try_new(&codec_params, &Default::default()).unwrap();

    let packet = Packet::new(0, Timestamp::ZERO, Duration::ZERO, data);
    let _ = decoder.decode(&packet);
});
