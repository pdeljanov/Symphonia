use symphonia::core::codecs::audio::AudioCodecParameters;
use symphonia::core::codecs::registry::RegisterableAudioDecoder;
use symphonia::core::packet::Packet;
use symphonia::core::units::{Duration, Timestamp};

pub fn fuzz_audio_decoder<D: RegisterableAudioDecoder>(
    params: &AudioCodecParameters,
    packet_data: Vec<u8>,
) {
    if let Ok(mut decoder) = D::try_registry_new(params, &Default::default()) {
        let packet = Packet::new(0, Timestamp::ZERO, Duration::ZERO, packet_data);
        let _ = decoder.decode(&packet);
    }
}
