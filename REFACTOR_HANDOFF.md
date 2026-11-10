# `symphonia-codec-opus`: RFC 8251 follow-up handoff

Brief for the next piece of work on branch `opus-decoder` of `jiztastamablastamarang/symphonia`.
It is self-contained: every external input below has a URL, a SHA-1 and a command.

## 1. Goal

Close the remaining RFC 8251 gaps of the Opus decoder and calibrate its conformance figures
against the RFC 6716 Appendix A reference decoder with the RFC 8251 patch:

1. Implement RFC 8251 Section 10 for mono output (Step 1).
2. Pin every RFC 8251 decoder change with a targeted test (Step 2).
3. Build the reference decoder, record its own conformance scores and compare outputs sample
   by sample (Step 3).
4. Fix each deviation that Step 3 finds (Step 4).

## 2. State of the branch

- `opus-decoder` at `9612e40` holds the finished refactor of the crate; issues #13 to #24
  tracked it and are closed.
- The branch backs the upstream pull request pdeljanov/Symphonia#398 (base `dev-0.6`), so every
  push to `opus-decoder` updates that pull request.
- Only `OpusDecoder` is public; every other item is `pub(crate)` or private, so the compiler
  reports dead code.
- Gate: 130 unit tests, clippy with warnings denied and rustdoc with private items are clean.
- Conformance: `tests/rfc6716_conformance.rs` passes all 120 runs (figures in Section 6).
- The unit-test fixtures hold values recorded from libopus 1.6.1; each test module states whether
  they pin normative behaviour (verified by the conformance test) or non-normative concealment
  (regression pins only).

## 3. Rules

- `AGENTS.md` at the repository root governs this crate: code style, testing layers and the
  verification gate. Read it first.
- Source of truth: RFC 6716 (prose plus the normative Appendix A reference decoder) as updated by
  RFC 8251, and RFC 7845. libopus mainline is not an authority. Cite RFC sections in code and tests.
- Every behaviour change starts from a test observed failing before the fix.
- One commit per step. Subject in the upstream style `opus: Sentence.` (`chore:` for development
  documents); the body says what changed and why, with RFC references. No AI attribution, no
  dates, no em or en dashes (U+2014, U+2013) anywhere.
- Stage named paths only (never `git add -A`) and inspect `git diff --cached --stat` before each
  commit. Never force-push.
- GitHub issues and comments only when the user asks.
- Downloads and reference builds stay outside the repository.

## 4. Setup

### 4.1 Test vectors (RFC 8251 Section 11)

```sh
mkdir -p ~/opus-ref && cd ~/opus-ref
curl -LO https://www.rfc-editor.org/rfc/rfc8251.txt
curl -LO https://www.ietf.org/proceedings/98/slides/materials-98-codec-opus-newvectors-00.tar.gz
tar xzf materials-98-codec-opus-newvectors-00.tar.gz      # creates opus_newvectors/
grep -E '^   [0-9a-f]{40}  testvector[0-9]{2}m?\.(bit|dec)$' rfc8251.txt \
  | sed 's/^   //' > opus_newvectors/expected.sha1
(cd opus_newvectors && sha1sum -c expected.sha1)          # 36 files, all OK
```

### 4.2 Reference decoder (RFC 6716 Appendix A with the RFC 8251 patch)

```sh
cd ~/opus-ref
curl -LO https://www.rfc-editor.org/rfc/rfc6716.txt
cat rfc6716.txt | grep '^\ \ \ ###' | sed -e 's/...###//' | base64 --decode > opus-rfc6716.tar.gz
sha1sum opus-rfc6716.tar.gz       # 86a927223e73d2476646a1b933fcd3fffb6ecc8c (RFC 6716 Appendix A.1)
curl -LO https://www.ietf.org/proceedings/98/slides/materials-98-codec-opus-update-00.patch
sha1sum materials-98-codec-opus-update-00.patch   # 029e3aa88fc342c91e67a21e7bfbc9458661cd5f (RFC 8251 Section 1)
tar xzf opus-rfc6716.tar.gz && cd opus-rfc6716
patch -p1 < ../materials-98-codec-opus-update-00.patch
make                              # float build (the Makefile default): opus_demo, opus_compare
sh run_vectors.sh . ../opus_newvectors 48000      # official check, one output rate per run
```

The patch changes exactly the code of RFC 8251 Sections 3 to 9 plus `run_vectors.sh`; it does
not implement Section 10. If the environment refuses to download or build external code, stop and
ask the user instead of working around it.

### 4.3 Crate conformance test

```sh
OPUS_TESTVECTORS_DIR=~/opus-ref/opus_newvectors cargo test --release -p symphonia-codec-opus \
  --test rfc6716_conformance -- --ignored --nocapture
```

About 90 seconds in release mode. It prints one line per run: packets, final range checks (the
first packet and packets next to a loss are not checked, as in `opus_demo`) and quality against
`.dec` and `m.dec`. A run passes when the final range matches on every checked packet and either
reference set scores at least 0 (RFC 8251 Section 11 accepts either set).

## 5. RFC 8251 audit of the current code

