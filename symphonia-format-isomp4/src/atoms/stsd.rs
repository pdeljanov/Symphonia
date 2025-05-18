// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use core::str;

use log::debug;
use symphonia_core::audio::{Channels, Position};
use symphonia_core::codecs::audio::well_known::CODEC_ID_MP3;
use symphonia_core::codecs::audio::well_known::{CODEC_ID_PCM_F32BE, CODEC_ID_PCM_F32LE};
use symphonia_core::codecs::audio::well_known::{CODEC_ID_PCM_F64BE, CODEC_ID_PCM_F64LE};
use symphonia_core::codecs::audio::well_known::{CODEC_ID_PCM_S16BE, CODEC_ID_PCM_S16LE};
use symphonia_core::codecs::audio::well_known::{CODEC_ID_PCM_S24BE, CODEC_ID_PCM_S24LE};
use symphonia_core::codecs::audio::well_known::{CODEC_ID_PCM_S32BE, CODEC_ID_PCM_S32LE};
use symphonia_core::codecs::audio::well_known::{CODEC_ID_PCM_S8, CODEC_ID_PCM_U8};
use symphonia_core::codecs::audio::well_known::{CODEC_ID_PCM_U16BE, CODEC_ID_PCM_U16LE};
use symphonia_core::codecs::audio::well_known::{CODEC_ID_PCM_U24BE, CODEC_ID_PCM_U24LE};
use symphonia_core::codecs::audio::well_known::{CODEC_ID_PCM_U32BE, CODEC_ID_PCM_U32LE};
use symphonia_core::codecs::audio::{
    AudioCodecId, AudioCodecParameters, VerificationCheck, CODEC_ID_NULL_AUDIO,
};
use symphonia_core::codecs::subtitle::well_known::CODEC_ID_MOV_TEXT;
use symphonia_core::codecs::subtitle::SubtitleCodecParameters;
use symphonia_core::codecs::video::{VideoCodecId, VideoCodecParameters, VideoExtraData};
use symphonia_core::codecs::{CodecParameters, CodecProfile};
use symphonia_core::errors::{decode_error, unsupported_error, Result};
use symphonia_core::io::ReadBytes;

use crate::atoms::{
    AlacAtom, Atom, AtomHeader, AtomIterator, AtomType, AvcCAtom, Dac3Atom, Dec3Atom, DoviAtom,
    EsdsAtom, FlacAtom, HvcCAtom, OpusAtom, WaveAtom,
};
use crate::fp::FpU16;

/// Sample description atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct StsdAtom {
    /// Sample entry.
    sample_entry: SampleEntry,
}

impl Atom for StsdAtom {
    fn read<B: ReadBytes>(reader: &mut B, mut header: AtomHeader) -> Result<Self> {
        let (_, _) = header.read_extended_header(reader)?;

        let num_entries = reader.read_be_u32()?;

        if num_entries == 0 {
            return decode_error("isomp4 (stsd): missing sample entry");
        }

        if num_entries > 1 {
            return unsupported_error("isomp4 (stsd): more than 1 sample entry");
        }

        let sample_entry_header = AtomHeader::read(reader)?;

        let sample_entry = match sample_entry_header.atom_type {
            AtomType::AudioSampleEntryMp4a
            | AtomType::AudioSampleEntryAlac
            | AtomType::AudioSampleEntryAc3
            | AtomType::AudioSampleEntryEc3
            | AtomType::AudioSampleEntryFlac
            | AtomType::AudioSampleEntryOpus
            | AtomType::AudioSampleEntryMp3
            | AtomType::AudioSampleEntryLpcm
            | AtomType::AudioSampleEntryQtWave
            | AtomType::AudioSampleEntryALaw
            | AtomType::AudioSampleEntryMuLaw
            | AtomType::AudioSampleEntryU8
            | AtomType::AudioSampleEntryS16Le
            | AtomType::AudioSampleEntryS16Be
            | AtomType::AudioSampleEntryS24
            | AtomType::AudioSampleEntryS32
            | AtomType::AudioSampleEntryF32
            | AtomType::AudioSampleEntryF64 => {
                read_audio_sample_entry(reader, sample_entry_header)?
            }
            AtomType::VisualSampleEntryAv1
            | AtomType::VisualSampleEntryAvc1
            | AtomType::VisualSampleEntryDvh1
            | AtomType::VisualSampleEntryDvhe
            | AtomType::VisualSampleEntryHev1
            | AtomType::VisualSampleEntryHvc1
            | AtomType::VisualSampleEntryMp4v
            | AtomType::VisualSampleEntryVp8
            | AtomType::VisualSampleEntryVp9 => {
                read_visual_sample_entry(reader, sample_entry_header)?
            }
            AtomType::SubtitleSampleEntryText
            | AtomType::SubtitleSampleEntryTimedText
            | AtomType::SubtitleSampleEntryXml => {
                read_subtitle_sample_entry(reader, sample_entry_header)?
            }
            _ => {
                // Potentially subtitles, metadata, hints, etc.
                SampleEntry::Other
            }
        };

        Ok(StsdAtom { sample_entry })
    }
}

