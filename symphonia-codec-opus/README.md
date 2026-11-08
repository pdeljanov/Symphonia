# Symphonia Opus Codec (`symphonia-codec-opus`)

A 100% safe Rust implementation of the [RFC 6716](https://datatracker.ietf.org/doc/html/rfc6716) (Opus Interactive Audio Codec) and [RFC 7845](https://datatracker.ietf.org/doc/html/rfc7845) (Ogg Encapsulation for the Opus Audio Codec) specifications for the [Symphonia](https://github.com/pdeljanov/Symphonia) multimedia framework.

## Architecture & Module Structure

```text
symphonia-codec-opus
├── src/
│   ├── lib.rs            - Crate root, public exports, #![forbid(unsafe_code)]
│   ├── toc.rs            - Table of Contents (TOC) byte parser (RFC 6716 Section 3.1)
│   ├── packet.rs         - Opus packet framing (Codes 0-3, VBR/CBR, self-delimited, padding)
│   ├── header.rs         - Ogg Opus Identification (OpusHead) and Comment (OpusTags) headers
│   ├── entropy.rs        - Forward range coder and backward raw-bit reader (RFC 6716 Section 4.1)
│   ├── decoder.rs        - Top-level OpusDecoder, Hybrid fusion, mode transitions, multi-stream
│   ├── silk/             - SILK LP layer decoder (RFC 6716 Section 4.2)
│   │   ├── mod.rs        - SILK decoder state, LBRR, DTX, and 48 kHz output orchestration
│   │   ├── constant.rs   - RFC 6716 SILK iCDF tables, codebooks, and filter coefficients
│   │   ├── error.rs      - SILK error definitions
│   │   ├── params.rs     - Header, stereo weights, logarithmic gain, and LTP unpacking
│   │   ├── lsf.rs        - Two-stage VQ NLSF decoding, stabilization, and interpolation
│   │   ├── excitation.rs - Rate-distortion LCG, shell coding, pulse signs, and scaling
│   │   ├── synthesis.rs  - NLSF-to-LPC conversion, LTP synthesis, LPC IIR, stereo unmixing
│   │   ├── resampler.rs  - Stateful polyphase IIR/FIR resampler (8/12/16 kHz -> 48 kHz)
│   │   └── plc.rs        - Packet Loss Concealment (PLC) and Comfort Noise Generation (CNG)
│   └── celt/             - CELT MDCT layer decoder (RFC 6716 Section 4.3)
│       ├── mod.rs        - CELT module exports
│       ├── constant.rs   - Bark frequency bands, bit-allocation tables, window & trig tables
│       ├── error.rs      - CELT error definitions
│       ├── energy.rs     - Coarse (2D IIR + Laplace) and fine energy quantization
│       ├── rate.rs       - Dynamic bit allocation, trim, intensity stereo, dual stereo
│       ├── cwrs.rs       - Combinatorial Pulse Vector Quantization (CWRS) index decoding
│       ├── vq.rs         - Algebraic PVQ search, Haar1 spreading rotations, pitch folding
│       ├── mdct.rs       - Short/long IMDCT wrapper over symphonia_core::dsp::mdct::Imdct
│       └── decoder.rs    - Complete CELT frame synthesis, pre-filter, post-filter, and PLC
└── tests/
    └── rfc6716_conformance.rs - Official IETF RFC 6716 / RFC 8251 testvector01..12 suite
```

## Implementation Status

| Component | Specification | Status | Details |
| :--- | :--- | :--- | :--- |
| **TOC Byte** | RFC 6716 Section 3.1 | 🟢 Implemented | All 32 configurations, modes (SILK, Hybrid, CELT), bandwidths (NB..FB), frame durations (2.5..60 ms) |
| **Packet Framing** | RFC 6716 Section 3.2, App. B | 🟢 Implemented | Code 0, 1, 2, 3 (CBR/VBR), padding, 120 ms validation, self-delimited multi-stream packets |
| **Ogg Headers** | RFC 7845 Section 5 | 🟢 Implemented | `OpusHead` (Mapping Families 0, 1, 255) and `OpusTags` vendor/comment parsing |
| **Entropy Decoder** | RFC 6716 Section 4.1 | 🟢 Implemented | 32-bit range decoder (`icdf`, `logp`, `uint`, Laplace) and backward bit reader (`raw_bits`) |
| **SILK Layer** | RFC 6716 Section 4.2 | 🟢 Implemented | NB/MB/WB mono & stereo, NLSF/LTP/LPC synthesis, 48 kHz resampler, LBRR, DTX/CNG, PLC |
| **CELT Layer** | RFC 6716 Section 4.3 | 🟢 Implemented | Transient/short blocks, coarse/fine/leftover energy, dynamic bit allocation, PVQ/CWRS, IMDCT, post-filter, PLC |
| **Hybrid Mode** | RFC 6716 Section 3.1, 4.3 | 🟢 Implemented | Simultaneous SILK (0..8 kHz) + CELT (8..20 kHz, bands 17..21) synthesis and delay compensation |
| **Mode Transitions** | RFC 6716 Section 4.4 | 🟢 Implemented | 5 ms redundant CELT frame transitions and window crossfades across SILK/Hybrid/CELT boundaries |
| **Multi-Stream** | RFC 7845 Section 5.1.1 | 🟢 Implemented | Coupled (2-channel) and uncoupled (1-channel) stream routing, Vorbis Family 1 (1..8 channels), Q8 output gain |
| **RFC Conformance** | RFC 6716 Section 6.1, RFC 8251 | 🟢 Verified | Verified against `libopus` C oracle and official IETF `testvector01`..`testvector12` |

## Design Principles

- **100% Safe Rust**: Enforced via `#![forbid(unsafe_code)]`.
- **Zero Per-Frame Heap Allocation**: Uses fixed-size RFC-bounded stack arrays and pre-allocated planar stream buffers inside `OpusDecoder`.
- **Deterministic Fixed-Point & Float DSP**: Uses exact RFC 6716 fixed-point integer arithmetic (`Q7`, `Q8`, `Q10`, `Q12`, `Q14`, `Q15`, `Q16`, `Q23`) across SILK stages and `symphonia_core::dsp::mdct::Imdct` `f32` synthesis in CELT.

## Verification

```bash
cargo test -p symphonia-codec-opus -- --test-threads=1
cargo clippy -p symphonia-codec-opus --all-targets
cargo doc -p symphonia-codec-opus --no-deps
```
