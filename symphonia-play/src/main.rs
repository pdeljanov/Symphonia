// Symphonia
// Copyright (c) 2019 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

// Justification: fields on DecoderOptions and FormatOptions may change at any time, but symphonia-play
// doesn't want to be updated every time those fields change, therefore always fill in the remaining
// fields with default values.
#![allow(clippy::needless_update)]

use std::fs::File;
use std::path::Path;

use symphonia;
use symphonia::core::errors::{Result, Error};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::{Cue, FormatReader, FormatOptions, SeekMode, SeekTo, Track};
use symphonia::core::meta::{ColorMode, MetadataOptions, Tag, Value, Visual};
use symphonia::core::io::{MediaSourceStream, MediaSource, ReadOnlySource};
use symphonia::core::probe::{Hint, ProbeResult};
use symphonia::core::units::{Duration, Time};

use clap::{Arg, App};
use log::{error, info, warn};
use pretty_env_logger;

mod output;

fn main() {
    pretty_env_logger::init();

    let matches = App::new("Symphonia Play")
                        .version("1.0")
                        .author("Philip Deljanov <philip.deljanov@gmail.com>")
                        .about("Play audio with Symphonia")
                        .arg(Arg::with_name("seek")
                            .long("seek")
                            .short("-s")
                            .value_name("TIME")
                            .help("Seek to the given time in seconds")
                            .conflicts_with_all(
                                &[
                                    "verify",
                                    "decode-only",
                                    "verify-only",
                                    "probe-only"
                                ]
                            ))
                        .arg(Arg::with_name("track")
                            .long("track")
                            .short("t")
                            .value_name("TRACK")
                            .help("The track to use"))
                        .arg(Arg::with_name("decode-only")
                            .long("decode-only")
                            .help("Decode, but do not play the audio")
                            .conflicts_with_all(&[ "probe-only", "verify-only", "verify" ]))
                        .arg(Arg::with_name("probe-only")
                            .long("probe-only")
                            .help("Only probe the input for metadata")
                            .conflicts_with_all(&[ "decode-only", "verify-only" ]))
                        .arg(Arg::with_name("verify-only")
                            .long("verify-only")
                            .help("Verify the decoded audio is valid, but do not play the audio")
                            .conflicts_with_all(&[ "verify" ]))
                        .arg(Arg::with_name("verify")
                            .long("verify")
                            .short("-V")
                            .help("Verify the decoded audio is valid during playback"))
                       .arg(Arg::with_name("verbose")
                            .short("v")
                            .multiple(true)
                            .help("Sets the level of verbosity"))
                        .arg(Arg::with_name("INPUT")
                            .help("The input file path, or specify - to use standard input")
                            .required(true)
                            .index(1))
                        .get_matches();

    let path_str = matches.value_of("INPUT").unwrap();

    // Create a hint to help the format registry guess what format reader is appropriate.
    let mut hint = Hint::new();

    // If the path string is '-' then read from standard input.
    let source = if path_str == "-" {
        Box::new(ReadOnlySource::new(std::io::stdin())) as Box<dyn MediaSource>
    }
    else {
        // Othwerise, get a Path from the path string.
        let path = Path::new(path_str);

        // Provide the file extension as a hint.
        if let Some(extension) = path.extension() {
            if let Some(extension_str) = extension.to_str() {
                hint.with_extension(extension_str);
            }
        }

        Box::new(File::open(path).unwrap())
    };

    // Create the media source stream using the boxed media source from above.
    let mss = MediaSourceStream::new(source, Default::default());

    // Use the default options for metadata and format readers.
    let format_opts: FormatOptions = Default::default();
    let metadata_opts: MetadataOptions = Default::default();

    // Get the track if provided.
    let track = match matches.value_of("track") {
        Some(track_str) => track_str.parse::<usize>().map_or(None, |t| Some(t)),
        _ => None,
    };

    // Probe the media source stream for metadata and get the format reader.
    match symphonia::default::get_probe().format(&hint, mss, &format_opts, &metadata_opts) {
        Ok(mut probed) => {
            let result = if matches.is_present("verify-only") {
                // Verify-only mode decodes and verifies the audio, but does not play it.
                decode_only(probed.format, &DecoderOptions { verify: true, ..Default::default() })
            }
            else if matches.is_present("decode-only") {
                // Decode-only mode decodes the audio, but does not play or verify it.
                decode_only(probed.format, &DecoderOptions { verify: false, ..Default::default() })
            }
            else if matches.is_present("probe-only") {
                // Probe-only mode only prints information about the format, tracks, metadata, etc.
                pretty_print_format(path_str, &mut probed);
                Ok(())
            }
            else {
                // Playback mode.
                pretty_print_format(path_str, &mut probed);

                // If present, parse the seek argument.
                let seek_time = matches.value_of("seek").map(|p| p.parse::<f64>().unwrap_or(0.0));

                // Set the decoder options.
                let options = DecoderOptions {
                    verify: matches.is_present("verify"),
                    ..Default::default()
                };

                // Play it!
                play(probed.format, track, seek_time, &options)
            };

            if let Err(err) = result {
                error!("error: {}", err);
            }
        }
        Err(err) => {
            // The input was not supported by any format reader.
            error!("file not supported. reason? {}", err);
        }
    }
}

