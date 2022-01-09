// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::{Seek, SeekFrom};

use symphonia_core::errors::{decode_error, Error, Result};
use symphonia_core::io::ReadBytes;
use symphonia_core::util::bits::sign_extend_leq64_to_i64;

use crate::element_ids::{ELEMENTS, ElementType, Type};
use crate::segment::EbmlHeaderElement;

/// Reads a single EBML element ID (as in RFC8794) from the stream
/// and returns it or an error.
pub(crate) fn read_tag<R: ReadBytes>(reader: R) -> Result<u32> {
    Ok(read_vint::<R, false>(reader)?.0 as u32)
}

/// Reads a single unsigned variable size integer (as in RFC8794) from the stream
/// and returns it or an error.
pub(crate) fn read_unsigned_vint<R: ReadBytes>(reader: R) -> Result<u64> {
    Ok(read_vint::<R, true>(reader)?.0)
}

/// Reads a single signed variable size integer (as in RFC8794) from the stream
/// and returns it or an error.
pub(crate) fn read_signed_vint<R: ReadBytes>(mut reader: R) -> Result<i64> {
    let (value, len) = read_vint::<_, true>(&mut reader)?;
    // Convert to a signed integer by range shifting.
    let half_range = i64::pow(2, (len * 7) as u32 - 1) - 1;
    Ok(value as i64 - half_range)
}

/// Reads a single unsigned variable size integer (as in RFC8794) from the stream
/// and returns both its value and length in octects, or an error.
fn read_vint<R: ReadBytes, const CLEAR_MARKER: bool>(mut reader: R) -> Result<(u64, u32)> {
    loop {
        let byte = reader.read_byte()?;
        if byte == 0x00 || byte == 0xFF {
            // Skip invalid data
            continue;
        }

        let vint_width = byte.leading_zeros();
        let mut vint = byte as u64;
        if CLEAR_MARKER {
            // Clear VINT_MARKER bit
            vint ^= 1 << (7 - vint_width);
        }

        // Read remaining octets
        for _ in 0..vint_width {
            let byte = reader.read_byte()?;
            vint = (vint << 8) | byte as u64;
        }

        return Ok((vint, vint_width + 1));
    }
}

#[cfg(test)]
mod tests {
    use symphonia_core::io::BufReader;

    use super::{read_signed_vint, read_tag, read_unsigned_vint};

    #[test]
    fn element_tag_parsing() {
        assert_eq!(read_tag(BufReader::new(&[0x82])).unwrap(), 0x82);
        assert_eq!(read_tag(BufReader::new(&[0x40, 0x02])).unwrap(), 0x4002);
        assert_eq!(read_tag(BufReader::new(&[0x20, 0x00, 0x02])).unwrap(), 0x200002);
        assert_eq!(read_tag(BufReader::new(&[0x10, 0x00, 0x00, 0x02])).unwrap(), 0x10000002);
    }

    #[test]
    fn variable_unsigned_integer_parsing() {
        assert_eq!(read_unsigned_vint(BufReader::new(&[0x82])).unwrap(), 2);
        assert_eq!(read_unsigned_vint(BufReader::new(&[0x40, 0x02])).unwrap(), 2);
        assert_eq!(read_unsigned_vint(BufReader::new(&[0x20, 0x00, 0x02])).unwrap(), 2);
        assert_eq!(read_unsigned_vint(BufReader::new(&[0x10, 0x00, 0x00, 0x02])).unwrap(), 2);
        assert_eq!(read_unsigned_vint(BufReader::new(&[0x08, 0x00, 0x00, 0x00, 0x02])).unwrap(), 2);
        assert_eq!(read_unsigned_vint(BufReader::new(&[0x04, 0x00, 0x00, 0x00, 0x00, 0x02])).unwrap(), 2);
        assert_eq!(read_unsigned_vint(BufReader::new(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02])).unwrap(), 2);
        assert_eq!(read_unsigned_vint(BufReader::new(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02])).unwrap(), 2);
    }

    #[test]
    fn variable_signed_integer_parsing() {
        assert_eq!(read_signed_vint(BufReader::new(&[0x80])).unwrap(), -63);
        assert_eq!(read_signed_vint(BufReader::new(&[0x40, 0x00])).unwrap(), -8191);
    }
}

