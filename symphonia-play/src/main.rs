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

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::Write;
use std::path::Path;

use lazy_static::lazy_static;
use symphonia::core::codecs::audio::{AudioDecoderOptions, FinalizeResult};
use symphonia::core::codecs::{CodecInfo, CodecParameters, CodecProfile};
use symphonia::core::errors::{Error, Result};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, Track, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::{
    Chapter, ChapterGroup, ChapterGroupItem, ColorMode, ColorModel, MetadataOptions,
    MetadataRevision, Tag, Visual,
};
use symphonia::core::units::{Time, TimeBase};

use clap::{Arg, ArgMatches};
use log::{error, info, warn};

mod output;

#[cfg(not(target_os = "linux"))]
mod resampler;

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

    // Get the value of the track option, if provided.
    let track = match args.value_of("track") {
        Some(track_str) => track_str.parse::<usize>().ok(),
        _ => None,
    };

    let no_progress = args.is_present("no-progress");

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

            // Select the operating mode.
            if args.is_present("verify-only") {
                // Verify-only mode decodes and verifies the audio, but does not play it.
                decode_only(format, &AudioDecoderOptions { verify: true, ..Default::default() })
            }
            else if args.is_present("decode-only") {
                // Decode-only mode decodes the audio, but does not play or verify it.
                decode_only(format, &AudioDecoderOptions { verify: false, ..Default::default() })
            }
            else if args.is_present("probe-only") {
                // Probe-only mode only prints information about the format, tracks, metadata, etc.
                print_format(path, &mut format);
                Ok(0)
            }
            else {
                // Playback mode.
                print_format(path, &mut format);

                // If present, parse the seek argument.
                let seek = if let Some(time) = args.value_of("seek") {
                    Some(SeekPosition::Time(time.parse::<f64>().unwrap_or(0.0)))
                }
                else {
                    args.value_of("seek-ts")
                        .map(|ts| SeekPosition::Timetamp(ts.parse::<u64>().unwrap_or(0)))
                };

                // Set the decoder options.
                let decode_opts =
                    AudioDecoderOptions { verify: args.is_present("verify"), ..Default::default() };

                // Play it!
                play(format, track, seek, &decode_opts, no_progress)
            }
        }
        Err(err) => {
            // The input was not supported by any format reader.
            info!("the input is not supported");
            Err(err)
        }
    }
}

