#!/bin/bash
set -e

# Default duration: 10 seconds per target
DURATION=${1:-10}

TARGETS=(
    "decode_any"
    "decode_mpa"
    "decode_flac"
    "decode_aac"
    "decode_vorbis"
    "decode_pcm"
    "decode_adpcm"
    "decode_alac"
)

echo "Running each fuzzer for $DURATION seconds..."

for target in "${TARGETS[@]}"; do
    echo "----------------------------------------------------------------"
    echo "Fuzzing target: $target"
    echo "----------------------------------------------------------------"
    # -max_total_time is a libFuzzer flag passed via cargo fuzz
    cargo fuzz run "$target" -- -max_total_time="$DURATION"
done

echo "----------------------------------------------------------------"
echo "All fuzzers finished running for $DURATION seconds each."
