// Integration tests for symphonia-bundle-ape.
//
// Tests the full pipeline: open file → ApeReader (FormatReader) → ApeDecoder → AudioBuffer.
// Compares decoded samples against the standalone ape-decoder's output.

use std::fs::File;
use std::path::PathBuf;

use symphonia_core::audio::{AudioBufferRef, Signal};
use symphonia_core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_MONKEYS_AUDIO};
use symphonia_core::formats::FormatReader;
use symphonia_core::io::{MediaSourceStream, MediaSourceStreamOptions};

use symphonia_bundle_ape::{ApeDecoder, ApeReader};

/// Path to APE test fixtures (shared with ape-decoder crate).
fn fixture_path(name: &str) -> PathBuf {
    // Navigate from symphonia-bundle-ape to the decoder's test fixtures.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../decoder/tests/fixtures/ape")
        .join(name)
}

/// Reference WAV path.
fn ref_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../decoder/tests/fixtures/ref")
        .join(name)
}

/// Open an APE file through the Symphonia pipeline and return (reader, decoder).
fn open_ape(filename: &str) -> (Box<ApeReader>, ApeDecoder) {
    let path = fixture_path(filename);
    let file = File::open(&path).unwrap_or_else(|e| panic!("open {}: {}", path.display(), e));
    let mss = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());

    let reader = ApeReader::try_new(mss, &Default::default())
        .unwrap_or_else(|e| panic!("ApeReader::try_new for {}: {}", filename, e));

    let track = &reader.tracks()[0];
    assert_eq!(track.codec_params.codec, CODEC_TYPE_MONKEYS_AUDIO);

    let decoder = ApeDecoder::try_new(&track.codec_params, &DecoderOptions::default())
        .unwrap_or_else(|e| panic!("ApeDecoder::try_new for {}: {}", filename, e));

    (Box::new(reader), decoder)
}

/// Decode an entire APE file through Symphonia, returning all samples as interleaved i32.
fn decode_all_symphonia(filename: &str) -> (Vec<i32>, u16, u16, u32) {
    let (mut reader, mut decoder) = open_ape(filename);

    let params = &reader.tracks()[0].codec_params;
    let channels = params.channels.unwrap().count() as u16;
    let bps = params.bits_per_sample.unwrap() as u16;
    let sample_rate = params.sample_rate.unwrap();

    let mut all_samples: Vec<i32> = Vec::new();

    loop {
        match reader.next_packet() {
            Ok(packet) => {
                let buf_ref = decoder.decode(&packet).unwrap();
                append_samples(&buf_ref, &mut all_samples, channels);
            }
            Err(symphonia_core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => panic!("next_packet error: {}", e),
        }
    }

    (all_samples, channels, bps, sample_rate)
}

/// Extract interleaved i32 samples from an AudioBufferRef.
fn append_samples(buf_ref: &AudioBufferRef<'_>, out: &mut Vec<i32>, channels: u16) {
    match buf_ref {
        AudioBufferRef::S32(buf) => {
            let frames = buf.frames();
            for frame in 0..frames {
                for ch in 0..channels as usize {
                    out.push(buf.chan(ch)[frame]);
                }
            }
        }
        _ => panic!("expected S32 AudioBuffer"),
    }
}

/// Load reference WAV file and return raw PCM samples as i32 (interleaved).
fn load_reference_wav(name: &str, bps: u16) -> Vec<i32> {
    let path = ref_path(name);
    let data = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    // Skip 44-byte WAV header.
    let pcm = &data[44..];

    match bps {
        8 => pcm.iter().map(|&b| b as i32 - 128).collect(),
        16 => {
            pcm.chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as i32)
                .collect()
        }
        24 => {
            pcm.chunks_exact(3)
                .map(|c| {
                    let raw = c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16;
                    if raw & 0x80_0000 != 0 {
                        (raw | 0xFF00_0000) as i32
                    }
                    else {
                        raw as i32
                    }
                })
                .collect()
        }
        32 => {
            pcm.chunks_exact(4)
                .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        }
        _ => panic!("unsupported bps {}", bps),
    }
}

// ---------------------------------------------------------------------------
// Test: format detection and codec parameters
// ---------------------------------------------------------------------------

#[test]
fn test_format_detection() {
    let (reader, _decoder) = open_ape("sine_16s_c2000.ape");
    let track = &reader.tracks()[0];

    assert_eq!(track.codec_params.codec, CODEC_TYPE_MONKEYS_AUDIO);
    assert_eq!(track.codec_params.sample_rate, Some(44100));
    assert_eq!(track.codec_params.bits_per_sample, Some(16));
    assert_eq!(track.codec_params.channels.unwrap().count(), 2);
}