fn decode_only(mut reader: Box<dyn FormatReader>, decode_options: &DecoderOptions) -> Result<()> {
    // Get the default track.
    // TODO: Allow track selection.
    let track = reader.default_track().unwrap();
    let track_id = track.id;

    // Create a decoder for the track.
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &decode_options)?;

    // Decode all packets, ignoring all decode errors.
    let result = loop {
        let packet = match reader.next_packet() {
            Ok(packet) => packet,
            Err(err) => break Err(err),
        };

        // If the packet does not belong to the selected track, skip over it.
        if packet.track_id() != track_id {
            continue;
        }

        // Decode the packet into audio samples.
        match decoder.decode(&packet) {
            Err(Error::DecodeError(err)) => warn!("decode error: {}", err),
            Err(err) => break Err(err),
            _ => continue,
        }
    };

    // Regardless of result, finalize the decoder to get the verification result.
    let finalize_result = decoder.finalize();

    if let Some(verify_ok) = finalize_result.verify_ok {
        if verify_ok {
            info!("verification passed");
        }
        else {
            info!("verification failed");
        }
    }

    result
}

fn play(
    mut reader: Box<dyn FormatReader>,
    play_track: Option<usize>,
    seek_time: Option<f64>,
    decode_options: &DecoderOptions
) -> Result<()> {
    // The audio output device.
    let mut audio_output = None;

    // Select the appropriate track.
    let track = match play_track {
        Some(t) => {
            // If provided a track number, try to use it, otherwise select the default track.
            reader.tracks().get(t).unwrap_or(reader.default_track().unwrap())
        }
        _ => reader.default_track().unwrap(),
    };

    let track_id = track.id;

    // Create a decoder for the track.
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &decode_options)?;

    // If there is a seek time, seek the reader to the time specified and get the timestamp of the
    // seeked position. All packets with a timestamp < the seeked position will not be played.
    //
    // Note: This is a half-baked approach to seeking! After seeking the reader, packets should be
    // decoded and *samples* discarded up-to the exact *sample* indicated by required_ts. The current
    // approach will discard excess samples if seeking to a sample within a packet.
    let seek_ts = if let Some(time) = seek_time {
        let seek_to = SeekTo::Time { time: Time::from(time), track_id: None };

        // Attempt the seek. If the seek fails, ignore the error and return a seek timestamp of 0 so
        // that no samples are trimmed.
        match reader.seek(SeekMode::Accurate, seek_to) {
            Ok(seeked_to) => {
                seeked_to.required_ts
            }
            Err(err) => {
                // Don't give-up on a seek error.
                warn!("seek error: {}", err);
                0
            }
        }
    }
    else {
        // If not seeking, the seek timestamp is 0.
        0
    };

    // Decode and play the packets belonging to the selected track.
    let result = loop {
        // Get the next packet from the media container.
        let packet = match reader.next_packet() {
            Ok(packet) => packet,
            Err(err) => break Err(err),
        };

        // If the packet does not belong to the selected track, skip over it.
        if packet.track_id() != track_id {
            continue;
        }

        // Decode the packet into audio samples.
        match decoder.decode(&packet) {
            Ok(decoded) => {
                // If the audio output is not open, try to open it.
                if audio_output.is_none() {
                    // Get the buffer specification. This is a description of the decoded audio
                    // buffer's sample format.
                    let spec = decoded.spec().clone();

                    // Get the duration of the decoded buffer.
                    let duration = Duration::from(decoded.capacity() as u64);

                    // Try to open the audio output.
                    audio_output = Some(output::try_open(spec, duration).unwrap());
                }

                // Write the decoded audio samples to the audio output if the presentation timestamp
                // for the packet is >= the seeked position (0 if not seeking).
                if packet.pts() >= seek_ts {
                    if let Some(audio_output) = audio_output.as_mut() {
                        audio_output.write(decoded).unwrap()
                    }
                }
            }
            Err(Error::DecodeError(err)) => {
                // Decode errors are not fatal. Print the error message and try to decode the next
                // packet as usual.
                warn!("decode error: {}", err);
            }
            Err(err) => break Err(err),
        }
    };

    // Flush the audio output to finish playing back any leftover samples.
    if let Some(audio_output) = audio_output.as_mut() {
        audio_output.flush()
    }

    // Regardless of result, finalize the decoder to get the verification result.
    let finalize_result = decoder.finalize();

    if let Some(verify_ok) = finalize_result.verify_ok {
        if verify_ok {
            info!("verification passed");
        }
        else {
            info!("verification failed");
        }
    }

    result
}

