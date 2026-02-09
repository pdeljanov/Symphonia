#![no_main]
use libfuzzer_sys::fuzz_target;
// use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

fuzz_target!(|data: Vec<u8>| {
    let data = std::io::Cursor::new(data);

    let source = MediaSourceStream::new(Box::new(data), Default::default());

    match symphonia::default::get_probe().probe(
        &Hint::new(),
        source,
        FormatOptions::default(),
        MetadataOptions::default(),
    ) {
        Ok(mut format) => {
            let Some(track) = format.default_track(TrackType::Audio)
            else {
                return;
            };

            let Some(codec_params) = track.codec_params.as_ref()
            else {
                return;
            };

            let Some(audio_codec_params) = codec_params.audio()
            else {
                return;
            };

            let mut decoder = match symphonia::default::get_codecs()
                .make_audio_decoder(&audio_codec_params, &Default::default())
            {
                Ok(d) => d,
                Err(_) => return,
            };

            loop {
                let packet = match format.next_packet() {
                    Ok(Some(p)) => p,
                    _ => return,
                };
                let _ = decoder.decode(&packet);
            }
        }
        Err(_) => {}
    }
});
