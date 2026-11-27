#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_ADPCM_SWF};
use symphonia::default::codecs::SwfAdpcmDecoder;
use symphonia_fuzz::fuzz_raw_decoder;

fuzz_target!(|data: &[u8]| {
    fuzz_raw_decoder::<SwfAdpcmDecoder>(CODEC_ID_ADPCM_SWF, &data);
});
