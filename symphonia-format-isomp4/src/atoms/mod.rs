// Symphonia
// Copyright (c) 2020 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{Result, decode_error};
use symphonia_core::io::ByteStream;

pub(crate) mod co64;
pub(crate) mod ctts;
pub(crate) mod edts;
pub(crate) mod elst;
pub(crate) mod esds;
pub(crate) mod ftyp;
pub(crate) mod hdlr;
pub(crate) mod mdhd;
pub(crate) mod mdia;
pub(crate) mod minf;
pub(crate) mod moov;
pub(crate) mod mp4a;
pub(crate) mod mvhd;
pub(crate) mod smhd;
pub(crate) mod stbl;
pub(crate) mod stco;
pub(crate) mod stsc;
pub(crate) mod stsd;
pub(crate) mod stss;
pub(crate) mod stsz;
pub(crate) mod stts;
pub(crate) mod tkhd;
pub(crate) mod trak;

pub use co64::Co64Atom;
pub use ctts::CttsAtom;
pub use edts::EdtsAtom;
pub use elst::ElstAtom;
pub use esds::EsdsAtom;
pub use ftyp::FtypAtom;
pub use hdlr::HdlrAtom;
pub use mdhd::MdhdAtom;
pub use mdia::MdiaAtom;
pub use minf::MinfAtom;
pub use moov::MoovAtom;
pub use mp4a::Mp4aAtom;
pub use mvhd::MvhdAtom;
pub use smhd::SmhdAtom;
pub use stbl::StblAtom;
pub use stco::StcoAtom;
pub use stsc::StscAtom;
pub use stsd::StsdAtom;
pub use stss::StssAtom;
pub use stsz::StszAtom;
pub use stts::SttsAtom;
pub use tkhd::TkhdAtom;
pub use trak::TrakAtom;

#[derive(Copy, Clone, Debug)]
pub enum AtomType {
    Co64,
    Ctts,
    Edts,
    Elst,
    Esds,
    Free,
    Ftyp,
    Hdlr,
    Mdat,
    Mdhd,
    Mdia,
    Meta,
    Minf,
    Moof,
    Moov,
    Mp4a,
    Mvhd,
    Sidx,
    Smhd,
    Stbl,
    Stco,
    Stsc,
    Stsd,
    Stss,
    Stsz,
    Stts,
    Tkhd,
    Trak,
    Unsupported([u8; 4]),
}

impl From<[u8; 4]> for AtomType {
    fn from(val: [u8; 4]) -> Self {
        match &val {
            b"co64" => AtomType::Co64,
            b"ctts" => AtomType::Ctts,
            b"edts" => AtomType::Edts,
            b"elst" => AtomType::Elst,
            b"esds" => AtomType::Esds,
            b"free" => AtomType::Free,
            b"ftyp" => AtomType::Ftyp,
            b"hdlr" => AtomType::Hdlr,
            b"mdat" => AtomType::Mdat,
            b"mdhd" => AtomType::Mdhd,
            b"mdia" => AtomType::Mdia,
            b"meta" => AtomType::Meta,
            b"minf" => AtomType::Minf,
            b"moof" => AtomType::Moof,
            b"moov" => AtomType::Moov,
            b"mp4a" => AtomType::Mp4a,
            b"mvhd" => AtomType::Mvhd,
            b"sidx" => AtomType::Sidx,
            b"smhd" => AtomType::Smhd,
            b"stbl" => AtomType::Stbl,
            b"stco" => AtomType::Stco,
            b"stsc" => AtomType::Stsc,
            b"stsd" => AtomType::Stsd,
            b"stss" => AtomType::Stss,
            b"stsz" => AtomType::Stsz,
            b"stts" => AtomType::Stts,
            b"tkhd" => AtomType::Tkhd,
            b"trak" => AtomType::Trak,
            _       => AtomType::Unsupported(val)
        }
    }
}

