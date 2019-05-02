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

use std::fmt;
use std::mem;
use std::num::NonZeroU32;
use sonata_core::audio::Channels;
use sonata_core::errors::{Result, decode_error};
use sonata_core::formats::{ColorMode, SeekIndex, Size, Visual};
use sonata_core::tags::*;
use sonata_core::io::*;

#[derive(Debug)]
pub enum MetadataBlockType {
    StreamInfo,
    Padding,
    Application,
    SeekTable,
    VorbisComment,
    Cuesheet,
    Picture,
    Unknown
}

fn flac_channels_to_channel_vec(channels: u32) -> Channels {
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
        _ => panic!("Invalid channel assignment for FLAC.")
    }
}


pub struct StreamInfo {
    pub block_size_bounds: (u16, u16),
    pub frame_size_bounds: (u32, u32),
    pub sample_rate: u32,
    pub channels: Channels,
    pub bits_per_sample: u32,
    pub n_samples: Option<u64>,
    pub md5: [u8; 16],
}

impl StreamInfo {

    pub fn read<B : Bytestream>(reader: &mut B)  -> Result<StreamInfo> {
        let mut info = StreamInfo {
            block_size_bounds: (0, 0),
            frame_size_bounds: (0, 0),
            sample_rate: 0,
            channels: Channels::empty(),
            bits_per_sample: 0,
            n_samples: None,
            md5: [0; 16],
        };

        // Read the block size bounds in samples. Valid values are 16-65535.
        info.block_size_bounds = (reader.read_be_u16()?, reader.read_be_u16()?);
        debug_assert!(info.block_size_bounds.0 >= 16 && info.block_size_bounds.1 >= 16);

        // Read the frame size bounds in bytes. Valid values are 0-2^24-1. A 0 indicates the size
        // is unknown.
        info.frame_size_bounds = (reader.read_be_u24()?, reader.read_be_u24()?);

        let mut br = BitReaderLtr::new();

        // Read sample rate, valid rates are 1-655350Hz.
        info.sample_rate = br.read_bits_leq32(reader, 20)?;

        if info.sample_rate < 1 || info.sample_rate > 655350 {
            return decode_error("Stream sample rate out of bounds.");
        }

        // Read number of channels minus 1. Valid number of channels are 1-8.
        let channels_enc = br.read_bits_leq32(reader, 3)? + 1;

        if channels_enc < 1 || channels_enc > 8 {
            return decode_error("Stream channels are out of bounds.");
        }

        info.channels = flac_channels_to_channel_vec(channels_enc);

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

impl fmt::Display for StreamInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "StreamInfo {{")?;
        writeln!(f, "\tblock_size_bounds: [{}, {}],", self.block_size_bounds.0, self.block_size_bounds.1)?;
        writeln!(f, "\tframe_size_bounds: [{}, {}],", self.frame_size_bounds.0, self.frame_size_bounds.1)?;
        writeln!(f, "\tsample_rate: {} Hz,", self.sample_rate)?;
        writeln!(f, "\tn_channels: {:?},", self.channels)?;
        writeln!(f, "\tbits_per_sample: {},", self.bits_per_sample)?;
        match self.n_samples {
            Some(n_samples) => writeln!(f, "\tn_samples: {},", n_samples)?,
            None => writeln!(f, "\tn_samples: ?")?,
        };
        writeln!(f, "\tmd5: {:x?}", self.md5)?;
        writeln!(f, "}}")
    }
}

macro_rules! verify_block_bounds {
    ($accum:ident, $bound:ident, $len:expr) => (
        $accum += $len;
        if $accum > $bound {
            return decode_error("Comment exceeded stated block length.");
        }
    )
}

pub struct VorbisComment;

impl VorbisComment {
    pub fn process<B : Bytestream>(reader: &mut B, block_length: usize, tags: &mut Vec<Tag>) -> Result<()> {
        // Accumulate the number of bytes read as the comment block is decoded and ensure that
        // the block_length as stated in the header is never exceeded.
        let mut block_bytes_read = 0usize;

        // Get the vendor string length in bytes.
        verify_block_bounds!(block_bytes_read, block_length, mem::size_of::<u32>());
        let vendor_length = reader.read_u32()? as usize;

        // Read the vendor string.
        verify_block_bounds!(block_bytes_read, block_length, vendor_length);

        let mut vendor_string_octets = Vec::<u8>::with_capacity(vendor_length);
        vendor_string_octets.resize(vendor_length, 0);
        reader.read_buf_bytes(&mut vendor_string_octets)?;

        let vendor = String::from_utf8_lossy(&vendor_string_octets).to_string();

        // Read the number of comments.
        verify_block_bounds!(block_bytes_read, block_length, mem::size_of::<u32>());
        let n_comments = reader.read_u32()? as usize;

        for _ in 0..n_comments {
            // Read the comment length in bytes.
            verify_block_bounds!(block_bytes_read, block_length, mem::size_of::<u32>());
            let comment_length = reader.read_u32()? as usize;

            // Read the comment string.
            verify_block_bounds!(block_bytes_read, block_length, comment_length);

            let mut comment_byte = Vec::<u8>::with_capacity(comment_length);
            comment_byte.resize(comment_length, 0);
            reader.read_buf_bytes(&mut comment_byte)?;

            // Parse comment as UTF-8 and add to list.
            tags.push(VorbisTag::parse(&String::from_utf8_lossy(&comment_byte).to_string()));
        }

        Ok(())
    }
}

