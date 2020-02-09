// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

use std::io::{Seek, SeekFrom};

use sonata_core::support_format;

use sonata_core::audio::Timestamp;
use sonata_core::codecs::CodecParameters;
use sonata_core::errors::{Result, seek_error, unsupported_error, SeekErrorKind};
use sonata_core::formats::{Cue, FormatOptions, FormatReader, Packet, Stream};
use sonata_core::io::*;
use sonata_core::meta::{Metadata, MetadataBuilder, MetadataQueue};
use sonata_core::probe::{Descriptor, Instantiate, QueryDescriptor};

mod chunks;

use chunks::*;

/// WAVE is actually a RIFF stream, with a "RIFF" ASCII stream marker.
const WAVE_STREAM_MARKER: [u8; 4] = *b"RIFF";

/// The RIFF form is "wave".
const WAVE_RIFF_FORM: [u8; 4] = *b"WAVE";

/// The maximum number of frames that will be in a packet.
const WAVE_MAX_FRAMES_PER_PACKET: u64 = 4096;

/// `Wav` (Wave) Format.
///
/// `WavReader` implements a demuxer for the Wave format container.
pub struct WavReader {
    reader: MediaSourceStream,
    streams: Vec<Stream>,
    cues: Vec<Cue>,
    metadata: MetadataQueue,
    frame_len: u16,
    data_offset: u64,
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

    fn score(_context: &[u8]) -> f32 {
        1.0
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
            eprintln!("wav: riff form is not wave ({})", std::str::from_utf8(&riff_form).unwrap());

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
                RiffWaveChunks::Data => {
                    // Record the offset of the Data chunk's contents to support seeking.
                    let data_offset = source.pos();

                    // Add a new stream using the collected codec parameters.
                    return Ok(WavReader {
                        reader: source,
                        streams: vec![ Stream::new(codec_params) ],
                        cues: Vec::new(),
                        metadata,
                        frame_len,
                        data_offset,
                    });
                }
            }
        }

        // Chunks are processed until the Data chunk is found, or an error occurs.
    }

    fn next_packet(&mut self) -> Result<Packet<'_>> {
        Ok(Packet::new_direct(0, &mut self.reader))
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

    fn seek(&mut self, ts: Timestamp) -> Result<u64> {

        if self.streams.is_empty() || self.frame_len == 0 {
            return seek_error(SeekErrorKind::Unseekable);
        }

        let params = &self.streams[0].codec_params;

        // Get the timestamp of the desired audio frame.
        let frame_ts = match ts {
            // Frame timestamp given.
            Timestamp::Frame(frame) => frame,
            // Time value given, calculate frame timestamp from sample rate.
            Timestamp::Time(time) => {
                // Ensure time value is positive.
                if time < 0.0 {
                    return seek_error(SeekErrorKind::OutOfRange);
                }
                // Use the sample rate to calculate the frame timestamp. If sample rate is not
                // known, the seek cannot be completed.
                if let Some(sample_rate) = params.sample_rate {
                    (time * f64::from(sample_rate)) as u64
                }
                else {
                    return seek_error(SeekErrorKind::Unseekable);
                }
            }
        };

        // If the total number of frames in the stream is known, verify the desired frame timestamp
        // does not exceed it.
        if let Some(n_frames) = params.n_frames {
            if frame_ts > n_frames {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        }

        // Calculate the absolute byte offset of the desired audio frame.
        let seek_pos = self.data_offset + (frame_ts * u64::from(self.frame_len));

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

        Ok(frame_ts)
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