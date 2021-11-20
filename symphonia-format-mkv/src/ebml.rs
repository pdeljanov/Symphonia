use symphonia_core::errors::{Error, Result};
use symphonia_core::io::ReadBytes;
use symphonia_core::util::bits::sign_extend_leq64_to_i64;

use crate::segment::EbmlHeaderElement;
use crate::element_ids::{ELEMENTS, ElementType, Type};

/// Parses a variable size integer according to RFC8794 (4)
pub(crate) fn read_vint<R: ReadBytes, const CLEAR_MARKER: bool>(mut reader: R) -> Result<u64> {
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

        return Ok(vint);
    }
}

/// Parses a variable size integer according to RFC8794 (4)
/// and converts it to a signed integer by shifting range.
pub(crate) fn read_vint_signed<R: ReadBytes>(mut reader: R) -> Result<i64> {
    // TODO: cleanup
    let before = reader.pos();
    let value = read_vint::<_, true>(&mut reader)?;
    let after = reader.pos();
    let len = after - before;
    Ok(value as i64 - (i64::pow(2, (len * 7) as u32 - 1) - 1))
}

#[cfg(test)]
mod tests {
    use symphonia_core::io::BufReader;
    use super::{read_vint_signed, read_vint};

    #[test]
    fn variable_integer_parsing() {
        assert_eq!(read_vint::<_, true>(BufReader::new(&[0x82])).unwrap(), 2);
        assert_eq!(read_vint::<_, true>(BufReader::new(&[0x40, 0x02])).unwrap(), 2);
        assert_eq!(read_vint::<_, true>(BufReader::new(&[0x20, 0x00, 0x02])).unwrap(), 2);
        assert_eq!(read_vint::<_, true>(BufReader::new(&[0x10, 0x00, 0x00, 0x02])).unwrap(), 2);

        assert_eq!(read_vint::<_, false>(BufReader::new(&[0x82])).unwrap(), 0x82);
        assert_eq!(read_vint::<_, false>(BufReader::new(&[0x40, 0x02])).unwrap(), 0x4002);
        assert_eq!(read_vint::<_, false>(BufReader::new(&[0x20, 0x00, 0x02])).unwrap(), 0x200002);
        assert_eq!(read_vint::<_, false>(BufReader::new(&[0x10, 0x00, 0x00, 0x02])).unwrap(), 0x10000002);
    }