impl StsdAtom {
    /// Fill the provided `CodecParameters` using the sample entry.
    pub fn make_codec_params(&self) -> Option<CodecParameters> {
        // Audio sample entry.
        match &self.sample_entry {
            SampleEntry::Audio(entry) => Some(CodecParameters::Audio(entry.make_codec_params())),
            SampleEntry::Visual(entry) => Some(CodecParameters::Video(entry.make_codec_params())),
            SampleEntry::Subtitle(entry) => {
                Some(CodecParameters::Subtitle(entry.make_codec_params()))
            }
            _ => None,
        }
    }
}

/// Generic sample entry.
#[allow(dead_code)]
#[derive(Debug)]
pub enum SampleEntry {
    Audio(AudioSampleEntry),
    Visual(VisualSampleEntry),
    Subtitle(SubtitleSampleEntry),
    // Metadata,
    Other,
}

/// Audio sample entry.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub struct AudioSampleEntry {
    pub num_channels: u32,
    pub sample_size: u16,
    pub sample_rate: f64,
    pub codec_id: AudioCodecId,
    pub bits_per_sample: Option<u32>,
    pub bits_per_coded_sample: Option<u32>,
    pub frames_per_packet: Option<u64>,
    pub channels: Option<Channels>,
    pub verification_check: Option<VerificationCheck>,
    pub extra_data: Option<Box<[u8]>>,
}

impl AudioSampleEntry {
    pub(crate) fn make_codec_params(&self) -> AudioCodecParameters {
        AudioCodecParameters {
            codec: self.codec_id,
            sample_rate: Some(self.sample_rate as u32),
            bits_per_sample: self.bits_per_sample,
            bits_per_coded_sample: self.bits_per_coded_sample,
            channels: self.channels.clone(),
            max_frames_per_packet: self.frames_per_packet,
            verification_check: self.verification_check,
            extra_data: self.extra_data.clone(),
            ..Default::default()
        }
    }
}
/// Gets if the sample entry atom is for a PCM codec.
fn is_pcm_codec(atype: AtomType) -> bool {
    // PCM data in version 0 and 1 is signalled by the sample entry atom type. In version 2, the
    // atom type for PCM data is always LPCM.
    atype == AtomType::AudioSampleEntryLpcm || pcm_codec_id(atype) != CODEC_ID_NULL_AUDIO
}

/// Gets the PCM codec from the sample entry atom type for version 0 and 1 sample entries.
fn pcm_codec_id(atype: AtomType) -> AudioCodecId {
    match atype {
        AtomType::AudioSampleEntryU8 => CODEC_ID_PCM_U8,
        AtomType::AudioSampleEntryS16Le => CODEC_ID_PCM_S16LE,
        AtomType::AudioSampleEntryS16Be => CODEC_ID_PCM_S16BE,
        AtomType::AudioSampleEntryS24 => CODEC_ID_PCM_S24LE,
        AtomType::AudioSampleEntryS32 => CODEC_ID_PCM_S32LE,
        AtomType::AudioSampleEntryF32 => CODEC_ID_PCM_F32LE,
        AtomType::AudioSampleEntryF64 => CODEC_ID_PCM_F64LE,
        _ => CODEC_ID_NULL_AUDIO,
    }
}

