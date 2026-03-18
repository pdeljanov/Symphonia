//! HE-AAC v1/v2 decode comparison test.
//!
//! Decodes an ADTS HE-AAC file with Symphonia and compares the output
//! RMS and peak levels with FFmpeg's reference decode.

use symphonia_codec_aac::{AacDecoder, AdtsReader};
use symphonia_core::audio::{AudioBufferRef, Signal};
use symphonia_core::codecs::{Decoder, DecoderOptions};
use symphonia_core::formats::{FormatOptions, FormatReader};
use symphonia_core::io::MediaSourceStream;

/// Decode an ADTS file with Symphonia and return all output samples as f32 (interleaved stereo).
fn decode_adts_file(path: &str) -> Vec<f32> {
    let file = std::fs::File::open(path).expect("failed to open ADTS file");
    let source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut reader = AdtsReader::try_new(source, &FormatOptions::default())
        .expect("failed to create ADTS reader");

    // Get codec params from the ADTS reader's track.
    let codec_params = reader.default_track().expect("no default track").codec_params.clone();
    eprintln!(
        "  Codec params: rate={:?} channels={:?}",
        codec_params.sample_rate, codec_params.channels
    );

    let mut decoder = AacDecoder::try_new(&codec_params, &DecoderOptions::default())
        .expect("failed to create AAC decoder");

    let mut all_samples = Vec::new();
    let mut frame_count = 0u32;

    loop {
        match reader.next_packet() {
            Ok(packet) => {
                match decoder.decode(&packet) {
                    Ok(audio_buf) => {
                        let channels = match &audio_buf {
                            AudioBufferRef::F32(buf) => buf.spec().channels.count(),
                            _ => panic!("unexpected sample format"),
                        };
                        let frames = match &audio_buf {
                            AudioBufferRef::F32(buf) => buf.frames(),
                            _ => 0,
                        };

                        if frame_count < 3 {
                            eprintln!(
                                "  Frame {}: {} channels, {} frames, sample_rate={}",
                                frame_count,
                                channels,
                                frames,
                                match &audio_buf {
                                    AudioBufferRef::F32(buf) => buf.spec().rate,
                                    _ => 0,
                                }
                            );
                        }

                        // Extract interleaved samples.
                        match &audio_buf {
                            AudioBufferRef::F32(buf) => {
                                for frame in 0..buf.frames() {
                                    for ch in 0..channels {
                                        all_samples.push(buf.chan(ch)[frame]);
                                    }
                                }
                            }
                            _ => {}
                        }

                        frame_count += 1;
                    }
                    Err(e) => {
                        eprintln!("  Decode error at frame {}: {}", frame_count, e);
                        frame_count += 1;
                    }
                }
            }
            Err(_) => break,
        }
    }

    eprintln!("  Total: {} frames decoded, {} samples", frame_count, all_samples.len());
    all_samples
}

/// Read FFmpeg reference f32le PCM file.
fn read_reference_f32le(path: &str) -> Vec<f32> {
    let data = std::fs::File::open(path).ok().and_then(|mut f| {
        use std::io::Read;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).ok()?;
        Some(buf)
    });

    match data {
        Some(bytes) => {
            let n_samples = bytes.len() / 4;
            let mut samples = Vec::with_capacity(n_samples);
            for chunk in bytes.chunks_exact(4) {
                samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
            samples
        }
        None => Vec::new(),
    }
}

fn compute_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

fn compute_peak(samples: &[f32]) -> f32 {
    samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max)
}

