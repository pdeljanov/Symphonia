// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{Seek, SeekFrom};

use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::errors::{Error, Result, SeekErrorKind};
use symphonia_core::errors::{decode_error, seek_error, unsupported_error};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_WAVE;
use symphonia_core::io::*;
use symphonia_core::meta::well_known::METADATA_ID_WAVE;
use symphonia_core::meta::{Metadata, MetadataInfo, MetadataLog};
use symphonia_core::support_format;

use log::{debug, error};

use crate::common::{
    ByteOrder, ChunksReader, PacketInfo, append_data_params, append_format_params, next_packet,
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

const WAVE_FORMAT_INFO: FormatInfo = FormatInfo {
    format: FORMAT_ID_WAVE,
    short_name: "wave",
    long_name: "Waveform Audio File Format",
};

const WAVE_METADATA_INFO: MetadataInfo = MetadataInfo {
    metadata: METADATA_ID_WAVE,
    short_name: "wave",
    long_name: "Waveform Audio File Format",
};

/// Waveform Audio File Format (WAV) format reader.
///
/// `WavReader` implements a demuxer for the WAVE container format.
pub struct WavReader<'s> {
    reader: MediaSourceStream<'s>,
    tracks: Vec<Track>,
    chapters: Option<ChapterGroup>,
    metadata: MetadataLog,
    packet_info: PacketInfo,
    data_start_pos: u64,
    data_end_pos: Option<u64>,
}

impl<'s> WavReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        // A Wave file is one large RIFF chunk, with the actual meta and audio data contained in
        // nested chunks. Therefore, the file starts with a RIFF chunk header (chunk ID & size).

        // The top-level chunk has the RIFF or RF64 chunk ID. This is also the file marker.
        let marker = mss.read_quad_bytes()?;

        let is_rf64 = match marker {
            WAVE_STREAM_MARKER => false,
            RF64_STREAM_MARKER => true,
            _ => return unsupported_error("wav: missing riff/rf64 stream marker"),
        };

        // The length of the top-level RIFF chunk. Must be atleast 4 bytes.
        // For RF64 files, this is 0xFFFFFFFF and the actual size is in the ds64 chunk.
        let riff_len = mss.read_u32()?;

        if riff_len < 4 && riff_len != u32::MAX {
            return decode_error("wav: invalid riff length");
        }

        // The form type. Only the WAVE form is supported.
        let riff_form = mss.read_quad_bytes()?;

        if riff_form != WAVE_RIFF_FORM {
            error!("riff form is not wave ({})", String::from_utf8_lossy(&riff_form));

            return unsupported_error("wav: riff form is not wave");
        }

        // When ffmpeg encodes wave to stdout the riff (parent) and data (child) chunk lengths are
        // (2^32)-1 since the size is not known ahead of time. For RF64 files, the riff length is
        // also 0xFFFFFFFF.
        let riff_data_len = if riff_len < u32::MAX { Some(riff_len - 4) } else { None };

        let mut riff_chunks =
            ChunksReader::<RiffWaveChunks>::new(riff_data_len, ByteOrder::LittleEndian);

        let mut codec_params = AudioCodecParameters::new();
        let mut metadata: MetadataLog = Default::default();
        let mut packet_info = None;
        let mut fact = None;
        let mut rf64_sizes = Rf64Sizes::default();

        loop {
            let chunk = riff_chunks.next(&mut mss)?;

            // The last chunk should always be a data chunk, if it is not, then the stream is
            // unsupported.
            if chunk.is_none() {
                return unsupported_error("wav: missing data chunk");
            }

            match chunk.unwrap() {
                RiffWaveChunks::Ds64(ds64_parser) => {
                    let ds64 = ds64_parser.parse(&mut mss)?;

                    // ds64 chunk is only meaningful in RF64 files. Ignore in standard WAV.
                    if !is_rf64 {
                        debug!("ignoring ds64 chunk in non-RF64 file");
                        continue;
                    }

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
                    let format = fmt.parse(&mut mss)?;

                    // The Format chunk contains the block_align field and possible additional
                    // information to handle packetization and seeking.
                    let info = format.packet_info()?;
                    codec_params
                        .with_max_frames_per_packet(info.max_frames_per_packet.get())
                        .with_frames_per_block(info.frames_per_block.get());

                    // Append Format chunk fields to codec parameters.
                    append_format_params(&mut codec_params, format.format_data, format.sample_rate);

                    packet_info = Some(info);
                }
                RiffWaveChunks::Fact(fct) => {
                    fact = Some(fct.parse(&mut mss)?);
                }
                RiffWaveChunks::List(lst) => {
                    let list = lst.parse(&mut mss)?;

                    // Riff Lists can have many different forms, but WavReader only supports Info
                    // lists.
                    match &list.form {
                        b"INFO" => metadata.push(read_info_chunk(&mut mss, list.len)?),
                        _ => list.skip(&mut mss)?,
                    }
                }
                RiffWaveChunks::Data(dat) => {
                    let data = dat.parse(&mut mss)?;

                    // Record the bounds of the data chunk.
                    let data_start_pos = mss.pos();

                    // Per EBU Tech 3306, ds64 values only replace the corresponding 32-bit
                    // field when that field is set to -1 (0xFFFFFFFF). DataChunk.len is None
                    // when the 32-bit value was 0xFFFFFFFF.
                    let data_len = match data.len {
                        Some(len) => Some(u64::from(len)),
                        None => rf64_sizes.data_size,
                    };

                    let data_end_pos = data_len.and_then(|len| data_start_pos.checked_add(len));

                    // Create the track.
                    let mut track = Track::new(0);

                    track.with_codec_params(CodecParameters::Audio(codec_params));

                    let Some(packet_info) = packet_info else {
                        return decode_error("wav: missing format chunk");
                    };

                    // Append Data chunk fields to track (sets num_frames from data length).
                    if let Some(data_len) = data_len {
                        append_data_params(&mut track, data_len, &packet_info);
                    }

                    // For RF64 files, prefer the sample count from ds64 over the computed
                    // value. For standard WAV, prefer fact chunk over computed value.
                    // Applied after append_data_params so the authoritative value wins.
                    if let Some(sample_count) = rf64_sizes.sample_count {
                        track.with_num_frames(sample_count);
                    } else if let Some(fact) = &fact {
                        append_fact_params(&mut track, fact);
                    }

                    // Instantiate the reader.
                    return Ok(WavReader {
                        reader: mss,
                        tracks: vec![track],
                        chapters: opts.external_data.chapters,
                        metadata: opts.external_data.metadata.unwrap_or_default(),
                        packet_info,
                        data_start_pos,
                        data_end_pos,
                    });
                }
            }
        }
    }
}

