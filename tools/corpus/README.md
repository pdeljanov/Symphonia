# Conformance corpus tools

Scripts to generate a large corpus of test files with `ffmpeg`, and to verify Symphonia's decoders and demuxers against it. The corpus is written to `tmp/corpus` (git-ignored) by default.

| Script | Purpose |
|--------|---------|
| `fetch_real.sh [dir]` | Downloads real-world samples (HE-AAC, WavPack, APE, TTA, DTS and DTS in WAV, AC-3, E-AC-3, AAC conformance streams) from samples.ffmpeg.org and fate-suite.ffmpeg.org. |
| `gen_synthetic.sh [dir]` | Encodes music and synthetic signals (sweep, noise, tones, silence, clipping) to ~1000 files covering codecs (AC-3, DTS, Speex, ADX, ...), containers (FLV, Wave64, ...), channel counts, sample rates, bit depths, and encoder options. |
| `run_check.sh [dir] [regex]` | Runs `symphonia-check` on every file and writes `results.tsv`. A file passes if it matches `ffmpeg` within `1e-5`, or, for lossy codecs, if the SNR against the reference is at least 40 dB (`snr.py`). |
| `seek_check.sh [regex]` | Seeks to 25%, 50%, and 90% of every file, and compares the audio decoded after the seek with a full `ffmpeg` decode (`seek_cmp.py`). |
| `corrupt_check.py [regex] [variants]` | Decodes truncated and bit-flipped variants of the corpus with `symphonia-play` and reports panics, aborts, and hangs. Use `PLAY_BIN=target/debug/symphonia-play` to also catch arithmetic overflows. |
| `snr.py`, `seek_cmp.py` | Helpers that compare raw `f32le` dumps (`symphonia-check --dump`). |

Typical use:

```bash
cargo build --release -p symphonia-check -p symphonia-play
tools/corpus/fetch_real.sh
tools/corpus/gen_synthetic.sh
tools/corpus/run_check.sh
tools/corpus/seek_check.sh 'music_2ch|luckynight'
python3 tools/corpus/corrupt_check.py . 6
```

`symphonia-check` has a few options that help when a file fails:

* `--ref-wav <file>` compares against a pre-decoded WAVE file instead of `ffmpeg`.
* `--tolerance <delta>` sets the absolute maximum allowed sample difference.
* `--dump <file>` writes the decoded audio as interleaved `f32le` samples.
* `--seek <seconds>` (with `--dump`) seeks first, then dumps the audio from the seeked-to frame.

## Known differences from `ffmpeg`

* `ffmpeg` decodes MP2 to 16-bit samples, so MP2 differs by 1 LSB.
* `ffmpeg`'s native Opus decoder is not bit-exact with `libopus`. The scripts use `libopus` as the reference for Opus (with `-request_sample_fmt flt`, because its default 16-bit output quantizes quiet passages and caps the SNR of the whole file), and require an SNR of 40 dB.
* `ffmpeg`'s G.726 decoder differs from the ITU-T reference (for 40 kbit/s it adapts the zero-predictor coefficients with the wrong shift), so G.726 files only agree to an SNR of about 30 dB.
* AC-3 replaces zero-bit mantissas with random dither noise, which differs between decoders. `run_check.sh` therefore decodes AC-3 with the `AC3_NODITHER` environment variable set, and passes a file if the SNR against `ffmpeg` is within 1.5 dB of the SNR against Symphonia's own dithered decode (that is, the dither noise explains every difference). Seek checks of AC-3 need `AC3_NODITHER=1` too, and the SNR is limited to that of the full decode.
* `ffmpeg`'s DTS decoder deviates from the specification when the input is at full scale (a clipped sine encoded at full scale differs by up to 0.34, while Symphonia is 70 dB closer to the source), so the DTS clip signal is encoded at 0.6 of full scale.
* MPEG-1 layer II does not allow 320 kbit/s in single channel mode, so the mono MP2 files stop at 192 kbit/s (ffmpeg writes and reads such files, Symphonia rejects them).
* Nellymoser replaces coefficients without bits by pseudo-random noise, which differs between decoders, so Nellymoser files only need an SNR of 30 dB.
* AAC with PNS (perceptual noise substitution) depends on the state of the noise generator, which differs after a seek.
