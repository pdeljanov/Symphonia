// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::default::Default;
use std::fmt;
use std::io;
use std::num::NonZeroU32;

use crate::audio::Timestamp;
use crate::codecs::CodecParameters;
use crate::errors::Result;
use crate::io::{MediaSource, MediaSourceStream, Bytestream};
use crate::tags::{StandardVisualKey, Tag};

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

/// Limit defines how a `Format` or `Codec` should handle resource allocation when the amount of
/// that resource to be allocated is dictated by the untrusted stream. Limits are used to prevent
/// denial-of-service attacks whereby the stream requests the `Format` or `Codec` to allocate large
/// amounts of a resource, usually memory. A limit will place an upper-bound on this allocation at
/// the risk of breaking potentially valid streams.
///
/// All limits can be defaulted to a reasonable value specific to the situation. These defaults will
/// generally not break any normal stream.
pub enum Limit {
    /// Do not impose any limit.
    None,
    /// Use the (reasonable) default specified by the `Format` or `Codec`.
    Default,
    /// Specify the upper limit of the resource. Units are use-case specific.
    Maximum(usize),
}

impl Limit {
    /// Gets the numeric limit of the limit, or default value. If there is no limit, None is
    /// returned.
    pub fn limit_or_default(&self, default: usize) -> Option<usize> {
        match self {
            Limit::None => None,
            Limit::Default => Some(default),
            Limit::Maximum(max) => Some(*max),
        }
    }
}

/// `FormatOptions` is a common set of options that all demuxers use.
pub struct FormatOptions {
    /// Selects the logging verbosity of the demuxer.
    pub verbosity: Verbosity,

    /// The maximum size limit in bytes that a tag may occupy in memory once decoded. Tags exceeding
    /// this limit will be skipped by the demuxer. Take note that tags in-memory are stored as UTF-8
    /// and therefore may occupy more than one byte per character.
    pub limit_metadata_bytes: Limit,

    // The maximum size limit in bytes that a visual (picture) may occupy.
    pub limit_visual_bytes: Limit,
}

impl Default for FormatOptions {
    fn default() -> Self {
        FormatOptions {
            verbosity: Verbosity::Error,
            limit_metadata_bytes: Limit::Default,
            limit_visual_bytes: Limit::Default,
        }
    }
}

/// The `ProbeDepth` is how hard a `FormatReader` should try to determine if it can support a stream.
#[derive(PartialEq)]
pub enum ProbeDepth {
    /// Don't probe at all. This is useful if joining a stream midway. A `FormatReader` is not
    /// required to support this, and it may be impossible for some media formats, if so an error
    /// may be immediately returned.
    NoProbe,
    /// Check if the header signature is correct. Event hooks will never fire.
    Superficial,
    /// Check if the header signature is correct and validate the stream playback information. Event
    /// hooks may fire if the reader encounters relevant metadata.
    Shallow,
    /// Search the stream for the header if it is not immediately available, and validate the stream
    /// playback information. Event hooks may fire if the reader encounters relevant metadata.
    Deep
}

/// A 2D (width and height) size type.
#[derive(Copy, Clone)]
pub struct Size {
    /// The width.
    pub width: u32,
    /// The height.
    pub height: u32,
}

/// `ColorMode` indicates how the color of a pixel is encoded in a `Visual`.
pub enum ColorMode {
    /// Each pixel in the `Visual` stores color information.
    Discrete,
    /// Each pixel in the `Visual` stores an index into a color palette containing the color
    /// information. The value stored by this variant indicates the number of colors in the color
    /// palette.
    Indexed(NonZeroU32),
}

/// A `Visual` is any 2D graphic that is embedded within a media format.
pub struct Visual {
    /// The Media Type (formerly known as the MIME Type) used to encode the `Visual`.
    pub media_type: String,
    /// The dimensions of the `Visual`.
    ///
    /// Note: This value may not be accurate as it comes from metadata, not the `Visual` itself.
    pub dimensions: Option<Size>,
    /// The number of bits-per-pixel (aka bit-depth) of the unencoded image.
    ///
    /// Note: This value may not be accurate as it comes from metadata, not the `Visual` itself.
    pub bits_per_pixel: Option<NonZeroU32>,
    /// The color mode of the `Visual`.
    ///
    /// Note: This value may not be accurate as it comes from metadata, not the `Visual` itself.
    pub color_mode: Option<ColorMode>,
    /// The usage and/or content of the `Visual`.
    pub usage: Option<StandardVisualKey>,
    /// Any tags associated with the `Visual`.
    pub tags: Vec<Tag>,
    /// The data of the `Visual`, encoded with the `codec` specified above.
    pub data: Box<[u8]>,
}

