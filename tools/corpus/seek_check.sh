#!/usr/bin/env bash
# Seeks to several positions in each file, and compares the decoded audio after the seek to the audio of a full ffmpeg decode.
# Usage: seek_check.sh [name-regex]
set -u
ROOT="${CORPUS:-tmp/corpus}"
BIN="${CHECK_BIN:-target/release/symphonia-check}"
FILTER="${1:-.}"
HERE="$(cd "$(dirname "$0")" && pwd)"
D=$(mktemp -d)
printf '%-48s %-6s %s\n' file seek result
for f in $(find "$ROOT/synth" "$ROOT/real" "$ROOT/fate_aac" -type f ! -name '*.txt' | grep -E "$FILTER" | sort); do
    dur=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$f" 2>/dev/null)
    [ -n "$dur" ] && [ "$dur" != "N/A" ] || continue
    codec=$(ffprobe -v error -select_streams a:0 -show_entries stream=codec_name -of csv=p=0 "$f" 2>/dev/null)
    dec=""; [ "$codec" = opus ] && dec="-c:a libopus -request_sample_fmt flt"; [ "$codec" = speex ] && dec="-c:a libspeex"
    ffmpeg -v error -y $dec -i "$f" -map 0:a:0 -f f32le "$D/ref.f32" 2>/dev/null || continue
    for pct in 25 50 90; do
        t=$(awk -v d="$dur" -v p="$pct" 'BEGIN{printf "%.3f", d*p/100}')
        out=$("$BIN" --seek "$t" --dump "$D/t.f32" "$f" 2>&1)
        if [ $? -ne 0 ]; then printf '%-48s %-6s %s\n' "$(basename "$f")" "$t" "ERROR: $(echo "$out" | tail -1 | cut -c1-80)"; continue; fi
        start=$(sed -n 's/.*required_ts=\([0-9]*\).*/\1/p' <<<"$out"); ch=$(sed -n 's/.*channels=\([0-9]*\).*/\1/p' <<<"$out")
        res=$(python3 -I "$HERE/seek_cmp.py" "$D/t.f32" "$D/ref.f32" "$start" "$ch")
        printf '%-48s %-6s %s\n' "$(basename "$f")" "$t" "$res"
    done
done
rm -rf "$D"
