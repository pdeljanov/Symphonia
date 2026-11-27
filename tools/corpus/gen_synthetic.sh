#!/usr/bin/env bash
# Generates a large matrix of encoded test files with ffmpeg into <out>/synth.
# Sources: real music excerpt (real/luckynight.flac if present) and lavfi synthetic signals.
set -u
ROOT="${1:-tmp/corpus}"
OUT="$ROOT/synth"
SRC="$ROOT/work"
mkdir -p "$OUT" "$SRC"
FF="ffmpeg -hide_banner -loglevel error -y"
DUR=6

# Source signals (float wav, so each encoder gets full precision).
mk_src() { # name channels rate
    local f="$SRC/$1_${2}ch_${3}.wav"
    [ -s "$f" ] && return
    if [ -s "$ROOT/real/luckynight.flac" ] && [ "$1" = music ]; then
        local n=$2 pan="" i
        for ((i=0;i<n;i++)); do pan+="|c$i=$(awk -v i=$i -v n=$n 'BEGIN{a=(i+1)/(n+1); printf "%.3f*c0+%.3f*c1", a, 1-a}')"; done
        $FF -ss 10 -t $DUR -i "$ROOT/real/luckynight.flac" -af "aresample=$3,pan=${n}c${pan}" -c:a pcm_f32le "$f"
    else
        local e
        case "$1" in
            sweep) e="aevalsrc='$(for ((i=0;i<$2;i++)); do printf '%s0.5*sin(2*PI*(100+%d*t+t*t*1500)*t)' "$([ $i -gt 0 ] && echo '|')" $((i*150)); done)':d=$DUR:s=$3" ;;
            noise) e="anoisesrc=d=$DUR:r=$3:c=pink:a=0.5" ;;
            tones) e="aevalsrc='$(for ((i=0;i<$2;i++)); do printf '%s0.3*sin(2*PI*%d*t)+0.2*sin(2*PI*%d*t)+0.1*sin(2*PI*%d*t)' "$([ $i -gt 0 ] && echo '|')" $((440+i*110)) $((1320+i*200)) $((5000+i*300)); done)':d=$DUR:s=$3" ;;
            silence) e="anullsrc=r=$3:cl=$([ $2 = 1 ] && echo mono || echo stereo):d=$DUR" ;;
            clip) e="aevalsrc='clip(2*sin(2*PI*300*t),-1,1)':d=$DUR:s=$3:c=${2}c" ;;
        esac
        $FF -f lavfi -i "$e" -ac "$2" -ar "$3" -c:a pcm_f32le "$f"
    fi
}

enc() { # outfile src_name ch rate encoder-args...
    local name="$1"; local out="$OUT/$1"; local sn="$2"; local ch="$3"; local rate="$4"; shift 4
    mk_src "$sn" "$ch" "$rate"
    [ -s "$out" ] && return
    $FF -i "$SRC/${sn}_${ch}ch_${rate}.wav" "$@" "$out" 2>/dev/null || { rm -f "$out"; echo "skip $name"; }
}

SIGNALS="music sweep noise tones silence clip"

# PCM / WAV / AIFF / CAF / W64 / RF64 matrix.
for sig in music sweep; do
for fmt in u8 s16le s24le s32le f32le f64le s16be s24be s32be f32be; do
    for ch in 1 2 6; do
        enc "pcm_${sig}_${fmt}_${ch}ch.$([[ $fmt == *be ]] && echo mov || echo wav)" $sig $ch 44100 -c:a pcm_$fmt
    done
done
for fmt in s8 s16be s24be s32be f32be f64be; do enc "pcm_${sig}_${fmt}_2ch.aiff" $sig 2 44100 -c:a pcm_$fmt -f aiff; done
for fmt in s16le s16be s24le s24be s32le f32le f64le; do enc "pcm_${sig}_${fmt}_2ch.caf" $sig 2 48000 -c:a pcm_$fmt; done
done
enc pcm_alaw_2ch.wav music 2 8000 -c:a pcm_alaw
enc pcm_mulaw_2ch.wav music 2 8000 -c:a pcm_mulaw
enc pcm_alaw_1ch.mkv music 1 8000 -c:a pcm_alaw
enc pcm_mulaw_1ch.mkv music 1 8000 -c:a pcm_mulaw
enc pcm_alaw.caf music 1 8000 -c:a pcm_alaw
enc pcm_mulaw.caf music 1 8000 -c:a pcm_mulaw
enc pcm_s24le_2ch.w64 music 2 96000 -c:a pcm_s24le -f w64
enc pcm_s16le_2ch.rf64 music 2 48000 -c:a pcm_s16le -f wav -rf64 always
for r in 8000 11025 22050 32000 44100 48000 88200 96000 192000; do enc "pcm_s16le_rate${r}.wav" tones 2 $r -c:a pcm_s16le; done

