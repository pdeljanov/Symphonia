#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::audio::layouts::CHANNEL_LAYOUT_STEREO;
use symphonia::core::codecs::audio::{
    AudioCodecParameters, well_known::CODEC_ID_AAC,
};
use symphonia::default::codecs::AacDecoder;
use symphonia_fuzz::fuzz_audio_decoder;

fuzz_target!(|data: Vec<u8>| {
    let mut codec_params = AudioCodecParameters::new();
    codec_params.for_codec(CODEC_ID_AAC)
        .with_sample_rate(44100)
        .with_channels(CHANNEL_LAYOUT_STEREO);

    // We initialize the AAC decoder in raw mode (no extra_data/ASC), relying on
    // provided parameters. This allows the fuzzer to directly attack the packet decoding logic.
    fuzz_audio_decoder::<AacDecoder>(&codec_params, data);
});
