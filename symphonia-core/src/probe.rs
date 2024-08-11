// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The `probe` module provides methods and traits to support auto-detection of media formats from
//! arbitrary media streams.

use crate::common::Tier;
use crate::errors::{unsupported_error, Error, Result};
use crate::formats::{FormatInfo, FormatOptions, FormatReader};
use crate::io::{MediaSourceStream, ReadBytes, ScopedStream, SeekBuffered};
use crate::meta::{MetadataInfo, MetadataOptions, MetadataReader};

use log::{debug, error, info, trace, warn};

mod bloom {

    fn fnv1a32(value: &[u8; 2]) -> u32 {
        const INIT: u32 = 0x811c_9dc5;
        const PRIME: u32 = 0x0100_0193;

        let mut state = INIT;

        for byte in value.iter() {
            state = (state ^ u32::from(*byte)).wrapping_mul(PRIME);
        }

        state
    }

    pub struct BloomFilter {
        filter: Box<[u64]>,
    }

    impl Default for BloomFilter {
        fn default() -> Self {
            BloomFilter { filter: vec![0; BloomFilter::M >> 6].into_boxed_slice() }
        }
    }

    impl BloomFilter {
        /// The number of bits, m, used by the bloom filter. Use 16384 bits (2KiB) by default.
        const M: usize = 2 * 1024 * 8;

        pub fn insert(&mut self, key: &[u8; 2]) {
            let hash = fnv1a32(key);

            let h0 = (hash >> 16) as u16;
            let h1 = (hash >> 0) as u16;

            let i0 = h0 as usize & (BloomFilter::M - 1);
            let i1 = h0.wrapping_add(h1.wrapping_mul(1)) as usize & (BloomFilter::M - 1);
            let i2 = h0.wrapping_add(h1.wrapping_mul(2)) as usize & (BloomFilter::M - 1);

            self.filter[i0 >> 6] |= 1 << (i0 & 63);
            self.filter[i1 >> 6] |= 1 << (i1 & 63);
            self.filter[i2 >> 6] |= 1 << (i2 & 63);
        }

        pub fn may_contain(&self, key: &[u8; 2]) -> bool {
            let hash = fnv1a32(key);

            let h0 = (hash >> 16) as u16;
            let h1 = (hash >> 0) as u16;

            let i0 = h0 as usize & (BloomFilter::M - 1);
            let i1 = h0.wrapping_add(h1.wrapping_mul(1)) as usize & (BloomFilter::M - 1);
            let i2 = h0.wrapping_add(h1.wrapping_mul(2)) as usize & (BloomFilter::M - 1);

            if (self.filter[i0 >> 6] & (1 << (i0 & 63))) == 0 {
                return false;
            }
            if (self.filter[i1 >> 6] & (1 << (i1 & 63))) == 0 {
                return false;
            }
            if (self.filter[i2 >> 6] & (1 << (i2 & 63))) == 0 {
                return false;
            }

            true
        }
    }
}

/// The probe match specification provides declarative information that is used by a `Probe` to
/// detect the presence of a specific container or metadata format while scanning a
/// `MediaSourceStream`.
#[derive(Copy, Clone)]
pub struct ProbeDataMatchSpec {
    /// A list of case-insensitive file extensions that are generally used by the format.
    pub extensions: &'static [&'static str],
    /// A list of case-insensitive MIME types that are generally used by the format.
    pub mime_types: &'static [&'static str],
    /// A byte-string start-of-format marker that will be searched for within the stream. Typically
    /// some magic numbers associated with start of the container or metadata format.
    pub markers: &'static [&'static [u8]],
}

/// Container format-specific probe data.
#[derive(Copy, Clone)]
pub struct ProbeFormatData {
    /// The match specification used by the probe to match against the media source stream.
    pub spec: ProbeDataMatchSpec,
    /// A description of the container format and reader if a match with the basic probe data is
    /// found.
    pub info: FormatInfo,
}

/// Metadata format-specific probe data.
#[derive(Copy, Clone)]
pub struct ProbeMetadataData {
    /// The match specification used by the probe to match against the media source stream.
    pub spec: ProbeDataMatchSpec,
    /// A description of the metadata format and reader if a match with the basic probe data is
    /// found.
    pub info: MetadataInfo,
}

