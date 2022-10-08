// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! An ID3v2 metadata reader.

use std::collections::HashMap;

use symphonia_core::errors::{decode_error, unsupported_error, Result};
use symphonia_core::io::*;
use symphonia_core::meta::{
    Chapter, MetadataBuilder, MetadataOptions, MetadataReader, MetadataRevision, StandardTagKey,
    TableOfContents, TableOfContentsItem, Value,
};
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};
use symphonia_core::support_metadata;

use log::{info, trace, warn};

mod frames;
mod unsync;

use frames::*;
use unsync::{read_syncsafe_leq32, UnsyncStream};

#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
struct Restrictions {
    tag_size: TagSizeRestriction,
    text_encoding: TextEncodingRestriction,
    text_field_size: TextFieldSize,
    image_encoding: ImageEncodingRestriction,
    image_size: ImageSizeRestriction,
}

#[derive(Debug)]
#[allow(dead_code)]
struct ExtendedHeader {
    /// ID3v2.3 only, the number of padding bytes.
    padding_size: Option<u32>,
    /// ID3v2.3+, a CRC32 checksum of the Tag.
    crc32: Option<u32>,
    /// ID3v2.4 only, is this Tag an update to an earlier Tag.
    is_update: Option<bool>,
    /// ID3v2.4 only, Tag modification restrictions.
    restrictions: Option<Restrictions>,
}

/// Read the header of an ID3v2 (verions 2.2+) tag.
fn read_id3v2_header<B: ReadBytes>(reader: &mut B) -> Result<Header> {
    let marker = reader.read_triple_bytes()?;

    if marker != *b"ID3" {
        return unsupported_error("id3v2: not an ID3v2 tag");
    }

    let major_version = reader.read_u8()?;
    let minor_version = reader.read_u8()?;
    let flags = reader.read_u8()?;
    let size = unsync::read_syncsafe_leq32(reader, 28)?;

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
        return decode_error("id3v2: invalid version number(s)");
    }

    // Only support versions 2.2.x (first version) to 2.4.x (latest version as of May 2019) of the
    // specification.
    if major_version < 2 || major_version > 4 {
        return unsupported_error("id3v2: unsupported ID3v2 version");
    }

    // Version 2.2 of the standard specifies a compression flag bit, but does not specify a
    // compression standard. Future versions of the standard remove this feature and repurpose this
    // bit for other features. Since there is no way to know how to handle the remaining tag data,
    // return an unsupported error.
    if major_version == 2 && (flags & 0x40) != 0 {
        return unsupported_error("id3v2: ID3v2.2 compression is not supported");
    }

    // With the exception of the compression flag in version 2.2, flags were added sequentially each
    // major version. Check each bit sequentially as they appear in each version.
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
fn read_id3v2p3_extended_header<B: ReadBytes>(reader: &mut B) -> Result<ExtendedHeader> {
    let size = reader.read_be_u32()?;
    let flags = reader.read_be_u16()?;
    let padding_size = reader.read_be_u32()?;

    if !(size == 6 || size == 10) {
        return decode_error("id3v2: invalid extended header size");
    }

    let mut header = ExtendedHeader {
        padding_size: Some(padding_size),
        crc32: None,
        is_update: None,
        restrictions: None,
    };

    // CRC32 flag.
    if size == 10 && flags & 0x8000 != 0 {
        header.crc32 = Some(reader.read_be_u32()?);
    }

    Ok(header)
}

