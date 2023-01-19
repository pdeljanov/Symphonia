# Benchmarks

**Last Updated**: January 18, 2022 using Symphonia v0.5.2

These benchmarks compare the single-threaded decoding performance of both Symphonia and FFMpeg with various audio files.

## Methodology

Each file was benchmarked for a minimum of 20 runs after 2 discarded cache warmup runs. [Hyperfine](https://github.com/sharkdp/hyperfine) was used to execute the test. A helper script was used to simplify the invocation and collection of results from Hyperfine.

```bash
#!/bin/bash
IN="${1}"
hyperfine --export-json ${IN}.json --warmup 2 -m 20 "ffmpeg -threads 1 -benchmark -v 0 -i ${IN} -f null -" "symphonia-play --decode-only ${IN}"
```

The mean relative time spent decoding is calculated by taking the mean run time of all Symphonia runs and dividing it by the mean run time of all FFMpeg runs. A value of less-than 1.0 indicates Symphonia beat FFMpeg. The lower the relative time spent decoding, the better.

## Limitations

These benchmarks are rather basic. If you're interested in helping implement a more comprehensive and reproduceable benchmark suite, please reach out!

In these tests, only a couple files are tested per decoder from the author's personal media library and test vectors. It should be assumed that the decoders are optimized for these files the best since they were used to test and validate the decoders themselves. Different encodings may exercise different code paths of a decoder. A more robust benchmark suite would test tens or hundreds of files per decoder.

The benchmarks are also only performed with Rust version `1.66.1`. Since Symphonia depends heavily on auto-vectorization and LLVM optimizations, the compiler version could strongly impact performance.

It is a good idea to do your own benchmarks if your application is performance critical!

## Results

### AMD Ryzen 3 2200G, 4C/4T @ 3.5 GHz

| Codec | Symphonia (ms) | FFMpeg (ms) | Relative (Smaller is Better) |
|:------|:--------------:|:-----------:|:--------------------------:|
| AAC-LC #1 (44 kHz, 260 kbps) | 243.2 ± 4.9 | 259.1 ± 13.0 | 0.94 |
| AAC-LC #2 (44 kHz, 260 kbps) | 215.4 ± 7.1 | 234.2 ± 7.4 | 0.92 |
| ADPCM IMA (44 kHz) | 83.7 ± 2.6 | 227.1 ± 3.3 | 0.37 |
| ADPCM MS (44 kHz) | 106.4 ± 4.1 | 179.6 ± 5.0 | 0.59 |
| ALAC (44 kHz, 16 bit) | 579.4 ± 6.8 | 662.9 ± 0.6 | 0.87 |
| ALAC (96 kHz, 32 bit) | 1826.2 ± 5.1 | 1942.6 ± 9.8 | 0.94 |
| FLAC (44 kHz, 16 bit) | 281.7 ± 5.9 | 313.2 ± 5.9 | 0.90 |
| FLAC (48 kHz, 24 bit) | 300.4 ± 4.2 | 352.8 ± 5.9 | 0.85 |
| FLAC (96 kHz, 24 bit) | 255.3 ± 5.2 | 288.2 ± 6.0 | 0.89 |
| MP1 (44 kHz, 192 kbps) | 120.2 ± 4.2 | 409.7 ± 7.1 | 0.29 |
| MP1 (44 kHz, 448 kbps) | 134.8 ± 3.6 | 441.2 ± 10.3 | 0.31 |
| MP2 (44 kHz, 128 kbps) | 128.8 ± 4.8 | 285.5 ± 6.4 | 0.45 |
| MP2 (44 kHz, 384 kbps) | 91.7 ± 4.6 | 216.7 ± 5.4 | 0.42 |
| MP3 (44 kHz, 128 kbps) | 408.7 ± 6.4 | 430.9 ± 6.7 | 0.95 |
| MP3 (44 kHz, 320 kbps) | 289.7 ± 5.7 | 370.6 ± 3.1 | 0.78 |
| PCM S32LE (44 kHz) | 43.7 ± 3.0 | 190.3 ± 4.5 | 0.23 |
| Vorbis (44 kHz, ~256 kbps) | 482.2 ± 5.6 | 547.9 ± 9.3 | 0.88 |

### Intel Core i7-4790K, 4C/8T @ 4.0 GHz

| Codec | Symphonia (ms) | FFMpeg (ms) | Relative (Smaller is Better) |
|:------|:--------------:|:-----------:|:--------------------------:|
| AAC-LC #1 (44 kHz, 260 kbps) | 220.3 ± 1.4 | 190.8 ± 2.4 | 1.15 |
| AAC-LC #2 (44 kHz, 260 kbps) | 194.1 ± 1.4 | 173.5 ± 2.5 | 1.12 |
| ADPCM IMA (44 kHz) | 80.3 ± 1.0 | 162.7 ± 1.8 | 0.49 |
| ADPCM MS (44 kHz) | 96.6 ± 1.3 | 140.7 ± 1.8 | 0.69 |
| ALAC (44 kHz, 16 bit) | 530.4 ± 8.0 | 631.6 ± 11.5 | 0.84 |
| ALAC (96 kHz, 32 bit) | 1643.0 ± 15.7 | 1790.0 ± 6.4 | 0.92 |
| FLAC (44 kHz, 16 bit) | 222.1 ± 2.5 | 283.9 ± 1.5 | 0.78 |
| FLAC (48 kHz, 24 bit) | 246.9 ± 2.3 | 312.5 ± 2.3 | 0.79 |
| FLAC (96 kHz, 24 bit) | 206.2 ± 1.6 | 259.2 ± 3.0 | 0.80 |
| MP1 (44 kHz, 192 kbps) | 105.9 ± 1.3 | 302.0 ± 2.3 | 0.35 |
| MP1 (44 kHz, 448 kbps) | 120.1 ± 1.0 | 326.3 ± 4.2 | 0.37 |
| MP2 (44 kHz, 128 kbps) | 117.6 ± 0.9 | 223.3 ± 2.3 | 0.53 |
| MP2 (44 kHz, 384 kbps) | 79.5 ± 1.3 | 170.3 ± 2.2 | 0.47 |
| MP3 (44 kHz, 128 kbps) | 344.5 ± 4.1 | 333.9 ± 5.1 | 1.03 |
| MP3 (44 kHz, 320 kbps) | 250.2 ± 3.7 | 295.1 ± 11.5 | 0.85 |
| PCM S32LE (44 kHz) | 43.8 ± 1.0 | 127.2 ± 2.3 | 0.34 |
| Vorbis (44 kHz, ~256 kbps) | 469.1 ± 4.3 | 437.7 ± 6.9 | 1.07 |

### Apple M1 Pro, 8C/8T Firestorm (Performance) Cores @ 3.2 GHz + 2C/2T Icestorm (Efficiency) Cores @ 2.0 GHz

| Codec | Symphonia (ms) | FFMpeg (ms) | Relative (Smaller is Better) |
|:------|:--------------:|:-----------:|:--------------------------:|
| AAC-LC #1 (44 kHz, 260 kbps) | 152.2 ± 1.2 | 151.6 ± 1.0 | 1.00 |
| AAC-LC #2 (44 kHz, 260 kbps) | 134.4 ± 0.3 | 136.7 ± 0.7 | 0.98 |
| ADPCM IMA (44 kHz) | 103.2 ± 1.6 | 145.3 ± 0.9 | 0.71 |
| ADPCM MS (44 kHz) | 62.9 ± 0.4 | 116.3 ± 0.8 | 0.54 |
| ALAC (44 kHz, 16 bit) | 334.9 ± 2.3 | 260.3 ± 1.0 | 1.29 |
| ALAC (96 kHz, 32 bit) | 1092.0 ± 0.8 | 806.8 ± 2.4 | 1.35 |
| FLAC (44 kHz, 16 bit) | 193.0 ± 0.4 | 165.0 ± 0.9 | 1.17 |
| FLAC (48 kHz, 24 bit) | 209.7 ± 0.8 | 185.0 ± 0.9 | 1.13 |
| FLAC (96 kHz, 24 bit) | 176.7 ± 0.3 | 159.3 ± 0.7 | 1.11 |
| MP1 (44 kHz, 192 kbps) | 70.2 ± 0.4 | 174.5 ± 1.3 | 0.40 |
| MP1 (44 kHz, 448 kbps) | 82.4 ± 0.3 | 189.6 ± 1.5 | 0.43 |
| MP2 (44 kHz, 128 kbps) | 65.9 ± 0.4 | 138.1 ± 1.1 | 0.48 |
| MP2 (44 kHz, 384 kbps) | 47.8 ± 0.3 | 110.8 ± 0.7 | 0.43 |
| MP3 (44 kHz, 128 kbps) | 269.5 ± 1.2 | 286.3 ± 1.3 | 0.94 |
| MP3 (44 kHz, 320 kbps) | 200.3 ± 1.5 | 276.0 ± 1.1 | 0.73 |
| PCM S32LE (44 kHz) | 28.5 ± 0.3 | 77.5 ± 1.0 | 0.37 |
| Vorbis (44 kHz, ~256 kbps) | 356.8 ± 0.7 | 316.8 ± 1.2 | 1.13 |

### Raspberry Pi 4B, Broadcom BCM2711, 4C/4T Cortex-A72 (ARM v8) @ 1.5 GHz

| Codec | Symphonia (ms) | FFMpeg (ms) | Relative (Smaller is Better) |
|:------|:--------------:|:-----------:|:--------------------------:|
| AAC-LC #1 (44 kHz, 260 kbps) | 705.0 ± 5.6 | 865.0 ± 3.6 | 0.82 |
| AAC-LC #2 (44 kHz, 260 kbps) | 629.2 ± 3.9 | 795.9 ± 2.7 | 0.79 |
| ADPCM IMA (44 kHz) | 249.4 ± 2.0 | 593.3 ± 3.3 | 0.42 |
| ADPCM MS (44 kHz) | 224.2 ± 1.9 | 514.1 ± 2.6 | 0.44 |
| ALAC (44 kHz, 16 bit) | 1314.3 ± 5.8 | 1789.0 ± 1.7 | 0.73 |
| ALAC (96 kHz, 32 bit) | 4195.5 ± 12.4 | 4953.3 ± 5.9 | 0.85 |
| FLAC (44 kHz, 16 bit) | 772.6 ± 2.1 | 901.8 ± 3.5 | 0.86 |
| FLAC (48 kHz, 24 bit) | 836.2 ± 2.8 | 1073.8 ± 5.4 | 0.78 |
| FLAC (96 kHz, 24 bit) | 672.3 ± 2.3 | 854.8 ± 3.8 | 0.79 |
| MP1 (44 kHz, 192 kbps) | 409.6 ± 9.4 | 1153.0 ± 5.8 | 0.36 |
| MP1 (44 kHz, 448 kbps) | 483.0 ± 10.8 | 1213.5 ± 4.8 | 0.40 |
| MP2 (44 kHz, 128 kbps) | 361.6 ± 8.9 | 762.3 ± 5.5 | 0.47 |
| MP2 (44 kHz, 384 kbps) | 272.6 ± 5.6 | 593.1 ± 2.7 | 0.46 |
| MP3 (44 kHz, 128 kbps) | 1231.8 ± 13.1 | 1409.3 ± 9.1 | 0.87 |
| MP3 (44 kHz, 320 kbps) | 828.8 ± 12.6 | 1175.9 ± 5.1 | 0.70 |
| PCM S32LE (44 kHz) | 173.3 ± 1.7 | 687.3 ± 4.4 | 0.25 |
| Vorbis (44 kHz, ~256 kbps) | 1507.1 ± 7.0 | 1842.6 ± 8.3 | 0.82 |

## Summary

Symphonia is extremely competitive against FFMpeg across a range of common processors and codecs.

Performance on x86_64 processors is typically at parity or better, with some marginally worse performing cases on older cores. This is likely due to improved branch prediction capabilities on newer cores. Performance on ARM is exceptional, though some outliers exist on Apple Silicon despite none existing on weaker ARM cores.
