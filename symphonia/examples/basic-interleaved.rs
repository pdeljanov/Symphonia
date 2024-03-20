use std::env;
use std::fs::File;
use std::path::Path;

use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::sample::Sample;

fn main() {
    // Get command line arguments.
    let args: Vec<String> = env::args().collect();

    // Create a media source. Note that the MediaSource trait is automatically implemented for File,
    // among other types.
    let file = Box::new(File::open(Path::new(&args[1])).unwrap());

    // Create the media source stream using the boxed media source from above.
    let mss = MediaSourceStream::new(file, Default::default());

    // Create a hint to help the format registry guess what format reader is appropriate. In this
    // example we'll leave it empty.
    let hint = Hint::new();

    // Use the default options when reading and decoding.
    let format_opts: FormatOptions = Default::default();
    let metadata_opts: MetadataOptions = Default::default();
    let decoder_opts: DecoderOptions = Default::default();

    // Probe the media source stream for a format.
    let mut format =
        symphonia::default::get_probe().format(&hint, mss, format_opts, metadata_opts).unwrap();

    // Get the default track.
    let track = format.default_track().unwrap();

    // Create a decoder for the track.
    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &decoder_opts).unwrap();

    // Store the track identifier, we'll use it to filter packets.
    let track_id = track.id;

    let mut samples: Vec<f32> = Default::default();
    let mut total_sample_count = 0;

    // Read and decode all packets from the format reader.
    while let Some(packet) = format.next_packet().unwrap() {
        // If the packet does not belong to the selected track, skip it.
        if packet.track_id() != track_id {
            continue;
        }

        // Decode the packet into audio samples, ignoring any decode errors.
        match decoder.decode(&packet) {
            Ok(audio_buf) => {
                // The decoded audio samples may now be accessed via the generic audio buffer
                // returned by the decoder. You may match on the buffer to access a sample-format
                // specific buffer, or use generic routines to copy out the audio samples in the
                // desired sample format.
                //
                // In the example below, we will copy the all the samples into a vector in
                // the f32 sample format in channel interleaved order.

                // Ensure the vector is large enough to hold all the samples.
                samples.resize(audio_buf.samples_interleaved(), f32::MID);

                // Copy the audio samples from the generic audio buffer to the vector in interleaved
                // order. The sample format to convert to is inferred from the type of the Vec.
                audio_buf.copy_to_slice_interleaved(&mut samples);

                // Sum up the total number of samples.
                total_sample_count += samples.len();
                print!("\rDecoded {} samples", total_sample_count);
            }
            Err(Error::DecodeError(_)) => (),
            Err(_) => break,
        }
    }
}