/// Determines the number of bytes per PCM sample for a PCM codec ID.
fn bytes_per_pcm_sample(pcm_codec_id: AudioCodecId) -> u32 {
    match pcm_codec_id {
        CODEC_ID_PCM_S8 | CODEC_ID_PCM_U8 => 1,
        CODEC_ID_PCM_S16BE | CODEC_ID_PCM_S16LE => 2,
        CODEC_ID_PCM_U16BE | CODEC_ID_PCM_U16LE => 2,
        CODEC_ID_PCM_S24BE | CODEC_ID_PCM_S24LE => 3,
        CODEC_ID_PCM_U24BE | CODEC_ID_PCM_U24LE => 3,
        CODEC_ID_PCM_S32BE | CODEC_ID_PCM_S32LE => 4,
        CODEC_ID_PCM_U32BE | CODEC_ID_PCM_U32LE => 4,
        CODEC_ID_PCM_F32BE | CODEC_ID_PCM_F32LE => 4,
        CODEC_ID_PCM_F64BE | CODEC_ID_PCM_F64LE => 8,
        _ => unreachable!(),
    }
}

/// Gets the PCM codec from the LPCM parameters in the version 2 sample entry atom.
fn lpcm_codec_id(bits_per_sample: u32, lpcm_flags: u32) -> AudioCodecId {
    let is_floating_point = lpcm_flags & 0x1 != 0;
    let is_big_endian = lpcm_flags & 0x2 != 0;
    let is_signed = lpcm_flags & 0x4 != 0;

    if is_floating_point {
        // Floating-point sample format.
        match bits_per_sample {
            32 if is_big_endian => CODEC_ID_PCM_F32BE,
            64 if is_big_endian => CODEC_ID_PCM_F64BE,
            32 => CODEC_ID_PCM_F32LE,
            64 => CODEC_ID_PCM_F64LE,
            _ => CODEC_ID_NULL_AUDIO,
        }
    }
    else {
        // Integer sample format.
        if is_signed {
            // Signed-integer sample format.
            match bits_per_sample {
                8 => CODEC_ID_PCM_S8,
                16 if is_big_endian => CODEC_ID_PCM_S16BE,
                24 if is_big_endian => CODEC_ID_PCM_S24BE,
                32 if is_big_endian => CODEC_ID_PCM_S32BE,
                16 => CODEC_ID_PCM_S16LE,
                24 => CODEC_ID_PCM_S24LE,
                32 => CODEC_ID_PCM_S32LE,
                _ => CODEC_ID_NULL_AUDIO,
            }
        }
        else {
            // Unsigned-integer sample format.
            match bits_per_sample {
                8 => CODEC_ID_PCM_U8,
                16 if is_big_endian => CODEC_ID_PCM_U16BE,
                24 if is_big_endian => CODEC_ID_PCM_U24BE,
                32 if is_big_endian => CODEC_ID_PCM_U32BE,
                16 => CODEC_ID_PCM_U16LE,
                24 => CODEC_ID_PCM_U24LE,
                32 => CODEC_ID_PCM_U32LE,
                _ => CODEC_ID_NULL_AUDIO,
            }
        }
    }
}

/// Gets the audio channels for a version 0 or 1 sample entry.
fn pcm_channels(num_channels: u32) -> Result<Channels> {
    match num_channels {
        1 => Ok(Channels::Positioned(Position::FRONT_LEFT)),
        2 => Ok(Channels::Positioned(Position::FRONT_LEFT | Position::FRONT_RIGHT)),
        _ => decode_error("isomp4: invalid number of channels"),
    }
}

/// Gets the audio channels for a version 2 LPCM sample entry.
fn lpcm_channels(num_channels: u32) -> Result<Channels> {
    if num_channels < 1 {
        return decode_error("isomp4: invalid number of channels");
    }

    if num_channels > 32 {
        return unsupported_error("isomp4: maximum 32 channels");
    }

    // TODO: For LPCM, the channels are "auxilary". They do not have a speaker assignment. Symphonia
    // does not have a way to represent this yet.
    let channel_mask = !((!0 << 1) << (num_channels - 1));

    match Position::from_bits(channel_mask) {
        Some(positions) => Ok(Channels::Positioned(positions)),
        _ => unsupported_error("isomp4: unsupported number of channels"),
    }
}

