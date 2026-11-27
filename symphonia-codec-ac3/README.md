# Symphonia AC-3 Codec

[![Docs](https://docs.rs/symphonia-codec-ac3/badge.svg)](https://docs.rs/symphonia-codec-ac3)

AC-3 (Dolby Digital) and E-AC-3 (Dolby Digital Plus) decoder and raw elementary stream demuxer for Project Symphonia.

**Note:** This crate is part of Symphonia. Please use the [`symphonia`](https://crates.io/crates/symphonia) crate instead of this one directly.

## Support

The decoder implements ETSI TS 102 366 (ATSC A/52) clauses 4 to 6 for all audio coding modes (1+1 through 3/2, with or without LFE), all three sample rates (and the half rates of bit stream ids 9 and 10), exponent strategies, parametric and delta bit allocation, grouped mantissas, dither, channel coupling, rematrixing, dynamic range control, and block switching. Channels are output as coded, without downmixing.

E-AC-3 (annex E) is decoded for independent substream 0 with 1, 2, 3, or 6 blocks per frame, standard coupling, the adaptive hybrid transform (vector and gain adaptive quantization), spectral extension, and the syntax flags and strategy tables of the audio frame. Dependent substreams and further programmes are skipped, so streams with more than 5.1 channels decode as their 5.1 core.

Not supported: enhanced coupling (streams that use it fail with an unsupported error), transient pre-noise processing of E-AC-3 (ignored), the reduced sample rates of E-AC-3, AC-3 in WAVE files, and downmixing. The `verify` decoder option checks the CRC of every frame.

The decoder output matches `ffmpeg` apart from the noise that replaces zero-bit mantissas (dither), and the noise that spectral extension blends in. The standard allows any random sequence for that noise, so the two decoders differ by noise of about -20 dB relative to the quantization noise floor. Setting the environment variable `AC3_NODITHER` disables dither, which the conformance scripts in `tools/corpus` use to show that all remaining differences stem from the dither.

## Provenance

The decoder was written from the specification. The constant tables were checked against the specification text, and the vector quantizer and remapping tables of the adaptive hybrid transform were generated from it. The scale factor of the inverse DCT of the adaptive hybrid transform (the square root of 2, where the specification prints 2) and the clarification of the gain adaptive quantizer widths follow `oxideav-ac3` (MIT, Copyright (c) 2026 Karpeles Lab Inc.), and were verified against `ffmpeg`.

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is a free and open-source project that welcomes contributions! To get started, please read our [Contribution Guidelines](https://github.com/pdeljanov/Symphonia/blob/main/CONTRIBUTING.md).
