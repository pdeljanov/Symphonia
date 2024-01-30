#[derive(Debug)]
pub(crate) enum Compression {
    Zlib,
    Bzlib,
    Lzo1x,
    HeaderStripping
}

pub(crate) fn decompress(data: &[u8], algorithm: &Compression, settings: &[u8]) -> Box<[u8]> {
    match algorithm {
        Compression::HeaderStripping => {
            [settings, data].concat().into_boxed_slice()
        },
        Compression::Zlib => { log::warn!("mkv: unimplemented compression algorithm [zlib]. Expect errors."); data.to_vec().into_boxed_slice() },
        Compression::Bzlib => { log::warn!("mkv: unimplemented compression algorithm [bzlib]. Expect errors."); data.to_vec().into_boxed_slice() },
        Compression::Lzo1x => { log::warn!("mkv: unimplemented compression algorithm [lzo1x]. Expect errors."); data.to_vec().into_boxed_slice() },
    }
}