# ADPCM.
for codec in adpcm_ima_wav adpcm_ms adpcm_ima_qt adpcm_swf adpcm_yamaha adpcm_g722 g722 g726 g726le adpcm_ima_ws adpcm_ima_amv adpcm_ima_apm adpcm_ima_alp adpcm_ima_ssi adpcm_adx adpcm_argo; do
    for ch in 1 2; do
        case $codec in
            adpcm_ima_wav|adpcm_ms) container=wav ;;
            adpcm_ima_qt) container=mov ;;
            adpcm_swf) container=flv; [ $ch = 1 ] || continue ;;
            adpcm_yamaha) container=wav ;;
            g722) container=wav; ch=1 ;;
            g726|g726le) container=wav; [ $ch = 1 ] || continue ;;
            adpcm_adx) container=adx ;;
            *) container=wav ;;
        esac
        rate=44100; case $codec in g722) rate=16000 ;; g726*) rate=8000 ;; adpcm_swf) rate=44100 ;; esac
        for sig in music sweep; do
            enc "adpcm_${codec}_${sig}_${ch}ch.$container" $sig $ch $rate -c:a $codec
        done
    done
done
for rate in 8000 11025 22050 44100; do
    enc "adpcm_ima_wav_rate${rate}.wav" music 2 $rate -c:a adpcm_ima_wav
    enc "adpcm_ms_rate${rate}.wav" music 2 $rate -c:a adpcm_ms
done
for bs in 256 512 1024 2048; do
    enc "adpcm_ima_wav_bs${bs}.wav" music 2 44100 -c:a adpcm_ima_wav -block_size $bs
    enc "adpcm_ms_bs${bs}.wav" music 2 44100 -c:a adpcm_ms -block_size $bs
done

# FLAC / ALAC.
for sig in music noise silence clip; do
    for ch in 1 2 6; do
        for sf in s16 s32; do enc "flac_${sig}_${sf}_${ch}ch.flac" $sig $ch 44100 -c:a flac -sample_fmt $sf; done
        enc "alac_${sig}_s16_${ch}ch.m4a" $sig $ch 44100 -c:a alac -sample_fmt s16p
        enc "alac_${sig}_s32_${ch}ch.m4a" $sig $ch 44100 -c:a alac -sample_fmt s32p
    done
done
enc alac_music_s16_2ch.caf music 2 44100 -c:a alac -sample_fmt s16p

# WavPack.
for sig in music sweep noise silence; do
    for ch in 1 2 6; do
        for mode in "" "-compression_level 0" "-compression_level 1" "-compression_level 2" "-compression_level 3"; do
            tag=$(echo "$mode" | tr -d ' -' )
            enc "wv_${sig}_${ch}ch_${tag:-default}.wv" $sig $ch 44100 -c:a wavpack $mode
        done
    done
done
enc wv_music_2ch_s32.wv music 2 44100 -c:a wavpack -sample_fmt s32
enc wv_music_2ch_f32.wv music 2 44100 -c:a wavpack -sample_fmt flt
enc wv_music_2ch_u8.wv music 2 44100 -c:a wavpack -sample_fmt u8p
enc wv_music_2ch_hybrid.wv music 2 44100 -c:a wavpack -b:a 128k
enc wv_music_2ch.mka music 2 44100 -c:a wavpack
enc wv_music_2ch.mkv music 2 44100 -c:a wavpack

# TTA.
for ch in 1 2; do for sf in s16 s32; do enc "tta_music_${sf}_${ch}ch.tta" music $ch 44100 -c:a tta -sample_fmt $sf; done; done

# Opus.
for sig in music sweep noise tones silence; do
    for ch in 1 2 6; do
        for br in 16k 64k 128k 256k; do
            for app in audio voip lowdelay; do
                [ "$sig" = music ] || [ "$app" = audio ] || continue
                [ $ch -le 2 ] || [ "$app" = audio ] || continue
                [ "$br" = 64k ] || [ "$app" = audio ] || continue
                enc "opus_${sig}_${ch}ch_${br}_${app}.opus" $sig $ch 48000 -c:a libopus -b:a $br -application $app -mapping_family $([ $ch -gt 2 ] && echo 1 || echo 0)
            done
        done
    done
