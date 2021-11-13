# Symphonia

[![Docs](https://docs.rs/symphonia/badge.svg)](https://docs.rs/symphonia)
[![Build Status](https://github.com/pdeljanov/Symphonia/actions/workflows/ci.yml/badge.svg)](https://github.com/pdeljanov/Symphonia/actions/workflows/ci.yml)

Symphonia is a pure Rust audio decoding and media demuxing library supporting AAC, ALAC, FLAC, MP3, MP4, OGG, Vorbis, and WAV.

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

Support for individual audio codecs and media formats is provided by separate crates. By default, Symphonia selects support for FOSS codecs and formats, but others may be included via the features option.

The follow status classifications are used to determine the state of development for each format or codec.

| Status  | Meaning                                                                                                                  |
|---------|--------------------------------------------------------------------------------------------------------------------------|
| -       | No work started or planned yet.                                                                                          |
| Next    | Is the next major work item.                                                                                             |
| Good    | Many media streams play. Some streams may panic, error, or produce audible glitches. Some features may not be supported. |
| Great   | Most media streams play. Inaudible glitches may be present. Most common features are supported.                          |
| Perfect | All media streams play.  No audible or inaudible glitches. All required features are supported.                          |

A classification of Great indicates the end of major development. Though bugs and smaller issues can occur, it would generally be safe to use in an application. Compliance testing according to standards will be delayed until most codecs and demuxers are implemented so it's expected that many will stay in the category for a while.

### Formats (Demux)

| Format   | Status  | Feature Flag | Default | Crate                       |
|----------|---------|--------------|---------|-----------------------------|
| ISO/MP4  | Great   | `isomp4`     | No      | [`symphonia-format-isomp4`] |
| MKV/WebM | -       | `mkv`        | Yes     | `symphonia-format-mkv`      |
| OGG      | Great   | `ogg`        | Yes     | [`symphonia-format-ogg`]    |
| Wave     | Perfect | `wav`        | Yes     | [`symphonia-format-wav`]    |

[`symphonia-format-isomp4`]: https://docs.rs/symphonia-format-isomp4
[`symphonia-format-ogg`]: https://docs.rs/symphonia-format-ogg
[`symphonia-format-wav`]: https://docs.rs/symphonia-format-wav

### Codecs (Decode)

| Codec                        | Status  | Feature Flag | Default | Crate                      |
|------------------------------|---------|--------------|---------|----------------------------|
| AAC-LC                       | Good    | `aac`        | No      | [`symphonia-codec-aac`]    |
| ALAC                         | Great   | `alac`       | No      | [`symphonia-codec-alac`]   |
| HE-AAC (AAC+, aacPlus)       | -       | `aac`        | No      | [`symphonia-codec-aac`]    |
| HE-AACv2 (eAAC+, aacPlus v2) | -       | `aac`        | No      | [`symphonia-codec-aac`]    |
| FLAC                         | Perfect | `flac`       | Yes     | [`symphonia-bundle-flac`]  |
| MP1                          | -       | `mp3`        | No      | [`symphonia-bundle-mp3`]   |
| MP2                          | -       | `mp3`        | No      | [`symphonia-bundle-mp3`]   |
| MP3                          | Great   | `mp3`        | No      | [`symphonia-bundle-mp3`]   |
| Opus                         | Next    | `opus`       | Yes     | `symphonia-codec-opus`     |
| PCM                          | Perfect | `pcm`        | Yes     | [`symphonia-codec-pcm`]    |
| Vorbis                       | Great   | `vorbis`     | Yes     | [`symphonia-codec-vorbis`] |
| WavPack                      | -       | `wavpack`    | Yes     | `symphonia-codec-wavpack`  |

A `symphonia-bundle-*` package is a combination of a decoder and a native bitstream demuxer.

[`symphonia-codec-aac`]: https://docs.rs/symphonia-codec-aac
[`symphonia-codec-alac`]: https://docs.rs/symphonia-codec-alac
[`symphonia-bundle-flac`]: https://docs.rs/symphonia-bundle-flac
[`symphonia-bundle-mp3`]: https://docs.rs/symphonia-bundle-mp3
[`symphonia-codec-pcm`]: https://docs.rs/symphonia-codec-pcm
[`symphonia-codec-vorbis`]: https://docs.rs/symphonia-codec-vorbis

### Tags (Read)

All metadata readers are provided by the `symphonia-metadata` crate.

| Format                | Status    |
|-----------------------|-----------|
| ID3v1                 | Great     |
| ID3v2                 | Great     |
| ISO/MP4               | Great     |
| RIFF                  | Great     |
| Vorbis comment (FLAC) | Perfect   |
| Vorbis comment (OGG)  | Perfect   |

## Quality

In addition to the safety guarantees provided by Rust, Symphonia aims to:

* Decode files as well as the leading free-and-open-source software decoders
* Provide a powerful, consistent, and easy to use API
* Be 100% safe code
* Have very minimal dependencies
* Prevent denial-of-service attacks
* Be fuzz-tested

## Performance

Symphonia aims to be equivalent in speed to popular open-source C-based implementations.

Symphonia does not include explicit SIMD optimizations, however the auto-vectorizer is leveraged as much as possible and the results have been *excellent*. As Rust support for packed SIMD grows, Symphonia will include explicit SIMD optimizations where necessary.

### Benchmarks (as of September 2019)

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

## Example Usage

Basic usage examples may be found in [`symphonia/examples`](https://github.com/pdeljanov/Symphonia/tree/master/symphonia/examples).

For a more complete application, see [`symphonia-play`](https://github.com/pdeljanov/Symphonia/tree/master/symphonia-play), a simple music player.

## Tools

Symphonia provides the following tools for debugging purposes:

* [`symphonia-play`](https://github.com/pdeljanov/Symphonia/tree/master/symphonia-play) for probing, decoding, validating, and playing back media streams.
* [`symphonia-check`](https://github.com/pdeljanov/Symphonia/tree/master/symphonia-check) for validating Symphonia's decoded output against `ffmpeg`.

## Authors

The primary author is Philip Deljanov.

## Special Thanks

* Kostya Shishkov (AAC-LC decoder contribution, see `symphonia-codec-aac`)

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is an open-source project and contributions are very welcome! If you would like to make a large contribution, please raise an issue ahead of time to make sure your efforts fit into the project goals, and that there's no duplication of effort. Please be aware that all contributions must also be licensed under the MPL v2.0 license to be accepted.

When submitting a pull request, be sure you have included yourself in the CONTRIBUTORS file!