fn read_audio_sample_entry<B: ReadBytes>(
    reader: &mut B,
    header: AtomHeader,
) -> Result<SampleEntry> {
    // An audio sample entry atom is derived from a base sample entry atom. The audio sample entry
    // atom contains the fields of the base sample entry first, then the audio sample entry fields
    // next. After those fields, a number of other atoms are nested, including the mandatory
    // codec-specific atom. Though the codec-specific atom is nested within the (audio) sample entry
    // atom, the (audio) sample entry atom uses the atom type of the codec-specific atom. This is
    // odd in-that the final structure will appear to have the codec-specific atom nested within
    // itself, which is not actually the case.

    // SampleEntry portion

    // Reserved. All 0.
    reader.ignore_bytes(6)?;

    // Sample entry data reference.
    let _ = reader.read_be_u16()?;

    // AudioSampleEntry(V1) portion

    let mut entry = AudioSampleEntry::default();

    // The version of the audio sample entry.
    let version = reader.read_be_u16()?;

    // Skip revision and vendor.
    reader.ignore_bytes(6)?;

    entry.num_channels = u32::from(reader.read_be_u16()?);
    entry.sample_size = reader.read_be_u16()?;

    // Skip compression ID and packet size.
    reader.ignore_bytes(4)?;

    entry.sample_rate = f64::from(FpU16::parse_raw(reader.read_be_u32()?));

    let is_pcm_codec = is_pcm_codec(header.atom_type);

    match version {
        0 => {
            // Version 0.
            if is_pcm_codec {
                entry.codec_id = pcm_codec_id(header.atom_type);
                let bits_per_sample = 8 * bytes_per_pcm_sample(entry.codec_id);

                // Validate the codec-derived bytes-per-sample equals the declared bytes-per-sample.
                if u32::from(entry.sample_size) != bits_per_sample {
                    return decode_error("isomp4: invalid pcm sample size");
                }
                entry.bits_per_sample = Some(bits_per_sample);
                entry.bits_per_coded_sample = Some(bits_per_sample);
                entry.frames_per_packet = Some(1);
                entry.channels = Some(pcm_channels(entry.num_channels)?);
            }
        }
        1 => {
            // Version 1.

            // The number of frames (ISO/MP4 samples) per packet. For PCM codecs, this is always 1.
            let _frames_per_packet = reader.read_be_u32()?;

            // The number of bytes per PCM audio sample. This value supersedes sample_size. For
            // non-PCM codecs, this value is not useful.
            let bytes_per_audio_sample = reader.read_be_u32()?;

            // The number of bytes per PCM audio frame (ISO/MP4 sample). For non-PCM codecs, this
            // value is not useful.
            let _bytes_per_frame = reader.read_be_u32()?;

            // The next value, as defined, is seemingly non-sensical.
            let _ = reader.read_be_u32()?;

            if is_pcm_codec {
                entry.codec_id = pcm_codec_id(header.atom_type);
                let codec_bytes_per_sample = bytes_per_pcm_sample(entry.codec_id);

                // Validate the codec-derived bytes-per-sample equals the declared bytes-per-sample.
                if bytes_per_audio_sample != codec_bytes_per_sample {
                    return decode_error("isomp4: invalid pcm bytes per sample");
                }

                // The new fields describe the PCM sample format and supersede the original version
                // 0 fields.
                entry.bits_per_sample = Some(8 * codec_bytes_per_sample);
                entry.bits_per_coded_sample = Some(8 * codec_bytes_per_sample);
                entry.frames_per_packet = Some(1);
                entry.channels = Some(pcm_channels(entry.num_channels)?);
            }
        }
        2 => {
            // Version 2.
            reader.ignore_bytes(4)?;

            entry.sample_rate = reader.read_be_f64()?;
            entry.num_channels = reader.read_be_u32()?;

            if reader.read_be_u32()? != 0x7f00_0000 {
                return decode_error("isomp4: audio sample entry v2 reserved must be 0x7f00_0000");
            }

            // The following fields are only useful for PCM codecs.
            let bits_per_sample = reader.read_be_u32()?;
            let lpcm_flags = reader.read_be_u32()?;
            let _bytes_per_packet = reader.read_be_u32()?;
            let lpcm_frames_per_packet = reader.read_be_u32()?;

            // This is only valid if this is a PCM codec.
            entry.codec_id = lpcm_codec_id(bits_per_sample, lpcm_flags);

            if is_pcm_codec && entry.codec_id != CODEC_ID_NULL_AUDIO {
                // Like version 1, the new fields describe the PCM sample format and supersede the
                // original version 0 fields.
                entry.bits_per_sample = Some(bits_per_sample);
                entry.bits_per_coded_sample = Some(bits_per_sample);
                entry.frames_per_packet = Some(u64::from(lpcm_frames_per_packet));
                entry.channels = Some(lpcm_channels(entry.num_channels)?);
            }
        }
        _ => {
            return unsupported_error("isomp4: unknown sample entry version");
        }
    };

    let mut iter = AtomIterator::new(reader, header);

    while let Some(entry_header) = iter.next()? {
        match entry_header.atom_type {
            AtomType::Esds => {
                let atom = iter.read_atom::<EsdsAtom>()?;
                atom.fill_audio_sample_entry(&mut entry)?;
            }
            AtomType::Ac3Config => {
                let atom = iter.read_atom::<Dac3Atom>()?;
                atom.fill_audio_sample_entry(&mut entry);
            }
            AtomType::AudioSampleEntryAlac => {
                let atom = iter.read_atom::<AlacAtom>()?;
                atom.fill_audio_sample_entry(&mut entry);
            }
            AtomType::Eac3Config => {
                let atom = iter.read_atom::<Dec3Atom>()?;
                atom.fill_audio_sample_entry(&mut entry);
            }
            AtomType::FlacDsConfig => {
                let atom = iter.read_atom::<FlacAtom>()?;
                atom.fill_audio_sample_entry(&mut entry);
            }
            AtomType::OpusDsConfig => {
                let atom = iter.read_atom::<OpusAtom>()?;
                atom.fill_audio_sample_entry(&mut entry);
            }
            AtomType::AudioSampleEntryQtWave => {
                // The QuickTime WAVE (aka. siDecompressionParam) atom may contain many different
                // types of sub-atoms to store decoder parameters.
                let atom = iter.read_atom::<WaveAtom>()?;
                atom.fill_audio_sample_entry(&mut entry)?;
            }
            _ => {
                debug!("unknown audio sample entry sub-atom: {:?}.", entry_header.atom_type());
            }
        }
    }

    // A MP3 sample entry has no codec-specific atom.
    if header.atom_type == AtomType::AudioSampleEntryMp3 {
        entry.codec_id = CODEC_ID_MP3;
    }

    Ok(SampleEntry::Audio(entry))
}

