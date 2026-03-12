# Getting Started

Last updated for `v0.6.0`.

## Multimedia Basics

A track in a piece of media consists of a packetized (chunked) audio, video, or subtitle codec bitstream. The codec bitstream is consumed by a decoder, one packet at a time, to produce a stream of audio samples (PCM), video frames, or subtitle text. The output from the decoder can then be played back using the relevant system APIs.

Generally, one or more packetized codec bitstream is encapsulated in a multimedia container format. The container wraps the codec bitstream packets with additional information such as track ID, timestamps, duration, size, etc. This allows the container to support features such as seeking, multiple tracks, and other expected features. Additionally, a container format may also support human readable metadata such as artist, album, track title, and so on.

The process of reading a container and progressively obtaining the packets for each track's codec bitstream is called demultiplexing (demuxing). The process of decompressing the codec bitstream data encapsulated in a given packet back into a playable form is called decoding.

## Symphonia Basics

Since it is possible to encapsulate the same codec bitstream into many different container formats, Symphonia is designed with a rigid boundary between demuxing and decoding.

In Symphonia, a container demuxer is known as a format reader. All format readers implement the `symphonia::core::formats::FormatReader` trait.

Likewise, a consumer of codec bitstream packets is known as a decoder and implements a decoder trait. Decoder traits are dependent on the type of the decoder. For example, the trait for audio decoders is `symphonia::core::codecs::audio::AudioDecoder`.

Therefore, in Symphonia, the basic process of decoding a particular track involves obtaining packets from a `FormatReader` and then decoding them with a decoder.

## Basic Usage

The following basic usage guide will describe how to implement basic audio decoding. Note that most error handling is omitted for clarity.

### Enable crate features

By default, Symphonia only enables support for royalty-free or open-standard media formats (OGG, Matroska/WebM, Wave) and codecs (ADPCM, FLAC, Vorbis, PCM).

Notably, if you want MP3 support, the `mp3` feature (or `mpa` for MP1, MP2, & MP3) must be enabled. For AAC or ALAC support, enable `isomp4` (to enable the MP4/M4A container), and either `aac` or `alac` for each codec respectively.

Make sure to consult the README for the latest set of features available!

### Create a media source

Symphonia can read from any source that implements the `symphonia::core::io::MediaSource` trait.

`MediaSource` is a composite trait consisting of `std::io::Read` and `std::io::Seek`. For convenience, it is already implemented for `std::fs::File` and `std::io::cursor::Cursor`.

Given a `MediaSource`, a `symphonia::core::io::MediaSourceStream` can be created.

`MediaSourceStream` is Symphonia's supercharged equivalent to `std::io::BufReader`. It implements many enhancements to better support multimedia use-cases.

```rust
use symphonia::core::io::MediaSourceStream;

fn main() {
    // Get the first command line argument.
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("file path not provided");

    // Open the media source.
    let src = std::fs::File::open(path).expect("failed to open media");

    // Create the media source stream.
    let mss = MediaSourceStream::new(Box::new(src), Default::default());
}
```

Additional options to control buffering may be passed to `MediaSourceStream::new` in the second parameter, but the defaults are suitable for most cases.

#### Unseekable Sources

If a source cannot implement `std::io::Seek`, then `symphonia::core::io::ReadOnlySource` can be used to wrap any source that only implements `std::io::Read`.

For example, to use standard input as a `MediaSource`:

```rust
use symphonia::core::io::ReadOnlySource;

let src = ReadOnlySource::new(std::io::stdin());
```

### Detect the media format

Symphonia can automatically detect the media format of a provided source and instantiate the correct format reader. Alternately, if the specific format is known beforehand, one could just instantiate the appropriate format reader manually.

Automatic detection is the preferred option for most cases.

