# AGENTS.md - Symphonia (`symphonia-codec-opus`) Repository Instructions

Applies to all agents working in this repository.

## 1. Start with the task

- Understand the requested outcome and inspect the relevant files before changing them.
- Resolve conflicts in this order:
  1. Current user instruction
  2. Normative specifications: RFC 6716 (Opus Audio Codec) and RFC 7845 (Ogg Opus Encapsulation)
  3. `symphonia-core` codec traits and API contracts (`AudioDecoder`, `RegisterableAudioDecoder`, `PacketRef`, `AudioBuffer`)
  4. Project roadmap issues on `jiztastamablastamarang/symphonia` (`#3`-`#12`)
  5. Existing `symphonia-codec-opus` implementation
- Verify status claims cheaply before relying on them. Source files, compiler output, and passing tests are evidence; a status note or closed label alone is not.
- Inspect discoverable facts directly in the workspace. Ask only about material intent, authorization, or design trade-offs that inspection cannot resolve.

## 2. Keep the solution proportional

- Make the smallest complete change that satisfies the request and RFC requirements.
- Reuse `symphonia-core` primitives (`BitReaderLtr`, `BitReaderRtl`, `FiniteBitStream`, `ReadBytes`, `AudioBuffer`, `AudioSpec`, `Channels`), standard library types, and existing workspace dependencies (`thiserror`, `log`). Add a new dependency only when it materially improves correctness or reduces maintained code, and explain the trade-off briefly.
- Fix defects at their owning code path, preserve unrelated work, and avoid opportunistic refactors. Never wrap a defect in fallbacks, staleness checks, or flags, and delete dead code instead of keeping it as a fallback.
- Do not weaken specification requirements to match a failing or partial implementation.
- Prefer `rg` for code search. Cap potentially long command output by bytes (`| head -c 4000`), never by lines.
- Before reporting completion, check the result against acceptance criteria, inspect the final diff, run the Rust verification gate, and report any unresolved gaps. Distinguish implemented, executed, and verified work.

### Communication and formatting rules

- Never use the em-dash or en-dash (`U+2014`, `U+2013`) in responses, commit messages, comments, or documentation; use a standard hyphen (`-`), colon (`:`), or comma instead.
- When writing reports, plans, or summaries, use passive or impersonal voice instead of first-person pronouns ("I", "we") and stay concise.

## 3. Apply design principles proportionally

Prefer deterministic bitstream and DSP transformations with explicit inputs and outputs, written in idiomatic, functional Rust rather than mechanical C translations.

| Principle | Requirement in `symphonia-codec-opus` |
| :--- | :--- |
| Deterministic DSP & Bitstream Core | Keep range decoding, parameter unpacking, LSF/LTP/LPC synthesis, and resampling deterministic with explicit state transitions. Use exact RFC 6716 fixed-point integer arithmetic (`Q7`, `Q8`, `Q10`, `Q12`, `Q14`, `Q15`, `Q16`, `Q23`) throughout all SILK stages (converting to `f32` only at the final output buffer), and use integer range/PVQ decoding paired with `f32` DSP in CELT (CELT MDCT sizes `120 * 2^k` are not powers of two, so `symphonia_core::dsp::mdct::Imdct` cannot be used). |
| Zero Per-Frame & Per-Packet Heap Allocation | Never allocate `Vec` in per-packet, per-frame, or per-subframe decode hot paths (`OpusDecoder::decode_ref`, `OpusDecoder::decode_lost_packet`, `silk::Decoder`, `celt::Decoder`). Use fixed-size RFC-bounded stack arrays (`MAX_SILK_FRAMES = 3`, `MAX_SUBFRAMES = 4`, `MAX_LPC_ORDER = 16`, `LTP_ORDER = 5`, `MAX_SUBFRAME_SAMPLES = 80`, `MAX_FRAME_SAMPLES = 320`, `MAX_PACKET_SAMPLES_48K = 5760`) and pre-allocated synthesis/scratch buffers (`DecodedOpusFrame`, `OpusDecoder::scratch_ch0`, `scratch_ch1`). |
| Functional & Iterator-First Rust (Not C Clones) | Never write C-clone manual index loops (`while i < N`, element-by-element history shifts, or unrolled C pointer arithmetic) when Rust slice methods and iterator combinators express the operation cleanly. Use `std::array::from_fn`, `.iter()`, `.iter_mut()`, `.zip()`, `.enumerate()`, `.fold()`, `.take_while()`, `.for_each()`, `.chunks_exact()`, `.chunks_exact_mut()`, `.windows()`, `.fill()`, `.copy_from_slice()`, and `.copy_within()`. Deduplicate repeated DSP blocks into cohesive helper functions. |
| Clean Code & Domain Naming | Use domain-specific RFC terminology (`toc`, `vad_flag`, `lbrr_flag`, `nlsf_q15`, `res_q10`, `ltp_coeffs`, `cdf`), named Q-format constants instead of magic numbers, cohesive functions, and flat control flow with early guard returns. |
| SOLID & GRASP | Keep module responsibilities separated: `OpusDecoder` (`decoder.rs`) owns `FramePacket` parsing, `entropy::Decoder<'a>` instantiation over `&'a [u8]`, and the output `AudioBuffer<f32>`, passing `&mut entropy::Decoder<'_>` into `silk::Decoder` and `celt::Decoder`. Split `silk/` and `celt/` into focused submodules (`params`, `lsf`, `excitation`, `synthesis`, `resampler`, `plc`, `energy`, `rate`, `cwrs`, `vq`, `mdct`, `decoder`, `constant`, `error`). Do not introduce wrapper traits (such as `RangeDecoder`) when concrete `entropy::Decoder<'a>` over `&'a [u8]` suffices for both production and real-bitstream unit tests. |

