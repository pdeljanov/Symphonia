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

---

## 3. Handoff Notes for Next Worker

- **Verification Commands**:
  Before any commit on `symphonia-codec-opus`, run all four checks specified in `AGENTS.md`:
  ```bash
  rg -n "else if|\b(ix|idx|s_idx|b_idx|frame_idx|subframe_idx)\b" symphonia-codec-opus/src/
  cargo test -p symphonia-codec-opus -- --test-threads=1
  cargo clippy -p symphonia-codec-opus --all-targets
  cargo doc -p symphonia-codec-opus --no-deps
  ```
- **Potential Follow-Up Tasks**:
  - If further test vector separation is desired in submodules, the `libopus` oracle constants in `src/celt/decoder.rs` (`MONO_PKT_*`, `PLC_*`, `STEREO_PKT_*`, `HYBRID_PKT`) can also be moved into `src/test_vectors.rs`.
  - End-to-end Ogg Opus demuxer + `OpusDecoder` integration tests with `symphonia-format-ogg` if container-level testing is requested.