/// A `Cue` is a designated point of time within a media stream.
///
/// A `Cue` may be a mapping from either a source track, a chapter, cuesheet, or a timestamp
/// depending on the source media. A `Cue`'s duration is the difference between the `Cue`'s
/// timestamp and the next. Each `Cue` may contain an optional index of points relative to the `Cue`
/// that never exceed the timestamp of the next `Cue`. A `Cue` may also have associated `Tag`s.
pub struct Cue {
    /// A unique index for the `Cue`.
    pub index: u32,
    /// The starting timestamp in number of frames from the start of the stream.
    pub start_ts: u64,
    /// A list of `Tag`s associated with the `Cue`.
    pub tags: Vec<Tag>,
    /// A list of `CuePoints`s that are contained within this `Cue`. These points are children of
    /// the `Cue` since the `Cue` itself is an implicit `CuePoint`.
    pub points: Vec<CuePoint>,
}

/// A `CuePoint` is a point, represented as a frame offset, within a `Cue`.
///
/// A `CuePoint` provides more precise indexing within a parent `Cue`. Additional `Tag`s may be
/// associated with a `CuePoint`.
pub struct CuePoint {
    /// The offset of the first frame in the `CuePoint` relative to the start of the parent `Cue`.
    pub start_offset_ts: u64,
    /// A list of `Tag`s associated with the `CuePoint`.
    pub tags: Vec<Tag>,
}

/// `VendorData` is application specific data embedded within the media format.
pub struct VendorData {
    /// A text representation of the vendor's application identifier.
    pub ident: String,
    /// The vendor data.
    pub data: Box<[u8]>,
}

/// A `SeekPoint` is a mapping between a sample or frame number to byte offset within a media
/// stream.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SeekPoint {
    /// The frame or sample timestamp of the `SeekPoint`.
    pub frame_ts: u64,
    /// The byte offset of the `SeekPoint`s timestamp relative to a format-specific location.
    pub byte_offset: u64,
    /// The number of frames the `SeekPoint` covers.
    pub n_frames: u32,
}

impl SeekPoint {
    fn new(frame_ts: u64, byte_offset: u64, n_frames: u32) -> Self {
        SeekPoint { frame_ts, byte_offset, n_frames }
    }
}

impl fmt::Display for SeekPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{{ frame_ts={}, n_frames={}, byte_offset={} }}",
            self.frame_ts,
            self.n_frames,
            self.byte_offset
        )
    }
}

/// A `SeekIndex` stores `SeekPoint`s (generally a sample or frame number to byte offset) within a
/// media stream and provides methods to efficiently search for the nearest `SeekPoint`(s) given a
/// timestamp.
///
/// A `SeekIndex` does not require complete coverage of the entire media stream. However, the better
/// the coverage, the smaller the manual search range the `SeekIndex` will return.
pub struct SeekIndex {
    points: Vec<SeekPoint>,
}

