# Symphonia Nellymoser Codec

[![Docs](https://docs.rs/symphonia-codec-nellymoser/badge.svg)](https://docs.rs/symphonia-codec-nellymoser)

Nellymoser Asao decoder for Project Symphonia.

**Note:** This crate is part of Symphonia. Please use the [`symphonia`](https://crates.io/crates/symphonia) crate instead of this one directly.

## Support

The decoder handles mono Nellymoser Asao as carried in FLV (sound formats 4, 5, and 6) at any sample rate. A packet holds one or more 64 byte blocks, each of which decodes to 256 samples.

Coefficients that carry no mantissa bits are replaced by noise of a fixed magnitude and a pseudo-random sign. The format does not define that sign, so the output differs from `ffmpeg` by that noise (about 33 dB below the signal on the test stream), and is otherwise equal.

## Provenance

The constant tables, the band layout, and the bit allocation procedure follow `nellymoser-rs` (MIT, Copyright (c) 2021 relrelb, derived from nelly2pcm and the MIT licensed Nellymoser files of FFmpeg). The inverse transform is a direct form implementation written for this crate. The MIT notice is reproduced in the NOTICE file.

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details. The notice of the MIT licensed sources is in the NOTICE file.

## Contributing

Symphonia is a free and open-source project that welcomes contributions! To get started, please read our [Contribution Guidelines](https://github.com/pdeljanov/Symphonia/blob/main/CONTRIBUTING.md).
