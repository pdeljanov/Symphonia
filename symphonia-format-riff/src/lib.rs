// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]
// The following lints are allowed in all Symphonia crates. Please see clippy.toml for their
// justification.
#![allow(clippy::comparison_chain)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_range_contains)]

use std::io::{Seek, SeekFrom};

use symphonia_core::codecs::CodecParameters;
use symphonia_core::errors::{decode_error, end_of_stream_error, seek_error, unsupported_error};
use symphonia_core::errors::{Result, SeekErrorKind};
use symphonia_core::formats::prelude::*;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};
use symphonia_core::support_format;

use log::{debug, error};

mod chunks;

use chunks::*;

/// Aiff is actually a RIFF stream, with a "FORM" ASCII stream marker.
const AIFF_STREAM_MARKER: [u8; 4] = *b"FORM";

/// A possible RIFF form is "aiff".
const AIFF_RIFF_FORM: [u8; 4] = *b"AIFF";
/// A possible RIFF form is "aifc", using compressed data.
const AIFC_RIFF_FORM: [u8; 4] = *b"AIFC";

/// The maximum number of frames that will be in a packet.
/// Took this from symphonia-format-wav, but I don't know if it's correct.
const AIFF_MAX_FRAMES_PER_PACKET: u64 = 1152;

pub(crate) struct PacketInfo {
    block_size: u64,
    frames_per_block: u64,
    max_blocks_per_packet: u64,
}

impl PacketInfo {
    #[allow(dead_code)]
    fn with_blocks(block_size: u16, frames_per_block: u64) -> Result<Self> {
        if frames_per_block == 0 {
            return decode_error("riff: frames per block is 0");
        }
        Ok(Self {
            block_size: u64::from(block_size),
            frames_per_block,
            max_blocks_per_packet: frames_per_block.max(AIFF_MAX_FRAMES_PER_PACKET)
                / frames_per_block,
        })
    }

    fn without_blocks(frame_len: u16) -> Self {
        Self {
            block_size: u64::from(frame_len),
            frames_per_block: 1,
            max_blocks_per_packet: AIFF_MAX_FRAMES_PER_PACKET,
        }
    }

    fn is_empty(&self) -> bool {
        self.block_size == 0
    }

    fn get_max_frames_per_packet(&self) -> u64 {
        self.max_blocks_per_packet * self.frames_per_block
    }

    fn get_frames(&self, data_len: u64) -> u64 {
        data_len / self.block_size * self.frames_per_block
    }

    fn get_actual_ts(&self, ts: u64) -> u64 {
        let max_frames_per_packet = self.get_max_frames_per_packet();
        ts / max_frames_per_packet * max_frames_per_packet
    }
}

pub struct RiffReader {
    reader: MediaSourceStream,
    tracks: Vec<Track>,
    cues: Vec<Cue>,
    metadata: MetadataLog,
    packet_info: PacketInfo,
    data_start_pos: u64,
    data_end_pos: u64,
}

impl QueryDescriptor for RiffReader {
    fn query() -> &'static [Descriptor] {
        &[
            // AIFF RIFF form
            support_format!(
                "riff",
                " Resource Interchange File Format",
                &["aiff", "aif", "aifc"], 
                &["audio/aiff", "audio/x-aiff", " sound/aiff", "audio/x-pn-aiff"], 
                &[b"FORM"] // TODO: In v0.6 this should also support wave ("RIFF") and avi
            ),
        ]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}

