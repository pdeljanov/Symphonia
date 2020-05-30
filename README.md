# Symphonia (formerly Sonata)

Symphonia is a pure Rust audio decoding and media demuxing library supporting OGG, FLAC, MP3, and WAV.

## Features

Symphonia's planned features are:

* Decode support for the most popular audio codecs
* Reading the most common media container formats
* Probing and guessing the correct format and decoder combination(s) for playback or inspection
* Reading metadata
* Providing a set of audio primitives for manipulating audio data efficiently
* Providing a C API for integration into other languages
* Providing a WASM API for web usage

## Format and Codec Support Roadmap

Support for individual audio codecs and media formats is provided by separate crates. By default, Symphonia selects
support for FOSS codecs and formats, but others may be included via the features option.

### Formats (Demux)

| Format  | Status      | Feature Flag | Default | Crate                     |  
|---------|-------------|--------------|---------|---------------------------|
| ISO/MP4 | -           | `isomp4`     | No      | `symphonia-format-isomp4` |
| MKV     | -           | `mkv`        | Yes     | `symphonia-format-mkv`    |
| OGG     | Functional  | `ogg`        | Yes     | `symphonia-format-ogg`    |
| Wave    | Complete    | `wav`        | Yes     | `symphonia-format-wav`    |
| WebM    | -           | `webm`       | No      | `symphonia-format-webm`   |

### Codecs (Decode)

| Codec    | Status      | Feature Flag | Default | Crate                     |
|----------|-------------|--------------|---------|---------------------------|
| AAC      | -           | `aac`        | No      | `symphonia-codec-aac`     |
| FLAC     | Complete    | `flac`       | Yes     | `symphonia-codec-flac`    |
| MP1      | Paused      | `mp3`        | No      | `symphonia-codec-mp3`     |
| MP2      | Paused      | `mp3`        | No      | `symphonia-codec-mp3`     |
| MP3      | Complete    | `mp3`        | No      | `symphonia-codec-mp3`     |
| Opus     | Next        | `opus`       | Yes     | `symphonia-codec-opus`    |
| PCM      | Complete    | `pcm`        | Yes     | `symphonia-codec-pcm`     |
| Vorbis   | -           | `vorbis`     | Yes     | `symphonia-codec-vorbis`  |
| WavPack  | -           | `wavpack`    | Yes     | `symphonia-codec-wavpack` |

<!--
### Codecs (Encode)

Symphonia does not aim to provide Rust-based encoders for codecs. This is because most encoders have undergone years of development, tweaking, and optimization. Replicating this work would be difficult and provide little benefit for safety because the input to an encoder is controlled by the developer unlike a decoder or demuxer.

Symphonia plans to provide "unsafe" encoder packages that wrap traditional C-based encoders.

| Codec    | Status      | Feature Flag | Default | Crate                           |
|----------|-------------|--------------|---------|---------------------------------|
| Flac     | -           | `libflac`    | No      | `symphonia-unsafe-codec-libflac`   |
| Hardware | -           | `hwenc`      | No      | `symphonia-codec-hwenc`            |
| Opus     | -           | `libopus`    | No      | `symphonia-unsafe-codec-libopus`   |
| Vorbis   | -           | `libvorbis`  | No      | `symphonia-unsafe-codec-libvorbis` |
-->

### Tags (Read)

Symphonia provides decoders for standard tagging formats in `symphonia-core` since many multimedia formats share common tagging formats.

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

In addition to the safety guarantees provided by Rust, Symphonia aims to:

* Decode files as well as the leading free-and-open-source software decoders
* Provide a powerful, consistent, and easy to use API
* Have absolutely no unsafe blocks outside of `symphonia-core`
* Have very minimal dependencies
* Prevent denial-of-service attacks
* Be fuzz-tested

## Performance

Symphonia aims to be equivalent in speed to popular open-source C-based implementations.

Symphonia does not include explicit SIMD optimizations, however the auto-vectorizer is leveraged as much as possible and the results have been *excellent*. As Rust support for packed SIMD grows, Symphonia will include explicit SIMD optimizations where necessary.

### Benchmarks (as of Sept. 7/2019)

These benchmarks compare the single-threaded decoding performance of both Symphonia and FFmpeg with various audio files.

The benchmarks were executed on an Arch Linux system with a Core i7 4790k and 32GB of RAM, for a minimum of 20 runs each. [Hyperfine](https://github.com/sharkdp/hyperfine) was used to execute the test. The full benchmark script is as follows:

```bash
#!/bin/bash
IN="${1@Q}"

hyperfine -m 20 "ffmpeg -threads 1 -benchmark -v 0 -i ${IN} -f null -" "symphonia-play --decode-only ${IN}"
```

#### MP3, 192kbps @ 44.1kHz

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| Symphonia | 306.2 ± 3.0 | 301.8 | 312.5 | 1.1 |
| FFmpeg | 272.7 ± 4.3 | 267.6 | 285.3 | 1.0 |

#### MP3, 320kbps @ 44.1kHz

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| Symphonia | 355.1 ± 8.4 | 348.2 | 376.2 | 1.1 |
| FFmpeg | 316.0 ± 3.5 | 308.8 | 322.8 | 1.0 |

#### FLAC, 24-bit @ 96kHz

| Decoder | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| Symphonia | 453.6 ± 2.9 | 449.3 | 462.4 | 1.0 |
| FFmpeg | 501.9 ± 4.3 | 496.4 | 512.7 | 1.1 |

#### FLAC, 24-bit @ 48kHz

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| Symphonia | 324.0 ± 8.9 | 315.4 | 346.3 | 1.0 |
| FFmpeg | 331.0 ± 7.4 | 323.6 | 354.5 | 1.0 |

#### WAVE, S32LE @ 44.1kHz

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| Symphonia | 84.5 ± 1.8 | 81.8 | 89.1 | 1.0 |
| FFmpeg | 129.8 ± 3.4 | 123.4 | 136.1 | 1.5 |

## Tools

Symphonia provides the following tools for debugging purposes:

* `symphonia-play` for probing files and playing back audio, as well as serving as a demo application

## Motivation

Rust makes a lot of sense for multimedia programming, particularly when accessing that media over the network. However, currently it is difficult to do something as simple as play a FLAC file. Rust does not have a library like FFMpeg, and even if one uses the FFI, FFMpeg is a difficult library to use and you'd have none of the protections afforded to you by Rust. Symphonia is therefore an attempt to fill-in that gap.

Personally, this is a project to learn Rust and experiment with signal processing.

## Authors

The primary author is Philip Deljanov.

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is an open-source project and contributions are very welcome! If you would like to make a large contribution, please raise an issue ahead of time to make sure your efforts fit into the project goals, and that there's no duplication of effort. Please be aware that all contributions must also be licensed under the MPL v2.0 license to be accepted.

When submitting a pull request, be sure you have included yourself in the CONTRIBUTORS file!