fn pretty_print_format(path: &str, probed: &mut ProbeResult) {
    println!("+ {}", path);
    pretty_print_tracks(probed.format.tracks());

    // Prefer metadata that's provided in the container format, over other tags found during the
    // probe operation.
    if let Some(metadata_rev) = probed.format.metadata().current() {
        pretty_print_tags(metadata_rev.tags());
        pretty_print_visuals(metadata_rev.visuals());

        // Warn that certain tags are preferred.
        if probed.metadata.get().as_ref().is_some() {
            info!("tags that are part of the container format are preferentially printed.");
            info!("not printing additional tags that were found while probing.");
        }
    }
    else if let Some(metadata_rev) = probed.metadata
        .get()
        .as_ref()
        .and_then(|m| m.current())
    {
        pretty_print_tags(metadata_rev.tags());
        pretty_print_visuals(metadata_rev.visuals());
    }

    pretty_print_cues(probed.format.cues());
    println!("-");
}

fn pretty_print_tracks(tracks: &[Track]) {
    if !tracks.is_empty() {
        println!("|");
        println!("| // Tracks //");

        for (idx, track) in tracks.iter().enumerate() {
            let params = &track.codec_params;

            print!("|     [{:0>2}] Codec:           ", idx + 1);

            if let Some(codec) = symphonia::default::get_codecs().get_codec(params.codec) {
                println!("{} ({})", codec.long_name, codec.short_name);
            }
            else {
                println!("Unknown (#{})", params.codec);
            }

            if let Some(sample_rate) = params.sample_rate {
                println!("|          Sample Rate:     {}", sample_rate);
            }
            if let Some(n_frames) = params.n_frames {
                println!("|          Frames:          {}", n_frames);
            }
            if let Some(sample_format) = params.sample_format {
                println!("|          Sample Format:   {:?}", sample_format);
            }
            if let Some(bits_per_sample) = params.bits_per_sample {
                println!("|          Bits per Sample: {}", bits_per_sample);
            }
            if let Some(channels) = params.channels {
                println!("|          Channel(s):      {}", channels.count());
                println!("|          Channel Map:     {}", channels);
            }
            if let Some(channel_layout) = params.channel_layout {
                println!("|          Channel Layout:  {:?}", channel_layout);
            }
            if let Some(language) = &track.language {
                println!("|          Language:        {}", language);
            }

        }
    }
}

