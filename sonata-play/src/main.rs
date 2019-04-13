// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This library is free software; you can redistribute it and/or
// modify it under the terms of the GNU Lesser General Public
// License as published by the Free Software Foundation; either
// version 2.1 of the License, or (at your option) any later version.
//
// This library is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
// Lesser General Public License for more details.
//
// You should have received a copy of the GNU Lesser General Public
// License along with this library; if not, write to the Free Software
// Foundation, Inc., 51 Franklin Street, Fifth Floor, Boston, MA  02110-1301 USA

use std::default::Default;
use std::fs::File;
use std::path::Path;
use clap::{Arg, App};
use sonata_core::errors::{Result, unsupported_error};
use sonata_core::audio::*;
use sonata_core::codecs::{CodecRegistry, DecoderOptions};
use sonata_core::formats::{FormatRegistry, Hint, FormatOptions};
use sonata_codec_flac::*;
use sonata_codec_pcm::*;
use sonata_format_wav::*;

use libpulse_binding as pulse;
use libpulse_simple_binding as psimple;

use lazy_static::lazy_static;

lazy_static! {
    static ref CODEC_REGISTRY: CodecRegistry = {
        let mut registry = CodecRegistry::new();
        registry.register_all::<FlacDecoder>(0);
        registry.register_all::<PcmDecoder>(0);
        registry
    };
}

lazy_static! {
    static ref FORMAT_REGISTRY: FormatRegistry = {
        let mut registry = FormatRegistry::new();
        registry.register_all::<FlacReader>(0);
        registry.register_all::<WavReader>(0);
        registry
    };
}

fn main() {
    let matches = App::new("Sonata Player")
                        .version("1.0")
                        .author("Philip Deljanov <philip.deljanov@gmail.com>")
                        .about("Play audio files with Sonata")
                        .arg(Arg::with_name("seek")
                            .long("seek")
                            .short("-s")
                            .value_name("TIMESTAMP")
                            .help("Seek to the given timestamp")
                            .conflicts_with_all(&[ "verify", "decode-only", "verify-only", "probe-only" ]))
                        .arg(Arg::with_name("decode-only")
                            .long("decode-only")
                            .help("Decodes, but does not play the audio")
                            .conflicts_with_all(&[ "probe-only", "verify-only", "verify" ]))
                        .arg(Arg::with_name("probe-only")
                            .long("probe-only")
                            .help("Only probe the file for metadata")
                            .conflicts_with_all(&[ "decode-only", "verify-only" ]))
                        .arg(Arg::with_name("verify-only")
                            .long("verify-only")
                            .help("Verifies the decoded audio is valid, but does not play the audio")
                            .conflicts_with_all(&[ "verify" ]))
                        .arg(Arg::with_name("verify")
                            .long("verify")
                            .short("-V")
                            .help("Verifies the decoded audio is valid during playback"))
                       .arg(Arg::with_name("verbose")
                            .short("v")
                            .multiple(true)
                            .help("Sets the level of verbosity"))
                        .arg(Arg::with_name("FILE")
                            .help("Sets the input file to use")
                            .required(true)
                            .index(1))
                        .get_matches();

    let file_path = matches.value_of("FILE").unwrap();

    // Create a reader from the given file.
    let mut hint = Hint::new();

    if let Some(extension) = Path::new(file_path).extension() {
        hint.with_extension(extension.to_str().unwrap());
    }

    let fmt_options = FormatOptions { ..Default::default() };

    // Open the given file.
    // TODO: Catch errors.
    let file = Box::new(File::open(file_path).unwrap());

    let mut reader = FORMAT_REGISTRY.guess(&hint, file, &fmt_options).unwrap();

    // Probe the file to check for support.
    let probe_info = reader.probe(ProbeDepth::Deep).unwrap();

    match probe_info {
        ProbeResult::Unsupported => {
            eprintln!("File not supported!");
        },
        ProbeResult::Supported => {
            // Verify only mode decodes and always verifies the audio, but doese not play it.
            if matches.is_present("verify-only") {
                let options = DecoderOptions { verify: true, ..Default::default() };
                decode_only(reader, &options).unwrap();
            }
            // Decode only mode decodes the audio, but not does verify it.
            else if matches.is_present("decode-only") {
                let options = DecoderOptions { verify: false, ..Default::default() };
                decode_only(reader, &options).unwrap();
            }
            // If not probing, play the audio back.
            else if !matches.is_present("probe-only") {
                // Seek to the desired timestamp if requested.
                match matches.value_of("seek") {
                    Some(seek_value) => {
                        let pos = seek_value.parse::<f64>().unwrap();
                        reader.seek(Timestamp::Time(pos)).unwrap();
                    },
                    None => ()
                };

                // Set the decoder options.
                let options = DecoderOptions { verify: matches.is_present("verify"), ..Default::default() };

                // Commence playback.
                play(reader, &options).unwrap();
            }
        }
    }
}