## 4. `symphonia-codec-opus` code style and Rust conventions

All code in `symphonia-codec-opus/` must preserve the readability, transparency, and idiomatic Rust style established in `toc.rs`, `header.rs`, and `packet.rs`:

- **Zero `else if` Chains (`match`-First Control Flow)**:
  - Never use `else if` anywhere in `symphonia-codec-opus/src/` (`rg "else if" symphonia-codec-opus/src/` must return `0` matches).
  - Replace multi-branch conditionals with exhaustive `match` expressions (`match a.cmp(&b)` with `std::cmp::Ordering`, tuple patterns `match (mode, bandwidth)`, or `match` guards) and early guard clauses (`if !active { return; }`) to avoid deep nesting.
  - Use concise one-line standard library arithmetic methods such as `value.checked_shr(shift).unwrap_or(0)` instead of multi-line shift-guard `if / else` blocks.
- **Functional Array & Slice Transformations**:
  - Construct fixed-size arrays functionally with `std::array::from_fn(|i| ...)` instead of mutating a zero-initialized array in a `while` or `for` loop.
  - Shift delay lines and history buffers with `slice.copy_within(src_range, dst_start)` and clear buffers with `slice.fill(0)` rather than manual index-copy loops.
  - Express dot products, energy accumulations, and max-gain searches with `.iter().zip(...).fold(...)` or `.iter().enumerate().fold(...)`.
- **Consistent Index Naming (`i`, `j`, `k`) & Minimal Type Casting**:
  - Always name loop and closure index variables `i` (and `j`, `k` for nested loops). Never use `ix` or any name containing `idx` (`idx`, `s_idx`, `b_idx`, `frame_idx`, `subframe_idx`, `idx0`).
  - Use `usize` natively for slice indices, band indices, and block sizes (`lm: usize`, `start_band: usize`, `end_band: usize`) to avoid noisy `as usize`, `as isize`, and `as i32` casts.
- **Named Q-Format & Domain Constants (No Magic Numbers)**:
  - Never write unexplained bit-shift or fixed-point magic numbers (`1 << 16`, `1 << 21`, `1 << 23`, `5`, `6`, `32767`) inline. Define named, documented constants that state the exact Q-format and RFC 6716 meaning (e.g., `UNIT_GAIN_Q16 = 1 << 16`, `CNG_HIGH_PLC_GAIN_THRES_Q16 = 1 << 21`, `CNG_HIGH_SMTH_GAIN_THRES_Q16 = 1 << 23`, `CNG_PLC_ENERGY_SUB_SHIFT = 5`, `Q16_TO_Q10_SHIFT = 6`).
- **Explicit `return` statements**:
  - Use explicit `return expr;` statements at the end of functions and methods (`return Ok(...);`, `return Err(...);`, `return value;`) rather than implicit tail expressions.
