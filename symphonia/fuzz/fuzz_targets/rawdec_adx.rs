#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_ADPCM_ADX};
use symphonia::default::codecs::AdxDecoder;
use symphonia_fuzz::fuzz_raw_decoder;

fuzz_target!(|data: &[u8]| {
    fuzz_raw_decoder::<AdxDecoder>(CODEC_ID_ADPCM_ADX, &data);
});
