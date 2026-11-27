#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_TTA};
use symphonia::default::codecs::TtaDecoder;
use symphonia_fuzz::fuzz_raw_decoder;

fuzz_target!(|data: &[u8]| {
    fuzz_raw_decoder::<TtaDecoder>(CODEC_ID_TTA, &data);
});
