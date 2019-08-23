// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]

use std::io::{Seek, SeekFrom};

use sonata_core::support_format;

use sonata_core::audio::Timestamp;
use sonata_core::codecs::CodecParameters;
use sonata_core::errors::{Result, seek_error, SeekErrorKind};
use sonata_core::formats::{FormatDescriptor, FormatOptions, FormatReader, Packet};
use sonata_core::formats::{Cue, ProbeDepth, ProbeResult, Stream, Visual};
use sonata_core::tags::Tag;
use sonata_core::io::*;

mod chunks;

use chunks::*;

/// The recommended maximum number of bytes advance a stream to find the stream marker before giving up.
const WAVE_PROBE_SEARCH_LIMIT: usize = 512 * 1024;

const WAVE_MAX_FRAMES_PER_PACKET: u64 = 4096;

/// `Wav` (Wave) Format.
/// 
/// `WavReader` implements a demuxer for the Wave format container.
pub struct WavReader {
    reader: MediaSourceStream,
    streams: Vec<Stream>,
    tags: Vec<Tag>,
    visuals: Vec<Visual>,
    cues: Vec<Cue>,
    frame_len: u16,
    data_offset: u64,
}

impl WavReader {

    fn read_metadata(&mut self, len: u32) -> Result<()> {
        let mut info_list = ChunksReader::<RiffInfoListChunks>::new(len);

        loop {
            let chunk = info_list.next(&mut self.reader)?;

            if chunk.is_none() {
                break;
            }

            match chunk.unwrap() {
                RiffInfoListChunks::Info(nfo) => { 
                    let info = nfo.parse(&mut self.reader)?;
                    self.tags.push(info.tag); 
                }
            }
        }
        
        info_list.finish(&mut self.reader)?;

        Ok(())
    }

}

impl FormatReader for WavReader {

    fn open(source: MediaSourceStream, _options: &FormatOptions) -> Self {
        WavReader {
            reader: source,
            streams: Vec::new(),
            tags: Vec::new(),
            visuals: Vec::new(),
            cues: Vec::new(),
            frame_len: 0,
            data_offset: 0,
        }
    }

