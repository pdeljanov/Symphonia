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
## 🛠 **Roadmap**
```mermaid
gantt
    title Silk Decoder Implementation Roadmap (Single Person Workflow)
    dateFormat  YYYY-MM-DD

    section Phase 1: Frame Parsing
    Parse Frame Header               :done, 2024-10-14, 5d
    Parse Frame Control Parameters    :2024-10-19, 5d
    
    section Phase 2: LPC Coefficients
    Extract LPC Coefficients          :2024-10-24, 7d
    Predict and Filter                :2024-11-01, 5d

    section LSF and LPC
    Implement LSF decoding            :2024-11-06, 3d
    Develop LSF to LPC conversion     :2024-11-09, 2d
    
    section Phase 3: Quantization and Coding
    Excitation Signal Quantization    :2024-11-11, 7d
    Decode Excitation Signal          :2024-11-18, 5d
    
    section Excitation Decoding
    Implement PVQ codebook decoding   :2024-11-23, 4d
    Develop pulse location decoding   :2024-11-27, 3d
    Implement sign and LSB decoding   :2024-11-30, 2d

    section LTP and LPC Synthesis
    Implement LTP filter              :2024-12-02, 3d
    Develop LPC synthesis             :2024-12-05, 4d

    section Phase 4: Gain Control
    Gain Quantization                 :2024-12-10, 5d
    Apply Gain                        :2024-12-15, 5d

    section Post-processing
    Implement stereo unmixing         :2024-12-20, 2d
    Develop resampling module         :2024-12-22, 3d
    De-emphasis Filtering             :2024-12-27, 3d
    Packet Reconstruction             :2024-12-30, 4d

    section Finalization
    Implement Finalize method         :2025-01-03, 1d
    Develop Last Decoded Buffer       :2025-01-04, 1d

    section Integration and Testing
    Integrate with symphonia          :2025-01-06, 3d
    Comprehensive testing             :2025-01-09, 4d
    End-to-End Decoder Testing        :2025-01-14, 5d
    Final Optimizations               :2025-01-19, 5d
```

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.


## Contributing

Symphonia is a free and open-source project that welcomes contributions! To get started, please read
our [Contribution Guidelines](https://github.com/pdeljanov/Symphonia/tree/master/CONTRIBUTING.md).
