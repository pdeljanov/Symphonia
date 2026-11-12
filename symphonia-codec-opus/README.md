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

### Accuracy & RFC Conformance

`symphonia-codec-opus` passes all **120 official IETF conformance runs** (`12` [RFC 8251](https://datatracker.ietf.org/doc/html/rfc8251) test vectors across `8, 12, 16, 24, and 48 kHz` in `mono` and `stereo`) and is calibrated sample-for-sample against `opus_demo -d`, the normative C reference decoder from [RFC 6716 Appendix A](https://datatracker.ietf.org/doc/html/rfc6716#appendix-A) with the [RFC 8251](https://datatracker.ietf.org/doc/html/rfc8251) update (see [Section 8](#8-conformance-verification-mechanics--quality) for full per-vector tables):

- **Bitstream & Range Coder (`100%` exact)**: Every checked packet across all 120 runs finishes with the exact 32-bit `final_range` state recorded by the reference encoder (`0` mismatches). Not a single bit or bit-allocation decision diverges.
- **Speech Layer / SILK (`100.00%` bit-exact, `0` LSB)**: Because SILK uses integer fixed-point arithmetic in both C and Rust, every decoded 16-bit PCM sample on SILK-only streams (`testvector02..04`) is bit-for-bit identical to `opus_demo -d`.
- **Music & Hybrid Layers / CELT (`99.41%..100.00%` identical, `<= 1` LSB)**: CELT runs in 32-bit IEEE-754 floating point (`f32`). Over `99.4%` of stereo 16-bit PCM samples (`-32768..32767`) match `opus_demo -d` to `0` LSB, and the remaining `< 0.6%` sit on `.5` half-integer rounding boundaries where ~1 ULP compiler `f32` differences (`cos`/`sin`, instruction scheduling, FMA) shift the quantized integer by at most `1` LSB (`1 / 32768`, or `-90.3 dBFS`, below the 16-bit noise floor).
- **Psychoacoustic Quality (`99.8..100.0 / 100` at 48 kHz stereo)**: On the official `opus_compare` metric ([RFC 6716 Section 6.1](https://datatracker.ietf.org/doc/html/rfc6716#section-6.1), where `0` is the compliance threshold and `> 90` is recommended at 48 kHz), stereo decodes score `99.8` to `100.0` across all 12 vectors.

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
│   ├── layout.rs         - Mono or coupled stereo layout of one elementary stream
│   ├── stream.rs         - StreamDecoder: layer sequencing, loss concealment, transitions and redundancy (RFC 6716 Sections 4.4, 4.5)
│   ├── routing.rs        - Multi-stream packet splitting and output channel routing (RFC 7845 Section 5.1.1)
│   ├── decoder.rs        - OpusDecoder: Symphonia AudioDecoder facade, output format and stream layout from the codec parameters
│   ├── silk/             - SILK linear prediction layer (RFC 6716 Section 4.2)
│   │   ├── mod.rs        - Module map, size constants and re-exports
│   │   ├── frame.rs      - Frame geometry (SilkLayout), header flags, stereo weights, frame and subframe parameters (Sections 4.2.2 to 4.2.7)
│   │   ├── state.rs      - Channel state and history, stereo history and coding references (Sections 4.2.7.9, 4.2.8, 4.5.2)
│   │   ├── constant.rs   - CDF tables, codebooks and filter coefficients
│   │   ├── error.rs      - SILK error type
│   │   ├── params.rs     - Header flags, stereo weights, gains and LTP parameters (Sections 4.2.3 to 4.2.7.6)
│   │   ├── lsf.rs        - NLSF decoding, stabilisation and interpolation (Section 4.2.7.5)
│   │   ├── lpc.rs        - NLSF to LPC conversion, coefficient limits and prediction kernels (Sections 4.2.7.5.6 to 4.2.7.5.8, 4.2.7.9)
│   │   ├── excitation.rs - LCG seed and shell-coded excitation (Sections 4.2.7.7, 4.2.7.8)
│   │   ├── synthesis.rs  - LTP and LPC synthesis, stereo unmixing (Sections 4.2.7.9, 4.2.8)
│   │   ├── resampler.rs  - Resampling to the output rate (Section 4.2.9)
│   │   ├── plc.rs        - Loss concealment (Section 4.4, non-normative)
│   │   ├── cng.rs        - Comfort noise generation for DTX gaps and lost frames (Sections 4.2.7.9, 4.4, non-normative)
│   │   └── decoder.rs    - Channel transitions and the LBRR and regular frame schedule (Sections 4.2.4 to 4.2.6)
│   └── celt/             - CELT transform layer (RFC 6716 Section 4.3)
│       ├── mod.rs        - Block size (LM), band constants, silence frame, module index
│       ├── constant.rs   - Band boundaries, energy models, allocation tables, window
│       ├── error.rs      - CELT error type
│       ├── energy.rs     - Coarse, fine and final energy decoding (Section 4.3.2)
│       ├── rate.rs       - Bit allocation and time-frequency parameters (Sections 4.3.1, 4.3.3)
│       ├── cwrs.rs       - PVQ codeword index decoding (Section 4.3.4.2)
│       ├── vq.rs         - PVQ unquantization, spreading, split decoding, folding and time-frequency changes of one partition (Section 4.3.4)
│       ├── stereo.rs     - Mid/side and two-phase stereo splits and the unmixing to left and right (Section 4.3.4.4, RFC 8251 Section 10)
│       ├── bands.rs      - Band sweep over a frame, folding history and anti-collapse (Sections 4.3.4, 4.3.5)
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
| :--- | :--- | :---: | :--- |
| **TOC Byte** | [RFC 6716 Section 3.1](https://datatracker.ietf.org/doc/html/rfc6716#section-3.1) | 🟢 | All 32 configurations, modes (SILK, Hybrid, CELT), bandwidths (NB..FB), frame durations (2.5..60 ms) |
| **Packet Framing** | [RFC 6716 Section 3.2](https://datatracker.ietf.org/doc/html/rfc6716#section-3.2), [Appendix B](https://datatracker.ietf.org/doc/html/rfc6716#appendix-B) | 🟢 | Code 0, 1, 2, 3 (CBR/VBR), padding, 120 ms validation, self-delimited multi-stream packets |
| **Ogg Headers** | [RFC 7845 Section 5](https://datatracker.ietf.org/doc/html/rfc7845#section-5) | 🟢 | `OpusHead` identification header with the channel mapping table for every family other than 0 and per-family channel bounds (RFC 7845 Section 5.1.1); `OpusTags` is left to the Ogg demuxer |
| **Entropy Decoder** | [RFC 6716 Section 4.1](https://datatracker.ietf.org/doc/html/rfc6716#section-4.1) | 🟢 | 32-bit range decoder over compile-time validated CDF tables (`decode_symbol`, `decode_symbol_logp`, `decode_uint`, `decode_laplace`) and backward raw-bit reader |
| **SILK Layer** | [RFC 6716 Section 4.2](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2) | 🟢 | NB/MB/WB mono & stereo, NLSF/LTP/LPC synthesis, 48 kHz resampler, LBRR, DTX/CNG, PLC |
| **CELT Layer** | [RFC 6716 Section 4.3](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3) | 🟢 | Transient/short blocks, coarse/fine/leftover energy, dynamic bit allocation, PVQ/CWRS, IMDCT, post-filter, PLC |
| **Hybrid Mode** | [RFC 6716 Section 3.1](https://datatracker.ietf.org/doc/html/rfc6716#section-3.1), [Section 4.3](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3) | 🟢 | Simultaneous SILK (0..8 kHz) + CELT (8..20 kHz, bands 17..21) synthesis and delay compensation |
| **Mode Transitions** | [RFC 6716 Section 4.5](https://datatracker.ietf.org/doc/html/rfc6716#section-4.5) | 🟢 | 5 ms redundant CELT frame transitions and window crossfades across SILK/Hybrid/CELT boundaries |
| **Multi-Stream** | [RFC 7845 Section 5.1.1](https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1) | 🟢 | Coupled (2-channel) and uncoupled (1-channel) stream routing, Vorbis Family 1 (1..8 channels), Q8 output gain |
| **RFC Conformance** | [RFC 6716 Section 6.1](https://datatracker.ietf.org/doc/html/rfc6716#section-6.1), [RFC 8251](https://datatracker.ietf.org/doc/html/rfc8251) | 🟢 | All 120 runs (12 vectors x 5 sample rates x mono/stereo) pass with exact `final_range` on every packet; bit-exact (`0` LSB) on SILK-only runs and within `1` LSB of the patched RFC 6716 Appendix A C reference decoder on stereo and non-inverted mono runs (see Section 8) |

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

```text
        +--------+--------+--------+-----+--------+--------+
        | byte 0 | byte 1 | byte 2 | ... |byte N-2|byte N-1|
        +--------+--------+--------+-----+--------+--------+
            |                                          |
          1 |  Forward --->              <--- Backward | 2
            \/                                         \/
      +------------+                             +------------+
      |   Range    |                             |  Raw-Bit   |
      |  Decoder   |                             |   Reader   |
      +------------+                             +------------+
            |                                          |
          3 +--------------------+---------------------+ 4
                                 |
                                 \/
                           +------------+
                           |  tell() &  |
                           |tell_frac() |
                           +------------+
```

1. **Forward Byte Stream (`byte 0` upward, [Section 4.1.1](https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.1) - [Section 4.1.5](https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.5))**: Maintains 32-bit interval size `rng` and offset `val` normalized to `2^23 < rng <= 2^24`.
2. **Backward Byte Stream (`byte N - 1` downward, [Section 4.1.3.2](https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.3.2))**: Reads raw bits from the end of the frame buffer toward the front.
3. **Range-Coded Symbols**: Decodes symbols from compile-time validated cumulative distribution tables (`decode_symbol` over `Cdf`), log-probability flags (`decode_symbol_logp`), uniform unsigned integers (`decode_uint`), and Laplace-distributed coarse energy deltas (`decode_laplace`).
4. **Raw Bits & Bit Accounting (`decode_raw_bits`, [Section 4.1.6](https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.6))**: Supplies CELT fine energy bits, PVQ pulse signs, and leftover bits; `tell()` and `tell_frac()` report the exact total bits (in whole bits and `1/8`th-bit units) consumed by both ends.

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
5. 10/16-order Linear Predictive Coding (LPC) coefficients in `Q12` and subframe gains in `Q16` (`silk/lsf.rs`, `silk/lpc.rs`)
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

### SILK Submodule Pipeline (`silk/*`)

```text
        +---------+    +------------+    +------------+    +------------+
     -->|  Range  |--->|  Header &  |--->| Frame Type |--->|   Decode   |
      1 | Decoder | 2  | LBRR Flags | 3  |  & Gains   | 4  |    NLSF    |
        +---------+    +------------+    +------------+    +------------+
             |                                   |                |
             +------------------+----------------+              5 |
             |                  |                |                \/
           6 |                7 |                |         +------------+
             \/                 \/               |         |  Stabilize |
        +---------+    +------------+            |         | & NLSF-LPC |
        |  Pitch  |    |   Shell    |            |         +------------+
        |  & LTP  |    | Excitation |            |                |
        +---------+    +------------+            |                |
             |               |                   |                |
             | 8           9 |                   +-------+        | 10
             |               \/                          |        |
             |         +------------+   +------------+   |        |
             +-------->|    LTP     |-->|    LPC     |<--+--------+
                       | Synthesis  |   | Synthesis  |
                       +------------+   +------------+
                                              |
                                              | 11
                                              \/
                       +------------+   +------------+
                    <--| Polyphase  |<--|   Stereo   |
                    13 | Resampler  |12 |  Unmixing  |
                       +------------+   +------------+
```

1. Shared frame range decoder (`entropy::Decoder<'a>`)
2. VAD flags (`1..3` frames), per-frame LBRR flags, and stereo weights `w0, w1` in `Q13` (`silk/decoder.rs`, `silk/params.rs`, [Section 4.2.3](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.3) - [Section 4.2.7.1](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.1))
3. `SignalType` (`Inactive`, `Unvoiced`, `Voiced`) and `QuantOffsetType` (`silk/params.rs`, [Section 4.2.7.2](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.2))
4. Logarithmic subframe gains unpacked to linear `Q16` gains (`silk/params.rs`, [Section 4.2.7.3](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.3))
5. Two-stage VQ Normalized Line Spectral Frequencies (`nlsf_q15` in `Q15`, `silk/lsf.rs`, [Section 4.2.7.5.1](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.1) - [Section 4.2.7.5.2](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.2))
6. Voiced primary pitch lag (`16..288`), subframe pitch contour, and 5-tap `Q14` LTP filter codebook (`silk/params.rs`, [Section 4.2.7.6](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6))
7. Rate level, 16-sample shell pulse tree splits, LSBs, signs, and LCG seed (`silk/excitation.rs`, [Section 4.2.7.7](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.7) - [Section 4.2.7.8](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8))
8. Subframe pitch lags, 5-tap `Q14` LTP coefficients, and LTP scale factor (`Q14`)
9. Quantization-offset-adjusted and LCG sign-inverted excitation `e_Q10` (`silk/excitation.rs`, [Section 4.2.7.8.6](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8.6))
10. Stabilized, interpolated NLSFs converted via Chebyshev polynomial evaluation and bandwidth expansion to 10th/16th-order `Q12` LPC coefficients (`silk/lsf.rs`, `silk/lpc.rs`, [Section 4.2.7.5.3](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.3) - [Section 4.2.7.5.8](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.5.8), [RFC 8251 Section 8](https://datatracker.ietf.org/doc/html/rfc8251#section-8))
11. Short-term LPC IIR synthesis and `Q16` subframe gain scaling to `i16` (`silk/synthesis.rs`, `silk/lpc.rs`, [Section 4.2.7.9](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.9); or non-normative PLC/CNG in `silk/plc.rs`, [Section 4.4](https://datatracker.ietf.org/doc/html/rfc6716#section-4.4))
12. Mid-Side to Left-Right unmixed stereo or mono signal (`silk/synthesis.rs`, [Section 4.2.8](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.8))
13. Polyphase IIR/FIR resampled output at `8, 12, 16, 24, or 48 kHz` (`silk/resampler.rs`, [Section 4.2.9](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9))

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

### CELT Submodule Pipeline (`celt/*`)

```text
        +---------+    +------------+    +------------+    +------------+
     -->|  Range  |--->|  Header &  |--->|   Coarse   |--->|TF, Spread &|
      1 | Decoder | 2  | PostFilter | 3  |   Energy   | 4  | Allocation |
        +---------+    +------------+    +------------+    +------------+
             |               |                 |                 |
             +---------------+-----------------+-----------------+
             |               |                 |
           5 |             6 |               7 |
             \/              \/                \/
        +---------+    +------------+    +------------+
        |  Fine   |--->| PVQ Shape  |--->|  Leftover  |
        | Energy  | 8  |   & CWRS   | 9  |   Energy   |
        +---------+    +------------+    +------------+
                             |                 |
                             +--------+--------+
                                      | 10
                                      \/
        +---------+    +------------+    +------------+
     <--|PostFilt |<---| Inverse    |<---|Denormalize |
     13 |& De-Emph| 12 | MDCT & WOLA| 11 |   Bands    |
        +---------+    +------------+    +------------+
```

1. Dual-ended range decoder and backward raw-bit reader (`entropy::Decoder<'a>`)
2. Silence flag, pitch post-filter period (`15..1024`), gain, tapset (`0..=2`), transient flag (`short_blocks = 2^LM`), and intra-energy flag (`celt/decoder.rs`, `celt/postfilter.rs`, [Section 4.3.1](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.1))
3. 2D separable IIR coarse energy prediction (`alpha`, `beta`) and Laplace-coded `6 dB` (`Q10`) energy residuals (`celt/energy.rs`, [Section 4.3.2.1](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.1))
4. Per-band time-frequency resolution, spreading mode, allocation trim, and dynamic pulse/fine-energy bit allocation (`celt/rate.rs`, [Section 4.3.3](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3))
5. Backward raw bits (`fine_quant[i]` per band) refining `Q10` band log-energies (`celt/energy.rs`, [Section 4.3.2.2](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.2))
6. Algebraic PVQ CWRS codeword indices (`[0, V(N, K))`), pulse signs, Haar1 spreading rotations, spectral folding, Mid-Side/dual/intensity stereo unmixing, and anti-collapse noise (`celt/cwrs.rs`, `celt/vq.rs`, [Section 4.3.4](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4), [RFC 8251 Section 10](https://datatracker.ietf.org/doc/html/rfc8251#section-10))
7. Remaining bit budget consumed on priority fine-energy bits (`fine_priority[i]`, `celt/energy.rs`, [Section 4.3.2.3](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.3))
8. Refined `Q10` band log-energies passed to shape folding and leftover refinement
9. Unit-norm (`||X||_2 = 1`) unmixed band shape vectors (`celt/vq.rs`)
10. Combined coarse, fine, and leftover linear band amplitudes (`2^energy`) paired with unit-norm shapes
11. Denormalized MDCT frequency coefficients scaled by band amplitudes (`celt/synthesis.rs`, [Section 4.3.6](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.6))
12. Mixed-radix FFT inverse MDCT (`1` long block of `N = 120 * 2^LM` or `2^LM` short `120`-sample blocks) and `120`-sample low-overlap window overlap-add (`celt/fft.rs`, `celt/mdct.rs`, [Section 4.3.7](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7))
13. 5-tap pitch comb post-filter, de-emphasis `H(z) = 1 / (1 - 0.85 * z^-1)`, optional decimation, and LPC/pitch PLC history update (`celt/postfilter.rs`, `celt/synthesis.rs`, `celt/plc.rs`, [Section 4.3.7.1](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.1) - [Section 4.4](https://datatracker.ietf.org/doc/html/rfc6716#section-4.4), [RFC 8251 Section 6](https://datatracker.ietf.org/doc/html/rfc8251#section-6))

---

## 7. Hybrid Mode, Mode Transitions & Multi-Stream (`stream.rs`, `routing.rs`)

### Hybrid Mode (`AudioMode::Hybrid`, [RFC 6716 Section 3.1](https://datatracker.ietf.org/doc/html/rfc6716#section-3.1), [Section 4.3](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3))

In Hybrid mode (TOC configurations `12..=15`, Super-Wideband 24 kHz or Fullband 48 kHz, 10 ms or 20 ms frames), both decoders share the **exact same `entropy::Decoder<'a>`** instance over a single frame byte slice:

```text
                             +------------+    +------------+
                          1  |    SILK    |    |Resample 16k| 2
                          +->|  Decoder   |--->|  & Delay   |----+
        +------------+    |  | (0..8 kHz) |    | (Fs / 250) |    |
     -->|   Shared   |----+  +------------+    +------------+    \/
        |   Range    |                                          /---\
        |  Decoder   |----+  +------------+    +------------+   | + |-->
        +------------+    |  |    CELT    |    | Decimation |   \---/ 5
                          +->|  Decoder   |--->| (Optional) |    ^
                          3  |(Bands 17+) |    |            |----+
                             +------------+    +------------+ 4
```

1. Forward range-coded SILK wideband (`0..8 kHz`) parameters and shell excitation (`silk::Decoder::decode_silk_frame`)
2. SILK output resampled from `16 kHz` to `Fs` and delayed by `Fs / 250` samples to align with CELT windowing ([RFC 6716 Section 4.2.9](https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9))
3. Same `entropy::Decoder<'a>` continuing at the exact range state after SILK, plus backward raw bits from the end of the frame, with `start_band = 17` (`8..12 kHz` SWB or `8..20 kHz` FB, `celt::Decoder::decode_celt_frame`)
4. High-band CELT synthesis (`48 kHz` decimated to `Fs` when `Fs < 48 kHz`)
5. Summed full-bandwidth hybrid output (`out[i] = silk[i] + celt[i]`)

### Seamless Mode Transitions & 5 ms Redundant CELT Frames ([RFC 6716 Section 4.4](https://datatracker.ietf.org/doc/html/rfc6716#section-4.4), [Section 4.5](https://datatracker.ietf.org/doc/html/rfc6716#section-4.5))

- **CELT -> SILK/Hybrid (`Redundancy::CeltToSilk`)**: The 5 ms redundant CELT frame appended to the first SILK or Hybrid frame is decoded before that frame's own CELT layer; its first 2.5 ms replace the start of the frame and its second 2.5 ms are cross-lapped with the arriving SILK output through the squared `CELT_WINDOW` (`crossfade` with `FadeDirection::In`, RFC 6716 Section 4.5.1.4).
- **SILK/Hybrid -> CELT (`Redundancy::SilkToCelt`)**: The redundant 5 ms CELT frame at the end of the last SILK or Hybrid frame is decoded after a CELT state reset and its second half is cross-lapped with the last 2.5 ms of that frame (`FadeDirection::Out`); the following CELT-only frame then needs no concealment.
- **Switches without redundancy (`Transition::IntoCelt`, `Transition::OutOfCelt`)**: 5 ms of audio concealed from the outgoing decoder is cross-lapped with the start of the new frame, the recommended behaviour of RFC 6716 Section 4.5.3, Figure 19.

### Ogg Opus Multi-Stream Surround Decoding ([RFC 7845 Section 5.1.1](https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1))

```text
        +--------------+--------------+--------------+
        |   Stream 0   |   Stream 1   |  Stream N-1  |
        |(Self-Delim.) |(Self-Delim.) |  (Standard)  |
        +--------------+--------------+--------------+
               |              |              |
             1 |            2 |            3 |
               \/             \/             \/
        +--------------+--------------+--------------+
        |StreamDecoder |StreamDecoder |StreamDecoder |
        | (Coupled 2ch)| (Coupled 2ch)|(Uncoupled 1c)|
        +--------------+--------------+--------------+
               |              |              |
               +--------------+--------------+
                              | 4
                              \/
                       +--------------+
                       |Channel Mapper|
                       | & Q7.8 Gain  |
                       +--------------+
                              | 5
                              \/
                       +--------------+
                       | AudioBuffer  |
                       |    <f32>     |
                       +--------------+
```

1. Self-delimited packet for coupled stream `0` (`packet::parse_packet(..., true)`, [RFC 6716 Appendix B](https://datatracker.ietf.org/doc/html/rfc6716#appendix-B))
2. Self-delimited packet for coupled stream `1..M-1` (`2` channels each)
3. Standard undelimited packet for final stream `N-1` (`packet::parse_packet(..., false)`, `1` channel when `N > M`)
4. Decoded planar channels (`2 * M + (N - M)` internal stream channels)
5. Output channels routed via `channel_mapping[0..C]` (`0..254` -> decoded channel, `255` -> silence) and scaled by `10^(output_gain / (20 * 256))` ([RFC 7845 Section 5.1.1](https://datatracker.ietf.org/doc/html/rfc7845#section-5.1.1))

---

## 8. Conformance, Verification Mechanics & Quality

### 8.1 Conformance Verification Pipeline ([RFC 6716 Section 6](https://datatracker.ietf.org/doc/html/rfc6716#section-6), [RFC 8251 Section 11](https://datatracker.ietf.org/doc/html/rfc8251#section-11))

The normative source of truth for Opus is [RFC 6716](https://datatracker.ietf.org/doc/html/rfc6716) (both the prose specification and the Base64-encoded C reference implementation in [Appendix A](https://datatracker.ietf.org/doc/html/rfc6716#appendix-A)) as updated by [RFC 8251](https://datatracker.ietf.org/doc/html/rfc8251).

For each of the **120 test runs** (`12 vectors x 5 sample rates [8, 12, 16, 24, 48 kHz] x 2 layouts [mono, stereo]`), `tests/rfc6716_conformance.rs` executes three independent verification checks on the decoded stream:

```text
        +--------------+--------------+--------------+
        |Payload Length|enc_final_rng | Opus Packet  |
        |   (4B BE)    |   (4B BE)    |   Payload    |
        +--------------+--------------+--------------+
                              |              |
                              |            1 |
                              |              \/
                              |       +--------------+
                              |       | OpusDecoder  |
                              |       | decode_ref() |
                              |       +--------------+
                              |          |        |
                            2 |        3 |      4 |
                              \/         \/       \/
                       +-------------------+ +--------------+
                       |   final_range()   | |  round_ties  |
                       | == enc_final_rng  | |  _even (i16) |
                       +-------------------+ +--------------+
                                                |        |
                                              5 |      6 |
                                                \/       \/
                                       +--------------+ +--------------+
                                       | opus_compare | |  opus_demo   |
                                       | (.dec/m.dec) | | Sample Diff  |
                                       +--------------+ +--------------+
```

1. **Packet Framing (`testvector01..12.bit`, [RFC 8251 Section 11](https://datatracker.ietf.org/doc/html/rfc8251#section-11))**: Each `.bit` record provides a 4-byte big-endian payload length, a 4-byte big-endian reference range coder window (`enc_final_rng`), and the raw Opus packet bytes passed to `OpusDecoder::decode_ref`.
2. **Expected Range Window (`enc_final_rng`)**: The 32-bit range coder window size (`rng`) recorded by the reference encoder after encoding that packet.
3. **Per-Packet Range Coder Check (`final_range`)**: After decoding each packet, `decoder.final_range()` must equal `enc_final_rng` bit-for-bit on `100%` of checked packets. Because the range decoder state depends on every symbol, raw bit, and bit-allocation decision in both SILK and CELT, even a 1-bit parsing bug or wrong branch immediately desynchronizes `final_range`.
4. **16-Bit PCM Quantization (`round_ties_even`)**: The decoded `f32` audio is scaled by `32768.0`, clamped to `[-32768, 32767]`, and rounded to nearest-even (`round_ties_even`, matching C `lrintf` in `opus_demo`).
5. **Psychoacoustic Spectral Comparison (`opus_compare` against `.dec` and `m.dec`)**:
   - `opus_compare` ([RFC 6716 Section 6.1](https://datatracker.ietf.org/doc/html/rfc6716#section-6.1)) windows both the 48 kHz reference (`.dec` or `m.dec`) and the decoded signal (using a `480 / downsample`-sample Hann window and `120 / downsample` hop so 100 Hz DFT bins align across sample rates), applies frequency, temporal, and stereo cross-talk masking across 21 Bark-derived bands (skipping transition bins near Nyquist at reduced rates and attenuating the 8 kHz SILK/CELT crossover bin), and maps the 16th-root weighted spectral error `err` to quality `Q = 100 * (1 - 0.5 * ln(1 + err) / ln(1.13))`.
   - `testvectorNN.dec` carries the CELT 180-degree intensity-stereo phase shift (matched by stereo output), while `testvectorNNm.dec` omits the 180-degree phase shift ([RFC 8251 Section 10](https://datatracker.ietf.org/doc/html/rfc8251#section-10), matched by mono output so phase-inverted bands do not cancel during downmixing).
6. **Sample-by-Sample Calibration Against `opus_demo -d`**:
   Every decoded 16-bit PCM sample is compared directly against the output of the patched RFC 6716 Appendix A C reference binary (`opus_demo -d <rate> <channels>`). Whereas `.dec` and `m.dec` provide only 48 kHz stereo spectra for `opus_compare`, `opus_demo -d` provides sample-level reference PCM at every rate and channel count, verifying that the integer fixed-point SILK pipeline is `100.00%` bit-exact (`0` LSB difference) and that in the floating-point CELT/Hybrid pipeline over `99.4%` of stereo samples match to `0` LSB while the remaining `< 0.6%` on `.5` half-integer rounding boundaries differ by at most `1` LSB (`-90.3 dBFS`).

### 8.2 Calibration Figures Across All 12 RFC Test Vectors & 5 Sample Rates

`symphonia-codec-opus` achieves the following results (`100%` safe Rust, `#![forbid(unsafe_code)]`, zero per-packet heap allocations):

| Vector | Stream Content | Range Checks | Mono `Q` (`m.dec`) | Stereo `Q` (`.dec`) | Ref `opus_demo` Stereo `Q` | Stereo Sample Match vs `opus_demo` | Max Stereo Diff (16-bit LSB) |
| :--- | :--- | :---: | ---: | ---: | ---: | ---: | ---: |
| `01` | CELT FB, 100% stereo | 2,146 / 2,146 | 98.2 | 99.9 | 99.8 | 99.81% | 1 LSB |
| `02` | SILK NB, 49% stereo | 1,184 / 1,184 | 96.2 | 100.0 | 100.0 | 100.00% | 0 LSB |
| `03` | SILK MB, 48% stereo | 997 / 997 | 96.7 | 100.0 | 100.0 | 100.00% | 0 LSB |
| `04` | SILK WB, 49% stereo | 1,264 / 1,264 | 96.5 | 100.0 | 100.0 | 100.00% | 0 LSB |
| `05` | Hybrid SWB, 49% stereo | 2,036 / 2,036 | 96.3 | 99.9 | 99.8 | 100.00% | 1 LSB |
| `06` | Hybrid FB, 49% stereo | 1,875 / 1,875 | 96.3 | 99.8 | 99.8 | 100.00% | 1 LSB |
| `07` | CELT NB..FB, 31 BW switches | 4,185 / 4,185 | 98.1 | 99.8 | 99.6 | 99.82% | 1 LSB |
| `08` | CELT NB..FB, 29 BW switches | 1,246 / 1,246 | 96.5 | 99.8 | 99.6 | 99.83% | 1 LSB |
| `09` | CELT NB..FB, 7 BW switches | 1,336 / 1,336 | 98.1 | 99.8 | 99.5 | 99.81% | 1 LSB |
| `10` | CELT FB + 16% Hybrid, 16 switches | 1,911 / 1,911 | 97.7 | 99.8 | 99.2 | 99.41% | 1 LSB |
| `11` | CELT FB, 100% stereo | 552 / 552 | 98.5 | 99.8 | 99.7 | 99.92% | 1 LSB |
| `12` | SILK NB..WB + 19% Hybrid SWB, mono | 1,331 / 1,331 | 99.9 | 99.9 | 99.8 | 100.00% | 1 LSB |

Summary across all 5 output sample rates (120 runs total):

| Output Rate | Range Checks Passed | `symphonia-codec-opus` Quality Range | Reference `opus_demo` Quality Range | SILK-Only Sample Diff vs `opus_demo` | Stereo Sample Diff vs `opus_demo` |
| :--- | :---: | ---: | ---: | :---: | :---: |
| **8 kHz** | 24 / 24 runs | `43.4` .. `93.3` | `43.4` .. `93.3` | `0` LSB (100.00% exact) | `<= 1` LSB (`99.43%`..`100.00%` exact) |
| **12 kHz** | 24 / 24 runs | `65.9` .. `96.0` | `65.9` .. `96.0` | `0` LSB (100.00% exact) | `<= 1` LSB (`99.43%`..`100.00%` exact) |
| **16 kHz** | 24 / 24 runs | `56.5` .. `96.4` | `56.5` .. `96.4` | `0` LSB (100.00% exact) | `<= 1` LSB (`99.43%`..`100.00%` exact) |
| **24 kHz** | 24 / 24 runs | `68.5` .. `98.1` | `68.5` .. `98.1` | `0` LSB (100.00% exact) | `<= 1` LSB (`99.43%`..`100.00%` exact) |
| **48 kHz** | 24 / 24 runs | `96.2` .. `100.0` | `96.2` .. `100.0` | `0` LSB (100.00% exact) | `<= 1` LSB (`99.41%`..`100.00%` exact) |

- **Why Mono SILK Scores `96.2`..`96.7` and Reduced Rates Score `43.4`..`98.1`**: Because `.dec` and `m.dec` are 48 kHz stereo files, `opus_compare` compares a mono decode against `(L + R) / 2` of the stereo reference (whereas a mono SILK decoder decodes only the mid channel) and compares reduced-rate decodes across internal resampler delay/phase differences ([RFC 6716 Section 6.1](https://datatracker.ietf.org/doc/html/rfc6716#section-6.1)). The C reference decoder `opus_demo -d` itself gets the exact same `96.2`..`96.7` on SILK mono and `43.4` on `testvector12` at 8 kHz, and its output is `100.00%` bit-exact (`0` LSB difference) with `symphonia-codec-opus` on those runs.

### 8.3 Running the Tests

The 143 self-contained unit tests require no external files:

```bash
cargo test -p symphonia-codec-opus
```

The packet fixtures and expected intermediate values of those unit tests were recorded with libopus 1.6.1 (floating-point build). They pin the behaviour of each stage; the RFC conformance of the decoder is established only by the test-vector suite below, which compares against the RFC 6716 Appendix A reference decoder with the RFC 8251 update.

The RFC 6716 / RFC 8251 conformance and reference comparison suite in `tests/rfc6716_conformance.rs` reads [`opus_newvectors`](https://www.ietf.org/proceedings/98/slides/materials-98-codec-opus-newvectors-00.tar.gz) from `OPUS_TESTVECTORS_DIR` (and optional `opus_demo -d` outputs from `OPUS_REFERENCE_OUTPUT_DIR`):

```bash
OPUS_TESTVECTORS_DIR=<path to opus_newvectors> \
OPUS_REFERENCE_OUTPUT_DIR=<optional path to ref_*.pcm> \
cargo test --release -p symphonia-codec-opus --test rfc6716_conformance -- --ignored --nocapture
```

---

## 9. License & IETF Document Attribution

`symphonia-codec-opus` is distributed under the Mozilla Public License 2.0 like the rest of Symphonia (see [`LICENSE`](LICENSE)).

This README and the crate's rustdoc reproduce figures, tables and short passages verbatim from the following IETF Documents, each marked with a link to its RFC section:

- [RFC 6716](https://datatracker.ietf.org/doc/html/rfc6716), "Definition of the Opus Audio Codec", Copyright (c) 2012 IETF Trust and the persons identified as the document authors.
- [RFC 7845](https://datatracker.ietf.org/doc/html/rfc7845), "Ogg Encapsulation for the Opus Audio Codec", Copyright (c) 2016 IETF Trust and the persons identified as the document authors.
- [RFC 8251](https://datatracker.ietf.org/doc/html/rfc8251), "Updates to the Opus Audio Codec", Copyright (c) 2017 IETF Trust and the persons identified as the document authors.

Those excerpts are reproduced under the [IETF Trust Legal Provisions Relating to IETF Documents (TLP 5)](https://trustee.ietf.org/documents/trust-legal-provisions/tlp-5/), Section 3.c, "Licenses for Use Outside the IETF Standards Process", which permit extracting and reproducing portions of IETF Documents provided that the IETF Trust and the document authors are attributed. The excerpts remain subject to the IETF Trust's terms and are not relicensed under the MPL 2.0; the surrounding text and all Rust source code in this crate are original work.
