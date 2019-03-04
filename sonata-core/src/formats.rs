// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This library is free software; you can redistribute it and/or
// modify it under the terms of the GNU Lesser General Public
// License as published by the Free Software Foundation; either
// version 2.1 of the License, or (at your option) any later version.
//
// This library is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
// Lesser General Public License for more details.
//
// You should have received a copy of the GNU Lesser General Public
// License along with this library; if not, write to the Free Software
// Foundation, Inc., 51 Franklin Street, Fifth Floor, Boston, MA  02110-1301 USA

use std::fmt;

use crate::io::{MediaSource, MediaSourceStream, Bytestream};
use crate::codecs::{CodecParameters};
use super::errors::Result;
use super::tags::Tag;

/// The verbosity of log messages produced by a decoder or demuxer.
pub enum Verbosity {
    /// No messages are logged.
    Silent,
    /// Only errors are logged.
    Error,
    /// Everything from the Error level, and warnings are logged.
    Warning,
    /// Everything from the Warning level, and info messages are logged.
    Info,
    /// Everything from the Info level, and debugging information is logged.
    Debug,
}

/// Limit defines how a `Format` or `Codec` should handle resource allocation when the amount of that resource to be
/// allocated is dictated by the untrusted stream. Limits are used to prevent denial-of-service attacks whereby the 
/// stream requests the `Format` or `Codec` to allocate large amounts of a resource, usually memory. A limit will place
/// an upper-bound on this allocation at the risk of breaking potentially valid streams.
///
/// All limits can be defaulted to a reasonable value specific to the situation. These defaults will generally not break
/// any normal stream.
pub enum Limit {
    /// Do not impose any limit.
    None,
    /// Use the (reasonable) default specified by the `Format` or `Codec`.
    Default,
    /// Specify the upper limit of the resource. Units are use-case specific.
    Maximum(u32),
}

/// `FormatOptions` is a common set of options that all demuxers use.
pub struct FormatOptions {
    /// Selects the logging verbosity of the demuxer and decoder.
    verbosity: Verbosity,

    /// The maximum size limit in bytes that a tag may occupy in memory once decoded. Tags exceeding this limit will be 
    /// skipped by the demuxer. Take note that tags in-memory are stored as UTF-8 and therefore may occupy more than one
    /// byte per character.
    limit_metadata_bytes: Limit,

    // The maximum size limit in bytes that a visual (picture) may occupy.
    limit_visual_bytes: Limit,
}

/// The `ProbeDepth` is how hard a `FormatReader` should try to determine if it can support a stream.
#[derive(PartialEq)]
pub enum ProbeDepth {
    /// Don't probe at all. This is useful if joining a stream midway. A `FormatReader` is not required to support this, 
    /// and it may be impossible for some media formats, if so an error may be immediately returned.
    NoProbe,
    /// Check if the header signature is correct. Event hooks will never fire.
    Superficial,
    /// Check if the header signature is correct and validate the stream playback information. Event hooks may fire if
    /// the reader encounters relevant metadata.
    Shallow,
    /// Search the stream for the header if it is not immediately available, and validate the stream playback 
    /// information. Event hooks may fire if the reader encounters relevant metadata.
    Deep
}

pub struct Visual {
    data: Vec<u8>,
}

impl Visual {
    fn as_bytes(&self) -> &[u8] {
        &self.data
    }
}

pub struct TableOfContents {

}

pub struct VendorData {

}




pub trait Format {
    type Reader;

    fn open<S: 'static + MediaSource>(src: Box<S>) -> Self::Reader;
}


/// The `EventHooks` traits provides an interface to (optionally) catch and react to supplementary
/// data present within a media stream. The most obvious use-case of this supplementary data is
/// metadata, the textual tags, describing the audio stream. If the data is not worthwhile for the
/// application at hand, it may simply be ignored.
///
/// Events may fire at any time during the decoding process, and will fire synchronously with the
/// decoding process.
pub trait Hooks {

    /// Commonly known as "tags," metadata is human readable textual information describing the
    /// audio. This function is called by the decoder when such information becomes available.
    fn on_metadata(&mut self, hook: Fn(&Tag));

    /// Application data is any data embedded into the audio stream that is to be processed by a
    /// third-party or vendor-specific extension or application. This data is ignored by the
    /// decoder.
    /// 
    fn on_visual(&mut self, hook: Fn(&Visual));

    /// A visual is any kind of graphic (picture, video, or text) that is embedded into the audio
    /// stream that should be displayed to the user. A visual may be loaded and presented
    /// immediately when a stream is loaded, or be presented at a designated time.
    fn on_table_of_contents(&mut self, hook: Fn(&TableOfContents));

    /// A table of contents may be embedded in an audio stream to allow the presentation of a
    /// single audio stream as many logical tracks.
    fn on_vendor_data(&mut self, hook: Fn(&VendorData));
    
}

#[derive(Copy,Clone,PartialEq,Debug)]
pub struct SeekPoint {
    pub frame_ts: u64,
    pub byte_offset: u64,
    pub n_frames: u32,
}

