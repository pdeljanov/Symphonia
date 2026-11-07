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

Prefer deterministic bitstream and DSP transformations with explicit inputs and outputs. Ordinary loops and local buffer mutation remain appropriate when clearer.

| Principle | Requirement in `symphonia-codec-opus` |
| :--- | :--- |
| Deterministic DSP & Bitstream Core | Keep range decoding, parameter unpacking, LSF/LTP/LPC synthesis, and resampling deterministic with explicit state transitions. Preserve exact Q-format fixed-point scaling (`Q7`, `Q8`, `Q10`, `Q12`, `Q14`, `Q15`, `Q16`, `Q23`) specified in RFC 6716. |
| Clean Code & Domain Naming | Use domain-specific RFC terminology (`toc`, `vad_flag`, `lbrr_flag`, `nlsf_q15`, `res_q10`, `ltp_coeffs`, `icdf`), cohesive functions, and clear control flow. Avoid clever compression. |
| SOLID & GRASP | Keep module responsibilities separated (`toc`, `packet`, `header`, `entropy`, `silk`, `celt`, `decoder`). Introduce traits (such as `RangeDecoder`) only where actual substitution or isolated unit testing requires them. |

## 4. `symphonia-codec-opus` code style and Rust conventions

All code in `symphonia-codec-opus/` must preserve the established crate style:

- **Explicit `return` statements**: Use explicit `return expr;` statements at the end of functions and methods (`return Ok(...);`, `return Err(...);`, `return value;`) rather than implicit tail expressions.
- **RFC ASCII Diagrams & Doc Comments**:
  - Use `//!` for module-level documentation at the top of files. Never place `///` doc comments immediately above `use` imports.
  - Include RFC 6716 / RFC 7845 ASCII bit-layout diagrams, block diagrams, and lookup tables inside ```` ```text ```` code fences with a single space after `///` or `//!` (`/// ```text`).
  - Surround all URLs in doc comments with angle brackets (`<https://datatracker.ietf.org/doc/html/rfc6716#section-...>`) so `cargo doc` renders clickable hyperlinks with zero warnings.
- **Safety & Error Handling**:
  - Maintain `#![forbid(unsafe_code)]` in `symphonia-codec-opus/src/lib.rs`.
  - Never use `.unwrap()` or `.expect()` on external packet or header input in non-test code; validate untrusted bitstream data at boundaries and propagate errors with `?`.
  - Define module-scoped error enums (`toc::Error`, `packet::Error`, `header::Error`, `silk::error::Error`) using `thiserror::Error` and convert them into `symphonia_core::errors::Error`.
- **Idiomatic Conversions & Representations**:
  - Use `TryFrom` and `From` (`TryFrom<u8>`, `TryFrom<&[u8]>`, `From<FrameDuration> for Duration`) for parsing and type conversions.
  - Use `#[repr(u32)]` / `#[repr(u64)]` enum discriminants when variants map directly to numeric constants (such as `Bandwidth` sample rates or `FrameDuration` nanoseconds).
- **Codebooks & Constants**:
  - Store RFC lookup tables, codebooks, and inverse CDFs (`ICDF_*`) in `symphonia-codec-opus/src/silk/constant.rs` (organized under domain submodules `header`, `gain`, `lsf`, `ltp`, `excitation`, `resampler` and re-exported at the `constant` root).
  - Use fixed-size arrays (`[[u8; 10]; 32]`, `[u32; N]`) rather than slice references (`&[u32]`), and keep `ICDF_*` arrays in the format `[total, cum_0, cum_1, ..., total]` (`total = 256`) expected by `RangeDecoder::decode_symbol_with_icdf`.
- **Workspace Membership**:
  - `symphonia-codec-opus` is a member of the root Cargo workspace (`Cargo.toml`). Never add a `[workspace]` table to a member crate.

## 5. Testing approach and verification gate

Testing in `symphonia-codec-opus` follows a five-layer strategy from static table invariants up to end-to-end RFC conformance:

### 5.1 Core testing rules

- **Reproduce before fixing**: Start every bug fix from a failing unit test or reproducing input whose failure was observed before modifying production code.
- **Never weaken expectations**: Never edit a test assertion or expected vector merely to make a failing test pass. Update an expectation only when proven incorrect against RFC 6716, RFC 7845, or RFC 8251, and cite the RFC section in the test or commit.
- **Keep tests self-contained and deterministic**: Place module unit tests in `#[cfg(test)] mod tests` at the bottom of the owning file (with nested submodules such as `mod id` and `mod comment` when grouping related suites). Avoid external network or filesystem state in unit tests.

### 5.2 Five testing layers