/// `FormatReader` probe factory function. Creates a boxed `FormatReader`.
pub type FormatFactoryFn =
    for<'s> fn(MediaSourceStream<'s>, FormatOptions) -> Result<Box<dyn FormatReader + 's>>;

/// `MetadataReader` probe factory function. Creates a boxed `MetadataReader`.
pub type MetadataFactoryFn =
    for<'s> fn(MediaSourceStream<'s>, MetadataOptions) -> Result<Box<dyn MetadataReader + 's>>;

/// A probe match is the result of one probe iteration on a given media source stream.
///
/// A probe match provides a basic description of the matching container or metadata format, and a
/// factory function to instantiate it.
#[derive(Copy, Clone)]
pub enum ProbeMatch {
    Format {
        /// A basic description about the container format.
        info: FormatInfo,
        /// A factory function to create an instance of the matching format reader.
        factory: FormatFactoryFn,
    },
    Metadata {
        /// A basic description about the metadata format.
        info: MetadataInfo,
        /// A factory function to create an instance of the matching metadata reader.
        factory: MetadataFactoryFn,
    },
}

/// A function pointer to the score function of the registered probeable.
type ScoreFn = fn(ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score>;

/// Private/internal generalized representation of a probeable format or metadata reader.
#[derive(Copy, Clone)]
struct GenericProbeMatch {
    /// The match specification.
    spec: ProbeDataMatchSpec,
    /// A function to assign a likelyhood score that the media source, readable with scoped access
    /// via. the provided stream, is the start of a metadate or container format
    score: ScoreFn,
    /// Format or metadata reader specific match information.
    specific: ProbeMatch,
}

/// The result of a scoring operation.
pub enum Score {
    /// The format is not supported.
    Unsupported,
    /// The format is supported with a confidence between 0 (not confident) and 255 (very
    /// confident).
    Supported(u8),
}

/// The `Scoreable` trait defines the scoring functionality a reader must implement to support
/// probing for a container or metadata format.
pub trait Scoreable {
    /// Using scoped access to a `MediaSourceStream`, calculate and return a value between 0 and 255
    /// indicating the confidence of the reader in decoding or parsing the stream.
    ///
    /// If the format is definitely not supported, then score should return [`Score::Unsupported`]
    /// since a score of 0 is still considered supported, even if unlikely.
    ///
    /// If an error is returned, errors other than [`Error::IoError`] (excluding the unexpected EOF
    /// kind) are treated as if [`Score::Unsupported`] was returned. All other IO errors abort
    /// the probe operation.
    fn score(src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score>;
}

/// To support probing, a `FormatReader` must implement the `ProbeableFormat` trait.
pub trait ProbeableFormat<'s>: FormatReader + Scoreable {
    /// Create an instance of the format reader.
    fn try_probe_new(
        mss: MediaSourceStream<'s>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + 's>>
    where
        Self: Sized;

    /// Returns a list of probe data that a [`Probe`] will use to determine if the reader
    /// implementing this trait may support the media source stream.
    fn probe_data() -> &'static [ProbeFormatData];
}

/// To support probing, a `MetadataReader` must implement the `ProbeableMetadata` trait.
pub trait ProbeableMetadata<'s>: MetadataReader + Scoreable {
    /// Create an instance of the metadata reader.
    fn try_probe_new(
        mss: MediaSourceStream<'s>,
        opts: MetadataOptions,
    ) -> Result<Box<dyn MetadataReader + 's>>
    where
        Self: Sized;

    /// Returns a list of probe data that a [`Probe`] will use to determine if the reader
    /// implementing this trait may support the media source stream.
    fn probe_data() -> &'static [ProbeMetadataData];
}

/// A `Hint` provides additional information and context when probing a media source stream.
///
/// For example, the `Probe` cannot examine the extension or mime-type of the media because
/// `MediaSourceStream` abstracts away such details. However, the embedder may have this information
/// from a file path, HTTP header, email attachment metadata, etc. `Hint`s are optional, and won't
/// lead the probe astray if they're wrong, but they may provide an informed initial guess and
/// optimize the guessing process siginificantly especially as more formats are registered.
#[derive(Clone, Debug, Default)]
pub struct Hint {
    extension: Option<String>,
    mime_type: Option<String>,
}

impl Hint {
    /// Instantiate an empty `Hint`.
    pub fn new() -> Self {
        Hint { extension: None, mime_type: None }
    }

    /// Add a file extension hint.
    pub fn with_extension(&mut self, extension: &str) -> &mut Self {
        self.extension = Some(extension.to_owned());
        self
    }

    /// Add a MIME/Media-type hint.
    pub fn mime_type(&mut self, mime_type: &str) -> &mut Self {
        self.mime_type = Some(mime_type.to_owned());
        self
    }
}

/// Options for controlling the behaviour of a `Probe`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ProbeOptions {
    /// The maximum number of bytes that will be scanned from the media source before giving up.
    ///
    /// The default is 1 MB, the maximum is 4 GB.
    pub max_probe_depth: u32,
    /// The maximum number of bytes a score operation may read before it will be forced to abort.
    ///
    /// The larger this value is, the larger the media source buffer must be, and therefore the more
    /// memory is consumed.
    ///
    /// The default is 16 kB, the maximum is 64 kB.
    pub max_score_depth: u16,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            max_probe_depth: 1 * 1024 * 1024, // 1 MB
            max_score_depth: 16 * 1024,       // 16 kB
        }
    }
}