impl SeekPoint {
    fn new(frame_ts: u64, byte_offset: u64, n_frames: u32) -> Self {
        SeekPoint {
            frame_ts,
            byte_offset,
            n_frames,
        }
    }
}

impl fmt::Display for SeekPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{ frame_ts={}, n_frames={}, byte_offset={} }}", self.frame_ts, self.n_frames, self.byte_offset)
    }
}

/*
use std::cmp::Ordering;

impl Ord for SeekPoint {
    fn cmp(&self, other: &SeekPoint) -> Ordering {
        self.0.cmp(&other.0)
    }
}
*/

/// A `SeekIndex` stores seek points (generally a sample or frame number to byte offset) within an audio stream.
pub struct SeekIndex {
    points: Vec<SeekPoint>,
}

/// `SeekSearchResult` is the return value for a search on a `SeekIndex`. It returns a range of `SeekPoint`s a 
/// `FormatReader` should search to find the desired timestamp. Ranges are lower-bound inclusive, and upper-bound 
/// exclusive.
#[derive(PartialEq,Debug)]
pub enum SeekSearchResult {
    /// The `SeekIndex` is empty so the desired timestamp could not be found. The entire stream should be searched for 
    /// the desired timestamp.
    Stream,
    /// The desired timestamp can be found before, the `SeekPoint`. The stream should be searched for the desired 
    /// timestamp from the start of the stream up-to, but not including, the `SeekPoint`.
    Upper(SeekPoint),
    /// The desired timestamp can be found at, or after, the `SeekPoint`. The stream should be searched for the desired 
    /// timestamp starting at the provided `SeekPoint` up-to the end of the stream.
    Lower(SeekPoint),
    /// The desired timestamp can be found within the range. The stream should be searched for the desired starting at 
    /// the first `SeekPoint` up-to, but not-including, the second `SeekPoint`.
    Range(SeekPoint, SeekPoint)
}

impl SeekIndex {

    /// Create an empty `SeekIndex`
    pub fn new() -> SeekIndex {
        SeekIndex {
            points: Vec::new(),
        }
    }

    /// Insert a `SeekPoint` into the index.
    pub fn insert(&mut self, frame: u64, byte_offset: u64, n_frames: u32) {
        self.points.push(SeekPoint::new(frame, byte_offset, n_frames));
    }

    /// Search the index to find a bounded range of bytes, wherein the specified frame timestamp will be contained. If 
    /// the index is empty, this function simply returns a result indicating the entire stream should be searched 
    /// manually.
    pub fn search(&self, frame_ts: u64) -> SeekSearchResult {
        // The index must contain atleast one SeekPoint to return a useful result.
        if self.points.len() > 0 {
            let mut lower = 0;
            let mut upper = self.points.len() - 1;

            // If the desired timestamp is less than the first SeekPoint within the index, indicate that the stream 
            // should be searched from the beginning.
            if frame_ts < self.points[lower].frame_ts {
                return SeekSearchResult::Upper(self.points[lower]);
            }
            // If the desired timestamp is greater than or equal to the last SeekPoint within the index, indicate that 
            // the stream should be searched from the last SeekPoint.
            else if frame_ts >= self.points[upper].frame_ts {
                return SeekSearchResult::Lower(self.points[upper]); 
            }

            // Desired timestamp is between the lower and upper indicies. Perform a binary search to find a range of 
            // SeekPoints containing the desired timestamp. The binary search exits when either two adjacent SeekPoints
            // or a single SeekPoint is found.
            while upper - lower > 1 {
                let mid = (lower + upper) / 2;
                
                let lower_ts = self.points[lower].frame_ts;
                let mid_ts = self.points[mid].frame_ts;
                let upper_ts = self.points[upper].frame_ts;

                if frame_ts < mid_ts {
                    upper = mid;
                }
                else if frame_ts >= mid_ts {
                    lower = mid;
                }
            }

            return SeekSearchResult::Range(self.points[lower], self.points[upper]);
        }

        // The index is empty, the stream must be searched manually.
        SeekSearchResult::Stream
    }
}

#[test]
fn verify_seek_index_search() {
    let mut index = SeekIndex::new();
    index.insert(50 , 0,  45);
    index.insert(120, 0,   4);
    index.insert(320, 0, 100);
    index.insert(421, 0,  10);
    index.insert(500, 0,  12);
    index.insert(600, 0,  12);

    assert_eq!(index.search(25) , SeekSearchResult::Upper(SeekPoint::new(50 ,0, 45)));
    assert_eq!(index.search(700), SeekSearchResult::Lower(SeekPoint::new(600,0, 12)));
    assert_eq!(index.search(110), SeekSearchResult::Range(SeekPoint::new(50 ,0, 45), SeekPoint::new(120,0,4)));
    assert_eq!(index.search(340), SeekSearchResult::Range(SeekPoint::new(320,0,100), SeekPoint::new(421,0,10)));
    assert_eq!(index.search(320), SeekSearchResult::Range(SeekPoint::new(320,0,100), SeekPoint::new(421,0,10)));
}

