#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_DFPWM};
use symphonia::default::codecs::DfpwmDecoder;
use symphonia_fuzz::fuzz_raw_decoder;

fuzz_target!(|data: &[u8]| {
    fuzz_raw_decoder::<DfpwmDecoder>(CODEC_ID_DFPWM, &data);
});
