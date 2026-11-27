#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_MONKEYS_AUDIO};
use symphonia::default::codecs::ApeDecoder;
use symphonia_fuzz::fuzz_raw_decoder;

fuzz_target!(|data: &[u8]| {
    fuzz_raw_decoder::<ApeDecoder>(CODEC_ID_MONKEYS_AUDIO, &data);
});
