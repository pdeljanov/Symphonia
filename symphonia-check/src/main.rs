// Symphonia Check Tool
// Copyright (c) 2021 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

// Justification: fields on DecoderOptions and FormatOptions may change at any time, but
// symphonia-check doesn't want to be updated every time those fields change, therefore always fill
// in the remaining fields with default values.
#![allow(clippy::needless_update)]

use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

use symphonia;
use symphonia::core::audio::{AudioBufferRef, SampleBuffer};
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::{Error, Result};
use symphonia::core::formats::{FormatReader, FormatOptions};
use symphonia::core::io::{MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Duration;

use clap::{Arg, App};
use log::{info, warn};
use pretty_env_logger;

#[derive(Default)]
struct TestOptions {
    is_quiet: bool,
    is_per_sample: bool,
    stop_after_fail: bool,
}

#[derive(Default)]
struct TestResult {
    n_samples: u64,
    n_failed_samples: u64,
    n_packets: u64,
    n_failed_packets: u64,
}

struct RefProcess {
    child: std::process::Child,
}

impl RefProcess {
    fn try_spawn(path: &str) -> Result<RefProcess> {
        let mut cmd = Command::new("ffmpeg");
        let cmd = cmd.arg("-flags2") // Do not trim encoder delay.
                     .arg("skip_manual")
                     .arg("-i")      // File path.
                     .arg(path)
                     .arg("-map")    // Select the first audio track.
                     .arg("0:a:0")
                     .arg("-c:a")    // Encode audio to pcm_s32le.
                     .arg("pcm_f32le")
                     .arg("-f")      // Output in WAVE format.
                     .arg("wav")
                     .arg("-")       // Pipe output to stdout.
                     .stdout(Stdio::piped())
                     .stderr(Stdio::null());    // Pipe errors to null.

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
    fn try_open(mss: MediaSourceStream) -> Result<DecoderInstance> {
        // Use the default options for metadata and format readers, and the decoder.
        let fmt_opts: FormatOptions = Default::default();
        let meta_opts: MetadataOptions = Default::default();
        let dec_opts: DecoderOptions = Default::default();

        let hint = Hint::new();

        let probed = symphonia::default::get_probe().format(&hint, mss, &fmt_opts, &meta_opts)?;
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

fn copy_audio_buf_to_sample_buf(src: AudioBufferRef<'_>, dst: &mut Option<SampleBuffer<f32>>) {
    if dst.is_none() {
        let spec = src.spec().clone();
        let duration = Duration::from(src.capacity() as u64);

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
        let tgt_audio = get_next_audio_buf(tgt_inst)?;

        // Copy to the target's audio buffer into the target sample buffer.
        copy_audio_buf_to_sample_buf(tgt_audio, &mut tgt_sample_buf);

        // Get a slice of the target sample buffer.
        let mut tgt_samples = tgt_sample_buf.as_mut().unwrap().samples();

        // The number of samples previously read & compared.
        let sample_num_base = acct.n_samples;

        // The number of failed samples in the target packet.
        let mut n_failed_pkt_samples = 0;

        while tgt_samples.len() > 0 {
            // Need to read a new reference packet.
            if ref_sample_pos == ref_sample_cnt {
                // Get next reference audio buffer.
                let ref_audio = get_next_audio_buf(ref_inst)?;

                // Copy to reference audio buffer to reference sample buffer.
                copy_audio_buf_to_sample_buf(ref_audio, &mut ref_sample_buf);

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
            for (&t, &r) in tgt_samples[..n_test_samples].iter()
                                                         .zip(&ref_samples[..n_test_samples])
            {
                // Clamp the reference and target samples between [-1.0, 1.0] and find the difference.
                let delta = t.clamp(-1.0, 1.0) - r.clamp(-1.0, 1.0);

                if delta.abs() > 0.00001 {

                    // Print per-sample or per-packet failure nessage based on selected options.
                    if !opts.is_quiet {
                        if opts.is_per_sample || n_failed_pkt_samples == 0 {
                            eprintln!(
                                "[FAIL] packet={:>6}, sample_num={:>12} ({:>4}), dec={:+.6}, ref={:+.6} ({:+.6})",
                                acct.n_packets,
                                acct.n_samples,
                                acct.n_samples - sample_num_base,
                                t,
                                r,
                                r - t
                            );
                        }
                    }

                    n_failed_pkt_samples += 1;
                }

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
    let mut ref_process = RefProcess::try_spawn(path)?;

    // 2. Instantiate a Symphonia decoder for the reference process output.
    let ref_ms = Box::new(ReadOnlySource::new(ref_process.child.stdout.take().unwrap()));
    let ref_mss = MediaSourceStream::new(ref_ms, Default::default());

    let mut ref_inst = DecoderInstance::try_open(ref_mss)?;

    // 3. Instantiate a Symphonia decoder for the test target.
    let tgt_ms = Box::new(File::open(Path::new(path))?);
    let tgt_mss = MediaSourceStream::new(tgt_ms, Default::default());

    let mut tgt_inst = DecoderInstance::try_open(tgt_mss)?;

    // 4. Begin check.
    run_check(&mut ref_inst, &mut tgt_inst, opts, result)
}

fn main() {
    pretty_env_logger::init();

    let matches = App::new("Symphonia Check")
                        .version("1.0")
                        .author("Philip Deljanov <philip.deljanov@gmail.com>")
                        .about("Check Symphonia output with a reference decoding")
                        .arg(Arg::with_name("samples")
                            .long("samples")
                            .help("Print failures per sample"))
                        .arg(Arg::with_name("stop-after-fail")
                            .long("first-fail")
                            .short("f")
                            .help("Stop testing after the first failed packet"))
                        .arg(Arg::with_name("quiet")
                            .long("quiet")
                            .short("q")
                            .help("Only print test results"))
                        .arg(Arg::with_name("INPUT")
                            .help("The input file path")
                            .required(true)
                            .index(1))
                        .get_matches();

    let path = matches.value_of("INPUT").unwrap();

    let opts = TestOptions {
        is_per_sample: matches.is_present("samples"),
        is_quiet: matches.is_present("quiet"),
        stop_after_fail: matches.is_present("stop-after-fail"),
        ..Default::default()
    };

    let mut res: TestResult = Default::default();

    if !opts.is_quiet {
        println!("Input Path: {}", path);
        println!("");
    }

    match run_test(path, &opts, &mut res) {
        Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => (),
        Err(err) => {
            eprintln!("Test interrupted by error: {}", err);
            std::process::exit(2);
        },
        _ => (),
    };

    if !opts.is_quiet {
        println!("");
    }

    println!("Test Results");
    println!("=================================================");
    println!("");
    println!("  Failed/Total Packets: {:>12}/{:>12}", res.n_failed_packets, res.n_packets);
    println!("  Failed/Total Samples: {:>12}/{:>12}", res.n_failed_samples, res.n_samples);
    println!("");

    let ret = if res.n_failed_samples == 0 {
        println!("PASS");
        0
    }
    else {
        println!("FAIL");
        1
    };

    std::process::exit(ret);
}





