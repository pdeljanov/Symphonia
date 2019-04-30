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
use sonata;
use sonata::core::errors::{Result, unsupported_error};
use sonata::core::audio::*;
use sonata::core::codecs::DecoderOptions;
use sonata::core::formats::{FormatReader, Hint, FormatOptions, ProbeDepth, ProbeResult};
use sonata::core::tags::Tag;
use libpulse_binding as pulse;
use libpulse_simple_binding as psimple;

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

    // Get the file path option.
    let path = Path::new(matches.value_of("FILE").unwrap());


    // Create a hint to help the format registry guess what format reader is appropriate for file at the given file 
    // path.
    let mut hint = Hint::new();

    // Use the file extension as a hint.
    if let Some(extension) = path.extension() {
        hint.with_extension(extension.to_str().unwrap());
    }

    // Open the given file.
    // TODO: Catch errors.
    let file = Box::new(File::open(path).unwrap());

    // Use the format registry to pick a format reader for the given file and instantiate it with a default set of 
    // options.
    let format_options = FormatOptions { ..Default::default() };
    let mut reader = sonata::default::get_formats().guess(&hint, file, &format_options).unwrap();

    // Probe the file using the format reader to verify the file is actually supported.
    let probe_info = reader.probe(ProbeDepth::Deep).unwrap();

    match probe_info {
        // The file was not actually supported by the format reader.
        ProbeResult::Unsupported => {
            eprintln!("File not supported!");
        },
        // The file is supported by the format reader.
        ProbeResult::Supported => {
            // Print metadata fancily.
            pretty_print_path(&path);
            pretty_print_tags(reader.tags());
            pretty_print_chapters();
            pretty_print_visuals();
            eprintln!("-");

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

fn pretty_print_path(path: &Path) {
    eprintln!("+ {}", path.display());
    eprintln!("|");
}

fn pretty_print_chapters() {
    eprintln!("|  // Cuepoints //");
}

fn pretty_print_tags(tags: &[Tag]) {
    eprintln!("|  // Tags //");
    
    // Print tags with a standard tag key first, these are the most common tags.
    for tag in tags.iter().filter(| tag | tag.is_known()) {
        if let Some(std_key) = tag.std_key {
            eprintln!("|      * {:<28} : {}", format!("{:?}", std_key), tag.value);
        }
    }

    // Print the remaining tags with keys truncated to 26 characters.
    for tag in tags.iter().filter(| tag | !tag.is_known()) {
        match tag.key.len() {
            0...28 => eprintln!("|      * {:<28} : {}", tag.key, tag.value),
            _ => eprintln!("|      * {:.<28} : {}", tag.key.split_at(26).0, tag.value),
        }
    }
}

fn pretty_print_visuals() {
    eprintln!("|  // Visuals //");
}

fn decode_only(mut reader: Box<dyn FormatReader>, decode_options: &DecoderOptions) -> Result<()> {
    // Get the default stream.
    // TODO: Allow stream selection.
    let stream = reader.default_stream().unwrap();

    // Create a decoder for the stream.
    let mut decoder = sonata::default::get_codecs().make(&stream.codec_params, &decode_options).unwrap();

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
                decoder.close();
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
    let mut decoder = sonata::default::get_codecs().make(&stream.codec_params, &decode_options).unwrap();

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
                decoder.close();
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