pub struct SeekTable;

impl SeekTable {
    pub fn process<B : Bytestream>(reader: &mut B, block_length: usize, table: &mut SeekIndex) -> Result<()> {
        let count = block_length / 18;

        for _ in 0..count {
            let sample = reader.read_be_u64()?;

            // A sample value of 0xFFFFFFFFFFFFFFFF is designated as a placeholder and is to be
            // ignored by decoders. The remaining 10 bytes of the seek point are undefined and must
            // still be consumed.
            if sample != 0xffffffffffffffff {
                table.insert(sample, reader.read_be_u64()?, reader.read_be_u16()? as u32);
            }
            else {
                reader.ignore_bytes(10)?;
            }
        }

        Ok(())
    }

}

/// Converts a string of bytes to an ASCII string if all characters are within the printable ASCII range. If a null
/// byte is encounted, the string terminates at that point.
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

pub struct CuesheetTrackIndex {
    pub n_offset_samples: u64,
    pub idx_point: u8,
}

impl CuesheetTrackIndex {
    pub fn read<B : Bytestream>(reader: &mut B) -> Result<CuesheetTrackIndex> {
        let n_offset_samples = reader.read_be_u64()?;
        let idx_point_enc = reader.read_be_u32()?;

        if idx_point_enc & 0x00ffffff != 0 {
            return decode_error("Cuesheet track index reserved bits should be 0.");
        }

        let idx_point = ((idx_point_enc & 0xff000000) >> 24) as u8;

        Ok(CuesheetTrackIndex {
            n_offset_samples,
            idx_point
        })
    }
}

pub struct CuesheetTrack {
    pub n_offset_samples: u64,
    pub number: u8,
    pub isrc: String,
    pub is_audio: bool,
    pub use_pre_emphasis: bool,
    pub index: Vec<CuesheetTrackIndex>,
}

impl CuesheetTrack {
    pub fn read<B : Bytestream>(reader: &mut B) -> Result<CuesheetTrack> {
        let n_offset_samples = reader.read_be_u64()?;
        let number = reader.read_u8()?;

        let mut isrc_buf = vec![0u8; 12];
        reader.read_buf_bytes(&mut isrc_buf)?;

        let isrc = match printable_ascii_to_string(&isrc_buf) {
            Some(s) => s,
            None => return decode_error("Cuesheet track ISRC contains invalid characters."),
        };

        // Next 14 bytes are reserved. However, the first two bits are flags. Consume the reserved bytes in u16 chunks 
        // a minor performance improvement.
        let flags = reader.read_u16()?;

        let is_audio = (flags & 0x8000) == 0x0000;
        let use_pre_emphasis = (flags & 0x4000) == 0x4000;

        if flags & 0xcfff != 0x0000 {
            return decode_error("Cuesheet track reserved bits should be zero.");
        }

        // Consume the remaining 12 bytes read in 6 u16 chunks.
        for _ in 0..6 {
            if reader.read_be_u16()? != 0 {
                return decode_error("Cuesheet track reserved bits should be zero.");
            }
        }

        let n_indicies = reader.read_u8()? as usize;

        let mut track = CuesheetTrack {
            n_offset_samples,
            number,
            isrc,
            is_audio,
            use_pre_emphasis,
            index: Vec::<CuesheetTrackIndex>::with_capacity(n_indicies),
        };

        for _ in 0..n_indicies {
            track.index.push(CuesheetTrackIndex::read(reader)?);
        }

        Ok(track)
    }
}

pub struct Cuesheet {
    pub catalog_number: String,
    pub n_lead_in_samples: u64,
    pub is_cdda: bool,
    pub tracks: Vec<CuesheetTrack>,
}

