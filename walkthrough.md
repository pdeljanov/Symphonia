# Symphonia Opus Decoder (`symphonia-codec-opus`) Walkthrough & Handoff

## 1. Branch & Repository State

- **Repository**: `/home/antigravity/jiztastamablastamarang/symphonia`
- **Active Branch**: `opus-decoder` (tracking `origin/opus-decoder`)
- **Crate**: `symphonia-codec-opus`
- **Safety & Style Enforcement**: `#![forbid(unsafe_code)]`, zero `else if` cascades, consistent index naming (`i`, `j`, `k`), explicit `return expr;` statements, zero per-packet heap allocations in steady-state decoding, and RFC 6716 / RFC 7845 / RFC 8251 ASCII diagrams with clickable RFC section links.

---

## 2. Completed Work & Commit History on `opus-decoder`

1. **SILK Layer Idiomatic Functional Refactoring (`06f4b3b`)**
   - Refactored `src/silk/plc.rs`, `src/silk/decoder.rs`, `src/silk/synthesis.rs`, `src/silk/lsf.rs`, `src/silk/excitation.rs`, `src/silk/params.rs`, `src/silk/resampler.rs`, `src/silk/constant.rs`, and `src/silk/mod.rs`.
   - Replaced C-style `if / else if / else` chains with flat `match` expressions.
   - Replaced manual `while` / indexed loops with slice iterators (`iter()`, `iter_mut()`, `zip()`, `fold()`, `enumerate()`).
   - Extracted named Q-format constants in `src/silk/plc.rs` (`MAX_PLC_GAIN_EXPAND_Q16`, `MAX_CNG_SMTH_GAIN_Q16`, `CNG_GAIN_DOWNSCALE_SHIFT`, `CNG_RAND_LCG_SHIFT`, `WB_LPC_MAX_GAIN_Q12`, `NB_MB_LPC_MAX_GAIN_Q12`).
   - Added declarative test macros (`assert_silk_gains!`, `assert_nlsf_to_lpc!`, `assert_resampler_tail!`, `assert_plc_loss_step!`).

2. **CELT Layer Idiomatic Functional Refactoring (`c0eb7d4`)**
   - Refactored `src/celt/decoder.rs`, `src/celt/vq.rs`, `src/celt/rate.rs`, `src/celt/mdct.rs`, `src/celt/energy.rs`, `src/celt/cwrs.rs`, `src/celt/constant.rs`, and `src/celt/mod.rs`.
   - Eliminated all `else if` cascades and standardized index names to `i`, `j`, `k`.

3. **Top-Level `OpusDecoder` & `entropy.rs` Refactoring (`d726564` & latest commit)**
   - Refactored `src/decoder.rs` and `src/entropy.rs`.
   - Eliminated per-packet `Vec` heap allocations in `OpusDecoder::decode_ref` and `OpusDecoder::decode_lost_packet` by using pre-allocated `[f32; MAX_PACKET_SAMPLES_48K]` scratch buffers on `OpusDecoder`.
   - Decomposed `StreamDecoder::decode_frame` into focused, single-purpose helper methods with `match mode` dispatch:
     - `StreamDecoder::decode_lost_or_dtx_frame`
     - `StreamDecoder::conceal_transition_tail`
     - `StreamDecoder::decode_silk_layer`
     - `StreamDecoder::decode_active_celt_layer`
     - `StreamDecoder::flush_hybrid_celt_overlap_tail`
     - `StreamDecoder::decode_redundant_celt_frame`
   - Extracted the ~1,120 lines of generated `libopus 1.6.1` oracle test vector arrays out of `src/decoder.rs` into a dedicated `#[cfg(test)]` module `src/test_vectors.rs` so `src/decoder.rs` stays concise and readable.

