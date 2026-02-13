// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{Seek, SeekFrom};

use symphonia_core::codecs::CodecParameters;
use symphonia_core::errors::{seek_error, unsupported_error};
use symphonia_core::errors::{Result, SeekErrorKind};
use symphonia_core::formats::prelude::*;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};
use symphonia_core::support_format;

use log::{debug, error};

use crate::common::{
    append_data_params, append_format_params, next_packet, ByteOrder, ChunksReader, PacketInfo,
};
mod chunks;
use chunks::*;

/// WAVE is actually a RIFF stream, with a "RIFF" ASCII stream marker.
const WAVE_STREAM_MARKER: [u8; 4] = *b"RIFF";
/// RF64 is a 64-bit extension of RIFF, with "RF64" as the stream marker.
/// Reference: EBU Tech 3306 - MBWF / RF64: An extended File Format for Audio.
const RF64_STREAM_MARKER: [u8; 4] = *b"RF64";
/// A possible RIFF form is "wave".
const WAVE_RIFF_FORM: [u8; 4] = *b"WAVE";

/// Holds 64-bit size information from the ds64 chunk for RF64 files.
#[derive(Default)]
struct Rf64Sizes {
    /// 64-bit data chunk size (None for standard WAV files).
    data_size: Option<u64>,
    /// 64-bit sample count (None for standard WAV or if not provided).
    sample_count: Option<u64>,
}

/// Waveform Audio File Format (WAV) format reader.
///
/// `WavReader` implements a demuxer for the WAVE container format.
pub struct WavReader {
    reader: MediaSourceStream,
    tracks: Vec<Track>,
    cues: Vec<Cue>,
    metadata: MetadataLog,
    packet_info: PacketInfo,
    data_start_pos: u64,
    data_end_pos: u64,
}

impl QueryDescriptor for WavReader {
    fn query() -> &'static [Descriptor] {
        &[
            // Standard WAVE RIFF form
            support_format!(
                "wave",
                "Waveform Audio File Format",
                &["wav", "wave"],
                &["audio/vnd.wave", "audio/x-wav", "audio/wav", "audio/wave"],
                &[b"RIFF"]
            ),
            // RF64 extended WAVE format (64-bit extension for files > 4GB)
            support_format!(
                "rf64",
                "RF64 Extended Waveform Audio File Format",
                &["wav", "wave", "rf64"],
                &["audio/vnd.wave", "audio/x-wav", "audio/wav", "audio/wave"],
                &[b"RF64"]
            ),
        ]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}

