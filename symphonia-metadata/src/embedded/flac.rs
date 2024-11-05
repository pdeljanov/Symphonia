// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! FLAC metadata block reading.

use std::num::NonZeroU8;
use std::sync::Arc;

use symphonia_core::errors::{decode_error, Result};
use symphonia_core::io::ReadBytes;
use symphonia_core::meta::{MetadataBuilder, Size, StandardTag, Tag, Visual};

use crate::embedded::vorbis;
use crate::id3v2;
use crate::utils::images::try_get_image_info;

/// Converts a string of bytes to an ASCII string if all characters are within the printable ASCII
/// range. If a null byte is encounted, the string terminates at that point.
fn printable_ascii_to_string(bytes: &[u8]) -> Option<String> {
    let mut result = String::with_capacity(bytes.len());

    for c in bytes {
        match c {
            0x00 => break,
            0x20..=0x7e => result.push(char::from(*c)),
            _ => return None,
        }
    }

    Some(result)
}

/// Read a comment metadata block.
pub fn read_flac_comment_block<B: ReadBytes>(
    reader: &mut B,
    metadata: &mut MetadataBuilder,
) -> Result<()> {
    vorbis::read_vorbis_comment(reader, metadata)
}

/// Read a picture metadata block.
pub fn read_flac_picture_block<B: ReadBytes>(
    reader: &mut B,
    builder: &mut MetadataBuilder,
) -> Result<()> {
    let type_enc = reader.read_be_u32()?;

    // Read the Media Type length in bytes.
    // TODO: Apply a limit.
    let media_type_len = reader.read_be_u32()? as usize;

    // Read the Media Type bytes
    let media_type_buf = reader.read_boxed_slice_exact(media_type_len)?;

    // Convert Media Type bytes to an ASCII string. Non-printable ASCII characters are invalid.
    let media_type = match printable_ascii_to_string(&media_type_buf) {
        Some(s) => {
            // Return None if the media-type string is empty.
            Some(s).filter(|s| !s.is_empty())
        }
        None => return decode_error("meta (flac): picture mime-type contains invalid characters"),
    };

    let mut tags = vec![];

    // Read the description length in bytes.
    // TODO: Apply a limit.
    let desc_len = reader.read_be_u32()? as usize;

    // Read the description bytes.
    let desc_buf = reader.read_boxed_slice_exact(desc_len)?;

    // Convert to a UTF-8 string.
    let desc = String::from_utf8_lossy(&desc_buf);

    if !desc.is_empty() {
        let desc = Arc::new(desc.into_owned());
        let tag =
            Tag::new_from_parts("DESCRIPTION", desc.clone(), Some(StandardTag::Description(desc)));

        tags.push(tag);
    }

    // Read the width, and height of the visual.
    let width = reader.read_be_u32()?;
    let height = reader.read_be_u32()?;

    // If either the width or height is 0, then the size is invalid.
    let dimensions = if width > 0 && height > 0 { Some(Size { width, height }) } else { None };

    // Read bits-per-pixel of the visual.
    let _bits_per_pixel = NonZeroU8::new(reader.read_be_u32()? as u8);

    // Indexed colours is only valid for image formats that use an indexed colour palette. If it is
    // 0, the image does not used indexed colours.
    let _color_mode = reader.read_be_u32()?;

    // Read the image data length in bytes.
    // TODO: Apply a limit.
    let data_len = reader.read_be_u32()? as usize;

    // Read the image data.
    let data = reader.read_boxed_slice_exact(data_len)?;

    // Try to detect the image characteristics from the image data. Detect image characteristics
    // will be preferred over what's been stated in the picture block.
    let image_info = try_get_image_info(&data);

    builder.add_visual(Visual {
        media_type: image_info.as_ref().map(|info| info.media_type.clone()).or(media_type),
        dimensions: image_info.as_ref().map(|info| info.dimensions).or(dimensions),
        color_mode: image_info.as_ref().map(|info| info.color_mode),
        usage: id3v2::util::apic_picture_type_to_visual_key(type_enc),
        tags,
        data,
    });

    Ok(())
}
