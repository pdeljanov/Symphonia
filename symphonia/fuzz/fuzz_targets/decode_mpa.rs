#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::AudioCodecParameters;
use symphonia::core::codecs::registry::RegisterableAudioDecoder;
use symphonia::default::codecs::MpaDecoder;
use symphonia_fuzz::fuzz_audio_decoder;

fuzz_target!(|data: Vec<u8>| {
    if data.is_empty() {
        return;
    }

    // Dynamically get the list of supported MPEG Audio codecs (MP1, MP2, MP3).
    let supported_codecs = MpaDecoder::supported_codecs();

    // Use the first byte to select the codec.
    let codec_idx = data[0] as usize % supported_codecs.len();
    let codec_id = supported_codecs[codec_idx].id;

    // Use the rest of the data as payload.
    let packet_data = data[1..].to_vec();

    let mut codec_params = AudioCodecParameters::new();
    codec_params.for_codec(codec_id);

    fuzz_audio_decoder::<MpaDecoder>(&codec_params, packet_data);
});