/// `Probe` scans a `MediaSourceStream` for metadata and container formats, and provides an
/// iterator-like interface to instantiate readers for the formats encountered.
#[derive(Default)]
pub struct Probe {
    filter: bloom::BloomFilter,
    preferred: Vec<GenericProbeMatch>,
    standard: Vec<GenericProbeMatch>,
    fallback: Vec<GenericProbeMatch>,
    opts: ProbeOptions,
}

impl Probe {
    /// Instantiate a probe with default options.
    pub fn new() -> Self {
        Probe::new_with_options(&Default::default())
    }

    /// Instantiate a probe with custom options.
    pub fn new_with_options(opts: &ProbeOptions) -> Self {
        Probe { opts: *opts, ..Default::default() }
    }

    /// Register the parameterized format reader at the standard tier.
    pub fn register_format<P>(&mut self)
    where
        for<'a> P: ProbeableFormat<'a>,
    {
        self.register_format_at_tier::<P>(Tier::Standard);
    }

    /// Register the parameterized metadata reader at the standard tier.
    pub fn register_metadata<P>(&mut self)
    where
        for<'a> P: ProbeableMetadata<'a>,
    {
        self.register_metadata_at_tier::<P>(Tier::Standard);
    }

    /// Register the parameterized format reader at a specific tier.
    pub fn register_format_at_tier<P>(&mut self, tier: Tier)
    where
        for<'a> P: ProbeableFormat<'a>,
    {
        for data in P::probe_data() {
            // Build a generic format probe candidate.
            let candidate = GenericProbeMatch {
                spec: data.spec,
                score: P::score,
                specific: ProbeMatch::Format {
                    info: data.info,
                    factory: |mss, opts| Ok(P::try_probe_new(mss, opts)?),
                },
            };

            self.register(tier, candidate);
        }
    }

    /// Register the parameterized metadata reader at a specific tier.
    pub fn register_metadata_at_tier<P>(&mut self, tier: Tier)
    where
        for<'a> P: ProbeableMetadata<'a>,
    {
        for data in P::probe_data() {
            // Build a generic metadata probe candidate.
            let candidate = GenericProbeMatch {
                spec: data.spec,
                score: P::score,
                specific: ProbeMatch::Metadata {
                    info: data.info,
                    factory: |mss, opts| Ok(P::try_probe_new(mss, opts)?),
                },
            };

            self.register(tier, candidate);
        }
    }