#[test]
fn heaac_v1_decode_compare() {
    let adts_path = "/tmp/test_heaac_v1.aac";
    let ref_path = "/tmp/ref_heaac_v1_adts_f32.raw";

    // Check if test files exist.
    if !std::path::Path::new(adts_path).exists() {
        eprintln!("SKIP: {} not found. Run the test file generation script first.", adts_path);
        return;
    }

    eprintln!("=== HE-AAC v1 Decode Comparison ===");

    // Decode with Symphonia.
    eprintln!("Decoding with Symphonia:");
    let sym_samples = decode_adts_file(adts_path);

    // Read FFmpeg reference.
    let ref_samples = read_reference_f32le(ref_path);

    // Skip initial transient (encoder delay / decoder warmup).
    let skip = 8192; // samples (4096 stereo frames)

    let sym_stable = if sym_samples.len() > skip { &sym_samples[skip..] } else { &sym_samples };

    let ref_stable = if ref_samples.len() > skip { &ref_samples[skip..] } else { &ref_samples };

    let sym_rms = compute_rms(sym_stable);
    let sym_peak = compute_peak(sym_stable);

    eprintln!("\nSymphonia output:");
    eprintln!("  Total samples: {}", sym_samples.len());
    eprintln!("  RMS (after skip):  {:.6}", sym_rms);
    eprintln!("  Peak (after skip): {:.6}", sym_peak);

    if !ref_samples.is_empty() {
        let ref_rms = compute_rms(ref_stable);
        let ref_peak = compute_peak(ref_stable);

        eprintln!("\nFFmpeg reference:");
        eprintln!("  Total samples: {}", ref_samples.len());
        eprintln!("  RMS (after skip):  {:.6}", ref_rms);
        eprintln!("  Peak (after skip): {:.6}", ref_peak);

        let rms_ratio = sym_rms / ref_rms;
        let peak_ratio = sym_peak / ref_peak;

        eprintln!("\nComparison:");
        eprintln!("  RMS ratio (Symphonia/FFmpeg):  {:.4}", rms_ratio);
        eprintln!("  Peak ratio (Symphonia/FFmpeg): {:.4}", peak_ratio);

        // Per-frame comparison: compute RMS of first 10 frames.
        let frame_size = 2048 * 2; // 2048 samples * 2 channels (1024 core * 2 SBR)
        eprintln!("\nPer-frame RMS comparison (first 10 frames):");
        for i in 0..10 {
            let start = i * frame_size;
            let sym_frame = if start + frame_size <= sym_samples.len() {
                &sym_samples[start..start + frame_size]
            }
            else {
                break;
            };
            let ref_frame = if start + frame_size <= ref_samples.len() {
                &ref_samples[start..start + frame_size]
            }
            else {
                &[]
            };

            let s_rms = compute_rms(sym_frame);
            let r_rms = if ref_frame.is_empty() { 0.0 } else { compute_rms(ref_frame) };
            let ratio = if r_rms > 0.0 { s_rms / r_rms } else { 0.0 };

            eprintln!(
                "  Frame {:2}: sym_rms={:.6}  ref_rms={:.6}  ratio={:.4}",
                i, s_rms, r_rms, ratio
            );
        }

        // The SBR output should be within reasonable range of the reference.
        // With perfect SBR, RMS ratio should be 0.8-1.2.
        // Even with minor differences, it shouldn't be below 0.5.
        // With the QMF band ordering fix, Symphonia should be within ~20% of FFmpeg.
        assert!(
            rms_ratio > 0.7,
            "Symphonia HE-AAC output is {:.1}x too quiet vs FFmpeg reference (RMS ratio: {:.4})",
            1.0 / rms_ratio,
            rms_ratio
        );
    }
    else {
        eprintln!("\nNo FFmpeg reference available for comparison.");
        // At minimum, check the output isn't silent.
        assert!(sym_rms > 0.01, "Symphonia HE-AAC output is nearly silent (RMS: {:.6})", sym_rms);
    }
}