/// Visual sample entry.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub struct VisualSampleEntry {
    pub width: u16,
    pub height: u16,
    pub horiz_res: f64,
    pub vert_res: f64,
    /// Frame count per sample.
    pub frame_count: u16,
    pub compressor: Option<String>,
    pub codec_id: VideoCodecId,
    pub profile: Option<CodecProfile>,
    pub level: Option<u32>,
    pub extra_data: Vec<VideoExtraData>,
}

impl VisualSampleEntry {
    pub(crate) fn make_codec_params(&self) -> VideoCodecParameters {
        let mut codec_params = VideoCodecParameters {
            width: Some(self.width),
            height: Some(self.height),
            codec: self.codec_id,
            extra_data: self.extra_data.clone(),
            ..Default::default()
        };

        if let Some(profile) = self.profile {
            codec_params.with_profile(profile);
        }
        if let Some(level) = self.level {
            codec_params.with_level(level);
        }

        codec_params
    }
}

fn read_visual_sample_entry<B: ReadBytes>(
    reader: &mut B,
    header: AtomHeader,
) -> Result<SampleEntry> {
    // SampleEntry portion

    // Reserved. All 0.
    reader.ignore_bytes(6)?;

    // Sample entry data reference.
    let _ = reader.read_be_u16()?;

    // VisualSampleEntry portion

    // Reserved.
    reader.ignore_bytes(16)?;

    let mut entry = VisualSampleEntry {
        width: reader.read_be_u16()?,
        height: reader.read_be_u16()?,
        horiz_res: f64::from(FpU16::parse_raw(reader.read_be_u32()?)),
        vert_res: f64::from(FpU16::parse_raw(reader.read_be_u32()?)),
        ..Default::default()
    };

    // Reserved.
    let _ = reader.read_be_u32()?;

    entry.frame_count = reader.read_be_u16()?;

    entry.compressor = {
        let len = usize::from(reader.read_u8()?);

        let mut name = [0u8; 31];
        reader.read_buf_exact(&mut name)?;

        match str::from_utf8(&name[..len]) {
            Ok(name) => Some(name.to_string()),
            _ => None,
        }
    };

    let _depth = reader.read_be_u16()?;

    // Reserved.
    reader.read_be_u16()?;

    let mut iter = AtomIterator::new(reader, header);

    while let Some(entry_header) = iter.next()? {
        match entry_header.atom_type {
            AtomType::Esds => {
                let atom = iter.read_atom::<EsdsAtom>()?;
                atom.fill_video_sample_entry(&mut entry)?;
            }
            AtomType::AvcConfiguration => {
                let atom = iter.read_atom::<AvcCAtom>()?;
                atom.fill_video_sample_entry(&mut entry);
            }
            AtomType::HevcConfiguration => {
                let atom = iter.read_atom::<HvcCAtom>()?;
                atom.fill_video_sample_entry(&mut entry);
            }
            AtomType::DolbyVisionConfiguration => {
                let atom = iter.read_atom::<DoviAtom>()?;
                atom.fill_video_sample_entry(&mut entry);
            }
            _ => {
                debug!("unknown visual sample entry sub-atom: {:?}.", entry_header.atom_type());
            }
        }
    }

    Ok(SampleEntry::Visual(entry))
}