fn decode_only(mut reader: Box<dyn FormatReader>, decode_options: &DecoderOptions) -> Result<()> {
    // Get the default stream.
    // TODO: Allow stream selection.
    let stream = reader.default_stream().unwrap();

    // Create a decoder for the stream.
    let mut decoder = CODEC_REGISTRY.make(&stream.codec_params, &decode_options).unwrap();

    // Get the expected signal spec from the decoder.
    // TODO: Handle the case where the signal spec is not known until the first buffer is decoded.
    let spec = decoder.spec().unwrap();

    let duration = match stream.codec_params.max_frames_per_packet {
        Some(frames) => Duration::Frames(frames),
        None => return unsupported_error("Variable frames per packet are not supported."),
    };

    // Create an audio buffer of the recommended length.
    let mut samples = AudioBuffer::<i32>::new(duration, &spec);

    loop {
        let packet = reader.next_packet()?;

        // Reuse the buffer.
        samples.clear();

        // Try to decode more frames until an error.
        match decoder.decode(packet, &mut samples) {
            Err(err) => {
                eprint!("Error: {}", err);
                break;
            },
            Ok(_) => ()
        }
    }

    Ok(())
}

fn play(mut reader: Box<dyn FormatReader>, decode_options: &DecoderOptions) -> Result<()> {

    // Get the default stream.
    // TODO: Allow stream selection.
    let stream = reader.default_stream().unwrap();

    // Create a decoder for the stream.
    let mut decoder = CODEC_REGISTRY.make(&stream.codec_params, &decode_options).unwrap();

    // Get the expected signal spec from the decoder.
    // TODO: Handle the case where the signal spec is not known until the first buffer is decoded.
    let spec = decoder.spec().unwrap();

    let duration = match stream.codec_params.max_frames_per_packet {
        Some(frames) => Duration::Frames(frames),
        None => return unsupported_error("Variable frames per packet are not supported."),
    };

    // Create an audio buffer of the recommended length.
    let mut samples = AudioBuffer::<i32>::new(duration, &spec);

    // An interleaved buffer is required to send data to the OS.
    let mut raw_samples = SampleBuffer::<i32>::new(duration, &spec);

    let pulse_spec = pulse::sample::Spec {
        format: pulse::sample::SAMPLE_S32NE,
        channels: spec.channels.len() as u8,
        rate: spec.rate,
    };

    assert!(pulse_spec.is_valid());

    let s = psimple::Simple::new(
        None,                                   // Use the default server
        "Sonata Player",                        // Our application’s name
        pulse::stream::Direction::Playback,     // We want a playback stream
        None,                                   // Use the default device
        "Music",                                // Description of our stream
        &pulse_spec,                            // Our sample format
        None,                                   // Use default channel map
        None                                    // Use default buffering attributes
    ).unwrap();

    loop {
        // Reuse the buffer.
        samples.clear();
        
        let packet = reader.next_packet()?;

        // Try to decode more frames until an error.
        match decoder.decode(packet, &mut samples) {
            Err(err) => {
                //reader.end();
                eprint!("Error: {}", err);
                break;
            },
            Ok(_) => {
                // Interleave samples for PulseAudio.
                samples.copy_interleaved(&mut raw_samples);
                // Write interleaved samples to PulseAudio.
                s.write(raw_samples.as_bytes()).unwrap();
            }
        }
    }

    Ok(())

}
