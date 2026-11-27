#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::default::formats::Ac3Reader;
use symphonia_fuzz::fuzz_demuxer_decode;

fuzz_target!(|data: &[u8]| {
    fuzz_demuxer_decode!(data.to_vec(), |mss, fmt: &FormatOptions, _: &MetadataOptions| Ac3Reader::try_new(
        mss,
        fmt.clone()
    ));
});