> [!WARNING]
> Please be aware that misdetection can occur! If you know for certain what format and codec to use, consider instantiating them manually. Enabling more format support can reduce the chances of misdetection. See [enable crate features](#enable-crate-features).

To automatically detect the media format, a `symphonia::core::probe::Probe` is used. A `Probe` takes a `MediaSourceStream` and examines it to determine what the media format is, and if the format is supported, returns the appropriate reader for the format. If metadata was encountered while probing, it will be read and made available on the format reader.

> [!TIP]
> The format probe also accepts a hint. If you know the file extension or MIME-type of the media, this may improve detection. However, it is not mandatory.

By default, a `Probe` with all enabled media formats pre-registered is provided via. `symphonia::default::get_probe()`.

```rust
use symphonia::core::formats::FormatOptions;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::probe::Hint;

fn main() {
    // Get the first command line argument.
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("file path not provided");

    // Open the media source.
    let src = std::fs::File::open(path).expect("failed to open media");

    // Create the media source stream.
    let mss = MediaSourceStream::new(Box::new(src), Default::default());

    // Create a probe hint using the file's extension. [Optional]
    let mut hint = Hint::new();
    hint.with_extension("mp3");

    // Use the default options for metadata and format readers.
    let meta_opts: MetadataOptions = Default::default();
    let fmt_opts: FormatOptions = Default::default();

    // Probe the media source.
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, fmt_opts, meta_opts)
        .expect("unsupported format");
}
```

#### Custom media formats

If you want to register one or more custom media formats for automatic detection, then it is possible to instantiate your own `Probe` and register the formats manually.

For convenience, `symphonia::default::register_enabled_formats()` may be used to register all enabled media formats into your custom `Probe`.

### Select a track

A media format may contain more than one track. For each track that you want to play, a decoder must be instantiated.

This step will vary depending on your application, though for simple cases, selecting the default track will be sufficient.

> [!WARNING]
> If playing the audio from video files is desireable, then note that the default track may *not* be an audio track. For this case, it is best to select the first supported audio track.

In the example below, the first audio track will be selected and a decoder instantiated.

```rust
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

fn main() {
    // Get the first command line argument.
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("file path not provided");

    // Open the media source.
    let src = std::fs::File::open(path).expect("failed to open media");

    // Create the media source stream.
    let mss = MediaSourceStream::new(Box::new(src), Default::default());

    // Create a probe hint using the file's extension. [Optional]
    let mut hint = Hint::new();
    hint.with_extension("mp3");

    // Use the default options for metadata and format readers.
    let meta_opts: MetadataOptions = Default::default();
    let fmt_opts: FormatOptions = Default::default();

    // Probe the media source.
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, fmt_opts, meta_opts)
        .expect("unsupported format");

    // Find the first audio track with a known (decodeable) codec.
    let track = format.default_track(TrackType::Audio).expect("no audio track");

    // Use the default options for the decoder.
    let dec_opts: AudioDecoderOptions = Default::default();

    // Create a decoder for the track.
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(
            track.codec_params.as_ref().expect("codec parameters missing").audio().unwrap(),
            &dec_opts,
        )
        .expect("unsupported codec");

    // Store the track identifier, it will be used to filter packets.
    let track_id = track.id;
}
```

> [!TIP]
> Gapless audio playback is enabled by default. It is recommended you leave it enabled. However, it may be disabled by setting `AudioDecoderOptions::enable_gapless` to `false`.

#### Custom Decoders

Much like how `Probe` is a registry of media formats, `symphonia::core::codecs::CodecRegistry` is a registry of codecs.

A registry of all enabled codecs is provided by `symphonia::default::get_codecs()`. If a custom decoder is required, then a `CodecRegistry` can be instantiated and the custom decoder registered.

For convenience, `symphonia::default::register_enabled_codecs()` may be used to register all enabled codecs into your custom `CodecRegistry`.

### Decode loop

The decode loop consists of four steps:

1. Acquiring a packet from the media format (container).
2. Consuming any new metadata.
3. Filtering the packet.
4. Decoding the packet into audio samples using its associated decoder.

These four steps are repeated until the format reader returns `None` when asked for the next packet.

With the addition of the decode loop, the example is now complete.

```rust
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

fn main() {
    // Get the first command line argument.
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("file path not provided");

    // Open the media source.
    let src = std::fs::File::open(path).expect("failed to open media");

    // Create the media source stream.
    let mss = MediaSourceStream::new(Box::new(src), Default::default());

    // Create a probe hint using the file's extension. [Optional]
    let mut hint = Hint::new();
    hint.with_extension("mp3");

    // Use the default options for metadata and format readers.
    let meta_opts: MetadataOptions = Default::default();
    let fmt_opts: FormatOptions = Default::default();

    // Probe the media source.
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, fmt_opts, meta_opts)
        .expect("unsupported format");

    // Find the first audio track with a known (decodeable) codec.
    let track = format.default_track(TrackType::Audio).expect("no audio track");

    // Use the default options for the decoder.
    let dec_opts: AudioDecoderOptions = Default::default();

    // Create a decoder for the track.
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(
            track.codec_params.as_ref().expect("codec parameters missing").audio().unwrap(),
            &dec_opts,
        )
        .expect("unsupported codec");

    // Store the track identifier, it will be used to filter packets.
    let track_id = track.id;

    // The decode loop.
    loop {
        // Get the next packet from the media format.
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => {
                // Reached the end of the stream.
                break;
            }
            Err(Error::ResetRequired) => {
                // The track list has been changed. Re-examine it and create a new set of decoders,
                // then restart the decode loop. This is an advanced feature and it is not
                // unreasonable to consider this "the end." As of v0.5.0, the only usage of this is
                // for chained OGG physical streams.
                unimplemented!();
            }
            Err(err) => {
                // A unrecoverable error occurred, halt decoding.
                panic!("{}", err);
            }
        };

        // Consume any new metadata that has been read since the last packet.
        while !format.metadata().is_latest() {
            // Pop the old head of the metadata queue.
            format.metadata().pop();

            // Consume the new metadata at the head of the metadata queue.
        }

        // If the packet does not belong to the selected track, skip over it.
        if packet.track_id() != track_id {
            continue;
        }

        // Decode the packet into audio samples.
        match decoder.decode(&packet) {
            Ok(_decoded) => {
                // Consume the decoded audio samples (see below).
            }
            Err(Error::IoError(_)) => {
                // The packet failed to decode due to an IO error, skip the packet.
                continue;
            }
            Err(Error::DecodeError(_)) => {
                // The packet failed to decode due to invalid data, skip the packet.
                continue;
            }
            Err(err) => {
                // An unrecoverable error occurred, halt decoding.
                panic!("{}", err);
            }
        }
    }
}
```

## Consuming Audio Data

After a packet is successfully decoded, a `Decoder` returns a reference to a  `symphonia::core::audio::AudioBuffer<S: Sample>`. Since an audio buffer is parameterized by the sample format, and a decoder can return an audio buffer of any sample format, `AudioDecoder::decode` actually returns the enum `symphonia::core::audio::GenericAudioBufferRef`. In Symphonia jargon, a generic audio buffer/slice is always an enum that wraps all possible buffer/slice types.

The last decoded audio buffer can also be obtained by using `AudioDecoder::last_decoded`. Note that if the last call to decode resulted in an error, then the last decoded audio buffer will be empty.

There are two ways the decoded audio data may be accessed:

1. The generic audio buffer may be used directly to copy audio data into a user-provided vector or slice in an interleaved or planar format, of a specific sample format, in raw bytes or in the in-memory representation of that sample format. It is not possible to access the audio samples directly.
2. The typed audio buffer may be used for type-safe access to the audio samples.

### Using the Generic Audio Buffer

Using the generic audio buffer is the simplest way to get audio data out of Symphonia. Various examples are provided below.

```rust
use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::audio::sample::i24;

let decoded = decoder.decode(&packet).expect("successfully decoded");

// Copy audio to a vector as interleaved f32 samples.
let mut out: Vec<f32> = Vec::new();
decoded.copy_to_vec_interleaved(&mut out);

// Copy packed audio bytes to a vector as interleaved i24 samples.
let mut out: Vec<u8> = Vec::new();
decoded.copy_bytes_to_vec_interleaved_as::<i24>(&mut out);

// Copy audio to a vector-of-vectors as planar i16 samples.
let mut out: Vec<Vec<i16>> = Vec::new();
decoded.copy_to_vecs_planar(&mut out);
```

In the examples above a vector is used as the target for the copy. However, functions to copy to an appropriately sized slice are also provided. Complementary functions for determing the correct size for those slices are also provided.

### Using the Audio Buffer

Using the typed audio buffers directly is a more advanced method that will usually involve writing generic code. However, this method provides type-safe access to the decoded audio samples.

This method requires matching on the returned `GenericAudioBufferRef` and using the methods provided by `AudioBuffer`.

#### Background

`AudioBuffer` implements 4 core audio traits providing different means of accessing audio data. It is recommended you familiarize yourself with these traits.

1. The `Audio` trait is the primary trait for immutably interacting with an audio. Along with providing basic information such as the number of planes and frames, this trait implements:
    * Iteration of, and access to, individual audio planes
    * Type-safe access to audio samples
    * Slicing
    * Copying audio samples to vectors or user-provided slices in interleaved or planar format with sample format conversion
2. The `AudioMut` trait is the mutable complement to `Audio`. Since a decoder yields an immutable reference to the decoded audio buffer, it is largely irrelevant for decoding use-cases.
3. The `AudioBytes` trait provides methods to copy the audio out of the audio buffer in interleaved or planar format, with sample format conversion, as bytes.
4. The `AudioBufferBytes` trait provides functions to obtain the maximum capacity of an `AudioBuffer` in bytes.

When an `AudioBuffer` is sliced, it returns an `AudioSlice`. `AudioSlice` implements both the `Audio` and `AudioBytes` traits. Thus, an `AudioBuffer` or `AudioSlice` may be operated on in the same manner.

This comprehensive functionality means it may also be possible to use Symphonia's audio primitives as the foundation for an application's entire audio pipeline.

#### Example: Iterate over all samples in a plane

To iterate over all samples in the left channel (plane) position of the audio buffer:

```rust
use symphonia::core::audio::{Audio, GenericAudioBufferRef};

let decoded = decoder.decode(&packet).expect("successfully decoded");

match decoded {
    AudioBufferRef::F32(buf) => {
        for &sample in buf.plane_by_position(Position::FRONT_LEFT).expect("a left channel") {
            // Do something with each audio `sample`.
        }
    }
    _ => {
        // Repeat for the different sample formats.
        unimplemented!()
    }
}
```

> [!WARNING]
> Make sure to handle cases where channels may not exist.

#### Example: Iterate over all samples in all planes

Another useful access pattern is viewing the audio buffer as a slice-of-slices wherein each slice is a complete audio plane (channel):

```rust
use symphonia::core::audio::{Audio, GenericAudioBufferRef};

match decoded {
    AudioBufferRef::F32(buf) => {
        for plane in buf.iter_planes() {
            for &sample in plane {
                // Do something with each audio `sample`.
            }
        }
    }
    _ => {
        // Repeat for the different sample formats.
        unimplemented!()
    }
}
```

## Consuming Metadata

When creating a format reader, and then while demuxing, metadata may be encountered by the reader. Each time a format reader encounters new metadata, it creates a metadata revision and then queues it for consumption. The user should frequently check this queue and pop old revisions of the metadata off the queue.

> [!WARNING]
> While this may vary based on the application, a newer revision should not fully replace a previous revision. For example, assume two metadata revisions containing the following tags:
>
> 1. `TrackTitle = "Title0"; AlbumArtist = "Artist"; Album = "Album";`
> 2. `TrackTitle = "Title1";`
>
> If both revisions are consumed, the final metadata should be:
>
> * `TrackTitle = "Title1"; AlbumArtist = "Artist"; Album = "Album";`
>
> In other words, a revision should generally be viewed as an "upsert" (update or insert) operation.
>
> An exception to this general rule is when the reader returns a `ResetRequired` error.

In the example shown above, metadata is consumed in an inner loop within the main decode loop:

```rust
// While there is newer metadata.
while !format.metadata().is_latest() {
    // Pop the old head of the metadata queue.
    format.metadata().pop();

    if let Some(rev) = format.metadata().current() {
        // Consume the new metadata at the head of the metadata queue.
    }
}
```
