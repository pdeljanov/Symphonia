// Symphonia Check Tool
// Copyright (c) 2019-2026 The Project Symphonia Developers.
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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::SeekErrorKind;
use symphonia::core::errors::{Error, Result, unsupported_error};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, Timestamp};

use clap::{Arg, ArgAction};
use log::warn;

// The absolute maximum allowable sample delta for a fully compliant MP3 decoder as specified by the
// ISO. Around 2^-14 (-84.2dB).
// const ABS_MAX_ALLOWABLE_SAMPLE_DELTA_MP3: f32 = 0.00006104;

#[derive(Clone, Default)]
enum RefDecoder {
    #[default]
    Ffmpeg,
    /// A pre-decoded WAVE file.
    Wav(PathBuf),
    Flac,
    Mpg123,
    Oggdec,
}

#[derive(Default)]
struct TestOptions {
    ref_decoder: RefDecoder,
    is_quiet: bool,
    is_per_sample: bool,
    stop_after_fail: bool,
    keep_going: bool,
    gapless: bool,
    tolerance: f32,
    dump: Option<PathBuf>,
}

#[derive(Default)]
struct TestResult {
    n_frames: u64,
    n_samples: u64,
    n_failed_samples: u64,
    n_packets: u64,
    n_failed_packets: u64,
    abs_max_delta: f32,
    tgt_unchecked_samples: u64,
    ref_unchecked_samples: u64,
}

fn build_ffmpeg_command(path: &Path, gapless: bool) -> Command {
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

fn build_cat_command(path: &Path) -> Command {
    let mut cmd = Command::new("cat");
    cmd.arg(path).stdout(Stdio::piped()).stderr(Stdio::null());
    cmd
}

fn build_flac_command(path: &Path) -> Command {
    let mut cmd = Command::new("flac");

    cmd.arg("--stdout").arg("-d").arg(path).stdout(Stdio::piped()).stderr(Stdio::null());

    cmd
}

fn build_mpg123_command(path: &Path, gapless: bool) -> Command {
    let mut cmd = Command::new("mpg123");

    if !gapless {
        cmd.arg("--no-gapless");
    }

    cmd.arg("--wav").arg("-").arg("--float").arg(path).stdout(Stdio::piped()).stderr(Stdio::null());

    cmd
}

fn build_oggdec_command(path: &Path) -> Command {
    let mut cmd = Command::new("oggdec");
    cmd.arg(path).arg("-o").arg("-").stdout(Stdio::piped()).stderr(Stdio::null());
    cmd
}

struct RefProcess {
    child: std::process::Child,
}

impl RefProcess {
    fn try_spawn(decoder: RefDecoder, gapless: bool, path: &Path) -> Result<RefProcess> {
        let mut cmd = match decoder {
            RefDecoder::Ffmpeg => build_ffmpeg_command(path, gapless),
            RefDecoder::Wav(wav) => build_cat_command(&wav),
            RefDecoder::Flac => build_flac_command(path),
            RefDecoder::Mpg123 => build_mpg123_command(path, gapless),
            RefDecoder::Oggdec => build_oggdec_command(path),
        };

        let child = cmd.spawn()?;
        Ok(RefProcess { child })
    }
}

#[derive(Default)]
struct FlushStats {
    n_packets: u64,
    n_samples: u64,
}

struct DecoderInstance {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    dec_opts: AudioDecoderOptions,
}

impl DecoderInstance {
    fn try_open(
        mss: MediaSourceStream<'static>,
        fmt_opts: FormatOptions,
        dec_opts: AudioDecoderOptions,
    ) -> Result<DecoderInstance> {
        // Use the default options for metadata and format readers, and the decoder.
        let meta_opts: MetadataOptions = Default::default();

        let hint = Hint::new();

        let format = symphonia::default::get_probe().probe(&hint, mss, fmt_opts, meta_opts)?;

        let track = match format.default_track(TrackType::Audio) {
            Some(t) => t,
            None => return unsupported_error("only audio tracks are supported"),
        };

        let codec_params = match &track.codec_params {
            Some(CodecParameters::Audio(params)) => params,
            _ => return unsupported_error("only audio tracks are supported"),
        };

        let decoder =
            symphonia::default::get_codecs().make_audio_decoder(codec_params, &dec_opts)?;

        let track_id = track.id;

        Ok(DecoderInstance { format, decoder, track_id, dec_opts })
    }

    /// Recreate the decoder from the track of the format reader, after the reader signalled that
    /// the tracks changed (e.g., the next link of a chained Ogg stream).
    fn reset_decoder(&mut self) -> Result<()> {
        let track = match self.format.default_track(TrackType::Audio) {
            Some(t) => t,
            None => return unsupported_error("only audio tracks are supported"),
        };

        let codec_params = match &track.codec_params {
            Some(CodecParameters::Audio(params)) => params,
            _ => return unsupported_error("only audio tracks are supported"),
        };

        self.decoder =
            symphonia::default::get_codecs().make_audio_decoder(codec_params, &self.dec_opts)?;
        self.track_id = track.id;

        Ok(())
    }

    fn samples_per_frame(&self) -> Option<u64> {
        self.decoder.codec_params().channels.as_ref().map(|ch| ch.count() as u64)
    }

    fn next_audio_buf(&mut self, keep_going: bool) -> Result<Option<GenericAudioBufferRef<'_>>> {
        loop {
            // Get the next packet.
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(None),
                Err(Error::ResetRequired) => {
                    self.reset_decoder()?;
                    continue;
                }
                Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                    // WavReader will always return an UnexpectedEof when it ends because the
                    // reference decoder is piping the decoded audio and cannot write out the
                    // actual length of the media. Treat UnexpectedEof as the end of the stream.
                    return Ok(None);
                }
                Err(err) => return Err(err),
            };

            // Skip packets that do not belong to the track being decoded.
            if packet.track_id != self.track_id {
                continue;
            }

            // Decode the packet, ignoring decode errors if `keep_going` is true.
            match self.decoder.decode(&packet) {
                Ok(_) => break,
                Err(Error::DecodeError(err)) if keep_going => warn!("{err}"),
                Err(err) => return Err(err),
            }
        }

        Ok(Some(self.decoder.last_decoded()))
    }

    fn flush(&mut self, keep_going: bool) -> Result<FlushStats> {
        let mut stats: FlushStats = Default::default();

        while let Some(buf) = self.next_audio_buf(keep_going)? {
            stats.n_packets += 1;
            stats.n_samples += buf.samples_interleaved() as u64;
        }

        Ok(stats)
    }
}

