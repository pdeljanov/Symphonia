// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::num::NonZero;

use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::ReadBytes;

pub(crate) mod alac;
pub(crate) mod avcc;
pub(crate) mod co64;
pub(crate) mod ctts;
pub(crate) mod dac3;
pub(crate) mod dec3;
pub(crate) mod dovi;
pub(crate) mod edts;
pub(crate) mod elst;
pub(crate) mod esds;
pub(crate) mod flac;
pub(crate) mod ftyp;
pub(crate) mod hdlr;
pub(crate) mod hvcc;
pub(crate) mod ilst;
pub(crate) mod mdhd;
pub(crate) mod mdia;
pub(crate) mod mehd;
pub(crate) mod meta;
pub(crate) mod mfhd;
pub(crate) mod minf;
pub(crate) mod moof;
pub(crate) mod moov;
pub(crate) mod mvex;
pub(crate) mod mvhd;
pub(crate) mod opus;
pub(crate) mod sidx;
pub(crate) mod smhd;
pub(crate) mod stbl;
pub(crate) mod stco;
pub(crate) mod stsc;
pub(crate) mod stsd;
pub(crate) mod stss;
pub(crate) mod stsz;
pub(crate) mod stts;
pub(crate) mod tfhd;
pub(crate) mod tkhd;
pub(crate) mod traf;
pub(crate) mod trak;
pub(crate) mod trex;
pub(crate) mod trun;
pub(crate) mod udta;
pub(crate) mod wave;

pub use self::meta::MetaAtom;
pub use alac::AlacAtom;
pub use avcc::AvcCAtom;
pub use co64::Co64Atom;
#[allow(unused_imports)]
pub use ctts::CttsAtom;
pub use dac3::Dac3Atom;
pub use dec3::Dec3Atom;
pub use dovi::DoviAtom;
pub use edts::EdtsAtom;
pub use elst::ElstAtom;
pub use esds::EsdsAtom;
pub use flac::FlacAtom;
pub use ftyp::FtypAtom;
pub use hdlr::HdlrAtom;
pub use hvcc::HvcCAtom;
pub use ilst::IlstAtom;
pub use mdhd::MdhdAtom;
pub use mdia::MdiaAtom;
pub use mehd::MehdAtom;
pub use mfhd::MfhdAtom;
pub use minf::MinfAtom;
pub use moof::MoofAtom;
pub use moov::MoovAtom;
pub use mvex::MvexAtom;
pub use mvhd::MvhdAtom;
pub use opus::OpusAtom;
pub use sidx::SidxAtom;
pub use smhd::SmhdAtom;
pub use stbl::StblAtom;
pub use stco::StcoAtom;
pub use stsc::StscAtom;
pub use stsd::StsdAtom;
#[allow(unused_imports)]
pub use stss::StssAtom;
pub use stsz::StszAtom;
pub use stts::SttsAtom;
pub use tfhd::TfhdAtom;
pub use tkhd::TkhdAtom;
pub use traf::TrafAtom;
pub use trak::TrakAtom;
pub use trex::TrexAtom;
pub use trun::TrunAtom;
pub use udta::UdtaAtom;
pub use wave::WaveAtom;

pub(crate) const MAX_ATOM_SIZE: u64 = 1024;

