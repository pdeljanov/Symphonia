# Symphonia

Symphonia is a pure Rust audio decoding and media demuxing library supporting AAC, OGG, FLAC, MP3, and WAV.

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

| Status    | Meaning                                                                                                                  |
|-----------|--------------------------------------------------------------------------------------------------------------------------|
| -         | No work started or planned yet.                                                                                          |
| Next      | Is the next major work item.                                                                                             |
| Viable    | Many media streams play. Some streams may panic, error, or produce audible glitches. Some features may not be supported. |
| Usable    | Most media streams play. Inaudible glitches may be present. Most common features are supported.                          |
| Compliant | All media streams play.  No audible or inaudible glitches. All required features are supported.                          |

A classification of usable indicates the end of major development. Though bugs and smaller issues can occur, it would generally be safe to use in an application. Compliance testing according to standards will be delayed until most codecs and demuxers are implemented so it's expected that many will stay in the category for a while.

### Formats (Demux)

| Format   | Status    | Feature Flag | Default | Crate                     |
|----------|-----------|--------------|---------|---------------------------|
| ISO/MP4  | Usable    | `isomp4`     | No      | `symphonia-format-isomp4` |
| MKV/WebM | -         | `mkv`        | Yes     | `symphonia-format-mkv`    |
| OGG      | Usable    | `ogg`        | Yes     | `symphonia-format-ogg`    |
| Wave     | Compliant | `wav`        | Yes     | `symphonia-format-wav`    |

### Codecs (Decode)

| Codec                        | Status    | Feature Flag | Default | Crate                     |
|------------------------------|-----------|--------------|---------|---------------------------|
| AAC-LC                       | Usable    | `aac`        | No      | `symphonia-codec-aac`     |
| HE-AAC (AAC+, aacPlus)       | -         | `aac`        | No      | `symphonia-codec-aac`     |
| HE-AACv2 (eAAC+, aacPlus v2) | -         | `aac`        | No      | `symphonia-codec-aac`     |
| FLAC                         | Compliant | `flac`       | Yes     | `symphonia-bundle-flac`   |
| MP1                          | -         | `mp3`        | No      | `symphonia-bundle-mp3`    |
| MP2                          | -         | `mp3`        | No      | `symphonia-bundle-mp3`    |
| MP3                          | Usable    | `mp3`        | No      | `symphonia-bundle-mp3`    |
| Opus                         | -         | `opus`       | Yes     | `symphonia-codec-opus`    |
| PCM                          | Compliant | `pcm`        | Yes     | `symphonia-codec-pcm`     |
| Vorbis                       | Next      | `vorbis`     | Yes     | `symphonia-codec-vorbis`  |
| WavPack                      | -         | `wavpack`    | Yes     | `symphonia-codec-wavpack` |

A `symphonia-bundle-*` package is a combination of a decoder and a native bitstream demuxer.

### Tags (Read)

All metadata readers are provided by the `symphonia-metadata` crate.

| Format                | Status    |
|-----------------------|-----------|
| APEv1                 | -         |
| APEv2                 | -         |
| ID3v1                 | Usable    |
| ID3v2                 | Usable    |
| ISO/MP4               | Usable    |
| RIFF                  | Usable    |
| Vorbis comment (FLAC) | Compliant |
| Vorbis comment (OGG)  | Compliant |

## Quality

In addition to the safety guarantees provided by Rust, Symphonia aims to:

* Decode files as well as the leading free-and-open-source software decoders
* Provide a powerful, consistent, and easy to use API
* Have absolutely no unsafe blocks
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

Please see [`symphonia-play`](https://github.com/pdeljanov/Symphonia/tree/master/symphonia-play) for a simple music player example.

## Tools

Symphonia provides the following tools for debugging purposes:

* `symphonia-play` for probing files and playing back audio, as well as serving as a demo application

## Authors

The primary author is Philip Deljanov.

## Special Thanks

* Kostya Shishkov (AAC-LC decoder contribution, see `symphonia-codec-aac`)

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is an open-source project and contributions are very welcome! If you would like to make a large contribution, please raise an issue ahead of time to make sure your efforts fit into the project goals, and that there's no duplication of effort. Please be aware that all contributions must also be licensed under the MPL v2.0 license to be accepted.

When submitting a pull request, be sure you have included yourself in the CONTRIBUTORS file!
