// Sonata
// Copyright (c) 2019 The Sonata Project Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::ascii;
use std::num::NonZeroU32;

use sonata_core::audio::Channels;
use sonata_core::errors::{Result, decode_error};
use sonata_core::formats::{Cue, CuePoint, SeekIndex};
use sonata_core::io::*;
use sonata_core::meta::{ColorMode, MetadataBuilder, Size, StandardTagKey, Tag, VendorData, Visual};

use sonata_metadata::{id3v2, vorbis};

pub enum MetadataBlockType {
    StreamInfo,
    Padding,
    Application,
    SeekTable,
    VorbisComment,
    Cuesheet,
    Picture,
    Unknown(u8)
}

fn flac_channels_to_channels(channels: u32) -> Channels {
    debug_assert!(channels > 0 && channels < 9);

    match channels {
        1 => { 
            Channels::FRONT_LEFT
        },
        2 => {
            Channels::FRONT_LEFT
                | Channels::FRONT_RIGHT
        },
        3 => {
            Channels::FRONT_LEFT
                | Channels::FRONT_RIGHT 
                | Channels::FRONT_CENTRE
        },
        4 => {
            Channels::FRONT_LEFT
                | Channels::FRONT_RIGHT
                | Channels::REAR_LEFT
                | Channels::REAR_RIGHT
        },
        5 => {
            Channels::FRONT_LEFT
                | Channels::FRONT_RIGHT
                | Channels::FRONT_CENTRE
                | Channels::REAR_LEFT
                | Channels::REAR_RIGHT
        },
        6 => {
            Channels::FRONT_LEFT
                | Channels::FRONT_RIGHT
                | Channels::FRONT_CENTRE
                | Channels::LFE1
                | Channels::REAR_LEFT
                | Channels::REAR_RIGHT
        },
        7 => {
            Channels::FRONT_LEFT
                | Channels::FRONT_RIGHT
                | Channels::FRONT_CENTRE
                | Channels::LFE1
                | Channels::REAR_CENTRE
                | Channels::REAR_LEFT
                | Channels::REAR_RIGHT
        },
        8 => {
            Channels::FRONT_LEFT
                | Channels::FRONT_RIGHT
                | Channels::FRONT_CENTRE
                | Channels::LFE1
                | Channels::REAR_LEFT
                | Channels::REAR_RIGHT
                | Channels::SIDE_LEFT
                | Channels::SIDE_RIGHT
        },
        _ => unreachable!()
    }
}

pub struct StreamInfo {
    /// The minimum and maximum number of decoded samples per block of audio.
    pub block_sample_len: (u16, u16),
    /// The minimum and maximum byte length of an encoded block (frame) of audio. Either value may
    /// be 0 if unknown.
    pub frame_byte_len: (u32, u32),
    /// The sample rate in Hz.
    pub sample_rate: u32,
    /// The channel mask.
    pub channels: Channels,
    /// The number of bits per sample of the stream.
    pub bits_per_sample: u32,
    /// The total number of samples in the stream, if available.
    pub n_samples: Option<u64>,
    /// The MD5 hash value of the decoded audio.
    pub md5: [u8; 16],
}

impl StreamInfo {

