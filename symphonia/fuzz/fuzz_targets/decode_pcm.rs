#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::audio::layouts::CHANNEL_LAYOUT_STEREO;
use symphonia::core::codecs::audio::{
    AudioCodecParameters,
    well_known::*,
};
use symphonia::core::codecs::registry::RegisterableAudioDecoder;
use symphonia::default::codecs::PcmDecoder;
use symphonia_fuzz::fuzz_audio_decoder;

fuzz_target!(|data: Vec<u8>| {
    if data.is_empty() {
        return;
    }

    // Dynamically get the list of supported PCM codecs.
    let supported_codecs = PcmDecoder::supported_codecs();

    // Use the first byte to select the PCM codec.
    let codec_idx = data[0] as usize % supported_codecs.len();
    let codec_id = supported_codecs[codec_idx].id;

    // Use the rest of the data as payload.
    let packet_data = data[1..].to_vec();

    let mut codec_params = AudioCodecParameters::new();
    codec_params.for_codec(codec_id)
        .with_sample_rate(44100)
        .with_channels(CHANNEL_LAYOUT_STEREO);

    // Set bits_per_sample based on the codec.
    // PcmDecoder requires this for Integer PCM formats (S16, U24, etc.) because it
    // needs to know the coded width to handle potential padding (e.g. 20-bit in 24-bit).
    // It is optional for Float and G.711, but good practice to provide.
    let bits_per_sample = match codec_id {
        CODEC_ID_PCM_S8 | CODEC_ID_PCM_U8 | CODEC_ID_PCM_ALAW | CODEC_ID_PCM_MULAW => 8,
        CODEC_ID_PCM_S16LE | CODEC_ID_PCM_S16BE | CODEC_ID_PCM_U16LE | CODEC_ID_PCM_U16BE => 16,
        CODEC_ID_PCM_S24LE | CODEC_ID_PCM_S24BE | CODEC_ID_PCM_U24LE | CODEC_ID_PCM_U24BE => 24,
        CODEC_ID_PCM_S32LE | CODEC_ID_PCM_S32BE | CODEC_ID_PCM_U32LE | CODEC_ID_PCM_U32BE | CODEC_ID_PCM_F32LE | CODEC_ID_PCM_F32BE => 32,
        CODEC_ID_PCM_F64LE | CODEC_ID_PCM_F64BE => 64,
        _ => 0, // Should be covered above
    };

    if bits_per_sample > 0 {
        codec_params.with_bits_per_sample(bits_per_sample);
    }

    fuzz_audio_decoder::<PcmDecoder>(&codec_params, packet_data);
});