fn run_check(
    ref_inst: &mut DecoderInstance,
    tgt_inst: &mut DecoderInstance,
    opts: &TestOptions,
    acct: &mut TestResult,
) -> Result<()> {
    // Reference
    let mut ref_sample_buf: Vec<f32> = Default::default();
    let mut ref_sample_cnt = 0;
    let mut ref_sample_pos = 0;

    // Target
    let mut tgt_sample_buf: Vec<f32> = Default::default();
    let mut tgt_sample_cnt = 0;
    let mut tgt_sample_pos = 0;

    // The target's output channels may only be known once the first packet has been decoded (e.g.,
    // an implicit parametric stereo upmix), so verify the samples/frame match after that.
    let mut samples_per_frame: u64;
    let ref_samples_per_frame = ref_inst.samples_per_frame().unwrap_or(1);
    let mut verified_samples_per_frame = false;

    let mut dump =
        opts.dump.as_ref().map(std::fs::File::create).transpose()?.map(std::io::BufWriter::new);

    let early_fail = 'outer: loop {
        // Decode the next target audio buffer and copy it to the target sample buffer.
        match tgt_inst.next_audio_buf(opts.keep_going)? {
            Some(buf) => {
                samples_per_frame = buf.spec().channels().count() as u64;
                buf.copy_to_vec_interleaved(&mut tgt_sample_buf)
            }
            None => break 'outer false,
        };

        // Samples/frame must match for both decoders.
        if !verified_samples_per_frame {
            if samples_per_frame != ref_samples_per_frame {
                return unsupported_error(
                    "target and reference decoder samples per frame mismatch",
                );
            }
            verified_samples_per_frame = true;
        }

        if let Some(dump) = dump.as_mut() {
            for sample in &tgt_sample_buf {
                dump.write_all(&sample.to_le_bytes())?;
            }
        }

        tgt_sample_cnt = tgt_sample_buf.len();
        tgt_sample_pos = 0;

        // The number of frames previously read & compared.
        let frame_num_base = acct.n_frames;

        // The number of failed samples in the target packet.
        let mut n_failed_pkt_samples = 0;

        while tgt_sample_pos < tgt_sample_cnt {
            // Need to read a decode a new reference buffer.
            if ref_sample_pos == ref_sample_cnt {
                // Get the next reference audio buffer and copy it to the reference sample buffer.
                match ref_inst.next_audio_buf(true)? {
                    Some(buf) => buf.copy_to_vec_interleaved(&mut ref_sample_buf),
                    None => break 'outer false,
                }

                ref_sample_cnt = ref_sample_buf.len();
                ref_sample_pos = 0;
            }

            // Get a slice of the remaining samples in the reference and target sample buffers.
            let ref_samples = &ref_sample_buf[ref_sample_pos..];
            let tgt_samples = &tgt_sample_buf[tgt_sample_pos..];

            // The number of samples that can be compared given the current length of the reference
            // and target sample buffers.
            let n_test_samples = std::cmp::min(ref_samples.len(), tgt_samples.len());

            // Perform the comparison.
            for (&t, &r) in tgt_samples[..n_test_samples].iter().zip(&ref_samples[..n_test_samples])
            {
                // Clamp the reference and target samples between [-1.0, 1.0] and find the
                // difference.
                let delta = t.clamp(-1.0, 1.0) - r.clamp(-1.0, 1.0);

                if delta.abs() > opts.tolerance {
                    // Print per-sample or per-packet failure nessage based on selected options.
                    if !opts.is_quiet && (opts.is_per_sample || n_failed_pkt_samples == 0) {
                        println!(
                            "[FAIL] packet={:>8}, frame={:>10} ({:>4}), plane={:>3}, dec={:+.8}, ref={:+.8} ({:+.8})",
                            acct.n_packets,
                            acct.n_frames,
                            acct.n_frames - frame_num_base,
                            acct.n_samples % samples_per_frame,
                            t,
                            r,
                            r - t
                        );
                    }

                    n_failed_pkt_samples += 1;
                }

                acct.abs_max_delta = acct.abs_max_delta.max(delta.abs());
                acct.n_samples += 1;

                if acct.n_samples % samples_per_frame == 0 {
                    acct.n_frames += 1;
                }
            }

            // Update position in reference and target buffers.
            ref_sample_pos += n_test_samples;
            tgt_sample_pos += n_test_samples;
        }

        acct.n_failed_samples += n_failed_pkt_samples;
        acct.n_failed_packets += u64::from(n_failed_pkt_samples > 0);
        acct.n_packets += 1;

        if opts.stop_after_fail && acct.n_failed_packets > 0 {
            break true;
        }
    };

    // Count how many samples were remaining for both the target and references if the loop did not
    // break out early due to a failed sample.
    if !early_fail {
        let tgt_stats = tgt_inst.flush(true)?;
        let ref_stats = ref_inst.flush(true)?;

        acct.n_packets += tgt_stats.n_packets;
        acct.tgt_unchecked_samples = (tgt_sample_cnt - tgt_sample_pos) as u64 + tgt_stats.n_samples;
        acct.ref_unchecked_samples = (ref_sample_cnt - ref_sample_pos) as u64 + ref_stats.n_samples;
    }

    Ok(())
}

