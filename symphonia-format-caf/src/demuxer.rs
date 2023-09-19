use symphonia_core::{
    errors::Result,
    formats::{Cue, FormatOptions, FormatReader, Packet, SeekMode, SeekTo, SeekedTo, Track},
    io::MediaSourceStream,
    meta::Metadata,
    probe::{Descriptor, Instantiate, QueryDescriptor},
    support_format,
};

/// Core Audio Format (CAF) format reader.
///
/// `CafReader` implements a demuxer for Core Audio Format containers.
pub struct CafReader {
    reader: MediaSourceStream,
}

impl QueryDescriptor for CafReader {
    fn query() -> &'static [Descriptor] {
        &[support_format!("caf", "Core Audio Format", &["caf"], &["audio/x-caf"], &[b"caff"])]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}

impl FormatReader for CafReader {
    fn try_new(_source: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {
        unimplemented!();
    }

    fn next_packet(&mut self) -> Result<Packet> {
        unimplemented!();
    }

    fn metadata(&mut self) -> Metadata<'_> {
        unimplemented!();
    }

    fn cues(&self) -> &[Cue] {
        unimplemented!();
    }

    fn tracks(&self) -> &[Track] {
        unimplemented!();
    }

    fn seek(&mut self, _mode: SeekMode, _to: SeekTo) -> Result<SeekedTo> {
        unimplemented!();
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        unimplemented!();
    }
}