#[derive(Copy, Clone, Debug)]
pub struct ElementHeader {
    /// The element tag.
    pub tag: u32,
    /// The element type.
    pub etype: ElementType,
    /// The element's offset in the stream.
    pub pos: u64,
    /// The total size of the element including the header.
    pub len: u64,
    /// The element's data offset in the stream.
    pub data_pos: u64,
    /// The size of the payload data.
    pub data_len: u64,
}

impl ElementHeader {
    /// Returns an iterator over child elements of the current element.
    pub(crate) fn children<R: ReadBytes>(&self, reader: R) -> ElementIterator<R> {
        assert_eq!(reader.pos(), self.data_pos, "unexpected position");
        ElementIterator::new_of(reader, *self)
    }
}

pub trait Element: Sized {
    const ID: ElementType;
    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self>;
}

impl ElementHeader {
    /// Reads a single EBML element header from the stream.
    pub(crate) fn read<R: ReadBytes>(mut reader: &mut R) -> Result<ElementHeader> {
        let header_start = reader.pos();
        let tag = read_tag(&mut reader)?;
        let size = read_unsigned_vint(&mut reader)?;
        log::debug!("found element with tag: {:X}", tag);
        Ok(ElementHeader {
            tag,
            etype: ELEMENTS.get(&tag).map_or(ElementType::Unknown, |(_, etype)| *etype),
            pos: header_start,
            len: reader.pos() - header_start + size,
            data_len: size,
            data_pos: reader.pos(),
        })
    }
}

#[derive(Debug)]
pub(crate) struct EbmlElement {
    pub(crate) header: EbmlHeaderElement,
}

impl Element for EbmlElement {
    const ID: ElementType = ElementType::Ebml;

    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self> {
        let mut it = header.children(reader);
        Ok(Self {
            header: it.read_element_data::<EbmlHeaderElement>()?
        })
    }
}

pub(crate) struct ElementIterator<R: ReadBytes> {
    /// Reader of the stream containing this element.
    reader: R,
    /// Store current element header (for sanity check purposes).
    current: Option<ElementHeader>,
    /// Position of the next element header that would be read.
    next_pos: u64,
    /// Position immediately past last byte of this element.
    end: Option<u64>,
}

impl<R: ReadBytes> ElementIterator<R> {
    /// Creates a new iterator over elements starting from the current stream position.
    pub(crate) fn new(reader: R, end: Option<u64>) -> Self {
        let pos = reader.pos();
        Self::new_at(reader, pos, end)
    }

    /// Creates a new iterator over elements starting from the given stream position.
    fn new_at(reader: R, start: u64, end: Option<u64>) -> Self {
        Self {
            reader,
            current: None,
            next_pos: start,
            end,
        }
    }

    /// Creates a new iterator over children of the given parent element.
    fn new_of(reader: R, parent: ElementHeader) -> Self {
        Self {
            reader,
            current: Some(parent),
            next_pos: parent.data_pos,
            end: Some(parent.data_pos + parent.data_len),
        }
    }

    /// Seek to a specified offset inside of the stream.
    pub(crate) fn seek(&mut self, pos: u64) -> Result<()> where R: Seek {
        self.current = None;
        self.reader.seek(SeekFrom::Start(pos))?;
        self.next_pos = pos;
        Ok(())
    }

    /// Consumes this iterator and return the original stream.
    pub(crate) fn into_inner(self) -> R {
        self.reader
    }

    /// Reads a single element header and moves to its next sibling by ignoring all the children.
    pub(crate) fn read_header(&mut self) -> Result<Option<ElementHeader>> {
        let header = self.read_header_no_consume()?;
        if let Some(header) = &header {
            // Move to next sibling.
            self.next_pos += header.len;
        }
        Ok(header)
    }

