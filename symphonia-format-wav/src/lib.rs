// Symphonia
// Copyright (c) 2019 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

use std::io::{Seek, SeekFrom};

use symphonia_core::support_format;
use symphonia_core::codecs::CodecParameters;
use symphonia_core::errors::{Result, seek_error, unsupported_error, SeekErrorKind};
use symphonia_core::formats::prelude::*;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataBuilder, MetadataQueue};
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};

use log::error;

mod chunks;

use chunks::*;

/// WAVE is actually a RIFF stream, with a "RIFF" ASCII stream marker.
const WAVE_STREAM_MARKER: [u8; 4] = *b"RIFF";

/// The RIFF form is "wave".
const WAVE_RIFF_FORM: [u8; 4] = *b"WAVE";

/// The maximum number of frames that will be in a packet.
const WAVE_MAX_FRAMES_PER_PACKET: u64 = 1152;

/// `Wav` (Wave) Format.
///
/// `WavReader` implements a demuxer for the Wave format container.
pub struct WavReader {
    reader: MediaSourceStream,
    streams: Vec<Stream>,
    cues: Vec<Cue>,
    metadata: MetadataQueue,
    frame_len: u16,
    data_start_pos: u64,
}

impl QueryDescriptor for WavReader {
    fn query() -> &'static [Descriptor] {
        &[
            // WAVE RIFF form
            support_format!(
                "wave",
                "Waveform Audio File Format",
                &[ "wav", "wave" ],
                &[ "audio/vnd.wave", "audio/x-wav", "audio/wav", "audio/wave" ],
                &[ b"RIFF" ]
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
            return unsupported_error("missing riff stream marker");
        }

        // A Wave file is one large RIFF chunk, with the actual meta and audio data as sub-chunks.
        // Therefore, the header was the chunk ID, and the next 4 bytes is the length of the RIFF
        // chunk.
        let riff_len = source.read_u32()?;
        let riff_form = source.read_quad_bytes()?;

        // The RIFF chunk contains WAVE data.
        if riff_form != WAVE_RIFF_FORM {
            error!("riff form is not wave ({})", std::str::from_utf8(&riff_form).unwrap());

            return unsupported_error("riff form is not wave");
        }

        let mut riff_chunks = ChunksReader::<RiffWaveChunks>::new(riff_len);

        let mut codec_params = CodecParameters::new();
        let mut metadata: MetadataQueue = Default::default();
        let mut frame_len = 0;

        loop {
            let chunk = riff_chunks.next(&mut source)?;

            // The last chunk should always be a data chunk, if it is not, then the stream is
            // unsupported.
            if chunk.is_none() {
                return unsupported_error("missing data chunk");
            }

            match chunk.unwrap() {
                RiffWaveChunks::Format(fmt) => {
                    let format = fmt.parse(&mut source)?;

                    // The Format chunk contains the block_align field which indicates the size
                    // of one full audio frame in bytes, atleast for the codecs supported by
                    // WavReader. This value is stored to support seeking.
                    frame_len = format.block_align;

                    // Append Format chunk fields to codec parameters.
                    append_format_params(&mut codec_params, &format);
                },
                RiffWaveChunks::Fact(fct) => {
                    let fact = fct.parse(&mut source)?;

                    // Append Fact chunk fields to codec parameters.
                    append_fact_params(&mut codec_params, &fact);
                },
                RiffWaveChunks::List(lst) => {
                    let list = lst.parse(&mut source)?;

                    // Riff Lists can have many different forms, but WavReader only supports Info
                    // lists.
                    match &list.form {
                        b"INFO" => metadata.push(read_info_chunk(&mut source, list.len)?),
                        _       => list.skip(&mut source)?,
                    }
                },
                RiffWaveChunks::Data(dat) => {
                    let data = dat.parse(&mut source)?;

                    // Record the offset of the Data chunk's contents to support seeking.
                    let data_start_pos = source.pos();

                    // Append Data chunk fields to codec parameters.
                    append_data_params(&mut codec_params, &data, frame_len);

                    // Add a new stream using the collected codec parameters.
                    return Ok(WavReader {
                        reader: source,
                        streams: vec![ Stream::new(0, codec_params) ],
                        cues: Vec::new(),
                        metadata,
                        frame_len,
                        data_start_pos,
                    });
                }
            }
        }

        // Chunks are processed until the Data chunk is found, or an error occurs.
    }

    fn next_packet(&mut self) -> Result<Packet> {
        let packet_buf = self.reader.read_boxed_slice(WAVE_MAX_FRAMES_PER_PACKET as usize)?;

        Ok(Packet::new_from_boxed_slice(0, 0, 0, packet_buf))
    }

    fn metadata(&self) -> &MetadataQueue {
        &self.metadata
    }

    fn cues(&self) -> &[Cue] {
        &self.cues
    }

    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn seek(&mut self, to: SeekTo) -> Result<SeekedTo> {

        if self.streams.is_empty() || self.frame_len == 0 {
            return seek_error(SeekErrorKind::Unseekable);
        }

        let params = &self.streams[0].codec_params;

        let ts = match to {
            // Frame timestamp given.
            SeekTo::TimeStamp { ts, .. } => ts,
            // Time value given, calculate frame timestamp from sample rate.
            SeekTo::Time { time } => {
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

        // If the total number of frames in the stream is known, verify the desired frame timestamp
        // does not exceed it.
        if let Some(n_frames) = params.n_frames {
            if ts > n_frames {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        }

        // Calculate the absolute byte offset of the desired audio frame.
        let seek_pos = self.data_start_pos + (ts * u64::from(self.frame_len));

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
                return seek_error(SeekErrorKind::ForwardOnly)
            }
        }

        Ok(to)
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.reader
    }

}

fn read_info_chunk(source: &mut MediaSourceStream, len: u32) -> Result<Metadata> {
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

fn append_format_params(codec_params: &mut CodecParameters, format: &WaveFormatChunk) {

    codec_params
        .with_max_frames_per_packet(WAVE_MAX_FRAMES_PER_PACKET)
        .with_sample_rate(format.sample_rate);

    match format.format_data {
        WaveFormatData::Pcm(ref pcm) => {
            codec_params
                .for_codec(pcm.codec)
                .with_bits_per_coded_sample(u32::from(pcm.bits_per_sample))
                .with_bits_per_sample(u32::from(pcm.bits_per_sample))
                .with_channels(pcm.channels);
        },
        WaveFormatData::IeeeFloat(ref ieee) => {
            codec_params
                .for_codec(ieee.codec)
                .with_channels(ieee.channels);
        },
        WaveFormatData::Extensible(ref ext) => {
            codec_params
                .for_codec(ext.codec)
                .with_bits_per_coded_sample(u32::from(ext.bits_per_coded_sample))
                .with_bits_per_sample(u32::from(ext.bits_per_sample))
                .with_channels(ext.channels);
        },
        WaveFormatData::ALaw(ref alaw) => {
            codec_params
                .for_codec(alaw.codec)
                .with_channels(alaw.channels);
        },
        WaveFormatData::MuLaw(ref mulaw) => {
            codec_params
                .for_codec(mulaw.codec)
                .with_channels(mulaw.channels);
        }
    }
}

fn append_fact_params(codec_params: &mut CodecParameters, fact: &FactChunk) {
    codec_params.with_n_frames(u64::from(fact.n_frames));
}

fn append_data_params(codec_params: &mut CodecParameters, data: &DataChunk, frame_len: u16) {
    if frame_len > 0 {
        let n_frames = data.len / u32::from(frame_len);
        codec_params.with_n_frames(u64::from(n_frames));
    }
}