- **RFC ASCII Diagrams & Doc Comments**:
  - Use `//!` for module-level documentation at the top of files. Never place `///` doc comments immediately above `use` imports.
  - Use RFC 6716 / RFC 7845 ASCII bit-layout diagrams, block diagrams, and lookup tables inside ```` ```text ```` code fences with a single space after `///` or `//!` (`/// ```text`) consistently across all crate documentation (`symphonia-codec-opus/README.md` and rustdoc). Never mix Mermaid diagrams with RFC ASCII diagrams, and keep crate documentation unified in `symphonia-codec-opus/README.md` and rustdoc rather than separate presentation markdown files.
  - Accompany every RFC diagram, table, and submodule with clickable links to the exact RFC 6716 / RFC 7845 / RFC 8251 section and figure (`<https://datatracker.ietf.org/doc/html/rfc6716#section-...>`) so `cargo doc` renders clickable hyperlinks with zero warnings.
- **Safety & Error Handling**:
  - Maintain `#![forbid(unsafe_code)]` in `symphonia-codec-opus/src/lib.rs`.
  - Never use `.unwrap()` or `.expect()` on external packet or header input in non-test code; validate untrusted bitstream data at boundaries and propagate errors with `?`.
  - Define module-scoped error enums (`toc::Error`, `packet::Error`, `header::Error`, `silk::error::Error`, `celt::error::Error`) using `thiserror::Error` and convert them into `symphonia_core::errors::Error`.
- **Idiomatic Conversions & Representations**:
  - Use `TryFrom` and `From` (`TryFrom<u8>`, `TryFrom<&[u8]>`, `From<FrameDuration> for Duration`) for parsing and type conversions.
  - Use `#[repr(u32)]` / `#[repr(u64)]` enum discriminants when variants map directly to numeric constants (such as `Bandwidth` sample rates or `FrameDuration` nanoseconds).
- **Codebooks & Constants**:
  - Store RFC lookup tables, codebooks, and cumulative distribution tables (`CDF_*`, built with `entropy::Cdf::new`) in `symphonia-codec-opus/src/silk/constant.rs` (organized under domain submodules `header`, `gain`, `lsf`, `ltp`, `excitation`, `resampler` and re-exported at the `constant` root) and `symphonia-codec-opus/src/celt/constant.rs`.
  - Use fixed-size arrays (`[[u8; 10]; 32]`, `[u32; N]`) rather than slice references (`&[u32]`), and build every `CDF_*` constant as `Cdf::new(&[total, cum_0, cum_1, ..., total])` so the layout `[total, fh_0, ..., total]` is validated at compile time and consumed by `entropy::Decoder::decode_symbol` without per-call checks.
- **Workspace Membership**:
  - `symphonia-codec-opus` is a member of the root Cargo workspace (`Cargo.toml`). Never add a `[workspace]` table to a member crate.

### 4.1 Concrete Anti-Patterns vs. Required Idiomatic Rust Examples

```rust
// 1. Control flow: NEVER write `else if` cascades or deeply nested `if / else`.
// BAD:
if frame_type == SignalType::Voiced {
    pitch_lag = last_pitch;
} else if prev_frame_type == SignalType::Voiced {
    pitch_lag = prev_pitch;
} else {
    pitch_lag = default_pitch;
}
// REQUIRED (`match` with tuple / guards / `Ordering` + early return):
let pitch_lag = match (frame_type, prev_frame_type) {
    (SignalType::Voiced, _) => last_pitch,
    (_, SignalType::Voiced) => prev_pitch,
    _ => default_pitch,
};

// 2. Shift bounds: NEVER write multi-line shift-guard `if / else` blocks.
// BAD:
energy = if energy_rshift < 32 {
    energy >> energy_rshift
} else {
    0
};
// REQUIRED (one-line standard library method):
energy = energy.checked_shr(energy_rshift).unwrap_or(0);

// 3. Array construction: NEVER mutate a zeroed array with a `while` or `for` loop.
// BAD:
let mut buf = [0i16; MAX_LPC_ORDER];
let mut i = 0;
while i < MAX_LPC_ORDER {
    buf[i] = ((i as i32 * step) >> 15) as i16;
    i += 1;
}
// REQUIRED (`std::array::from_fn` without extra allocations or borrows):
let buf = std::array::from_fn(|i| ((i as i32 * step) >> 15) as i16);

// 4. Conditional slice mutation: Prefer iterator combinators over imperative `for` + `break`.
// BAD:
for sample in frame_pcm.iter_mut() {
    gain_q16 += step_q16;
    if gain_q16 >= target_q16 {
        break;
    }
    *sample = ((*sample as i64 * gain_q16 as i64) >> 16) as i16;
}
// REQUIRED (`.iter_mut().take_while(...).for_each(...)`):
frame_pcm
    .iter_mut()
    .take_while(|_| {
        gain_q16 += step_q16;
        return gain_q16 < target_q16;
    })
    .for_each(|sample| {
        *sample = ((*sample as i64 * gain_q16 as i64) >> 16) as i16;
    });

// 5. Reductions & index naming: Always use `i` (never `idx` or `ix`) and `.fold(...)`.
// BAD:
let mut max_subfr = 0usize;
let mut max_gain = 0i32;
for (idx, &gain_q16) in subframe_gains[..active].iter().enumerate() {
    if gain_q16 > max_gain {
        max_gain = gain_q16;
        max_subfr = idx;
    }
}
// REQUIRED:
let (max_subfr, _) = subframe_gains[..active]
    .iter()
    .enumerate()
    .fold((0usize, 0i32), |(best_i, best_gain), (i, &gain_q16)| {
        if gain_q16 > best_gain {
            return (i, gain_q16);
        }
        return (best_i, best_gain);
    });

// 6. Named Q-format constants: NEVER inline unexplained fixed-point shifts or thresholds.
// BAD:
let cng_gain_q16 = if plc_gain_q16 >= (1 << 21) || smth_gain_q16 > (1 << 23) {
    smulww(plc_gain_q16, plc_gain_q16) >> 5
} else {
    smulwb(smth_gain_q16, plc_gain_q16)
};
// REQUIRED:
const CNG_HIGH_PLC_GAIN_THRES_Q16: i32 = 1 << 21;
const CNG_HIGH_SMTH_GAIN_THRES_Q16: i32 = 1 << 23;
const CNG_PLC_ENERGY_SUB_SHIFT: i32 = 5;
```