    fn supported_formats() -> &'static [FormatDescriptor] {
        &[
            support_format!(
                &["wav", "wave"], 
                &["audio/vnd.wave", "audio/x-wav", "audio/wav", "audio/wave"], 
                b"RIFF    ", 4, 0)
        ]
    }

    fn next_packet(&mut self) -> Result<Packet<'_>> {
        Ok(Packet::new_direct(0, &mut self.reader))
    }

    fn tags(&self) -> &[Tag] {
        &self.tags
    }

    fn visuals(&self) -> &[Visual] {
        &self.visuals
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
                // Use the sample rate to calculate the frame timestamp. If sample rate is not known, the seek cannot 
                // be completed.
                if let Some(sample_rate) = params.sample_rate {
                    (time * f64::from(sample_rate)) as u64
                }
                else {
                    return seek_error(SeekErrorKind::Unseekable);
                }
            }
        };

        // If the total number of frames in the stream is known, verify the desired frame timestamp does not exceed it.
        if let Some(n_frames) = params.n_frames {
            if frame_ts > n_frames {
                return seek_error(SeekErrorKind::OutOfRange);
            }
        }

        // Calculate the absolute byte offset of the desired audio frame.
        let seek_pos = self.data_offset + (frame_ts * u64::from(self.frame_len));

        // If the reader supports seeking we can seek directly to the frame's offset wherever it may be.
        if self.reader.is_seekable() {
            self.reader.seek(SeekFrom::Start(seek_pos))?;
        }
        // If the reader does not support seeking, we can only emulate forward seeks by consuming bytes. If the reader
        // has to seek backwards, return an error.
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

    fn probe(&mut self, depth: ProbeDepth) -> Result<ProbeResult> {

        // Search for the "RIFF" marker.
        let marker = search_for_marker(&mut self.reader, *b"RIFF", depth)?;

        if marker.is_none() {
            return Ok(ProbeResult::Unsupported);
        }

        // A Wave file is one large RIFF chunk, with the actual meta and audio data as sub-chunks. Therefore, 
        // the header was the chunk ID, and the next 4 bytes is the length of the RIFF chunk.
        let riff_len = self.reader.read_u32()?;
        let riff_form = self.reader.read_quad_bytes()?;

        // The RIFF chunk contains WAVE data.
        if riff_form != *b"wave" {

            let mut riff_chunks = ChunksReader::<RiffWaveChunks>::new(riff_len);
            
            let mut codec_params = CodecParameters::new();

            loop {
                let chunk = riff_chunks.next(&mut self.reader)?;

                // The last chunk should always be a data chunk. Probe will exit with a supported result in that case.
                // Therefore, if there is no more chunks left, then the file is unsupported. Exit.
                if chunk.is_none() {
                    break;
                }

                match chunk.unwrap() {
                    RiffWaveChunks::Format(fmt) => {
                        let format = fmt.parse(&mut self.reader)?;

                        // The Format chunk contains the block_align field which indicates the size of one full audio 
                        // frame in bytes, atleast for the codecs supported by WavReader. This value is stored to
                        // support seeking.
                        self.frame_len = format.block_align;

                        // Append Format chunk fields to codec parameters.
                        append_format_params(&mut codec_params, &format);
                    },
                    RiffWaveChunks::Fact(fct) => {
                        let fact = fct.parse(&mut self.reader)?;

                        // Append Fact chunk fields to codec parameters.
                        append_fact_params(&mut codec_params, &fact);
                    },
                    RiffWaveChunks::List(lst) => {
                        let list = lst.parse(&mut self.reader)?;

                        // Riff Lists can have many different forms, but WavReader only supports Info lists.
                        match &list.form {
                            b"INFO" => self.read_metadata(list.len)?,
                            _ => list.skip(&mut self.reader)?
                        }
                    },
                    RiffWaveChunks::Data => {
                        // Record the offset of the Data chunk's contents to support seeking.
                        self.data_offset = self.reader.pos();

                        // Add a new stream using the collected codec parameters.
                        self.streams.push(Stream::new(codec_params));

                        return Ok(ProbeResult::Supported);
                    }
                }
            }
        }

        // Not supported.
        Ok(ProbeResult::Unsupported)
    }
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

fn search_for_marker<B: Bytestream>(reader: &mut B, marker: [u8; 4], depth: ProbeDepth) -> Result<Option<[u8; 4]>> {
    let mut window = [0u8; 4];

    reader.read_buf_bytes(&mut window)?;

    // Count the number of bytes read in the probe so that a limit may (optionally) be applied.
    let mut probed_bytes = 4usize;

    loop {
        if window == marker {
            // Found the marker.
            eprintln!("Probe: Found stream marker @ +{} bytes.", probed_bytes - 4);
            return Ok(Some(marker));
        }
        // If the ProbeDepth is deep, continue searching for the stream marker.
        else if depth == ProbeDepth::Deep {
            // Do not search more than the designated search limit.
            if probed_bytes <= WAVE_PROBE_SEARCH_LIMIT {

                if probed_bytes % 4096 == 0 {
                    eprintln!("Probe: Searching for stream marker... ({} / {}) bytes.", 
                        probed_bytes, WAVE_PROBE_SEARCH_LIMIT);
                }

                window[0] = window[1];
                window[1] = window[2];
                window[2] = window[3];
                window[3] = reader.read_u8()?;

                probed_bytes += 1;
            }
            else {
                eprintln!("Probe: Stream marker search limit exceeded.");
                break;
            }
        }
        else {
            break;
        }
    }

    // Loop exited, therefore stream is unsupported.
    Ok(None)
}