| RFC 8251 | Decoder change | Where in the crate | Targeted test |
| :--- | :--- | :--- | :--- |
| §3 Stereo state reset in SILK | stereo state and `prev_decode_only_middle` cleared on init | `silk::Decoder::new` and `reset` (`silk/decoder.rs`), `StereoState::new` and `reset_side` (`silk/mod.rs`) | none |
| §4 Padding parsing | padding subtracted from the remaining length while it is read | `take_padding_length` (`packet.rs`) | `code3_padding_flag_without_a_complete_padding_length_is_rejected`, `packet_with_extended_padding` |
| §5 Resampler buffer | buffer sizes and a safe in-place copy | `resample` (`silk/resampler.rs`): fixed arrays and bounds-checked slices rule out the C overflow | `resampler_rejects_unsupported_rates_and_short_buffers` |
| §6 Inverse gain wrap-around | saturating subtraction, 64-bit product, out of range means unstable | `ReflectionStep::remove_reflection` inside `inverse_prediction_gain_q30` (`silk/lpc.rs`) | none |
| §7 NLSF stabilization wrap-around | saturating addition when enforcing the minimum spacing | `enforce_spacing_by_sorting` (`silk/lsf.rs`) | `nlsf_stabilization_saturates_at_the_q15_ceiling` |
| §8 Band energy cap | log band energy clamped to 32 before exponentiation | `denormalise_bands` (`celt/synthesis.rs`) | none |
| §9 Hybrid folding | first Hybrid band duplicated as folding source, fold end bounded by the current band | `fold_first_hybrid_band` and `select_lowband` (`celt/vq.rs`) | `hybrid_folding_and_recursive_split_match_recorded_values` |
| §10 Downmix to mono | the decoder MAY skip the intensity stereo 180-degree phase shift | missing: the never-set `disable_inv` flag was deleted as dead code during the refactor | none (Step 1) |
| §11 New test vectors | `.dec` and `m.dec` reference sets | `tests/rfc6716_conformance.rs` | the conformance test |

## 6. Baseline figures (`9612e40`)

Quality at 48 kHz (`opus_compare` against each reference set). The content column is a census of
the TOC bytes of each `.bit` file.

| Vector | Content | Mono `.dec` | Mono `m.dec` | Stereo `.dec` | Stereo `m.dec` |
| :--- | :--- | ---: | ---: | ---: | ---: |
| 01 | CELT FB, all packets stereo | 98.4 | 39.2 | 99.9 | 66.3 |
| 02 | SILK NB, 49% stereo | 96.2 | 96.2 | 100.0 | 100.0 |
| 03 | SILK MB, 48% stereo | 96.7 | 96.7 | 100.0 | 100.0 |
| 04 | SILK WB, 49% stereo | 96.5 | 96.5 | 100.0 | 100.0 |
| 05 | Hybrid SWB, 49% stereo | 96.2 | -29.4 | 99.9 | 33.4 |
| 06 | Hybrid FB, 49% stereo | 96.5 | -28.2 | 99.8 | 45.0 |
| 07 | CELT NB to FB, 31 bandwidth switches, 49% stereo | 98.1 | -24.1 | 99.8 | -21.9 |
| 08 | CELT NB to FB, 29 bandwidth switches, 71% stereo | 96.5 | -46.7 | 99.8 | 13.2 |
| 09 | CELT NB to FB, 7 bandwidth switches, 52% stereo | 98.1 | 6.4 | 99.8 | 40.7 |
| 10 | CELT FB with 16% Hybrid, 16 mode switches, 51% stereo | 97.7 | 97.7 | 99.8 | 99.8 |
| 11 | CELT FB, all packets stereo | 98.4 | 93.9 | 99.8 | 97.0 |
| 12 | SILK NB to WB with 19% Hybrid SWB, all mono | 99.9 | 99.9 | 99.9 | 99.9 |

Range of `.dec` quality per output rate, mono and stereo together: 8 kHz 43.4 to 93.3, 12 kHz
65.9 to 96.0, 16 kHz 56.5 to 96.3, 24 kHz 68.5 to 98.1, 48 kHz 96.2 to 100.0. The lowest run at
every reduced rate is testvector12 mono. The final range is equal on every checked packet of all
120 runs.

Reading the figures:

- Stereo output matches the `.dec` set at 99.8 to 100.0; the SILK-only vectors are exact.
- Mono output matches `.dec` and not `m.dec` on every CELT and Hybrid vector: the decoder applies
  the intensity stereo phase shift and then downmixes, so the phase-inverted bands cancel. RFC 8251
  Section 10 describes exactly this artifact. Step 1 addresses it.
- Open question 1: SILK-only streams with stereo packets (vectors 02 to 04) decoded to mono score
  96.2 to 96.7 while stereo scores 100.0. SILK has no phase shift, so Section 10 does not explain
  the gap. Step 3 shows whether the reference decoder has it too.
- Open question 2: testvector12 at 8 kHz scores 43.4, the only run below the "as low as 50" that
  RFC 6716 Section 6.1 calls normal at reduced rates. The stream switches SILK bandwidth 9 times
  and between SILK and Hybrid 3 times. The resampler is non-normative (RFC 6716 Section 4.2.9), so
  this is a quality question, not a conformance failure.