| Layer | Scope | Required Pattern in `symphonia-codec-opus` |
| :--- | :--- | :--- |
| **1. Table & Constant Invariants** | `silk/constant.rs`, `celt/` tables | Verify structural and mathematical invariants on all static RFC tables: iCDF format (`icdf[0] == 256`, `icdf.last() == Some(&256)`, strict cumulative monotonicity), codebook selector bounds (`0..16` for LSF Stage-2, `0..2` for prediction weights), permutation validity (LSF polynomial evaluation orderings), and symmetry/monotonicity of trigonometric lookup tables (Q12 cosine table). |
| **2. Bitstream & Header Parsers** | `toc.rs`, `packet.rs`, `header.rs` | Use **table-driven exhaustive tests** (`TestCase` arrays covering all 32 TOC configs, bandwidths, frame durations, and code modes `0..=3`) and **fluent test builders** (`PacketBuilder`, `IdHeaderBuilder`, `CommentHeaderBuilder`) to construct valid and malformed binary inputs cleanly without opaque raw byte slices. |
| **3. Boundary & Malformed Input Checks** | All parsers and decoders | Explicitly test every error variant in the module's `Error` enum: empty/truncated slices (`PacketTooShort`, `UnexpectedEof`), frame length overflow (`> 1275` bytes), multi-byte padding (`255` -> `254` bytes), total packet duration boundary (`<= 120_000 us` across `2.5 ms`..`60 ms` frames, `<= 48` frames), zero-length DTX frames, invalid magic signatures, unsupported versions, and invalid channel mappings. |
| **4. Isolated Entropy & DSP Stage Tests** | `entropy.rs`, `silk/decoder.rs`, `celt/` | 1) In `entropy.rs`, test range decoder initialization, normalization (`rng`, `val`), `decode_symbol_with_icdf`, `decode_symbol_logp`, raw bits (RTL), uniform uint, Laplace, and `tell`/`tell_frac` against known bitstreams.<br>2) In `silk/decoder.rs` and `celt/`, exploit the `RangeDecoder` trait by passing a **deterministic mock/scripted `RangeDecoder`** (returning queued symbols and asserting the exact iCDF table passed at each step) to test parameter decoding (`decode_lsf_stage2`, `decode_gains`, `decode_pitch_lags`, `decode_ltp`, `decode_excitation`) and DSP synthesis (`reconstruct_nlsf`, `stabilize_nlsf`, `lsf_to_lpc`, `ltp_synthesis`, `lpc_synthesis`, stereo unmixing, resampling) in isolation. |
| **5. End-to-End Decoder & RFC Test Vectors** | `decoder.rs`, integration tests | Verify `OpusDecoder` lifecycle (`try_new`, `decode`, `reset`, `finalize`, `last_decoded`) and validate full SILK, CELT, and Hybrid packet decoding against the official IETF RFC 6716 / RFC 8251 test vectors (`testvector01`..`testvector12`) using bit-exact internal parameter checks and the RFC 6716 Section 6.1 `opus_compare` quality metric. |

### 5.3 Mandatory verification gate

Before reporting any Rust change complete or closing an issue, execute and pass all three commands:

```bash
cargo test -p symphonia-codec-opus -- --test-threads=1
cargo clippy -p symphonia-codec-opus --all-targets
cargo doc -p symphonia-codec-opus --no-deps
```

- All unit and doc tests must pass (`0 failed`).
- `cargo clippy` must introduce zero new warnings.
- `cargo doc -p symphonia-codec-opus --no-deps` must complete with `0 warnings`.

## 6. Git, branch organization, and authorization

- **Remotes & Branches**:
  - `origin`: `https://github.com/jiztastamablastamarang/symphonia.git` (user fork)
  - `upstream`: `https://github.com/pdeljanov/Symphonia.git` (parent repository)
  - `main`: tracks `upstream/main` 1:1. Never commit Opus work directly to `main`.
  - `opus-decoder`: active development branch rebased linearly onto `upstream/main`.
  - Do not open or update a Pull Request against `pdeljanov/Symphonia` until all Opus decoder phases (`#3`-`#10`) are complete and verified.
  - Remove `AGENTS.md` from the branch (`git rm AGENTS.md` or drop its commit during rebase) as part of final pre-PR cleanup before submitting to `pdeljanov/Symphonia`.
- **Staging & Commits**:
  - Commit only when requested by the user.
  - Never commit with `git add -A` or broad snapshot commands; stage explicit file paths (`git add <path>`) and inspect `git diff --cached --stat` before committing.
  - Keep separate logical fixes or issues in separate commits when requested.
  - Never commit credentials, tokens, editor-generated changelogs (`CHANGELOG.md`), or temporary scratch files (`help.txt`) to tracked paths. Store disposable scripts or logs in untracked scratch locations only.