impl FormatReader for WavReader {
    fn try_new(mut source: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {
        // The RIFF or RF64 marker should be present.
        let marker = source.read_quad_bytes()?;

        let is_rf64 = match marker {
            WAVE_STREAM_MARKER => false,
            RF64_STREAM_MARKER => true,
            _ => return unsupported_error("wav: missing riff/rf64 stream marker"),
        };

        // A Wave file is one large RIFF chunk, with the actual meta and audio data as sub-chunks.
        // Therefore, the header was the chunk ID, and the next 4 bytes is the length of the RIFF
        // chunk. For RF64 files, this is 0xFFFFFFFF and the actual size is in the ds64 chunk.
        let riff_len = source.read_u32()?;
        let riff_form = source.read_quad_bytes()?;

        // The RIFF chunk contains WAVE data.
        if riff_form != WAVE_RIFF_FORM {
            error!("riff form is not wave ({})", String::from_utf8_lossy(&riff_form));

            return unsupported_error("wav: riff form is not wave");
        }

        let mut riff_chunks =
            ChunksReader::<RiffWaveChunks>::new(riff_len, ByteOrder::LittleEndian);

        let mut codec_params = CodecParameters::new();
        let mut metadata: MetadataLog = Default::default();
        let mut packet_info = PacketInfo::without_blocks(0);
        let mut rf64_sizes = Rf64Sizes::default();

        loop {
            let chunk = riff_chunks.next(&mut source)?;

            // The last chunk should always be a data chunk, if it is not, then the stream is
            // unsupported.
            if chunk.is_none() {
                return unsupported_error("wav: missing data chunk");
            }

            match chunk.unwrap() {
                RiffWaveChunks::Ds64(ds64_parser) => {
                    // ds64 chunk is only valid in RF64 files.
                    if !is_rf64 {
                        // Parse to consume the bytes, but ignore in standard WAV files.
                        let _ = ds64_parser.parse(&mut source)?;
                        debug!("ignoring ds64 chunk in non-RF64 file");
                        continue;
                    }

                    let ds64 = ds64_parser.parse(&mut source)?;
                    debug!(
                        "parsed ds64 chunk: data_size={}, sample_count={}",
                        ds64.data_size, ds64.sample_count
                    );

                    rf64_sizes.data_size = Some(ds64.data_size);
                    if ds64.sample_count > 0 {
                        rf64_sizes.sample_count = Some(ds64.sample_count);
                    }
                }
                RiffWaveChunks::Format(fmt) => {
                    let format = fmt.parse(&mut source)?;

                    // The Format chunk contains the block_align field and possible additional information
                    // to handle packetization and seeking.
                    packet_info = format.packet_info()?;
                    codec_params
                        .with_max_frames_per_packet(packet_info.get_max_frames_per_packet())
                        .with_frames_per_block(packet_info.frames_per_block);

                    // Append Format chunk fields to codec parameters.
                    append_format_params(
                        &mut codec_params,
                        &format.format_data,
                        format.sample_rate,
                    );
                }
                RiffWaveChunks::Fact(fct) => {
                    let fact = fct.parse(&mut source)?;

                    // For RF64 files, prefer the sample count from ds64 over fact chunk.
                    // Only use fact chunk if we don't have a 64-bit sample count.
                    if rf64_sizes.sample_count.is_none() {
                        append_fact_params(&mut codec_params, &fact);
                    }
                }
                RiffWaveChunks::List(lst) => {
                    let list = lst.parse(&mut source)?;

                    // Riff Lists can have many different forms, but WavReader only supports Info
                    // lists.
                    match &list.form {
                        b"INFO" => metadata.push(read_info_chunk(&mut source, list.len)?),
                        _ => list.skip(&mut source)?,
                    }
                }
                RiffWaveChunks::Data(dat) => {
                    let data = dat.parse(&mut source)?;

                    // Record the bounds of the data chunk.
                    let data_start_pos = source.pos();

                    // Use 64-bit data size from ds64 for RF64 files, otherwise use 32-bit size.
                    let data_len = rf64_sizes.data_size.unwrap_or(u64::from(data.len));
                    let data_end_pos = data_start_pos + data_len;

                    // Append Data chunk fields to codec parameters.
                    append_data_params(&mut codec_params, data_len, &packet_info);

                    // If we have a 64-bit sample count from ds64, use it.
                    if let Some(sample_count) = rf64_sizes.sample_count {
                        codec_params.with_n_frames(sample_count);
                    }

                    // Add a new track using the collected codec parameters.
                    return Ok(WavReader {
                        reader: source,
                        tracks: vec![Track::new(0, codec_params)],
                        cues: Vec::new(),
                        metadata,
                        packet_info,
                        data_start_pos,
                        data_end_pos,
                    });
                }
            }
        }
    }

    fn next_packet(&mut self) -> Result<Packet> {
        next_packet(
            &mut self.reader,
            &self.packet_info,
            &self.tracks,
            self.data_start_pos,
            self.data_end_pos,
        )
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn cues(&self) -> &[Cue] {
        &self.cues
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        if self.tracks.is_empty() || self.packet_info.is_empty() {
            return seek_error(SeekErrorKind::Unseekable);
        }

        let params = &self.tracks[0].codec_params;

        let ts = match to {
            // Frame timestamp given.
            SeekTo::TimeStamp { ts, .. } => ts,
            // Time value given, calculate frame timestamp from sample rate.
            SeekTo::Time { time, .. } => {
                // Use the sample rate to calculate the frame timestamp. If sample rate is not
                // known, the seek cannot be completed.
                if let Some(sample_rate) = params.sample_rate {
                    TimeBase::new(1, sample_rate).calc_timestamp(time)
                }
                else {
                    return seek_error(SeekErrorKind::Unseekable);
                }
            }
        };

        // If the total number of frames in the track is known, verify the desired frame timestamp
        // does not exceed it.
        if let Some(n_frames) = params.n_frames {
            if ts > n_frames {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        }

        debug!("seeking to frame_ts={}", ts);

        // WAVE is not internally packetized for PCM codecs. Packetization is simulated by trying to
        // read a constant number of samples or blocks every call to next_packet. Therefore, a packet begins
        // wherever the data stream is currently positioned. Since timestamps on packets should be
        // determinstic, instead of seeking to the exact timestamp requested and starting the next
        // packet there, seek to a packet boundary. In this way, packets will have have the same
        // timestamps regardless if the stream was seeked or not.
        let actual_ts = self.packet_info.get_actual_ts(ts);

        // Calculate the absolute byte offset of the desired audio frame.
        let seek_pos = self.data_start_pos + (actual_ts * self.packet_info.block_size);

        // If the reader supports seeking we can seek directly to the frame's offset wherever it may
        // be.
        if self.reader.is_seekable() {
            self.reader.seek(SeekFrom::Start(seek_pos))?;
        }
        // If the reader does not support seeking, we can only emulate forward seeks by consuming
        // bytes. If the reader has to seek backwards, return an error.
        else {
            let current_pos = self.reader.pos();
            if seek_pos >= current_pos {
                self.reader.ignore_bytes(seek_pos - current_pos)?;
            }
            else {
                return seek_error(SeekErrorKind::ForwardOnly);
            }
        }

        debug!("seeked to packet_ts={} (delta={})", actual_ts, actual_ts as i64 - ts as i64);

        Ok(SeekedTo { track_id: 0, actual_ts, required_ts: ts })
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.reader
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use symphonia_core::formats::FormatReader;
    use symphonia_core::io::ReadOnlySource;

    /// Creates a minimal valid RF64 file in memory.
    fn create_rf64_test_file(data_size: u64, sample_count: u64, pcm_data: &[u8]) -> Vec<u8> {
        let mut file = Vec::new();

        // RF64 header
        file.extend_from_slice(b"RF64");
        file.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes()); // placeholder size
        file.extend_from_slice(b"WAVE");

        // ds64 chunk (28 bytes minimum)
        file.extend_from_slice(b"ds64");
        file.extend_from_slice(&28u32.to_le_bytes()); // chunk size
        // Calculate actual riff size (not used in parsing but included for completeness)
        let riff_size: u64 = 4 + 8 + 28 + 8 + 16 + 8 + data_size; // WAVE + ds64 + fmt + data
        file.extend_from_slice(&riff_size.to_le_bytes()); // riffSize64
        file.extend_from_slice(&data_size.to_le_bytes()); // dataSize64
        file.extend_from_slice(&sample_count.to_le_bytes()); // sampleCount64
        file.extend_from_slice(&0u32.to_le_bytes()); // tableLength

        // fmt chunk (16 bytes, PCM format)
        file.extend_from_slice(b"fmt ");
        file.extend_from_slice(&16u32.to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes()); // format = PCM
        file.extend_from_slice(&1u16.to_le_bytes()); // channels = 1
        file.extend_from_slice(&44100u32.to_le_bytes()); // sample rate
        file.extend_from_slice(&88200u32.to_le_bytes()); // byte rate
        file.extend_from_slice(&2u16.to_le_bytes()); // block align
        file.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

        // data chunk
        file.extend_from_slice(b"data");
        // For RF64, data chunk size is 0xFFFFFFFF when > 4GB, actual size in ds64
        let chunk_size = if data_size > u32::MAX as u64 { 0xFFFFFFFF } else { data_size as u32 };
        file.extend_from_slice(&chunk_size.to_le_bytes());
        file.extend_from_slice(pcm_data);

        file
    }

    /// Creates a minimal valid standard WAV file in memory.
    fn create_wav_test_file(pcm_data: &[u8]) -> Vec<u8> {
        let mut file = Vec::new();
        let data_len = pcm_data.len() as u32;

        // RIFF header
        file.extend_from_slice(b"RIFF");
        let total_size = 4 + 8 + 16 + 8 + data_len; // WAVE + fmt chunk + data chunk
        file.extend_from_slice(&total_size.to_le_bytes());
        file.extend_from_slice(b"WAVE");

        // fmt chunk
        file.extend_from_slice(b"fmt ");
        file.extend_from_slice(&16u32.to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes()); // PCM
        file.extend_from_slice(&1u16.to_le_bytes()); // mono
        file.extend_from_slice(&44100u32.to_le_bytes()); // sample rate
        file.extend_from_slice(&88200u32.to_le_bytes()); // byte rate
        file.extend_from_slice(&2u16.to_le_bytes()); // block align
        file.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

        // data chunk
        file.extend_from_slice(b"data");
        file.extend_from_slice(&data_len.to_le_bytes());
        file.extend_from_slice(pcm_data);

        file
    }

    #[test]
    fn test_rf64_small_file() {
        // Test RF64 with data that fits in 32-bit (to verify format still works)
        let pcm_data = vec![0u8; 1000]; // 500 samples at 16-bit mono
        let rf64_file = create_rf64_test_file(1000, 500, &pcm_data);

        let source = ReadOnlySource::new(Cursor::new(rf64_file));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());

        let reader = WavReader::try_new(mss, &FormatOptions::default()).unwrap();

        assert_eq!(reader.tracks.len(), 1);
        let params = &reader.tracks[0].codec_params;
        assert_eq!(params.n_frames, Some(500));
        assert_eq!(params.sample_rate, Some(44100));
    }

    #[test]
    fn test_rf64_large_data_size() {
        // Test RF64 with >4GB data size (just the metadata, not actual data)
        let pcm_data = vec![0u8; 100]; // Small actual data for test
        let large_data_size: u64 = 5_000_000_000; // 5GB
        let sample_count = large_data_size / 2; // 16-bit samples
        let rf64_file = create_rf64_test_file(large_data_size, sample_count, &pcm_data);

        let source = ReadOnlySource::new(Cursor::new(rf64_file));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());

        let reader = WavReader::try_new(mss, &FormatOptions::default()).unwrap();

        assert_eq!(reader.tracks.len(), 1);
        let params = &reader.tracks[0].codec_params;
        // The n_frames should come from ds64's 64-bit sample count
        assert_eq!(params.n_frames, Some(sample_count));
        // data_end_pos should use 64-bit size
        assert_eq!(reader.data_end_pos - reader.data_start_pos, large_data_size);
    }

    #[test]
    fn test_standard_wav_unchanged() {
        // Verify standard WAV files still work (regression test)
        let pcm_data = vec![0u8; 100];
        let wav_file = create_wav_test_file(&pcm_data);

        let source = ReadOnlySource::new(Cursor::new(wav_file));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());

        let reader = WavReader::try_new(mss, &FormatOptions::default()).unwrap();

        assert_eq!(reader.tracks.len(), 1);
        assert_eq!(reader.data_end_pos - reader.data_start_pos, 100);
    }