fn decode_only(
    mut reader: Box<dyn FormatReader>,
    decode_opts: &AudioDecoderOptions,
) -> Result<i32> {
    // Get the default audio track.
    // TODO: Allow track selection.
    let track = reader.default_track(TrackType::Audio).unwrap();
    let track_id = track.id;

    // Create a decoder for the track.
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(track.codec_params.as_ref().unwrap().audio().unwrap(), decode_opts)?;

    // Decode all packets, ignoring all decode errors.
    loop {
        let packet = match reader.next_packet()? {
            Some(packet) => packet,
            None => break,
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

#[derive(Copy, Clone)]
struct PlayTrackOptions {
    track_id: u32,
    seek_ts: u64,
}

fn play(
    mut reader: Box<dyn FormatReader>,
    track_num: Option<usize>,
    seek: Option<SeekPosition>,
    decode_opts: &AudioDecoderOptions,
    no_progress: bool,
) -> Result<i32> {
    // If the user provided a track number, select that track if it exists, otherwise, select the
    // default audio track.
    let track = track_num
        .and_then(|t| reader.tracks().get(t))
        .or_else(|| reader.default_track(TrackType::Audio));

    let mut track_id = match track {
        Some(track) => track.id,
        _ => return Ok(0),
    };

    // If seeking, seek the reader to the time or timestamp specified and get the timestamp of the
    // seeked position. All packets with a timestamp < the seeked position will not be played.
    //
    // Note: This is a half-baked approach to seeking! After seeking the reader, packets should be
    // decoded and *samples* discarded up-to the exact *sample* indicated by required_ts. The
    // current approach will discard excess samples if seeking to a sample within a packet.
    let seek_ts = if let Some(seek) = seek {
        let seek_to = match seek {
            SeekPosition::Time(t) => SeekTo::Time { time: Time::from(t), track_id: Some(track_id) },
            SeekPosition::Timetamp(ts) => SeekTo::TimeStamp { ts, track_id },
        };

        // Attempt the seek. If the seek fails, ignore the error and return a seek timestamp of 0 so
        // that no samples are trimmed.
        match reader.seek(SeekMode::Accurate, seek_to) {
            Ok(seeked_to) => seeked_to.required_ts,
            Err(Error::ResetRequired) => {
                print_blank();
                print_tracks(reader.tracks());
                track_id = reader.default_track(TrackType::Audio).unwrap().id;
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

    // The audio output device.
    let mut audio_output = None;

    let mut track_info = PlayTrackOptions { track_id, seek_ts };

    let result = loop {
        match play_track(&mut reader, &mut audio_output, track_info, decode_opts, no_progress) {
            Err(Error::ResetRequired) => {
                // The demuxer indicated that a reset is required. This is sometimes seen with
                // streaming OGG (e.g., Icecast) wherein the entire contents of the container change
                // (new tracks, codecs, metadata, etc.). Therefore, we must select a new track and
                // recreate the decoder.
                print_blank();
                print_tracks(reader.tracks());

                // Select the first supported track since the user's selected track number might no
                // longer be valid or make sense.
                let track_id = reader.default_track(TrackType::Audio).unwrap().id;
                track_info = PlayTrackOptions { track_id, seek_ts: 0 };
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
    play_opts: PlayTrackOptions,
    decode_opts: &AudioDecoderOptions,
    no_progress: bool,
) -> Result<i32> {
    // Get the selected track using the track ID.
    let track = match reader.tracks().iter().find(|track| track.id == play_opts.track_id) {
        Some(track) => track,
        _ => return Ok(0),
    };

    // Create a decoder for the track.
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(track.codec_params.as_ref().unwrap().audio().unwrap(), decode_opts)?;

    // Get the selected track's timebase and duration.
    let tb = track.time_base;
    let dur = track.num_frames.map(|frames| track.start_ts + frames);

    // Decode and play the packets belonging to the selected track.
    loop {
        // Get the next packet from the format reader.
        let packet = match reader.next_packet()? {
            Some(packet) => packet,
            None => break,
        };

        // If the packet does not belong to the selected track, skip it.
        if packet.track_id() != play_opts.track_id {
            continue;
        }

        //Print out new metadata.
        while !reader.metadata().is_latest() {
            reader.metadata().pop();

            if let Some(rev) = reader.metadata().current() {
                print_update(rev);
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
                if packet.ts() >= play_opts.seek_ts {
                    if !no_progress {
                        print_progress(packet.ts(), dur, tb);
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

    if !no_progress {
        println!();
    }

    info!("end of stream");

    // Finalize the decoder and return the verification result if it's been enabled.
    do_verification(decoder.finalize())
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

fn dump_visuals(format: &mut Box<dyn FormatReader>, file_name: &OsStr) {
    if let Some(metadata) = format.metadata().current() {
        for (i, visual) in metadata.visuals().iter().enumerate() {
            dump_visual(visual, file_name, i);
        }
    }
}

/// The minimum padding for tag keys.
const MIN_PAD: usize = 20;
/// The maximum padding for tag keys.
const MAX_PAD: usize = 40;

fn print_format(path: &Path, format: &mut Box<dyn FormatReader>) {
    println!("+ {}", path.display());

    let format_info = format.format_info();

    print_blank();
    print_header("Container");
    print_pair(
        "Format Name:",
        &format!("{} ({})", format_info.long_name, format_info.short_name),
        Bullet::None,
        1,
    );
    print_pair("Format ID:", &format_info.format, Bullet::None, 1);

    print_tracks(format.tracks());

    // Consume all metadata revisions up-to and including the latest.
    loop {
        if let Some(revision) = format.metadata().current() {
            print_tags(revision.tags());
            print_visuals(revision.visuals());
        }

        if format.metadata().is_latest() {
            break;
        }

        format.metadata().pop();
    }

    print_chapters(format.chapters());
    println!(":");
    println!();
}

fn print_update(rev: &MetadataRevision) {
    print_tags(rev.tags());
    print_visuals(rev.visuals());
    println!(":");
    println!();
}

fn print_tracks(tracks: &[Track]) {
    if !tracks.is_empty() {
        // Default codec registry.
        let reg = symphonia::default::get_codecs();

        print_blank();
        print_header("Tracks");

        for (idx, track) in tracks.iter().enumerate() {
            match &track.codec_params {
                Some(CodecParameters::Audio(params)) => {
                    let codec_info = reg.get_audio_decoder(params.codec).map(|d| &d.codec.info);

                    print_pair("Track Type:", &"Audio", Bullet::Num(idx + 1), 1);
                    print_pair("Codec Name:", &fmt_codec_name(codec_info), Bullet::None, 1);
                    print_pair("Codec ID:", &params.codec, Bullet::None, 1);

                    if let Some(profile) = params.profile {
                        print_pair(
                            "Profile:",
                            &fmt_codec_profile(profile, codec_info),
                            Bullet::None,
                            1,
                        );
                    }
                    if let Some(rate) = params.sample_rate {
                        print_pair("Sample Rate:", &rate, Bullet::None, 1);
                    }
                    if let Some(fmt) = params.sample_format {
                        print_pair("Sample Format:", &format!("{:?}", fmt), Bullet::None, 1);
                    }
                    if let Some(bits_per_sample) = params.bits_per_sample {
                        print_pair("Bits per Sample:", &bits_per_sample, Bullet::None, 1);
                    }
                    if let Some(channels) = &params.channels {
                        print_pair("Channel(s):", &channels.count(), Bullet::None, 1);
                        print_pair("Channel Map:", &channels, Bullet::None, 1);
                    }
                }
                Some(CodecParameters::Video(params)) => {
                    let codec_info = reg.get_video_decoder(params.codec).map(|d| &d.codec.info);

                    print_pair("Track Type:", &"Video", Bullet::Num(idx + 1), 1);
                    print_pair("Codec Name:", &fmt_codec_name(codec_info), Bullet::None, 1);
                    print_pair("Codec ID:", &params.codec, Bullet::None, 1);

                    if let Some(profile) = params.profile {
                        print_pair(
                            "Profile:",
                            &fmt_codec_profile(profile, codec_info),
                            Bullet::None,
                            1,
                        );
                    }
                    if let Some(level) = params.level {
                        print_pair("Level:", &level, Bullet::None, 1);
                    }
                    if let Some(width) = params.width {
                        print_pair("Width:", &width, Bullet::None, 1);
                    }
                    if let Some(height) = params.height {
                        print_pair("Height:", &height, Bullet::None, 1);
                    }
                }
                Some(CodecParameters::Subtitle(params)) => {
                    let codec_name = fmt_codec_name(
                        reg.get_subtitle_decoder(params.codec).map(|d| &d.codec.info),
                    );

                    print_pair("Track Type:", &"Subtitle", Bullet::Num(idx + 1), 1);
                    print_pair("Codec Name:", &codec_name, Bullet::None, 1);
                    print_pair("Codec ID:", &params.codec, Bullet::None, 1);
                }
                _ => {
                    print_pair("Track Type:", &"*Unsupported*", Bullet::Num(idx + 1), 1);
                }
            }

            if let Some(tb) = track.time_base {
                print_pair("Time Base:", &tb, Bullet::None, 1);
            }

            if track.start_ts > 0 {
                if let Some(tb) = track.time_base {
                    print_pair(
                        "Start Time:",
                        &format!("{} ({})", fmt_ts(track.start_ts, tb), track.start_ts),
                        Bullet::None,
                        1,
                    );
                }
                else {
                    print_pair("Start Time:", &track.start_ts, Bullet::None, 1);
                }
            }

            if let Some(num_frames) = track.num_frames {
                if let Some(tb) = track.time_base {
                    print_pair(
                        "Duration:",
                        &format!("{} ({})", fmt_ts(num_frames, tb), num_frames),
                        Bullet::None,
                        1,
                    );
                }
                else {
                    print_pair("Frames:", &num_frames, Bullet::None, 1);
                }
            }

            if let Some(delay) = track.delay {
                print_pair("Encoder Delay:", &delay, Bullet::None, 1);
            }

            if let Some(padding) = track.padding {
                print_pair("Encoder Padding:", &padding, Bullet::None, 1);
            }

            if let Some(language) = &track.language {
                print_pair("Language:", &language, Bullet::None, 1);
            }
        }
    }
}

fn print_chapters(chapters: Option<&ChapterGroup>) {
    if let Some(chapters) = chapters {
        print_blank();
        print_header("Chapters");

        fn print_chapter(chap: &Chapter, idx: usize, depth: usize) {
            // Chapter bounds.
            print_pair("Start Time:", &fmt_time(chap.start_time), Bullet::Num(idx), depth);
            if let Some(end_time) = chap.end_time {
                print_pair("End Time:", &fmt_time(end_time), Bullet::None, depth);
            }

            // Chapter tags.
            if !chap.tags.is_empty() {
                print_one("Tags:", Bullet::None, depth);
                let pad = optimal_tag_key_pad(&chap.tags, MIN_PAD - 5, MAX_PAD);

                for (i, tag) in chap.tags.iter().enumerate() {
                    let key = fmt_tag_key(tag);
                    print_pair_custom(&key, &tag.value, Bullet::Num(i + 1), pad, depth + 1);
                }
            }
        }

        fn print_chapter_group(group: &ChapterGroup, idx: usize, depth: usize) {
            print_one("Chapter Group:", Bullet::Num(idx), depth);

            // Chapter group tags.
            if !group.tags.is_empty() {
                print_one("Tags:", Bullet::None, depth);
                let pad = optimal_tag_key_pad(&group.tags, MIN_PAD - 5, MAX_PAD);

                for (i, tag) in group.tags.iter().enumerate() {
                    let key = fmt_tag_key(tag);
                    print_pair_custom(&key, &tag.value, Bullet::Num(i + 1), pad, depth + 1);
                }
            }

            // Chapter group items.
            print_one("Items:", Bullet::None, depth);
            for (i, item) in group.items.iter().enumerate() {
                match item {
                    ChapterGroupItem::Group(group) => print_chapter_group(group, i, depth + 1),
                    ChapterGroupItem::Chapter(chap) => print_chapter(chap, i + 1, depth + 1),
                }
            }
        }

        // Start recursion.
        print_chapter_group(chapters, 1, 1);
    }
}

fn print_tags(tags: &[Tag]) {
    if !tags.is_empty() {
        print_blank();
        print_header("Tags");

        let mut idx = 1;

        // Find maximum tag key string length, then constrain it to reasonable limits.
        let pad = optimal_tag_key_pad(tags, MIN_PAD, MAX_PAD);

        // Print tags with a standard tag key first, these are the most common tags.
        for tag in tags.iter().filter(|tag| tag.is_known()) {
            print_pair_custom(&fmt_tag_key(tag), &tag.value, Bullet::Num(idx), pad, 1);
            idx += 1;
        }

        // Print the remaining tags with keys truncated to 26 characters.
        for tag in tags.iter().filter(|tag| !tag.is_known()) {
            print_pair_custom(&fmt_tag_key(tag), &tag.value, Bullet::Num(idx), pad, 1);
            idx += 1;
        }
    }
}

fn print_visuals(visuals: &[Visual]) {
    if !visuals.is_empty() {
        print_blank();
        print_header("Visuals");

        for (idx, visual) in visuals.iter().enumerate() {
            if let Some(usage) = visual.usage {
                print_pair("Usage:", &format!("{:?}", usage), Bullet::Num(idx + 1), 1);
            }
            if let Some(media_type) = &visual.media_type {
                let bullet =
                    if visual.usage.is_some() { Bullet::None } else { Bullet::Num(idx + 1) };
                print_pair("Media Type:", media_type, bullet, 1);
            }
            if let Some(dimensions) = visual.dimensions {
                print_pair(
                    "Dimensions:",
                    &format!("{} x {} px", dimensions.width, dimensions.height),
                    Bullet::None,
                    1,
                );
            }

            match visual.color_mode {
                Some(ColorMode::Direct(model)) => {
                    print_pair("Color Mode:", &"Direct", Bullet::None, 1);
                    print_pair("Color Model:", &fmt_color_model(model), Bullet::None, 1);
                    print_pair("Bits/Pixel:", &model.bits_per_pixel(), Bullet::None, 1);
                }
                Some(ColorMode::Indexed(palette)) => {
                    print_pair("Color Mode:", &"Indexed", Bullet::None, 1);
                    print_pair("Bits/Pixel:", &palette.bits_per_pixel, Bullet::None, 1);
                    print_pair(
                        "Color Model:",
                        &fmt_color_model(palette.color_model),
                        Bullet::None,
                        1,
                    );
                }
                _ => (),
            }

            print_pair("Size:", &fmt_size(visual.data.len()), Bullet::None, 1);

            // Print out tags similar to how regular tags are printed.
            if !visual.tags.is_empty() {
                print_one("Tags:", Bullet::None, 1);

                let pad = optimal_tag_key_pad(&visual.tags, MIN_PAD - 5, MAX_PAD);

                for (tidx, tag) in visual.tags.iter().enumerate() {
                    print_pair_custom(&fmt_tag_key(tag), &tag.value, Bullet::Num(tidx + 1), pad, 2);
                }
            }
        }
    }
}

/// A list bullet.
#[allow(dead_code)]
enum Bullet {
    /// No bullet.
    None,
    /// A numbered bullet.
    Num(usize),
    /// A custom character.
    Char(char),
}

impl std::fmt::Display for Bullet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The bullet must occupy 4 characters.
        match self {
            Bullet::None => write!(f, "    "),
            Bullet::Num(num) => write!(f, "[{:0>2}]", num),
            Bullet::Char(ch) => write!(f, "   {}", ch),
        }
    }
}

/// Print one value as a plain, numbered, or bulleted list item in a hierarchical list.
fn print_one(value: &str, bullet: Bullet, depth: usize) {
    let indent = 5 * depth;
    // The format is: "|<INDENT><BULLET> <VALUE>"
    println!("|{:indent$}{} {}", "", bullet, value)
}

/// Print a key-value pair as a plain, numbered, or bulleted list item in a hierarchical list.
///
/// The key padding may be customized with `pad`.
fn print_pair_custom<T>(key: &str, value: &T, bullet: Bullet, pad: usize, depth: usize)
where
    T: std::fmt::Display,
{
    let indent = 5 * depth;
    let key = pad_key(key, pad);

    // The format is: "|<INDENT><BULLET> <KEY> "
    print!("|{:indent$}{} {} ", "", bullet, key);

    print_pair_value(&value.to_string(), indent + key.len() + 4 + 2);
}

/// Print a key-value pair as a plain, numbered, or bulleted list item in a hierarchical list with
/// default key padding.
fn print_pair<T>(key: &str, value: &T, bullet: Bullet, depth: usize)
where
    T: std::fmt::Display,
{
    print_pair_custom(key, value, bullet, MIN_PAD, depth)
}

#[inline(never)]
fn print_pair_value(value: &str, lead: usize) {
    if !value.is_empty() {
        // Print multi-line values with wrapping.
        //
        // TODO: lines() does not split on carriage returns ('\r') if a line feed ('\n') does not
        // follow. These orphan carriage returns break the output unfortunately.
        for (i, line) in value.lines().enumerate() {
            let mut chars = line.chars();

            for (j, seg) in (0..)
                .map(|_| {
                    // Try to wrap at the first whitespace character after 60 characters, or force
                    // wrapping at 80 charaters.
                    chars
                        .by_ref()
                        .enumerate()
                        .take_while(|(i, c)| *i <= 60 || *i <= 80 && !c.is_whitespace())
                        .map(|(_, c)| c)
                        .collect::<String>()
                })
                .take_while(|s| !s.is_empty())
                .enumerate()
            {
                // Print new output line prefix.
                if i > 0 || j > 0 {
                    print!("|{:lead$}", "");
                }
                // Print line-wrapping character if this is a line-wrap.
                if j > 0 {
                    print!("\u{21aa} ")
                }
                // Print sub-string.
                println!("{}", seg)
            }
        }
    }
    else {
        println!();
    }
}

/// Print a list header.
fn print_header(title: &str) {
    println!("| // {} //", title)
}

/// Print a blank list line.
fn print_blank() {
    println!("|")
}

/// Calculate the appropriate length for tag key padding.
fn optimal_tag_key_pad(tags: &[Tag], min: usize, max: usize) -> usize {
    tags.iter().map(|tag| fmt_tag_key(tag).chars().count()).max().unwrap_or(min).clamp(min, max)
}

// Format a tag's key.
fn fmt_tag_key(tag: &Tag) -> Cow<'_, str> {
    if let Some(std_key) = tag.std_key {
        Cow::Owned(format!("{:?}", std_key))
    }
    else {
        Cow::Borrowed(&tag.key)
    }
}

/// Pad a key.
fn pad_key(key: &str, pad: usize) -> String {
    if key.len() <= pad {
        format!("{:<pad$}", key)
    }
    else {
        // Key length too large.
        format!("{:.<pad$}", key.split_at(pad - 2).0)
    }
}

fn fmt_color_model(model: ColorModel) -> String {
    match model {
        ColorModel::Y(b) => format!("Y{b}"),
        ColorModel::YA(b) => format!("Y{b}A{b}"),
        ColorModel::RGB(b) => format!("R{b}G{b}B{b}"),
        ColorModel::RGBA(b) => format!("R{b}G{b}B{b}A{b}"),
        ColorModel::CMYK(b) => format!("C{b}M{b}Y{b}K{b}"),
        _ => "*Unknown*".to_string(),
    }
}

fn fmt_codec_name(info: Option<&CodecInfo>) -> String {
    match info {
        Some(info) => format!("{} ({})", info.long_name, info.short_name),
        None => "*Unknown*".to_string(),
    }
}

fn fmt_codec_profile(profile: CodecProfile, info: Option<&CodecInfo>) -> String {
    // Try to find the codec profile information.
    let profile_info = info
        .map(|codec_info| codec_info.profiles)
        .and_then(|profiles| profiles.iter().find(|profile_info| profile_info.profile == profile));

    match profile_info {
        Some(info) => format!("{} ({}) [{}]", info.long_name, info.short_name, profile.get()),
        None => format!("{}", profile.get()),
    }
}

fn fmt_size(size: usize) -> String {
    // < 1 KiB
    if size < 1 << 10 {
        // Show in Bytes
        format!("{} B", size)
    }
    // < 1 MiB
    else if size < 1 << 20 {
        // Show in Kibibytes
        format!("{:.1} KiB ({} B)", (size as f64) / 1024.0, size)
    }
    // < 1 GiB
    else if size < 1 << 30 {
        // Show in Mebibytes
        format!("{:.1} MiB ({} B)", ((size >> 10) as f64) / 1024.0, size)
    }
    // >= 1 GiB
    else {
        // Show in Gibibytes
        format!("{:.1} GiB ({} B)", ((size >> 20) as f64) / 1024.0, size)
    }
}

fn fmt_ts(ts: u64, tb: TimeBase) -> String {
    let time = tb.calc_time(ts);
    fmt_time(time)
}

fn fmt_time(time: Time) -> String {
    let hours = time.seconds / (60 * 60);
    let mins = (time.seconds % (60 * 60)) / 60;
    let secs = f64::from((time.seconds % 60) as u32) + time.frac;

    format!("{}:{:0>2}:{:0>6.3}", hours, mins, secs)
}

fn print_progress(ts: u64, dur: Option<u64>, tb: Option<TimeBase>) {
    // Get a string slice containing a progress bar.
    fn progress_bar(ts: u64, dur: u64) -> &'static str {
        const NUM_STEPS: usize = 60;

        lazy_static! {
            static ref PROGRESS_BAR: Vec<String> = {
                (0..NUM_STEPS + 1).map(|i| format!("[{:<60}]", str::repeat("■", i))).collect()
            };
        }

        let i = (NUM_STEPS as u64)
            .saturating_mul(ts)
            .checked_div(dur)
            .unwrap_or(0)
            .clamp(0, NUM_STEPS as u64);

        &PROGRESS_BAR[i as usize]
    }

    // Multiple print! calls would need to be made to print the progress, so instead, only lock
    // stdout once and use write! rather then print!.
    let stdout = std::io::stdout();
    let mut output = stdout.lock();

    if let Some(tb) = tb {
        let t = tb.calc_time(ts);

        let hours = t.seconds / (60 * 60);
        let mins = (t.seconds % (60 * 60)) / 60;
        let secs = f64::from((t.seconds % 60) as u32) + t.frac;

        write!(output, "\r\u{25b6}\u{fe0f}  {}:{:0>2}:{:0>4.1}", hours, mins, secs).unwrap();

        if let Some(dur) = dur {
            let d = tb.calc_time(dur.saturating_sub(ts));

            let hours = d.seconds / (60 * 60);
            let mins = (d.seconds % (60 * 60)) / 60;
            let secs = f64::from((d.seconds % 60) as u32) + d.frac;

            write!(output, " {} -{}:{:0>2}:{:0>4.1}", progress_bar(ts, dur), hours, mins, secs)
                .unwrap();
        }
    }
    else {
        write!(output, "\r\u{25b6}\u{fe0f}  {}", ts).unwrap();
    }

    // This extra space is a workaround for Konsole to correctly erase the previous line.
    write!(output, " ").unwrap();

    // Flush immediately since stdout is buffered.
    output.flush().unwrap();
}
