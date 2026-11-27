# Symphonia Speex Codec

[![Docs](https://docs.rs/symphonia-codec-speex/badge.svg)](https://docs.rs/symphonia-codec-speex)

Speex decoder for Project Symphonia.

**Note:** This crate is part of Symphonia. Please use the [`symphonia`](https://crates.io/crates/symphonia) crate instead of this one directly.

## Support

The decoder supports the narrowband (8 kHz), wideband (16 kHz) and ultra wideband (32 kHz) modes with every submode, variable and average bit rates, discontinuous transmission (comfort noise), multiple frames per packet, in-band signalling and intensity stereo. It matches libspeex to within the rounding of 16-bit samples on every file of the conformance corpus (`tools/corpus`).

The Ogg mapping is in `symphonia-format-ogg`. The timestamps of a stream include the decoder delay (the lookahead), the samples of which are not trimmed. Lost packets (packet loss concealment) are not concealed.

## Provenance

The decoder is a port of the decoder of the floating point build of the Speex reference implementation by Xiph.Org, which is available under the revised BSD license. The license and the copyright notices of the reference implementation are in `NOTICE-xiph.txt`, and the code tables are extracted from its sources by script.

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is a free and open-source project that welcomes contributions! To get started, please read our [Contribution Guidelines](https://github.com/pdeljanov/Symphonia/blob/main/CONTRIBUTING.md).
