#![warn(rust_2018_idioms)]

use sonata_core::audio::{Duration, AudioBuffer, SignalSpec};
use sonata_core::codecs::{CODEC_TYPE_FLAC, CodecParameters};
use sonata_core::errors::Result;
use sonata_core::formats::{Stream, Packet};
use sonata_core::io::*;

mod metadata;
mod framing;
mod validate;

use metadata::{MetadataBlockType, MetadataBlockHeader};
use metadata::{StreamInfo, VorbisComment, SeekTable, Cuesheet, Application, Picture};

use framing::FrameStream;

/// The FLAC start of stream marker: "fLaC" in ASCII.
const FLAC_STREAM_MARKER: [u8; 4] = [0x66, 0x4c, 0x61, 0x43];

/// The recommended maximum number of bytes advance a stream to find the stream marker before giving up.
const FLAC_PROBE_SEARCH_LIMIT: usize = 512 * 1024;

pub use sonata_core::formats::{ProbeDepth, ProbeResult, Format, FormatReader};
pub use sonata_core::codecs::Decoder;

/// `Flac` (FLAC) is the Free Lossless Audio Codec.
/// 
/// This format only supports reading.
pub struct Flac;

impl Format for Flac {
    type Reader = FlacReader;

    fn open<S: 'static + MediaSource>(source: Box<S>) -> Self::Reader {
        let mss = MediaSourceStream::new(source);
        FlacReader::open(mss)
    }
}

/// `FlacReader` implements a demultiplexer for the native FLAC format container.
pub struct FlacReader {
    reader: MediaSourceStream,
    streams: Vec<Stream>,
}

impl FlacReader {

    pub fn open(source: MediaSourceStream) -> Self {
        FlacReader {
            reader: source,
            streams: Vec::new(),
        }
    }

}

impl FormatReader for FlacReader {

    fn next_packet(&mut self) -> Result<Packet<'_, MediaSourceStream>> {
        // FLAC is not a "real" container format. FLAC frames are more-so part of the codec bitstream than the actual 
        // format. In fact, it is not possible to know how long a FLAC frame is without decoding its header and 
        // practically decoding it. This is all to say that the what follows the metadata blocks is a codec bitstream.
        // Therefore, next_packet will simply always return the reader and let the codec advance the stream.
        Ok(Packet::new(0, &mut self.reader))
    }

    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn probe(&mut self, depth: ProbeDepth) -> Result<ProbeResult> {

        // Read the first 4 bytes of the stream. Ideally this will be the FLAC stream marker. If not, use this as a 
        // window to scroll byte-after-byte searching for the stream marker if the ProbeDepth is Deep.
        let mut marker = [
            self.reader.read_u8()?,
            self.reader.read_u8()?,
            self.reader.read_u8()?,
            self.reader.read_u8()?,
        ];

        // Count the number of bytes read in the probe so that a limit may (optionally) be applied.
        let mut probed_bytes = 4usize;

        loop {
            if marker == FLAC_STREAM_MARKER {
                // Found the header. This is enough for a Superficial probe, but not enough for a Default probe.
                eprintln!("Probe: Found FLAC header @ +{} bytes.", probed_bytes - 4);

                // Strictly speaking, the first metadata block must be a StreamInfo block. There is no technical need 
                // for this from the reader's point of view. Additionally, if the reader is fed a stream mid-way there
                // is no StreamInfo block. Therefore, don't enforce this too strictly.
                let header = MetadataBlockHeader::read(&mut self.reader)?;

                match header.block_type {
                    MetadataBlockType::StreamInfo => {

                        let info = StreamInfo::read(&mut self.reader)?;

                        let mut codec_params = CodecParameters::new(CODEC_TYPE_FLAC);

                        // Populate the codec parameters with the information read from StreamInfo.
                        codec_params
                            .with_sample_rate(info.sample_rate)
                            .with_bits_per_sample(info.bits_per_sample)
                            .with_max_frames_per_packet(info.block_size_bounds.1 as u64)
                            .with_channels(&info.channels);
                        
                        // Total samples (per channel) may or may not be stated in StreamInfo.
                        if let Some(samples) = info.n_samples {
                            codec_params.with_length(&Duration::Frames(samples));
                        }

                        // Add the stream.
                        self.streams.push(Stream::new(codec_params));
                    },
                    _ => {
                        eprintln!("Probe: First block is not StreamInfo.");
                        break;
                    }
                }

                // If there are more metablocks, read and process them.
                if !header.is_last {
                    read_all_metadata_blocks(&mut self.reader)?;
                }

                // Read the rest of the metadata blocks.
                return Ok(ProbeResult::Supported);
            }
            // If the ProbeDepth is deep, continue searching for the stream marker.
            else if depth == ProbeDepth::Deep {
                // Do not search more than the designated search limit.
                // TODO: Replace with programmable limit.
                if probed_bytes <= FLAC_PROBE_SEARCH_LIMIT {

                    if probed_bytes % 4096 == 0 {
                        eprintln!("Probe: Searching for stream marker... ({} / {}) bytes.", 
                            probed_bytes, FLAC_PROBE_SEARCH_LIMIT);
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

fn read_all_metadata_blocks<B: Bytestream>(reader: &mut B) -> Result<()> {
    loop {
        let header = MetadataBlockHeader::read(reader)?;

        match header.block_type {
            MetadataBlockType::Application => {
                eprintln!("{}", Application::read(reader, header.block_length)?);
            },
            MetadataBlockType::SeekTable => {
                eprintln!("{}", SeekTable::read(reader, header.block_length)?);
            },
            MetadataBlockType::VorbisComment => {
                eprintln!("{}", VorbisComment::read(reader, header.block_length)?);
            },
            MetadataBlockType::Cuesheet => {
                eprintln!("{}", Cuesheet::read(reader, header.block_length)?);
            },
            MetadataBlockType::Picture => {
                eprintln!("{}", Picture::read(reader, header.block_length)?);
            },
            //MetadataBlockType::StreamInfo => SUPER ILLEGAL,
            _ => {
                reader.ignore_bytes(header.block_length)?;
                eprintln!("Ignoring {} bytes of {:?} block.", header.block_length, header.block_type);
            }
        }

        // Exit when the last header is processed.
        if header.is_last {
            break;
        }
    }

    Ok(())
}

/// `FlacDecoder` implements a decoder for the FLAC codec bitstream. The decoder is compatible with OGG encapsulated 
/// FLAC.
pub struct FlacDecoder {
    params: CodecParameters,
    fs: FrameStream,
}

impl Decoder for FlacDecoder {

    fn new(params: &CodecParameters) -> Self {
        FlacDecoder {
            params: params.clone(),
            fs: FrameStream::new(params.bits_per_sample, params.sample_rate),
        }
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    fn spec(&self) -> Option<SignalSpec> {
        if let Some(rate) = self.params.sample_rate {
            // Prefer the channel layout over a list of channels.
            if let Some(layout) = self.params.channel_layout {
                return Some(SignalSpec::new_with_layout(rate, layout));
            }
            else if let Some(ref channels) = self.params.channels {
                return Some(SignalSpec::new(rate, &channels));
            }
        }
        None
    }

    fn decode<B: Bytestream>(&mut self, packet: &mut Packet<'_, B>, buf: &mut AudioBuffer<i32>) -> Result<()> {
        self.fs.next(packet.reader(), buf)
    }
}