// Symphonia Check Tool
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]
// Justification: Fields on DecoderOptions and FormatOptions may change at any time, but
// symphonia-check doesn't want to be updated every time those fields change, therefore always fill
// in the remaining fields with default values.
#![allow(clippy::needless_update)]

use std::{io::ErrorKind, process::Command};

use audio::run_audio;
use clap::{Parser, Subcommand, ValueEnum};
use info::run_info;
use log::warn;
use symphonia::core::errors::{Error, Result};

mod audio;
mod info;

#[derive(Parser, Debug)]
#[command(
    author = "Philip Deljanov <philip.deljanov@gmail.com>",
    version = "1.0",
    about = "Check Symphonia output with a reference decoding"
)]
struct Cli {
    /// Select the check mode
    #[command(subcommand)]
    mode: CheckMode,
}

#[derive(Subcommand, Debug)]
enum CheckMode {
    /// Check Symphonia info output with a reference decoder (mediainfo)
    Info(InfoTestOptions),

    /// Check Symphonia audio output with a reference decoder (ffmpeg or other)
    Audio(AudioTestOptions),
}

#[derive(Parser, Debug)]
struct InfoTestOptions {
    /// Specify a particular decoder to be used as the reference
    #[arg(long = "ref", value_enum, default_value = "mediainfo")]
    ref_decoder: InfoTestDecoder,

    /// The input file path
    #[arg(required = true)]
    input: String,
}

#[derive(Parser, Debug)]
struct AudioTestOptions {
    /// Print failures per sample
    #[arg(long = "samples")]
    is_per_sample: bool,

    /// Stop testing after the first failed packet
    #[arg(long = "first-fail", short = 'f')]
    stop_after_fail: bool,

    /// Only print test results
    #[arg(long = "quiet", short = 'q')]
    is_quiet: bool,

    /// Continue after a decode error (may cause many failures)
    #[arg(long)]
    keep_going: bool,

    /// Specify a particular decoder to be used as the reference
    #[arg(long = "ref", value_enum, default_value = "ffmpeg")]
    ref_decoder: AudioTestDecoder,

    /// Disable gapless decoding
    #[arg(long = "no-gapless", action = clap::ArgAction::SetFalse, default_value_t = true)]
    gapless: bool,

    /// The input file path
    #[arg(required = true)]
    input: String,
}

#[derive(ValueEnum, Clone, Debug)]
enum AudioTestDecoder {
    Ffmpeg,
    Flac,
    Mpg123,
    Oggdec,
}

#[derive(ValueEnum, Clone, Debug)]
enum InfoTestDecoder {
    Mediainfo,
}

struct RefProcess {
    child: std::process::Child,
}

impl RefProcess {
    fn try_spawn(mut cmd: Command) -> Result<RefProcess> {
        let child = cmd.spawn().map_err(|e| {
            if e.kind() == ErrorKind::NotFound {
                std::io::Error::new(
                    e.kind(),
                    format!("file not found in PATH: {:?}", cmd.get_program()),
                )
            }
            else {
                e
            }
        })?;
        Ok(RefProcess { child })
    }
}

fn main() {
    pretty_env_logger::init();
    let cli = Cli::parse();

    let result = match cli.mode {
        CheckMode::Info(options) => {
            println!("Input Path: {}", options.input);
            run_info(options)
        }
        CheckMode::Audio(options) => {
            println!("Input Path: {}", options.input);
            run_audio(options)
        }
    };

    match result {
        // test was succesfull
        Ok(_) => {
            println!("PASS");
            std::process::exit(0);
        }
        // ref_output and file were processed succesfully, but data doesn't match
        Err(Error::Unsupported(msg)) => {
            println!();
            eprintln!("FAIL: {}", msg);
            std::process::exit(1);
        }
        // test was interrupted by some processing failure
        Err(err) => {
            println!();
            eprintln!("FAIL: Test interrupted by error: {}", err);
            std::process::exit(2);
        }
    }
}
