# Symphonia DCA (DTS) Codec

[![Docs](https://docs.rs/symphonia-codec-dca/badge.svg)](https://docs.rs/symphonia-codec-dca)

DTS Coherent Acoustics (DCA) decoder for Project Symphonia.

**Note:** This crate is part of Symphonia. Please use the [`symphonia`](https://crates.io/crates/symphonia) crate instead of this one directly.

## Support

The decoder follows the DTS core profile of ETSI TS 102 114. It matches `ffmpeg` to within rounding on every file of the conformance corpus (`tools/corpus`), including real 5.1 and 6.1 (DTS-ES) streams.

| Feature | Status |
|---------|--------|
| Sample rates | 8, 11.025, 12, 16, 22.05, 24, 32, 44.1 and 48 kHz (no resampling to the hardware rates) |
| Channel arrangements | AMODE 0 to 9 (mono to 5.0), the LFE channel, and the extra centre surround channel (XCh) of DTS-ES, decoded as 6.1 |
| Bit stream packing | 16-bit and 14-bit words, big and little endian |
| Coding tools | Huffman, block and plain quantization, joint intensity coding, sum/difference stereo, transient scale factors, perfect and non-perfect reconstruction filter banks, 64x and 128x LFE interpolation |
| Not supported | Subband ADPCM prediction and high frequency vector quantization (see Provenance), AMODE 10 to 15, the extensions XXCH, X96, XBR and LBR (the core is decoded), dynamic range control, termination frames, downmixing |

The decoder does not check the optional CRCs.

## Provenance

The decoder is written from ETSI TS 102 114, and its tables are extracted from the text of the specification by script. The two vector quantization codebooks of annex D.10 (the ADPCM prediction coefficients and the high frequency subbands) are omitted from the specification. They exist in FFmpeg (`libavcodec/dcadata.c`, LGPL), which cannot be used here, so a stream that uses subband ADPCM prediction or high frequency vector quantization is rejected with an unsupported error.

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is a free and open-source project that welcomes contributions! To get started, please read our [Contribution Guidelines](https://github.com/pdeljanov/Symphonia/blob/main/CONTRIBUTING.md).
