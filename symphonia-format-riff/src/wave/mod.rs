// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{Seek, SeekFrom};

use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::codecs::CodecParameters;
use symphonia_core::errors::{decode_error, seek_error, unsupported_error};
use symphonia_core::errors::{Result, SeekErrorKind};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_WAVE;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::support_format;

use log::{debug, error};

use crate::common::{
    append_data_params, append_format_params, next_packet, ByteOrder, ChunksReader, PacketInfo,
};
mod chunks;
use chunks::*;

/// WAVE is actually a RIFF stream, with a "RIFF" ASCII stream marker.
const WAVE_STREAM_MARKER: [u8; 4] = *b"RIFF";
/// A possible RIFF form is "wave".
const WAVE_RIFF_FORM: [u8; 4] = *b"WAVE";

const WAVE_FORMAT_INFO: FormatInfo = FormatInfo {
    format: FORMAT_ID_WAVE,
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

        // The top-level chunk has the RIFF chunk ID. This is also the file marker.
        let marker = mss.read_quad_bytes()?;

        if marker != WAVE_STREAM_MARKER {
            return unsupported_error("wav: missing wave riff stream marker");
        }

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
        // (2^32)-1 since the size is not known ahead of time.
        let riff_data_len = if riff_len < u32::MAX { Some(riff_len - 4) } else { None };

        let mut riff_chunks =
            ChunksReader::<RiffWaveChunks>::new(riff_data_len, ByteOrder::LittleEndian);

        let mut codec_params = AudioCodecParameters::new();
        let mut metadata: MetadataLog = Default::default();
        let mut packet_info = PacketInfo::without_blocks(0);
        let mut fact = None;

        loop {
            let chunk = riff_chunks.next(&mut mss)?;

            // The last chunk should always be a data chunk, if it is not, then the stream is
            // unsupported.
            if chunk.is_none() {
                return unsupported_error("wav: missing data chunk");
            }

            match chunk.unwrap() {
                RiffWaveChunks::Format(fmt) => {
                    let format = fmt.parse(&mut mss)?;

                    // The Format chunk contains the block_align field and possible additional
                    // information to handle packetization and seeking.
                    packet_info = format.packet_info()?;
                    codec_params
                        .with_max_frames_per_packet(packet_info.get_max_frames_per_packet())
                        .with_frames_per_block(packet_info.frames_per_block);

                    // Append Format chunk fields to codec parameters.
                    append_format_params(&mut codec_params, format.format_data, format.sample_rate);
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
                    let data_end_pos = data.len.map(|len| data_start_pos + u64::from(len));

                    // Create the track.
                    let mut track = Track::new(0);

                    track.with_codec_params(CodecParameters::Audio(codec_params));

                    // Append Fact chunk fields to track.
                    if let Some(fact) = &fact {
                        append_fact_params(&mut track, fact);
                    }

                    // Append Data chunk fields to track.
                    if let Some(data_len) = data.len {
                        append_data_params(&mut track, u64::from(data_len), &packet_info);
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
        // Perform simple scoring by testing that the RIFF stream marker and RIFF form are both
        // valid for WAVE.
        let riff_marker = src.read_quad_bytes()?;
        src.ignore_bytes(4)?;
        let riff_form = src.read_quad_bytes()?;

        if riff_marker != WAVE_STREAM_MARKER || riff_form != WAVE_RIFF_FORM {
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
        if self.tracks.is_empty() || self.packet_info.is_empty() {
            return seek_error(SeekErrorKind::Unseekable);
        }

        let track = &self.tracks[0];

        let ts = match to {
            // Frame timestamp given.
            SeekTo::TimeStamp { ts, .. } => ts,
            // Time value given, calculate frame timestamp using the time base.
            SeekTo::Time { time, .. } => {
                // Use the time base to calculate the frame timestamp. If time base is not
                // known, the seek cannot be completed.
                if let Some(tb) = track.time_base {
                    tb.calc_timestamp(time)
                }
                else {
                    return seek_error(SeekErrorKind::Unseekable);
                }
            }
        };

        // If the total number of frames in the track is known, verify the desired frame timestamp
        // does not exceed it.
        if let Some(num_frames) = track.num_frames {
            if ts > num_frames {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        }

        debug!("seeking to frame_ts={}", ts);

        // WAVE is not internally packetized for PCM codecs. Packetization is simulated by trying to
        // read a constant number of samples or blocks every call to next_packet. Therefore, a
        // packet begins wherever the data stream is currently positioned. Since timestamps on
        // packets should be determinstic, instead of seeking to the exact timestamp requested and
        // starting the next packet there, seek to a packet boundary. In this way, packets will have
        // the same timestamps regardless if the stream was seeked or not.
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

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}
