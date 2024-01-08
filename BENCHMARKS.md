# Benchmarks

**Last Updated**: February 3, 2023

These benchmarks compare the single-thread[^1] decoding performance of Symphonia against FFmpeg with various audio files.

*It is always a good idea to run your own benchmarks!*

## Tools

### Symphonia

The `symphonia-play` demo application is used in decode-only mode. In this mode, `symphonia-play` will decode the first audio track as fast as possible.

Symphonia is built with `RUSTFLAGS="-C target-cpu=native"`, SIMD optimizations enabled, and the latest Rust compiler (currently `1.67.0`).

`symphonia-play` can be run in decode-only mode with the following command:

```bash
symphonia-play --decode-only /path/to/file/to/decode
```

### FFmpeg

Using the `ffmpeg` utility with the [null](https://trac.ffmpeg.org/wiki/Null) muxer is not an acceptable benchmark because it performs audio conversion to PCM S16LE even though the output is discarded. Additionally, because the `ffmpeg` binary shipped by most package managers dynamically links the underlying `libavcodec`, `libavformat`, `libavutil`, etc. libraries, there is a non-negligible amount of time spent loading those shared libraries among others.

To benchmark FFmpeg decode performance, a new tool named [`ffbench`](https://github.com/pdeljanov/FFmpeg/blob/symphonia-ffbench/doc/examples/ffbench.c) was added to the examples of [`pdeljanov/FFmpeg`](https://github.com/pdeljanov/FFmpeg). This tool is based off the demuxing & decoding example. Since the tool is built as an example, the underlying libraries are statically linked.

To build this tool yourself, the following steps may be used:

```bash
git clone https://github.com/pdeljanov/FFmpeg.git
cd FFmpeg
git checkout symphonia-ffbench
./configure
make -j16
make -j16 examples
```

`ffbench` can be run with the following command:

```bash
./doc/examples/ffbench /path/to/file/to/decode
```

## Methodology

The latest development versions based off the `master` branch of both projects is used.

[Hyperfine](https://github.com/sharkdp/hyperfine) is used to execute the tests. Each file is benchmarked for a minimum of 30 runs after 3 discarded cache warmup runs.

The following script makes it easier to run a benchmark:

```bash
#!/bin/bash
hyperfine --warmup 3 -m 30 "./ffbench ${1}" "./symphonia-play --decode-only ${1}"
```

The mean relative time spent decoding is calculated by taking the mean run time of all Symphonia runs and dividing it by the mean run time of all FFmpeg runs. A value of less-than 1.0 indicates Symphonia beat FFmpeg. The lower the relative time spent decoding, the better.

## Limitations

These benchmarks are basic. If you're interested in helping implement a more comprehensive and reproduceable benchmark suite, please reach out!

In these tests, only a couple files are tested per decoder from the author's personal media library and test vectors. It should be assumed that the decoders are optimized for these files the best since they were used to test and validate the decoders themselves. Different encodings may exercise different code paths of a decoder. A more robust benchmark suite would test tens or hundreds of files per decoder.

Since Symphonia depends heavily on auto-vectorization and LLVM optimizations, the Rust compiler version may strongly impact performance.

## Results

### Intel Core i7-4790K, 4C/8T @ 4.0 GHz

| Codec | Symphonia (ms) | FFmpeg (ms) | Relative (Smaller is Better) |
|:------|:--------------:|:------------:|:----------------------------:|
| AAC-LC #1 (44 kHz, 260 kbps) | 148.8 ± 1.5 | 123.5 ± 1.1 | 1.21 |
| AAC-LC #2 (44 kHz, 260 kbps) | 131.0 ± 1.9 | 109.8 ± 1.0 | 1.19 |
| ADPCM IMA (44 kHz) | 905.0 ± 15.3 | 1095.6 ± 5.7 | 0.83 |
| ADPCM MS (44 kHz) | 1095.3 ± 7.6 | 972.3 ± 8.3 | 1.13 |
| ALAC (44 kHz, 16 bit) | 426.7 ± 3.4 | 586.7 ± 1.5 | 0.73 |
| ALAC (96 kHz, 32 bit) | 1363.6 ± 5.2 | 1724.8 ± 3.9 | 0.79 |
| FLAC (44 kHz, 16 bit) | 190.9 ± 1.0 | 267.8 ± 1.3 | 0.71 |
| FLAC (48 kHz, 24 bit) | 211.7 ± 0.8 | 295.5 ± 0.9 | 0.72 |
| FLAC (96 kHz, 24 bit) | 176.4 ± 1.4 | 239.5 ± 1.0 | 0.74 |
| MP1 (44 kHz, 192 kbps) | 91.9 ± 0.8 | 202.0 ± 1.8 | 0.45 |
| MP1 (44 kHz, 448 kbps) | 106.2 ± 0.9 | 222.8 ± 1.3 | 0.48 |
| MP2 (44 kHz, 128 kbps) | 207.1 ± 2.0 | 346.1 ± 5.0 | 0.60 |
| MP2 (44 kHz, 384 kbps) | 239.5 ± 2.7 | 396.4 ± 3.6 | 0.60 |
| MP3 (44 kHz, 128 kbps) | 262.2 ± 2.0 | 253.0 ± 1.0 | 1.04 |
| MP3 (44 kHz, 320 kbps) | 187.6 ± 1.5 | 222.9 ± 1.1 | 0.84 |
| Vorbis (44 kHz, ~256 kbps) | 317.6 ± 2.2 | 302.9 ± 2.5 | 1.05 |

### Apple M1 Pro, 8C/8T Firestorm (Performance) Cores @ 3.2 GHz + 2C/2T Icestorm (Efficiency) Cores @ 2.0 GHz

| Codec | Symphonia (ms) | FFmpeg (ms) | Relative (Smaller is Better) |
|:------|:--------------:|:-----------:|:----------------------------:|
| AAC-LC #1 (44 kHz, 260 kbps) | 127.4 ± 0.6 | 115.6 ± 0.7 | 1.10 |
| AAC-LC #2 (44 kHz, 260 kbps) | 112.7 ± 0.3 | 102.2 ± 0.5 | 1.10 |
| ADPCM IMA (44 kHz) | 1191.3 ± 1.6 | 789.4 ± 0.9 | 1.51 |
| ADPCM MS (44 kHz) | 724.5 ± 1.8 | 1235.5 ± 6.0 | 0.59 |
| ALAC (44 kHz, 16 bit) | 339.3 ± 2.6 | 420.6 ± 1.2 | 0.81 |
| ALAC (96 kHz, 32 bit) | 1079.5 ± 1.7 | 1403.2 ± 1.5 | 0.77 |
| FLAC (44 kHz, 16 bit) | 170.8 ± 0.9 | 267.7 ± 0.5 | 0.64 |
| FLAC (48 kHz, 24 bit) | 179.2 ± 0.4 | 349.6 ± 0.8 | 0.51 |
| FLAC (96 kHz, 24 bit) | 155.2 ± 0.5 | 274.4 ± 1.1 | 0.57 |
| MP1 (44 kHz, 192 kbps) | 72.2 ± 0.3 | 114.4 ± 0.7 | 0.63 |
| MP1 (44 kHz, 448 kbps) | 83.2 ± 0.4 | 130.5 ± 0.4 | 0.64 |
| MP2 (44 kHz, 128 kbps) | 144.8 ± 0.4 | 202.1 ± 0.5 | 0.72 |
| MP2 (44 kHz, 384 kbps) | 177.0 ± 0.3 | 230.8 ± 1.6 | 0.77 |
| MP3 (44 kHz, 128 kbps) | 204.3 ± 0.5 | 203.9 ± 0.6 | 1.00 |
| MP3 (44 kHz, 320 kbps) | 150.2 ± 0.3 | 186.5 ± 0.7 | 0.81 |
| Vorbis (44 kHz, ~256 kbps) | 307.9 ± 0.6 | 247.6 ± 0.6 | 1.24 |

### Raspberry Pi 4B, Broadcom BCM2711, 4C/4T Cortex-A72 (ARM v8) @ 1.5 GHz

| Codec | Symphonia (ms) | FFmpeg (ms) | Relative (Smaller is Better) |
|:------|:--------------:|:-----------:|:----------------------------:|
| AAC-LC #1 (44 kHz, 260 kbps) | 566.6 ± 2.5 | 446.8 ± 2.6 | 1.27 |
| AAC-LC #2 (44 kHz, 260 kbps) | 502.8 ± 3.0 | 401.7 ± 2.9 | 1.25 |
| ADPCM IMA (44 kHz) | 3033.3 ± 16.9 | 2879.9 ± 5.8 | 1.05 |
| ADPCM MS (44 kHz) | 2536.5 ± 5.9 | 2655.5 ± 7.1 | 0.96 |
| ALAC (44 kHz, 16 bit) | 1227.2 ± 2.7 | 1512.9 ± 1.6 | 0.81 |
| ALAC (96 kHz, 32 bit) | 3900.4 ± 13.8 | 4456.7 ± 5.7 | 0.88 |
| FLAC (44 kHz, 16 bit) | 734.2 ± 2.6 | 704.2 ± 2.0 | 1.04 |
| FLAC (48 kHz, 24 bit) | 818.3 ± 2.3 | 855.8 ± 2.5 | 0.96 |
| FLAC (96 kHz, 24 bit) | 650.4 ± 1.8 | 677.0 ± 2.0 | 0.96 |
| MP1 (44 kHz, 192 kbps) | 428.2 ± 11.7 | 514.9 ± 3.5 | 0.83 |
| MP1 (44 kHz, 448 kbps) | 498.5 ± 6.5 | 574.1 ± 7.4 | 0.87 |
| MP2 (44 kHz, 128 kbps) | 796.8 ± 23.6 | 879.9 ± 3.9 | 0.91 |
| MP2 (44 kHz, 384 kbps) | 930.7 ± 6.6 | 1001.5 ± 19.0 | 0.93 |
| MP3 (44 kHz, 128 kbps) | 964.4 ± 8.1 | 899.5 ± 6.8 | 1.07 |
| MP3 (44 kHz, 320 kbps) | 693.3 ± 8.0 | 726.6 ± 3.8 | 0.95 |
| Vorbis (44 kHz, ~256 kbps) | 1209.9 ± 3.0 | 1075.5 ± 6.9 | 1.12 |

## Summary

Overall, Symphonia is very competitive against FFmpeg across a range of common processors and codecs.

Interestingly, the performance of some decoders varies wildy depending on CPU architecture. If you are up for an optimization challenge, please consider reaching out!

[^1]: FFmpeg supports decoding ALAC and FLAC using multiple threads, however, this does not appear to be the default behaviour of the library. Currently, Symphonia decoders do not use more than one thread. Therefore, for these codecs, this is a fair comparison of the efficiency of both libraries.
