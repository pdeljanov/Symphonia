# Symphonia SBC Codec

[![Docs](https://docs.rs/symphonia-bundle-sbc/badge.svg)](https://docs.rs/symphonia-bundle-sbc)

SBC (the low-complexity subband codec of Bluetooth A2DP) decoder and demuxer for Project Symphonia.

**Note:** This crate is part of Symphonia. Please use the [`symphonia`](https://crates.io/crates/symphonia) crate instead of this one directly.

## Support

The decoder supports 4 and 8 subbands, 4, 8, 12, and 16 blocks, mono, dual channel, stereo, and joint stereo streams, and the loudness and SNR bit allocation methods. The demuxer reads raw `.sbc` streams, where every frame starts with the sync byte 0x9c. The decoder is a floating point one, and its output matches `ffmpeg` within a few LSBs of 16-bit samples. The mSBC variant of the Bluetooth hands-free profile is not supported.

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is a free and open-source project that welcomes contributions! To get started, please read our [Contribution Guidelines](https://github.com/pdeljanov/Symphonia/blob/main/CONTRIBUTING.md).