done
for fd in 2.5 5 10 20 40 60 80 120; do enc "opus_music_2ch_fd${fd}.opus" music 2 48000 -c:a libopus -frame_duration $fd; done
for vbr in on off constrained; do enc "opus_music_2ch_vbr_${vbr}.opus" music 2 48000 -c:a libopus -vbr $vbr; done
for r in 8000 12000 16000 24000 44100; do enc "opus_music_2ch_in${r}.opus" music 2 $r -c:a libopus; done
enc opus_music_2ch.mka music 2 48000 -c:a libopus
enc opus_music_2ch.webm music 2 48000 -c:a libopus
enc opus_music_2ch.mp4 music 2 48000 -c:a libopus
enc opus_music_2ch.ogg music 2 48000 -c:a libopus -f ogg

# Vorbis.
for sig in music sweep noise; do for ch in 1 2 6; do for q in -1 3 10; do
    enc "vorbis_${sig}_${ch}ch_q${q}.ogg" $sig $ch 44100 -c:a libvorbis -q:a $q
done; done; done
enc vorbis_music_2ch.mka music 2 44100 -c:a libvorbis
enc vorbis_music_2ch.webm music 2 44100 -c:a libvorbis

# MP1/MP2/MP3.
for sig in music sweep noise tones; do for ch in 1 2; do
    for br in 64k 128k 192k 320k; do [ "$ch" = 1 ] && [ "$br" = 320k ] && continue; enc "mp2_${sig}_${ch}ch_${br}.mp2" $sig $ch 44100 -c:a mp2 -b:a $br; done
    for r in 24000 32000 48000; do enc "mp2_${sig}_${ch}ch_rate${r}.mp2" $sig $ch $r -c:a mp2 -b:a 96k; done
done; done
enc mp2_music_2ch.mka music 2 44100 -c:a mp2
enc mp2_music_2ch.mp4 music 2 44100 -c:a mp2
enc mp2_music_2ch.wav music 2 44100 -c:a mp2
for sig in music sweep noise tones silence; do for ch in 1 2; do
    for q in 0 4 9; do enc "mp3_${sig}_${ch}ch_v${q}.mp3" $sig $ch 44100 -c:a libmp3lame -q:a $q; done
    for m in "-b:a 128k" "-b:a 320k" "-b:a 64k -joint_stereo 0" "-abr 1 -b:a 128k"; do
        tag=$(echo "$m" | tr -d ' -:'); enc "mp3_${sig}_${ch}ch_$tag.mp3" $sig $ch 44100 -c:a libmp3lame $m
    done
    for r in 8000 16000 22050 24000 32000 48000; do enc "mp3_${sig}_${ch}ch_rate${r}.mp3" $sig $ch $r -c:a libmp3lame -b:a 64k; done
done; done
enc mp3_music_2ch.mka music 2 44100 -c:a libmp3lame
enc mp3_music_2ch.mp4 music 2 44100 -c:a libmp3lame
enc mp3_music_2ch.wav music 2 44100 -c:a libmp3lame
enc mp3_music_2ch_shine.mp3 music 2 44100 -c:a libshine

# AAC (ffmpeg native encoder: LC only).
for sig in music sweep noise tones silence clip; do for ch in 1 2 3 4 5 6 8; do
    for br in 32k 128k 256k; do
        enc "aac_${sig}_${ch}ch_${br}.m4a" $sig $ch 44100 -c:a aac -b:a $((ch * ${br%k}))k
    done
    enc "aac_${sig}_${ch}ch.adts.aac" $sig $ch 44100 -c:a aac -f adts
done; done
for r in 8000 11025 16000 22050 24000 32000 44100 48000 64000 88200 96000; do enc "aac_music_2ch_rate${r}.m4a" music 2 $r -c:a aac; done
for prof in aac_low aac_main aac_ltp aac_ssr; do enc "aac_music_2ch_${prof}.m4a" music 2 44100 -c:a aac -profile:a $prof; done
enc aac_music_2ch_pns.m4a music 2 44100 -c:a aac -aac_pns 1
enc aac_music_2ch_tns.m4a music 2 44100 -c:a aac -aac_tns 1
enc aac_music_2ch_is.m4a music 2 44100 -c:a aac -aac_is 1
enc aac_music_2ch_ms0.m4a music 2 44100 -c:a aac -aac_ms 0
enc aac_music_2ch_twoloop.m4a music 2 44100 -c:a aac -aac_coder twoloop
enc aac_music_2ch_anmr.m4a music 2 44100 -c:a aac -aac_coder anmr
enc aac_music_2ch.mka music 2 44100 -c:a aac
enc aac_music_2ch.mov music 2 44100 -c:a aac
enc aac_music_2ch.3gp music 2 44100 -c:a aac
enc aac_music_2ch.ts music 2 44100 -c:a aac
enc aac_music_2ch.latm music 2 44100 -c:a aac -f latm