#[test]
fn heaac_v1_wideband_decode_compare() {
    let adts_path = "/tmp/test_heaac_v1_wideband.aac";
    let ref_path = "/tmp/ref_heaac_v1_wideband_f32.raw";

    if !std::path::Path::new(adts_path).exists() {
        eprintln!("SKIP: {} not found.", adts_path);
        return;
    }

    eprintln!("=== HE-AAC v1 Wideband Decode Comparison ===");
    eprintln!("  (test signal includes 10kHz and 15kHz tones in SBR range)");

    eprintln!("Decoding with Symphonia:");
    let sym_samples = decode_adts_file(adts_path);

    let ref_samples = read_reference_f32le(ref_path);

    let skip = 8192;
    let sym_stable = if sym_samples.len() > skip { &sym_samples[skip..] } else { &sym_samples };
    let ref_stable = if ref_samples.len() > skip { &ref_samples[skip..] } else { &ref_samples };

    let sym_rms = compute_rms(sym_stable);
    let sym_peak = compute_peak(sym_stable);

    eprintln!("\nSymphonia: rms={:.6} peak={:.6} samples={}", sym_rms, sym_peak, sym_samples.len());

    if !ref_samples.is_empty() {
        let ref_rms = compute_rms(ref_stable);
        let ref_peak = compute_peak(ref_stable);
        let rms_ratio = sym_rms / ref_rms;

        eprintln!(
            "FFmpeg:    rms={:.6} peak={:.6} samples={}",
            ref_rms,
            ref_peak,
            ref_samples.len()
        );
        eprintln!("RMS ratio: {:.4}", rms_ratio);

        // Per-frame comparison (first 10 frames).
        let frame_size = 2048 * 2;
        eprintln!("\nPer-frame RMS (first 10):");
        for i in 0..10 {
            let start = i * frame_size;
            let sym_frame = if start + frame_size <= sym_samples.len() {
                &sym_samples[start..start + frame_size]
            }
            else {
                break;
            };
            let ref_frame = if start + frame_size <= ref_samples.len() {
                &ref_samples[start..start + frame_size]
            }
            else {
                &[]
            };
            let s_rms = compute_rms(sym_frame);
            let r_rms = if ref_frame.is_empty() { 0.0 } else { compute_rms(ref_frame) };
            let ratio = if r_rms > 0.0 { s_rms / r_rms } else { 0.0 };
            eprintln!("  Frame {:2}: sym={:.6} ref={:.6} ratio={:.4}", i, s_rms, r_rms, ratio);
        }

        assert!(rms_ratio > 0.7, "Wideband HE-AAC output too quiet (ratio: {:.4})", rms_ratio);
    }
    else {
        eprintln!("No FFmpeg reference for wideband test.");
        assert!(sym_rms > 0.01, "Output nearly silent (rms={:.6})", sym_rms);
    }
}

/// Compare AAC-LC (no SBR) output to isolate core decoder vs SBR issues.
#[test]
fn aaclc_decode_compare() {
    let adts_path = "/tmp/test_aaclc.aac";
    let ref_path = "/tmp/ref_aaclc_f32.raw";

    if !std::path::Path::new(adts_path).exists() {
        eprintln!("SKIP: {} not found.", adts_path);
        return;
    }

    eprintln!("=== AAC-LC (no SBR) Decode Comparison ===");

    eprintln!("Decoding with Symphonia:");
    let sym_samples = decode_adts_file(adts_path);

    let ref_samples = read_reference_f32le(ref_path);

    let skip = 8192;
    let sym_stable = if sym_samples.len() > skip { &sym_samples[skip..] } else { &sym_samples };
    let ref_stable = if ref_samples.len() > skip { &ref_samples[skip..] } else { &ref_samples };

    let sym_rms = compute_rms(sym_stable);
    let sym_peak = compute_peak(sym_stable);

    eprintln!("\nSymphonia: rms={:.6} peak={:.6} samples={}", sym_rms, sym_peak, sym_samples.len());

    if !ref_samples.is_empty() {
        let ref_rms = compute_rms(ref_stable);
        let ref_peak = compute_peak(ref_stable);
        let rms_ratio = sym_rms / ref_rms;
        let peak_ratio = sym_peak / ref_peak;

        eprintln!(
            "FFmpeg:    rms={:.6} peak={:.6} samples={}",
            ref_rms,
            ref_peak,
            ref_samples.len()
        );
        eprintln!("RMS ratio:  {:.4}", rms_ratio);
        eprintln!("Peak ratio: {:.4}", peak_ratio);

        // Core AAC-LC should be very close to FFmpeg reference.
        assert!(
            rms_ratio > 0.9 && rms_ratio < 1.1,
            "AAC-LC RMS ratio {:.4} is outside [0.9, 1.1]",
            rms_ratio
        );
    }
}