/// Common atom header.
#[derive(Copy, Clone, Debug)]
pub struct AtomHeader {
    /// The atom type.
    pub atype: AtomType,
    /// The total size of the atom including the header.
    pub atom_len: u64,
    /// The size of the payload data.
    pub data_len: u64,
}

impl AtomHeader {
    const HEADER_SIZE: u64 = 8;
    const EXTENDED_HEADER_SIZE: u64 = AtomHeader::HEADER_SIZE + 8;

    /// Reads an atom header from the provided `ByteStream`.
    pub fn read<B: ByteStream>(reader: &mut B) -> Result<AtomHeader> {
        let mut atom_len = u64::from(reader.read_be_u32()?);
        let atype = AtomType::from(reader.read_quad_bytes()?);

        let data_len = match atom_len {
            0 => {
                0
            }
            1 => {
                atom_len = reader.read_be_u64()?;

                // The atom size should be atleast the length of the header.
                if atom_len < AtomHeader::EXTENDED_HEADER_SIZE {
                    return decode_error("atom size is invalid");
                }

                atom_len - AtomHeader::EXTENDED_HEADER_SIZE
            }
            _ => {
                // The atom size should be atleast the length of the header.
                if atom_len < AtomHeader::HEADER_SIZE {
                    dbg!(atom_len);
                    return decode_error("atom size is invalid");
                }

                atom_len - AtomHeader::HEADER_SIZE
            }
        };

        Ok(AtomHeader { atype, atom_len, data_len })
    }

    /// For applicable atoms, reads the atom header extra data: a tuple composed of a u8 version
    /// number, and a u24 bitset of flags.
    pub fn read_extra<B: ByteStream>(reader: &mut B) -> Result<(u8, u32)> {
        Ok((
            reader.read_u8()?,
            reader.read_be_u24()?,
        ))
    }
}

pub trait Atom : Sized {
    fn header(&self) -> AtomHeader;

    fn read<B: ByteStream>(reader: &mut B, header: AtomHeader) -> Result<Self>;
}

pub struct AtomIterator<'a, B: ByteStream> {
    reader: &'a mut B,
    len: Option<u64>,
    cur_atom: Option<AtomHeader>,
    base_pos: u64,
    next_atom_pos: u64,
}

impl<'a, B: ByteStream> AtomIterator<'a, B> {

    pub fn new_root(reader: &'a mut B, len: Option<u64>) -> Self {
        let base_pos = reader.pos();

        AtomIterator {
            reader,
            len,
            cur_atom: None,
            base_pos,
            next_atom_pos: base_pos,
        }
    }

    pub fn new(reader: &'a mut B, container: AtomHeader) -> Self {
        let base_pos = reader.pos();

        AtomIterator {
            reader,
            len: Some(container.data_len),
            cur_atom: None,
            base_pos,
            next_atom_pos: base_pos,
        }
    }

    pub fn inner(&self) -> &B {
        &self.reader
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
            return decode_error("overread atom");
        }

        // If len is specified, then do not read more than len bytes.
        if let Some(len) = self.len {
            if self.next_atom_pos - self.base_pos >= len {
                return Ok(None);
            }
        }

        // Read the next atom header.
        let atom = AtomHeader::read(self.reader)?;

        // Calculate the start position for the next atom (the exclusive end of the current atom).
        self.next_atom_pos += match atom.atom_len {
            0 => {
                // An atom with a length of zero is defined to span to the end of the stream. If
                // len is available, use it for the next atom start position, otherwise, use u64 max
                // which will trip an end of stream error on the next iteration.
                self.len.unwrap_or(std::u64::MAX) - self.next_atom_pos
            }
            len => len,
        };

        self.cur_atom = Some(atom);

        Ok(self.cur_atom)
    }

    pub fn read_atom<A: Atom>(&mut self) -> Result<A> {
        // It is not possible to read the current atom more than once because ByteStream is not
        // seekable. Therefore, raise an assert if read_atom is called more than once between calls
        // to next, or after next returns None.
        assert!(self.cur_atom.is_some());
        A::read(self.reader, self.cur_atom.take().unwrap())
    }

}