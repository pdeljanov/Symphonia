# Fuzzing in Symphonia

This directory contains the fuzzing infrastructure for Symphonia, utilizing `cargo-fuzz` and `libfuzzer-sys`.

## Targets

### General

* **`decode_any`**: Fuzzes the full pipeline (Probe -> Demuxer -> Decoder). Good for container formats.

### Component Decoder Fuzzers

These targets bypass the demuxer and feed data directly to the codec implementation, maximizing decoder coverage.

* **`decode_mpa`**: MPEG Audio Layer 1/2/3
* **`decode_flac`**: FLAC
* **`decode_aac`**: AAC (LC)
* **`decode_vorbis`**: Vorbis
* **`decode_pcm`**: PCM (All supported integer/float formats)
* **`decode_adpcm`**: ADPCM (Microsoft, IMA-WAV, IMA-QT)
* **`decode_alac`**: Apple Lossless (ALAC)

### Component Demuxer Fuzzers

These targets instantiate a specific `FormatReader` directly, bypassing probing. This ensures coverage of demuxing logic even if the probe would have failed or if specific header bytes are missing.

* **`demux_mkv`**: Matroska / WebM
* **`demux_isomp4`**: ISO/MP4
* **`demux_ogg`**: OGG
* **`demux_flac`**: FLAC container
* **`demux_wav`**: WAV / RIFF
* **`demux_aiff`**: AIFF
* **`demux_caf`**: CAF

### Format + Decoder Fuzzers (`fmtdec_*`)

These targets instantiate one `FormatReader` directly and decode every packet it produces with the decoder that the registry selects for the track parameters, so the container and the codec are exercised together with realistic parameters. There is one for each of: `ape`, `tta`, `adx`, `sbc`, `dfpwm`, `truehd`, `ac3`, `dca`, `wavpack`, `flv`, `ts`, `w64`, `adts`, `ogg`, `wav`, `isomp4`, `mkv`, `caf`, `aiff`, `flac`, `mpa`.

### Raw Decoder Fuzzers (`rawdec_*`)

These targets feed a decoder directly. The first bytes of the input choose the channel layout, sample rate, bits per sample and extra data length, followed by the extra data and then packets that are each prefixed with a 16-bit little endian length (see `fuzz_raw_decoder`). Targets: `opus`, `speex`, `g723`, `nellymoser`, `ac3` (bit 2 of the first byte selects E-AC-3), `dca`, `sbc`, `dfpwm`, `truehd` (bit 2 selects MLP), `adx`, `tta`, `ape`, `wavpack`, `swf_adpcm`, `he_aac`.

Targets take `&[u8]` (not `Vec<u8>`, whose `Arbitrary` decoding destroys the structure of seed files), so real files work as seeds.

## How to Run

From the `symphonia/fuzz` directory:

### Run a specific target

```bash
# Run the MPEG Audio fuzzer indefinitely
cargo fuzz run decode_mpa
```

### Run all targets (Smoke Test)

You can use the provided script to run all fuzzers sequentially for a set duration (default 10s) to verify they are working.

```bash
./run_all.sh [duration_in_seconds; default 10s]
```