#[test]
fn test_mono_format_detection() {
    let (reader, _decoder) = open_ape("sine_16m_c2000.ape");
    let track = &reader.tracks()[0];

    assert_eq!(track.codec_params.channels.unwrap().count(), 1);
    assert_eq!(track.codec_params.bits_per_sample, Some(16));
}

// ---------------------------------------------------------------------------
// Test: decode and compare with reference
// ---------------------------------------------------------------------------

/// Decode through Symphonia and compare with reference WAV.
/// Symphonia normalizes to 32-bit (left-shifts), so we do the same to the reference.
fn assert_decode_matches_reference(ape_file: &str, ref_file: &str, bps: u16) {
    let (samples, _channels, actual_bps, _sr) = decode_all_symphonia(ape_file);
    assert_eq!(actual_bps, bps);

    let reference = load_reference_wav(ref_file, bps);
    let shift = 32 - bps as u32;

    // Left-shift reference to match Symphonia's 32-bit normalization.
    let reference_shifted: Vec<i32> = reference.iter().map(|&s| s << shift).collect();

    assert_eq!(
        samples.len(),
        reference_shifted.len(),
        "sample count mismatch for {} (got {} vs ref {})",
        ape_file,
        samples.len(),
        reference_shifted.len()
    );

    for (i, (&got, &expected)) in samples.iter().zip(reference_shifted.iter()).enumerate() {
        assert_eq!(
            got, expected,
            "sample mismatch at index {} for {}: got {} expected {}",
            i, ape_file, got, expected
        );
    }
}

#[test]
fn test_decode_sine_16s_c2000() {
    assert_decode_matches_reference("sine_16s_c2000.ape", "sine_16s_c2000.wav", 16);
}

#[test]
fn test_decode_sine_16m_c2000() {
    assert_decode_matches_reference("sine_16m_c2000.ape", "sine_16m_c2000.wav", 16);
}

#[test]
fn test_decode_silence_16s_c2000() {
    assert_decode_matches_reference("silence_16s_c2000.ape", "silence_16s_c2000.wav", 16);
}

#[test]
fn test_decode_noise_16s_c2000() {
    assert_decode_matches_reference("noise_16s_c2000.ape", "noise_16s_c2000.wav", 16);
}

#[test]
fn test_decode_multiframe_16s_c2000() {
    assert_decode_matches_reference("multiframe_16s_c2000.ape", "multiframe_16s_c2000.wav", 16);
}

#[test]
fn test_decode_short_16s_c2000() {
    assert_decode_matches_reference("short_16s_c2000.ape", "short_16s_c2000.wav", 16);
}

// ---------------------------------------------------------------------------
// Test: compression levels
// ---------------------------------------------------------------------------

#[test]
fn test_decode_compression_levels() {
    for level in &[1000, 2000, 3000, 4000, 5000] {
        let ape_file = format!("sine_16s_c{}.ape", level);
        let ref_file = format!("sine_16s_c{}.wav", level);
        assert_decode_matches_reference(&ape_file, &ref_file, 16);
    }
}

// ---------------------------------------------------------------------------
// Test: bit depths
// ---------------------------------------------------------------------------

#[test]
fn test_decode_8bit() {
    assert_decode_matches_reference("sine_8s_c2000.ape", "sine_8s_c2000.wav", 8);
}

#[test]
fn test_decode_24bit() {
    assert_decode_matches_reference("sine_24s_c2000.ape", "sine_24s_c2000.wav", 24);
}

#[test]
fn test_decode_32bit() {
    assert_decode_matches_reference("sine_32s_c2000.ape", "sine_32s_c2000.wav", 32);
}

// ---------------------------------------------------------------------------
// Test: seeking
// ---------------------------------------------------------------------------

#[test]
fn test_seek_to_start() {
    let (mut reader, mut decoder) = open_ape("multiframe_16s_c2000.ape");

    // Decode first packet.
    let packet = reader.next_packet().unwrap();
    let _buf = decoder.decode(&packet).unwrap();

    // Seek back to start.
    let seeked = reader
        .seek(
            symphonia_core::formats::SeekMode::Accurate,
            symphonia_core::formats::SeekTo::TimeStamp { ts: 0, track_id: 0 },
        )
        .unwrap();
    assert_eq!(seeked.actual_ts, 0);

    // Should be able to decode from the start again.
    let packet2 = reader.next_packet().unwrap();
    assert_eq!(packet2.ts, 0);
    let _buf2 = decoder.decode(&packet2).unwrap();
}
