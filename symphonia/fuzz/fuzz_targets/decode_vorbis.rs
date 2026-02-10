#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::{
    AudioCodecParameters, well_known::CODEC_ID_VORBIS,
};
use symphonia::default::codecs::VorbisDecoder;
use symphonia_fuzz::fuzz_audio_decoder;

fuzz_target!(|data: Vec<u8>| {
    let mut codec_params = AudioCodecParameters::new();
    codec_params.for_codec(CODEC_ID_VORBIS);

    // Vorbis decoder requires valid setup headers in extra_data.
    // Since these are complex, we feed the fuzz data into extra_data.
    // If the fuzzer manages to generate valid headers, the decoder is created,
    // and we call decode with an empty packet (or remaining data if we implemented splitting).
    // For now, we focus on fuzzing the header parser (try_new).
    
    // Check if we have enough data to potentially be a header.
    if data.is_empty() {
        return;
    }

    codec_params.extra_data = Some(data.clone().into_boxed_slice());

    // If we successfully created a decoder (fuzzer guessed valid headers!),
    // we try to decode an empty packet or we could try to split the data.
    // Given the complexity, reaching this point is a success for the fuzzer.
    // We pass an empty packet just to exercise the decode path slightly.
    fuzz_audio_decoder::<VorbisDecoder>(&codec_params, vec![]);
});
