pub mod demuxer;

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

use symphonia::core::audio::layouts::{
    CHANNEL_LAYOUT_5P1, CHANNEL_LAYOUT_7P1, CHANNEL_LAYOUT_MONO, CHANNEL_LAYOUT_STEREO,
};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioCodecId;
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatReader;

const MAX_PACKETS: usize = 256;
const RATES: [u32; 8] = [8000, 11025, 16000, 22050, 32000, 44100, 48000, 96000];
const BITS: [u32; 4] = [8, 16, 24, 32];

/// Decodes the packets that a demuxer produces with the decoders that the registry picks for the
/// parameters of the tracks.
pub fn decode_all_packets(mut reader: Box<dyn FormatReader + '_>) {
    let mut decoders = std::collections::HashMap::new();

    for track in reader.tracks() {
        if let Some(CodecParameters::Audio(params)) = track.codec_params.as_ref() {
            if let Ok(decoder) =
                symphonia::default::get_codecs().make_audio_decoder(params, &Default::default())
            {
                decoders.insert(track.id, decoder);
            }
        }
    }

    for _ in 0..MAX_PACKETS {
        let packet = match reader.next_packet() {
            Ok(Some(packet)) => packet,
            _ => return,
        };
        if let Some(decoder) = decoders.get_mut(&packet.track_id) {
            if let Err(Error::ResetRequired) = decoder.decode(&packet) {
                decoder.reset();
            }
        }
    }
}

/// Decodes a stream of packets without a demuxer. The first bytes of the input choose the
/// parameters (channel layout, sample rate, bits per sample, extra data), the rest is split into
/// packets that are each prefixed with a 16-bit little endian length.
pub fn fuzz_raw_decoder<D: RegisterableAudioDecoder>(codec: AudioCodecId, data: &[u8]) {
    if data.len() < 4 {
        return;
    }
    let channels = match data[0] & 3 {
        0 => CHANNEL_LAYOUT_MONO,
        1 => CHANNEL_LAYOUT_STEREO,
        2 => CHANNEL_LAYOUT_5P1,
        _ => CHANNEL_LAYOUT_7P1,
    };
    let rate = RATES[usize::from(data[1] & 7)];
    let bits = BITS[usize::from(data[1] >> 6)];
    let extra_len = usize::from(data[2]).min(data.len() - 3);
    let extra = data[3..3 + extra_len].to_vec().into_boxed_slice();
    let mut rest = &data[3 + extra_len..];

    let mut params = AudioCodecParameters::new();
    params
        .for_codec(codec)
        .with_sample_rate(rate)
        .with_channels(channels)
        .with_bits_per_sample(bits)
        .with_bits_per_coded_sample(bits)
        .with_max_frames_per_packet(4096)
        .with_frames_per_block(1024);
    if !extra.is_empty() {
        params.with_extra_data(extra);
    }

    let Ok(mut decoder) = D::try_registry_new(&params, &Default::default())
    else {
        return;
    };

    for _ in 0..MAX_PACKETS {
        if rest.len() < 2 {
            return;
        }
        let len = usize::from(u16::from_le_bytes([rest[0], rest[1]])).min(rest.len() - 2);
        let packet = Packet::new(0, Timestamp::ZERO, Duration::ZERO, rest[2..2 + len].to_vec());
        rest = &rest[2 + len..];
        if let Err(Error::ResetRequired) = decoder.decode(&packet) {
            decoder.reset();
        }
    }
}
