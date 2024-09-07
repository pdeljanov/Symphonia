// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{Seek, SeekFrom};

use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::codecs::CodecParameters;
use symphonia_core::errors::{seek_error, unsupported_error};
use symphonia_core::errors::{Result, SeekErrorKind};
use symphonia_core::formats::{prelude::*, FORMAT_TYPE_AIFF};
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::support_format;

use log::debug;

use crate::common::{
    append_data_params, append_format_params, next_packet, ByteOrder, ChunksReader, PacketInfo,
};
mod chunks;
use chunks::*;

/// Aiff is actually a RIFF stream, with a "FORM" ASCII stream marker.
const AIFF_STREAM_MARKER: [u8; 4] = *b"FORM";
/// A possible RIFF form is "aiff".
const AIFF_RIFF_FORM: [u8; 4] = *b"AIFF";
/// A possible RIFF form is "aifc", using compressed data.
const AIFC_RIFF_FORM: [u8; 4] = *b"AIFC";

const AIFF_FORMAT_INFO: FormatInfo = FormatInfo {
    format: FORMAT_TYPE_AIFF,
    short_name: "aiff",
    long_name: "Audio Interchange File Format",
};

/// Audio Interchange File Format (AIFF) format reader.
///
/// `AiffReader` implements a demuxer for the AIFF container format.
pub struct AiffReader<'s> {
    reader: MediaSourceStream<'s>,
    tracks: Vec<Track>,
    cues: Vec<Cue>,
    metadata: MetadataLog,
    packet_info: PacketInfo,
    data_start_pos: u64,
    data_end_pos: u64,
}

impl<'s> AiffReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        // The FORM marker should be present.
        let marker = mss.read_quad_bytes()?;
        if marker != AIFF_STREAM_MARKER {
            return unsupported_error("aiff: missing riff stream marker");
        }

        // File is basically one RIFF chunk, with the actual meta and audio data as sub-chunks (called local chunks).
        // Therefore, the header was the chunk ID, and the next 4 bytes is the length of the RIFF
        // chunk.
        let riff_len = mss.read_be_u32()?;
        let riff_form = mss.read_quad_bytes()?;

        let mut riff_chunks = ChunksReader::<RiffAiffChunks>::new(riff_len, ByteOrder::BigEndian);

        let mut codec_params = AudioCodecParameters::new();
        //TODO: Chunks such as marker contain metadata, get it.
        let metadata = opts.metadata.unwrap_or_default();
        let mut packet_info = PacketInfo::without_blocks(0);

        loop {
            let chunk = riff_chunks.next(&mut mss)?;

            // The last chunk should always be a data chunk, if it is not, then the stream is
            // unsupported.
            // TODO: According to the spec additional chunks can be added after the sound data chunk. In fact any order can be possible.
            if chunk.is_none() {
                return unsupported_error("aiff: missing sound chunk");
            }

            match chunk.unwrap() {
                RiffAiffChunks::Common(common) => {
                    let common = match riff_form {
                        AIFF_RIFF_FORM => common.parse_aiff(&mut mss)?,
                        AIFC_RIFF_FORM => common.parse_aifc(&mut mss)?,
                        _ => return unsupported_error("aiff: riff form is not supported"),
                    };

                    // The Format chunk contains the block_align field and possible additional information
                    // to handle packetization and seeking.
                    packet_info = common.packet_info()?;
                    codec_params
                        .with_max_frames_per_packet(packet_info.get_max_frames_per_packet())
                        .with_frames_per_block(packet_info.frames_per_block);

                    // Append Format chunk fields to codec parameters.
                    append_format_params(&mut codec_params, common.format_data, common.sample_rate);
                }
                RiffAiffChunks::Sound(dat) => {
                    let data = dat.parse(&mut mss)?;

                    // Record the bounds of the data chunk.
                    let data_start_pos = mss.pos();
                    let data_end_pos = data_start_pos + u64::from(data.len);

                    // Create a new track using the collected codec parameters.
                    let mut track = Track::new(0);

                    track.with_codec_params(CodecParameters::Audio(codec_params));

                    // Append Sound chunk fields to track.
                    append_data_params(&mut track, data.len as u64, &packet_info);

                    // Instantiate the AIFF reader.
                    return Ok(AiffReader {
                        reader: mss,
                        tracks: vec![track],
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
}

impl Scoreable for AiffReader<'_> {
    fn score(_src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        Ok(Score::Supported(255))
    }
}

impl ProbeableFormat<'_> for AiffReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(AiffReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[
            // AIFF RIFF form
            support_format!(
                AIFF_FORMAT_INFO,
                &["aiff", "aif", "aifc"],
                &["audio/aiff", "audio/x-aiff", " sound/aiff", "audio/x-pn-aiff"],
                &[b"FORM"]
            ),
        ]
    }
}

impl FormatReader for AiffReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &AIFF_FORMAT_INFO
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
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

        let track = &self.tracks[0];

        let ts = match to {
            // Frame timestamp given.
            SeekTo::TimeStamp { ts, .. } => ts,
            // Time value given, calculate frame timestamp using the timebase.
            SeekTo::Time { time, .. } => {
                // Use the timebase to calculate the frame timestamp. If timebase is not
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
        if let Some(n_frames) = track.num_frames {
            if ts > n_frames {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        }

        debug!("seeking to frame_ts={}", ts);

        // RIFF is not internally packetized for PCM codecs. Packetization is simulated by trying to
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

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}