/// Atom types.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AtomType {
    Ac3Config,
    AdvisoryTag,
    AlbumArtistTag,
    AlbumTag,
    ArrangerTag,
    ArtistTag,
    AudioSampleEntryAc3,
    AudioSampleEntryAlac,
    AudioSampleEntryALaw,
    AudioSampleEntryEc3,
    AudioSampleEntryF32,
    AudioSampleEntryF64,
    AudioSampleEntryFlac,
    AudioSampleEntryLpcm,
    AudioSampleEntryMp3,
    AudioSampleEntryMp4a,
    AudioSampleEntryMuLaw,
    AudioSampleEntryOpus,
    AudioSampleEntryQtWave,
    AudioSampleEntryS16Be,
    AudioSampleEntryS16Le,
    AudioSampleEntryS24,
    AudioSampleEntryS32,
    AudioSampleEntryU8,
    AuthorTag,
    AvcConfiguration,
    BitRate,
    ChunkOffset,
    ChunkOffset64,
    CleanAperture,
    CommentTag,
    CompilationTag,
    ComposerTag,
    CompositionTimeToSample,
    ConductorTag,
    CopyrightTag,
    CoverTag,
    CustomGenreTag,
    DateTag,
    DescriptionTag,
    DiskNumberTag,
    DolbyVisionConfiguration,
    Eac3Config,
    Edit,
    EditList,
    EncodedByTag,
    EncoderTag,
    Esds,
    FileCreatorUrlTag,
    FileType,
    FlacDsConfig,
    Free,
    FreeFormTag,
    GaplessPlaybackTag,
    GenreTag,
    GroupingTag,
    Handler,
    HdVideoTag,
    HevcConfiguration,
    IdentPodcastTag,
    IsrcTag,
    ItunesAccountIdTag,
    ItunesAccountTypeIdTag,
    ItunesArtistIdTag,
    ItunesComposerIdTag,
    ItunesContentIdTag,
    ItunesCountryIdTag,
    ItunesGenreIdTag,
    ItunesPlaylistIdTag,
    LabelTag,
    LabelUrlTag,
    LongDescriptionTag,
    LyricsTag,
    Media,
    MediaData,
    MediaHeader,
    MediaInfo,
    MediaTypeTag,
    Meta,
    MetaList,
    MetaTagData,
    MetaTagMeaning,
    MetaTagName,
    MovementCountTag,
    MovementIndexTag,
    MovementTag,
    Movie,
    MovieExtends,
    MovieExtendsHeader,
    MovieFragment,
    MovieFragmentHeader,
    MovieHeader,
    NarratorTag,
    OpusDsConfig,
    OriginalArtistTag,
    OwnerTag,
    PixelAspectRatio,
    PodcastCategoryTag,
    PodcastKeywordsTag,
    PodcastTag,
    ProducerTag,
    PublisherTag,
    PurchaseDateTag,
    RatingTag,
    RecordingCopyrightTag,
    SampleDescription,
    SampleSize,
    SampleTable,
    SampleToChunk,
    SegmentIndex,
    ShowMovementTag,
    Skip,
    SoloistTag,
    SortAlbumArtistTag,
    SortAlbumTag,
    SortArtistTag,
    SortComposerTag,
    SortNameTag,
    SortShowNameTag,
    SoundMediaHeader,
    SubtitleSampleEntryText,
    SubtitleSampleEntryTimedText,
    SubtitleSampleEntryXml,
    SyncSample,
    TempoTag,
    TextConfig,
    TimeToSample,
    Track,
    TrackArtistUrl,
    TrackExtends,
    TrackFragment,
    TrackFragmentHeader,
    TrackFragmentRun,
    TrackHeader,
    TrackNumberTag,
    TrackTitleTag,
    TvEpisodeNameTag,
    TvEpisodeNumberTag,
    TvNetworkNameTag,
    TvSeasonNumberTag,
    TvShowNameTag,
    UrlPodcastTag,
    UserData,
    Uuid,
    VisualSampleEntryAv1,
    VisualSampleEntryAvc1,
    VisualSampleEntryDvh1,
    VisualSampleEntryDvhe,
    VisualSampleEntryHev1,
    VisualSampleEntryHvc1,
    VisualSampleEntryMp4v,
    VisualSampleEntryVp8,
    VisualSampleEntryVp9,
    WorkTag,
    WriterTag,
    XidTag,
    Other([u8; 4]),
}