/// Read the extended header of an ID3v2.4 tag.
fn read_id3v2p4_extended_header<B: ReadBytes>(reader: &mut B) -> Result<ExtendedHeader> {
    let _size = read_syncsafe_leq32(reader, 28)?;

    if reader.read_u8()? != 1 {
        return decode_error("id3v2: extended flags should have a length of 1");
    }

    let flags = reader.read_u8()?;

    let mut header = ExtendedHeader {
        padding_size: None,
        crc32: None,
        is_update: Some(false),
        restrictions: None,
    };

    // Tag is an update flag.
    if flags & 0x40 != 0x0 {
        let len = reader.read_u8()?;
        if len != 1 {
            return decode_error("id3v2: is update extended flag has invalid size");
        }

        header.is_update = Some(true);
    }

    // CRC32 flag.
    if flags & 0x20 != 0x0 {
        let len = reader.read_u8()?;
        if len != 5 {
            return decode_error("id3v2: CRC32 extended flag has invalid size");
        }

        header.crc32 = Some(read_syncsafe_leq32(reader, 32)?);
    }

    // Restrictions flag.
    if flags & 0x10 != 0x0 {
        let len = reader.read_u8()?;
        if len != 1 {
            return decode_error("id3v2: restrictions extended flag has invalid size");
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

fn read_id3v2_body<B: ReadBytes + FiniteStream>(
    reader: &mut B,
    header: &Header,
    metadata: &mut MetadataBuilder,
) -> Result<()> {
    // If there is an extended header, read and parse it based on the major version of the tag.
    if header.has_extended_header {
        let extended = match header.major_version {
            3 => read_id3v2p3_extended_header(reader)?,
            4 => read_id3v2p4_extended_header(reader)?,
            _ => unreachable!(),
        };
        trace!("{:#?}", &extended);
    }

    let min_frame_size = match header.major_version {
        2 => 6,
        3 | 4 => 10,
        _ => unreachable!(),
    };

    let mut chapters = HashMap::new();
    let mut tables_of_content = HashMap::new();
    let mut table_of_content = None;

    loop {
        // Read frames based on the major version of the tag.
        let frame = match header.major_version {
            2 => read_id3v2p2_frame(reader),
            3 => read_id3v2p3_frame(reader),
            4 => read_id3v2p4_frame(reader),
            _ => break,
        }?;

        match frame {
            // The padding has been reached, don't parse any further.
            FrameResult::Padding => break,
            // A frame was parsed into a tag, add it to the tag collection.
            FrameResult::Tag(tag) => {
                match tag.std_key {
                    Some(StandardTagKey::TableOfContents) => {
                        if let Value::Binary(value) = tag.value {
                            let mut reader = BufReader::new(value.as_ref());
                            let reader: &mut BufReader<'_> = &mut reader;
                            // the flags
                            // - bit 0 is the "ordered" bit
                            // - bit 1 is the "top-level" bit
                            let flags = reader.read_u8()?;
                            // The number of items in this table of contents
                            let entry_count = reader.read_u8()?;

                            let mut items = vec![];
                            for _ in 0..entry_count {
                                let data = reader.scan_bytes_aligned_ref(
                                    &[0x00],
                                    1,
                                    reader.bytes_available() as usize,
                                )?;
                                let name: String = data
                                    .iter()
                                    .filter(|&b| *b > 0x1f)
                                    .map(|&b| b as char)
                                    .collect();
                                items.push(name);
                            }

                            let mut tags = vec![];
                            while reader.bytes_available() > min_frame_size {
                                let frame = match header.major_version {
                                    2 => read_id3v2p2_frame(reader),
                                    3 => read_id3v2p3_frame(reader),
                                    4 => read_id3v2p4_frame(reader),
                                    _ => break,
                                }?;
                                match frame {
                                    FrameResult::MultipleTags(tag_list) => {
                                        tags.extend(tag_list.into_iter())
                                    }
                                    FrameResult::Tag(tag) => tags.push(tag),
                                    _ => {}
                                }
                            }
                            let toc = (
                                TableOfContents { items: vec![], ordered: flags & 1 == 1, tags },
                                items,
                            );
                            if (flags >> 1) & 1 == 1 {
                                table_of_content = Some(toc);
                            } else {
                                tables_of_content.insert(tag.key[5..].to_string(), toc);
                            }
                        } else {
                            unreachable!()
                        }
                    }
                    Some(StandardTagKey::Chapter) => {
                        if let Value::Binary(value) = tag.value {
                            let mut reader = BufReader::new(value.as_ref());
                            // start_time in ms
                            let start_ms = reader.read_be_u32()?;
                            // end_time in ms
                            let end_ms = reader.read_be_u32()?;
                            // start_byte
                            let start_byte = reader.read_be_u32()?;
                            // end_byte
                            let end_byte = reader.read_be_u32()?;

                            let mut tags = vec![];
                            while reader.bytes_available() > min_frame_size {
                                let frame = match header.major_version {
                                    2 => read_id3v2p2_frame(&mut reader),
                                    3 => read_id3v2p3_frame(&mut reader),
                                    4 => read_id3v2p4_frame(&mut reader),
                                    _ => break,
                                }?;
                                match frame {
                                    FrameResult::MultipleTags(tag_list) => {
                                        tags.extend(tag_list.into_iter())
                                    }
                                    FrameResult::Tag(tag) => tags.push(tag),
                                    _ => {}
                                }
                            }
                            chapters.insert(
                                tag.key[5..].to_string(),
                                Chapter { start_ms, end_ms, start_byte, end_byte, tags },
                            );
                        } else {
                            unreachable!()
                        }
                    }
                    _ => {
                        metadata.add_tag(tag);
                    }
                };
            }
            // A frame was parsed into multiple tags, add them all to the tag collection.
            FrameResult::MultipleTags(multi_tags) => {
                for tag in multi_tags {
                    metadata.add_tag(tag);
                }
            }
            // A frame was parsed into a visual, add it to the visual collection.
            FrameResult::Visual(visual) => {
                metadata.add_visual(visual);
            }
            // An unknown frame was encountered.
            FrameResult::UnsupportedFrame(ref id) => {
                info!("unsupported frame {}", id);
            }
            // The frame contained invalid data.
            FrameResult::InvalidData(ref id) => {
                warn!("invalid data for {} frame", id);
            }
        }

        // Read frames until there is not enough bytes available in the ID3v2 tag for another frame.
        if reader.bytes_available() < min_frame_size {
            break;
        }
    }

    // We need to fill out the table of contents items
    // Do a depth-first search of the table of contents, rooted at the base table of contents
    fn dfs_table_items(
        items: &Vec<String>,
        target: &mut Vec<TableOfContentsItem>,
        tables_of_content: &mut HashMap<String, (TableOfContents, Vec<String>)>,
        chapters: &mut HashMap<String, Chapter>,
    ) -> Vec<String> {
        // According to https://id3.org/id3v2-chapters-1.0#Notes there may be chapters that don't
        // have a related table of contents.
        // We can determine which chapters are not in any table of contents by keeping track of
        // those that _are_ in any given table of contents and removing it from the total list of
        // chapters later.
        let mut to_remove = vec![];
        for item in items {
            if let Some(chapter) = chapters.get(item) {
                to_remove.push(item.clone());
                target.push(TableOfContentsItem::Chapter(chapter.clone()));
                continue;
            }
            // We remove to prevent loops in tables of content
            // As an example of something that we don't want to happen:
            //    Toc1(root):
            //      - Chap1
            //      - Toc2
            //      - Toc3
            //    Toc2:
            //      - Chap2
            //      - Toc3
            //      - Chap3
            //    Toc3:
            //      - Chap5
            //      - Toc2
            //      - Chap4
            // There's nothing in the spec itself that says that such a layout isn't valid, but the
            // issue of parsing becomes much more difficult, and we'd have to move to using `Arc`
            // or `Rc` instead of a vector.
            //
            // With removal, the above tree becomes the following, as duplicate tocs are removed:
            //    Toc1(root):
            //      - Chap1
            //      - Toc2
            //    Toc2:
            //      - Chap2
            //      - Toc3
            //      - Chap3
            //    Toc3:
            //      - Chap5
            //      - Chap4
            if let Some((mut toc, sub_items)) = tables_of_content.remove(item) {
                to_remove.extend(dfs_table_items(
                    &sub_items,
                    &mut toc.items,
                    tables_of_content,
                    chapters,
                ));
                target.push(TableOfContentsItem::TableOfContents(toc));
                continue;
            }
        }
        to_remove
    }
    if let Some((mut toc, items)) = table_of_content {
        let to_remove =
            dfs_table_items(&items, &mut toc.items, &mut tables_of_content, &mut chapters);
        metadata.add_table_of_contents(toc);
        for chap in to_remove.into_iter() {
            chapters.remove(&chap);
        }
    }
    for chapter in chapters.into_values() {
        metadata.add_chapter(chapter);
    }

    Ok(())
}

pub fn read_id3v2<B: ReadBytes>(reader: &mut B, metadata: &mut MetadataBuilder) -> Result<()> {
    // Read the (sorta) version agnostic tag header.
    let header = read_id3v2_header(reader)?;

    // If the unsynchronisation flag is set in the header, all tag data must be passed through the
    // unsynchronisation decoder before being read for verions < 4 of ID3v2.
    let mut scoped = if header.unsynchronisation && header.major_version < 4 {
        let mut unsync = UnsyncStream::new(ScopedStream::new(reader, u64::from(header.size)));

        read_id3v2_body(&mut unsync, &header, metadata)?;

        unsync.into_inner()
    }
    // Otherwise, read the data as-is. Individual frames may be unsynchronised for major versions
    // >= 4.
    else {
        let mut scoped = ScopedStream::new(reader, u64::from(header.size));

        read_id3v2_body(&mut scoped, &header, metadata)?;

        scoped
    };

    // Ignore any remaining data in the tag.
    scoped.ignore()?;

    Ok(())
}

pub mod util {
    use symphonia_core::meta::StandardVisualKey;

    /// Try to get a `StandardVisualKey` from the APIC picture type identifier.
    pub fn apic_picture_type_to_visual_key(apic: u32) -> Option<StandardVisualKey> {
        match apic {
            0x01 => Some(StandardVisualKey::FileIcon),
            0x02 => Some(StandardVisualKey::OtherIcon),
            0x03 => Some(StandardVisualKey::FrontCover),
            0x04 => Some(StandardVisualKey::BackCover),
            0x05 => Some(StandardVisualKey::Leaflet),
            0x06 => Some(StandardVisualKey::Media),
            0x07 => Some(StandardVisualKey::LeadArtistPerformerSoloist),
            0x08 => Some(StandardVisualKey::ArtistPerformer),
            0x09 => Some(StandardVisualKey::Conductor),
            0x0a => Some(StandardVisualKey::BandOrchestra),
            0x0b => Some(StandardVisualKey::Composer),
            0x0c => Some(StandardVisualKey::Lyricist),
            0x0d => Some(StandardVisualKey::RecordingLocation),
            0x0e => Some(StandardVisualKey::RecordingSession),
            0x0f => Some(StandardVisualKey::Performance),
            0x10 => Some(StandardVisualKey::ScreenCapture),
            0x12 => Some(StandardVisualKey::Illustration),
            0x13 => Some(StandardVisualKey::BandArtistLogo),
            0x14 => Some(StandardVisualKey::PublisherStudioLogo),
            _ => None,
        }
    }
}

pub struct Id3v2Reader;

impl QueryDescriptor for Id3v2Reader {
    fn query() -> &'static [Descriptor] {
        &[support_metadata!("id3v2", "ID3v2", &[], &[], &[b"ID3"])]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}

impl MetadataReader for Id3v2Reader {
    fn new(_options: &MetadataOptions) -> Self {
        Id3v2Reader {}
    }

    fn read_all(&mut self, reader: &mut MediaSourceStream) -> Result<MetadataRevision> {
        let mut builder = MetadataBuilder::new();
        read_id3v2(reader, &mut builder)?;
        Ok(builder.metadata())
    }
}