fn pretty_print_cues(cues: &[Cue]) {
    if !cues.is_empty() {
        println!("|");
        println!("| // Cues //");

        for (idx, cue) in cues.iter().enumerate() {
            println!("|     [{:0>2}] Track:      {}", idx + 1, cue.index);
            println!("|          Timestamp:  {}", cue.start_ts);

            // Print tags associated with the Cue.
            if !cue.tags.is_empty() {
                println!("|          Tags:");

                for (tidx, tag) in cue.tags.iter().enumerate() {
                    if let Some(std_key) = tag.std_key {
                        println!("{}", pretty_print_tag_item(tidx + 1, &format!("{:?}", std_key), &tag.value, 21));
                    }
                    else {
                        println!("{}", pretty_print_tag_item(tidx + 1, &tag.key, &tag.value, 21));
                    }
                }
            }

            // Print any sub-cues.
            if !cue.points.is_empty() {
                println!("|          Sub-Cues:");

                for (ptidx, pt) in cue.points.iter().enumerate() {
                    println!("|                      [{:0>2}] Offset:    {:?}", ptidx + 1, pt.start_offset_ts);

                    // Start the number of sub-cue tags, but don't print them.
                    if !pt.tags.is_empty() {
                        println!("|                           Sub-Tags:  {} (not listed)", pt.tags.len());
                    }
                }
            }

        }
    }
}

fn pretty_print_tags(tags: &[Tag]) {
    if !tags.is_empty() {
        println!("|");
        println!("| // Tags //");

        let mut idx = 1;

        // Print tags with a standard tag key first, these are the most common tags.
        for tag in tags.iter().filter(| tag | tag.is_known()) {
            if let Some(std_key) = tag.std_key {
                println!("{}", pretty_print_tag_item(idx, &format!("{:?}", std_key), &tag.value, 4));
            }
            idx += 1;
        }

        // Print the remaining tags with keys truncated to 26 characters.
        for tag in tags.iter().filter(| tag | !tag.is_known()) {
            println!("{}", pretty_print_tag_item(idx, &tag.key, &tag.value, 4));
            idx += 1;
        }
    }
}

fn pretty_print_visuals(visuals: &[Visual]) {
    if !visuals.is_empty() {
        println!("|");
        println!("| // Visuals //");

        for (idx, visual) in visuals.iter().enumerate() {

            if let Some(usage) = visual.usage {
                println!("|     [{:0>2}] Usage:      {:?}", idx + 1, usage);
                println!("|          Media Type: {}", visual.media_type);
            }
            else {
                println!("|     [{:0>2}] Media Type: {}", idx + 1, visual.media_type);
            }
            if let Some(dimensions) = visual.dimensions {
                println!("|          Dimensions: {} px x {} px", dimensions.width, dimensions.height);
            }
            if let Some(bpp) = visual.bits_per_pixel {
                println!("|          Bits/Pixel: {}", bpp);
            }
            if let Some(ColorMode::Indexed(colors)) = visual.color_mode {
                println!("|          Palette:    {} colors", colors);
            }
            println!("|          Size:       {} bytes", visual.data.len());

            // Print out tags similar to how regular tags are printed.
            if !visual.tags.is_empty() {
                println!("|          Tags:");
            }

            for (tidx, tag) in visual.tags.iter().enumerate() {
                if let Some(std_key) = tag.std_key {
                    println!("{}", pretty_print_tag_item(tidx + 1, &format!("{:?}", std_key), &tag.value, 21));
                }
                else {
                    println!("{}", pretty_print_tag_item(tidx + 1, &tag.key, &tag.value, 21));
                }
            }
        }
    }
}

fn pretty_print_tag_item(idx: usize, key: &str, value: &Value, indent: usize) -> String {
    let key_str = match key.len() {
        0..=28 => format!("| {:w$}[{:0>2}] {:<28} : ", "", idx, key, w = indent),
        _ => format!("| {:w$}[{:0>2}] {:.<28} : ", "", idx, key.split_at(26).0, w = indent),
    };

    let line_prefix = format!("\n| {:w$} : ", "", w = indent + 4 + 28 + 1);
    let line_wrap_prefix = format!("\n| {:w$}   ", "", w = indent + 4 + 28 + 1);

    let mut out = String::new();

    out.push_str(&key_str);

    for (wrapped, line) in value.to_string().lines().enumerate() {
        if wrapped > 0 {
            out.push_str(&line_prefix);
        }

        let mut chars = line.chars();
        let split = (0..)
            .map(|_| chars.by_ref().take(72).collect::<String>())
            .take_while(|s| !s.is_empty())
            .collect::<Vec<_>>();

        out.push_str(&split.join(&line_wrap_prefix));
    }

    out
}