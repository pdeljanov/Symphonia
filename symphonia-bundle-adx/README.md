# Symphonia ADX Codec

[![Docs](https://docs.rs/symphonia-bundle-adx/badge.svg)](https://docs.rs/symphonia-bundle-adx)

CRI ADX decoder and demuxer for Project Symphonia.

**Note:** This crate is part of Symphonia. Please use the [`symphonia`](https://crates.io/crates/symphonia) crate instead of this one directly.

## Support

The decoder supports the ADX files with the fixed predictor (encodings 2 and 3) and any number of channels, and is bit exact with `ffmpeg`. The encodings with an exponential scale (4) and the AHX variant (0x11) are not supported, and neither are the loops that the header may describe.

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is a free and open-source project that welcomes contributions! To get started, please read our [Contribution Guidelines](https://github.com/pdeljanov/Symphonia/blob/main/CONTRIBUTING.md).