#[derive(Debug)]
pub enum SubtitleCodecSpecific {
    /// MOV_TEXT
    TimedText,
}

/// Subtitle sample entry type.
#[allow(dead_code)]
#[derive(Debug)]
pub struct SubtitleSampleEntry {
    btrt: Option<BtrtAtom>,
    txtc: Option<TxtcAtom>,
    codec_specific: Option<SubtitleCodecSpecific>,
}

impl SubtitleSampleEntry {
    pub(crate) fn make_codec_params(&self) -> SubtitleCodecParameters {
        let mut codec_params = SubtitleCodecParameters::new();

        if let Some(SubtitleCodecSpecific::TimedText) = self.codec_specific {
            codec_params.for_codec(CODEC_ID_MOV_TEXT);
        }

        codec_params
    }
}

fn read_subtitle_sample_entry<B: ReadBytes>(
    reader: &mut B,
    header: AtomHeader,
) -> Result<SampleEntry> {
    // SampleEntry portion

    // Reserved. All 0.
    reader.ignore_bytes(6)?;

    // Sample entry data reference.
    let _ = reader.read_be_u16()?;

    let mut codec_specific = None;
    // SubtitleSampleEntry portion

    match header.atom_type {
        AtomType::SubtitleSampleEntryText => {
            let (_, _encoding) =
                read_null_terminated_utf8(reader, header.data_unread_at(reader.pos()))?;

            let (_, _mime_type) =
                read_null_terminated_utf8(reader, header.data_unread_at(reader.pos()))?;
        }
        AtomType::SubtitleSampleEntryTimedText => {
            // Standard - 3GPP TS 26.245 - TextSampleEntry
            // display flags - 4 bytes
            // horizontal justification - 1 bytes
            // vertical justification - 1 bytes
            // background color rgba - 4 bytes
            // box record - 8 bytes
            // style record - 12 bytes
            reader.ignore_bytes(30)?;

            codec_specific = Some(SubtitleCodecSpecific::TimedText);
        }
        AtomType::SubtitleSampleEntryXml => {
            let (_, _namespace) =
                read_null_terminated_utf8(reader, header.data_unread_at(reader.pos()))?;

            let (_, _schema_location) =
                read_null_terminated_utf8(reader, header.data_unread_at(reader.pos()))?;

            let (_, _auxiliary_mime_types) =
                read_null_terminated_utf8(reader, header.data_unread_at(reader.pos()))?;
        }
        _ => {}
    }

    let mut iter = AtomIterator::new(reader, header);

    let mut btrt = None;
    let mut txtc = None;

    while let Some(entry_header) = iter.next()? {
        match entry_header.atom_type {
            AtomType::BitRate => {
                btrt = Some(iter.read_atom::<BtrtAtom>()?);
            }
            AtomType::TextConfig => {
                txtc = Some(iter.read_atom::<TxtcAtom>()?);
            }
            _ => {
                debug!("unknown subtitle sample entry sub-atom: {:?}.", entry_header.atom_type());
            }
        }
    }

    Ok(SampleEntry::Subtitle(SubtitleSampleEntry { btrt, txtc, codec_specific }))
}