    fn register(&mut self, tier: Tier, candidate: GenericProbeMatch) {
        // Insert 2-byte prefixes for each marker into the bloom filter.
        for marker in candidate.spec.markers {
            let mut prefix = [0u8; 2];

            match marker.len() {
                2..=16 => prefix.copy_from_slice(&marker[0..2]),
                _ => panic!("invalid marker length (only 2-16 bytes supported)."),
            }

            self.filter.insert(&prefix);
        }

        // Register at the desired tier.
        match tier {
            Tier::Preferred => self.preferred.push(candidate),
            Tier::Standard => self.standard.push(candidate),
            Tier::Fallback => self.fallback.push(candidate),
        }
    }

    /// Scans the provided `MediaSourceStream` from the current position for the best next metadata
    /// or format reader. If a match is found, returns it.
    pub fn next<'s>(&self, mss: &mut MediaSourceStream<'s>, _hint: &Hint) -> Result<ProbeMatch> {
        let mut win = 0u16;

        let init_pos = mss.pos();
        let mut count = 0;

        // Scan the stream byte-by-byte. Shifting each byte through a 2-byte window.
        while let Ok(byte) = mss.read_byte() {
            win = (win << 8) | u16::from(byte);

            count += 1;

            if count > self.opts.max_probe_depth {
                break;
            }

            if count % 4096 == 0 {
                debug!(
                    "searching for format marker... {}+{} / {} bytes",
                    init_pos, count, self.opts.max_probe_depth
                );
            }

            // Use the bloom filter to check if the the 2-byte window may be a prefix of a
            // registered marker.
            if self.filter.may_contain(&win.to_be_bytes()) {
                // Using the 2-byte window, and a further 14 bytes, create a larger 16-byte window.
                let mut window = [0u8; 16];

                window[0..2].copy_from_slice(&win.to_be_bytes()[0..2]);
                mss.read_buf_exact(&mut window[2..])?;

                // Re-align stream to the start of the marker for scoring.
                mss.seek_buffered_rel(-16);

                // Try to find a descriptor in the preferred tier.
                if let Some(inst) =
                    find_descriptor(mss, &self.preferred, window, self.opts.max_score_depth)?
                {
                    warn_junk_bytes(mss.pos(), init_pos);
                    return Ok(inst);
                }

                // Try to find a descriptor in the standard tier.
                if let Some(inst) =
                    find_descriptor(mss, &self.standard, window, self.opts.max_score_depth)?
                {
                    warn_junk_bytes(mss.pos(), init_pos);
                    return Ok(inst);
                }

                // Try to find a descriptor in the fallback tier.
                if let Some(inst) =
                    find_descriptor(mss, &self.fallback, window, self.opts.max_score_depth)?
                {
                    warn_junk_bytes(mss.pos(), init_pos);
                    return Ok(inst);
                }

                // If no registered markers were matched, re-align the stream to the end of the
                // 2-byte window, and continue probing.
                mss.seek_buffered_rel(2);
            }
        }

        if count < self.opts.max_probe_depth {
            error!("probe reached EOF at {} bytes", count);
        }
        else {
            // Could not find any marker within the probe limit.
            error!("reached probe limit of {} bytes", self.opts.max_probe_depth);
        }

        unsupported_error("core (probe): no suitable format reader found")
    }

    /// Searches the provided `MediaSourceStream` for a container format. Any metadata that is read
    /// during the search will be queued and attached to the `FormatReader` instance once a
    /// container format is found.
    pub fn format<'s>(
        &self,
        hint: &Hint,
        mut mss: MediaSourceStream<'s>,
        mut fmt_opts: FormatOptions,
        meta_opts: MetadataOptions,
    ) -> Result<Box<dyn FormatReader + 's>> {
        // Loop over all elements in the stream until a container format is found.
        loop {
            match self.next(&mut mss, hint)? {
                // If a container format is found, return an instance to it's reader.
                ProbeMatch::Format { factory, .. } => {
                    // Instantiate the format reader.
                    let format = factory(mss, fmt_opts)?;
                    return Ok(format);
                }
                // If metadata was found, instantiate the metadata reader, read the metadata, and
                // push it onto the metadata log.
                ProbeMatch::Metadata { factory, .. } => {
                    // Create the metadata reader.
                    let mut reader = factory(mss, meta_opts)?;

                    // Read all metadata and get a metdata revision.
                    let rev = reader.read_all()?;

                    // Consume the metadata reader and get the media source stream.
                    mss = reader.into_inner();

                    // Insert it into the metadata log.
                    fmt_opts.metadata.get_or_insert_with(Default::default).push(rev);

                    debug!("chaining a metadata element");
                }
            }
        }

        // This function returns when either the end-of-stream is reached, an error occurs, or a
        // container format is found.
    }
}

