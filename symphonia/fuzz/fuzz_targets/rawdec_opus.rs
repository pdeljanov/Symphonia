#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_OPUS};
use symphonia::default::codecs::OpusDecoder;
use symphonia_fuzz::fuzz_raw_decoder;

fuzz_target!(|data: &[u8]| {
    fuzz_raw_decoder::<OpusDecoder>(CODEC_ID_OPUS, &data);
});
