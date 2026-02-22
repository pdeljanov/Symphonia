#[macro_export]
macro_rules! fuzz_demuxer {
    ($data:expr, $constructor:expr) => {
        {
            use symphonia::core::io::MediaSourceStream;
            use symphonia::core::formats::{FormatOptions, FormatReader};
            use symphonia::core::meta::MetadataOptions;

            let cursor = std::io::Cursor::new($data);
            let mss = MediaSourceStream::new(Box::new(cursor), Default::default());
            
            let format_opts = FormatOptions::default();
            let meta_opts = MetadataOptions::default();

            if let Ok(mut reader) = $constructor(mss, &format_opts, &meta_opts) {
                loop {
                    match reader.next_packet() {
                        Ok(_) => (),
                        Err(_) => break,
                    }
                }
            }
        }
    };
}