/// `SeekSearchResult` is the return value for a search on a `SeekIndex`. It returns a range of
/// `SeekPoint`s a `FormatReader` should search to find the desired timestamp. Ranges are
/// lower-bound inclusive, and upper-bound exclusive.
#[derive(Debug, PartialEq)]
pub enum SeekSearchResult {
    /// The `SeekIndex` is empty so the desired timestamp could not be found. The entire stream
    /// should be searched for the desired timestamp.
    Stream,
    /// The desired timestamp can be found before, the `SeekPoint`. The stream should be searched
    /// for the desired timestamp from the start of the stream up-to, but not including, the
    /// `SeekPoint`.
    Upper(SeekPoint),
    /// The desired timestamp can be found at, or after, the `SeekPoint`. The stream should be
    /// searched for the desired timestamp starting at the provided `SeekPoint` up-to the end of the
    /// stream.
    Lower(SeekPoint),
    /// The desired timestamp can be found within the range. The stream should be searched for the
    /// desired starting at the first `SeekPoint` up-to, but not-including, the second `SeekPoint`.
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
        // TODO: Ensure monotonic timestamp ordering of self.points.
        self.points.push(SeekPoint::new(frame, byte_offset, n_frames));
    }

    /// Search the index to find a bounded range of bytes, wherein the specified frame timestamp
    /// will be contained. If the index is empty, this function simply returns a result indicating
    /// the entire stream should be searched manually.
    pub fn search(&self, frame_ts: u64) -> SeekSearchResult {
        // The index must contain atleast one SeekPoint to return a useful result.
        if !self.points.is_empty() {
            let mut lower = 0;
            let mut upper = self.points.len() - 1;

            // If the desired timestamp is less than the first SeekPoint within the index, indicate
            // that the stream should be searched from the beginning.
            if frame_ts < self.points[lower].frame_ts {
                return SeekSearchResult::Upper(self.points[lower]);
            }
            // If the desired timestamp is greater than or equal to the last SeekPoint within the
            // index, indicate that the stream should be searched from the last SeekPoint.
            else if frame_ts >= self.points[upper].frame_ts {
                return SeekSearchResult::Lower(self.points[upper]);
            }

            // Desired timestamp is between the lower and upper indicies. Perform a binary search to
            // find a range of SeekPoints containing the desired timestamp. The binary search exits
            // when either two adjacent SeekPoints or a single SeekPoint is found.
            while upper - lower > 1 {
                let mid = (lower + upper) / 2;
                let mid_ts = self.points[mid].frame_ts;

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
    assert_eq!(
        index.search(110),
        SeekSearchResult::Range(SeekPoint::new(50 ,0, 45),
        SeekPoint::new(120,0,4))
    );
    assert_eq!(
        index.search(340),
        SeekSearchResult::Range(SeekPoint::new(320,0,100),
        SeekPoint::new(421,0,10))
    );
    assert_eq!(
        index.search(320),
        SeekSearchResult::Range(SeekPoint::new(320,0,100),
        SeekPoint::new(421,0,10))
    );
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

/// A `Stream` is an independently coded media stream. A media format may contain multiple media
/// streams in one container. Each of those media streams are represented by one `Stream`.
pub struct Stream {
    /// The parameters defining the codec for the `Stream`.
    pub codec_params: CodecParameters,
    /// The language of the `Stream`.
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

/// A `FormatReader` is a container demuxer. It provides methods to probe a media container for
/// information and access the streams encapsulated in the container.
///
/// Most, if not all, media containers contain metadata, then a number of packetized, and
/// interleaved media streams. Generally, the encapsulated streams are independently encoded using
/// some codec. The allowed codecs for a container are defined in the specification of the
/// container format.
///
/// During demuxing, packets are read one-by-one and may be discarded or decoded at the choice of
/// the caller. The definition of a packet is ambiguous, it may be a frame of video, 1 millisecond
/// or 1 second of audio, but a packet will never contain data from two different media streams.
/// Therefore the caller can be selective in what stream(s) should be decoded and consumed.
///
/// `FormatReader` provides an Iterator-like interface over packets for easy consumption and
/// filtering. Seeking will invalidate the assumed state of any decoder processing the packets from
/// `FormatReader` and should be reset after a successful seek operation.
pub trait FormatReader {

    /// Instantiates the `FormatReader` with the provided `FormatOptions`.
    fn open(source: MediaSourceStream, options: &FormatOptions) -> Self
    where
        Self: Sized;

    /// Gets a list of `FormatDescriptor`s for the formats supported by this `FormatReader`.
    fn supported_formats() -> &'static [FormatDescriptor]
    where
        Self: Sized;

    /// Probes the container to check for support, contained streams, and other metadata. The
    /// complexity of the probe can be set based on the caller's use-case.
    fn probe(&mut self, depth: ProbeDepth) -> Result<ProbeResult>;

    /// Gets a list of all `Tag`s.
    fn tags(&self) -> &[Tag];

    /// Gets a list of all `Visual`s.
    fn visuals(&self) -> &[Visual];

    /// Gets a list of all `Cue`s.
    fn cues(&self) -> &[Cue];

    //fn vendor_data(&self) -> &[u8];

    /// Seek, as closely as possible, to the timestamp requested.
    ///
    /// Note that many containers cannot seek to an exact timestamp, rather they can only seek to a
    /// coarse location and then to the decoder must decode packets until the exact timestamp is
    /// reached.
    fn seek(&mut self, ts: Timestamp) -> Result<u64>;

    /// Gets a list of streams in the container.
    fn streams(&self) -> &[Stream];

    /// Gets the default stream. If the media container has a method of determing the default
    /// stream, this function should return it. Otherwise, the first stream is returned. If no
    /// streams are present, None is returned.
    fn default_stream(&self) -> Option<&Stream> {
        let streams = self.streams();
        match streams.len() {
            0 => None,
            _ => Some(&streams[0]),
        }
    }

    /// Lazily get the next packet from the container.
    fn next_packet(&mut self) -> Result<Packet<'_>>;
}


pub enum PacketSource<'a> {
    Direct(&'a mut MediaSourceStream),
}

pub struct PacketStream<'a, B: Bytestream> {
    src: &'a mut B
}

impl<'a, B: Bytestream> Bytestream for PacketStream<'a, B> {

    /// Reads a single byte from the stream and returns it or an error.
    #[inline(always)]
    fn read_byte(&mut self) -> io::Result<u8> {
        self.src.read_byte()
    }

    // Reads two bytes from the stream and returns them in read-order or an error.
    #[inline(always)]
    fn read_double_bytes(&mut self) -> io::Result<[u8; 2]> {
       self.src.read_double_bytes()
    }

    // Reads three bytes from the stream and returns them in read-order or an error.
    #[inline(always)]
    fn read_triple_bytes(&mut self) -> io::Result<[u8; 3]> {
        self.src.read_triple_bytes()
    }

    // Reads four bytes from the stream and returns them in read-order or an error.
    #[inline(always)]
    fn read_quad_bytes(&mut self) -> io::Result<[u8; 4]> {
        self.src.read_quad_bytes()
    }

    #[inline(always)]
    fn read_buf_bytes(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.src.read_buf_bytes(buf)
    }

    #[inline(always)]
    fn scan_bytes_aligned<'b>(
        &mut self, pattern: &[u8],
        align: usize,
        buf: &'b mut [u8],
    ) -> io::Result<&'b mut [u8]> {
        self.src.scan_bytes_aligned(pattern, align, buf)
    }

    #[inline(always)]
    fn ignore_bytes(&mut self, count: u64) -> io::Result<()> {
        self.src.ignore_bytes(count)
    }

}

/// A `Packet` contains a discrete amount of encoded data for a single media stream. The exact
/// amount of data is bounded, but not defined and is dependant on the container and how it was
/// muxed.
///
/// Packets may be read by using the provided reader.
pub struct Packet<'a> {
    idx: u32,
    len: Option<usize>,
    src: PacketSource<'a>,
}

