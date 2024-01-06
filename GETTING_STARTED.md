# Getting Started

Last updated for `v0.5.0`.

## Multimedia Basics

A track of audio consists of a packetized (chunked) codec bitstream. The codec bitstream is consumed by a decoder one packet at a time to produce a stream of audio samples (PCM). The audio samples can then be played back by the audio hardware.

Generally, the packetized codec bitstream is encapsulated in a multimedia container format. The container wraps the codec bitstream packets with additional information such as track ID, timestamps, duration, size, etc. This allows the container to support features such as seeking, multiple tracks, and other expected features. Additionally, a container format may also support human readable metadata such as artist, album, track title, and so on.

The process of reading a container and progressively obtaining the packets of each track's codec bitstream is called demultiplexing (demuxing). The process of converting the codec bitstream data encapsulated in a given packet back into audio samples is called decoding.

## Symphonia Basics

Since it is possible to encapsulate the same codec bitstream into many different container formats, Symphonia creates a hard boundary between demuxing and decoding.

In Symphonia, a container format reader is known as a format reader. All format readers implement the `symphonia::core::formats::FormatReader` trait.

Likewise, a consumer of codec bitstream packets is known as a decoder and implements the `symphonia::core::codecs::Decoder` trait.

Therefore, in Symphonia, the basic process of decoding a particular audio track involves obtaining packets from a `FormatReader` and then decoding them with a `Decoder`.

## Basic Usage

### Enable crate features

By default, Symphonia only enables support for royalty-free or open-standard media formats (OGG, Matroska/WebM, Wave) and codecs (FLAC, Vorbis, PCM).

Notably, if you want MP3 support, the `mp3` feature must be enabled. For AAC or ALAC support, enable `isomp4` (to enable the MP4/M4A container), and either `aac` or `alac` for each codec respectively.

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
    let src = std::fs::File::open(&path).expect("failed to open media");

    // Create the media source stream.
    let mss = MediaSourceStream::new(Box::new(src), Default::default());
}
```

Additional options to control buffering may be passed to `MediaSourceStream::new` in the second parameter, but the defaults are suitable for most cases.

#### Unseekable Sources

If a source cannot implement `std::io::Seek`, then `symphonia::core::io::ReadOnlySource` can be used to wrap any source that implements `std::io::Read`.

For example, to use standard input as a `MediaSource`:

```rust
use symphonia::core::io::ReadOnlySource;

let src = ReadOnlySource::new(std::io::stdin());
```

### Detect the media format

Symphonia can automatically detect the media format of a provided source and instantiate the correct format reader. Alternately, if the specific format is known beforehand, one could just instantiate the appropriate format reader manually.

Automatic detection is the preferred option for most cases.

> :warning: Please be aware that misdetection can occur! If you know for certain what format and codec to use, consider instantiating them manually. Enabling more format support can reduce the chances of misdetection. See [enable crate features](#enable-crate-features).

To automatically detect the media format, a `symphonia::core::probe::Probe` is used. A `Probe` takes a `MediaSourceStream` and examines it to determine what the media format is, and if the format is supported, returns the appropriate reader for the format. If metadata was encountered while probing, it will also be returned with the reader.

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
    let src = std::fs::File::open(&path).expect("failed to open media");

    // Create the media source stream.
    let mss = MediaSourceStream::new(Box::new(src), Default::default());

    // Create a probe hint using the file's extension. [Optional]
    let mut hint = Hint::new();
    hint.with_extension("mp3");

    // Use the default options for metadata and format readers.
    let meta_opts: MetadataOptions = Default::default();
    let fmt_opts: FormatOptions = Default::default();

    // Probe the media source.
    let probed = symphonia::default::get_probe().format(&hint, mss, &fmt_opts, &meta_opts)
                                                .expect("unsupported format");
}
```

#### Gapless playback

If gapless playback is desired, and generally it is, set `FormatOptions::enable_gapless` to `true`.

#### Custom media formats

If you want to register one or more custom media formats for automatic detection, then it is possible to instantiate your own `Probe` and register the formats manually.

For convenience, `symphonia::default::register_enabled_formats()` may be used to register all enabled media formats into your custom `Probe`.

### Select a track

A media format may contain more than one track. For each track that you want to play, a decoder must be instantiated.

