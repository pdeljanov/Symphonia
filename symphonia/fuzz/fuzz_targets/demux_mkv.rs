#![no_main]
use libfuzzer_sys::fuzz_target;
use symphonia::default::formats::MkvReader;
use symphonia_fuzz::fuzz_demuxer;

fuzz_target!(|data: &[u8]| {
    fuzz_demuxer!(data.to_vec(), |mss, fmt: &FormatOptions, _: &MetadataOptions| MkvReader::try_new(
        mss,
        fmt.clone()
    ));
});
