# Symphonia

[![Docs](https://docs.rs/symphonia/badge.svg)](https://docs.rs/symphonia)
[![Build Status](https://github.com/pdeljanov/Symphonia/actions/workflows/ci.yml/badge.svg)](https://github.com/pdeljanov/Symphonia/actions/workflows/ci.yml)
[![dependency status](https://deps.rs/repo/github/pdeljanov/symphonia/status.svg)](https://deps.rs/repo/github/pdeljanov/symphonia)

Symphonia is a pure Rust audio decoding and media demuxing library supporting AAC, ALAC, FLAC, MP3, MP4, OGG, Vorbis, and WAV.

## Features

* Decode support for the most popular audio codecs
* Demux the most common media container formats
* Read most metadata and tagging formats
* Automatic format and decoder detection
* Provides a set of basic audio primitives for manipulating audio data efficiently
* 100% safe Rust
* Minimal dependencies
* Fast with no compromises in performance!

Additionally, planned features include:

* Providing a C API for integration into other languages
* Providing a WASM API for web usage

## Format and Codec Support Roadmap

Support for individual audio codecs and media formats is provided by separate crates. By default, Symphonia enables support for FOSS codecs and formats, but others may be enabled via the features option.

The follow status classifications are used to determine the state of development for each format or codec.

| Status    | Meaning                                                                                                                  |
|-----------|--------------------------------------------------------------------------------------------------------------------------|
| -         | No work started or planned yet.                                                                                          |
| In Work   | Is in work or will be started next.                                                                                      |
| Good      | Many media streams play. Some streams may panic, error, or produce audible glitches. Some features may not be supported. |
| Great     | Most media streams play. Inaudible glitches may be present. Most common features are supported.                          |
| Excellent | All media streams play.  No audible or inaudible glitches. All required features are supported.                          |

A status of *great* indicates that major development is complete and that the feature is in a state that would be acceptable for most applications to use. A status of *excellent* is only assigned after the feature passes all compliance tests. If no compliance tests are freely available, then a status of *excellent* will be assigned if Symphonia's implementation matches the quality of a reference implementation, or `ffmpeg`.

### Formats (Demuxers)

| Format   | Status    | Feature Flag | Default | Crate                       |
|----------|-----------|--------------|---------|-----------------------------|
| ISO/MP4  | Great     | `isomp4`     | No      | [`symphonia-format-isomp4`] |
| MKV/WebM | In Work   | none yet     | Yes     | `symphonia-format-mkv`      |
| OGG      | Great     | `ogg`        | Yes     | [`symphonia-format-ogg`]    |
| Wave     | Excellent | `wav`        | Yes     | [`symphonia-format-wav`]    |

[`symphonia-format-isomp4`]: https://docs.rs/symphonia-format-isomp4
[`symphonia-format-ogg`]: https://docs.rs/symphonia-format-ogg
[`symphonia-format-wav`]: https://docs.rs/symphonia-format-wav

### Codecs (Decoder)

| Codec                        | Status    | Feature Flag | Default | Crate                      |
|------------------------------|-----------|--------------|---------|----------------------------|
| AAC-LC                       | Good      | `aac`        | No      | [`symphonia-codec-aac`]    |
| ALAC                         | Great     | `alac`       | No      | [`symphonia-codec-alac`]   |
| HE-AAC (AAC+, aacPlus)       | -         | `aac`        | No      | [`symphonia-codec-aac`]    |
| HE-AACv2 (eAAC+, aacPlus v2) | -         | `aac`        | No      | [`symphonia-codec-aac`]    |
| FLAC                         | Excellent | `flac`       | Yes     | [`symphonia-bundle-flac`]  |
| MP1                          | -         | `mp3`        | No      | [`symphonia-bundle-mp3`]   |
| MP2                          | -         | `mp3`        | No      | [`symphonia-bundle-mp3`]   |
| MP3                          | Excellent | `mp3`        | No      | [`symphonia-bundle-mp3`]   |
| Opus                         | In Work   | none yet     | Yes     | `symphonia-codec-opus`     |
| PCM                          | Excellent | `pcm`        | Yes     | [`symphonia-codec-pcm`]    |
| Vorbis                       | Great     | `vorbis`     | Yes     | [`symphonia-codec-vorbis`] |
| WavPack                      | -         | `wavpack`    | Yes     | `symphonia-codec-wavpack`  |

A `symphonia-bundle-*` package is a combination of a decoder and a native bitstream demuxer.

[`symphonia-codec-aac`]: https://docs.rs/symphonia-codec-aac
[`symphonia-codec-alac`]: https://docs.rs/symphonia-codec-alac
[`symphonia-bundle-flac`]: https://docs.rs/symphonia-bundle-flac
[`symphonia-bundle-mp3`]: https://docs.rs/symphonia-bundle-mp3
[`symphonia-codec-pcm`]: https://docs.rs/symphonia-codec-pcm
[`symphonia-codec-vorbis`]: https://docs.rs/symphonia-codec-vorbis

### Tags (Readers)

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

In addition to the safety guarantees afforded by Rust, Symphonia aims to:

* Decode media as correctly as the leading free-and-open-source software decoders
* Prevent denial-of-service attacks
* Be fuzz-tested
* Provide a powerful, consistent, and easy to use API

## Performance

Symphonia aims to be comparable or better in performance to popular open-source C-based implementations. Currently, Symphonia's decoders are generally +/-15% the performance of `ffmpeg`. The exact amount will depend strongly on the codec, and which features of the codec are leveraged in the encoding.

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
