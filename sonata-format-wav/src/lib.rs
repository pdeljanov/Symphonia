#![warn(rust_2018_idioms)]
use std::io;

use sonata_core::audio::Timestamp;
use sonata_core::codecs::CodecParameters;
use sonata_core::errors::{Result, seek_error, unsupported_error, SeekErrorKind};
use sonata_core::formats::{Packet, Stream};
use sonata_core::io::*;

mod chunks;

use chunks::*;

pub use sonata_core::formats::{ProbeDepth, ProbeResult, Format, FormatReader, SeekSearchResult};
pub use sonata_core::codecs::Decoder;

/// The recommended maximum number of bytes advance a stream to find the stream marker before giving up.
const WAVE_PROBE_SEARCH_LIMIT: usize = 512 * 1024;

const WAVE_MAX_FRAMES_PER_PACKET: u64 = 4096;

/// `Wav` (Wave) Format.
/// 
/// This format only supports reading.
pub struct Wav;

impl Format for Wav {
    type Reader = WavReader;

    fn open<S: 'static + MediaSource>(source: Box<S>) -> Self::Reader {
        let mss = MediaSourceStream::new(source);
        WavReader::open(mss)
    }
}

/// `WavReader` implements a demultiplexer for the native Wav format container.
pub struct WavReader {
    reader: MediaSourceStream,
    streams: Vec<Stream>,
}

impl WavReader {

    pub fn open(source: MediaSourceStream) -> Self {
        WavReader {
            reader: source,
            streams: Vec::new(),
        }
    }

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
                    eprintln!("{}", info.tag); 
                }
            }
        }
        
        info_list.finish(&mut self.reader)?;

        Ok(())
    }

}

impl FormatReader for WavReader {

    fn next_packet(&mut self) -> Result<Packet<'_>> {
        Ok(Packet::new_direct(0, &mut self.reader))
    }

    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn seek(&mut self, ts: Timestamp) -> Result<u64> {
        unsupported_error("Seeking is unsupported")
    }

    fn probe(&mut self, depth: ProbeDepth) -> Result<ProbeResult> {

        // Search for the "RIFF" marker.
        let marker = search_for_marker(&mut self.reader, b"RIFF", depth)?;

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
                        eprintln!("{}", format);

                        // Append Format chunk fields to codec parameters.
                        append_format_params(&mut codec_params, &format);
                    },
                    RiffWaveChunks::Fact(fct) => {
                        let fact = fct.parse(&mut self.reader)?;
                        eprintln!("{}", fact);

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
                        // Add new stream using the collected codec parameters.
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
                .with_bits_per_coded_sample(pcm.bits_per_sample as u32)
                .with_bits_per_sample(pcm.bits_per_sample as u32)
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
                .with_bits_per_coded_sample(ext.bits_per_coded_sample as u32)
                .with_bits_per_sample(ext.bits_per_sample as u32)
                .with_channels(ext.channels);
        }
    }
}

fn append_fact_params(codec_params: &mut CodecParameters, fact: &FactChunk) {
    codec_params.with_n_frames(fact.n_frames as u64);
}

fn search_for_marker<B: Bytestream>(reader: &mut B, marker: &[u8; 4], depth: ProbeDepth) -> Result<Option<[u8; 4]>> {
    let mut window = [0u8; 4];

    reader.read_buf_bytes(&mut window)?;

    // Count the number of bytes read in the probe so that a limit may (optionally) be applied.
    let mut probed_bytes = 4usize;

    loop {
        if window == *marker {
            // Found the marker.
            eprintln!("Probe: Found stream marker @ +{} bytes.", probed_bytes - 4);
            return Ok(Some(*marker));
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



#[cfg(test)]
mod tests {
    use std::fs::File;
    use super::{Format, FormatReader, Wav, ProbeDepth};

    #[test]
    fn it_works() {
        let file = Box::new(File::open("samples/wav/metadata_pcm32le.wav").unwrap());
        let mut reader = Wav::open(file);
        let probe_info = reader.probe(ProbeDepth::Deep).unwrap();
    }
}