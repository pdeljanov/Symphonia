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

use sonata_core::errors::{Result, decode_error, unsupported_error};
use sonata_core::io::*;

#[derive(Debug)]
enum TagSizeRestriction {
    Max128Frames1024KiB,
    Max64Frames128KiB,
    Max32Frames40KiB,
    Max32Frames4KiB,
}

#[derive(Debug)]
enum TextEncodingRestriction {
    None,
    Utf8OrIso88591,
}

#[derive(Debug)]
enum TextFieldSize {
    None,
    Max1024Characters,
    Max128Characters,
    Max30Characters,
}

#[derive(Debug)]
enum ImageEncodingRestriction {
    None,
    PngOrJpegOnly,
}

#[derive(Debug)]
enum ImageSizeRestriction {
    None,
    LessThan256x256,
    LessThan64x64,
    Exactly64x64,
}

#[derive(Debug)]
struct Header {
    major_version: u8,
    minor_version: u8,
    size: u32,
    unsynchronisation: bool,
    has_extended_header: bool,
    experimental: bool,
    has_footer: bool,
}

#[derive(Debug)]
struct Restrictions {
    tag_size: TagSizeRestriction,
    text_encoding: TextEncodingRestriction,
    text_field_size: TextFieldSize,
    image_encoding: ImageEncodingRestriction,
    image_size: ImageSizeRestriction,
}

#[derive(Debug)]
struct ExtendedHeader {
    size: u32,
    is_update: bool,
    crc32: Option<u32>,
    restrictions: Option<Restrictions>,
}

fn read_syncsafe_leq32<B: Bytestream>(reader: &mut B, bit_width: u32) -> Result<u32> {
    debug_assert!(bit_width <= 32);

    let mut result = 0u32;
    let mut bits_read = 0;

    while bits_read < bit_width {
        bits_read += 7;
        result |= ((reader.read_u8()? & 0x7f) as u32) << (bit_width - bits_read);
    }

    Ok(result & (0xffffffff >> (32 - bit_width)))
}

/// Read the header of an ID3v2 (verions 2.2+) tag.
fn read_id3v2_header<B: Bytestream>(reader: &mut B) -> Result<Header> {
    let marker = reader.read_triple_bytes()?;

    if marker != *b"ID3" {
        return unsupported_error("Not an ID3 tag.");
    }

    let major_version = reader.read_u8()?;
    let minor_version = reader.read_u8()?;
    let flags = reader.read_u8()?;
    let size = read_syncsafe_leq32(reader, 28)?;

    let mut header = Header {
        major_version,
        minor_version,
        size,
        unsynchronisation: false,
        has_extended_header: false,
        experimental: false,
        has_footer: false,
    };

    // Major and minor version numbers should never equal 0xff as per the specification. 
    if major_version == 0xff || minor_version == 0xff {
        return decode_error("Invalid version number(s).");
    }

    // Only support versions 2.2.x (first version) to 2.4.x (latest version as of May 2019) of the specification.
    if major_version < 2 || major_version > 4 {
        return unsupported_error("Unsupported ID3v2.");
    }

    // Version 2.2 of the standard specifies a compression flag bit, but does not specify a compression standard. 
    // Future versions of the standard remove this feature and repurpose this bit for other features. Since there is
    // no way to know how to handle the remaining tag data, return an unsupported error.
    if major_version == 2 && (flags & 0x40) != 0 {
        return unsupported_error("ID3v2.2 compression is not supported.");
    }

    // With the exception of the compression flag in version 2.2, flags were added sequentially each major version.
    // Check each bit sequentially as they appear in each version.
    if major_version >= 2 {
        header.unsynchronisation = flags & 0x80 != 0;
    }

    if major_version >= 3 {
        header.has_extended_header = flags & 0x40 != 0;
        header.experimental = flags & 0x20 != 0;
    }

    if major_version >= 4 {
        header.has_footer = flags & 0x10 != 0;
    }

    Ok(header)
}

/// Read the extended header of an ID3v2.3 tag.
fn read_id3v2p3_extended_header<B: Bytestream>(reader: &mut B) -> Result<ExtendedHeader> {
    // Don't support until we can read unsychronisation streams.
    return unsupported_error("ID3v2.3 extended headers not supported.");
}

