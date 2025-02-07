// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]
// Justification: Fields on DecoderOptions and FormatOptions may change at any time, but
// symphonia-play doesn't want to be updated every time those fields change, therefore always fill
// in the remaining fields with default values.
#![allow(clippy::needless_update)]

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::Write;
use std::path::Path;

use symphonia::core::codecs::audio::{AudioDecoderOptions, FinalizeResult};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::errors::{Error, Result};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::{MetadataOptions, Visual};
use symphonia::core::units::Time;

use clap::{Arg, ArgMatches};
use log::{error, info, warn};

mod output;
mod ui;

#[cfg(not(target_os = "linux"))]
mod resampler;

#[derive(Copy, Clone)]
enum SeekPosition {
    Time(f64),
    Timetamp(u64),
}

fn main() {
    pretty_env_logger::init();

    let args = clap::Command::new("Symphonia Play")
        .version("1.0")
        .author("Philip Deljanov <philip.deljanov@gmail.com>")
        .about("Play audio with Symphonia")
        .arg(
            Arg::new("seek")
                .long("seek")
                .short('s')
                .value_name("TIME")
                .help("Seek to the time in seconds")
                .conflicts_with_all(&[
                    "seek-ts",
                    "decode-only",
                    "probe-only",
                    "verify",
                    "verify-only",
                ]),
        )
        .arg(
            Arg::new("seek-ts")
                .long("seek-ts")
                .short('S')
                .value_name("TIMESTAMP")
                .help("Seek to the timestamp in timebase units")
                .conflicts_with_all(&[
                    "seek",
                    "decode-only",
                    "probe-only",
                    "verify",
                    "verify-only",
                ]),
        )
        .arg(
            Arg::new("track").long("track").short('t').value_name("TRACK").help("The track to use"),
        )
        .arg(
            Arg::new("decode-only")
                .long("decode-only")
                .help("Decode, but do not play the audio")
                .conflicts_with_all(&["probe-only", "verify-only", "verify"]),
        )
        .arg(
            Arg::new("probe-only")
                .long("probe-only")
                .help("Only probe the input for metadata")
                .conflicts_with_all(&["decode-only", "verify-only"]),
        )
        .arg(
            Arg::new("verify-only")
                .long("verify-only")
                .help("Verify the decoded audio is valid, but do not play the audio")
                .conflicts_with_all(&["verify"]),
        )
        .arg(
            Arg::new("verify")
                .long("verify")
                .short('v')
                .help("Verify the decoded audio is valid during playback"),
        )
        .arg(Arg::new("no-progress").long("no-progress").help("Do not display playback progress"))
        .arg(
            Arg::new("no-gapless").long("no-gapless").help("Disable gapless decoding and playback"),
        )
        .arg(
            Arg::new("dump-visuals")
                .long("dump-visuals")
                .help("Dump all visuals to the current working directory"),
        )
        .arg(
            Arg::new("INPUT")
                .help("The input file path, or - to use standard input")
                .required(true)
                .index(1),
        )
        .get_matches();

    // For any error, return an exit code -1. Otherwise return the exit code provided.
    let code = match run(&args) {
        Ok(code) => code,
        Err(err) => {
            error!("{}", err.to_string().to_lowercase());
            -1
        }
    };

    std::process::exit(code)
}