impl From<[u8; 4]> for AtomType {
    fn from(val: [u8; 4]) -> Self {
        match &val {
            b".mp3" => AtomType::AudioSampleEntryMp3,
            b"ac-3" => AtomType::AudioSampleEntryAc3,
            b"alac" => AtomType::AudioSampleEntryAlac,
            b"alaw" => AtomType::AudioSampleEntryALaw,
            b"av01" => AtomType::VisualSampleEntryAv1,
            b"avc1" => AtomType::VisualSampleEntryAvc1,
            b"avcC" => AtomType::AvcConfiguration,
            b"btrt" => AtomType::BitRate,
            b"ec-3" => AtomType::AudioSampleEntryEc3,
            b"clap" => AtomType::CleanAperture,
            b"co64" => AtomType::ChunkOffset64,
            b"ctts" => AtomType::CompositionTimeToSample,
            b"dac3" => AtomType::Ac3Config,
            b"dec3" => AtomType::Eac3Config,
            b"data" => AtomType::MetaTagData,
            b"dfLa" => AtomType::FlacDsConfig,
            b"dOps" => AtomType::OpusDsConfig,
            b"dvcC" => AtomType::DolbyVisionConfiguration,
            b"dvh1" => AtomType::VisualSampleEntryDvh1,
            b"dvhe" => AtomType::VisualSampleEntryDvhe,
            b"dvvC" => AtomType::DolbyVisionConfiguration,
            b"edts" => AtomType::Edit,
            b"elst" => AtomType::EditList,
            b"esds" => AtomType::Esds,
            b"fl32" => AtomType::AudioSampleEntryF32,
            b"fl64" => AtomType::AudioSampleEntryF64,
            b"fLaC" => AtomType::AudioSampleEntryFlac,
            b"free" => AtomType::Free,
            b"ftyp" => AtomType::FileType,
            b"hdlr" => AtomType::Handler,
            b"hev1" => AtomType::VisualSampleEntryHev1,
            b"hvc1" => AtomType::VisualSampleEntryHvc1,
            b"hvcC" => AtomType::HevcConfiguration,
            b"ilst" => AtomType::MetaList,
            b"in24" => AtomType::AudioSampleEntryS24,
            b"in32" => AtomType::AudioSampleEntryS32,
            b"lpcm" => AtomType::AudioSampleEntryLpcm,
            b"mdat" => AtomType::MediaData,
            b"mdhd" => AtomType::MediaHeader,
            b"mdia" => AtomType::Media,
            b"mean" => AtomType::MetaTagMeaning,
            b"mehd" => AtomType::MovieExtendsHeader,
            b"meta" => AtomType::Meta,
            b"mfhd" => AtomType::MovieFragmentHeader,
            b"minf" => AtomType::MediaInfo,
            b"moof" => AtomType::MovieFragment,
            b"moov" => AtomType::Movie,
            b"mp4a" => AtomType::AudioSampleEntryMp4a,
            b"mp4v" => AtomType::VisualSampleEntryMp4v,
            b"mvex" => AtomType::MovieExtends,
            b"mvhd" => AtomType::MovieHeader,
            b"name" => AtomType::MetaTagName,
            b"Opus" => AtomType::AudioSampleEntryOpus,
            b"pasp" => AtomType::PixelAspectRatio,
            b"raw " => AtomType::AudioSampleEntryU8,
            b"sbtt" => AtomType::SubtitleSampleEntryText,
            b"sidx" => AtomType::SegmentIndex,
            b"skip" => AtomType::Skip,
            b"smhd" => AtomType::SoundMediaHeader,
            b"sowt" => AtomType::AudioSampleEntryS16Le,
            b"stbl" => AtomType::SampleTable,
            b"stco" => AtomType::ChunkOffset,
            b"stpp" => AtomType::SubtitleSampleEntryXml,
            b"stsc" => AtomType::SampleToChunk,
            b"stsd" => AtomType::SampleDescription,
            b"stss" => AtomType::SyncSample,
            b"stsz" => AtomType::SampleSize,
            b"stts" => AtomType::TimeToSample,
            b"tfhd" => AtomType::TrackFragmentHeader,
            b"tkhd" => AtomType::TrackHeader,
            b"traf" => AtomType::TrackFragment,
            b"trak" => AtomType::Track,
            b"trex" => AtomType::TrackExtends,
            b"trun" => AtomType::TrackFragmentRun,
            b"twos" => AtomType::AudioSampleEntryS16Be,
            b"tx3g" => AtomType::SubtitleSampleEntryTimedText,
            b"txtC" => AtomType::TextConfig,
            b"udta" => AtomType::UserData,
            b"ulaw" => AtomType::AudioSampleEntryMuLaw,
            b"uuid" => AtomType::Uuid,
            b"vp08" => AtomType::VisualSampleEntryVp8,
            b"vp09" => AtomType::VisualSampleEntryVp9,
            b"wave" => AtomType::AudioSampleEntryQtWave,
            // Metadata Boxes
            b"----" => AtomType::FreeFormTag,
            b"aART" => AtomType::AlbumArtistTag,
            b"akID" => AtomType::ItunesAccountTypeIdTag,
            b"apID" => AtomType::ItunesAccountIdTag,
            b"atID" => AtomType::ItunesArtistIdTag,
            b"catg" => AtomType::PodcastCategoryTag,
            b"cmID" => AtomType::ItunesComposerIdTag,
            b"cnID" => AtomType::ItunesContentIdTag,
            b"covr" => AtomType::CoverTag,
            b"cpil" => AtomType::CompilationTag,
            b"cprt" => AtomType::CopyrightTag,
            b"desc" => AtomType::DescriptionTag,
            b"disk" => AtomType::DiskNumberTag,
            b"egid" => AtomType::IdentPodcastTag,
            b"geID" => AtomType::ItunesGenreIdTag,
            b"gnre" => AtomType::GenreTag,
            b"hdvd" => AtomType::HdVideoTag,
            b"keyw" => AtomType::PodcastKeywordsTag,
            b"ldes" => AtomType::LongDescriptionTag,
            b"ownr" => AtomType::OwnerTag,
            b"pcst" => AtomType::PodcastTag,
            b"pgap" => AtomType::GaplessPlaybackTag,
            b"plID" => AtomType::ItunesPlaylistIdTag,
            b"purd" => AtomType::PurchaseDateTag,
            b"purl" => AtomType::UrlPodcastTag,
            b"rate" => AtomType::RatingTag,
            b"rtng" => AtomType::AdvisoryTag,
            b"sfID" => AtomType::ItunesCountryIdTag,
            b"shwm" => AtomType::ShowMovementTag,
            b"soaa" => AtomType::SortAlbumArtistTag,
            b"soal" => AtomType::SortAlbumTag,
            b"soar" => AtomType::SortArtistTag,
            b"soco" => AtomType::SortComposerTag,
            b"sonm" => AtomType::SortNameTag,
            b"sosn" => AtomType::SortShowNameTag,
            b"stik" => AtomType::MediaTypeTag,
            b"tmpo" => AtomType::TempoTag,
            b"trkn" => AtomType::TrackNumberTag,
            b"tven" => AtomType::TvEpisodeNameTag,
            b"tves" => AtomType::TvEpisodeNumberTag,
            b"tvnn" => AtomType::TvNetworkNameTag,
            b"tvsh" => AtomType::TvShowNameTag,
            b"tvsn" => AtomType::TvSeasonNumberTag,
            b"xid " => AtomType::XidTag,
            b"\xa9alb" => AtomType::AlbumTag,
            b"\xa9arg" => AtomType::ArrangerTag,
            b"\xa9ART" => AtomType::ArtistTag,
            b"\xa9aut" => AtomType::AuthorTag,
            b"\xa9cmt" => AtomType::CommentTag,
            b"\xa9com" => AtomType::ComposerTag,
            b"\xa9con" => AtomType::ConductorTag,
            b"\xa9day" => AtomType::DateTag,
            b"\xa9enc" => AtomType::EncodedByTag,
            b"\xa9gen" => AtomType::CustomGenreTag,
            b"\xa9grp" => AtomType::GroupingTag,
            b"\xa9isr" => AtomType::IsrcTag,
            b"\xa9lab" => AtomType::LabelTag,
            b"\xa9lal" => AtomType::LabelUrlTag,
            b"\xa9lyr" => AtomType::LyricsTag,
            b"\xa9mal" => AtomType::FileCreatorUrlTag,
            b"\xa9mvc" => AtomType::MovementCountTag,
            b"\xa9mvi" => AtomType::MovementIndexTag,
            b"\xa9mvn" => AtomType::MovementTag,
            b"\xa9nam" => AtomType::TrackTitleTag,
            b"\xa9nrt" => AtomType::NarratorTag,
            b"\xa9ope" => AtomType::OriginalArtistTag,
            b"\xa9phg" => AtomType::RecordingCopyrightTag,
            b"\xa9prd" => AtomType::ProducerTag,
            b"\xa9prl" => AtomType::TrackArtistUrl,
            b"\xa9pub" => AtomType::PublisherTag,
            b"\xa9sol" => AtomType::SoloistTag,
            b"\xa9too" => AtomType::EncoderTag,
            b"\xa9wrk" => AtomType::WorkTag,
            b"\xa9wrt" => AtomType::WriterTag,
            _ => AtomType::Other(val),
        }
    }
}

