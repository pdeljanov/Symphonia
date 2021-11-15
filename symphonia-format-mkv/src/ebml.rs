use std::convert::TryFrom;
use std::io::{Read, Seek, SeekFrom};

use symphonia_core::errors::{Error, Result};
use symphonia_core::io::ReadBytes;
use symphonia_core::meta::Value;
use symphonia_core::util::bits::sign_extend_leq64_to_i64;

use crate::{EbmlHeaderElement, read_children};
use crate::element_ids::{ELEMENTS, ElementType, Type};
use crate::element_ids::Type::Master;

/// Parses a variable size integer according to RFC8794 (4)
pub(crate) fn read_vint<R: ReadBytes, const CLEAR_MARKER: bool>(mut reader: R) -> Result<u64> {
    loop {
        let byte = reader.read_byte()?;
        let vint_width = byte.leading_zeros();
        if vint_width == 8 {
            // Skip invalid data
            continue;
        }

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

#[cfg(test)]
mod tests {
    use symphonia_core::io::BufReader;

    use super::read_vint;

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
}

#[derive(Copy, Clone, Debug)]
pub struct ElementHeader {
    /// The element type.
    pub etype: ElementType,
    /// The total size of the element including the header.
    pub element_len: u64,
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

        Ok(ElementHeader {
            etype: ELEMENTS.iter()
                .find_map(|it| (it.0 == tag).then(|| it.2))
                .unwrap_or(ElementType::Void),
            element_len: reader.pos() - header_start + size,
            data_len: size,
            data_pos: reader.pos(),
        })
    }
}

#[derive(Debug)]
pub(crate) struct EbmlElement {
    header: EbmlHeaderElement,
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
            self.next_pos += header.element_len;
        }
        Ok(header)
    }

    #[track_caller]
    pub(crate) fn read_child_header(&mut self) -> Result<Option<ElementHeader>> {
        let mut header = self.read_header_no_consume()?;
        if let Some(header) = &mut header {
            // FIXME
            if let Some(Type::Master) = ELEMENTS.iter().find_map(|it| (it.2 == header.etype).then(|| it.1)) {
                self.next_pos = header.data_pos;
            } else {
                self.next_pos += header.element_len;
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
            assert_eq!(header.etype, E::ID);
            elements.push(E::read(&mut self.reader, header)?);
        }
        Ok(elements.into_boxed_slice())
    }

    pub(crate) fn read_value(&mut self) -> Result<Value> {
        let hdr = self.current.unwrap();
        let value = get_value(&mut self.reader, hdr)?.unwrap();
        Ok(value)
    }
}

pub(crate) fn get_value<R: ReadBytes>(mut reader: R, header: ElementHeader) -> Result<Option<Value>> {
    Ok(match ELEMENTS.iter().find(|it| it.2 == header.etype) {
        Some((_, ty, etype)) => {
            assert_eq!(header.data_pos, reader.pos());
            Some(match ty {
                Type::Master => {
                    return Ok(None);
                }
                Type::Unsigned => {
                    assert!(header.data_len <= 8);

                    let mut buff = [0u8; 8];
                    let offset = 8 - header.data_len as usize;
                    reader.read_buf_exact(&mut buff[offset..])?;
                    let value = u64::from_be_bytes(buff);
                    Value::UnsignedInt(value)
                }
                Type::Signed => {
                    assert!(header.data_len <= 8);
                    let len = header.data_len as usize;
                    let mut buff = [0u8; 8];
                    reader.read_buf_exact(&mut buff[8 - len..])?;
                    let value = u64::from_be_bytes(buff);
                    Value::SignedInt(sign_extend_leq64_to_i64(value, (len as u32) * 8));
                }
                Type::Float => {
                    let value = match header.data_len {
                        0 => 0.0,
                        4 => reader.read_be_f32()? as f64,
                        8 => reader.read_be_f64()?,
                        _ => return Err(Error::DecodeError("mkv: invalid float length")),
                    };
                    Value::Float(value)
                }
                Type::Unknown => {
                    Value::Binary(reader.read_boxed_slice_exact(header.data_len as usize)?)
                }
                Type::Date => todo!(),
                Type::String => {
                    let mut v = vec![0u8; header.data_len as usize];
                    reader.read_buf_exact(&mut v)?;
                    let s = v.split(|b| *b == 0).next().unwrap_or(&v);
                    Value::String(std::str::from_utf8(&s).unwrap().to_string())
                }
                Type::Binary => {
                    Value::Binary(reader.read_boxed_slice_exact(header.data_len as usize)?)
                }
            })
        }
        None => None,
    })
}
