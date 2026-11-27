#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_AC3, CODEC_ID_EAC3};
use symphonia::default::codecs::Ac3Decoder;
use symphonia_fuzz::fuzz_raw_decoder;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let codec = if data[0] & 4 != 0 { CODEC_ID_EAC3 } else { CODEC_ID_AC3 };
    fuzz_raw_decoder::<Ac3Decoder>(codec, &data);
});
