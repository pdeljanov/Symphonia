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

use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

use symphonia::core::audio::{AudioBufferRef, SampleBuffer};
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::{Error, Result};
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::{MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use clap::Arg;
use log::{info, warn};

/// The absolute maximum allowable sample delta. Around 2^-17 (-102.4dB).
const ABS_MAX_ALLOWABLE_SAMPLE_DELTA: f32 = 0.00001;

// The absolute maximum allowable sample delta for a fully compliant MP3 decoder as specified by the
// ISO. Around 2^-14 (-84.2dB).
// const ABS_MAX_ALLOWABLE_SAMPLE_DELTA_MP3: f32 = 0.00006104;

#[derive(Copy, Clone)]
enum RefDecoder {
    Ffmpeg,
    Flac,
    Mpg123,
    Oggdec,
}

impl Default for RefDecoder {
    fn default() -> Self {
        RefDecoder::Ffmpeg
    }
}

#[derive(Default)]
struct TestOptions {
    ref_decoder: RefDecoder,
    is_quiet: bool,
    is_per_sample: bool,
    stop_after_fail: bool,
    keep_going: bool,
    gapless: bool,
}

#[derive(Default)]
struct TestResult {
    n_samples: u64,
    n_failed_samples: u64,
    n_packets: u64,
    n_failed_packets: u64,
    abs_max_delta: f32,
}

fn build_ffmpeg_command(path: &str, gapless: bool) -> Command {
    let mut cmd = Command::new("ffmpeg");

    // Gapless argument must come before everything else.
    if !gapless {
        cmd.arg("-flags2").arg("skip_manual");
    }

    cmd.arg("-nostats") // Quiet command.
        .arg("-hide_banner")
        .arg("-i") // Input path.
        .arg(path)
        .arg("-map") // Select the first audio track.
        .arg("0:a:0")
        .arg("-c:a") // Encode audio to pcm_s32le.
        .arg("pcm_f32le")
        .arg("-f") // Output in WAVE format.
        .arg("wav")
        .arg("-") // Pipe output to stdout.
        .stdout(Stdio::piped())
        .stderr(Stdio::null()); // Pipe errors to null.

    cmd
}

fn build_flac_command(path: &str) -> Command {
    let mut cmd = Command::new("flac");

    cmd.arg("--stdout").arg("-d").arg(path).stdout(Stdio::piped()).stderr(Stdio::null());

    cmd
}

fn build_mpg123_command(path: &str, gapless: bool) -> Command {
    let mut cmd = Command::new("mpg123");

    if !gapless {
        cmd.arg("--no-gapless");
    }

    cmd.arg("--wav").arg("-").arg("--float").arg(path).stdout(Stdio::piped()).stderr(Stdio::null());

    cmd
}

fn build_oggdec_command(path: &str) -> Command {
    let mut cmd = Command::new("oggdec");
    cmd.arg(path).arg("-o").arg("-").stdout(Stdio::piped()).stderr(Stdio::null());
    cmd
}

struct RefProcess {
    child: std::process::Child,
}

impl RefProcess {
    fn try_spawn(decoder: RefDecoder, gapless: bool, path: &str) -> Result<RefProcess> {
        let mut cmd = match decoder {
            RefDecoder::Ffmpeg => build_ffmpeg_command(path, gapless),
            RefDecoder::Flac => build_flac_command(path),
            RefDecoder::Mpg123 => build_mpg123_command(path, gapless),
            RefDecoder::Oggdec => build_oggdec_command(path),
        };

        let child = cmd.spawn()?;
        Ok(RefProcess { child })
    }
}

struct DecoderInstance {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
}

impl DecoderInstance {
    fn try_open(mss: MediaSourceStream, fmt_opts: &FormatOptions) -> Result<DecoderInstance> {
        // Use the default options for metadata and format readers, and the decoder.
        let meta_opts: MetadataOptions = Default::default();
        let dec_opts: DecoderOptions = Default::default();

        let hint = Hint::new();

        let probed = symphonia::default::get_probe().format(&hint, mss, fmt_opts, &meta_opts)?;
        let format = probed.format;

        let track = format.default_track().unwrap();

        let decoder = symphonia::default::get_codecs().make(&track.codec_params, &dec_opts)?;

        let track_id = track.id;

        Ok(DecoderInstance { format, decoder, track_id })
    }
}

fn get_next_audio_buf(inst: &mut DecoderInstance) -> Result<AudioBufferRef<'_>> {
    let pkt = loop {
        // Get next packet.
        let pkt = inst.format.next_packet()?;

        // Ensure packet is from the correct track.
        if pkt.track_id() == inst.track_id {
            break pkt;
        }
    };

    // Decode packet audio.
    inst.decoder.decode(&pkt)
}

