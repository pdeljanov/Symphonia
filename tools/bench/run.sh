#!/usr/bin/env bash
# Compares the decoding time of symphonia-play (--decode-only) and single-threaded ffmpeg on every file of a directory.
# Usage: run.sh [DIR] [RUNS]   Results: DIR/results.json (hyperfine) and a table from summarize.py
set -u
DIR="${1:-tmp/bench}"
RUNS="${2:-5}"
SYM="${SYMPHONIA_PLAY:-target/release/symphonia-play}"
CPU="${BENCH_CPU:-3}"
HERE="$(cd "$(dirname "$0")" && pwd)"
rm -f "$DIR"/res_*.json "$DIR"/failed.txt
for f in "$DIR"/*; do
    b=$(basename "$f")
    case "$b" in _*|res_*|results*|failed*) continue;; esac
    if ! taskset -c "$CPU" "$SYM" --decode-only --no-progress "$f" >/dev/null 2>&1; then echo "$b" >> "$DIR/failed.txt"; continue; fi
    taskset -c "$CPU" hyperfine -N --warmup 1 --runs "$RUNS" --export-json "$DIR/res_$b.json" \
        -n "ffmpeg" "ffmpeg -threads 1 -benchmark -v 0 -i $f -f null -" \
        -n "symphonia" "$SYM --decode-only --no-progress $f" \
        -n "ffmpeg-demux" "ffmpeg -threads 1 -v 0 -i $f -c copy -f null -" \
        -n "symphonia-probe" "$SYM --probe-only --no-progress $f" >/dev/null 2>&1 || echo "$b (hyperfine)" >> "$DIR/failed.txt"
done
python3 "$HERE/summarize.py" "$DIR"
