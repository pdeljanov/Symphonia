#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::{
    AudioCodecParameters, well_known::CODEC_ID_FLAC,
};
use symphonia::default::codecs::FlacDecoder;
use symphonia_fuzz::fuzz_audio_decoder;

fuzz_target!(|data: Vec<u8>| {
    let mut codec_params = AudioCodecParameters::new();
    codec_params.for_codec(CODEC_ID_FLAC);

    // FLAC decoder requires a STREAMINFO block in extra_data.
    // We try to use the first part of the fuzz data as extra_data if possible,
    // to allow the fuzzer to explore both header parsing and frame decoding.
    // STREAMINFO is 34 bytes.
    let (header, packet_data) = if data.len() >= 34 {
        let (h, b) = data.split_at(34);
        (Some(h.to_vec().into_boxed_slice()), b)
    } else {
        (Some(data.to_vec().into_boxed_slice()), &[][..])
    };

    codec_params.extra_data = header;

    if !packet_data.is_empty() {
        fuzz_audio_decoder::<FlacDecoder>(&codec_params, packet_data.to_vec());
    }
});
