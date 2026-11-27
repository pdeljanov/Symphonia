# Symphonia FLV Format

[![Docs](https://docs.rs/symphonia-format-flv/badge.svg)](https://docs.rs/symphonia-format-flv)

Flash Video (FLV) demuxer for Project Symphonia.

**Note:** This crate is part of Symphonia. Please use the [`symphonia`](https://crates.io/crates/symphonia) crate instead of this one directly.

## Support

The demuxer reads the first audio stream of a file. AAC, MP3, PCM (little endian), A-law and mu-law audio is supported; other codecs (ADPCM, Nellymoser, Speex, and so on) are reported as unsupported. Video and script data are skipped. A file can be sought by reading it again from its start, because FLV has no index.

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is a free and open-source project that welcomes contributions! To get started, please read our [Contribution Guidelines](https://github.com/pdeljanov/Symphonia/blob/main/CONTRIBUTING.md).