    #[test]
    fn variable_signed_integer_parsing() {
        assert_eq!(read_vint_signed(BufReader::new(&[0x80])).unwrap(), -63);
        assert_eq!(read_vint_signed(BufReader::new(&[0x40, 0x00])).unwrap(), -8191);
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
    pub(crate) fn children<R: ReadBytes>(&self, mut reader: R) -> ElementIterator<R> {
        assert_eq!(reader.pos(), self.data_pos, "unexpected position");
        ElementIterator::new_of(reader, *self)
    }
}

pub trait Element: Sized {
    const ID: ElementType;
    fn read<B: ReadBytes>(reader: &mut B, header: ElementHeader) -> Result<Self>;
}

impl ElementHeader {
    pub(crate) fn read<R: ReadBytes>(mut reader: &mut R) -> Result<ElementHeader> {
        let header_start = reader.pos();
        let tag = read_vint::<_, false>(&mut reader)? as u32;
        let size = read_vint::<_, true>(&mut reader)?;
        log::debug!("found element with tag: {:X}", tag);
        Ok(ElementHeader {
            tag: tag,
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
    reader: R,
    /// Store current element header (for sanity check purposes)
    current: Option<ElementHeader>,
    next_pos: u64,
    end: Option<u64>,
}

impl<R: ReadBytes> ElementIterator<R> {
    pub(crate) fn new(reader: R) -> Self {
        let pos = reader.pos();
        Self::new_at(reader, pos)
    }

    pub(crate) fn new_at(reader: R, offset: u64) -> Self {
        Self {
            reader,
            current: None,
            next_pos: offset,
            end: None,
        }
    }

    pub(crate) fn new_of(reader: R, parent: ElementHeader) -> Self {
        Self {
            next_pos: parent.data_pos,
            reader,
            current: Some(parent),
            end: Some(parent.data_pos + parent.data_len),
        }
    }

    pub(crate) fn into_inner(self) -> R {
        self.reader
    }

    #[track_caller]
    pub(crate) fn read_header(&mut self) -> Result<Option<ElementHeader>> {
        let mut header = self.read_header_no_consume()?;
        if let Some(header) = &mut header {
            self.next_pos += header.len;
        }
        Ok(header)
    }

    #[track_caller]
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

    /// Read element header at the current stream position
    /// without moving to the end of the whole element.
    #[track_caller]
    fn read_header_no_consume(&mut self) -> Result<Option<ElementHeader>> {
        let pos = self.reader.pos();
        if pos < self.next_pos {
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

    #[track_caller]
    pub(crate) fn read_element<E: Element>(&mut self) -> Result<E> {
        let header = self.read_header()?;
        self.read_element_data()
    }

    #[track_caller]
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

    #[track_caller]
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

    pub(crate) fn read_data(&mut self) -> Result<ElementData> {
        let hdr = self.current.unwrap();
        let value = self.try_read_data(hdr)?
            .ok_or_else(|| Error::DecodeError("mkv: element has no primitive data"))?;
        Ok(value)
    }

    pub(crate) fn read_u64(&mut self) -> Result<u64> {
        match self.read_data()? {
            ElementData::UnsignedInt(s) => Ok(s),
            _ => Err(Error::DecodeError("mkv: expected u64")),
        }
    }

    pub(crate) fn read_f64(&mut self) -> Result<f64> {
        match self.read_data()? {
            ElementData::Float(s) => Ok(s),
            _ => Err(Error::DecodeError("mkv: expected f64")),
        }
    }

    pub(crate) fn read_string(&mut self) -> Result<String> {
        match self.read_data()? {
            ElementData::String(s) => Ok(s),
            _ => Err(Error::DecodeError("mkv: expected string")),
        }
    }

    pub(crate) fn read_boxed_slice(&mut self) -> Result<Box<[u8]>> {
        match self.read_data()? {
            ElementData::Binary(b) => Ok(b),
            _ => Err(Error::DecodeError("mkv: expected binary")),
        }
    }

    pub(crate) fn try_read_data(&mut self, header: ElementHeader) -> Result<Option<ElementData>> {
        Ok(match ELEMENTS.get(&header.tag) {
            Some((ty, _)) => {
                assert_eq!(header.data_pos, self.reader.pos());
                if let (Some(cur), Some(end)) = (self.current, self.end) {
                    assert!(cur.pos + cur.len <= end);
                }
                Some(match ty {
                    Type::Master => {
                        return Ok(None);
                    }
                    Type::Unsigned => {
                        assert!(header.data_len <= 8);

                        let mut buff = [0u8; 8];
                        let offset = 8 - header.data_len as usize;
                        self.reader.read_buf_exact(&mut buff[offset..])?;
                        let value = u64::from_be_bytes(buff);
                        ElementData::UnsignedInt(value)
                    }
                    Type::Signed | Type::Date => {
                        assert!(header.data_len <= 8);
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
                            _ => return Err(Error::DecodeError("mkv: invalid float length")),
                        };
                        ElementData::Float(value)
                    }
                    Type::Unknown => {
                        ElementData::Binary(self.reader.read_boxed_slice_exact(header.data_len as usize)?)
                    }
                    Type::String => {
                        let data = self.reader.read_boxed_slice_exact(header.data_len as usize)?;
                        let bytes = data.split(|b| *b == 0).next().unwrap_or(&data);
                        ElementData::String(String::from_utf8_lossy(&bytes).into_owned())
                    }
                    Type::Binary => {
                        ElementData::Binary(self.reader.read_boxed_slice_exact(header.data_len as usize)?)
                    }
                })
            }
            None => None,
        })
    }

    pub(crate) fn ignore_data(&mut self) -> Result<()> {
        if let Some(header) = self.current {
            log::warn!("ignoring data of {:?} element", header.etype);
            self.reader.ignore_bytes(header.data_len)?;
            self.next_pos = header.data_pos + header.data_len;
        }
        Ok(())
    }

    pub(crate) fn pos(&self) -> u64 {
        self.reader.pos()
    }
}

/// An EBML element data.
#[derive(Clone, Debug)]
pub(crate) enum ElementData {
    /// A binary buffer.
    Binary(Box<[u8]>),
    /// A boolean value.
    Boolean(bool),
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
