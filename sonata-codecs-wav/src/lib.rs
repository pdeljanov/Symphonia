#![warn(rust_2018_idioms)]

use std::io;
use std::io::{Seek, SeekFrom};

use sonata_core::audio::{AudioBuffer, SignalSpec, Timestamp};
use sonata_core::codecs::{CODEC_TYPE_WAVE, CodecParameters, DecoderOptions};
use sonata_core::errors::{Result, Error, decode_error, seek_error, unsupported_error, SeekErrorKind};
use sonata_core::formats::{Packet, Stream, SeekIndex};
use sonata_core::io::*;

pub use sonata_core::formats::{ProbeDepth, ProbeResult, Format, FormatReader, SeekSearchResult};
pub use sonata_core::codecs::Decoder;


/// The Wav (RIFF) start of stream marker: "RIFF" in ASCII.
const RIFF_STREAM_MARKER: [u8; 4] = [0x52, 0x49, 0x46, 0x46];

// RIFF chunk, id parameter for WAVE
const RIFF_ID_WAVE: u32 = 0x57415645;

/// The recommended maximum number of bytes advance a stream to find the stream marker before giving up.
const WAVE_PROBE_SEARCH_LIMIT: usize = 512 * 1024;


enum Chunk {
    Format,
    List,
    Fact,
    Data,
}

enum WaveFormatExtra {
    Pcm(WaveFormatPcm),
    Extensible(WaveFormatExtensible),
}

struct WaveFormatPcm {
    bits_per_sample: u16,
}

struct WaveFormatExtensible {
    bits_per_sample: u16,
    bits_per_coded_sample: u16,
    channel_mask: u32,
    sub_format_guid: [u8; 16],
}

struct WaveFormat {
    format: u16,
    n_channels: u16,
    sample_rate: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    extra_data: WaveFormatExtra,
}

const WAVE_FORMAT_PCM: u16        = 0x0001;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
const WAVE_FORMAT_ALAW: u16       = 0x0006;
const WAVE_FORMAT_MULAW: u16      = 0x0007;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xfffe;

impl WaveFormat {
    fn read<B: Bytestream>(reader: &mut B) -> Result<WaveFormat> {
        let format = reader.read_u16()?;
        let n_channels = reader.read_u16()?;
        let sample_rate = reader.read_u32()?;
        let avg_bytes_per_sec = reader.read_u32()?;
        let block_align = reader.read_u16()?;
        let bits_per_sample = reader.read_u16()?;
        let extra_size = reader.read_u16()?;

        let extra_data = match format {
            // The PCM Wave Format
            WAVE_FORMAT_PCM => {
                // Bits per sample for PCM is both the decoded width, and actual sample width. This must either be 8 
                // or 16 bits. Higher widths must use the extensible format.
                if bits_per_sample != 8 || bits_per_sample != 16 {
                    return decode_error("Bits per sample for PCM Wave Format must either be 8 or 16 bits.");
                }

                if extra_size > 0 {
                    return decode_error("Extra data size must be 0 for PCM Wave Format.");
                }

                WaveFormatExtra::Pcm(WaveFormatPcm { bits_per_sample })
            },
            // The Extensible Wave Format
            WAVE_FORMAT_EXTENSIBLE => {
                // Bits per sample for extensible formats is the decoded "container" width per sample. This must be 
                // a multiple of 8.
                if bits_per_sample % 8 > 0 {
                    return decode_error("Bits per sample for extensible Wave Format must be a multiple of 8 bits.");
                }
                
                // The declared extra size must be 22 bytes for the extensible format.
                if extra_size != 22 {
                    return decode_error("Extra data size not 22 bytes for extensible Wave Format.");
                }

                let bits_per_coded_sample = reader.read_u16()?;
                let channel_mask = reader.read_u32()?;
                let mut sub_format_guid = [0u8; 16];

                reader.read_buf_bytes(&mut sub_format_guid)?;

                WaveFormatExtra::Extensible(WaveFormatExtensible { 
                    bits_per_sample, bits_per_coded_sample, channel_mask, sub_format_guid })
            },
            _ => return decode_error("Unsupported Wave Format."),
        };

        Ok(WaveFormat { format, n_channels, sample_rate, avg_bytes_per_sec, block_align, extra_data })
    }

}

