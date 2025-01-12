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

use log::warn;
use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::errors::{unsupported_error, Error, Result};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;

use crate::{AudioTestDecoder, AudioTestOptions, RefProcess};

/// The absolute maximum allowable sample delta. Around 2^-17 (-102.4dB).
const ABS_MAX_ALLOWABLE_SAMPLE_DELTA: f32 = 0.00001;

// The absolute maximum allowable sample delta for a fully compliant MP3 decoder as specified by the
// ISO. Around 2^-14 (-84.2dB).
// const ABS_MAX_ALLOWABLE_SAMPLE_DELTA_MP3: f32 = 0.00006104;

#[derive(Default)]
struct AudioTestResult {
    n_frames: u64,
    n_samples: u64,
    n_failed_samples: u64,
    n_packets: u64,
    n_failed_packets: u64,
    abs_max_delta: f32,
    tgt_unchecked_samples: u64,
    ref_unchecked_samples: u64,
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

#[derive(Default)]
struct FlushStats {
    n_packets: u64,
    n_samples: u64,
}

struct DecoderInstance {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
}

impl DecoderInstance {
    fn try_open(
        mss: MediaSourceStream<'static>,
        fmt_opts: FormatOptions,
    ) -> Result<DecoderInstance> {
        // Use the default options for metadata and format readers, and the decoder.
        let meta_opts: MetadataOptions = Default::default();
        let dec_opts: AudioDecoderOptions = Default::default();

        let hint = Hint::new();

        let format = symphonia::default::get_probe().probe(&hint, mss, fmt_opts, meta_opts)?;

        let track = format.default_track(TrackType::Audio).unwrap();

        let codec_params = match &track.codec_params {
            Some(CodecParameters::Audio(params)) => params,
            _ => return unsupported_error("only audio tracks are supported"),
        };

        let decoder =
            symphonia::default::get_codecs().make_audio_decoder(codec_params, &dec_opts)?;

        let track_id = track.id;

        Ok(DecoderInstance { format, decoder, track_id })
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
                Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                    // WavReader will always return an UnexpectedEof when it ends because the
                    // reference decoder is piping the decoded audio and cannot write out the
                    // actual length of the media. Treat UnexpectedEof as the end of the stream.
                    return Ok(None);
                }
                Err(err) => return Err(err),
            };

            // Skip packets that do not belong to the track being decoded.
            if packet.track_id() != self.track_id {
                continue;
            }

            // Decode the packet, ignoring decode errors if `keep_going` is true.
            match self.decoder.decode(&packet) {
                Ok(_) => break,
                Err(Error::DecodeError(err)) if keep_going => warn!("{}", err),
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
    opts: &AudioTestOptions,
    acct: &mut AudioTestResult,
) -> Result<()> {
    // Reference
    let mut ref_sample_buf: Vec<f32> = Default::default();
    let mut ref_sample_cnt = 0;
    let mut ref_sample_pos = 0;

    // Target
    let mut tgt_sample_buf: Vec<f32> = Default::default();
    let mut tgt_sample_cnt = 0;
    let mut tgt_sample_pos = 0;

    let samples_per_frame = tgt_inst.samples_per_frame().unwrap_or(1);

    // Samples/frame must match for both decoders.
    if samples_per_frame != ref_inst.samples_per_frame().unwrap_or(1) {
        return unsupported_error("target and reference decoder samples per frame mismatch");
    }

    let early_fail = 'outer: loop {
        // Decode the next target audio buffer and copy it to the target sample buffer.
        match tgt_inst.next_audio_buf(opts.keep_going)? {
            Some(buf) => buf.copy_to_vec_interleaved(&mut tgt_sample_buf),
            None => break 'outer false,
        };

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

                if delta.abs() > ABS_MAX_ALLOWABLE_SAMPLE_DELTA {
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

fn run_test(path: &str, opts: &AudioTestOptions, result: &mut AudioTestResult) -> Result<()> {
    let command = match opts.ref_decoder {
        AudioTestDecoder::Ffmpeg => build_ffmpeg_command(path, opts.gapless),
        AudioTestDecoder::Flac => build_flac_command(path),
        AudioTestDecoder::Mpg123 => build_mpg123_command(path, opts.gapless),
        AudioTestDecoder::Oggdec => build_oggdec_command(path),
    };

    // 1. Start the reference decoder process.
    let mut ref_process = RefProcess::try_spawn(command)?;

    // 2. Instantiate a Symphonia decoder for the reference process output.
    let ref_ms = Box::new(ReadOnlySource::new(ref_process.child.stdout.take().unwrap()));
    let ref_mss = MediaSourceStream::new(ref_ms, Default::default());

    let mut ref_inst = DecoderInstance::try_open(ref_mss, Default::default())?;

    // 3. Instantiate a Symphonia decoder for the test target.
    let tgt_ms = Box::new(File::open(Path::new(path))?);
    let tgt_mss = MediaSourceStream::new(tgt_ms, Default::default());

    let tgt_fmt_opts = FormatOptions { enable_gapless: opts.gapless, ..Default::default() };

    let mut tgt_inst = DecoderInstance::try_open(tgt_mss, tgt_fmt_opts)?;

    // 4. Begin check.
    run_check(&mut ref_inst, &mut tgt_inst, opts, result)
}

pub fn run_audio(opts: AudioTestOptions) -> Result<()> {
    let mut res: AudioTestResult = Default::default();

    run_test(&opts.input, &opts, &mut res)?;

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

    if res.n_failed_samples == 0 {
        Ok(())
    }
    else {
        unsupported_error("Some samples didn't pass validation")
    }
}