/// Common atom header.
#[derive(Copy, Clone, Debug)]
pub struct AtomHeader {
    /// The atom type.
    atom_type: AtomType,
    /// The size of all reader headers.
    header_len: u8,
    /// The position of the atom.
    atom_pos: u64,
    /// The total size of the atom including all headers.
    atom_len: Option<NonZero<u64>>,
}

impl AtomHeader {
    /// Size of a standard atom header.
    const HEADER_SIZE: u8 = 8;
    /// Size of a standard atom header with a 64-bit size.
    const LARGE_HEADER_SIZE: u8 = AtomHeader::HEADER_SIZE + 8;

    /// Reads an atom header from the provided `ByteStream`.
    pub fn read<B: ReadBytes>(reader: &mut B) -> Result<AtomHeader> {
        let atom_pos = reader.pos();

        let atom_len = u64::from(reader.read_be_u32()?);
        let atom_type = AtomType::from(reader.read_quad_bytes()?);

        let (header_len, atom_len) = match atom_len {
            0 => {
                // An atom size of 0 indicates the atom spans the remainder of the stream or file.
                (AtomHeader::HEADER_SIZE, None)
            }
            1 => {
                // An atom size of 1 indicates a 64-bit atom size should be read.
                let large_atom_len = reader.read_be_u64()?;

                // The atom size should be atleast the size of the header.
                if large_atom_len < u64::from(AtomHeader::LARGE_HEADER_SIZE) {
                    return decode_error("isomp4: atom size is invalid");
                }

                (AtomHeader::LARGE_HEADER_SIZE, NonZero::new(large_atom_len))
            }
            _ => {
                // The atom size should be atleast the size of the header.
                if atom_len < u64::from(AtomHeader::HEADER_SIZE) {
                    return decode_error("isomp4: atom size is invalid");
                }

                (AtomHeader::HEADER_SIZE, NonZero::new(atom_len))
            }
        };

        Ok(AtomHeader { atom_type, atom_pos, atom_len, header_len })
    }