/// Seek to a time, then decode the remainder of the stream, writing the decoded audio, starting
/// from the exact seeked-to frame, as interleaved f32le samples.
fn run_seek_dump(path: &Path, time: f64, dump: &Path, gapless: bool) -> Result<()> {
    let tgt_mss = MediaSourceStream::new(Box::new(File::open(path)?), Default::default());
    let dec_opts = AudioDecoderOptions::default().gapless(gapless);

    let mut inst = DecoderInstance::try_open(tgt_mss, Default::default(), dec_opts)?;

    let seek_to = SeekTo::Time {
        time: Time::try_from_secs_f64(time).ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?,
        track_id: Some(inst.track_id),
    };

    // Decode the start of the stream first, as a player that seeks while playing would have. Some
    // decoders keep stream parameters (e.g., the SBR header) that only appear at the start.
    for _ in 0..8 {
        match inst.format.next_packet() {
            Ok(Some(packet)) if packet.track_id == inst.track_id => {
                let _ = inst.decoder.decode(&packet);
            }
            Ok(Some(_)) => (),
            _ => break,
        }
    }

    let seeked_to = inst.format.seek(SeekMode::Accurate, seek_to)?;
    inst.decoder.reset();

    let time_base = inst
        .format
        .tracks()
        .iter()
        .find(|track| track.id == inst.track_id)
        .and_then(|track| track.time_base)
        .ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;

    let mut out = std::io::BufWriter::new(File::create(dump)?);
    let mut samples: Vec<f32> = Vec::new();
    let mut channels = 0;

    // Timestamps are in the units of the timebase, which may differ from the sampling rate of the
    // decoded audio (e.g., for SBR). Convert them to frames of the decoded audio.
    let to_frames = |ts: i64, rate: u32| {
        let secs = time_base.calc_time(Timestamp::new(ts));
        secs.map(|time| (time.as_secs_f64() * f64::from(rate)).round() as i64).unwrap_or(0)
    };

    let mut required_frame = None;

    loop {
        let packet = match inst.format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(err),
        };

        if packet.track_id != inst.track_id {
            continue;
        }

        // The decoded frames of a gapless packet begin after the trimmed frames.
        let pts = packet.pts.get() + if gapless { packet.trim_start.get() as i64 } else { 0 };

        let buf = match inst.decoder.decode(&packet) {
            Ok(buf) => buf,
            Err(Error::DecodeError(err)) => {
                warn!("{err}");
                continue;
            }
            Err(err) => return Err(err),
        };

        channels = buf.spec().channels().count();
        let rate = buf.spec().rate();
        buf.copy_to_vec_interleaved(&mut samples);

        // The first sample that should be output is the one that was required.
        let required =
            *required_frame.get_or_insert_with(|| to_frames(seeked_to.required_ts.get(), rate));

        let skip =
            (required - to_frames(pts, rate)).clamp(0, (samples.len() / channels.max(1)) as i64);

        for sample in &samples[skip as usize * channels..] {
            out.write_all(&sample.to_le_bytes())?;
        }
    }

    println!(
        "required_ts={} actual_ts={} channels={channels}",
        required_frame.unwrap_or(0),
        seeked_to.actual_ts
    );

    Ok(())
}