# AC-3 / E-AC-3 / DTS.
for ch in 1 2 6; do for br in 96k 192k 448k; do
    enc "ac3_music_${ch}ch_${br}.ac3" music $ch 48000 -c:a ac3 -b:a $br
    enc "eac3_music_${ch}ch_${br}.eac3" music $ch 48000 -c:a eac3 -b:a $br
done
for br in 256k 768k 1509k; do enc "dts_music_${ch}ch_${br}.dts" music $ch 48000 -c:a dca -strict -2 -b:a $br; done
done
# MPEG transport streams.
enc ts_aac_1ch.ts music 1 22050 -c:a aac -f mpegts
enc ts_aac_6ch.ts music 6 48000 -c:a aac -f mpegts
enc ts_mp3_2ch.ts music 2 44100 -c:a libmp3lame -f mpegts
enc ts_mp2_2ch.ts music 2 48000 -c:a mp2 -f mpegts
enc ts_ac3_2ch.ts music 2 48000 -c:a ac3 -f mpegts
enc ts_ac3_6ch.ts music 6 48000 -c:a ac3 -f mpegts
enc ts_dts_6ch.ts music 6 48000 -c:a dca -strict -2 -f mpegts
# Flash Video.
enc flv_aac_2ch.flv music 2 44100 -c:a aac -f flv
enc flv_aac_1ch.flv music 1 22050 -c:a aac -f flv
enc flv_mp3_2ch.flv music 2 44100 -c:a libmp3lame -f flv
enc flv_mp3_1ch.flv music 1 22050 -c:a libmp3lame -f flv
enc flv_pcm_1ch.flv music 1 22050 -c:a pcm_s16le -f flv
enc flv_adpcm_2ch.flv music 2 44100 -c:a adpcm_swf -f flv
enc flv_adpcm_1ch_22k.flv music 1 22050 -c:a adpcm_swf -f flv
enc flv_alaw_1ch.flv music 1 11025 -c:a pcm_alaw -f flv
enc flv_mulaw_1ch.flv music 1 11025 -c:a pcm_mulaw -f flv
# Wave64.
enc w64_s16_1ch.w64 music 1 44100 -c:a pcm_s16le
enc w64_s16_2ch.w64 music 2 44100 -c:a pcm_s16le
enc w64_f32_2ch.w64 music 2 48000 -c:a pcm_f32le
enc w64_s24_6ch.w64 music 6 48000 -c:a pcm_s24le
enc w64_alaw_1ch.w64 music 1 8000 -c:a pcm_alaw
enc w64_u8_2ch.w64 music 2 22050 -c:a pcm_u8
# CRI ADX at other sampling frequencies (the cutoff coefficients depend on it),.
for rate in 22050 32000 48000; do enc "adx_music_2ch_${rate}.adx" music 2 $rate -c:a adpcm_adx; done
# DTS signals, sample rates, and containers.
for sig in sweep noise tones silence; do enc "dts_${sig}_2ch_768k.dts" $sig 2 48000 -c:a dca -strict -2 -b:a 768k; done
enc dts_clip_2ch_768k.dts clip 2 48000 -af volume=0.6 -c:a dca -strict -2 -b:a 768k
for rate in 32000 44100; do enc "dts_music_2ch_${rate}.dts" music 2 $rate -c:a dca -strict -2 -b:a 512k; done
enc dts_music_6ch.mka music 6 48000 -c:a dca -strict -2 -b:a 768k
enc dts_music_2ch.mka music 2 48000 -c:a dca -strict -2 -b:a 768k
enc dts_music_2ch.mp4 music 2 48000 -c:a dca -strict -2 -b:a 768k -f mp4
# AC-3 audio coding modes (the pan filter assigns the source channels to the layout) and sample rates.
for lay in "2.1:3" "3.0:3" "3.0(back):3" "4.0:4" "quad:4" "4.1:5" "5.0:5" "5.0(side):5" "5.1:6"; do
    name="${lay%%:*}"; n="${lay##*:}"; pan="pan=$name"; for ((i=0;i<n;i++)); do pan+="|c$i=c$i"; done
    mk_src music $n 48000
    $FF -i "$SRC/music_${n}ch_48000.wav" -af "$pan" -c:a ac3 -b:a 256k "$OUT/ac3_layout_$(tr -c 'a-z0-9.\n' '_' <<<"$name").ac3" 2>/dev/null || echo "skip ac3 layout $name"
