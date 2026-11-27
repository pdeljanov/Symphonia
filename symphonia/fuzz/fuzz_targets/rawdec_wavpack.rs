#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_WAVPACK};
use symphonia::default::codecs::WavPackDecoder;
use symphonia_fuzz::fuzz_raw_decoder;

fuzz_target!(|data: &[u8]| {
    fuzz_raw_decoder::<WavPackDecoder>(CODEC_ID_WAVPACK, &data);
});
