// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![deprecated(since = "0.5.4", note = "superseded by `symphonia-format-riff`")]
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
use symphonia_core::meta::{Metadata, MetadataBuilder, MetadataLog, MetadataRevision};
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};
use symphonia_core::support_format;

use log::{debug, error};

mod chunks;

use chunks::*;

/// WAVE is actually a RIFF stream, with a "RIFF" ASCII stream marker.
const WAVE_STREAM_MARKER: [u8; 4] = *b"RIFF";

/// The RIFF form is "wave".
const WAVE_RIFF_FORM: [u8; 4] = *b"WAVE";

/// The maximum number of frames that will be in a packet.
const WAVE_MAX_FRAMES_PER_PACKET: u64 = 1152;

/// `PacketInfo` helps to simulate packetization over a number of blocks of data.
/// In case the codec is blockless the block size equals one full audio frame in bytes.
pub(crate) struct PacketInfo {
    block_size: u64,
    frames_per_block: u64,
    max_blocks_per_packet: u64,
}

impl PacketInfo {
    fn with_blocks(block_size: u16, frames_per_block: u64) -> Result<Self> {
        if frames_per_block == 0 {
            return decode_error("wav: frames per block is 0");
        }
        Ok(Self {
            block_size: u64::from(block_size),
            frames_per_block,
            max_blocks_per_packet: frames_per_block.max(WAVE_MAX_FRAMES_PER_PACKET)
                / frames_per_block,
        })
    }

    fn without_blocks(frame_len: u16) -> Self {
        Self {
            block_size: u64::from(frame_len),
            frames_per_block: 1,
            max_blocks_per_packet: WAVE_MAX_FRAMES_PER_PACKET,
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

/// WAVE (WAV) format reader.
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
            // WAVE RIFF form
            support_format!(
                "wave",
                "Waveform Audio File Format",
                &["wav", "wave"],
                &["audio/vnd.wave", "audio/x-wav", "audio/wav", "audio/wave"],
                &[b"RIFF"]
            ),
        ]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}

impl FormatReader for WavReader {
    fn try_new(mut source: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {
        // The RIFF marker should be present.
        let marker = source.read_quad_bytes()?;

        if marker != WAVE_STREAM_MARKER {
            return unsupported_error("wav: missing riff stream marker");
        }

        // A Wave file is one large RIFF chunk, with the actual meta and audio data as sub-chunks.
        // Therefore, the header was the chunk ID, and the next 4 bytes is the length of the RIFF
        // chunk.
        let riff_len = source.read_u32()?;
        let riff_form = source.read_quad_bytes()?;

        // The RIFF chunk contains WAVE data.
        if riff_form != WAVE_RIFF_FORM {
            error!("riff form is not wave ({})", String::from_utf8_lossy(&riff_form));

            return unsupported_error("wav: riff form is not wave");
        }

        let mut riff_chunks = ChunksReader::<RiffWaveChunks>::new(riff_len);

        let mut codec_params = CodecParameters::new();
        let mut metadata: MetadataLog = Default::default();
        let mut packet_info = PacketInfo::without_blocks(0);

        loop {
            let chunk = riff_chunks.next(&mut source)?;

            // The last chunk should always be a data chunk, if it is not, then the stream is
            // unsupported.
            if chunk.is_none() {
                return unsupported_error("wav: missing data chunk");
            }

            match chunk.unwrap() {
                RiffWaveChunks::Format(fmt) => {
                    let format = fmt.parse(&mut source)?;

                    // The Format chunk contains the block_align field and possible additional information
                    // to handle packetization and seeking.
                    packet_info = format.packet_info()?;
                    codec_params
                        .with_max_frames_per_packet(packet_info.get_max_frames_per_packet())
                        .with_frames_per_block(packet_info.frames_per_block);

                    // Append Format chunk fields to codec parameters.
                    append_format_params(&mut codec_params, format);
                }
                RiffWaveChunks::Fact(fct) => {
                    let fact = fct.parse(&mut source)?;

                    // Append Fact chunk fields to codec parameters.
                    append_fact_params(&mut codec_params, &fact);
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
                    let data_end_pos = data_start_pos + u64::from(data.len);

                    // Append Data chunk fields to codec parameters.
                    append_data_params(&mut codec_params, &data, &packet_info);

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

        // Chunks are processed until the Data chunk is found, or an error occurs.
    }

    fn next_packet(&mut self) -> Result<Packet> {
        let pos = self.reader.pos();
        if self.tracks.is_empty() {
            return decode_error("wav: no tracks");
        }
        if self.packet_info.is_empty() {
            return decode_error("wav: block size is 0");
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

fn read_info_chunk(source: &mut MediaSourceStream, len: u32) -> Result<MetadataRevision> {
    let mut info_list = ChunksReader::<RiffInfoListChunks>::new(len);

    let mut metadata_builder = MetadataBuilder::new();

    loop {
        let chunk = info_list.next(source)?;

        if let Some(RiffInfoListChunks::Info(info)) = chunk {
            let parsed_info = info.parse(source)?;
            metadata_builder.add_tag(parsed_info.tag);
        }
        else {
            break;
        }
    }

    info_list.finish(source)?;

    Ok(metadata_builder.metadata())
}

fn append_format_params(codec_params: &mut CodecParameters, format: WaveFormatChunk) {
    codec_params
        .with_sample_rate(format.sample_rate)
        .with_time_base(TimeBase::new(1, format.sample_rate));

    match format.format_data {
        WaveFormatData::Pcm(pcm) => {
            codec_params
                .for_codec(pcm.codec)
                .with_bits_per_coded_sample(u32::from(pcm.bits_per_sample))
                .with_bits_per_sample(u32::from(pcm.bits_per_sample))
                .with_channels(pcm.channels);
        }
        WaveFormatData::Adpcm(adpcm) => {
            codec_params.for_codec(adpcm.codec).with_channels(adpcm.channels);
        }
        WaveFormatData::IeeeFloat(ieee) => {
            codec_params.for_codec(ieee.codec).with_channels(ieee.channels);
        }
        WaveFormatData::Extensible(ext) => {
            codec_params
                .for_codec(ext.codec)
                .with_bits_per_coded_sample(u32::from(ext.bits_per_coded_sample))
                .with_bits_per_sample(u32::from(ext.bits_per_sample))
                .with_channels(ext.channels);
        }
        WaveFormatData::ALaw(alaw) => {
            codec_params.for_codec(alaw.codec).with_channels(alaw.channels);
        }
        WaveFormatData::MuLaw(mulaw) => {
            codec_params.for_codec(mulaw.codec).with_channels(mulaw.channels);
        }
    }
}

fn append_fact_params(codec_params: &mut CodecParameters, fact: &FactChunk) {
    codec_params.with_n_frames(u64::from(fact.n_frames));
}

fn append_data_params(
    codec_params: &mut CodecParameters,
    data: &DataChunk,
    packet_info: &PacketInfo,
) {
    if !packet_info.is_empty() {
        let n_frames = packet_info.get_frames(u64::from(data.len));
        codec_params.with_n_frames(n_frames);
    }
}
