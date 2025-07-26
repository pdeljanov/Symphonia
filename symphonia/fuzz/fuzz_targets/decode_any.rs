#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::TrackType;
use symphonia::core::meta::MetadataOptions;

fuzz_target!(|data: Vec<u8>| {
    let data = std::io::Cursor::new(data);
    let source = symphonia::core::io::MediaSourceStream::new(Box::new(data), Default::default());
    
    if let Ok(mut format) = symphonia::default::get_probe().probe(
        &Hint::new(),
        source,
        FormatOptions::default(),
        MetadataOptions::default(),
    ) {
        if let Some(track) = format.default_track(TrackType::Audio) {
            if let Some(codec_params) = track.codec_params.as_ref() {
                if let Ok(mut decoder) = symphonia::default::get_codecs().make_audio_decoder(
                    &codec_params.audio().unwrap(),
                    &AudioDecoderOptions::default(),
                ) {
                    while let Ok(Some(packet)) = format.next_packet() {
                        let _ = decoder.decode(&packet);
                    }
                }
            }
        }
    }
});
