#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::{
    AudioCodecParameters, well_known::CODEC_ID_ALAC,
};
use symphonia::default::codecs::AlacDecoder;
use symphonia_fuzz::fuzz_audio_decoder;

fuzz_target!(|data: Vec<u8>| {
    let mut codec_params = AudioCodecParameters::new();
    codec_params.for_codec(CODEC_ID_ALAC);

    // ALAC decoder requires a Magic Cookie in extra_data.
    // We try to use the first part of the fuzz data as extra_data if possible.
    // Minimum valid cookie size is 24 bytes.
    let (header, packet_data) = if data.len() >= 24 {
        // Use a variable length for header to let fuzzer explore different cookie sizes/formats
        // but ensure at least 24 bytes are used if available.
        // We'll arbitrarily say up to 48 bytes (another common size) or just the first 24-48 bytes.
        // Let's split at 48 if available, or just split at 24.
        let split_idx = if data.len() >= 48 { 48 } else { 24 };
        let (h, b) = data.split_at(split_idx);
        (Some(h.to_vec().into_boxed_slice()), b)
    } else {
        (Some(data.to_vec().into_boxed_slice()), &[][..])
    };

    codec_params.extra_data = header;

    // ALAC decoder try_new parses the extra data immediately.
    // If successful, we decode the remaining data.
    if !packet_data.is_empty() {
        fuzz_audio_decoder::<AlacDecoder>(&codec_params, packet_data.to_vec());
    } else {
         // Even if empty, we might want to check if try_new doesn't panic on just header
         fuzz_audio_decoder::<AlacDecoder>(&codec_params, vec![]);
    }
});
