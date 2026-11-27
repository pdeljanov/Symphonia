#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_NELLYMOSER};
use symphonia::default::codecs::NellymoserDecoder;
use symphonia_fuzz::fuzz_raw_decoder;

fuzz_target!(|data: &[u8]| {
    fuzz_raw_decoder::<NellymoserDecoder>(CODEC_ID_NELLYMOSER, &data);
});