    /// Reads a single element header and shifts the stream to element's child
    /// if it'a a master element or to next sibling otherwise.
    pub(crate) fn read_child_header(&mut self) -> Result<Option<ElementHeader>> {
        let header = self.read_header_no_consume()?;
        if let Some(header) = &header {
            match ELEMENTS.get(&header.tag).map(|it| it.0) {
                Some(Type::Master) => {
                    // Move to start of a child element.
                    self.next_pos = header.data_pos;
                }
                _ => {
                    // Move to next sibling.
                    self.next_pos += header.len;
                }
            }
        }
        Ok(header)
    }

    /// Reads element header at the current stream position
    /// without moving to the end of the parent element.
    /// Returns [None] if the current element has no more children or reached end of the stream.
    fn read_header_no_consume(&mut self) -> Result<Option<ElementHeader>> {
        let pos = self.reader.pos();
        if pos < self.next_pos {
            // Ignore bytes that were not read
            self.reader.ignore_bytes(self.next_pos - pos)?;
        }

        assert_eq!(self.next_pos, self.reader.pos(), "invalid position");

        if self.reader.pos() < self.end.unwrap_or(u64::MAX) {
            let header = ElementHeader::read(&mut self.reader)?;
            self.current = Some(header);
            return Ok(Some(header));
        }

        Ok(None)
    }

    /// Reads a single element with its data.
    pub(crate) fn read_element<E: Element>(&mut self) -> Result<E> {
        let _header = self.read_header()?;
        self.read_element_data()
    }

    /// Reads data of current element. Must be used after
    /// [Self::read_header] or [Self::read_child_header].
    pub(crate) fn read_element_data<E: Element>(&mut self) -> Result<E> {
        let header = match self.current {
            Some(header) => header,
            None => {
                let header = ElementHeader::read(&mut self.reader)?;
                self.current = Some(header);
                header
            }
        };
        assert_eq!(header.etype, E::ID, "reading invalid element");
        let element = E::read(&mut self.reader, header)?;
        // Update position to match the position element reader finished at
        self.next_pos = self.reader.pos();
        Ok(element)
    }

    /// Reads a collection of element with the given type.
    pub(crate) fn read_elements<E: Element>(&mut self) -> Result<Box<[E]>> {
        let mut elements = vec![];
        while let Some(header) = self.read_header()? {
            if header.etype == ElementType::Crc32 {
                // TODO: ignore crc for now
                continue;
            }
            assert_eq!(header.etype, E::ID);
            elements.push(E::read(&mut self.reader, header)?);
        }
        Ok(elements.into_boxed_slice())
    }

    /// Reads any primitive data inside of the current element.
    pub(crate) fn read_data(&mut self) -> Result<ElementData> {
        let hdr = self.current.expect("not in an element");
        let value = self.try_read_data(hdr)?
            .ok_or(Error::DecodeError("mkv: element has no primitive data"))?;
        Ok(value)
    }

    /// Reads data of the current element as an unsigned integer.
    pub(crate) fn read_u64(&mut self) -> Result<u64> {
        match self.read_data()? {
            ElementData::UnsignedInt(s) => Ok(s),
            _ => Err(Error::DecodeError("mkv: expected an unsigned int")),
        }
    }

    /// Reads data of the current element as a floating-point number.
    pub(crate) fn read_f64(&mut self) -> Result<f64> {
        match self.read_data()? {
            ElementData::Float(s) => Ok(s),
            _ => Err(Error::DecodeError("mkv: expected a float")),
        }
    }

    /// Reads data of the current element as a string.
    pub(crate) fn read_string(&mut self) -> Result<String> {
        match self.read_data()? {
            ElementData::String(s) => Ok(s),
            _ => Err(Error::DecodeError("mkv: expected a string")),
        }
    }

