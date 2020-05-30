# Symphonia

Symphonia is a pure Rust audio decoding and media demuxing library aiming to support the most popular formats and codecs.

## Current Support

Support for individual audio codecs and media formats is provided by separate crates. By default, Symphonia selects
support for FOSS codecs and formats, but others may be included via the features option.

### Formats

| Format  | Feature Flag | Default | Crate                     |  
|---------|--------------|---------|---------------------------|
| OGG     | `ogg`        | Yes     | `symphonia-format-ogg`    |
| Wave    | `wav`        | Yes     | `symphonia-format-wav`    |

Planned future format support include: MKV, WebM, and ISO/MP4.

### Codecs

| Codec    | Feature Flag | Default | Crate                     |
|----------|--------------|---------|---------------------------|
| FLAC     | `flac`       | Yes     | `symphonia-codec-flac`    |
| MP3      | `mp3`        | No      | `symphonia-codec-mp3`     |
| PCM      | `pcm`        | Yes     | `symphonia-codec-pcm`     |

Planned future decoders include: Opus, Vorbis, AAC, and WavPack.

### Metadata

Metadata readers are provided by the `symphonia-metadata` crate and supports:

* ID3v1
* ID3v2
* Vorbis Comment
* FLAC Metadata Block
* RIFF Info Block

## Features

Symphonia's features are:

* Decode support for the most popular audio codecs
* Reading the most common media container formats
* Probing and guessing the correct format and decoder combination(s) for playback or inspection
* Reading metadata
* Providing a set of audio primitives for manipulating audio data efficiently

## Usage

Check out [Symphonia Play](https://github.com/pdeljanov/symphonia/tree/master/symphonia-play), a small music-player showcasing the basic usage of Symphonia.

## Authors

The primary author is Philip Deljanov. All additional contributors are credited within the CONTRIBUTORS file.

## License

Symphonia is provided under the MPL v2.0 license. Please refer to the LICENSE file for more details.

## Contributing

Symphonia is an open-source project and contributions are very welcome! If you would like to make a large contribution, please raise an issue ahead of time to make sure your efforts fit into the project goals, and that no duplication of efforts occurs.

When submitting a pull request, be sure you have included yourself in the CONTRIBUTORS file!