fn run_test(path: &Path, opts: &TestOptions, result: &mut TestResult) -> Result<()> {
    // 1. Start the reference decoder process.
    let mut ref_process = RefProcess::try_spawn(opts.ref_decoder.clone(), opts.gapless, path)?;

    // 2. Instantiate a Symphonia decoder for the reference process output.
    let ref_ms = Box::new(ReadOnlySource::new(match ref_process.child.stdout.take() {
        Some(s) => s,
        None => return unsupported_error("reference decoder has no stdout"),
    }));
    let ref_mss = MediaSourceStream::new(ref_ms, Default::default());

    let mut ref_inst = DecoderInstance::try_open(ref_mss, Default::default(), Default::default())?;

    // 3. Instantiate a Symphonia decoder for the test target.
    let tgt_ms = Box::new(File::open(path)?);
    let tgt_mss = MediaSourceStream::new(tgt_ms, Default::default());

    let tgt_fmt_opts = Default::default();
    let tgt_dec_opts = AudioDecoderOptions::default().gapless(opts.gapless);

    let mut tgt_inst = DecoderInstance::try_open(tgt_mss, tgt_fmt_opts, tgt_dec_opts)?;

    // 4. Begin check.
    run_check(&mut ref_inst, &mut tgt_inst, opts, result)
}