## 7. Tasks

### Step 1: RFC 8251 Section 10, no phase shift for mono output

- Test first, in the `celt/vq.rs` tests: decode a stereo intensity band whose inversion bit is
  set, once with phase inversion on and once off. Both must leave the range decoder in the same
  state (`tell_frac` and final range); with it off the side shape must not be negated. The test
  must fail before the change.
- Change: `ShapeFrame` (`celt/vq.rs`) gains `phase_inversion: bool`, set in
  `celt::Decoder::decode_shapes` (`celt/decoder.rs`) to `config.output_channels == 2`.
  `decode_split_angle` still decodes the inversion bit; `quant_band_stereo` negates `y` only when
  `split.invert_side && phase_inversion`. Decoding the bit keeps the final range unchanged.
- Docs: rustdoc cites RFC 8251 Section 10; the README conformance row says that stereo output
  matches the `.dec` set and mono output the `m.dec` set.
- Done when: the gate is clean; the conformance test still has the final range equal everywhere,
  every mono run passes against `m.dec` with a quality at least its `.dec` figure in Section 6, and
  the stereo figures are unchanged.
- Commit: `opus: Skip the intensity stereo phase shift when decoding to mono.`

### Step 2: pin every RFC 8251 decoder change with a test

| RFC 8251 | Test | Module |
| :--- | :--- | :--- |
| §3 | after a stereo frame, `reset()` makes the next packet decode exactly as in a fresh decoder | `silk/decoder.rs` |
| §6 | a Q12 coefficient vector whose update overflows 32 bits makes `inverse_prediction_gain_q30` return 0 (unstable); find one with a throwaway search and commit it as a constant | `silk/lpc.rs` |
| §8 | a band log energy above 32 gives the finite gain `2^32` in `denormalise_bands` | `celt/synthesis.rs` |

- Each test is checked by mutation: deleting the RFC 8251 line it guards, in a throwaway copy,
  must make it fail.
- Commit: `opus: Pin the RFC 8251 decoder changes with tests.`

### Step 3: calibrate against the reference decoder

1. Decode every vector with the reference at 8, 12, 16, 24 and 48 kHz in mono and stereo, keep the
   outputs (about 200 MB, outside the repository) and score each one against both sets:

   ```sh
   cd ~/opus-ref/opus-rfc6716 && mkdir -p ../ref
   for rate in 8000 12000 16000 24000 48000; do
     for v in 01 02 03 04 05 06 07 08 09 10 11 12; do
       ./opus_demo -d $rate 1 ../opus_newvectors/testvector$v.bit ../ref/ref_${v}_${rate}_mono.pcm
       ./opus_demo -d $rate 2 ../opus_newvectors/testvector$v.bit ../ref/ref_${v}_${rate}_stereo.pcm
       for set in "" m; do
         ./opus_compare -r $rate ../opus_newvectors/testvector$v$set.dec ../ref/ref_${v}_${rate}_mono.pcm
         ./opus_compare -s -r $rate ../opus_newvectors/testvector$v$set.dec ../ref/ref_${v}_${rate}_stereo.pcm
       done
     done
   done 2>&1 | tee ../ref/reference_scores.log
   ```

2. Put the reference's own figures beside Section 6. A run where the crate scores clearly below the
   reference is a deviation; equal figures mean the gap is inherent to the comparison.
3. Add one ignored test to `tests/rfc6716_conformance.rs` that runs when `OPUS_REFERENCE_OUTPUT_DIR`
   points at the `ref_*.pcm` files: it decodes each run and compares it sample by sample with the
   reference output, reporting the share of identical samples and the largest difference. Reuse
   `parse_bit_records`, `decode_records` and `read_pcm16`; the crate's output is already rounded to
   16 bits like `opus_demo` (`lrintf`). Expected: SILK-only runs bit-exact (integer pipeline),
   CELT and Hybrid within float rounding.
- Commit: `opus: Compare decoded output with the RFC 8251 reference decoder.`

### Step 4: fix the deviations

Every confirmed deviation gets a failing test first, a fix at the owning code path and its own
commit. If the reference itself scores about 96 on SILK mono and about 43 on testvector12 at
8 kHz, nothing is left to fix; record that in the README conformance row.

## 8. Verification for every commit

```sh
rg -n "else if|\bix\b|\w*idx\w*" symphonia-codec-opus/src/
cargo fmt -p symphonia-codec-opus --check
cargo test -p symphonia-codec-opus -- --test-threads=1
cargo clippy -p symphonia-codec-opus --all-targets --no-deps -- -D warnings
cargo doc -p symphonia-codec-opus --no-deps --document-private-items
```

Run the conformance test (Section 4.3) after Steps 1 and 4 as well.

## 9. Out of scope unless the user asks

- Regenerating the libopus-recorded unit-test fixtures from the reference decoder.
- Pushing, GitHub issues and comments.
- Before review of PR #398 is requested: remove `AGENTS.md`, `walkthrough.md` and this document
  from the branch, then rerun the gate and the conformance test.