    pub fn read<B : ByteStream>(reader: &mut B)  -> Result<StreamInfo> {
        let mut info = StreamInfo {
            block_sample_len: (0, 0),
            frame_byte_len: (0, 0),
            sample_rate: 0,
            channels: Channels::empty(),
            bits_per_sample: 0,
            n_samples: None,
            md5: [0; 16],
        };

        // Read the block length bounds in number of samples.
        info.block_sample_len = (reader.read_be_u16()?, reader.read_be_u16()?);

        // Validate the block length bounds are in the range [16, 65535] samples.
        if info.block_sample_len.0 < 16 || info.block_sample_len.1 < 16{
            return decode_error("Minimum block length is 16 samples.");
        }

        // Validate the maximum block size is greater than or equal to the minimum block size.
        if info.block_sample_len.1 < info.block_sample_len.0 {
            return decode_error("Maximum block length cannot be less than the minimum block length.");
        }

        // Read the frame byte length bounds.
        info.frame_byte_len = (reader.read_be_u24()?, reader.read_be_u24()?);

        // Validate the maximum frame byte length is greater than or equal to the minimum frame byte
        // length if both are known. A value of 0 for either indicates the respective byte length is
        // unknown. Valid values are in the range [0, (2^24) - 1] bytes.
        if info.frame_byte_len.0 > 0 
            && info.frame_byte_len.1 > 0 
            && info.frame_byte_len.1 < info.frame_byte_len.0
        {
            return decode_error("Maximum frame length cannot be less than the minimum frame length.");
        }

        let mut br = BitReaderLtr::new();

        // Read sample rate, valid rates are [1, 655350] Hz.
        info.sample_rate = br.read_bits_leq32(reader, 20)?;

        if info.sample_rate < 1 || info.sample_rate > 655_350 {
            return decode_error("Stream sample rate out of bounds.");
        }

        // Read number of channels minus 1. Valid number of channels are 1-8.
        let channels_enc = br.read_bits_leq32(reader, 3)? + 1;

        if channels_enc < 1 || channels_enc > 8 {
            return decode_error("Stream channels are out of bounds.");
        }

        info.channels = flac_channels_to_channels(channels_enc);

        // Read bits per sample minus 1. Valid number of bits per sample are 4-32.
        info.bits_per_sample = br.read_bits_leq32(reader, 5)? + 1;

        if info.bits_per_sample < 4 || info.bits_per_sample > 32 {
            return decode_error("Stream bits per sample are out of bounds.")
        }

        // Read the total number of samples. All values are valid. A value of 0 indiciates a stream
        // of unknown length.
        info.n_samples = match br.read_bits_leq64(reader, 36)? {
            0 => None,
            samples => Some(samples)
        };

        // Read the decoded audio data MD5.
        reader.read_buf_bytes(&mut info.md5)?;

        Ok(info)
    }
}

pub fn read_comment_block<B : ByteStream>(
    reader: &mut B,
    metadata: &mut MetadataBuilder,
) -> Result<()> {
    vorbis::read_comment_no_framing(reader, metadata)
}

pub fn read_seek_table_block<B : ByteStream>(
    reader: &mut B,
    block_length: u32,
    table: &mut SeekIndex
) -> Result<()> {
    // The number of seek table entries is always the block length divided by the length of a single
    // entry, 18 bytes.
    let count = block_length / 18;

    for _ in 0..count {
        let sample = reader.read_be_u64()?;

        // A sample value of 0xFFFFFFFFFFFFFFFF is designated as a placeholder and is to be
        // ignored by decoders. The remaining 10 bytes of the seek point are undefined and must
        // still be consumed.
        if sample != 0xffff_ffff_ffff_ffff {
            table.insert(sample, reader.read_be_u64()?, u32::from(reader.read_be_u16()?));
        }
        else {
            reader.ignore_bytes(10)?;
        }
    }

    Ok(())
}

/// Converts a string of bytes to an ASCII string if all characters are within the printable ASCII
/// range. If a null byte is encounted, the string terminates at that point.
fn printable_ascii_to_string(bytes: &[u8]) -> Option<String> {
    let mut result = String::with_capacity(bytes.len());

    for c in bytes {
        match c {
            0x00        => break,
            0x20..=0x7e => result.push(char::from(*c)),
            _           => return None,
        }
    }

    Some(result)
}

pub fn read_cuesheet_block<B: ByteStream>(reader: &mut B, cues: &mut Vec<Cue>) -> Result<()> {
    // Read cuesheet catalog number. The catalog number only allows printable ASCII characters.
    let mut catalog_number_buf = vec![0u8; 128];
    reader.read_buf_bytes(&mut catalog_number_buf)?;

    let _catalog_number = match printable_ascii_to_string(&catalog_number_buf) {
        Some(s) => s,
        None => return decode_error("Cuesheet catalog number contains invalid characters."),
    };

    // Number of lead-in samples.
    let n_lead_in_samples = reader.read_be_u64()?;

    // Next bit is set for CD-DA cuesheets.
    let is_cdda = (reader.read_u8()? & 0x80) == 0x80;

    // Lead-in should be non-zero only for CD-DA cuesheets.
    if !is_cdda && n_lead_in_samples > 0 {
        return decode_error("Cuesheet lead-in samples should be zero if not CD-DA.");
    }

    // Next 258 bytes (read as 129 u16's) must be zero.
    for _ in 0..129 {
        if reader.read_be_u16()? != 0 {
            return decode_error("Cuesheet reserved bits should be zero.");
        }
    }

    let n_tracks = reader.read_u8()?;

    // There should be at-least one track in the cuesheet.
    if n_tracks == 0 {
        return decode_error("Cuesheet must have at-least one track.");
    }

    // CD-DA cuesheets must have no more than 100 tracks (99 audio tracks + lead-out track)
    if is_cdda && n_tracks > 100 {
        return decode_error("Cuesheets for CD-DA must not have more than 100 tracks.");
    }

    for _ in 0..n_tracks {
        read_cuesheet_track(reader, is_cdda, cues)?;
    }

    Ok(())
}

