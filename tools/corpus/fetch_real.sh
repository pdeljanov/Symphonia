#!/usr/bin/env bash
# Downloads real-world conformance samples from samples.ffmpeg.org into <out>/real.
set -u
OUT="${1:-tmp/corpus}/real"
mkdir -p "$OUT"
A=https://samples.ffmpeg.org/A-codecs
get() { [ -s "$OUT/$2" ] || curl -sf -m 300 -o "$OUT/$2" "$A/$1" || echo "FAIL $1"; }

for f in AAC/ct_nero-heaac.mp4 AAC/8_Channel_ID.m4a AAC/ct_faac-adts.aac AAC/channel_model/elem_id0.mp4 AAC/channel_model/lfe_is_sce.mp4 \
         lossless/luckynight.flac lossless/luckynight.ape lossless/luckynight.wv lossless/luckynight.tta lossless/luckynight.m4a \
         DTS/lotr_5.1_768.dts DTS/scissorhands-4.0-48_24.dts DTS/Fire__VooDoo_Studio_30_sec.wav DTS/dts/dtswavsample14.wav ima-adpcm/test_ima_adpcm.wav lossless/wavpack/because.wv; do
    get "$f" "$(basename "$f")"
done

P=AAC/aacPlusDecoderCheckPackage_v2.1
for f in aac.wav aacPlusv1.wav aacPlusv2.wav; do get "$P/$f" "ref_$f"; done
for f in File1.aac File2.mp4 File3.mp4 File4.mp4 File5.mp4 File6.3gp File7.3gp; do get "$P/bitstreams/$f" "heaac_$f"; done

W=lossless/wavpack/test_suite
for f in bit_depths/8bit bit_depths/12bit bit_depths/16bit bit_depths/20bit bit_depths/24bit bit_depths/32bit_int bit_depths/32bit_float \
         bit_depths/32bit_int_p bit_depths/32bit_float_p num_channels/mono-1 num_channels/stereo-2 num_channels/panslab_sample_5.1_16bit \
         num_channels/panslab_sample_5.1_16bit_lossy num_channels/edward_4.0_16bit special_cases/false_stereo special_cases/zero_lsbs \
         speed_modes/default speed_modes/fast speed_modes/high speed_modes/vhigh hybrid_bitrates/128kbps hybrid_bitrates/24kbps; do
    get "$W/$f.wv" "wv_$(basename "$f").wv"
done

# FATE AAC conformance streams (profiles, tools, channel configurations).
mkdir -p "$OUT/../fate_aac"
for f in Fd_2_c1_Ms_0x01.mp4 Fd_2_c1_Ms_0x04.mp4 aac-sce-in-stereo.mp4 al04_44.mp4 al04sf_48.mp4 al05_44.mp4 al06_44.mp4 al07_96.mp4 al15_44.mp4 al17_44.mp4 al18_44.mp4 al22_chCfg0PCE_44.mp4 al_sbr_cm_48_2.mp4 al_sbr_cm_48_5.1.mp4 al_sbr_ps_04_new.mp4 al_sbr_ps_06_new.mp4 al_sbr_sr_48_2_fsaac48.mp4 am00_88.mp4 am05_44.mp4 ap05_48.mp4 er_ad6000np_44_ep0.mp4 er_eld1001np_44_ep0.mp4 er_eld2000np_48_ep0.mp4 er_eld2100np_48_ep0.mp4 sbr_edit_end.mp4; do
    [ -s "$OUT/../fate_aac/$f" ] || curl -sf -m 300 -o "$OUT/../fate_aac/$f" "https://fate-suite.ffmpeg.org/aac/$f" || echo "FAIL $f"
done

# AC-3 and E-AC-3 streams (a52dec test streams, 5.1 and stereo music, E-AC-3 with and without AHT and SPX).
AC3=AC3
for f in Broadway-5.1-48khz-448kbit.ac3 TomorrowNeverDies-2.1-48khz-192kbit.ac3 monsters_inc_2.0_192.ac3 \
         eac3/csi_miami_stereo_128_spx.eac3 eac3/csi_miami_5.1_256_spx.eac3 eac3/rio_bravo_mono_64_spx.ac3 eac3/casablanca_aht_mono_64.eac3 \
         eac3/matrix2_commentary1_stereo_192.eac3 eac3/matrix2_english_5.1_640.eac3 eac3/buggy_aht_5.1_384.eac3; do
    get "$AC3/$f" "$(basename "$f")"
done
for f in egypt thx thx_2_0; do get "$AC3/a52dec-streams/$f" "a52dec_$f.ac3"; done