4. **Documentation & Style Guidelines (`8dde01b`, `5d93fef`, `448016f`)**
   - Consolidated `present.md` into `symphonia-codec-opus/README.md` (and removed `present.md`), replacing all Mermaid diagrams with RFC 6716 / RFC 7845 ASCII `text` diagrams and clickable RFC section links covering SILK, CELT, Hybrid mode, mode transitions, and multi-stream surround decoding.
   - Added RFC ASCII architecture diagrams and clickable RFC links to `src/lib.rs`, `src/silk/mod.rs`, and `src/celt/mod.rs`.
   - Updated `AGENTS.md` with concrete `BAD` vs `REQUIRED` Rust style rules, ASCII diagram standards, and pre-commit verification commands.

5. **Refactor step S01 (#13), truth and dead code (2026-11-09)**
   - Removed the `dead_code`, `unused_imports`, `unreachable_patterns` allows from `lib.rs`; deleted 47 dead production items (encoder-side PVQ search and stereo angle, `silk_plc_*` wrappers, `Comment` header parsing, unreachable `match` arms, unused setters, duplicate `denormalise_bands`); 91 tests green with clippy `-D warnings` (`--no-deps`), zero doc warnings, gate regex clean.
   - README conformance row and module tree made truthful; `makefile` and unused dev-dependencies removed; AGENTS.md names RFC 6716 Appendix A as updated by RFC 8251 as the only reference decoder.

6. **Refactor step S02 (#14), verified defects (2026-11-09)**
   - Fixed with tests written first: two `String` leaks in error conversion, `u8` overflow and missing per-family channel bounds and family 255 mapping table in `header.rs` (RFC 7845 Section 5.1.1), Code 3 padding flag without a length byte (RFC 6716 Section 3.2.5), one-byte frames decoded as audio instead of concealed (RFC 6716 Appendix A), vacuous TOC tests, fallible range decoder constructor, saturating NLSF spacing (RFC 8251 Section 7). 98 tests green, gate clean.
   - Left open in S02 until reference decoder outputs were available: the stereo-to-mono rule at a changed internal rate (fixed in entry 14) and the comb filter during CELT noise concealment (kept: concealment is non-normative, RFC 6716 Section 4.4).

7. **Refactor step S03 (#15), shared value types (2026-11-09)**
   - Added `src/fixed.rs` (Q-format arithmetic with semantic names), `silk::SilkLayout` (RFC 6716 Section 4.2.2 frame geometry), `celt::BlockSize` (the `LM` exponent), `src/sample_rate.rs` (`SampleRate`), and `entropy::Cdf` (compile-time validated cumulative distribution tables, every table renamed `CDF_*`). 103 tests green after each type.
   - `Planes` landed last: every channel-pair signature in the four decoder files takes one value; 108 tests green. S03 complete.

8. **Refactor step S04 (#16), framing and entropy (2026-11-09)**
   - `toc.rs`: infallible `Toc::from(u8)`, RFC 6716 Table 2 as one `CONFIG_TABLE` behind `Config`, `FrameCountCode`, no per-packet logging, no error type.
   - `packet.rs`: one parser for undelimited and self-delimited framing (`Framing`), `Frames` with 48 fixed slots and no heap allocation, seven error variants named after the RFC 6716 Section 3.4 rules, cursor-based single pass.
   - `entropy.rs`: RFC vocabulary in rustdoc, named constants in `tell_frac` and `decode_laplace`, one refill loop for raw bits.
   - The `frame_size` to `frame_duration` rename and the `AudioMode::{Silk, Celt}` casing were completed in S05; the `final_range` method rename on the range decoder waits for its CELT callers (S09d).

9. **Refactor step S08 (#20), CELT file split (2026-11-09)**
   - `celt/mdct.rs` (1620 lines, six responsibilities) split as pure moves into `fft.rs`, `mdct.rs` (inverse MDCT only), `synthesis.rs`, `postfilter.rs`, `plc.rs`; module index in `celt/mod.rs` rewritten with one sentence and the RFC section per submodule. 108 tests green.

10. **Refactor step S05 (#17), top-level decoder (2026-11-09)**
   - `decoder.rs` re-expressed in RFC 6716 Section 4.4 and 4.5 terms: `FrameConfig`, `LastFrame`, `Redundancy`, `FramePayload`, `Transition`, one `crossfade`, `OutputRouting`, `StreamPackets`; no production function over 60 lines; no libopus citations.
   - Output gain per RFC 7845 Section 5.1, buffer sized to 120 ms, one CELT silence frame constant, `AudioMode::{Silk, Celt}`, `Bandwidth` variants in RFC spelling; `lib.rs` keeps only `needless_return`. 108 tests green.

11. **Session hand-over (2026-11-09, 10:30 EET)**
   - Issues #13 to #24 track the twelve steps. The RFC 6716 Appendix A source, the RFC 8251 patch and the RFC 8251 test vectors are downloaded with verified SHA-1 sums; building the patched reference decoder needs a manual run. S07 and S10 come next.
   - In flight at hand-over: S06 (SILK state model), S09 a to c (CELT state model), the docs pass on the root modules, and `tests/rfc6716_conformance.rs`.
   - Final state at 10:52: 115 unit tests and the new RFC 6716 Section 6 conformance test pass (all 12 RFC 8251 vectors, 48 kHz mono and stereo); clippy with warnings denied, `cargo doc`, `cargo fmt --check` and the index regex are clean. S06 complete apart from three small items, S09 a to c, S10 for `fft`, `mdct`, `cwrs`, `plc`, and the RFC-anchored documentation pass over every module except `decoder.rs` are done; S09d, S10 for `vq`, `rate`, `energy`, `constant`, `postfilter`, S07 and fixture relabelling remain.

12. **Refactor step S10 (#22), CELT algorithms (2026-11-09)**
   - `rate.rs`: `CacheLevel` replaces signed cache-row arithmetic; `BandAllocation::compute(dec, AllocationLayout, AllocationBudget)` replaces the twelve-argument allocation entry; `fine_priority` is `[bool; MAX_BANDS]`; the post-filter field is `period` (RFC 6716 Section 4.3.7.1 "pitch period").
   - `vq.rs`: PVQ band decoding rebuilt around `ShapeFrame`, `Partition`, `Lowband` and `BandSweep`; no lint allows, no function over 60 lines.
   - `energy.rs`: one `Range<usize>` band parameter per stage, the channel count taken from the energy rows, named constants for the coarse-residual budget rule and the non-normative loss-recovery rule, exact power-of-two fine steps. The fine stage no longer skips bands whose bits exceed the frame: the RFC 6716 Appendix A decoder (with RFC 8251) has no such check, and a new unit test pins the reference behaviour.
   - `celt/decoder.rs`: silence, transient and intra flags decoded through one budgeted-flag helper with the RFC 6716 Table 56 probabilities; one complete band-range check; `entropy::Decoder::storage_bits` replaces four byte-to-bit conversions.
   - `mdct.rs`: one inverse-MDCT loop where the long block is the one-block case. Comb constants renamed `MIN_PITCH_PERIOD` and `PITCH_HISTORY_LEN`; the unreachable upper clamp of the comb period removed.
   - The `celt/decoder.rs` fixtures stay in that module's tests: AGENTS.md Section 5.1 requires fixtures in the owning module's test module.
   - Verified: 118 unit tests, clippy with warnings denied, `cargo doc`, the index regex, and the RFC 6716 Section 6 conformance suite (120 of 120 runs: twelve vectors, five output rates, mono and stereo, final range equal on every packet).

13. **Refactor step S07 (#19), SILK kernels and modules (2026-11-09)**
   - The partial rewrite of `params`, `lsf`, `excitation`, `lpc`, `synthesis`, `resampler` and `mod` left by an interrupted agent was integrated: parse stages are free functions or methods on the new `CodingReferences` (the RFC 6716 Section 4.2.7.4 chain of conditional-coding references), `ChannelHeader` replaces parallel flag arrays, and the comfort noise estimate reads a `CngFrameInput` view instead of a copied frame.
   - `silk/decoder.rs`: the 10 KB `lbrr_scratch` copy and its swaps are gone; LBRR frames are parsed against fresh `CodingReferences`, regular frames against the channel's own. Frame-pair output is split into unmixing and resampling; no `pub(super)` fields.
   - `silk/plc.rs`: voiced and unvoiced concealment share one excitation loop (`ExcitationShape`, `ConcealmentGeometry`); both `too_many_arguments` allows are gone; state fields are private and documented.
   - Component tests moved to their owning modules (AGENTS.md Section 5.1). Wrong section anchors fixed crate-wide (`4.2.7.9.3`, `4.2.7.9.4` and `4.3.2.3` do not exist in RFC 6716).
   - Verified: 119 unit tests, clippy with warnings denied, `cargo doc`, and the conformance suite with every range check and quality figure identical to the run before the step. No production function in `silk/` exceeds 60 lines.

14. **Fixes, documentation and fixture provenance (2026-11-09)**
   - SILK channel transitions follow RFC 6716 Sections 4.2.7.4 and 4.2.7.5.5: a side channel that the previous Opus frame did not code restarts as after a reset, and a stereo stream turning mono at a new internal rate duplicates the mid channel. Both were pinned by tests that failed first; testvector10 at 48 kHz stereo improved from quality 99.0 to 99.8.
   - CELT: split angles decode in closed form through `entropy::Decoder::decode_with` (no per-split table), `decode_bit_allocation` is shared by the decoder and its test, one de-emphasis loop, `decode_spread_decision` returns `Spread`. SILK: one `sqrt_approx` and one LCG step. Error enums keep one message per variant and no dead variants.
   - Documentation: one module-doc shape crate-wide; nonexistent RFC sections and wrong figure numbers corrected; fixtures labelled as normative fixtures or regression pins; tests renamed after what they assert. AGENTS.md uses the tightened index regex.
   - Verified: 121 unit tests, clippy with warnings denied, `cargo doc` with and without private items, the index regex, and the conformance suite (120 of 120 runs; every figure identical to the baseline apart from the testvector10 improvement).

15. **Error variants, fixture tables and visibility (2026-11-09)**
   - Every error variant of the four error enums is tested or ruled out. Five SILK variants guarded states that the probability tables make impossible; those tables are now indexed directly behind compile-time assertions on their symbol counts, and the variants are gone. The other variants have exact-variant tests.
   - The recorded fixtures are private; the tests iterate 14 scenario tables, which halved the top-level decoder test module. The gate runs `cargo doc` with `--document-private-items`.
   - Only `OpusDecoder` and its methods are public; every other item is `pub(crate)` or private, so the compiler reports dead code again, and there is none.
   - Verified: 130 unit tests, clippy, private-item rustdoc, the index regex, and conformance identical on all 120 runs.

---

## 3. Handoff Notes for Next Worker

- **Verification Commands**:
  Before any commit on `symphonia-codec-opus`, run all four checks specified in `AGENTS.md`:
  ```bash
  rg -n "else if|\bix\b|\w*idx\w*" symphonia-codec-opus/src/
  cargo test -p symphonia-codec-opus -- --test-threads=1
  cargo clippy -p symphonia-codec-opus --all-targets --no-deps -- -D warnings
  cargo doc -p symphonia-codec-opus --no-deps --document-private-items
  ```
- **Potential Follow-Up Tasks** (from before the refactor, kept for history):
  - If further test vector separation is desired in submodules, the fixture constants in `src/celt/decoder.rs` (`MONO_PKT_*`, `PLC_*`, `STEREO_PKT_*`, `HYBRID_PKT`) can also be moved into `src/test_vectors.rs`.
  - End-to-end Ogg Opus demuxer + `OpusDecoder` integration tests with `symphonia-format-ogg` if container-level testing is requested.

---

## 4. Refactoring Review (2026-11-08, revised 2026-11-09)

A full read-only review of the crate produced 170 findings, and five more came up during the work.
They were ordered into twelve dependency-ordered steps (`S01` to `S12`) that issues #13 to #24
track. Source of truth is RFC 6716 (prose plus the normative Appendix A reference decoder) as
updated by RFC 8251, and RFC 7845; libopus mainline is not an authority, and fixtures recorded
from `libopus 1.6.1` are regression pins unless the conformance test covers what they pin.