fn run(args: &ArgMatches) -> Result<i32> {
    let path = Path::new(args.value_of("INPUT").unwrap());

    // Create a hint to help the format registry guess what format reader is appropriate.
    let mut hint = Hint::new();

    // If the path string is '-' then read from standard input.
    let source = if path.as_os_str() == "-" {
        Box::new(ReadOnlySource::new(std::io::stdin())) as Box<dyn MediaSource>
    }
    else {
        // Othwerise, get a Path from the path string.

        // Provide the file extension as a hint.
        if let Some(extension) = path.extension() {
            if let Some(extension_str) = extension.to_str() {
                hint.with_extension(extension_str);
            }
        }

        Box::new(File::open(path)?)
    };

    // Create the media source stream using the boxed media source from above.
    let mss = MediaSourceStream::new(source, Default::default());

    // Use the default options for format readers other than for gapless playback.
    let fmt_opts =
        FormatOptions { enable_gapless: !args.is_present("no-gapless"), ..Default::default() };

    // Use the default options for metadata readers.
    let meta_opts: MetadataOptions = Default::default();

    // Probe the media source stream for metadata and get the format reader.
    match symphonia::default::get_probe().probe(&hint, mss, fmt_opts, meta_opts) {
        Ok(mut format) => {
            // Dump visuals if requested.
            if args.is_present("dump-visuals") {
                let name = match path.file_name() {
                    Some(name) if name != "-" => name,
                    _ => OsStr::new("NoName"),
                };

                dump_visuals(&mut format, name);
            }

            // Get the value of the track number option, if provided.
            let track_num = args.value_of("track").and_then(|value| value.parse::<usize>().ok());

            // Select the operating mode.
            if args.is_present("probe-only") {
                // Probe-only mode only prints information about the format, tracks, metadata, etc.
                ui::print_format(path, &mut format);
                Ok(0)
            }
            else if args.is_present("verify-only") {
                // Verify-only mode decodes and verifies the audio, but does not play it.
                let opts = DecodeOptions {
                    decoder_opts: AudioDecoderOptions { verify: true, ..Default::default() },
                    track_num,
                };

                decode_only(format, opts)
            }
            else if args.is_present("decode-only") {
                // Decode-only mode decodes the audio, but does not play or verify it.
                let opts = DecodeOptions {
                    decoder_opts: AudioDecoderOptions { verify: false, ..Default::default() },
                    track_num,
                };

                decode_only(format, opts)
            }
            else {
                // Playback mode.
                ui::print_format(path, &mut format);

                // If present, parse the seek argument.
                let seek_pos = if let Some(time) = args.value_of("seek") {
                    Some(SeekPosition::Time(time.parse::<f64>().unwrap_or(0.0)))
                }
                else {
                    args.value_of("seek-ts")
                        .map(|ts| SeekPosition::Timetamp(ts.parse::<u64>().unwrap_or(0)))
                };

                // Setup playback options.
                let opts = PlayOptions {
                    // Decoder options.
                    decoder_opts: AudioDecoderOptions {
                        verify: args.is_present("verify"),
                        ..Default::default()
                    },
                    track_num,
                    seek_pos,
                    no_progress: args.is_present("no-progress"),
                };

                // Play it!
                play(format, opts)
            }
        }
        Err(err) => {
            // The input was not supported by any format reader.
            info!("the input is not supported");
            Err(err)
        }
    }
}

/// Options for the decode command.
#[derive(Copy, Clone)]
struct DecodeOptions {
    decoder_opts: AudioDecoderOptions,
    track_num: Option<usize>,
}

fn decode_only(mut reader: Box<dyn FormatReader>, opts: DecodeOptions) -> Result<i32> {
    // If the user provided a track number, select that track if it exists, otherwise, select the
    // default audio track.
    let track = opts
        .track_num
        .and_then(|t| reader.tracks().get(t))
        .or_else(|| reader.default_track(TrackType::Audio));

    // Return if no track has been found.
    let track = match track {
        Some(track) => track,
        _ => return Ok(0),
    };

    // Get the audio codec parameters from the track. Return if the track is not an audio track, or
    // does not have any codec parameters.
    let codec_params = match track.codec_params.as_ref() {
        Some(CodecParameters::Audio(audio)) => audio,
        _ => return Ok(0),
    };

    // Create a decoder for the track.
    let mut decoder =
        symphonia::default::get_codecs().make_audio_decoder(codec_params, &opts.decoder_opts)?;

    // Save the track ID to filter demuxed packets.
    let track_id = track.id;

    // Decode all packets, ignoring all decode errors.
    loop {
        let Some(packet) = reader.next_packet()?
        else {
            break;
        };

        // If the packet does not belong to the selected track, skip over it.
        if packet.track_id() != track_id {
            continue;
        }

        // Decode the packet into audio samples.
        match decoder.decode(&packet) {
            Ok(_decoded) => continue,
            Err(Error::DecodeError(err)) => warn!("decode error: {}", err),
            Err(err) => return Err(err),
        }
    }

    info!("end of stream");

    // Finalize the decoder and return the verification result if it's been enabled.
    do_verification(decoder.finalize())
}

