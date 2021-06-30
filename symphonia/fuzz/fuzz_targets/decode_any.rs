#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

fuzz_target!(|data: Vec<u8>| {
    let data = std::io::Cursor::new(data);

    let source = symphonia::core::io::MediaSourceStream::new(Box::new(data), Default::default());

    match symphonia::default::get_probe().format(
        &Hint::new(),
        source,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) {
        Ok(mut probed) => {
            let track = probed.format.default_track().unwrap();

            let mut decoder = match symphonia::default::get_codecs()
                .make(&track.codec_params, &DecoderOptions::default())
            {
                Ok(d) => d,
                Err(_) => return,
            };

            loop {
                let packet = match probed.format.next_packet() {
                    Ok(p) => p,
                    Err(_) => return,
                };
                let _ = decoder.decode(&packet);
            }
        }
        Err(_) => {}
    }
});
