# Symphonia WavPack Codec

This crate contains Project Symphonia's native WavPack reader and decoder implementation. The
reader parses native WavPack blocks and packetizes `.wv` streams. The decoder is a pure Rust
implementation of WavPack decoding, including word decoding, decorrelation, joint stereo, false
stereo, integer and float sample fixup, hybrid lossy streams, embedded correction bitstreams,
trailing metadata, and Matroska `A_WAVPACK4` packets.

DSD is supported for raw mode (`mode 0`) packed-byte output (`U8`). Compressed DSD modes are not
implemented yet. External `.wvc` correction-file reconstruction is not implemented yet.

> [!NOTE]
> This crate is part of Symphonia. Please use the [`symphonia`](https://crates.io/crates/symphonia) crate instead of this one directly.

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is a free and open-source project that welcomes contributions! To get started, please read our [Contribution Guidelines](https://github.com/pdeljanov/Symphonia/blob/main/CONTRIBUTING.md).
