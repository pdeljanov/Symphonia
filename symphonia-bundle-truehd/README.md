# Symphonia TrueHD Codec

[![Docs](https://docs.rs/symphonia-bundle-truehd/badge.svg)](https://docs.rs/symphonia-bundle-truehd)

Dolby TrueHD and Meridian Lossless Packing (MLP) decoder and elementary stream demuxer for Project Symphonia.

**Note:** This crate is part of Symphonia. Please use the [`symphonia`](https://crates.io/crates/symphonia) crate instead of this one directly.

## Support

The demuxer reads raw TrueHD (`.thd`) and MLP (`.mlp`) streams, and the decoder is bit exact with `ffmpeg` on the streams of its encoders. Only the first substream of every access unit is decoded, which is the stereo presentation of a TrueHD stream and the whole of an MLP stream, so the multichannel presentations and the object audio of a TrueHD stream are not decoded. The restart headers of the substreams that use the 31EB and 31EC sync words are not supported either.

## Credits

The bitstream syntax (major sync, restart header, block header, Huffman trees, filters, matrixing and the noise generator) was cross-checked against the [`truehd`](https://github.com/truehdd/truehdd) crate by Rainbaby, which is licensed under the Apache License 2.0. No code was copied, the decoder was written from scratch for Symphonia.

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is a free and open-source project that welcomes contributions! To get started, please read our [Contribution Guidelines](https://github.com/pdeljanov/Symphonia/blob/main/CONTRIBUTING.md).