    /// Get the atom type.
    pub fn atom_type(&self) -> AtomType {
        self.atom_type
    }

    /// Get the atom position.
    pub fn atom_pos(&self) -> u64 {
        self.atom_pos
    }

    /// If known, get the total atom size.
    pub fn atom_len(&self) -> Option<NonZero<u64>> {
        self.atom_len
    }

    /// Get the atom's header size.
    #[allow(dead_code)]
    pub fn header_len(&self) -> u64 {
        u64::from(self.header_len)
    }

    /// If the atom size is known, get the total payload data size.
    pub fn data_len(&self) -> Option<u64> {
        self.atom_len.map(|atom_len| atom_len.get() - u64::from(self.header_len))
    }

    /// Given a position, and if the atom size is known, calculate the amount of unread payload
    /// data.
    ///
    /// Panics if the position is before the atom payload. This is a coding error.
    pub fn data_unread_at(&self, pos: u64) -> Option<u64> {
        self.atom_len.map(|atom_len| {
            // Payload data position and size.
            let data_pos = self.atom_pos + u64::from(self.header_len);
            let data_len = atom_len.get() - u64::from(self.header_len);

            if pos >= data_pos + data_len {
                // Current position after the atom payload.
                0
            }
            else if pos >= data_pos {
                // Current position within the atom payload.
                data_len - (pos - data_pos)
            }
            else {
                // Current position before atom payload (this is a coding error).
                panic!("isomp4: current position preceeds atom payload");
            }
        })
    }

    /// If the header belongs to a UUID atom, read and return the UUID. Panics if called on a
    /// non-UUID atom.
    ///
    /// On success, consumes 16 bytes from the payload size.
    #[allow(dead_code)]
    pub fn read_uuid<B: ReadBytes>(&mut self, reader: &mut B) -> Result<[u8; 16]> {
        match self.atom_type {
            AtomType::Uuid => {
                // If the payload size is known, then check that 16 bytes of the payload is
                // available to be read as the UUID.
                if let Some(data_len) = self.data_len() {
                    if data_len < 16 {
                        return decode_error("isomp4: uuid atom too small");
                    }
                }

                // Read a 16-byte UUID.
                let mut uuid = [0; 16];
                reader.read_buf_exact(&mut uuid)?;
                // Adjust header size.
                self.header_len += 16;

                Ok(uuid)
            }
            _ => panic!("isomp4: attempted to read a uuid on a non-uuid atom"),
        }
    }