fn read_cuesheet_track<B: ByteStream>(
    reader: &mut B,
    is_cdda: bool,
    cues: &mut Vec<Cue>
) -> Result<()> {
    let n_offset_samples = reader.read_be_u64()?;

    // For a CD-DA cuesheet, the track sample offset is the same as the first index (INDEX 00 or
    // INDEX 01) on the CD. Therefore, the offset must be a multiple of 588 samples
    // (588 samples = 44100 samples/sec * 1/75th of a sec).
    if is_cdda && n_offset_samples % 588 != 0 {
        return decode_error("Cuesheet track sample offset is not a multiple of 588 for CD-DA.");
    }

    let number = u32::from(reader.read_u8()?);

    // A track number of 0 is disallowed in all cases. For CD-DA cuesheets, track 0 is reserved for
    // lead-in.
    if number == 0 {
        return decode_error("Cuesheet track number of 0 not allowed.");
    }

    // For CD-DA cuesheets, only track numbers 1-99 are allowed for regular tracks and 170 for
    // lead-out.
    if is_cdda && number > 99 && number != 170 {
        return decode_error("Cuesheet track numbers greater than 99 are not allowed for CD-DA.");
    }

    let mut isrc_buf = vec![0u8; 12];
    reader.read_buf_bytes(&mut isrc_buf)?;

    let isrc = match printable_ascii_to_string(&isrc_buf) {
        Some(s) => s,
        None => return decode_error("Cuesheet track ISRC contains invalid characters."),
    };

    // Next 14 bytes are reserved. However, the first two bits are flags. Consume the reserved bytes
    // in u16 chunks a minor performance improvement.
    let flags = reader.read_be_u16()?;

    // These values are contained in the Cuesheet but have no analogue in Sonata.
    let _is_audio = (flags & 0x8000) == 0x0000;
    let _use_pre_emphasis = (flags & 0x4000) == 0x4000;

    if flags & 0x3fff != 0 {
        return decode_error("Cuesheet track reserved bits should be zero.");
    }

    // Consume the remaining 12 bytes read in 3 u32 chunks.
    for _ in 0..3 {
        if reader.read_be_u32()? != 0 {
            return decode_error("Cuesheet track reserved bits should be zero.");
        }
    }

    let n_indicies = reader.read_u8()? as usize;

    // For CD-DA cuesheets, the track index cannot exceed 100 indicies.
    if is_cdda && n_indicies > 100 {
        return decode_error("Cuesheet track indicies cannot exceed 100 for CD-DA.");
    }

    let mut cue = Cue {
        index: number,
        start_ts: n_offset_samples,
        tags: Vec::new(),
        points: Vec::new(),
    };

    // Push the ISRC as a tag.
    cue.tags.push(Tag::new(Some(StandardTagKey::IdentIsrc), "ISRC", &isrc));

    for _ in 0..n_indicies {
        cue.points.push(read_cuesheet_track_index(reader, is_cdda)?);
    }

    cues.push(cue);

    Ok(())
}

fn read_cuesheet_track_index<B: ByteStream>(reader: &mut B, is_cdda: bool) -> Result<CuePoint> {
    let n_offset_samples = reader.read_be_u64()?;
    let idx_point_enc = reader.read_be_u32()?;

    // CD-DA track index points must have a sample offset that is a multiple of 588 samples 
    // (588 samples = 44100 samples/sec * 1/75th of a sec).
    if is_cdda && n_offset_samples % 588 != 0 {
        return decode_error("Cuesheet track index point sample offset is not a multiple of 588 for CD-DA.");
    }

    if idx_point_enc & 0x00ff_ffff != 0 {
        return decode_error("Cuesheet track index reserved bits should be 0.");
    }

    // TODO: Should be 0 or 1 for the first index for CD-DA.
    let _idx_point = ((idx_point_enc & 0xff00_0000) >> 24) as u8;

    Ok(CuePoint {
        start_offset_ts: n_offset_samples,
        tags: Vec::new(),
    })
}

