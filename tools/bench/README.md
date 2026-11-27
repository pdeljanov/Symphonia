# Benchmarks against ffmpeg

Compares the decoding time of `symphonia-play --decode-only` with single threaded `ffmpeg -f null` for one file per codec and container.

```
cargo build --release -p symphonia-play
tools/bench/gen.sh SOURCE_AUDIO [OUT_DIR] [SECONDS]   # encodes the source into every supported format (default: tmp/bench, 300 s)
tools/bench/run.sh [OUT_DIR] [RUNS]                   # runs hyperfine on every file and prints the table
```

Requires `ffmpeg` and `hyperfine`. The generated files are for local use only (keep them in the git-ignored `tmp/`). Set `BENCH_CPU` to pin the runs to another core and `SYMPHONIA_PLAY` to benchmark another binary.

A ratio below 1 means Symphonia is faster. ffmpeg also converts the sample format and runs the null muxer, so it is slightly penalised. Run `tools/corpus/run_check.sh` on the same files first to confirm that the whole file decodes correctly.
