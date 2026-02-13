#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::audio::layouts::CHANNEL_LAYOUT_STEREO;
use symphonia::core::codecs::audio::AudioCodecParameters;
use symphonia::core::codecs::registry::RegisterableAudioDecoder;
use symphonia::default::codecs::AdpcmDecoder;
use symphonia_fuzz::fuzz_audio_decoder;

fuzz_target!(|data: Vec<u8>| {
    if data.is_empty() {
        return;
    }

    // Dynamically get the list of supported ADPCM codecs.
    let supported_codecs = AdpcmDecoder::supported_codecs();

    // Use the first byte to select the ADPCM codec.
    let codec_idx = data[0] as usize % supported_codecs.len();
    let codec_id = supported_codecs[codec_idx].id;
    
    // Use the rest of the data as payload.
    let packet_data = data[1..].to_vec();

    let mut codec_params = AudioCodecParameters::new();
    codec_params.for_codec(codec_id)
        .with_sample_rate(44100)
        .with_channels(CHANNEL_LAYOUT_STEREO)
        // ADPCM requires block alignment/frames per packet info
        .with_frames_per_block(1024) 
        .with_max_frames_per_packet(1024);

    fuzz_audio_decoder::<AdpcmDecoder>(&codec_params, packet_data);
});
