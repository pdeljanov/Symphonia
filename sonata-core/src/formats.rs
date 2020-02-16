// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The `format` module provides the traits and support structures necessary to implement media
//! demuxers.

use std::default::Default;
use std::fmt;

use crate::audio::Timestamp;
use crate::codecs::CodecParameters;
use crate::errors::Result;
use crate::io::{BufStream, MediaSourceStream};
use crate::meta::{MetadataQueue, Tag};

pub mod prelude {
    pub use super::{Cue, FormatOptions, FormatReader, Packet, SeekIndex, SeekSearchResult, Stream};
}

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

/// `FormatOptions` is a common set of options that all demuxers use.
pub struct FormatOptions {
    /// Selects the logging verbosity of the demuxer.
    pub verbosity: Verbosity,
}

impl Default for FormatOptions {
    fn default() -> Self {
        FormatOptions {
            verbosity: Verbosity::Error,
        }
    }
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
/// interleaved codec bitstreams. Generally, the encapsulated bitstreams are independently encoded
/// using some codec. The allowed codecs for a container are defined in the specification of the
/// container format.
///
/// While demuxing, packets are read one-by-one and may be discarded or decoded at the choice of
/// the caller. The contents of a packet is undefined, it may be a frame of video, 1 millisecond
/// or 1 second of audio, but a packet will never contain data from two different bitstreams.
/// Therefore the caller can be selective in what stream(s) should be decoded and consumed.
///
/// `FormatReader` provides an Iterator-like interface over packets for easy consumption and
/// filtering. Seeking will invalidate the assumed state of any decoder processing packets from
/// `FormatReader` and should be reset after a successful seek operation.
pub trait FormatReader {
    /// Attempt to instantiates a `FormatReader` using the provided `FormatOptions` and
    /// `MediaSourceStream`. The reader will probe the container to verify format support, determine
    /// the number of contained streams, and read any metadata.
    fn try_new(source: MediaSourceStream, options: &FormatOptions) -> Result<Self>
    where
        Self: Sized;

    /// Gets a list of all `Cue`s.
    fn cues(&self) -> &[Cue];

    /// Gets the metadata revision queue.
    fn metadata(&self) -> &MetadataQueue;

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

    /// Get the next packet from the container.
    fn next_packet(&mut self) -> Result<Packet>;
}

/// A `Packet` contains a discrete amount of encoded data for a single codec bitstream. The exact
/// amount of data is bounded, but not defined, and is dependant on the container and/or the
/// encapsulated codec.
pub struct Packet {
    id: u32,
    pts: u64,
    data: Box<[u8]>,
}

impl Packet {
    /// Create a new `Packet` from a slice.
    pub fn new_from_slice(id: u32, pts: u64, buf: &[u8]) -> Self {
        Packet { id, pts, data: Box::from(buf) }
    }

    /// Create a new `Packet` from a boxed slice.
    pub fn new_from_boxed_slice(id: u32, pts: u64, data: Box<[u8]>) -> Self {
        Packet { id, pts, data }
    }

    /// The stream identifier of the stream this packet belongs to.
    pub fn stream_id(&self) -> u32 {
        self.id
    }

    /// Get the presentation timestamp of the packet.
    pub fn pts(&self) -> u64 {
        self.pts
    }

    /// Get the length in time of the packet.
    pub fn tlen(&self) -> u64 {
        0
    }

    /// Get the packet data buffer as an immutable slice.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Get a `BufStream` to read the packet data buffer sequentially.
    pub fn as_buf_stream(&self) -> BufStream {
        BufStream::new(&self.data)
    }
}