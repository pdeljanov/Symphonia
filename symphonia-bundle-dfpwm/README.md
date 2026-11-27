# Symphonia DFPWM Codec

[![Docs](https://docs.rs/symphonia-bundle-dfpwm/badge.svg)](https://docs.rs/symphonia-bundle-dfpwm)

DFPWM (1a) decoder and demuxer for Project Symphonia.

**Note:** This crate is part of Symphonia. Please use the [`symphonia`](https://crates.io/crates/symphonia) crate instead of this one directly.

## Support

The decoder supports the DFPWM 1a variant and is bit exact with `ffmpeg`. A raw `.dfpwm` file has no header, so the file cannot be probed, and the sample rate and the channel count are not stored. `DfpwmReader::try_new` assumes 48000 Hz and mono, like `ffmpeg`, and `DfpwmReader::try_new_with` takes them explicitly. Like `ffmpeg`, the channels of a stream with more than one channel are the consecutive decoded samples of a single bit stream.

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is a free and open-source project that welcomes contributions! To get started, please read our [Contribution Guidelines](https://github.com/pdeljanov/Symphonia/blob/main/CONTRIBUTING.md).
