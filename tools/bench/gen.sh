#!/usr/bin/env bash
# Encodes a long test signal into every format that ffmpeg can write, for tools/bench/run.sh.
# Usage: gen.sh SOURCE_AUDIO [OUT_DIR] [SECONDS]   (the files are for local use only, do not commit them)
set -u
SRC="${1:?usage: gen.sh SOURCE_AUDIO [OUT_DIR] [SECONDS]}"
OUT="${2:-tmp/bench}"
SECS="${3:-300}"
mkdir -p "$OUT"
FF="ffmpeg -v error -y -nostdin"

wav() { [ -s "$OUT/_$1.wav" ] || $FF -stream_loop -1 -i "$SRC" -t "$SECS" -ac "$2" -ar "$3" -c:a pcm_s16le "$OUT/_$1.wav"; }
wav st44 2 44100; wav st48 2 48000; wav m16 1 16000; wav m8 1 8000; wav m22 1 22050; wav st32 2 32000

enc() { # name source-wav ffmpeg-args...
    local name="$1" src="$2"; shift 2
    [ -s "$OUT/$name" ] || $FF -i "$OUT/_$src.wav" "$@" "$OUT/$name" 2>/dev/null || { rm -f "$OUT/$name"; echo "skip $name"; }
}
enc pcm_s16.wav st44 -c:a pcm_s16le
enc pcm_f32.wav st44 -c:a pcm_f32le
enc pcm_s24.aiff st44 -c:a pcm_s24be
enc pcm_alaw.wav m8 -c:a pcm_alaw
enc flac.flac st44 -c:a flac
enc alac.m4a st44 -c:a alac
enc wavpack.wv st44 -c:a wavpack
enc wavpack_lossy.wv st44 -c:a wavpack -b:a 192k
enc tta.tta st44 -c:a tta
enc mp3.mp3 st44 -c:a libmp3lame -b:a 192k
enc mp2.mp2 st44 -c:a mp2 -b:a 192k
enc vorbis.ogg st44 -c:a libvorbis -q:a 5
enc opus.ogg st48 -c:a libopus -b:a 128k
enc opus_silk.ogg m16 -c:a libopus -b:a 24k -application voip
enc speex_wb.ogg m16 -c:a libspeex -q:a 6
enc aac.m4a st44 -c:a aac -b:a 192k
enc aac.aac st44 -c:a aac -b:a 192k
enc aac.ts st44 -c:a aac -b:a 192k -f mpegts
enc aac.flv st44 -c:a aac -b:a 192k -f flv
enc ac3.ac3 st48 -c:a ac3 -b:a 448k
enc eac3.eac3 st48 -c:a eac3 -b:a 224k
enc dts.dts st48 -c:a dca -strict -2 -b:a 768k
enc truehd.thd st48 -c:a truehd -strict -2
enc mlp.mlp st48 -c:a mlp -strict -2
enc sbc.sbc st44 -c:a sbc
enc adx.adx st44 -c:a adpcm_adx
enc adpcm_ima.wav st44 -c:a adpcm_ima_wav
enc adpcm_ms.wav st44 -c:a adpcm_ms
enc adpcm_yamaha.wav st44 -c:a adpcm_yamaha
enc g722.wav m16 -c:a g722
enc g726.wav m8 -c:a g726 -b:a 32k
enc g723.wav m8 -c:a g723_1
enc nellymoser.flv m22 -c:a nellymoser
enc pcm.w64 st44 -c:a pcm_s16le -f w64
enc opus.mka st48 -c:a libopus -b:a 128k
enc flac.mkv st44 -c:a flac
ls "$OUT" | grep -v '^_' | wc -l