fn read_null_terminated_utf8<B: ReadBytes>(
    reader: &mut B,
    max_bytes: Option<u64>,
) -> Result<(usize, String)> {
    // If not maximum length was specified, read upto usize limit.
    let max_bytes = max_bytes.map(|max| max as usize).unwrap_or(usize::MAX);

    let mut buf = Vec::new();
    let mut len = 0;

    loop {
        // Do not exceed maximum length.
        if len >= max_bytes {
            return decode_error("isomp4: maximum string length exceeded");
        }

        let byte = reader.read_u8()?;
        len += 1;

        // Break at the null-terminator. Do not add it to the string buffer.
        if byte == 0 {
            break;
        }

        buf.push(byte);
    }

    let value = match String::from_utf8(buf) {
        Ok(value) => value,
        Err(_) => return decode_error("isomp4: invalid utf8"),
    };

    Ok((len, value))
}

/// Bitrate atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct BtrtAtom {
    /// Size of the decoding buffer for an elementary stream in bytes.
    pub buf_size_db: u32,
    /// Maximum bitrate in bits/second over a window of 1 second.
    pub max_bitrate: u32,
    /// Average bitrate in bits/second.
    pub avg_bitrate: u32,
}

impl Atom for BtrtAtom {
    fn read<B: ReadBytes>(reader: &mut B, _header: AtomHeader) -> Result<Self> {
        Ok(BtrtAtom {
            buf_size_db: reader.read_be_u32()?,
            max_bitrate: reader.read_be_u32()?,
            avg_bitrate: reader.read_be_u32()?,
        })
    }
}

/// Text config atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct TxtcAtom {
    /// Initial text to be prepended before the contents of each sync sample.
    pub text_config: String,
}

impl Atom for TxtcAtom {
    fn read<B: ReadBytes>(reader: &mut B, mut header: AtomHeader) -> Result<Self> {
        let (_, _) = header.read_extended_header(reader)?;

        let (_, text_config) = read_null_terminated_utf8(reader, header.data_len())?;

        Ok(TxtcAtom { text_config })
    }
}

/// Clean aperture atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct ClapAtom {
    pub h_spacing: u32,
    pub v_spacing: u32,
}

impl Atom for ClapAtom {
    fn read<B: ReadBytes>(reader: &mut B, _header: AtomHeader) -> Result<Self> {
        Ok(ClapAtom { h_spacing: reader.read_be_u32()?, v_spacing: reader.read_be_u32()? })
    }
}

/// Pixel aspect ratio atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct PaspAtom {
    clean_aperture_width_n: u32,
    clean_aperture_width_d: u32,
    clean_aperture_height_n: u32,
    clean_aperture_height_d: u32,
    horiz_off_n: u32,
    horiz_off_d: u32,
    vert_off_n: u32,
    vert_off_d: u32,
}

impl Atom for PaspAtom {
    fn read<B: ReadBytes>(reader: &mut B, _header: AtomHeader) -> Result<Self> {
        Ok(PaspAtom {
            clean_aperture_width_n: reader.read_be_u32()?,
            clean_aperture_width_d: reader.read_be_u32()?,
            clean_aperture_height_n: reader.read_be_u32()?,
            clean_aperture_height_d: reader.read_be_u32()?,
            horiz_off_n: reader.read_be_u32()?,
            horiz_off_d: reader.read_be_u32()?,
            vert_off_n: reader.read_be_u32()?,
            vert_off_d: reader.read_be_u32()?,
        })
    }
}
