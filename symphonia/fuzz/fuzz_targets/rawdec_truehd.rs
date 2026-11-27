#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_TRUEHD, CODEC_ID_MLP};
use symphonia::default::codecs::TrueHdDecoder;
use symphonia_fuzz::fuzz_raw_decoder;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let codec = if data[0] & 4 != 0 { CODEC_ID_MLP } else { CODEC_ID_TRUEHD };
    fuzz_raw_decoder::<TrueHdDecoder>(codec, &data);
});