/// Read the extended header of an ID3v2.4 tag.
fn read_id3v2p4_extended_header<B: Bytestream>(reader: &mut B) -> Result<ExtendedHeader> {
    let size = read_syncsafe_leq32(reader, 28)?;
    
    if reader.read_u8()? != 1 {
        return decode_error("Extended flags should have a length of 1.");
    }

    let flags = reader.read_u8()?;

    let mut header = ExtendedHeader {
        size,
        is_update: false,
        crc32: None,
        restrictions: None,
    };

    // Tag is an update flag.
    if (flags & 0x40) == 0x40 {
        let len = reader.read_u8()?;
        if len != 1 {
            return decode_error("Is update extended flag has invalid size.");
        }

        header.is_update = true;
    }

    // CRC32 flag.
    if (flags & 0x20) == 0x20 {
        let len = reader.read_u8()?;
        if len != 5 {
            return decode_error("CRC32 extended flag has invalid size.");
        }

        header.crc32 = Some(read_syncsafe_leq32(reader, 32)?);
    }

    // Restrictions flag.
    if (flags & 0x10) == 0x10 {
        let len = reader.read_u8()?;
        if len != 1 {
            return decode_error("Restrictions extended flag has invalid size.");
        }

        let restrictions = reader.read_u8()?;

        let tag_size = match (restrictions & 0xc0) >> 6 {
            0 => TagSizeRestriction::Max128Frames1024KiB,
            1 => TagSizeRestriction::Max64Frames128KiB,
            2 => TagSizeRestriction::Max32Frames40KiB,
            3 => TagSizeRestriction::Max32Frames4KiB,
            _ => unreachable!(),
        };

        let text_encoding = match (restrictions & 0x40) >> 5 {
            0 => TextEncodingRestriction::None,
            1 => TextEncodingRestriction::Utf8OrIso88591,
            _ => unreachable!(),
        };

        let text_field_size = match (restrictions & 0x18) >> 3 {
            0 => TextFieldSize::None,
            1 => TextFieldSize::Max1024Characters,
            2 => TextFieldSize::Max128Characters,
            3 => TextFieldSize::Max30Characters,
            _ => unreachable!(),
        };

        let image_encoding = match (restrictions & 0x04) >> 2 {
            0 => ImageEncodingRestriction::None,
            1 => ImageEncodingRestriction::PngOrJpegOnly,
            _ => unreachable!(),
        };

        let image_size = match restrictions & 0x03 {
            0 => ImageSizeRestriction::None,
            1 => ImageSizeRestriction::LessThan256x256,
            2 => ImageSizeRestriction::LessThan64x64,
            3 => ImageSizeRestriction::Exactly64x64,
            _ => unreachable!(),
        };

        header.restrictions = Some(Restrictions {
            tag_size,
            text_encoding,
            text_field_size,
            image_encoding,
            image_size,
        })
    }

    Ok(header)
}

fn read_id3v2p2_frame<B: Bytestream>(reader: &mut B) -> Result<()> {
    let id = reader.read_triple_bytes()?;
    let size = reader.read_be_u24()?;

    if id == [0, 0, 0] {
        return Ok(());
    }

    eprintln!("Frame\t{}\t{}", String::from_utf8_lossy(&id), size);

    reader.ignore_bytes(size as u64)?;

    Ok(())
}

fn read_id3v2p3_frame<B: Bytestream>(reader: &mut B) -> Result<()> {
    let id = reader.read_quad_bytes()?;
    let size = reader.read_be_u32()?;
    let flags = reader.read_be_u16()?;

    if id == [0, 0, 0, 0] {
        return Ok(());
    }

    eprintln!("Frame\t{}\t{}\t{:#b}", String::from_utf8_lossy(&id), size, flags);

    reader.ignore_bytes(size as u64)?;

    Ok(())
}

fn read_id3v2p4_frame<B: Bytestream>(reader: &mut B) -> Result<()> {
    let id = reader.read_quad_bytes()?;
    let size = read_syncsafe_leq32(reader, 28)?;
    let flags = reader.read_be_u16()?;

    if id == [0, 0, 0, 0] {
        return Ok(());
    }

    eprintln!("Frame\t{}\t{}\t{:#b}", String::from_utf8_lossy(&id), size, flags);

    reader.ignore_bytes(size as u64)?;

    Ok(())
}

pub fn read_id3v2<B: Bytestream>(reader: &mut B) -> Result<()> {
    // Read the version agnostic tag header.
    let header = read_id3v2_header(reader)?;
    eprintln!("{:?}", &header);

    // The header specified the byte length of the contents of the ID3v2 tag (excluding the header), use a scoped
    // reader to ensure we don't exceed that length, and to determine if there are no more frames left to parse.
    let mut scoped = ScopedStream::new(reader, header.size as u64);

    // If the version is <= 3, and the unsynchronisation flag is set in the header, data must be passed through the 
    // unsynchronisation decoder before being read.
    if header.major_version <= 3 && header.unsynchronisation {

    }

    // If there is an extended header, read and parse it based on the major version of the tag.
    if header.has_extended_header {
        let extended = match header.major_version {
            3 => read_id3v2p3_extended_header(&mut scoped)?,
            4 => read_id3v2p4_extended_header(&mut scoped)?,
            _ => unreachable!(),
        };
        eprintln!("{:?}", &extended);
    }

    loop {
        // Read frames based on the major version of the tag.
        let frame = match header.major_version {
            2 => read_id3v2p2_frame(&mut scoped)?,
            3 => read_id3v2p3_frame(&mut scoped)?,
            4 => read_id3v2p4_frame(&mut scoped)?,
            _ => break,
        };

        // Read frames until either there are no more bytes available in the tag.
        if scoped.bytes_available() == 0 {
            break;
        }

        // Alternatively, if the padding bytes (all 0s) have been reached, skip the padding bytes as a block and 
        // exit.
        if false {
            scoped.ignore()?;
        }
    }

    Ok(())
}