fn get_next_audio_buf_best_effort(inst: &mut DecoderInstance) -> Result<()> {
    loop {
        match get_next_audio_buf(inst) {
            Ok(_) => break Ok(()),
            Err(Error::DecodeError(err)) => warn!("{}", err),
            Err(err) => break Err(err),
        }
    }
}

fn copy_audio_buf_to_sample_buf(src: AudioBufferRef<'_>, dst: &mut Option<SampleBuffer<f32>>) {
    if dst.is_none() {
        let spec = *src.spec();
        let duration = src.capacity() as u64;

        info!(
            "created target raw audio buffer with rate={}, channels={}, duration={}",
            spec.rate,
            spec.channels.count(),
            duration
        );

        *dst = Some(SampleBuffer::<f32>::new(duration, spec));
    }

    let dst_raw_audio = dst.as_mut().unwrap();

    dst_raw_audio.copy_interleaved_ref(src)
}

fn run_check(
    ref_inst: &mut DecoderInstance,
    tgt_inst: &mut DecoderInstance,
    opts: &TestOptions,
    acct: &mut TestResult,
) -> Result<()> {
    // Reference
    let mut ref_sample_buf = None;
    let mut ref_sample_cnt = 0;
    let mut ref_sample_pos = 0;

    // Target
    let mut tgt_sample_buf = None;

    loop {
        // Decode target's next audio buffer.
        if opts.keep_going {
            get_next_audio_buf_best_effort(tgt_inst)?;
        }
        else {
            get_next_audio_buf(tgt_inst)?;
        }

        // Copy to the target's audio buffer into the target sample buffer.
        copy_audio_buf_to_sample_buf(tgt_inst.decoder.last_decoded(), &mut tgt_sample_buf);

        // Get a slice of the target sample buffer.
        let mut tgt_samples = tgt_sample_buf.as_mut().unwrap().samples();

        // The number of samples previously read & compared.
        let sample_num_base = acct.n_samples;

        // The number of failed samples in the target packet.
        let mut n_failed_pkt_samples = 0;

        while !tgt_samples.is_empty() {
            // Need to read a new reference packet.
            if ref_sample_pos == ref_sample_cnt {
                // Get next reference audio buffer.
                get_next_audio_buf_best_effort(ref_inst)?;

                // Copy to reference audio buffer to reference sample buffer.
                copy_audio_buf_to_sample_buf(ref_inst.decoder.last_decoded(), &mut ref_sample_buf);

                // Save number of reference samples in the sample buffer and reset the sample buffer
                // position counter.
                ref_sample_cnt = ref_sample_buf.as_ref().unwrap().len();
                ref_sample_pos = 0;
            }

            // The reference sample audio buffer.
            let ref_samples = &ref_sample_buf.as_mut().unwrap().samples()[ref_sample_pos..];

            // The number of samples that can be compared given the current state of the reference
            // and target sample buffers.
            let n_test_samples = std::cmp::min(ref_samples.len(), tgt_samples.len());

            // Perform the comparison.
            for (&t, &r) in tgt_samples[..n_test_samples].iter().zip(&ref_samples[..n_test_samples])
            {
                // Clamp the reference and target samples between [-1.0, 1.0] and find the
                // difference.
                let delta = t.clamp(-1.0, 1.0) - r.clamp(-1.0, 1.0);

                if delta.abs() > ABS_MAX_ALLOWABLE_SAMPLE_DELTA {
                    // Print per-sample or per-packet failure nessage based on selected options.
                    if !opts.is_quiet && (opts.is_per_sample || n_failed_pkt_samples == 0) {
                        println!(
                            "[FAIL] packet={:>6}, sample_num={:>12} ({:>4}), dec={:+.8}, ref={:+.8} ({:+.8})",
                            acct.n_packets,
                            acct.n_samples,
                            acct.n_samples - sample_num_base,
                            t,
                            r,
                            r - t
                        );
                    }

                    n_failed_pkt_samples += 1;
                }

                acct.abs_max_delta = acct.abs_max_delta.max(delta.abs());
                acct.n_samples += 1;
            }

            // Update position in reference buffer.
            ref_sample_pos += n_test_samples;

            // Update slice to compare next round.
            tgt_samples = &tgt_samples[n_test_samples..];
        }

        acct.n_failed_samples += n_failed_pkt_samples;
        acct.n_failed_packets += if n_failed_pkt_samples > 0 { 1 } else { 0 };
        acct.n_packets += 1;

        if opts.stop_after_fail && acct.n_failed_packets > 0 {
            break;
        }
    }

    Ok(())
}