This step will vary depending on your application, though for simple cases, selecting the default track will be sufficient.

> :warning: If playing the audio from video files is desireable, then note that the default track may *not* be an audio track. For this case, it is best to select the first supported audio track.

In the example below, the first audio track will be selected and a decoder instantiated.

```rust
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::formats::FormatOptions;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::probe::Hint;

fn main() {
    // Get the first command line argument.
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("file path not provided");

    // Open the media source.
    let src = std::fs::File::open(&path).expect("failed to open media");

    // Create the media source stream.
    let mss = MediaSourceStream::new(Box::new(src), Default::default());

    // Create a probe hint using the file's extension. [Optional]
    let mut hint = Hint::new();
    hint.with_extension("mp3");

    // Use the default options for metadata and format readers.
    let meta_opts: MetadataOptions = Default::default();
    let fmt_opts: FormatOptions = Default::default();

    // Probe the media source.
    let probed = symphonia::default::get_probe().format(&hint, mss, &fmt_opts, &meta_opts)
                                                .expect("unsupported format");

    // Get the instantiated format reader.
    let format = probed.format;

    // Find the first audio track with a known (decodeable) codec.
    let track = format.tracks()
                    .iter()
                    .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
                    .expect("no supported audio tracks");

    // Use the default options for the decoder.
    let dec_opts: DecoderOptions = Default::default();

    // Create a decoder for the track.
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &dec_opts)
                                                    .expect("unsupported codec");

    // Store the track identifier, it will be used to filter packets.
    let track_id = track.id;
}
```

#### Custom Decoders

Much like how `Probe` is a registry of media formats, `symphonia_core::codecs::CodecRegistry` is a registry of codecs.

A registry of all enabled codecs is provided by `symphonia::default::get_codecs()`. If a custom decoder is required, then a `CodecRegistry` can be instantiated and the custom decoder registered.

For convenience, `symphonia::default::register_enabled_codecs()` may be used to register all enabled codecs into your custom `CodecRegistry`.

### Decode loop

The decode loop consists of four steps:

1. Acquiring a packet from the media format (container).
2. Consuming any new metadata.
3. Filtering the packet.
4. Decoding the packet into audio samples using its associated decoder.

With the addition of the decode loop, the example is now complete.

```rust
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatOptions;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::probe::Hint;

fn main() {
    // Get the first command line argument.
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("file path not provided");

    // Open the media source.
    let src = std::fs::File::open(&path).expect("failed to open media");

    // Create the media source stream.
    let mss = MediaSourceStream::new(Box::new(src), Default::default());

    // Create a probe hint using the file's extension. [Optional]
    let mut hint = Hint::new();
    hint.with_extension("mp3");

    // Use the default options for metadata and format readers.
    let meta_opts: MetadataOptions = Default::default();
    let fmt_opts: FormatOptions = Default::default();

    // Probe the media source.
    let probed = symphonia::default::get_probe().format(&hint, mss, &fmt_opts, &meta_opts)
                                                .expect("unsupported format");

    // Get the instantiated format reader.
    let mut format = probed.format;

    // Find the first audio track with a known (decodeable) codec.
    let track = format.tracks()
                    .iter()
                    .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
                    .expect("no supported audio tracks");

    // Use the default options for the decoder.
    let dec_opts: DecoderOptions = Default::default();

    // Create a decoder for the track.
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &dec_opts)
                                                    .expect("unsupported codec");

    // Store the track identifier, it will be used to filter packets.
    let track_id = track.id;

    // The decode loop.
    loop {
        // Get the next packet from the media format.
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(Error::ResetRequired) => {
                // The track list has been changed. Re-examine it and create a new set of decoders,
                // then restart the decode loop. This is an advanced feature and it is not
                // unreasonable to consider this "the end." As of v0.5.0, the only usage of this is
                // for chained OGG physical streams.
                unimplemented!();
            }
            Err(err) => {
                // A unrecoverable error occured, halt decoding.
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
                // An unrecoverable error occured, halt decoding.
                panic!("{}", err);
            }
        }
    }
}
```

> :warning: This example will always panic when it reaches the end of the file because an end-of-stream IO error signals the end of the file. Therefore, proper error handling is required.

## Consuming Audio Data