impl fmt::Display for SeekIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "SeekIndex [")?;
        for point in &self.points {
            writeln!(f, "\t{},", point)?;
        }
        writeln!(f, "]")
    }
}

pub struct Stream {
    pub codec_params: CodecParameters,
    pub language: Option<String>,
}

impl Stream {
    pub fn new(codec_params: CodecParameters) -> Self {
        Stream {
            codec_params,
            language: None,
        }
    }



}

/// A `FormatReader` is a container demuxer. It provides methods to probe a media container for information and access
/// the streams encapsulated in the container.
///
/// Most, if not all, media containers contain some metadata, then a number of packetized and interleaved media streams. 
/// Generally, the encapsulated streams are individually encoded using some codec. The allowed codecs for a container 
/// are defined in the specification.
///
/// During demuxing, packets are read one-by-one and may be discarded or decoded at the choice of the caller. The 
/// definition of a packet is ambiguous, it may a frame of video, 1 millisecond or 1 second of audio, but a packet will 
/// never contain encoded data from two different media streams. Therefore the caller can be selective in what stream
/// should be decoded and played back.
///
/// `FormatReader` provides an iterator interface over packets for easy consumption and filterting. Iterators are valid 
/// until a seek.
pub trait FormatReader {

    /// Probes the container to check for support, contained streams, and other metadata. The complexity of the probe 
    /// can be set based on the caller's use-case.
    fn probe(&mut self, depth: ProbeDepth) -> Result<ProbeResult>;

    /// Seek, as closely as possible, to the timestamp requested. 
    /// 
    /// Note that many containers cannot seek to an exact timestamp, rather they can only seek to a coarse location and 
    /// then to the decoder must decode packets until the exact timestamp is reached. 
    fn seek(&mut self, frame_ts: u64) -> Result<u64>;

    /// Gets a list of streams in the container.
    fn streams(&self) -> &[Stream];

    /// Gets the default stream. If the media container has a method of determing the default stream, the function 
    /// should return it. Otherwise, the first stream is returned. If no streams are present, None is returned.
    fn default_stream(&self) -> Option<&Stream> {
        let streams = self.streams();
        match streams.len() {
            0 => None,
            _ => Some(&streams[0]),
        }
    }

    /// Lazily get the next packet from the container. 
    fn next_packet(&mut self) -> Result<Packet<'_, MediaSourceStream>>;

}

/// A `Packet` contains a discrete amount of encoded data for a single media stream. The exact amount of data is 
/// bounded, but not defined and is dependant on the container and how it was muxed.
///
/// Packets may be read by using the provided reader. 
pub struct Packet<'b, B: Bytestream> {
    idx: u32, 
    len: Option<usize>,
    reader: &'b mut B,
}

impl<'b, B: Bytestream> Packet<'b, B> {
    pub fn new_with_len(idx: u32, len: usize, reader: &'b mut B) -> Self {
        Packet { idx, len: Some(len), reader }
    }

    pub fn new(idx: u32, reader: &'b mut B) -> Self {
        Packet { idx, len: None, reader }
    }

    /// The stream index for the stream this packet belongs to.
    pub fn stream_idx(&self) -> u32 {
        self.idx
    }

    /// Read the contents of the packet as a bytestream.
    pub fn reader(&mut self) -> &mut B {
        self.reader
    }

    /// The length of the packet in bytes.
    pub fn len(&self) -> Option<usize> {
        self.len
    }
}


/*


    let dec0 = reader.streams()[0].make_decoder();
    let dec1 = reader.streams()[1].make_decoder();

    for packet in reader.iter_packets() {
        match packet.stream() {
            dec0.stream() => dec0.decode(packet);
            dec1.stream() => dec1.decode(packet);
            _ => ();
        }
    }




*/

pub enum ProbeResult {
    Unsupported,
    Supported
}

/// The `ProbeInfo` struct contains implementation specific format information from the result of a
/// probe operation. It is not directly useful to the end-user as it generally only contains
/// information required by the `FormatReader` to continue where the probe left off. This is
/// because a probe is simply the cheapest possible check to see if a byte stream asserts it is
/// a certain media format type and that the `FormatReader` could potentially read it. Probe does
/// not verify the validity of the stream, nor does it read all the metadata.
///
/// Additionally, the stream is owned by `ProbeInfo` to ensure the streaam is not modified between
/// a `probe()` and `make_reader()` call.
pub struct ProbeInfo {

}

/*
struct FormatRegistry {

}

impl FormatRegistry {

    pub fn register<F: Format>(&mut self, tier: u32) {
        F::supported_types();
    }

    /// Attempts to guess the appropriate demuxer and select it for use. If the guess is incorrect
    /// further attempts will be made with progressively more complexity.
    pub fn select() -> Box<Format> {

    }

    /// Guesses the demuxer to use from a media (MIME) type.
    pub fn guess_by_media_type(){}

    /// Guesses the demuxer to use from a file extension.
    pub fn guess_by_extension(){}

    /// Guess the demuxer to use through analysis of the file contents.
    pub fn guess_by_analysis(){}
}
*/
