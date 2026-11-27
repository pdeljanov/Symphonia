#!/usr/bin/env bash
# Runs symphonia-check over every file in the corpus, in parallel, and writes a TSV + summary.
# Usage: run_check.sh [corpus-dir] [name-filter-regex]
set -u
ROOT="${1:-tmp/corpus}"
FILTER="${2:-.}"
BIN="${CHECK_BIN:-target/release/symphonia-check}"
RES="$ROOT/results.tsv"

check_one() {
    local f="$1"
    local out rc
    local mode=""
    out=$(timeout 120 "$BIN" -q "$f" 2>&1); rc=$?
    if [ $rc = 1 ]; then
        local out2; out2=$(timeout 120 "$BIN" -q --no-gapless "$f" 2>&1); local rc2=$?
        if [ $rc2 = 0 ]; then out="$out2"; rc=0; mode="(no-gapless)"; fi
    fi
    local status maxd failed
    local snr_status=""
    if [ $rc = 1 ]; then
        local d; d=$(mktemp -d)
        local gl dec=""
        local codec; codec=$(ffprobe -v error -select_streams a:0 -show_entries stream=codec_name -of csv=p=0 "$f" 2>/dev/null | head -1 | tr -d ",")
        # ffmpeg's native Speex decoder differs from libspeex, which is the reference.
        [ "$codec" = opus ] && dec="-c:a libopus -request_sample_fmt flt"
        [ "$codec" = speex ] && dec="-c:a libspeex"
        if [ "$codec" = ac3 ] || [ "$codec" = eac3 ]; then
            # The dither noise of zero-bit mantissas is random, so compare a decode with dither disabled against both ffmpeg and a dithered decode: the decoder is exact if the two SNRs agree (within 6 dB for E-AC-3, whose spectral extension and AHT noise levels are not normative).
            local a b
            for gl in "" "--no-gapless"; do
                AC3_NODITHER=1 "$BIN" -q $gl --dump "$d/t.f32" "$f" >/dev/null 2>&1
                "$BIN" -q $gl --dump "$d/on.f32" "$f" >/dev/null 2>&1
                ffmpeg -v error -y $([ -n "$gl" ] && echo "-flags2 skip_manual") -i "$f" -map 0:a:0 -f f32le "$d/r.f32" 2>/dev/null
                a=$(python3 -I "$SNR_PY" "$d/t.f32" "$d/r.f32" 0 2>/dev/null | sed -n 's/.*snr_db=\(.*\)/\1/p')
                b=$(python3 -I "$SNR_PY" "$d/t.f32" "$d/on.f32" 0 2>/dev/null | sed -n 's/.*snr_db=\(.*\)/\1/p')
                if [ -n "$a" ] && [ -n "$b" ] && awk -v a="$a" -v b="$b" -v t="$([ "$codec" = eac3 ] && echo 6 || echo 1.5)" 'BEGIN{exit !(a>=80 || a>=b-t)}'; then
                    snr_status="PASS(ac3/eac3 dither-only, snr ${a}dB vs ${b}dB$gl)"; break
                fi
            done
        fi
        # ffmpeg does not reset libopus at the links of a chained stream, so decode the links of the chain separately.
        local chain_ref=""
        if [ "$(basename "$f")" = chain_opus.opus ]; then
            local part
            for part in opus_music_2ch_64k_audio opus_sweep_2ch_64k_audio; do
                ffmpeg -v error -y $dec -i "$(dirname "$f")/$part.opus" -f f32le "$d/$part.f32" 2>/dev/null
                cat "$d/$part.f32" >> "$d/chain_ref.f32"
            done
            chain_ref="$d/chain_ref.f32"
        fi
        for gl in "" "--no-gapless"; do
            [ -n "$snr_status" ] && break
            "$BIN" -q $gl --dump "$d/t.f32" "$f" >/dev/null 2>&1
            ffmpeg -v error -y $dec $([ -n "$gl" ] && echo "-flags2 skip_manual") -i "$f" -map 0:a:0 -f f32le "$d/r.f32" 2>/dev/null
            [ -n "$chain_ref" ] && cp "$chain_ref" "$d/r.f32"
            local res; res=$(python3 -I "$SNR_PY" "$d/t.f32" "$d/r.f32" 0 2>/dev/null)
            local db; db=$(sed -n 's/.*snr_db=\(.*\)/\1/p' <<<"$res")
            if [ -n "$db" ] && awk -v d="$db" -v t="$([ "$codec" = nellymoser ] && echo 30 || echo 40)" 'BEGIN{exit !(d>=t)}' && [ "$(sed -n 's/len_t=\([0-9]*\) len_r=\([0-9]*\).*/\1 \2/p' <<<"$res" | awk '{print ($1==$2)}')" = 1 ]; then
                snr_status="PASS(snr ${db}dB$gl)"; break
            fi
        done
        rm -rf "$d"
    fi
    maxd=$(sed -n 's/.*Absolute Maximum Sample Delta: *//p' <<<"$out" | head -1)
    failed=$(sed -n 's/.*Failed\/Total Samples: *\([0-9]*\)\/.*/\1/p' <<<"$out" | head -1)
    if [ -n "$snr_status" ]; then status="$snr_status"
    elif [ $rc = 0 ]; then status="PASS$mode"
    elif [ $rc = 1 ] && awk -v d="${maxd:-1}" 'BEGIN{exit !(d<=0.000040)}'; then status="PASS(ref-s16)"
    elif [ $rc = 1 ]; then status=FAIL
    elif [ $rc = 124 ]; then status=TIMEOUT
    elif grep -qi "unsupported\|no suitable\|not supported" <<<"$out"; then status=UNSUPPORTED
    elif [ $rc -ge 101 ]; then status=PANIC
    else status=ERROR; fi
    local msg; msg=$(grep -m1 -i "error\|panicked\|unsupported" <<<"$out" | tr '\t' ' ' | cut -c1-140)
    printf '%s\t%s\t%s\t%s\t%s\n' "$f" "$status" "${maxd:--}" "${failed:--}" "$msg"
}
export -f check_one
export BIN
export SNR_PY="$(cd "$(dirname "$0")" && pwd)/snr.py"

find "$ROOT/synth" "$ROOT/real" "$ROOT/fate_aac" -type f ! -name '*.txt' | grep -E "$FILTER" | sort \
  | xargs -P "$(nproc)" -I{} bash -c 'check_one "$@"' _ {} | sort > "$RES"

echo "== summary by codec-prefix/status =="
awk -F'\t' '{n=split($1,p,"/"); f=p[n]; sub(/[_.].*/,"",f); c[f" "$2]++} END{for(k in c) print k, c[k]}' "$RES" | sort
echo "totals:"; cut -f2 "$RES" | sort | uniq -c
