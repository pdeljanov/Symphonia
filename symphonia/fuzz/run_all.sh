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
    "demux_mkv"
    "demux_isomp4"
    "demux_ogg"
    "demux_flac"
    "demux_wav"
    "demux_aiff"
    "demux_caf"
    "fmtdec_ape"
    "fmtdec_tta"
    "fmtdec_adx"
    "fmtdec_sbc"
    "fmtdec_dfpwm"
    "fmtdec_truehd"
    "fmtdec_ac3"
    "fmtdec_dca"
    "fmtdec_wavpack"
    "fmtdec_flv"
    "fmtdec_ts"
    "fmtdec_w64"
    "fmtdec_adts"
    "fmtdec_ogg"
    "fmtdec_wav"
    "fmtdec_isomp4"
    "fmtdec_mkv"
    "fmtdec_caf"
    "fmtdec_aiff"
    "fmtdec_flac"
    "fmtdec_mpa"
    "rawdec_opus"
    "rawdec_speex"
    "rawdec_g723"
    "rawdec_nellymoser"
    "rawdec_ac3"
    "rawdec_dca"
    "rawdec_sbc"
    "rawdec_dfpwm"
    "rawdec_truehd"
    "rawdec_adx"
    "rawdec_tta"
    "rawdec_ape"
    "rawdec_wavpack"
    "rawdec_swf_adpcm"
    "rawdec_he_aac"
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