    /// Reads binary data of the current element as boxed slice.
    pub(crate) fn read_boxed_slice(&mut self) -> Result<Box<[u8]>> {
        match self.read_data()? {
            ElementData::Binary(b) => Ok(b),
            _ => Err(Error::DecodeError("mkv: expected binary data")),
        }
    }

    /// Reads any primitive data of the current element. It returns [None]
    /// if the it is a master element.
    pub(crate) fn try_read_data(&mut self, header: ElementHeader) -> Result<Option<ElementData>> {
        Ok(match ELEMENTS.get(&header.tag) {
            Some((ty, _)) => {
                assert_eq!(header.data_pos, self.reader.pos(), "invalid stream position");
                if let (Some(cur), Some(end)) = (self.current, self.end) {
                    assert!(cur.pos + cur.len <= end, "invalid stream position");
                }
                Some(match ty {
                    Type::Master => {
                        return Ok(None);
                    }
                    Type::Unsigned => {
                        if header.data_len > 8 {
                            self.ignore_data()?;
                            return decode_error("mkv: invalid unsigned integer length");
                        }

                        let mut buff = [0u8; 8];
                        let offset = 8 - header.data_len as usize;
                        self.reader.read_buf_exact(&mut buff[offset..])?;
                        let value = u64::from_be_bytes(buff);
                        ElementData::UnsignedInt(value)
                    }
                    Type::Signed | Type::Date => {
                        if header.data_len > 8 {
                            self.ignore_data()?;
                            return decode_error("mkv: invalid signed integer length");
                        }

                        let len = header.data_len as usize;
                        let mut buff = [0u8; 8];
                        self.reader.read_buf_exact(&mut buff[8 - len..])?;
                        let value = u64::from_be_bytes(buff);
                        let value = sign_extend_leq64_to_i64(value, (len as u32) * 8);

                        match ty {
                            Type::Signed => ElementData::SignedInt(value),
                            Type::Date => ElementData::Date(value),
                            _ => unreachable!(),
                        }
                    }
                    Type::Float => {
                        let value = match header.data_len {
                            0 => 0.0,
                            4 => self.reader.read_be_f32()? as f64,
                            8 => self.reader.read_be_f64()?,
                            _ => {
                                self.ignore_data()?;
                                return Err(Error::DecodeError("mkv: invalid float length"));
                            }
                        };
                        ElementData::Float(value)
                    }
                    Type::String => {
                        let data = self.reader.read_boxed_slice_exact(header.data_len as usize)?;
                        let bytes = data.split(|b| *b == 0).next().unwrap_or(&data);
                        ElementData::String(String::from_utf8_lossy(bytes).into_owned())
                    }
                    Type::Binary => {
                        ElementData::Binary(self.reader.read_boxed_slice_exact(header.data_len as usize)?)
                    }
                })
            }
            None => None,
        })
    }

    /// Ignores content of the current element. It can be used after calling
    /// [Self::read_child_header] to ignore children of a master element.
    pub(crate) fn ignore_data(&mut self) -> Result<()> {
        if let Some(header) = self.current {
            log::debug!("ignoring data of {:?} element", header.etype);
            self.reader.ignore_bytes(header.data_len)?;
            self.next_pos = header.data_pos + header.data_len;
        }
        Ok(())
    }

    /// Gets the position of the underlying stream.
    pub(crate) fn pos(&self) -> u64 {
        self.reader.pos()
    }
}

/// An EBML element data.
#[derive(Clone, Debug)]
pub(crate) enum ElementData {
    /// A binary buffer.
    Binary(Box<[u8]>),
    /// A floating point number.
    Float(f64),
    /// A signed integer.
    SignedInt(i64),
    /// A string.
    String(String),
    /// An unsigned integer.
    UnsignedInt(u64),
    /// A point in time referenced in nanoseconds from the precise beginning
    /// of the third millennium of the Gregorian Calendar in Coordinated Universal Time
    /// (also known as 2001-01-01T00:00:00.000000000 UTC).
    Date(i64),
}