After a packet is successfully decoded, a `Decoder` returns a reference to a copy-on-write `symphonia::core::audio::AudioBuffer<S: Sample>`. Since an audio buffer is parameterized by the sample format, and a decoder can return an audio buffer of any sample format, `Decoder::decode` actually returns the enum `symphonia::core::audio::AudioBufferRef`.

### Accessing audio samples directly

Samples can be directly accessed by matching on the returned `AudioBufferRef` and using the methods provided by `AudioBuffer`.

For example, to iterate over all samples in the first channel (plane) of the audio buffer:

```rust
use symphonia_core::audio::{AudioBufferRef, Signal};

let decoded = decoder.decode(&packet).unwrap();

match decoded {
    AudioBufferRef::F32(buf) => {
        for &sample in buf.chan(0) {
            // Do something with `sample`.
        }
    }
    _ => {
        // Repeat for the different sample formats.
        unimplemented!()
    }
}
```

The last decoded audio buffer can also be obtained by using `Decoder::last_decoded`.

> :warning: If the last call to decode resulted in an error, then the last decoded audio buffer will have a length of 0.

Another useful access pattern is viewing the audio buffer as a slice-of-slices wherein each slice is a complete audio plane (channel):

```rust
use symphonia_core::audio::{AudioBufferRef, Signal};

match decoded {
    AudioBufferRef::F32(buf) => {
        let planes = buf.planes();

        for plane in planes.planes() {
            for &sample in plane.iter() {
                // Do something with `sample`.
            }
        }
    }
    _ => {
        // Repeat for the different sample formats.
        unimplemented!()
    }
}
```

### Converting to an interleaved sample buffer

If the audio samples are required to be of a certain sample format and interleaved, then a `symphonia::core::audio::SampleBuffer` can be used to perform the conversion.

```rust
use symphonia_core::audio::SampleBuffer;

// Create a sample buffer that matches the parameters of the decoded audio buffer.
let mut sample_buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());

// Copy the contents of the decoded audio buffer into the sample buffer whilst performing
// any required conversions.
sample_buf.copy_interleaved_ref(decoded);

// The interleaved f32 samples can be accessed as follows.
let samples = sample_buf.samples();
```

A `SampleBuffer` should be reused for the life of the decoder. The only exception where the `SampleBuffer` must be recreated is if `Decoder::decode` returns `Error::ResetRequired`.

### Converting to a byte-oriented interleaved sample buffer

If the samples must be returned as a slice of bytes, then a `symphonia::core::audio::RawSampleBuffer` can be used to perform the conversion. A `RawSampleBuffer` is effectively the same as a `SampleBuffer` except the samples can be accessed as a slice of bytes.

```rust
use symphonia_core::audio::RawSampleBuffer;

// Create a raw sample buffer that matches the parameters of the decoded audio buffer.
let mut byte_buf = RawSampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());

// Copy the contents of the decoded audio buffer into the sample buffer whilst performing
// any required conversions.
byte_buf.copy_interleaved_ref(decoded);

// The interleaved f32 samples can be accessed as a slice of bytes as follows.
let bytes = byte_buf.as_bytes();
```

Just like a `SampleBuffer`, a `RawSampleBuffer` should be reused for the life of the decoder. The only exception where the `RawSampleBuffer` must be recreated is if `Decoder::decode` returns `Error::ResetRequired`.

### Converting to a planar sample buffer

If the samples in the `SampleBuffer` should be in a planar format, then use `SampleBuffer::copy_planar_ref`.

### Converting to a byte-oriented planar sample buffer

If the samples in the `RawSampleBuffer` should be in a planar format, then use `RawSampleBuffer::copy_planar_ref`.

## Consuming Metadata

When creating a format reader, and then while demuxing, metadata may be encountered by the reader. Each time a format reader encounters new metadata, it creates a metadata revision and then queues it for consumption. The user should frequently check this queue and pop old revisions of the metadata off the queue.

> :warning: While this may vary based on the application, a newer revision should not fully replace a previous revision. For example, assume two metadata revisions containing the following tags:
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

### Probed Metadata

Metadata may also be encountered while probing before the container begins. For example, ID3v2 tags are appended to the start of a MP3 file before the actual format data. In this case, rather than getting the metadata from the format reader, the probe result will contain the metadata. It can be obtained via the `metadata` field of `ProbeResult`.