pub fn read_application_block<B : ByteStream>(
    reader: &mut B,
    block_length: u32,
) -> Result<VendorData> {
    // Read the application identifier. Usually this is just 4 ASCII characters, but it is not
    // limited to that. Non-printable ASCII characters must be escaped to create a valid UTF8
    // string.
    let ident_buf = reader.read_quad_bytes()?;
    let ident = String::from_utf8(
        ident_buf.as_ref()
                 .iter()
                 .map(|b| ascii::escape_default(*b))
                 .flatten()
                 .collect()
        ).unwrap();

    let data = reader.read_boxed_slice_bytes(block_length as usize - 4)?;
    Ok(VendorData { ident, data })
}

pub fn read_picture_block<B : ByteStream>(
    reader: &mut B,
    metadata: &mut MetadataBuilder,
) -> Result<()> {
    let type_enc = reader.read_be_u32()?;
    
    // Read the Media Type length in bytes.
    let media_type_len = reader.read_be_u32()? as usize;

    // Read the Media Type bytes
    let mut media_type_buf = vec![0u8; media_type_len];
    reader.read_buf_bytes(&mut media_type_buf)?;

    // Convert Media Type bytes to an ASCII string. Non-printable ASCII characters are invalid.
    let media_type = match printable_ascii_to_string(&media_type_buf) {
        Some(s) => s,
        None => return decode_error("Picture mime-type contains invalid characters."),
    };

    // Read the description length in bytes.
    let desc_len = reader.read_be_u32()? as usize;
    
    // Read the description bytes.
    let mut desc_buf = vec![0u8; desc_len];
    reader.read_buf_bytes(&mut desc_buf)?;

    // Convert description bytes into a standard Vorbis DESCRIPTION tag.
    let mut tags = Vec::<Tag>::new();
    tags.push(
        Tag::new(Some(StandardTagKey::Description), "DESCRIPTION", &String::from_utf8_lossy(&desc_buf))
    );

    // Read the width, and height of the visual.
    let width = reader.read_be_u32()?;
    let height = reader.read_be_u32()?;

    // If either the width or height is 0, then the size is invalid.
    let dimensions = if width > 0 && height > 0 {
        Some(Size { width, height })
    }
    else {
        None
    };

    // Read bits-per-pixel of the visual.
    let bits_per_pixel = NonZeroU32::new(reader.read_be_u32()?);

    // Indexed colours is only valid for image formats that use an indexed colour palette. If it is
    // 0, the image does not used indexed colours.
    let indexed_colours_enc = reader.read_be_u32()?;

    let color_mode = match indexed_colours_enc {
        0 => Some(ColorMode::Discrete),
        _ => Some(ColorMode::Indexed(NonZeroU32::new(indexed_colours_enc).unwrap())),
    };

    // Read the image data
    let data_len = reader.read_be_u32()? as usize;
    let data = reader.read_boxed_slice_bytes(data_len)?;

    metadata.add_visual(Visual {
        media_type,
        dimensions,
        bits_per_pixel,
        color_mode,
        usage: id3v2::util::apic_picture_type_to_visual_key(type_enc),
        tags,
        data,
    });

    Ok(())
}

pub struct MetadataBlockHeader {
    pub is_last: bool,
    pub block_type: MetadataBlockType,
    pub block_len: u32
}

impl MetadataBlockHeader {
    pub fn read<B : ByteStream>(reader: &mut B) -> Result<MetadataBlockHeader> {
        let header_enc = reader.read_u8()?;

        // First bit of the header indicates if this is the last metadata block.
        let is_last = (header_enc & 0x80) == 0x80;

        // The next 7 bits of the header indicates the block type.
        let block_type_id = (header_enc & 0x7f) as u8;

        let block_type = match block_type_id {
            0 => MetadataBlockType::StreamInfo,
            1 => MetadataBlockType::Padding,
            2 => MetadataBlockType::Application,
            3 => MetadataBlockType::SeekTable,
            4 => MetadataBlockType::VorbisComment,
            5 => MetadataBlockType::Cuesheet,
            6 => MetadataBlockType::Picture,
            _ => MetadataBlockType::Unknown(block_type_id),
        };

        let block_len = reader.read_be_u24()?;

        Ok(MetadataBlockHeader {
            is_last,
            block_type,
            block_len,
        })
    }
}