## 5. Testing approach and verification gate

Testing in `symphonia-codec-opus` follows a five-layer strategy from static table invariants up to end-to-end RFC conformance:

### 5.1 Core testing rules

- **Reproduce before fixing**: Start every bug fix from a failing unit test or reproducing input whose failure was observed before modifying production code.
- **Never weaken expectations**: Never edit a test assertion or expected vector merely to make a failing test pass. Update an expectation only when proven incorrect against RFC 6716 (the text or the normative Appendix A reference decoder), RFC 7845, or RFC 8251, and cite the RFC section in the test or commit. libopus mainline is not a source of truth.
- **Real Bitstreams & RFC Reference Decoder Ground Truth**: Do not use mock range decoders that bypass `entropy::Decoder` or ignore iCDF tables. Instead, feed real range-coded packet byte slices `&[u8]` (extracted from official RFC 6716 `opus_testvectors` or generated with the RFC 6716 Appendix A reference decoder carrying the RFC 8251 patch, built in a scratch directory) into `entropy::Decoder<'a>` and assert intermediate and final outputs (`vad_flags`, `lbrr_flags`, `stereo_weights`, `gain_q16`, `nlsf_q15`, `pitch_lag`, `ltp_coeffs`, `excitation`, and synthesized PCM) against ground-truth values produced by that reference decoder.
- **Compact, Readable Tests with Declarative Macros (`macro_rules!`) & `TestCase` Tables**:
  - Match the clarity and compactness of `toc.rs`, `header.rs`, and `packet.rs` tests.
  - Eliminate repetitive decode-and-assert boilerplate using module-local declarative macros (`assert_mono_f32_packets!`, `assert_stereo_f32_packets!`, `assert_celt_frame_matches_reference!`, `assert_plc_loss_steps!`, `assert_subframe_field!`), `TestCase` table structs, and stack-array helpers (`std::array::from_fn` instead of `.collect::<Vec<_>>()`).
- **Keep tests self-contained and deterministic**: Embed extracted packet byte slices and expected reference decoder values directly in `#[cfg(test)] mod tests` at the bottom of the owning module so unit tests execute with zero external network or filesystem dependencies.

### 5.2 Five testing layers

