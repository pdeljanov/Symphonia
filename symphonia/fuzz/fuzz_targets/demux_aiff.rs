#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::default::formats::AiffReader;
use symphonia_fuzz::fuzz_demuxer;

fuzz_target!(|data: &[u8]| {
    fuzz_demuxer!(data.to_vec(), |mss, fmt: &FormatOptions, _: &MetadataOptions| AiffReader::try_new(
        mss,
        fmt.clone()
    ));
});
