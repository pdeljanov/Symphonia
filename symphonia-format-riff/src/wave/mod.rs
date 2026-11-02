// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
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
/// WAVE files over 4 GB are RF64 (EBU Tech 3306) or BW64 (ITU-R BS.2088) streams. Both are RIFF
/// with a different marker and a ds64 chunk carrying the 64-bit sizes.
const RF64_STREAM_MARKER: [u8; 4] = *b"RF64";
const BW64_STREAM_MARKER: [u8; 4] = *b"BW64";
/// A possible RIFF form is "wave".
const WAVE_RIFF_FORM: [u8; 4] = *b"WAVE";

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
    media_info: MediaInfo,
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

        // The top-level chunk has the RIFF chunk ID. This is also the file marker.
        let marker = mss.read_quad_bytes()?;

        let is_rf64 = match marker {
            WAVE_STREAM_MARKER => false,
            RF64_STREAM_MARKER | BW64_STREAM_MARKER => true,
            _ => return unsupported_error("wav: missing wave riff stream marker"),
        };

        // The length of the top-level RIFF chunk. Must be atleast 4 bytes.
        let riff_len = mss.read_u32()?;

        if riff_len < 4 {
            return decode_error("wav: invalid riff length");
        }

        // The form type. Only the WAVE form is supported.
        let riff_form = mss.read_quad_bytes()?;

        if riff_form != WAVE_RIFF_FORM {
            error!("riff form is not wave ({})", String::from_utf8_lossy(&riff_form));

            return unsupported_error("wav: riff form is not wave");
        }

        // When ffmpeg encodes wave to stdout the riff (parent) and data (child) chunk lengths are
        // (2^32)-1 since the size is not known ahead of time. RF64 sets them to (2^32)-1 as well,
        // with the real sizes in the ds64 chunk.
        let riff_data_len = if riff_len < u32::MAX { Some(riff_len - 4) } else { None };

        let mut riff_chunks =
            ChunksReader::<RiffWaveChunks>::new(riff_data_len, ByteOrder::LittleEndian);

        let mut codec_params = AudioCodecParameters::new();
        let mut metadata: MetadataLog = Default::default();
        let mut packet_info = None;
        let mut fact = None;
        let mut ds64: Option<Ds64Chunk> = None;

        loop {
            let chunk = riff_chunks.next(&mut mss)?;

            // The last chunk should always be a data chunk, if it is not, then the stream is
            // unsupported.
            let Some(chunk) = chunk
            else {
                return unsupported_error("wav: missing data chunk");
            };

            match chunk {
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
                RiffWaveChunks::Ds64(ds) => {
                    let chunk = ds.parse_and_skip_unread(&mut mss)?;

                    // A ds64 chunk only has meaning in an RF64 or BW64 stream.
                    if is_rf64 {
                        ds64 = Some(chunk);
                    }
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

                    // In RF64 a data length of (2^32)-1 means the real length is in the ds64 chunk.
                    let data_len = match (data.len, &ds64) {
                        (Some(len), _) => Some(u64::from(len)),
                        (None, Some(ds64)) => Some(ds64.data_size),
                        (None, None) if is_rf64 => {
                            return decode_error("wav: rf64 missing ds64 chunk");
                        }
                        (None, None) => None,
                    };

                    // Record the bounds of the data chunk.
                    let data_start_pos = mss.pos();
                    let data_end_pos = data_len.map(|len| data_start_pos + len);

                    // Create the track.
                    let mut track = Track::new(0);

                    track.with_codec_params(CodecParameters::Audio(codec_params));

                    let Some(packet_info) = packet_info
                    else {
                        return decode_error("wav: missing format chunk");
                    };

                    // Append Fact chunk fields to track.
                    if let Some(fact) = &fact {
                        let num_frames = match &ds64 {
                            Some(ds64) if fact.num_frames == u32::MAX => ds64.sample_count,
                            _ => u64::from(fact.num_frames),
                        };
                        append_fact_params(&mut track, num_frames);
                    }

                    // Append Data chunk fields to track.
                    if let Some(data_len) = data_len {
                        append_data_params(&mut track, data_len, &packet_info);
                    }

                    // Instantiate the reader.
                    return Ok(WavReader {
                        reader: mss,
                        media_info: MediaInfo::from_track(&track),
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
        // Perform simple scoring by testing that the RIFF stream marker and RIFF form are both
        // valid for WAVE.
        let riff_marker = src.read_quad_bytes()?;
        src.ignore_bytes(4)?;
        let riff_form = src.read_quad_bytes()?;

        let is_riff_marker =
            matches!(riff_marker, WAVE_STREAM_MARKER | RF64_STREAM_MARKER | BW64_STREAM_MARKER);

        if !is_riff_marker || riff_form != WAVE_RIFF_FORM {
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
                &[b"RIFF", b"RF64", b"BW64"]
            ),
        ]
    }
}

impl FormatReader for WavReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &WAVE_FORMAT_INFO
    }

    fn media_info(&self) -> &MediaInfo {
        &self.media_info
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
            SeekTo::Timestamp { ts, .. } => ts,
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
    use std::io::Cursor;

    use symphonia_core::formats::probe::{Hint, Probe};
    use symphonia_core::formats::{FormatOptions, FormatReader};
    use symphonia_core::io::MediaSourceStream;
    use symphonia_core::meta::MetadataOptions;

    use super::WavReader;

    const NUM_FRAMES: u32 = 64;
    const BLOCK_ALIGN: u32 = 4;

    /// 16-bit stereo PCM. The bytes contain an ADTS-like 0xFFF1 sync word, which is what the probe
    /// latched onto when RF64 was not recognised.
    fn pcm() -> Vec<u8> {
        (0..NUM_FRAMES * BLOCK_ALIGN).map(|i| if i % 2 == 0 { 0xff } else { 0xf1 }).collect()
    }

    fn chunk(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = tag.to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    fn fmt_chunk() -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&1u16.to_le_bytes()); // PCM
        body.extend_from_slice(&2u16.to_le_bytes()); // channels
        body.extend_from_slice(&48_000u32.to_le_bytes()); // sample rate
        body.extend_from_slice(&(48_000 * BLOCK_ALIGN).to_le_bytes()); // byte rate
        body.extend_from_slice(&(BLOCK_ALIGN as u16).to_le_bytes()); // block align
        body.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        chunk(b"fmt ", &body)
    }

    fn ds64_chunk(data_size: u64, sample_count: u64) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&u64::MAX.to_le_bytes()); // riff size, unused
        body.extend_from_slice(&data_size.to_le_bytes());
        body.extend_from_slice(&sample_count.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes()); // one table entry, skipped
        body.extend_from_slice(b"junk");
        body.extend_from_slice(&0u64.to_le_bytes());
        chunk(b"ds64", &body)
    }

    /// An RF64-style file: sizes over 4 GB are written as u32::MAX. A chunk follows the audio, so
    /// reading past the ds64 data size would return it as samples.
    fn rf64(marker: &[u8; 4], ds64: bool, fact: Option<u32>) -> Vec<u8> {
        let mut out = marker.to_vec();
        out.extend_from_slice(&u32::MAX.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        if ds64 {
            out.extend(ds64_chunk(u64::from(NUM_FRAMES * BLOCK_ALIGN), u64::from(NUM_FRAMES)));
        }
        out.extend(fmt_chunk());
        if let Some(num_frames) = fact {
            out.extend(chunk(b"fact", &num_frames.to_le_bytes()));
        }
        out.extend_from_slice(b"data");
        out.extend_from_slice(&u32::MAX.to_le_bytes());
        out.extend(pcm());
        out.extend(chunk(b"junk", &[0x55; 64]));
        out
    }

    fn riff() -> Vec<u8> {
        let body = [b"WAVE".to_vec(), fmt_chunk(), chunk(b"data", &pcm())].concat();
        [b"RIFF".to_vec(), (body.len() as u32).to_le_bytes().to_vec(), body].concat()
    }

    fn mss(buf: Vec<u8>) -> MediaSourceStream<'static> {
        MediaSourceStream::new(Box::new(Cursor::new(buf)), Default::default())
    }

    fn read_all(reader: &mut dyn FormatReader) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(packet) = reader.next_packet().unwrap() {
            out.extend_from_slice(&packet.data);
        }
        out
    }

    fn assert_reads_pcm(buf: Vec<u8>) {
        let mut reader = WavReader::try_new(mss(buf), FormatOptions::default()).unwrap();
        assert_eq!(reader.tracks()[0].num_frames, Some(u64::from(NUM_FRAMES)));
        assert_eq!(read_all(&mut reader), pcm());
    }

    #[test]
    fn reads_rf64_sizes_from_ds64() {
        assert_reads_pcm(rf64(b"RF64", true, None));
    }

    #[test]
    fn reads_bw64() {
        assert_reads_pcm(rf64(b"BW64", true, None));
    }

    #[test]
    fn rf64_fact_uses_ds64_sample_count() {
        assert_reads_pcm(rf64(b"RF64", true, Some(u32::MAX)));
    }

    #[test]
    fn rf64_without_ds64_is_an_error() {
        let err = WavReader::try_new(mss(rf64(b"RF64", false, None)), FormatOptions::default())
            .err()
            .unwrap();
        assert!(err.to_string().contains("ds64"), "{err}");
    }

    #[test]
    fn plain_riff_unchanged() {
        assert_reads_pcm(riff());
    }

    #[test]
    fn probe_recognises_rf64() {
        let mut probe = Probe::new();
        probe.register_format::<WavReader<'_>>();

        let mut reader = probe
            .probe(
                &Hint::new(),
                mss(rf64(b"RF64", true, None)),
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .unwrap();

        assert_eq!(reader.format_info().short_name, "wave");
        assert_eq!(read_all(reader.as_mut()), pcm());
    }
}
