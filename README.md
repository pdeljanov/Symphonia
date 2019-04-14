# Sonata

Sonata is a pure Rust audio decoding and multimedia format library.

## Features

Sonata's planned features are:

 * Decode support for the most popular audio codecs
 * Reading and writing the most common media container formats
 * Probing and guessing the correct format and decoder combination(s) for playback or inspection
 * Reading and writing metadata
 * Providing a set of audio primitives for manipulating audio data
 * Providing a C API for integration into other languages

## Format and Codec Support Roadmap

Support for individual audio codecs and media formats is provided by separate crates. By default, Sonata selects
support for FOSS codecs and formats, but others may be included via the features option.

Up-to-date project progress on the [Trello Board](https://trello.com/b/2EhGvDsR).

### Formats (Mux/Demux)

| Format       | Status   | Feature Flag | Default | Crate                  |  
|--------------|----------|--------------|---------|------------------------|
| ISO/MP4      | Planning | `isomp4`     | No      | `sonata-format-isomp4` |
| MKV          | -        | `mkv`        | Yes     | `sonata-format-mkv`    |
| OGG          | -        | `ogg`        | Yes     | `sonata-format-ogg`    |
| Wave/AIFF    | In Work  | `wav`        | Yes     | `sonata-format-wav`    |
| WebM         | -        | `webm`       | No      | `sonata-format-webm`   |

### Codecs (Decode)

| Codec    | Status      | Feature Flag | Default | Crate                  |
|----------|-------------|--------------|---------|------------------------|
| AAC      | -           | `aac`        | No      | `sonata-codec-aac`     |
| Flac     | Functional  | `flac`       | Yes     | `sonata-codec-flac`    |
| Hardware | -           | `hwdec`      | No      | `sonata-codec-hwdec`   |
| MP3      | Planning    | `mp3`        | No      | `sonata-codec-mp3`     |
| Opus     | -           | `opus`       | Yes     | `sonata-codec-opus`    |
| PCM      | Functional  | `pcm`        | Yes     | `sonata-codec-pcm`     |
| Vorbis   | -           | `vorbis`     | Yes     | `sonata-codec-vorbis`  |
| WavPack  | -           | `wavpack`    | Yes     | `sonata-codec-wavpack` |

### Codecs (Encode)

Sonata does not aim to provide Rust-based encoders for codecs. This is because most encoders have undergone years of development, tweaking, and optimization. Replicating this work would be difficult and provide little benefit for safety because the input to an encoder is controlled by the developer unlike a decoder or demuxer.

Sonata plans to provide "unsafe" encoder packages that wrap traditional C-based encoders.

| Codec    | Status      | Feature Flag | Default | Crate                           |
|----------|-------------|--------------|---------|---------------------------------|
| Flac     | -           | `libflac`    | No      | `sonata-unsafe-codec-libflac`   |
| Hardware | -           | `hwenc`      | No      | `sonata-codec-hwenc`            |
| Opus     | -           | `libopus`    | No      | `sonata-unsafe-codec-libopus`   |
| Vorbis   | -           | `libvorbis`  | No      | `sonata-unsafe-codec-libvorbis` |

## Quality

In addition to the safety guarantees provided by Rust, Sonata aims to:

 * Decode files identically to the leading free-and-open-source software decoder
 * Provide a powerful, consistent, and easy to use API
 * Prevent denial-of-service attacks
 * Be fuzz-tested

## Speed

Sonata aims to be equivalent in speed to C-based implementations. As Rust support for SIMD grows, Sonata will include SIMD optimizations where possible. However, safety is the number one priority.

## Tools

Sonata provides the following tools for debugging purposes:

 * `sonata-play` for playing back audio from files

## Why?

Rust makes a lot of sense for multimedia programming, particularly when accessing that media over the network. However, currently it is difficult to do something as simple as play a FLAC file. Rust does not have a library like FFMpeg, and even if one uses the FFI, FFMpeg is a difficult library to use and you'd have none of the protections afforded to you by Rust. Sonata is therefore an attempt to fill-in that gap. 

Personally, this is a project to learn Rust, and experiment with signal processing.

## Authors

The primary author is Philip Deljanov.

## License

Sonata is provided under the LGPLv2.1 license. Please refer to the LICENSE file for more details.

## Contributing

Sonata is an open-source project and contributions are very welcome! If you would like to make a large contribution, please raise an issue ahead of time to make sure your efforts fit into the project goals, and that there's duplication of effort.

All contributors will be credited within the CONTRIBUTORS file.