/// Options for the play command.
#[derive(Copy, Clone)]
struct PlayOptions {
    decoder_opts: AudioDecoderOptions,
    track_num: Option<usize>,
    seek_pos: Option<SeekPosition>,
    no_progress: bool,
}

/// Options for playing a single track.
#[derive(Copy, Clone)]
struct PlayTrackOptions {
    decoder_opts: AudioDecoderOptions,
    track_id: u32,
    seek_ts: u64,
    no_progress: bool,
}

fn play(mut reader: Box<dyn FormatReader>, opts: PlayOptions) -> Result<i32> {
    // If the user provided a track number, select that track if it exists, otherwise, select the
    // default audio track.
    let track = opts
        .track_num
        .and_then(|t| reader.tracks().get(t))
        .or_else(|| reader.default_track(TrackType::Audio));

    // Get the track ID for filtering packets. Return if no track has been found.
    let mut track_id = match track {
        Some(track) => track.id,
        _ => return Ok(0),
    };

    // If seeking, seek the reader to the time or timestamp specified and get the timestamp of the
    // seeked position.
    let seek_ts = if let Some(seek_pos) = opts.seek_pos {
        let seek_to = match seek_pos {
            SeekPosition::Time(t) => SeekTo::Time { time: Time::from(t), track_id: Some(track_id) },
            SeekPosition::Timetamp(ts) => SeekTo::TimeStamp { ts, track_id },
        };

        // Attempt the seek. If the seek fails, ignore the error and return a seek timestamp of 0 so
        // that no samples are trimmed.
        match reader.seek(SeekMode::Accurate, seek_to) {
            Ok(seeked_to) => seeked_to.required_ts,
            Err(Error::ResetRequired) => {
                // Handle a demuxer reset.
                track_id = match do_reset(&mut reader) {
                    Some(id) => id,
                    _ => return Ok(0),
                };
                0
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

    let mut track_options = PlayTrackOptions {
        decoder_opts: opts.decoder_opts,
        track_id,
        seek_ts,
        no_progress: opts.no_progress,
    };

    // The audio output device.
    let mut audio_output = None;

    let result = loop {
        match play_track(&mut reader, &mut audio_output, track_options) {
            Err(Error::ResetRequired) => {
                // Handle a demuxer reset.
                track_options.track_id = match do_reset(&mut reader) {
                    Some(id) => id,
                    _ => return Ok(0),
                };
                track_options.seek_ts = 0;
            }
            res => break res,
        }
    };

    // Flush the audio output to finish playing back any leftover samples.
    if let Some(audio_output) = audio_output.as_mut() {
        audio_output.flush()
    }

    result
}

fn play_track(
    reader: &mut Box<dyn FormatReader>,
    audio_output: &mut Option<Box<dyn output::AudioOutput>>,
    opts: PlayTrackOptions,
) -> Result<i32> {
    // Get the selected track using the track ID.
    let track = match reader.tracks().iter().find(|track| track.id == opts.track_id) {
        Some(track) => track,
        _ => return Ok(0),
    };

    // Get the audio codec parameters from the track. Return if the track is not an audio track, or
    // does not have any codec parameters.
    let codec_params = match track.codec_params.as_ref() {
        Some(CodecParameters::Audio(audio)) => audio,
        _ => return Ok(0),
    };

    // Create a decoder for the track.
    let mut decoder =
        symphonia::default::get_codecs().make_audio_decoder(codec_params, &opts.decoder_opts)?;

    // Get the selected track's timebase and duration.
    let tb = track.time_base;
    let dur = track.num_frames.map(|frames| track.start_ts + frames);

    // Decode and play the packets belonging to the selected track.
    loop {
        // Get the next packet from the format reader.
        let Some(packet) = reader.next_packet()?
        else {
            break;
        };

        // If the packet does not belong to the selected track, skip it.
        if packet.track_id() != opts.track_id {
            continue;
        }

        //Print out new metadata.
        while !reader.metadata().is_latest() {
            reader.metadata().pop();

            if let Some(rev) = reader.metadata().current() {
                ui::print_update(rev);
            }
        }

        // Decode the packet into audio samples.
        match decoder.decode(&packet) {
            Ok(decoded) => {
                // If the audio output is not open, try to open it.
                if audio_output.is_none() {
                    // Get the capacity of the decoded buffer. Note that this is capacity, not
                    // length! The output will use this to size its internal buffers appropriately.
                    let duration = decoded.capacity() as u64;

                    // Try to open the audio output.
                    audio_output.replace(output::try_open(decoded.spec(), duration).unwrap());
                }
                else {
                    // TODO: Check the audio spec. and duration hasn't changed.
                }

                // Write the decoded audio samples to the audio output if the presentation timestamp
                // for the packet is >= the seeked position (0 if not seeking).
                //
                // NOTE: This is a half-baked approach to seeking! After seeking the reader, packets
                // should be decoded and *samples* discarded up-to the exact *sample* indicated by
                // required_ts. The current approach will discard extra samples if seeking to a
                // sample within a packet.
                if packet.pts() >= opts.seek_ts {
                    if !opts.no_progress {
                        ui::print_progress(packet.pts(), dur, tb);
                    }

                    if let Some(audio_output) = audio_output {
                        audio_output.write(decoded).unwrap()
                    }
                }
            }
            Err(Error::DecodeError(err)) => {
                // Decode errors are not fatal. Print the error message and try to decode the next
                // packet as usual.
                warn!("decode error: {}", err);
            }
            Err(err) => return Err(err),
        }
    }

    if !opts.no_progress {
        println!();
    }

    info!("end of stream");

    // Finalize the decoder and return the verification result if it's been enabled.
    do_verification(decoder.finalize())
}

fn do_reset(reader: &mut Box<dyn FormatReader>) -> Option<u32> {
    // The demuxer indicated that a reset is required. This is sometimes seen with streaming OGG
    // (e.g., Icecast) wherein the entire contents of the container change (new tracks, codecs,
    // metadata, etc.). Therefore, we must select a new track and recreate the decoder.
    ui::print_blank();
    ui::print_tracks(reader.tracks());

    // Select the default audio track since the user's selected track number may no longer be exist
    // or make sense.
    reader.default_track(TrackType::Audio).map(|track| track.id)
}

fn do_verification(finalization: FinalizeResult) -> Result<i32> {
    match finalization.verify_ok {
        Some(is_ok) => {
            // Got a verification result.
            println!("verification: {}", if is_ok { "passed" } else { "failed" });

            Ok(i32::from(!is_ok))
        }
        // Verification not enabled by user, or unsupported by the codec.
        _ => Ok(0),
    }
}

fn dump_visuals(format: &mut Box<dyn FormatReader>, file_name: &OsStr) {
    if let Some(metadata) = format.metadata().current() {
        for (i, visual) in metadata.visuals().iter().enumerate() {
            dump_visual(visual, file_name, i);
        }
    }
}

fn dump_visual(visual: &Visual, file_name: &OsStr, index: usize) {
    let extension = match visual.media_type.as_ref().map(|m| m.to_lowercase()).as_deref() {
        Some("image/bmp") => ".bmp",
        Some("image/gif") => ".gif",
        Some("image/jpeg") => ".jpg",
        Some("image/png") => ".png",
        _ => ".bin",
    };

    let mut out_file_name = OsString::from(file_name);
    out_file_name.push(format!("-{:0>2}{}", index, extension));

    if let Err(err) = File::create(out_file_name).and_then(|mut file| file.write_all(&visual.data))
    {
        warn!("failed to dump visual due to error {}", err);
    }
}
