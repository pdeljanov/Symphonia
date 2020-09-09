// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::support_format;
use symphonia_core::errors::{Result, Error, unsupported_error};
use symphonia_core::formats::prelude::*;
use symphonia_core::io::{ByteStream, MediaSource, MediaSourceStream};
use symphonia_core::meta::MetadataQueue;
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};

use std::io;
use std::io::{Seek, SeekFrom};

use crate::atoms::*;
use crate::track::Track;

use log::info;

/// ISO Base Media File Format (MP4, M4A, MOV, etc.) demultiplexer.
///
/// `IsoMp4Reader` implements a demuxer for the ISO Base Media File Format.
pub struct IsoMp4Reader {
    reader: MediaSourceStream,
    streams: Vec<Stream>,
    cues: Vec<Cue>,
    metadata: MetadataQueue,
    tracks: Vec<Track>,
}

impl QueryDescriptor for IsoMp4Reader {
    fn query() -> &'static [Descriptor] {
        &[
            support_format!(
                "isomp4",
                "ISO Base Media File Format",
                &[ "mp4", "m4a", "m4p", "m4b", "m4r", "m4v", "mov" ],
                &[ "video/mp4", "audio/m4a" ],
                &[ b"ftyp" ] // Top-level atoms
            ),
        ]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}

impl IsoMp4Reader {

    /// Gets a tuple containing the track index and timestamp for the next sample.
    fn next_nearest_timestamp(&self) -> Option<(usize, u64)> {
        let mut nearest = None;

        for (i, track) in self.tracks.iter().enumerate() {
            if let Some(ts) = track.next_packet_time() {
                nearest = match nearest {
                    Some((_, min_ts)) if ts >= min_ts => continue,
                    _                                 => Some((i, ts)),
                };
            }
        }

        nearest
    }

}

impl FormatReader for IsoMp4Reader {

    fn try_new(mut mss: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {

        // To get to beginning of the atom.
        mss.rewind(4);

        let is_seekable = mss.is_seekable();

        let mut ftyp = None;
        let mut moov = None;

        // Get the total length of the stream, if possible.
        let total_len = if is_seekable {
            let pos = mss.pos();
            let len = mss.seek(SeekFrom::End(0))?;
            mss.seek(SeekFrom::Start(pos))?;
            info!("stream is seekable with len={} bytes", len);
            Some(len)
        }
        else {
            None
        };

        let mut mdat_pos = None;

        // Parse all atoms if the stream is seekable, otherwise parse all atoms up-to the mdat atom.
        let mut iter = AtomIterator::new_root(&mut mss, total_len);

        while let Some(header) = iter.next()? {
            // Top-level atoms.
            match header.atype {
                AtomType::Ftyp => {
                    ftyp = Some(iter.read_atom::<FtypAtom>()?);
                }
                AtomType::Moov => {
                    moov = Some(iter.read_atom::<MoovAtom>()?);
                }
                // AtomType::Moof => {
                    // Fragmented file
                // }
                AtomType::Mdat => {
                    // The mdat atom contains the codec bitstream data. If the source is unseekable
                    // then the format reader cannot skip past this atom.
                    if !is_seekable {
                        break;
                    }
                    else {
                        // If the source is seekable, then get the position of the mdat atom.
                        // We'll continue parsing atoms and then seek back to the mdat atom.
                        mdat_pos = Some(iter.inner().pos());
                    }
                }
                // AtomType::Meta => {

                // }
                AtomType::Free => (),
                _ => {
                    info!("skipping over atom: {:?}", header.atype);
                }
            }
        }

        if ftyp.is_none() {
            return unsupported_error("missing ftyp atom");
        }

        if moov.is_none() {
            return unsupported_error("missing moov atom");
        }

        // If required and/or possible, seek back to the mdat atom.
        if let Some(pos) = mdat_pos {
            mss.seek(SeekFrom::Start(pos))?;
        }

        // Iterate over all trak atoms and wrap all traks belonging to an audio track in a Track
        // struct.
        let tracks = moov.unwrap()
                        .traks
                        .into_iter()
                        .filter(|trak| trak.mdia.hdlr.track_type == hdlr::TrackType::Sound )
                        .map(|trak| Track::new(trak))
                        .collect::<Vec<Track>>();

        // Instantiate Stream(s) for all Track(s).
        let streams = tracks.iter()
                        .enumerate()
                        .map(|(i, track)| Stream::new(i as u32, track.codec_params()))
                        .collect();

        Ok(IsoMp4Reader {
            reader: mss,
            streams,
            cues: Default::default(),
            metadata: Default::default(),
            tracks,
        })
    }

    fn next_packet(&mut self) -> Result<Packet> {
        // Get the next packet from the track with the nearest timestamp, or return an EOS.
        if let Some((i, _ts)) = self.next_nearest_timestamp() {
            self.tracks[i].read_next_packet(&mut self.reader)
        }
        else {
            Err(Error::IoError(io::Error::new(io::ErrorKind::UnexpectedEof, "end of stream")))
        }
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

    fn seek(&mut self, _to: SeekTo) -> Result<SeekedTo> {
        unsupported_error("seeking unsupported")
    }

}