fn main() {
    pretty_env_logger::init();

    let matches = clap::Command::new("Symphonia Check")
        .version("1.0")
        .author("Philip Deljanov <philip.deljanov@gmail.com>")
        .about("Check Symphonia output with a reference decoding")
        .arg(
            Arg::new("samples")
                .long("samples")
                .action(ArgAction::SetTrue)
                .help("Print failures per sample"),
        )
        .arg(
            Arg::new("stop-after-fail")
                .long("first-fail")
                .short('f')
                .action(ArgAction::SetTrue)
                .help("Stop testing after the first failed packet"),
        )
        .arg(
            Arg::new("quiet")
                .long("quiet")
                .short('q')
                .action(ArgAction::SetTrue)
                .help("Only print test results"),
        )
        .arg(
            Arg::new("keep-going")
                .long("keep-going")
                .action(ArgAction::SetTrue)
                .help("Continue after a decode error (may cause many failures)"),
        )
        .arg(
            Arg::new("decoder")
                .long("ref")
                .value_parser(["ffmpeg", "flac", "mpg123", "oggdec"])
                .default_value("ffmpeg")
                .help("Specify a particular decoder to be used as the reference"),
        )
        .arg(
            Arg::new("ref-wav")
                .long("ref-wav")
                .value_parser(clap::value_parser!(PathBuf))
                .help("Use a pre-decoded WAVE file as the reference instead of a decoder"),
        )
        .arg(
            Arg::new("dump")
                .long("dump")
                .value_parser(clap::value_parser!(PathBuf))
                .help("Write the target's decoded audio as interleaved f32le samples to a file"),
        )
        .arg(
            Arg::new("seek")
                .long("seek")
                .value_parser(clap::value_parser!(f64))
                .requires("dump")
                .help("Seek to a time in seconds and dump the audio from there (no comparison)"),
        )
        .arg(
            Arg::new("tolerance")
                .long("tolerance")
                .value_parser(clap::value_parser!(f32))
                .default_value("0.00001")
                .help("The absolute maximum allowable sample delta"),
        )
        .arg(
            Arg::new("no-gapless")
                .long("no-gapless")
                .action(ArgAction::SetTrue)
                .help("Disable gapless decoding"),
        )
        .arg(
            Arg::new("INPUT")
                .help("The input file path")
                .value_parser(clap::value_parser!(PathBuf))
                .required(true)
                .index(1),
        )
        .get_matches();

    let path = matches.get_one::<PathBuf>("INPUT").expect("path is a required argument");

    let ref_decoder = match matches
        .get_one::<String>("decoder")
        .expect("decoder is a required argument")
        .as_ref()
    {
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

    let ref_decoder = match matches.get_one::<PathBuf>("ref-wav") {
        Some(wav) => RefDecoder::Wav(wav.clone()),
        None => ref_decoder,
    };

    let opts = TestOptions {
        dump: matches.get_one::<PathBuf>("dump").cloned(),
        tolerance: *matches.get_one::<f32>("tolerance").expect("tolerance has a default"),
        ref_decoder,
        is_per_sample: matches.get_flag("samples"),
        is_quiet: matches.get_flag("quiet"),
        stop_after_fail: matches.get_flag("stop-after-fail"),
        keep_going: matches.get_flag("keep-going"),
        gapless: !matches.get_flag("no-gapless"),
        ..Default::default()
    };

    if let Some(&time) = matches.get_one::<f64>("seek") {
        let dump = opts.dump.as_ref().expect("dump is required by seek");

        if let Err(err) = run_seek_dump(path, time, dump, opts.gapless) {
            eprintln!("Seek test interrupted by error: {err}");
            std::process::exit(2);
        }

        return;
    }

    let mut res: TestResult = Default::default();

    println!("Input Path: {}", path.to_string_lossy());
    println!();

    if let Err(err) = run_test(path, &opts, &mut res) {
        eprintln!("Test interrupted by error: {err}");
        std::process::exit(2);
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
    println!("  Remaining Target Samples:          {:>12}", res.tgt_unchecked_samples);
    println!("  Remaining Reference Samples:       {:>12}", res.ref_unchecked_samples);
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