    #[test]
    fn test_rf64_missing_ds64_uses_fallback() {
        // RF64 file without ds64 chunk should still work using 32-bit sizes
        let mut file = Vec::new();

        // RF64 header (but no ds64 chunk)
        file.extend_from_slice(b"RF64");
        let total_size = 4 + 8 + 16 + 8 + 100;
        file.extend_from_slice(&(total_size as u32).to_le_bytes());
        file.extend_from_slice(b"WAVE");

        // fmt chunk
        file.extend_from_slice(b"fmt ");
        file.extend_from_slice(&16u32.to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes()); // PCM
        file.extend_from_slice(&1u16.to_le_bytes()); // mono
        file.extend_from_slice(&44100u32.to_le_bytes());
        file.extend_from_slice(&88200u32.to_le_bytes());
        file.extend_from_slice(&2u16.to_le_bytes());
        file.extend_from_slice(&16u16.to_le_bytes());

        // data chunk
        file.extend_from_slice(b"data");
        file.extend_from_slice(&100u32.to_le_bytes());
        file.extend_from_slice(&vec![0u8; 100]);

        let source = ReadOnlySource::new(Cursor::new(file));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());

        // Should work, falling back to 32-bit sizes
        let reader = WavReader::try_new(mss, &FormatOptions::default()).unwrap();
        assert_eq!(reader.data_end_pos - reader.data_start_pos, 100);
    }

    #[test]
    fn test_ds64_ignored_in_standard_wav() {
        // Standard RIFF/WAV with a ds64 chunk should ignore it
        let mut file = Vec::new();

        // RIFF header
        file.extend_from_slice(b"RIFF");
        let total_size = 4 + 8 + 28 + 8 + 16 + 8 + 100; // includes ds64
        file.extend_from_slice(&(total_size as u32).to_le_bytes());
        file.extend_from_slice(b"WAVE");

        // ds64 chunk (should be ignored in standard WAV)
        file.extend_from_slice(b"ds64");
        file.extend_from_slice(&28u32.to_le_bytes());
        file.extend_from_slice(&999999999u64.to_le_bytes()); // fake riff size
        file.extend_from_slice(&888888888u64.to_le_bytes()); // fake data size (should NOT be used)
        file.extend_from_slice(&777777777u64.to_le_bytes()); // fake sample count
        file.extend_from_slice(&0u32.to_le_bytes());

        // fmt chunk
        file.extend_from_slice(b"fmt ");
        file.extend_from_slice(&16u32.to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&44100u32.to_le_bytes());
        file.extend_from_slice(&88200u32.to_le_bytes());
        file.extend_from_slice(&2u16.to_le_bytes());
        file.extend_from_slice(&16u16.to_le_bytes());

        // data chunk
        file.extend_from_slice(b"data");
        file.extend_from_slice(&100u32.to_le_bytes());
        file.extend_from_slice(&vec![0u8; 100]);

        let source = ReadOnlySource::new(Cursor::new(file));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());

        let reader = WavReader::try_new(mss, &FormatOptions::default()).unwrap();

        // Should use 32-bit size from data chunk, NOT 64-bit from ds64
        assert_eq!(reader.data_end_pos - reader.data_start_pos, 100);
    }
}