impl<'a> Packet<'a> {

    pub fn new_direct(idx: u32, mss: &'a mut MediaSourceStream) -> Self {
        Packet { idx, len: None, src: PacketSource::Direct(mss) }
    }

    /// The stream index for the stream this packet belongs to.
    pub fn stream_idx(&self) -> u32 {
        self.idx
    }

    /// The length of the packet in bytes.
    pub fn len(&self) -> Option<usize> {
        self.len
    }

    /// Converts the packet into a `Bytestream` for consumption.
    pub fn into_stream(self) -> PacketStream<'a, impl Bytestream> {
        match self.src {
            PacketSource::Direct(src) => PacketStream::<'a, MediaSourceStream> { src }
        }
    }

}

/// The result of a probe operation.
pub enum ProbeResult {
    /// The format is unsupported.
    Unsupported,
    /// The format is supported.
    Supported
}

/// The `FormatRegistry` allows the registration of an arbitrary number of `FormatReader`s and
/// subsequently will guess which `FormatReader` among them is appropriate for reading a given
/// `MediaSource`.
///
/// It is recommended that one `FormatRegistry` be created for the life of the application.
pub struct FormatRegistry {
    descriptors: Vec<(FormatDescriptor, u32)>,
}

impl FormatRegistry {

    pub fn new() -> Self {
        FormatRegistry {
            descriptors: Vec::new(),
        }
    }

