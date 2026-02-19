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