/// `Wav` (Wave) is the Free Lossless Audio Codec.
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
    index: Option<SeekIndex>,
}

impl WavReader {

    pub fn open(source: MediaSourceStream) -> Self {
        WavReader {
            reader: source,
            streams: Vec::new(),
            index: None,
        }
    }

    fn read_chunk(&mut self) -> Result<()>{
        // First four bytes in a RIFF chunk is the type id.
        let chunk_id = self.reader.read_quad_bytes()?;
        // Next, an unsigned 32-bit length field of the data to follow.
        let chunk_size = self.reader.read_u32()?;

        match &chunk_id {
            b"fmt " => (),
            b"list" => (),
            b"fact" => (),
            b"data" => (),
            _ => {
                // As per the RIFF spec, unknown chunks are to be ignored.
                eprintln!("Unknown chunks of type={}, size={}. Ignoring...", 
                    String::from_utf8_lossy(&chunk_id), chunk_size);
            
                self.reader.ignore_bytes(chunk_size as u64)?;
            }
        }

        Ok(())
    }

}

impl FormatReader for WavReader {

    fn next_packet(&mut self) -> Result<Packet<'_, MediaSourceStream>> {
        // Return next RIFF chunk.
        unsupported_error("Packet streaming is unsupported")
    }

    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn seek(&mut self, ts: Timestamp) -> Result<u64> {
        unsupported_error("Seeking is unsupported")
    }

    fn probe(&mut self, depth: ProbeDepth) -> Result<ProbeResult> {
        let mut marker = [
            self.reader.read_u8()?,
            self.reader.read_u8()?,
            self.reader.read_u8()?,
            self.reader.read_u8()?,
        ];

        // Count the number of bytes read in the probe so that a limit may (optionally) be applied.
        let mut probed_bytes = 4usize;

        loop {
            if marker == RIFF_STREAM_MARKER {
                // Found the marker.
                eprintln!("Probe: Found RIFF header @ +{} bytes.", probed_bytes - 4);

                // A Wave file is one large RIFF chunk, with the actual meta and audio data as sub-chunks. Therefore, 
                // the header was the chunk ID, and the next 4 bytes is the length of the RIFF chunk.
                let riff_size = self.reader.read_u32()?;
                let id = self.reader.read_u32()?;

                // The RIFF chunk contains WAVE data.
                if id == RIFF_ID_WAVE {

                    // Read chunks until the audio data is found.
                    loop {
                        let chunk = self.read_chunk()?;

                    }
                    
                }

                return Ok(ProbeResult::Unsupported);
            }
            // If the ProbeDepth is deep, continue searching for the stream marker.
            else if depth == ProbeDepth::Deep {
                // Do not search more than the designated search limit.
                if probed_bytes <= WAVE_PROBE_SEARCH_LIMIT {

                    if probed_bytes % 4096 == 0 {
                        eprintln!("Probe: Searching for stream marker... ({} / {}) bytes.", 
                            probed_bytes, WAVE_PROBE_SEARCH_LIMIT);
                    }

                    marker[0] = marker[1];
                    marker[1] = marker[2];
                    marker[2] = marker[3];
                    marker[3] = self.reader.read_u8()?;

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
        Ok(ProbeResult::Unsupported)
    }

}



/// `WavDecoder` implements a decoder for the Wav codec bitstream. The decoder is compatible with OGG encapsulated 
/// Wav.
pub struct WavDecoder {
    params: CodecParameters,
}

impl Decoder for WavDecoder {

    fn new(params: &CodecParameters, options: &DecoderOptions) -> Self {
        WavDecoder {
            params: params.clone(),
        }
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn spec(&self) -> Option<SignalSpec> {
        None
    }

    fn decode<B: Bytestream>(&mut self, packet: &mut Packet<'_, B>, buf: &mut AudioBuffer<i32>) -> Result<()> {
        unsupported_error("Decoding is unsupported.")
    }
}


#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}