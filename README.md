# Sonata

Sonata is a pure Rust audio decoding and media demuxing library supporting OGG, FLAC, MP3, and WAV.

## Features

Sonata's planned features are:

* Decode support for the most popular audio codecs
* Reading the most common media container formats
* Probing and guessing the correct format and decoder combination(s) for playback or inspection
* Reading metadata
* Providing a set of audio primitives for manipulating audio data efficiently
* Providing a C API for integration into other languages
* Providing a WASM API for web usage

## Format and Codec Support Roadmap

Support for individual audio codecs and media formats is provided by separate crates. By default, Sonata selects
support for FOSS codecs and formats, but others may be included via the features option.

### Formats (Demux)

| Format  | Status      | Feature Flag | Default | Crate                  |  
|---------|-------------|--------------|---------|------------------------|
| ISO/MP4 | -           | `isomp4`     | No      | `sonata-format-isomp4` |
| MKV     | -           | `mkv`        | Yes     | `sonata-format-mkv`    |
| OGG     | Functional  | `ogg`        | Yes     | `sonata-format-ogg`    |
| Wave    | Complete    | `wav`        | Yes     | `sonata-format-wav`    |
| WebM    | -           | `webm`       | No      | `sonata-format-webm`   |

### Codecs (Decode)

| Codec    | Status      | Feature Flag | Default | Crate                  |
|----------|-------------|--------------|---------|------------------------|
| AAC      | -           | `aac`        | No      | `sonata-codec-aac`     |
| FLAC     | Complete    | `flac`       | Yes     | `sonata-codec-flac`    |
| MP1      | Paused      | `mp3`        | No      | `sonata-codec-mp3`     |
| MP2      | Paused      | `mp3`        | No      | `sonata-codec-mp3`     |
| MP3      | Complete    | `mp3`        | No      | `sonata-codec-mp3`     |
| Opus     | Next        | `opus`       | Yes     | `sonata-codec-opus`    |
| PCM      | Complete    | `pcm`        | Yes     | `sonata-codec-pcm`     |
| Vorbis   | -           | `vorbis`     | Yes     | `sonata-codec-vorbis`  |
| WavPack  | -           | `wavpack`    | Yes     | `sonata-codec-wavpack` |

<!--
### Codecs (Encode)

Sonata does not aim to provide Rust-based encoders for codecs. This is because most encoders have undergone years of development, tweaking, and optimization. Replicating this work would be difficult and provide little benefit for safety because the input to an encoder is controlled by the developer unlike a decoder or demuxer.

Sonata plans to provide "unsafe" encoder packages that wrap traditional C-based encoders.

| Codec    | Status      | Feature Flag | Default | Crate                           |
|----------|-------------|--------------|---------|---------------------------------|
| Flac     | -           | `libflac`    | No      | `sonata-unsafe-codec-libflac`   |
| Hardware | -           | `hwenc`      | No      | `sonata-codec-hwenc`            |
| Opus     | -           | `libopus`    | No      | `sonata-unsafe-codec-libopus`   |
| Vorbis   | -           | `libvorbis`  | No      | `sonata-unsafe-codec-libvorbis` |
-->

### Tags (Read)

Sonata provides decoders for standard tagging formats in `sonata-core` since many multimedia formats share common tagging formats.

| Format                | Status      |
|-----------------------|-------------|
| ID3v1                 | Complete    |
| ID3v2                 | Complete    |
| Vorbis comment (OGG)  | In Work     |
| Vorbis comment (FLAC) | Complete    |
| RIFF                  | Complete    |
| APEv1                 | -           |
| APEv2                 | -           |

## Quality

In addition to the safety guarantees provided by Rust, Sonata aims to:

* Decode files as well as the leading free-and-open-source software decoders
* Provide a powerful, consistent, and easy to use API
* Have absolutely no unsafe blocks outside of `sonata-core`
* Have very minimal dependencies
* Prevent denial-of-service attacks
* Be fuzz-tested

## Performance

Sonata aims to be equivalent in speed to popular open-source C-based implementations.

Sonata does not include explicit SIMD optimizations, however the auto-vectorizer is leveraged as much as possible and the results have been *excellent*. As Rust support for packed SIMD grows, Sonata will include explicit SIMD optimizations where necessary.

### Benchmarks (as of Sept. 7/2019)

These benchmarks compare the single-threaded decoding performance of both Sonata and FFmpeg with various audio files.

The benchmarks were executed on an Arch Linux system with a Core i7 4790k and 32GB of RAM, for a minimum of 20 runs each. [Hyperfine](https://github.com/sharkdp/hyperfine) was used to execute the test. The full benchmark script is as follows:

```bash
#!/bin/bash
IN="${1@Q}"

hyperfine -m 20 "ffmpeg -threads 1 -benchmark -v 0 -i ${IN} -f null -" "sonata-play --decode-only ${IN}"
```

#### MP3, 192kbps @ 44.1kHz

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| Sonata | 306.2 ± 3.0 | 301.8 | 312.5 | 1.1 |
| FFmpeg | 272.7 ± 4.3 | 267.6 | 285.3 | 1.0 |

#### MP3, 320kbps @ 44.1kHz

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| Sonata | 355.1 ± 8.4 | 348.2 | 376.2 | 1.1 |
| FFmpeg | 316.0 ± 3.5 | 308.8 | 322.8 | 1.0 |

#### FLAC, 24-bit @ 96kHz

| Decoder | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| Sonata | 453.6 ± 2.9 | 449.3 | 462.4 | 1.0 |
| FFmpeg | 501.9 ± 4.3 | 496.4 | 512.7 | 1.1 |

#### FLAC, 24-bit @ 48kHz

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| Sonata | 324.0 ± 8.9 | 315.4 | 346.3 | 1.0 |
| FFmpeg | 331.0 ± 7.4 | 323.6 | 354.5 | 1.0 |

#### WAVE, S32LE @ 44.1kHz

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| Sonata | 84.5 ± 1.8 | 81.8 | 89.1 | 1.0 |
| FFmpeg | 129.8 ± 3.4 | 123.4 | 136.1 | 1.5 |

## Tools

Sonata provides the following tools for debugging purposes:

* `sonata-play` for probing files and playing back audio, as well as serving as a demo application

## Motivation

Rust makes a lot of sense for multimedia programming, particularly when accessing that media over the network. However, currently it is difficult to do something as simple as play a FLAC file. Rust does not have a library like FFMpeg, and even if one uses the FFI, FFMpeg is a difficult library to use and you'd have none of the protections afforded to you by Rust. Sonata is therefore an attempt to fill-in that gap.

Personally, this is a project to learn Rust and experiment with signal processing.

## Authors

The primary author is Philip Deljanov.

## License

Sonata is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Sonata is an open-source project and contributions are very welcome! If you would like to make a large contribution, please raise an issue ahead of time to make sure your efforts fit into the project goals, and that there's no duplication of effort. Please be aware that all contributions must also be licensed under the MPL v2.0 license to be accepted.

All contributors will be credited within the CONTRIBUTORS file.