impl FormatReader for RiffReader {
    fn try_new(mut source: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {
        // The FORM marker should be present.
        // TODO: in v0.6 this should also support wave and avi
        let marker = source.read_quad_bytes()?;
        if marker != AIFF_STREAM_MARKER {
            return unsupported_error("riff: missing riff stream marker");
        }

        // File is basically one RIFF chunk, with the actual meta and audio data as sub-chunks (called local chunks).
        // Therefore, the header was the chunk ID, and the next 4 bytes is the length of the RIFF
        // chunk.
        let riff_len = source.read_be_u32()?;
        let riff_form = source.read_quad_bytes()?;

        // TODO: in v0.6 this should also support wave and avi
        if riff_form == AIFF_RIFF_FORM {
            debug!("riff form is aiff");
        } else if riff_form == AIFC_RIFF_FORM {
            return unsupported_error("riff: No support for aifc files");
        } else {
            error!("riff form is not supported ({})", String::from_utf8_lossy(&riff_form));
            return unsupported_error("riff: riff form is not supported");
        }

        let mut riff_chunks = ChunksReader::<RiffAiffChunks>::new(riff_len);

        let mut codec_params = CodecParameters::new();
        //TODO: Chunks such as marker contain metadata, get it.
        let metadata: MetadataLog = Default::default();
        let mut packet_info = PacketInfo::without_blocks(0);

        // TODO: in v0.6 this should also support wave and avi
        loop {
            let chunk = riff_chunks.next(&mut source)?;

            // The last chunk should always be a data chunk, if it is not, then the stream is
            // unsupported. 
            // TODO: According to the spec additional chunks can be added after the sound data chunk. In fact any order can be possible.
            if chunk.is_none() {
                return unsupported_error("riff: missing data chunk");
            }

            match chunk.unwrap() {
                RiffAiffChunks::Common(common) => {
                    let common = common.parse(&mut source)?;

                    // The Format chunk contains the block_align field and possible additional information
                    // to handle packetization and seeking.
                    packet_info = common.packet_info()?;
                    codec_params
                        .with_max_frames_per_packet(packet_info.get_max_frames_per_packet())
                        .with_frames_per_block(packet_info.frames_per_block);

                    // Append Format chunk fields to codec parameters.
                    append_common_params(&mut codec_params, common);
                }
                RiffAiffChunks::Sound(dat) => {
                    let data = dat.parse(&mut source)?;

                    // Record the bounds of the data chunk.
                    let data_start_pos = source.pos();
                    let data_end_pos = data_start_pos + u64::from(data.len);

                    // Append Sound chunk fields to codec parameters.
                    append_sound_params(&mut codec_params, &data, &packet_info);

                    // Add a new track using the collected codec parameters.
                    return Ok(RiffReader {
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
        let pos = self.reader.pos();
        if self.tracks.is_empty() {
            return decode_error("riff: no tracks");
        }
        if self.packet_info.is_empty() {
            return decode_error("riff: block size is 0");
        }

        // Determine the number of complete blocks remaining in the data chunk.
        let num_blocks_left = if pos < self.data_end_pos {
            (self.data_end_pos - pos) / self.packet_info.block_size
        }
        else {
            0
        };

        if num_blocks_left == 0 {
            return end_of_stream_error();
        }

        let blocks_per_packet = num_blocks_left.min(self.packet_info.max_blocks_per_packet);

        let dur = blocks_per_packet * self.packet_info.frames_per_block;
        let packet_len = blocks_per_packet * self.packet_info.block_size;

        // Copy the frames.
        let packet_buf = self.reader.read_boxed_slice(packet_len as usize)?;

        // The packet timestamp is the position of the first byte of the first frame in the
        // packet relative to the start of the data chunk divided by the length per frame.
        let pts = self.packet_info.get_frames(pos - self.data_start_pos);

        Ok(Packet::new_from_boxed_slice(0, pts, dur, packet_buf))
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

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.reader
    }
}

fn append_common_params(codec_params: &mut CodecParameters, format: CommonChunk) {
    codec_params
        .with_sample_rate(format.sample_rate)
        .with_time_base(TimeBase::new(1, format.sample_rate));

    match format.format_data {
        FormatData::Pcm(pcm) => {
            codec_params
                .for_codec(pcm.codec)
                .with_bits_per_coded_sample(u32::from(pcm.bits_per_sample))
                .with_bits_per_sample(u32::from(pcm.bits_per_sample))
                .with_channels(pcm.channels);
        }
    }
}

fn append_sound_params(
    codec_params: &mut CodecParameters,
    data: &SoundChunk,
    packet_info: &PacketInfo,
) {
    if !packet_info.is_empty() {
        let n_frames = packet_info.get_frames(u64::from(data.len));
        codec_params.with_n_frames(n_frames);
    }
}