done
for r in 32000 44100; do enc "ac3_music_2ch_rate${r}.ac3" music 2 48000 -c:a ac3 -ar $r; enc "ac3_music_6ch_rate${r}.ac3" music 6 48000 -c:a ac3 -ar $r -b:a 384k; done
for r in 32000 44100; do enc "eac3_music_2ch_rate${r}.eac3" music 2 48000 -c:a eac3 -ar $r; done
enc eac3_music_6ch.mka music 6 48000 -c:a eac3
enc eac3_music_6ch.mp4 music 6 48000 -c:a eac3
enc ac3_music_2ch.mka music 2 48000 -c:a ac3
enc ac3_music_2ch.mp4 music 2 48000 -c:a ac3
enc ac3_music_2ch.wav music 2 48000 -c:a ac3

# WMA.
for v in wmav1 wmav2; do for ch in 1 2; do enc "${v}_music_${ch}ch.wma" music $ch 44100 -c:a $v -b:a 128k; done; done

# Speex / AMR / misc speech codecs.
enc speex_music_1ch.ogg music 1 16000 -c:a libspeex
for rate in 8000 16000 32000; do
    for lvl in 0 4 10; do enc "speex_music_1ch_${rate}_c${lvl}.ogg" music 1 $rate -c:a libspeex -compression_level $lvl; done
    enc "speex_music_2ch_${rate}.ogg" music 2 $rate -c:a libspeex
done
for br in 2150 3950 6000 8000 11000 15000 18200 24600; do enc "speex_music_1ch_8000_${br}.ogg" music 1 8000 -c:a libspeex -b:a $br; done
enc speex_music_1ch_16000_dtx.ogg music 1 16000 -c:a libspeex -dtx 1 -vad 1
enc speex_music_1ch_8000_dtx.ogg music 1 8000 -c:a libspeex -dtx 1 -vad 1
enc speex_music_1ch_16000_abr.ogg music 1 16000 -c:a libspeex -abr 1 -b:a 20k
enc speex_music_1ch_16000_fpp4.ogg music 1 16000 -c:a libspeex -frames_per_packet 4
enc speex_sweep_1ch_16000.ogg sweep 1 16000 -c:a libspeex
enc speex_noise_1ch_8000.ogg noise 1 8000 -c:a libspeex
enc amrnb_1ch.amr music 1 8000 -c:a libopencore_amrnb
enc g723_1.wav music 1 8000 -c:a g723_1
enc dfpwm_1ch.dfpwm music 1 48000 -c:a dfpwm
enc nellymoser.flv music 1 22050 -c:a nellymoser
enc sbc.sbc music 2 44100 -c:a sbc
enc truehd_2ch.thd music 2 48000 -c:a truehd -strict -2
enc mlp_2ch.mlp music 2 48000 -c:a mlp -strict -2
enc real144.ra music 1 8000 -c:a real_144