    /// Read the version and flags extended atom header fields.
    ///
    /// On success, consumes 4 bytes from the payload size.
    pub fn read_extended_header<B: ReadBytes>(&mut self, reader: &mut B) -> Result<(u8, u32)> {
        // If the payload size is known, then check that 4 bytes of the payload is available to be
        // read as the extended header.
        if let Some(data_len) = self.data_len() {
            if data_len < 4 {
                return decode_error("isomp4: uuid atom too small");
            }
        }

        // Read the extended header fields.
        let header = (reader.read_u8()?, reader.read_be_u24()?);
        // Adjust the header size.
        self.header_len += 4;

        Ok(header)
    }
}

pub trait Atom: Sized {
    fn read<B: ReadBytes>(reader: &mut B, header: AtomHeader) -> Result<Self>;
}

pub struct AtomIterator<B: ReadBytes> {
    reader: B,
    len: Option<u64>,
    cur_atom: Option<AtomHeader>,
    base_pos: u64,
    next_atom_pos: u64,
}

impl<B: ReadBytes> AtomIterator<B> {
    pub fn new_root(reader: B, len: Option<u64>) -> Self {
        let base_pos = reader.pos();

        AtomIterator { reader, len, cur_atom: None, base_pos, next_atom_pos: base_pos }
    }

    pub fn new(reader: B, parent: AtomHeader) -> Self {
        let base_pos = reader.pos();

        AtomIterator {
            reader,
            len: parent.data_unread_at(base_pos),
            cur_atom: None,
            base_pos,
            next_atom_pos: base_pos,
        }
    }

    pub fn into_inner(self) -> B {
        self.reader
    }

    pub fn inner_mut(&mut self) -> &mut B {
        &mut self.reader
    }

    pub fn next(&mut self) -> Result<Option<AtomHeader>> {
        // Ignore any remaining data in the current atom that was not read.
        let cur_pos = self.reader.pos();

        if cur_pos < self.next_atom_pos {
            self.reader.ignore_bytes(self.next_atom_pos - cur_pos)?;
        }
        else if cur_pos > self.next_atom_pos {
            // This is very bad, either the atom's length was incorrect or the demuxer erroroneously
            // overread an atom.
            return decode_error("isomp4: overread atom");
        }

        // If len is specified, then do not read more than len bytes.
        if let Some(len) = self.len {
            if self.next_atom_pos - self.base_pos >= len {
                return Ok(None);
            }
        }

        // Read the next atom header.
        let atom = AtomHeader::read(&mut self.reader)?;

        // Calculate the start position for the next atom (the exclusive end of the current atom).
        self.next_atom_pos = match atom.atom_len {
            None => {
                // An atom with a length of zero is defined to span to the end of the stream. If
                // len is available, use it for the next atom start position, otherwise, use u64 max
                // which will trip an end of stream error on the next iteration.
                self.len.map(|l| self.base_pos + l).unwrap_or(u64::MAX)
            }

            Some(atom_len) => self.next_atom_pos + atom_len.get(),
        };

        self.cur_atom = Some(atom);

        Ok(self.cur_atom)
    }

    pub fn next_no_consume(&mut self) -> Result<Option<AtomHeader>> {
        if self.cur_atom.is_some() {
            Ok(self.cur_atom)
        }
        else {
            self.next()
        }
    }

    pub fn read_atom<A: Atom>(&mut self) -> Result<A> {
        // It is not possible to read the current atom more than once because ByteStream is not
        // seekable. Therefore, raise an assert if read_atom is called more than once between calls
        // to next, or after next returns None.
        assert!(self.cur_atom.is_some());
        A::read(&mut self.reader, self.cur_atom.take().unwrap())
    }

    pub fn consume_atom(&mut self) {
        assert!(self.cur_atom.take().is_some());
    }
}