fn find_descriptor(
    mss: &mut MediaSourceStream,
    descs: &[GenericProbeMatch],
    win: [u8; 16],
    max_depth: u16,
) -> Result<Option<ProbeMatch>> {
    // Ensure the seekback buffer can satisfy the maximum amount of bytes a score operation may
    // consume.
    mss.ensure_seekback_buffer(usize::from(max_depth));

    for desc in descs {
        // If any format descriptor marker matches, then the format should be scored.
        let should_score = desc.spec.markers.iter().any(|marker| {
            let is_match = win[0..marker.len()] == **marker;

            if is_match {
                trace!("found the marker {:x?} @ {} bytes", &win[0..marker.len()], mss.pos());
            }

            is_match
        });

        // If a match is found, then score using the descriptor's score function.
        if should_score {
            // If supported, return the instantiate.
            if let Score::Supported(score) = score(desc, mss, max_depth)? {
                match &desc.specific {
                    ProbeMatch::Format { info, .. } => {
                        info!("selected format reader '{}' with score {}", info.short_name, score)
                    }
                    ProbeMatch::Metadata { info, .. } => {
                        info!("selected metadata reader '{}' with score {}", info.short_name, score)
                    }
                }

                return Ok(Some(desc.specific));
            }

            match &desc.specific {
                ProbeMatch::Format { info, .. } => {
                    trace!("format reader '{}' failed scoring", info.short_name)
                }
                ProbeMatch::Metadata { info, .. } => {
                    trace!("metadata reader '{}' failed scoring", info.short_name)
                }
            }
        }
    }

    Ok(None)
}

fn score(
    candidate: &GenericProbeMatch,
    mss: &mut MediaSourceStream,
    max_depth: u16,
) -> Result<Score> {
    // Save the initial position to rewind back to after scoring is complete.
    let init_pos = mss.pos();

    // Perform the scoring operation.
    let result = match (candidate.score)(ScopedStream::new(mss, u64::from(max_depth))) {
        Err(Error::IoError(err)) if err.kind() != std::io::ErrorKind::UnexpectedEof => {
            // IO errors that are not an unexpected end-of-file (or out-of-bounds) error, abort the
            // entire probe operation.
            Err(Error::IoError(err))
        }
        Err(_) => {
            // All other errors are caught and return unsupported.
            Ok(Score::Unsupported)
        }
        result => result,
    };

    // Rewind to the initial position.
    mss.seek_buffered(init_pos);

    result
}

fn warn_junk_bytes(pos: u64, init_pos: u64) {
    // Warn if junk bytes were skipped.
    if pos > init_pos {
        warn!("skipped {} bytes of junk at {}", pos - init_pos, init_pos);
    }
}

/// Convenience macro for declaring a `ProbeData` for a `FormatReader`.
#[macro_export]
macro_rules! support_format {
    ($info:expr, $exts:expr, $mimes:expr, $markers:expr) => {
        symphonia_core::probe::ProbeFormatData {
            spec: symphonia_core::probe::ProbeDataMatchSpec {
                extensions: $exts,
                mime_types: $mimes,
                markers: $markers,
            },
            info: $info,
        }
    };
}

/// Convenience macro for declaring a `ProbeData` for a `MetadataReader`.
#[macro_export]
macro_rules! support_metadata {
    ($info:expr, $exts:expr, $mimes:expr, $markers:expr) => {
        symphonia_core::probe::ProbeMetadataData {
            spec: symphonia_core::probe::ProbeDataMatchSpec {
                extensions: $exts,
                mime_types: $mimes,
                markers: $markers,
            },
            info: $info,
        }
    };
}