| Layer | Scope | Required Pattern in `symphonia-codec-opus` |
| :--- | :--- | :--- |
| **1. Table & Constant Invariants** | `silk/constant.rs`, `celt/constant.rs` | Verify structural and mathematical invariants on all static RFC tables: CDF format (`cdf[0] == total`, `cdf.last() == Some(&total)`, strict cumulative monotonicity), codebook selector bounds (`0..16` for LSF Stage-2, `0..2` for prediction weights), permutation validity (LSF polynomial evaluation orderings), and symmetry/monotonicity of trigonometric lookup tables (Q12 cosine table). |
| **2. Bitstream & Header Parsers** | `toc.rs`, `packet.rs`, `header.rs` | Use **table-driven exhaustive tests** (`TestCase` arrays covering all 32 TOC configs, bandwidths, frame durations, and code modes `0..=3`) and **fluent test builders** (`PacketBuilder`, `IdHeaderBuilder`, `CommentHeaderBuilder`) to construct valid and malformed binary inputs cleanly without opaque raw byte slices. |
| **3. Boundary & Malformed Input Checks** | All parsers and decoders | Explicitly test every error variant in the module's `Error` enum: empty/truncated slices (`PacketTooShort`, `UnexpectedEof`), frame length overflow (`> 1275` bytes), multi-byte padding (`255` -> `254` bytes), total packet duration boundary (`<= 120_000 us` across `2.5 ms`..`60 ms` frames, `<= 48` frames), zero-length DTX frames, invalid magic signatures, unsupported versions, and invalid channel mappings. |
| **4. Real-Packet Entropy & DSP Stage Tests** | `entropy.rs`, `silk/*`, `celt/*` | 1) In `entropy.rs`, test range decoder initialization, normalization (`rng`, `val`), final-byte LSB handling, `decode_symbol`, `decode_symbol_logp`, backward raw bits (`decode_raw_bits`), uniform uint (`decode_uint`), Laplace, and `tell`/`tell_frac` against RFC 6716 Section 4.1 reference decoder outputs.<br>2) In `silk/` and `celt/`, feed real range-coded packet bytes (from RFC 6716 `opus_testvectors` and the RFC 6716 Appendix A reference encoder) through `entropy::Decoder<'a>` and assert bit-exact equality (`assert_eq!`) against reference decoder dumps for every stage (`decode_silk_header`, `decode_stereo_prediction_weights`, `decode_gains`, `decode_lsf`, `decode_ltp`, `decode_excitation`, `lsf_to_lpc`, `ltp_synthesis`, `lpc_synthesis`, stereo unmixing, resampling, coarse/fine energy, PVQ/CWRS, IMDCT, post-filter, PLC). |
| **5. End-to-End Decoder & RFC Test Vectors** | `decoder.rs`, integration tests | Verify `OpusDecoder` lifecycle (`try_new`, `decode_ref`, `reset`, `finalize`, `last_decoded`) and validate full SILK, CELT, and Hybrid packet decoding against the official IETF RFC 6716 / RFC 8251 test vectors (`testvector01`..`testvector12`) using bit-exact internal parameter checks and the RFC 6716 Section 6.1 `opus_compare` quality metric. |

### 5.3 Mandatory verification gate

Before reporting any Rust change complete or closing an issue, execute and pass all three checks:

```bash
cargo test -p symphonia-codec-opus -- --test-threads=1
cargo clippy -p symphonia-codec-opus --all-targets --no-deps -- -D warnings
cargo doc -p symphonia-codec-opus --no-deps --document-private-items
```

- All unit, integration, and doc tests must pass (`0 failed`).
- `cargo clippy` must report zero warnings for this crate (`--no-deps` scopes the lints to it; `-D warnings` fails the gate on any).
- `cargo doc -p symphonia-codec-opus --no-deps --document-private-items` must complete with `0 warnings`; the crate's modules are private, so the flag is what checks their intra-doc links.

## 6. Git, branch organization, and authorization

- **Remotes & Branches**:
  - `origin`: `https://github.com/jiztastamablastamarang/symphonia.git` (user fork)
  - `upstream`: `https://github.com/pdeljanov/Symphonia.git` (parent repository)
  - `main`: tracks `upstream/main` 1:1. Never commit Opus work directly to `main`.
  - `opus-decoder`: active development branch rebased linearly onto `upstream/dev-0.6`; it is the head branch of the open upstream Pull Request `pdeljanov/Symphonia#398`, so every push to `origin/opus-decoder` updates that PR.
  - Do not mark PR `#398` ready for review or open further Pull Requests against `pdeljanov/Symphonia` without the user's explicit instruction.
  - Remove `AGENTS.md` from the branch (`git rm AGENTS.md` or drop its commit during rebase) before PR `#398` is marked ready for review.
- **Staging & Commits**:
  - Commit only when requested by the user.
  - Never commit with `git add -A` or broad snapshot commands; stage explicit file paths (`git add <path>`) and inspect `git diff --cached --stat` before committing.
  - Keep separate logical fixes or issues in separate commits when requested.
  - Never commit credentials, tokens, editor-generated changelogs (`CHANGELOG.md`), or temporary scratch files (`help.txt`) to tracked paths. Store disposable scripts or logs in untracked scratch locations only.
  - NEVER include Claude/AI-related comments, attributions, co-author tags, or signatures in commit messages, PR descriptions, or code comments.

