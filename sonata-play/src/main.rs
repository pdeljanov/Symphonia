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

use std::fs::File;
use clap::{Arg, App};
use sonata_core::errors::{Result, unsupported_error};
use sonata_core::audio::*;
use sonata_codecs_flac::*;

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
                            .conflicts_with_all(&["check", "probe"]))
                        .arg(Arg::with_name("probe")
                            .long("probe")
                            .short("-p")
                            .help("Only probe the file for metadata")
                            .conflicts_with_all(&["check"]))
                        .arg(Arg::with_name("check")
                            .long("check")
                            .short("-c")
                            .help("Decodes the entire file and checks that the decoded audio is valid"))
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

    // Open the given file.
    // TODO: Catch errors.
    let file = Box::new(File::open(file_path).unwrap());

    // Create a FLAC reader from the given file.
    let mut reader = Flac::open(file);

    // Probe the file to check for support.
    let probe_info = reader.probe(ProbeDepth::Deep).unwrap();

    match probe_info {
        ProbeResult::Unsupported => {
            eprintln!("File not supported!");
        }
        ProbeResult::Supported => {
            if matches.is_present("check") {
                validate(&mut reader);
            }
            else if !matches.is_present("probe") {

                match matches.value_of("seek") {
                    Some(seek_value) => {
                        let pos = seek_value.parse::<u64>().unwrap();
                        reader.seek(pos).unwrap();
                    },
                    None => ()
                };

                play(&mut reader);
            }
        }
    }
}

fn validate(reader: &mut FlacReader) -> Result<()> {
    // Get the default stream.
    // TODO: Allow stream selection.
    let stream = reader.default_stream().unwrap();

    // Create a decoder for the stream.
    // TODO: Implement stream.make_decoder().
    let mut decoder = FlacDecoder::new(&stream.codec_params);

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
        let mut packet = reader.next_packet()?;

        // Reuse the buffer.
        samples.renew();

        // Try to decode more frames until an error.
        match decoder.decode(&mut packet, &mut samples) {
            Err(err) => {
                //reader.end();
                eprint!("Error: {}", err);
                break;
            },
            Ok(_) => ()
        }
    }

    Ok(())
}

fn play(reader: &mut FlacReader) -> Result<()> {

    // Get the default stream.
    // TODO: Allow stream selection.
    let stream = reader.default_stream().unwrap();

    // Create a decoder for the stream.
    // TODO: Implement stream.make_decoder().
    let mut decoder = FlacDecoder::new(&stream.codec_params);

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
    

    let spec = pulse::sample::Spec {
        format: pulse::sample::SAMPLE_S32NE,
        channels: spec.channels.len() as u8,
        rate: spec.rate,
    };

    assert!(spec.is_valid());

    let s = psimple::Simple::new(
        None,                                   // Use the default server
        "Sonata Player",                        // Our application’s name
        pulse::stream::Direction::Playback,     // We want a playback stream
        None,                                   // Use the default device
        "Music",                                // Description of our stream
        &spec,                                  // Our sample format
        None,                                   // Use default channel map
        None                                    // Use default buffering attributes
    ).unwrap();

    loop {
        // Reuse the buffer.
        samples.renew();
        
        let mut packet = reader.next_packet()?;

        // Try to decode more frames until an error.
        match decoder.decode(&mut packet, &mut samples) {
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