    /// Attempts to guess and instantiate the appropriate `FormatReader` for the given source
    /// through analysis and the provided hints.
    ///
    /// Note: Guessing is currently naively implemented and only uses the extension and mime-type to
    /// choose the format.
    pub fn guess<S>(
        &self,
        hint: &Hint,
        src: Box<S>,
        options: &FormatOptions,
    ) -> Option<Box<dyn FormatReader>>
    where
        S: 'static + MediaSource
    {
        for descriptor in &self.descriptors {
            let mut supported = false;

            supported |= match hint.extension {
                Some(ref extension) => descriptor.0.supports_extension(extension),
                None => false
            };

            supported |= match hint.mime_type {
                Some(ref mime_type) => descriptor.0.supports_mime_type(mime_type),
                None => false
            };

            if supported {
                let mss = MediaSourceStream::new(src);
                return Some((descriptor.0.inst_func)(mss, options));
            }
        }

        None
    }

    /// Registers all formats supported by the Demuxer at the provided tier.
    pub fn register_all<F: FormatReader>(&mut self, tier: u32) {
        for descriptor in F::supported_formats() {
            self.register(&descriptor, tier);
        }
    }

    /// Register a single format at the provided tier.
    pub fn register(&mut self, descriptor: &FormatDescriptor, tier: u32) {
        let pos = self.descriptors.iter()
                                  .position(|entry| entry.1 < tier)
                                  .unwrap_or_else(|| self.descriptors.len());

        self.descriptors.insert(pos, (*descriptor, tier));
    }

}

/// A `Hint` provides additional information and context to the `FormatRegistry` when guessing what
/// `FormatReader` to use to open and read a piece of media.
///
/// For example, the `FormatRegistry` cannot examine the extension or mime-type of the media because
/// `MediaSourceStream` abstracts away such details. However, the embedder may have this information
/// from a file path, HTTP header, email  attachment metadata, etc. `Hint`s are optional, and won't
/// lead the registry astray if they're wrong, but they may provide an informed initial guess and
/// optimize the guessing process siginificantly especially as more formats are registered.
pub struct Hint {
    extension: Option<String>,
    mime_type: Option<String>,
}

impl Hint {
    /// Instantiate an empty `Hint`.
    pub fn new() -> Self {
        Hint {
            extension: None,
            mime_type: None,
        }
    }

    /// Add a file extension `Hint`.
    pub fn with_extension(&mut self, extension: &str) -> &mut Self {
        self.extension = Some(extension.to_owned());
        self
    }

    /// Add a MIME/Media-type `Hint`.
    pub fn mime_type(&mut self, mime_type: &str) -> &mut Self {
        self.mime_type = Some(mime_type.to_owned());
        self
    }
}

/// `FormatDescriptor` provides declarative information about the multimedia format that is used by
/// the `FormatRegistry`  and related machinery to guess the appropriate `FormatReader` for a given
/// media stream.
#[derive(Copy, Clone)]
pub struct FormatDescriptor {
    /// A list of case-insensitive file extensions that are generally used by the format.
    pub extensions: &'static [&'static str],
    /// A list of case-insensitive MIME types that are generally used by the format.
    pub mime_types: &'static [&'static str],
    /// An up-to 8 byte start-of-stream marker that will be searched for within the stream.
    pub marker: &'static [u8; 8],
    /// A bitmask applied to the aforementioned marker and the stream data to allow for bit-aligned
    /// start-of-stream markers. A mask of 0 disables masking.
    pub marker_mask: u64,
    /// The length of the marker in bytes (between 1 and 8). A length of 0 disables the marker
    /// search.
    pub marker_len: usize,
    // An instantiation function for the format.
    pub inst_func: fn(MediaSourceStream, &FormatOptions) -> Box<dyn FormatReader>,
}

impl FormatDescriptor {
    fn supports_extension(&self, extension: &str) -> bool {
        for supported_extension in self.extensions {
            if supported_extension.eq_ignore_ascii_case(extension) {
                return true
            }
        }
        false
    }

    fn supports_mime_type(&self, mime_type: &str) -> bool {
        false
    }
}

/// Convenience macro for declaring a `FormatDescriptor`.
#[macro_export]
macro_rules! support_format {
    ($exts:expr, $mimes:expr, $marker:expr, $mask:expr, $len:expr) => {
        FormatDescriptor {
            extensions: $exts,
            mime_types: $mimes,
            marker: $marker,
            marker_mask: $mask,
            marker_len: $len,
            inst_func: |source, opt| { Box::new(Self::open(source, &opt)) }
        }
    };
}