fn run_test(path: &str, opts: &TestOptions, result: &mut TestResult) -> Result<()> {
    // 1. Start the reference decoder process.
    let mut ref_process = RefProcess::try_spawn(opts.ref_decoder, opts.gapless, path)?;

    // 2. Instantiate a Symphonia decoder for the reference process output.
    let ref_ms = Box::new(ReadOnlySource::new(ref_process.child.stdout.take().unwrap()));
    let ref_mss = MediaSourceStream::new(ref_ms, Default::default());

    let mut ref_inst = DecoderInstance::try_open(ref_mss, &Default::default())?;

    // 3. Instantiate a Symphonia decoder for the test target.
    let tgt_ms = Box::new(File::open(Path::new(path))?);
    let tgt_mss = MediaSourceStream::new(tgt_ms, Default::default());

    let tgt_fmt_opts = FormatOptions { enable_gapless: opts.gapless, ..Default::default() };

    let mut tgt_inst = DecoderInstance::try_open(tgt_mss, &tgt_fmt_opts)?;

    // 4. Begin check.
    run_check(&mut ref_inst, &mut tgt_inst, opts, result)
}

fn main() {
    pretty_env_logger::init();

    let matches = clap::Command::new("Symphonia Check")
        .version("1.0")
        .author("Philip Deljanov <philip.deljanov@gmail.com>")
        .about("Check Symphonia output with a reference decoding")
        .arg(Arg::new("samples").long("samples").help("Print failures per sample"))
        .arg(
            Arg::new("stop-after-fail")
                .long("first-fail")
                .short('f')
                .help("Stop testing after the first failed packet"),
        )
        .arg(Arg::new("quiet").long("quiet").short('q').help("Only print test results"))
        .arg(
            Arg::new("keep-going")
                .long("keep-going")
                .help("Continue after a decode error (may cause many failures)"),
        )
        .arg(
            Arg::new("decoder")
                .long("ref")
                .takes_value(true)
                .possible_values(&["ffmpeg", "flac", "mpg123", "oggdec"])
                .default_value("ffmpeg")
                .help("Specify a particular decoder to be used as the reference"),
        )
        .arg(Arg::new("no-gapless").long("no-gapless").help("Disable gapless decoding"))
        .arg(Arg::new("INPUT").help("The input file path").required(true).index(1))
        .get_matches();

    let path = matches.value_of("INPUT").unwrap();

    let ref_decoder = match matches.value_of("decoder").unwrap() {
        "ffmpeg" => RefDecoder::Ffmpeg,
        "flac" => RefDecoder::Flac,
        "mpg123" => RefDecoder::Mpg123,
        "oggdec" => RefDecoder::Oggdec,
        _ => {
            // This will never occur if the possible values of the argument are the same as the
            // match arms above.
            unreachable!()
        }
    };

    let opts = TestOptions {
        ref_decoder,
        is_per_sample: matches.is_present("samples"),
        is_quiet: matches.is_present("quiet"),
        stop_after_fail: matches.is_present("stop-after-fail"),
        keep_going: matches.is_present("keep-going"),
        gapless: !matches.is_present("no-gapless"),
        ..Default::default()
    };

    let mut res: TestResult = Default::default();

    println!("Input Path: {}", path);
    println!();

    match run_test(path, &opts, &mut res) {
        Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => (),
        Err(err) => {
            eprintln!("Test interrupted by error: {}", err);
            std::process::exit(2);
        }
        _ => (),
    };

    if !opts.is_quiet {
        println!();
    }

    println!("Test Results");
    println!("=================================================");
    println!();
    println!("  Failed/Total Packets: {:>12}/{:>12}", res.n_failed_packets, res.n_packets);
    println!("  Failed/Total Samples: {:>12}/{:>12}", res.n_failed_samples, res.n_samples);
    println!();
    println!("  Absolute Maximum Sample Delta:       {:.8}", res.abs_max_delta);
    println!();

    let ret = if res.n_failed_samples == 0 {
        println!("PASS");
        0
    }
    else {
        println!("FAIL");
        1
    };
    println!();

    std::process::exit(ret);
}
