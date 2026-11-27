# Symphonia G.723.1 Codec

[![Docs](https://docs.rs/symphonia-codec-g723/badge.svg)](https://docs.rs/symphonia-codec-g723)

ITU-T G.723.1 decoder for Project Symphonia.

**Note:** This crate is part of Symphonia. Please use the [`symphonia`](https://crates.io/crates/symphonia) crate instead of this one directly.

## Support

The decoder handles mono 8 kHz G.723.1 at both rates, 6.3 kbit/s (multipulse excitation, 24 byte frames) and 5.3 kbit/s (algebraic excitation, 20 byte frames), with the pitch and formant postfilters. A packet holds one or more frames, each of which decodes to 240 samples. The kind of each frame is read from the two low bits of its first byte, so the rates may change from frame to frame.

Frames that fail to unpack, silence insertion descriptors, and untransmitted frames are concealed like lost frames (the pitch is repeated or noise is generated, and the output fades out over three frames). Comfort noise generation of the silence compression annex is not implemented.

The decoder output matches `ffmpeg` to about 50 dB signal to noise ratio on 6.3 kbit/s speech. The two differ in rounding, since `ffmpeg` does not use the fixed-point arithmetic of the Recommendation throughout.

## Provenance

The constant tables are the normative tables of ITU-T G.723.1 (not taken from FFmpeg or from the ITU reference code), extracted by script from `oxideav-g7231` (MIT, Copyright (c) 2026 Karpeles Lab Inc.). The fixed-point reconstruction of the excitation, the postfilters, the concealment, and the bit stream conventions (which `oxideav-g7231` verified against the conformance bit streams of the Recommendation) follow that crate, and the code was written for this crate. The MIT notice is reproduced in the NOTICE file.

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details. The notice of the MIT licensed source is in the NOTICE file.

## Contributing

Symphonia is a free and open-source project that welcomes contributions! To get started, please read our [Contribution Guidelines](https://github.com/pdeljanov/Symphonia/blob/main/CONTRIBUTING.md).
