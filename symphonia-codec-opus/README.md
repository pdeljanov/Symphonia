# Symphonia Opus Codec (`symphonia-codec-opus`)

A 100% safe Rust (`#![forbid(unsafe_code)]`) implementation of the Opus interactive audio codec ([RFC 6716](https://datatracker.ietf.org/doc/html/rfc6716), [RFC 8251](https://datatracker.ietf.org/doc/html/rfc8251)) and Ogg Opus encapsulation ([RFC 7845](https://datatracker.ietf.org/doc/html/rfc7845)) for the [Symphonia](https://github.com/pdeljanov/Symphonia) multimedia framework.

---

## 1. Overview & RFC Specifications

Opus is a low-latency audio codec standardized by the IETF in [RFC 6716](https://datatracker.ietf.org/doc/html/rfc6716). It combines two coding technologies within a single unified framing and range-coding bitstream ([RFC 6716 Section 2](https://datatracker.ietf.org/doc/html/rfc6716#section-2)):

1. **SILK ([RFC 6716 Section 4.2](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2))**: Linear Predictive Coding (LPC) and Long-Term Pitch Prediction (LTP) optimized for speech signals from Narrowband (8 kHz) up to Wideband (16 kHz) over 10, 20, 40, or 60 ms frames.
2. **CELT ([RFC 6716 Section 4.3](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3))**: Constrained Energy Lapped Transform (MDCT + Algebraic Pyramid Vector Quantization) optimized for music, transients, and fullband audio from Narrowband (8 kHz) up to Fullband (48 kHz) over 2.5, 5, 10, or 20 ms frames.
3. **Hybrid Mode ([RFC 6716 Section 3.1](https://datatracker.ietf.org/doc/html/rfc6716#section-3.1), [Section 4.3](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3))**: Simultaneously combines SILK (`0..8 kHz` WB) and CELT (`8..12 kHz` SWB or `8..20 kHz` FB, bands `17..21`) over 10 or 20 ms frames.

| Mode | TOC Configs | Bandwidths ([Table 1](https://datatracker.ietf.org/doc/html/rfc6716#section-2)) | Frame Durations | Active Layers |
| :--- | :--- | :--- | :--- | :--- |
| **SILK-only** | `0..=11` | NB (8 kHz), MB (12 kHz), WB (16 kHz) | 10, 20, 40, 60 ms | `silk::Decoder` |
| **Hybrid** | `12..=15` | SWB (24 kHz), FB (48 kHz) | 10, 20 ms | `silk::Decoder` (`0..8 kHz`) + `celt::Decoder` (bands `17..21`) |
| **CELT-only** | `16..=31` | NB (8 kHz), WB (16 kHz), SWB (24 kHz), FB (48 kHz) | 2.5, 5, 10, 20 ms | `celt::Decoder` (bands `0..end_band`) |

---

## 2. Crate Architecture & Module Structure

```text
symphonia-codec-opus
├── src/
│   ├── lib.rs            - Crate root, public exports, #![forbid(unsafe_code)]
│   ├── toc.rs            - TOC byte: configuration table, stereo flag, frame count code (RFC 6716 Section 3.1)
│   ├── packet.rs         - Packet framing for Codes 0 to 3 and self-delimited packets, no heap allocation (RFC 6716 Section 3.2, Appendix B)
│   ├── header.rs         - Ogg Opus identification header and channel mapping table (RFC 7845 Section 5.1)
│   ├── entropy.rs        - Range decoder, raw-bit reader and compile-time validated CDF tables (RFC 6716 Section 4.1)
│   ├── fixed.rs          - Q-format fixed-point arithmetic shared by the SILK layer (RFC 6716 Section 4.2)
│   ├── sample_rate.rs    - Output sample rates, samples per frame duration, 48 kHz decimation factor
│   ├── planes.rs         - Planar f32 channel pair handed down the decode chain
│   ├── decoder.rs        - OpusDecoder: layer sequencing, loss concealment, transitions and redundancy, multi-stream routing (RFC 6716 Sections 4.4, 4.5; RFC 7845 Section 5.1.1)
│   ├── silk/             - SILK linear prediction layer (RFC 6716 Section 4.2)
│   │   ├── mod.rs        - Frame geometry (SilkLayout), channel state and coding references, frame types
│   │   ├── constant.rs   - CDF tables, codebooks and filter coefficients
│   │   ├── error.rs      - SILK error type
│   │   ├── params.rs     - Header flags, stereo weights, gains and LTP parameters (Sections 4.2.3 to 4.2.7.6)
│   │   ├── lsf.rs        - NLSF decoding, stabilisation and interpolation (Section 4.2.7.5)
│   │   ├── lpc.rs        - NLSF to LPC conversion, coefficient limits and prediction kernels (Sections 4.2.7.5.6 to 4.2.7.5.8, 4.2.7.9)
│   │   ├── excitation.rs - LCG seed and shell-coded excitation (Sections 4.2.7.7, 4.2.7.8)
│   │   ├── synthesis.rs  - LTP and LPC synthesis, stereo unmixing (Sections 4.2.7.9, 4.2.8)
│   │   ├── resampler.rs  - Resampling to the output rate (Section 4.2.9)
│   │   ├── plc.rs        - Loss concealment and comfort noise (Section 4.4, non-normative)
│   │   └── decoder.rs    - Channel transitions and the LBRR and regular frame schedule (Sections 4.2.4 to 4.2.6)
│   └── celt/             - CELT transform layer (RFC 6716 Section 4.3)
│       ├── mod.rs        - Block size (LM), band constants, silence frame, module index
│       ├── constant.rs   - Band boundaries, energy models, allocation tables, window
│       ├── error.rs      - CELT error type
│       ├── energy.rs     - Coarse, fine and final energy decoding (Section 4.3.2)
│       ├── rate.rs       - Bit allocation and time-frequency parameters (Sections 4.3.1, 4.3.3)
│       ├── cwrs.rs       - PVQ codeword index decoding (Section 4.3.4.2)
│       ├── vq.rs         - PVQ shape decoding, spreading, folding, anti-collapse (Sections 4.3.4, 4.3.5)
│       ├── fft.rs        - Mixed-radix FFT kernel of the inverse MDCT (Section 4.3.7)
│       ├── mdct.rs       - Inverse MDCT with overlap-add (Section 4.3.7)
│       ├── synthesis.rs  - Band denormalisation and de-emphasis (Sections 4.3.6, 4.3.7.2)
│       ├── postfilter.rs - Pitch comb post-filter (Section 4.3.7.1)
│       ├── plc.rs        - LPC analysis and pitch search for concealment (Section 4.4)
│       └── decoder.rs    - CELT frame decoder state and synthesis orchestration (Sections 4.3, 4.4)
└── tests/
    └── rfc6716_conformance.rs - RFC 6716 Section 6 conformance on the RFC 8251 vectors (ignored unless OPUS_TESTVECTORS_DIR is set)
```

### Implementation Status

| Component | Normative Specification | Status | Details |
| :--- | :--- | :--- | :--- |
| **TOC Byte** | [RFC 6716 Section 3.1](https://datatracker.ietf.org/doc/html/rfc6716#section-3.1) | 🟢 Implemented | All 32 configurations, modes (SILK, Hybrid, CELT), bandwidths (NB..FB), frame durations (2.5..60 ms) |
| **Packet Framing** | [RFC 6716 Section 3.2](https://datatracker.ietf.org/doc/html/rfc6716#section-3.2), [Appendix B](https://datatracker.ietf.org/doc/html/rfc6716#appendix-B) | 🟢 Implemented | Code 0, 1, 2, 3 (CBR/VBR), padding, 120 ms validation, self-delimited multi-stream packets |
| **Ogg Headers** | [RFC 7845 Section 5](https://datatracker.ietf.org/doc/html/rfc7845#section-5) | 🟢 Implemented | `OpusHead` identification header with the channel mapping table for every family other than 0 and per-family channel bounds (RFC 7845 Section 5.1.1); `OpusTags` is left to the Ogg demuxer |
| **Entropy Decoder** | [RFC 6716 Section 4.1](https://datatracker.ietf.org/doc/html/rfc6716#section-4.1) | 🟢 Implemented | 32-bit range decoder over compile-time validated CDF tables (`decode_symbol`, `decode_symbol_logp`, `decode_uint`, `decode_laplace`) and backward raw-bit reader |
| **SILK Layer** | [RFC 6716 Section 4.2](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2) | 🟢 Implemented | NB/MB/WB mono & stereo, NLSF/LTP/LPC synthesis, 48 kHz resampler, LBRR, DTX/CNG, PLC |
| **CELT Layer** | [RFC 6716 Section 4.3](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3) | 🟢 Implemented | Transient/short blocks, coarse/fine/leftover energy, dynamic bit allocation, PVQ/CWRS, IMDCT, post-filter, PLC |
| **Hybrid Mode** | [RFC 6716 Section 3.1](https://datatracker.ietf.org/doc/html/rfc6716#section-3.1), [Section 4.3](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3) | 🟢 Implemented | Simultaneous SILK (0..8 kHz) + CELT (8..20 kHz, bands 17..21) synthesis and delay compensation |
| **Mode Transitions** | [RFC 6716 Section 4.5](https://datatracker.ietf.org/doc/html/rfc6716#section-4.5) | 🟢 Implemented | 5 ms redundant CELT frame transitions and window crossfades across SILK/Hybrid/CELT boundaries |
| **Multi-Stream** | [RFC 7845 Section 5.1.1](https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1) | 🟢 Implemented | Coupled (2-channel) and uncoupled (1-channel) stream routing, Vorbis Family 1 (1..8 channels), Q8 output gain |
| **RFC Conformance** | [RFC 6716 Section 6.1](https://datatracker.ietf.org/doc/html/rfc6716#section-6.1), [RFC 8251](https://datatracker.ietf.org/doc/html/rfc8251) | 🟢 Passing at every output rate | `tests/rfc6716_conformance.rs` decodes the 12 RFC 8251 test vectors (`OPUS_TESTVECTORS_DIR`) at 8, 12, 16, 24 and 48 kHz in mono and stereo, requires the final range decoder state to equal the reference on every packet and passes the `opus_compare` metric against the `.dec` set in all 120 runs (quality 96.2 to 100.0 at 48 kHz; 43.4 to 98.1 at reduced rates, where RFC 6716 Section 6.1 expects lower figures from the internal rate conversion). The unit-test fixtures carry values recorded from libopus 1.6.1 and are labelled by provenance; the normative reference is RFC 6716 Appendix A as updated by RFC 8251. |

---

## 3. Ogg Opus Encapsulation & Packet Framing (`header.rs`, `toc.rs`, `packet.rs`)

### Packet Organization in a Logical Ogg Opus Stream ([RFC 7845 Section 3, Figure 1](https://datatracker.ietf.org/doc/html/rfc7845#section-3))

```text
        Page 0         Pages 1 ... n        Pages (n+1) ...
     +------------+ +---+ +---+ ... +---+ +-----------+ +---------+ +--
     |            | |   | |   |     |   | |           | |         | |
     |+----------+| |+-----------------+| |+-------------------+ +-----
     |||ID Header|| ||  Comment Header || ||Audio Data Packet 1| | ...
     |+----------+| |+-----------------+| |+-------------------+ +-----
     |            | |   | |   |     |   | |           | |         | |
     +------------+ +---+ +---+ ... +---+ +-----------+ +---------+ +--
     ^      ^                           ^
     |      |                           |
     |      |                           Mandatory Page Break
     |      |
     |      ID header is contained on a single page
     |
     'Beginning Of Stream'
```

### Opus Packet Layout & TOC Byte ([RFC 6716 Section 3.1, Figure 1](https://datatracker.ietf.org/doc/html/rfc6716#section-3.1) & [Section 3.2](https://datatracker.ietf.org/doc/html/rfc6716#section-3.2))

```text
  Packet Layout:                        TOC Byte Layout (Figure 1):
  +----------+--------------+---------+   0 1 2 3 4 5 6 7
  | TOC Byte | Frame Packet | Padding |  +-+-+-+-+-+-+-+-+
  +----------+--------------+---------+  | config  |s| c |
                                         +-+-+-+-+-+-+-+-+
```

- **config** (bits 0..4, [RFC 6716 Table 2](https://datatracker.ietf.org/doc/html/rfc6716#section-3.1)): Encodes coding mode (SILK, Hybrid, or CELT), audio bandwidth, and frame duration (`2.5 ms`..`60 ms`).
- **s** (bit 5): Stereo flag (`0` = mono, `1` = stereo).
- **c** (bits 6..7): Frame count code:
  - `0`: 1 frame in packet ([Section 3.2.2](https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.2))
  - `1`: 2 frames in packet, each with equal compressed size ([Section 3.2.3](https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.3))
  - `2`: 2 frames in packet, with different compressed sizes ([Section 3.2.4](https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.4))
  - `3`: Arbitrary number of frames (`1..=48`, total duration `<= 120 ms`), CBR or VBR, with optional padding ([Section 3.2.5](https://datatracker.ietf.org/doc/html/rfc6716#section-3.2.5))

---

## 4. Top-Level Decoder & Dual-Ended Range Coder (`decoder.rs`, `entropy.rs`)

### Top-Level Opus Decoder Block Diagram ([RFC 6716 Section 4](https://datatracker.ietf.org/doc/html/rfc6716#section-4))

```text
                            +---------+    +------------+
                            |  SILK   |    |   Sample   |
                         +->| Decoder |--->|    Rate    |----+
   Bit-    +---------+   |  |         |    | Conversion |    v
   stream  |  Range  |---+  +---------+    +------------+  /---\  Audio
   ------->| Decoder |                                     | + |------>
           |         |---+  +---------+    +------------+  \---/
           +---------+   |  |  CELT   |    | Decimation |    ^
                         +->| Decoder |--->| (Optional) |----+
                            |         |    |            |
                            +---------+    +------------+
```

### Dual-Ended Range Decoder & Raw-Bit Reader ([RFC 6716 Section 4.1](https://datatracker.ietf.org/doc/html/rfc6716#section-4.1))

Every Opus frame is decoded using `entropy::Decoder<'a>` over a single `&'a [u8]` slice:
- **Forward Range Decoder ([Section 4.1.1](https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.1) - [Section 4.1.5](https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.5))**: Reads from byte `0` forward, maintaining 32-bit interval size `rng` and offset `val` normalized to `2^23 < rng <= 2^24`. Decodes symbols from compile-time validated cumulative distribution tables (`decode_symbol` over `Cdf`), log-probability flags (`decode_symbol_logp`), uniform unsigned integers (`decode_uint`), and Laplace-distributed coarse energy deltas (`decode_laplace`).
- **Backward Raw-Bit Reader ([Section 4.1.3.2](https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.3.2))**: Reads raw bits (`decode_raw_bits`) from byte `N - 1` backward toward the front for CELT fine energy and PVQ pulse signs/LSBs.
- **Bit Accounting ([Section 4.1.6](https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.6))**: `tell()` and `tell_frac()` compute the exact total bits (in whole bits and `1/8`th-bit units) consumed by both ends of the frame buffer.

```text
  Frame Byte Slice:
  +--------+--------+--------+-----+--------+--------+
  | byte 0 | byte 1 | byte 2 | ... |byte N-2|byte N-1|
  +--------+--------+--------+-----+--------+--------+
    |                                              |
    +---> Forward Range Coder                      +---> Backward Raw-Bit Reader
          (SILK all symbols; CELT flags,                 (CELT fine energy bits,
           coarse energy, CWRS pulse indices)             PVQ signs, leftover bits)
```

---

## 5. SILK LP Layer Decoder (`silk/*`, [RFC 6716 Section 4.2](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2))

### SILK Decoder Block Diagram ([RFC 6716 Section 4.2.1, Figure 14](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.1))

```text
        +---------+    +------------+
     -->| Range   |--->| Decode     |---------------------------+
      1 | Decoder | 2  | Parameters |----------+       5        |
        +---------+    +------------+     4    |                |
                            3 |                |                |
                             \/               \/               \/
                       +------------+   +------------+   +------------+
                       | Generate   |-->| LTP        |-->| LPC        |
                       | Excitation |   | Synthesis  |   | Synthesis  |
                       +------------+   +------------+   +------------+
                                               ^                |
                                               |                |
                           +-------------------+----------------+
                           |                                      6
                           |   +------------+   +-------------+
                           +-->| Stereo     |-->| Sample Rate |-->
                               | Unmixing   | 7 | Conversion  | 8
                               +------------+   +-------------+
```

1. Range encoded bitstream (`entropy::Decoder<'a>`)
2. Coded SILK parameters (`silk/params.rs`)
3. Shell-coded pulses, LSBs, and signs (`silk/excitation.rs`)
4. Pitch lags and 5-tap Long-Term Prediction (LTP) coefficients in `Q14` (`silk/params.rs`)
5. 10/16-order Linear Predictive Coding (LPC) coefficients in `Q12` and subframe gains in `Q16` (`silk/lsf.rs`)
6. Synthesized mono or Mid-Side stereo signal (`silk/synthesis.rs`)
7. Unmixed mono or Left-Right stereo signal (`silk/synthesis.rs`)
8. Resampled 48 kHz output signal (`silk/resampler.rs`)

### SILK Bitstream Layout ([RFC 6716 Section 4.2.2](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.2) - [Section 4.2.7](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7))

```text
  +----------------------------------+
  |           Header Bits            |  RFC 6716 Section 4.2.3
  | +------------------------------+ |
  | |    VAD flag (1 bit/frame)    | |
  | +------------------------------+ |
  | |        LBRR flag (1 bit)     | |
  | +------------------------------+ |
  +----------------------------------+
  |      Per-Frame LBRR Flags        |  RFC 6716 Section 4.2.4
  |          (optional)              |
  +----------------------------------+
  |        LBRR Frames (1..3)        |  RFC 6716 Section 4.2.5
  |          (optional)              |
  +----------------------------------+
  |      Regular SILK Frames (1..3)  |  RFC 6716 Section 4.2.7
  | +------------------------------+ |
  | |     Stereo Weights (if st)   | |  Section 4.2.7.1
  | |        Frame Type & Gains    | |  Section 4.2.7.2 - 4.2.7.3
  | |   NLSF Stage-1 & Stage-2 VQ  | |  Section 4.2.7.5.1 - 4.2.7.5.2
  | |   LSF Interpolation Weight   | |  Section 4.2.7.5.6
  | |  Pitch Lags & 5-tap LTP VQ   | |  Section 4.2.7.6.1 - 4.2.7.6.2
  | |  LTP Scaling & LCG Seed      | |  Section 4.2.7.6.3 - 4.2.7.7
  | |  Shell-Coded Excitation      | |  Section 4.2.7.8
  | +------------------------------+ |
  +----------------------------------+
```

### SILK Processing Pipeline (`silk/*`)

```text
  [Range Decoder (entropy.rs)]
               |
               +---> 1. Header & LBRR Unpacking (silk/mod.rs, silk/params.rs)
               |        - VAD flags (1..3 frames) & LBRR flags (Section 4.2.3 - 4.2.4)
               |        - Stereo prediction weights w0, w1 in Q13 & mid-only flag (Section 4.2.7.1)
               |
               +---> 2. Frame Type & Subframe Gains (silk/params.rs)
               |        - SignalType (Inactive, Unvoiced, Voiced) & QuantOffsetType (Section 4.2.7.2)
               |        - Logarithmic subframe gains -> Q16 linear gains (Section 4.2.7.3)
               |
               +---> 3. NLSF Decoding & NLSF-to-LPC Conversion (silk/lsf.rs, silk/synthesis.rs)
               |        - Stage-1 codebook VQ + Stage-2 residual VQ in Q15 (Section 4.2.7.5)
               |        - NLSF minimum-spacing stabilization & 4-subframe interpolation
               |        - Chebyshev polynomial evaluation -> Q12 LPC coefficients (Section 4.2.7.5.5)
               |
               +---> 4. Voiced Pitch & LTP Unpacking (silk/params.rs)
               |        - Primary pitch lag (16..288) & subframe pitch contour (Section 4.2.7.6.1)
               |        - Periodicity index & 5-tap Q14 LTP filter codebook (Section 4.2.7.6.2)
               |
               +---> 5. Shell-Coded Excitation Reconstruction (silk/excitation.rs)
                        - Rate level, 16-sample shell pulse tree splits, LSBs, signs (Section 4.2.7.8)
                        - Quantization offset addition & LCG sign/scale inversion -> e_Q10
                                      |
                                      v
  [LTP Pitch Synthesis (silk/synthesis.rs, Section 4.2.7.9.1)]
  (Voiced 5-tap pitch prediction across re-whitened LPC residual history)
                                      |
                                      v
  [LPC Short-Term IIR Synthesis (silk/synthesis.rs, Section 4.2.7.9.2)]
  (10th/16th-order Q12 IIR filter + Q16 subframe gain scaling -> saturating i16)
                                      |
                                      v
  [Mid-Side to Left-Right Stereo Unmixing (silk/synthesis.rs, Section 4.2.8)]
                                      |
                                      v
  [Polyphase IIR/FIR Resampler (silk/resampler.rs, Section 4.2.9) & PLC/CNG (silk/plc.rs)]
```

---

## 6. CELT MDCT Layer Decoder (`celt/*`, [RFC 6716 Section 4.3](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3))

### CELT Decoder Block Diagram ([RFC 6716 Section 4.3, Figure 17](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3))

```text
                                     +--+
                                     |  |
                                     |  +------------+
                                     |               |
                                     v               |
   +---------+    +--------+    +---------+     +----+----+
-->|  Range  |--->| Decode |--->|  Shape  |---->| Stereo  |--+
 1 | Decoder | 2  | Pulses | 3  | Unquant |     |  Unmix  |  |
   +---------+    +--------+    +---------+     +---------+  |
        |                            ^               ^       |
        | 4                          | 5             | 6     | 7
        |      +----------------+    |               |       |
        +----->|Bit Allocation &|----+---------------+       |
        |      |    Parameters   |                            |
        |      +----------------+                            v
        |                                                 /-----\
        | 8    +----------------+    +--------------+     |  x  |---+
        +----->| Decode  Coarse |--->| Decode Fine  |---->|     |   |
               |     Energy     | 9  |    Energy    | 10  \-----/   |
               +----------------+    +--------------+               |
                                                                    |
        +-----------------------------------------------------------+
        | 11
        v
   +---------+    +------------+    +---------------+
   | Inverse |--->| Windowing &|--->| Post-Filter & |-->
   |  MDCT   | 12 | Overlap-Add| 13 |  De-Emphasis  | 14
   +---------+    +------------+    +---------------+
```

1. Range encoded bitstream (`entropy::Decoder<'a>`)
2. Algebraic PVQ CWRS index, sign, and pitch-folding bits (`celt/cwrs.rs`, `celt/vq.rs`)
3. Unit-norm band shape vectors (`||X||_2 = 1`)
4. Silence, transient, intra-energy, pitch post-filter, spread, trim, skip, and stereo flags
5. Per-band pulse bit allocation (`pulses[i]`, `fine_quant[i]`, `fine_priority[i]` in `celt/rate.rs`)
6. Intensity stereo (`intensity`) and dual stereo (`dual_stereo`) parameters
7. Unmixed normalized MDCT coefficients per channel
8. Coarse energy Laplace symbols (`celt/energy.rs`)
9. Quantized coarse band log-energies (`Q10`)
10. Linear band amplitudes (`2^energy`) after fine & leftover energy refinement
11. Denormalized MDCT frequency coefficients
12. Time-domain windowed synthesis blocks (1 long IMDCT or `B = 2^LM` short IMDCTs in `celt/mdct.rs`)
13. Overlap-added signal (`120`-sample low-overlap Vorbis-derived window)
14. Comb-filtered and de-emphasized 48 kHz output PCM (`celt/decoder.rs`)

### CELT Processing Pipeline (`celt/*`)

```text
  [Range Decoder (entropy.rs)]
               |
               +---> 1. Frame Header & Transient/Post-Filter Flags (celt/decoder.rs, Section 4.3.1)
               |        - Silence flag, post-filter pitch period (15..1024), gain, and tapset (0..=2)
               |        - Transient flag (short_blocks = 2^LM vs 1 long block) & intra-energy flag
               |
               +---> 2. Coarse Energy Unquantization (celt/energy.rs, Section 4.3.2.1)
               |        - 2D separable IIR prediction across time (alpha) and band (beta)
               |        - Laplace-coded energy residuals in 6 dB (DB6) Q10 log-energy units
               |
               +---> 3. TF Resolution, Spread, Trim & Dynamic Bit Allocation (celt/rate.rs, Section 4.3.3)
               |        - Per-band time-frequency (TF) resolution selection (Section 4.3.3.1)
               |        - Spread mode (None, Light, Normal, Aggressive) & dynamic caps
               |        - clt_compute_allocation -> per-band PVQ pulse bits, fine_quant, fine_priority
               |
               +---> 4. Fine Energy Unquantization (celt/energy.rs, Section 4.3.2.2)
               |        - Reads fine_quant[i] backward raw bits per band to refine Q10 log-energies
               |
               +---> 5. Algebraic PVQ Shape Decoding & Stereo Unmixing (celt/cwrs.rs, celt/vq.rs, Section 4.3.4)
               |        - decode_pulses (CWRS index in [0, V(N, K))) -> K signed integer pulses
               |        - Haar1 spreading rotations (exp_rotation) & intra/inter spectral folding
               |        - Mid-Side theta unmixing, dual stereo, intensity stereo & anti-collapse noise
               |
               +---> 6. Leftover Energy Finalization & Denormalization (celt/energy.rs, Section 4.3.2.2)
                        - Consumes remaining bit budget on priority fine-energy bits
                        - Scales unit-norm band shapes by 2^energy linear amplitudes
                                      |
                                      v
  [Inverse MDCT & 120-Sample Window Overlap-Add (celt/mdct.rs, Section 4.3.5)]
  (Single long IMDCT of length N = 120 << LM or B = 2^LM short 120-sample IMDCTs)
                                      |
                                      v
  [5-Tap Pitch Comb Post-Filter & De-Emphasis H(z) = 1 / (1 - 0.85 z^-1) (Section 4.3.6)]
                                      |
                                      v
  [CELT Pitch-Correlation PLC History Update (celt/decoder.rs, Section 4.3.7)]
```

---

## 7. Hybrid Mode, Mode Transitions & Multi-Stream (`decoder.rs`)

### Hybrid Mode (`AudioMode::Hybrid`, [RFC 6716 Section 3.1](https://datatracker.ietf.org/doc/html/rfc6716#section-3.1), [Section 4.3](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3))

In Hybrid mode (TOC configurations `12..=15`, Super-Wideband 24 kHz or Fullband 48 kHz, 10 ms or 20 ms frames), both decoders share the **exact same `entropy::Decoder<'a>`** instance over a single frame byte slice:

```text
  Shared Frame Byte Slice & entropy::Decoder<'a>
  +-----------------------------------+-----------------------------------+
  | 1. SILK Layer (0..8 kHz WB)       | 2. CELT Layer (8..20 kHz, B17..21)|
  | - Decodes VAD/LBRR, gains, NLSF,  | - Starts at current range coder   |
  |   LTP, LPC, shell excitation      |   state with start_band = 17      |
  | - Resamples 16 kHz -> Fs          | - Decodes coarse/fine energy,     |
  | - Delayed by Fs / 250 samples     |   PVQ shape, IMDCT for bands 17+  |
  +-----------------------------------+-----------------------------------+
                   |                                     |
                   +------------------+------------------+
                                      |
                                      v
                    out_pcm[i] = silk_pcm[i] + celt_pcm[i]
```

### Seamless Mode Transitions & 5 ms Redundant CELT Frames ([RFC 6716 Section 4.4](https://datatracker.ietf.org/doc/html/rfc6716#section-4.4), [Section 4.5](https://datatracker.ietf.org/doc/html/rfc6716#section-4.5))

- **CELT -> SILK/Hybrid (`Redundancy::CeltToSilk`)**: The 5 ms redundant CELT frame appended to the first SILK or Hybrid frame is decoded before that frame's own CELT layer; its first 2.5 ms replace the start of the frame and its second 2.5 ms are cross-lapped with the arriving SILK output through the squared `CELT_WINDOW` (`crossfade` with `FadeDirection::In`, RFC 6716 Section 4.5.1.4).
- **SILK/Hybrid -> CELT (`Redundancy::SilkToCelt`)**: The redundant 5 ms CELT frame at the end of the last SILK or Hybrid frame is decoded after a CELT state reset and its second half is cross-lapped with the last 2.5 ms of that frame (`FadeDirection::Out`); the following CELT-only frame then needs no concealment.
- **Switches without redundancy (`Transition::IntoCelt`, `Transition::OutOfCelt`)**: 5 ms of audio concealed from the outgoing decoder is cross-lapped with the start of the new frame, the recommended behaviour of RFC 6716 Section 4.5.3, Figure 19.

### Ogg Opus Multi-Stream Surround Decoding ([RFC 7845 Section 5.1.1](https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1))

```text
  Multi-Stream Opus Packet (RFC 6716 Appendix B & RFC 7845 Section 5.1.1):
  +---------------------------+---------------------------+-----------------------+
  | Stream 0 (Self-Delimited) | Stream 1 (Self-Delimited) | Stream N-1 (Standard) |
  +---------------------------+---------------------------+-----------------------+
                |                           |                         |
                v                           v                         v
      StreamDecoder[0]            StreamDecoder[1]          StreamDecoder[N-1]
     (Coupled Stereo: 2ch)       (Coupled Stereo: 2ch)      (Uncoupled Mono: 1ch)
                |                           |                         |
                +---------------------------+-------------------------+
                                            |
                                            v
                    Channel Mapping Table (Family 0 RTP / Family 1 Vorbis)
                    + Q7.8 dB Output Gain Scaling (10^(gain / (20 * 256)))
                                            |
                                            v
                              Output AudioBuffer<f32>
```

---

## 8. Testing

The unit tests need no external files:

```bash
cargo test -p symphonia-codec-opus
```

The conformance test needs the RFC 8251 test vectors
([`opus_newvectors`](https://www.ietf.org/proceedings/98/slides/materials-98-codec-opus-newvectors-00.tar.gz)),
so it is ignored by default. Point `OPUS_TESTVECTORS_DIR` at the extracted directory to run it:

```bash
OPUS_TESTVECTORS_DIR=<path to opus_newvectors> cargo test --release -p symphonia-codec-opus --test rfc6716_conformance -- --ignored --nocapture
```

The source of truth for decoder behaviour is RFC 6716 (prose plus the normative Appendix A
reference decoder) as updated by RFC 8251. The conformance test follows RFC 6716 Section 6: the
final range decoder state must equal the reference for every packet, and the output must pass the
`opus_compare` threshold against either RFC 8251 test vector set.