# Extended FLAC / ALAC / Vorbis / MP3 / PCM variants.
for lvl in 0 3 5 8 12; do enc "flac_music_2ch_level${lvl}.flac" music 2 44100 -c:a flac -compression_level $lvl; done
for bs in 16 64 256 1152 4096 8192 16384; do enc "flac_music_2ch_bs${bs}.flac" music 2 44100 -c:a flac -frame_size $bs; done
for r in 8000 11025 16000 22050 32000 48000 88200 96000 176400 192000; do enc "flac_music_2ch_rate${r}.flac" music 2 $r -c:a flac; done
for ch in 3 4 5 7 8; do enc "flac_music_${ch}ch.flac" music $ch 44100 -c:a flac; done
enc flac_music_2ch_s32_24bit.flac music 2 44100 -c:a flac -sample_fmt s32 -bits_per_raw_sample 24
enc flac_music_2ch_ch_mode_ls.flac music 2 44100 -c:a flac -ch_mode left_side
enc flac_music_2ch_ch_mode_rs.flac music 2 44100 -c:a flac -ch_mode right_side
enc flac_music_2ch_ch_mode_ms.flac music 2 44100 -c:a flac -ch_mode mid_side
enc flac_music_2ch_ch_mode_ind.flac music 2 44100 -c:a flac -ch_mode indep
enc flac_music_2ch_lpc_cholesky.flac music 2 44100 -c:a flac -lpc_type cholesky -lpc_passes 3
enc flac_music_2ch_ogg.oga music 2 44100 -c:a flac -f ogg
enc flac_music_2ch.mka music 2 44100 -c:a flac
enc flac_music_2ch.mp4 music 2 44100 -c:a flac -strict -2
for r in 8000 22050 48000 96000 192000; do enc "alac_music_2ch_rate${r}.m4a" music 2 $r -c:a alac -sample_fmt s16p; done
enc alac_music_2ch_s32_20bit.m4a music 2 44100 -c:a alac -sample_fmt s32p -bits_per_raw_sample 20
for ch in 3 4 5 7 8; do enc "alac_music_${ch}ch.m4a" music $ch 44100 -c:a alac -sample_fmt s16p; done
enc alac_music_2ch.mov music 2 44100 -c:a alac -sample_fmt s16p
enc alac_music_2ch.mka music 2 44100 -c:a alac -sample_fmt s16p
for q in 0 5 10; do for r in 8000 22050 48000 96000; do enc "vorbis_music_2ch_q${q}_rate${r}.ogg" music 2 $r -c:a libvorbis -q:a $q; done; done
for ch in 3 4 5 7 8; do enc "vorbis_music_${ch}ch.ogg" music $ch 44100 -c:a libvorbis; done
enc vorbis_music_2ch.oga music 2 44100 -c:a libvorbis -f ogg
enc vorbis_music_2ch.mp4 music 2 44100 -c:a libvorbis -strict -2
for ch in 3 4 5 7 8; do enc "pcm_music_s16le_${ch}ch.wav" music $ch 44100 -c:a pcm_s16le; enc "pcm_music_s24le_${ch}ch.wav" music $ch 48000 -c:a pcm_s24le; done
enc pcm_music_s16le_2ch.mka music 2 44100 -c:a pcm_s16le
enc pcm_music_s32le_2ch.mka music 2 44100 -c:a pcm_s32le
enc pcm_music_f32le_2ch.mka music 2 44100 -c:a pcm_f32le
enc pcm_music_s16le_2ch.mov music 2 44100 -c:a pcm_s16le
enc pcm_music_u8_2ch.mov music 2 44100 -c:a pcm_u8
enc pcm_music_s16le_2ch.caf music 2 44100 -c:a pcm_s16le
enc pcm_music_u8_2ch.aiff music 2 44100 -c:a pcm_u8 -f aiff
enc mp3_music_2ch_cbr_vbr.mp3 music 2 44100 -c:a libmp3lame -q:a 2 -compression_level 0
enc mp3_music_2ch_notags.mp3 music 2 44100 -c:a libmp3lame -write_xing 0 -id3v2_version 0
enc mp3_music_2ch_id3v24.mp3 music 2 44100 -c:a libmp3lame -id3v2_version 4 -metadata title=Test -metadata artist=Tester
enc mp3_music_2ch_crc.mp3 music 2 44100 -c:a libmp3lame -error_protection 1
enc mp3_music_2ch_mono_to_mixed.mp3 music 2 44100 -c:a libmp3lame -joint_stereo 1 -b:a 32k
for sig in music sweep noise; do enc "mp1_${sig}_2ch.mp1" $sig 2 44100 -c:a mp2 -f mp2 -b:a 192k; done
enc aac_music_2ch_adts_crc.aac music 2 44100 -c:a aac -f adts -adts_crc 1 2>/dev/null

# Chained Ogg streams (the decoder must be reset between links).
[ -s "$OUT/chain_vorbis.ogg" ] || cat "$OUT/vorbis_music_2ch_q3.ogg" "$OUT/vorbis_sweep_2ch_q3.ogg" > "$OUT/chain_vorbis.ogg"
[ -s "$OUT/chain_opus.opus" ] || cat "$OUT/opus_music_2ch_64k_audio.opus" "$OUT/opus_sweep_2ch_64k_audio.opus" > "$OUT/chain_opus.opus"

echo "Generated $(find "$OUT" -type f | wc -l) files in $OUT"