impl Cuesheet {
    pub fn read<B : Bytestream>(reader: &mut B, _block_length: usize) -> Result<Cuesheet> {

        // Read cuesheet catalog number. The catalog number only allows printable ASCII characters.
        let mut catalog_number_buf = vec![0u8; 128];
        reader.read_buf_bytes(&mut catalog_number_buf)?;

        let catalog_number = match printable_ascii_to_string(&catalog_number_buf) {
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

        let mut cuesheet = Cuesheet {
            catalog_number,
            n_lead_in_samples,
            is_cdda,
            tracks: Vec::<CuesheetTrack>::with_capacity(n_tracks as usize),
        };

        for _ in 0..n_tracks {
            cuesheet.tracks.push(CuesheetTrack::read(reader)?);
        }

        Ok(cuesheet)
    }
}

impl fmt::Display for Cuesheet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Cuesheet {{")?;
        writeln!(f, "\tcatalog_number={},", self.catalog_number)?;
        writeln!(f, "\tn_lead_in_samples={},", self.n_lead_in_samples)?;
        writeln!(f, "\tis_cdda={},", self.is_cdda)?;
        writeln!(f, "\ttracks=[")?;
        for track in &self.tracks {
            writeln!(f, "\t\t{{")?;
            writeln!(f, "\t\t\tn_offset_samples={}", track.n_offset_samples)?;
            writeln!(f, "\t\t\tnumber={}", track.number)?;
            writeln!(f, "\t\t\tisrc={}", track.isrc)?;
            writeln!(f, "\t\t\tis_audio={}", track.is_audio)?;
            writeln!(f, "\t\t\tuse_pre_emphasis={}", track.use_pre_emphasis)?;
            writeln!(f, "\t\t\tindex=[")?;
            for index in &track.index {
                writeln!(f, "\t\t\t\t{{ n_offset_samples={}, idx_point={} }}", index.n_offset_samples, index.idx_point)?;
            }
            writeln!(f, "\t\t}}")?;
            writeln!(f, "\t\t]")?;
        }
        writeln!(f, "\t]")?;
        writeln!(f, "}}")
    }
}

pub struct Application {
    pub application: u32,
}

impl Application {
    pub fn read<B : Bytestream>(reader: &mut B, block_length: usize) -> Result<Application> {
        let application = reader.read_be_u32()?;
        // TODO: Actually read the application data.
        reader.ignore_bytes(block_length as u64 - 4)?;
        Ok(Application { application })
    }
}

impl fmt::Display for Application {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Application {{ application={} }}", self.application)
    }
}


pub struct Picture;

fn visual_key_from_id3v2_apic(apic: u32) -> Option<StandardVisualKey> {
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

impl Picture {
    pub fn read<B : Bytestream>(reader: &mut B, _block_length: usize, visuals: &mut Vec<Visual>) -> Result<()> {
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
        tags.push(Tag::new(Some(StandardTagKey::Description), "DESCRIPTION", &String::from_utf8_lossy(&desc_buf)));

        // Read the width, height, and bits-per-pixel of the visual.
        let width = reader.read_be_u32()?;
        let height = reader.read_be_u32()?;
        let bits_per_pixel = NonZeroU32::new(reader.read_be_u32()?);

        // Indexed colours is only valid for image formats that use an indexed colour palette. If it is 0, the image 
        // does not used indexed colours.
        let indexed_colours_enc = reader.read_be_u32()?;

        let color_mode = match indexed_colours_enc {
            0 => ColorMode::Discrete,
            _ => ColorMode::Indexed(NonZeroU32::new(indexed_colours_enc).unwrap()),
        };

        // Read the image data
        let data_len = reader.read_be_u32()? as usize;

        let mut data_buf = Vec::<u8>::with_capacity(data_len);
        unsafe { data_buf.set_len(data_len); }
        reader.read_buf_bytes(&mut data_buf)?;

        visuals.push(Visual {
            media_type,
            dimensions: Size { width, height },
            bits_per_pixel,
            color_mode,
            usage: visual_key_from_id3v2_apic(type_enc),
            tags,
            data: data_buf.into_boxed_slice(),
        });

        Ok(())
    }
}

pub struct MetadataBlockHeader {
    pub is_last: bool,
    pub block_type: MetadataBlockType,
    pub block_length: usize
}

impl MetadataBlockHeader {
    pub fn read<B : Bytestream>(reader: &mut B) -> Result<MetadataBlockHeader> {
        let header_enc = reader.read_u8()?;

        // First bit of the header indicates if this is the last metadata block.
        let is_last = (header_enc & 0x80) == 0x80;

        // Next 7 bits of the header indicates the metadata block type.
        let block_type = match header_enc & 0x7f {
            0 => MetadataBlockType::StreamInfo,
            1 => MetadataBlockType::Padding,
            2 => MetadataBlockType::Application,
            3 => MetadataBlockType::SeekTable,
            4 => MetadataBlockType::VorbisComment,
            5 => MetadataBlockType::Cuesheet,
            6 => MetadataBlockType::Picture,
            _ => MetadataBlockType::Unknown,
        };

        Ok(MetadataBlockHeader {
            is_last,
            block_type,
            block_length: reader.read_be_u24()? as usize,
        })
    }
}