impl Scoreable for WavReader<'_> {
    fn score(mut src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        // Perform simple scoring by testing that the RIFF/RF64 stream marker and RIFF form are
        // both valid for WAVE.
        let marker = src.read_quad_bytes()?;
        src.ignore_bytes(4)?;
        let riff_form = src.read_quad_bytes()?;

        let is_valid_marker = marker == WAVE_STREAM_MARKER || marker == RF64_STREAM_MARKER;

        if !is_valid_marker || riff_form != WAVE_RIFF_FORM {
            return Ok(Score::Unsupported);
        }

        Ok(Score::Supported(255))
    }
}

impl ProbeableFormat<'_> for WavReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(WavReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[
            // WAVE RIFF form
            support_format!(
                WAVE_FORMAT_INFO,
                &["wav", "wave"],
                &["audio/vnd.wave", "audio/x-wav", "audio/wav", "audio/wave"],
                &[b"RIFF"]
            ),
            // RF64 extended WAVE format (64-bit extension for files > 4GB)
            support_format!(
                WAVE_FORMAT_INFO,
                &["wav", "wave", "rf64"],
                &["audio/vnd.wave", "audio/x-wav", "audio/wav", "audio/wave"],
                &[b"RF64"]
            ),
        ]
    }
}

impl FormatReader for WavReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &WAVE_FORMAT_INFO
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        next_packet(
            &mut self.reader,
            &self.packet_info,
            &self.tracks,
            self.data_start_pos,
            self.data_end_pos.unwrap_or(u64::MAX),
        )
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn chapters(&self) -> Option<&ChapterGroup> {
        self.chapters.as_ref()
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        if self.tracks.is_empty() {
            return seek_error(SeekErrorKind::Unseekable);
        }

        let track = &self.tracks[0];

        let required_ts = match to {
            // Frame timestamp given.
            SeekTo::TimeStamp { ts, .. } => ts,
            // Time value given, calculate frame timestamp using the time base.
            SeekTo::Time { time, .. } => {
                // The timebase is required to calculate the timestamp.
                let tb = track.time_base.ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;

                // If the timestamp overflows, the seek if out-of-range.
                tb.calc_timestamp(time).ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?
            }
        };

        // Negative timestamps are not allowed.
        if required_ts.is_negative() {
            return seek_error(SeekErrorKind::OutOfRange);
        }

        // If the total number of frames in the track is known, verify the desired frame timestamp
        // does not exceed it.
        if let Some(num_frames) = track.num_frames {
            if required_ts.get() as u64 > num_frames {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        }

        debug!("seeking to frame_ts={required_ts}");

        // WAVE is not internally packetized for PCM codecs. Packetization is simulated by trying to
        // read a constant number of samples or blocks every call to next_packet. Therefore, a
        // packet begins wherever the data stream is currently positioned. Since timestamps on
        // packets should be determinstic, instead of seeking to the exact timestamp requested and
        // starting the next packet there, seek to a packet boundary. In this way, packets will have
        // the same timestamps regardless if the stream was seeked or not.
        let actual_ts = self.packet_info.get_actual_ts(required_ts);

        // Calculate the absolute byte offset of the desired audio frame.
        let seek_pos =
            self.data_start_pos + (actual_ts.get() as u64 * self.packet_info.block_size.get());

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

        debug!(
            "seeked to packet_ts={} (delta={})",
            actual_ts,
            actual_ts.saturating_delta(required_ts)
        );

        Ok(SeekedTo { track_id: 0, actual_ts, required_ts })
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use symphonia_core::io::ReadOnlySource;

    /// Creates a minimal valid RF64 file in memory.
    fn create_rf64_test_file(data_size: u64, sample_count: u64, pcm_data: &[u8]) -> Vec<u8> {
        let mut file = Vec::new();

        // RF64 header
        file.extend_from_slice(b"RF64");
        file.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes()); // placeholder size
        file.extend_from_slice(b"WAVE");

        // ds64 chunk (28 bytes)
        file.extend_from_slice(b"ds64");
        file.extend_from_slice(&28u32.to_le_bytes()); // chunk size
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
        let pcm_data = vec![0u8; 1000]; // 500 samples at 16-bit mono
        let rf64_file = create_rf64_test_file(1000, 500, &pcm_data);

        let source = ReadOnlySource::new(Cursor::new(rf64_file));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());

        let reader = WavReader::try_new(mss, FormatOptions::default()).unwrap();

        assert_eq!(reader.tracks.len(), 1);
        assert_eq!(reader.tracks[0].num_frames, Some(500));
    }

    #[test]
    fn test_rf64_large_data_size() {
        let pcm_data = vec![0u8; 100];
        let large_data_size: u64 = 5_000_000_000; // 5GB
        let sample_count = large_data_size / 2; // 16-bit samples
        let rf64_file = create_rf64_test_file(large_data_size, sample_count, &pcm_data);

        let source = ReadOnlySource::new(Cursor::new(rf64_file));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());

        let reader = WavReader::try_new(mss, FormatOptions::default()).unwrap();

        assert_eq!(reader.tracks.len(), 1);
        assert_eq!(reader.tracks[0].num_frames, Some(sample_count));
        // data_end_pos should use 64-bit size
        let data_end = reader.data_end_pos.unwrap();
        assert_eq!(data_end - reader.data_start_pos, large_data_size);
    }

    #[test]
    fn test_standard_wav_unchanged() {
        let pcm_data = vec![0u8; 100];
        let wav_file = create_wav_test_file(&pcm_data);

        let source = ReadOnlySource::new(Cursor::new(wav_file));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());

        let reader = WavReader::try_new(mss, FormatOptions::default()).unwrap();

        assert_eq!(reader.tracks.len(), 1);
        let data_end = reader.data_end_pos.unwrap();
        assert_eq!(data_end - reader.data_start_pos, 100);
    }

    #[test]
    fn test_rf64_missing_ds64_uses_fallback() {
        // RF64 file without ds64 chunk should still work using 32-bit sizes.
        let mut file = Vec::new();

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

        let reader = WavReader::try_new(mss, FormatOptions::default()).unwrap();
        let data_end = reader.data_end_pos.unwrap();
        assert_eq!(data_end - reader.data_start_pos, 100);
    }

    #[test]
    fn test_ds64_ignored_in_standard_wav() {
        // Standard RIFF/WAV with a ds64 chunk should ignore it.
        let mut file = Vec::new();

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

        let reader = WavReader::try_new(mss, FormatOptions::default()).unwrap();

        // Should use 32-bit size from data chunk, NOT 64-bit from ds64
        let data_end = reader.data_end_pos.unwrap();
        assert_eq!(data_end - reader.data_start_pos, 100);
    }
}
