# Symphonia Opus Codec

This is a SILK-focused Opus decoder as an addition to the [Symphonia project](https://github.com/pdeljanov/Symphonia) 
offering a pure-Rust implementation of the Opus audio codec, designed for decoding audio stream.
The Opus codec handles a wide range of audio applications, and this module focuses on implementing the Opus decoder, with plans to support all modes (SILK, CELT, and Hybrid) as described in the [Opus specification](https://datatracker.ietf.org/doc/html/rfc6716).

**Note:** This crate is part of Symphonia. Please use the [`symphonia`](https://crates.io/crates/symphonia) crate
instead of this one directly.
---


## 🏗 **Project Structure**
```sourcegraph
/symphonia/symphonia-codec-opus
├── Cargo.toml
├── README.md
├── src
│   ├── decoder.rs      # Main Opus decoder implementation (SILK Mode)
│   ├── entropy.rs      # Entropy decoding using range coding
│   ├── header.rs       # Handles Opus headers
│   ├── lib.rs          # Library entry point
│   ├── packet.rs       # Packet processing
│   ├── toc.rs          # Handles TOC byte processing
│   └── silk            # SILK decoder components
│       ├── decoder.rs  # SILK decoder (prototype)
│       ├── error.rs    # SILK-specific error handling
|       └── constant.rs # codebooks, tables, pdfs, etc.
└── tests               # Unit tests for each component
```
---
## 📜 **Features Overview**

| Feature                 | Status         | Notes                                                             |
|-------------------------|----------------|-------------------------------------------------------------------|
| **SILK Decoder**        | 🟡 Prototype   | Draft that needs refactoring and testing. Handles speech streams. |
| **CELT Decoder**        | 🔴 Missing     | Placeholder. Needed for high-quality music decoding.              |
| **Hybrid Decoder**      | 🔴 Missing     | Placeholder. Needed for mixed music and speech decoding.          |
| **Range Coding**        | 🟢 Implemented | Based on RFC 6716, tested and working.                            |
| **Packet Handling**     | 🟢 Implemented | Basic packet parsing and processing is working fine.              |
| **Finalize Method**     | 🔴 Missing     | Required to free resources after decoding completes.              |
| **Last Decoded Buffer** | 🔴 Missing     | Needs implementation to return the last decoded audio buffer.     |

---

## ⚠️ **Immediate Action Items**

| Task                                      | Status         | Priority | Notes                                                                                                                                        |
|-------------------------------------------|----------------|----------|----------------------------------------------------------------------------------------------------------------------------------------------|
| **Refactor SILK Decoder**                 | 🟡 In Progress | 🔥 High  | Improve code quality and add more tests. Ensure compatibility with more Opus streams.                                                        |
| **Implement CELT Decoder**                | 🔴 Not Started | 🔥 High  | CELT mode is crucial for decoding music streams. Refer to [RFC 6716 Section 4.3](https://datatracker.ietf.org/doc/html/rfc6716#section-4.3). |
| **Implement Hybrid Mode**                 | 🔴 Not Started | 🔥 High  | Hybrid mode combines SILK and CELT decoding. Placeholder needs full implementation.                                                          |
| **Add Finalize and Last Decoded Buffers** | 🔴 Not Started | ⚡ Medium | Complete the `finalize()` and `last_decoded()` methods for a full decoder API.                                                               |
| **Optimize Range Decoder**                | 🟢 Done        | ⚡ Medium | Range decoder is implemented but can be further optimized for performance.                                                                   |
| **Expand Tests & Benchmarks**             | 🟡 Ongoing     | ⚡ Medium | Increase test coverage for edge cases, especially malformed packets. Add benchmarks.                                                         |


## 🛠 **Roadmap**

| Phase                         | Estimated Duration | Deliverables                                                                                          |
|-------------------------------|--------------------|-------------------------------------------------------------------------------------------------------|
| **Phase 0**: Initial          | **3 weeks**        | Finalizing implementation of Silk decoder. Add tests.                                                 |
| **Phase 1**: CELT and Hybrid  | **6 weeks**        | Complete CELT and Hybrid decoding modes. Add corresponding tests.                                     |
| **Phase 2**: Refactor & Tests | **5 weeks**        | Refactor the SILK decoder, improve test coverage                                                      |
| **Phase 3**: Optimization     | **2 weeks**        | Profile and optimize range decoder performance. Ensure handling of edge cases and improve efficiency. |
| **Phase 4**: Documentation    | **Ongoing**        | Keep updating documentation, encourage community involvement, and respond to user feedback.           |


## Codec integration

Symphonia uses a modular approach where demuxers handle container formats (e.g., OGG) and pass compressed audio streams
to decoders. The role of symphonia-codec-opus crate will be strictly limited to decoding Opus-encoded audio
streams. Container-level operations such as OGG demuxing is already handled by the existing demuxers like symphonia-format-ogg.

## Packet parsing and decoding

Opus packet structure and frame sizes are well-documented in [RFC 6716](https://datatracker.ietf.org/doc/html/rfc6716)
and [RFC 7587](https://datatracker.ietf.org/doc/html/rfc7845).
The decoding process involves:

* Extracting frames from the Opus packet (with variable-length frame packing).
* Handling different frame durations (2.5, 5, 10, 20 ms) as described in the RFC.
* Decoding frames based on the SILK (for low bitrates) or CELT (for high bitrates) hybrid mode that uses both.
* Using Opus’s range decoder to interpret symbols packed into each frame, particularly for audio bandwidth and
  prediction settings.

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is a free and open-source project that welcomes contributions! To get started, please read
our [Contribution Guidelines](https://github.com/pdeljanov/Symphonia/tree/master/CONTRIBUTING.md).
