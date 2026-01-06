#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::meta::MetadataOptions;

fuzz_target!(|data: Vec<u8>| {
    let data = std::io::Cursor::new(data);

    let source = symphonia::core::io::MediaSourceStream::new(Box::new(data), Default::default());

    let Ok(mut format) = symphonia::default::get_probe().probe(
        &Hint::new(),
        source,
        FormatOptions::default(),
        MetadataOptions::default(),
    )
    else {
        return;
    };

    // Find the first audio track with a known (decodeable) codec.
    let Some(track) = format.default_track(TrackType::Audio)
    else {
        return;
    };

    // Use the default options for the decoder.
    let dec_opts: AudioDecoderOptions = Default::default();

    // Create a decoder for the track.
    let audio_params =
        track.codec_params.as_ref().expect("codec parameters missing").audio().unwrap();
    let Ok(mut decoder) =
        symphonia::default::get_codecs().make_audio_decoder(audio_params, &dec_opts)
    else {
        return;
    };

    loop {
        let Ok(Some(packet)) = format.next_packet()
        else {
            return;
        };
        let _ = decoder.decode(&packet);
    }
});
