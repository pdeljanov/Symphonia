# Symphonia MPEG Transport Stream Format

[![Docs](https://docs.rs/symphonia-format-ts/badge.svg)](https://docs.rs/symphonia-format-ts)

MPEG transport stream (TS) demuxer for Project Symphonia.

**Note:** This crate is part of Symphonia. Please use the [`symphonia`](https://crates.io/crates/symphonia) crate instead of this one directly.

## Support

The demuxer reads the first audio stream of the first program of a stream of 188-byte packets, which may be a file or an HLS segment. AAC in ADTS (stream type 0x0f), MPEG audio (0x03 and 0x04), AC-3 (0x81, or private data with the AC-3 descriptor), and DTS (0x82, or private data with the DTS descriptor) are supported. Video and other data are skipped. The timestamps are derived from the frames, not from the presentation timestamps of the PES packets, and a file can be sought by reading it again from its start. LATM, E-AC-3, and streams with 192-byte packets are not supported.

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is a free and open-source project that welcomes contributions! To get started, please read our [Contribution Guidelines](https://github.com/pdeljanov/Symphonia/blob/main/CONTRIBUTING.md).
