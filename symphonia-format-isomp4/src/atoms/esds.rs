// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::codecs::video::{VideoExtraData, VIDEO_EXTRA_DATA_ID_NULL};
use symphonia_core::codecs::CodecId;
use symphonia_core::errors::{decode_error, unsupported_error, Error, Result};
use symphonia_core::io::{FiniteStream, ReadBytes, ScopedStream};

use crate::atoms::stsd::{AudioSampleEntry, VisualSampleEntry};
use crate::atoms::{Atom, AtomHeader};

use log::{debug, warn};

const ES_DESCRIPTOR: u8 = 0x03;
const DECODER_CONFIG_DESCRIPTOR: u8 = 0x04;
const DECODER_SPECIFIC_DESCRIPTOR: u8 = 0x05;
const SL_CONFIG_DESCRIPTOR: u8 = 0x06;

const MIN_DESCRIPTOR_SIZE: u64 = 2;

fn read_descriptor_header<B: ReadBytes>(reader: &mut B) -> Result<(u8, u32)> {
    let tag = reader.read_u8()?;

    let mut size = 0;

    for _ in 0..4 {
        let val = reader.read_u8()?;
        size = (size << 7) | u32::from(val & 0x7f);
        if val & 0x80 == 0 {
            break;
        }
    }

    Ok((tag, size))
}

/// Elementary stream descriptor atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct EsdsAtom {
    /// Elementary stream descriptor.
    descriptor: Option<ESDescriptor>,
}

impl Atom for EsdsAtom {
    fn read<B: ReadBytes>(reader: &mut B, mut header: AtomHeader) -> Result<Self> {
        let (_, _) = header.read_extended_header(reader)?;

        // The ES descriptors occupy the rest of the atom.
        let ds_size = header
            .data_len()
            .ok_or(Error::DecodeError("isomp4 (esds): expected atom size to be known"))?;

        let mut scoped = ScopedStream::new(reader, ds_size);

        let mut descriptor = None;

        while scoped.bytes_available() > MIN_DESCRIPTOR_SIZE {
            let (desc, desc_len) = read_descriptor_header(&mut scoped)?;

            match desc {
                ES_DESCRIPTOR => {
                    descriptor = Some(ESDescriptor::read(&mut scoped, desc_len)?);
                }
                _ => {
                    warn!("unknown descriptor in esds atom, desc={}", desc);
                    scoped.ignore_bytes(desc_len as u64)?;
                }
            }
        }

        // Ignore remainder of the atom.
        scoped.ignore()?;

        Ok(EsdsAtom { descriptor })
    }
}

impl EsdsAtom {
    /// If the elementary stream descriptor describes an audio stream, populate the provided
    /// audio sample entry.
    pub fn fill_audio_sample_entry(&self, entry: &mut AudioSampleEntry) -> Result<()> {
        if let Some(desc) = &self.descriptor {
            match get_codec_id_from_object_type(desc.dec_config.object_type_indication) {
                Some(CodecId::Audio(id)) => {
                    // Object type indication identified an audio codec.
                    entry.codec_id = id;
                }
                Some(_) => {
                    // Object type indication identified a non-audio codec. This is unexpected.
                    return decode_error("isomp4 (esds): expected an audio codec type");
                }
                None => {}
            }

            if let Some(ds_config) = &desc.dec_config.dec_specific_info {
                entry.extra_data = Some(ds_config.extra_data.clone());
            }
        }

        Ok(())
    }

    /// If the elementary stream descriptor describes an video stream, populate the provided
    /// video sample entry.
    pub fn fill_video_sample_entry(&self, entry: &mut VisualSampleEntry) -> Result<()> {
        if let Some(desc) = &self.descriptor {
            match get_codec_id_from_object_type(desc.dec_config.object_type_indication) {
                Some(CodecId::Video(id)) => {
                    // Object type indication identified an video codec.
                    entry.codec_id = id;
                }
                Some(_) => {
                    // Object type indication identified a non-video codec. This is unexpected.
                    return decode_error("isomp4 (esds): expected a video codec type");
                }
                None => {}
            }

            if let Some(ds_config) = &desc.dec_config.dec_specific_info {
                entry.extra_data.push(VideoExtraData {
                    id: VIDEO_EXTRA_DATA_ID_NULL,
                    data: ds_config.extra_data.clone(),
                });
            }
        }

        Ok(())
    }
}

/// Try to get a codec ID from from an object type indication.
fn get_codec_id_from_object_type(obj_type: u8) -> Option<CodecId> {
    use symphonia_core::codecs::audio::well_known::{
        CODEC_ID_AAC, CODEC_ID_AC3, CODEC_ID_DCA, CODEC_ID_EAC3, CODEC_ID_MP3,
    };
    use symphonia_core::codecs::video::well_known::{
        CODEC_ID_H264, CODEC_ID_HEVC, CODEC_ID_MPEG2, CODEC_ID_MPEG4, CODEC_ID_VP9,
    };

    // AAC
    const OBJ_TYPE_AUDIO_MPEG4_3: u8 = 0x40; // Audio ISO/IEC 14496-3
    const OBJ_TYPE_AUDIO_MPEG2_7_MAIN: u8 = 0x66; // Audio ISO/IEC 13818-7 Main Profile
    const OBJ_TYPE_AUDIO_MPEG2_7_LC: u8 = 0x67; // Audio ISO/IEC 13818-7 Low Complexity

    // MP3
    const OBJ_TYPE_AUDIO_MPEG2_3: u8 = 0x69; // Audio ISO/IEC 13818-3 (MP3)
    const OBJ_TYPE_AUDIO_MPEG1_3: u8 = 0x6b; // Audio ISO/IEC 11172-3 (MP3)

    const OBJ_TYPE_AUDIO_AC3: u8 = 0xa5;
    const OBJ_TYPE_AUDIO_EAC3: u8 = 0xa6;
    const OBJ_TYPE_AUDIO_DTS: u8 = 0xa9;

    // MPEG2 video
    const OBJ_TYPE_VISUAL_MPEG2_2_SP: u8 = 0x60; // Visual ISO/IEC 13818-2 Simple Profile
    const OBJ_TYPE_VISUAL_MPEG2_2_MP: u8 = 0x61; // Visual ISO/IEC 13818-2 Main Profile
    const OBJ_TYPE_VISUAL_MPEG2_2_SNR: u8 = 0x62; // Visual ISO/IEC 13818-2 SNR Profile
    const OBJ_TYPE_VISUAL_MPEG2_2_SPATIAL: u8 = 0x63; // Visual ISO/IEC 13818-2 Spatial Profile
    const OBJ_TYPE_VISUAL_MPEG2_2_HP: u8 = 0x64; // Visual ISO/IEC 13818-2 High Profile
    const OBJ_TYPE_VISUAL_MPEG2_2_422: u8 = 0x65; // Visual ISO/IEC 13818-2 422 Profile

    // MPEG4 video
    const OBJ_TYPE_VISUAL_MPEG4_2: u8 = 0x20; // Visual ISO/IEC 14496-2

    // H264
    const OBJ_TYPE_VISUAL_AVC1: u8 = 0x21; // ISO/IEC 14496-10

    // HEVC
    const OBJ_TYPE_VISUAL_HEVC1: u8 = 0x23; // Visual ISO/IEC 23008-2

    // VP9
    const OBJ_TYPE_VISUAL_VP09: u8 = 0xb1;

    let codec_id = match obj_type {
        OBJ_TYPE_AUDIO_MPEG4_3 | OBJ_TYPE_AUDIO_MPEG2_7_LC | OBJ_TYPE_AUDIO_MPEG2_7_MAIN => {
            CodecId::Audio(CODEC_ID_AAC)
        }
        OBJ_TYPE_AUDIO_MPEG2_3 | OBJ_TYPE_AUDIO_MPEG1_3 => CodecId::Audio(CODEC_ID_MP3),
        OBJ_TYPE_AUDIO_AC3 => CodecId::Audio(CODEC_ID_AC3),
        OBJ_TYPE_AUDIO_EAC3 => CodecId::Audio(CODEC_ID_EAC3),
        OBJ_TYPE_AUDIO_DTS => CodecId::Audio(CODEC_ID_DCA),
        OBJ_TYPE_VISUAL_MPEG2_2_SP
        | OBJ_TYPE_VISUAL_MPEG2_2_MP
        | OBJ_TYPE_VISUAL_MPEG2_2_SNR
        | OBJ_TYPE_VISUAL_MPEG2_2_SPATIAL
        | OBJ_TYPE_VISUAL_MPEG2_2_HP
        | OBJ_TYPE_VISUAL_MPEG2_2_422 => CodecId::Video(CODEC_ID_MPEG2),
        OBJ_TYPE_VISUAL_MPEG4_2 => CodecId::Video(CODEC_ID_MPEG4),
        OBJ_TYPE_VISUAL_AVC1 => CodecId::Video(CODEC_ID_H264),
        OBJ_TYPE_VISUAL_HEVC1 => CodecId::Video(CODEC_ID_HEVC),
        OBJ_TYPE_VISUAL_VP09 => CodecId::Video(CODEC_ID_VP9),
        _ => {
            debug!("unknown object type indication {:#x} for decoder config descriptor", obj_type);
            return None;
        }
    };

    Some(codec_id)
}

pub trait ObjectDescriptor: Sized {
    fn read<B: ReadBytes>(reader: &mut B, len: u32) -> Result<Self>;
}

/*
class ES_Descriptor extends BaseDescriptor : bit(8) tag=ES_DescrTag {
    bit(16) ES_ID;
    bit(1) streamDependenceFlag;
    bit(1) URL_Flag;
    bit(1) OCRstreamFlag;
    bit(5) streamPriority;
    if (streamDependenceFlag)
        bit(16) dependsOn_ES_ID;
    if (URL_Flag) {
        bit(8) URLlength;
        bit(8) URLstring[URLlength];
    }
    if (OCRstreamFlag)
        bit(16) OCR_ES_Id;
    DecoderConfigDescriptor decConfigDescr;
    SLConfigDescriptor slConfigDescr;
    IPI_DescrPointer ipiPtr[0 .. 1];
    IP_IdentificationDataSet ipIDS[0 .. 255];
    IPMP_DescriptorPointer ipmpDescrPtr[0 .. 255];
    LanguageDescriptor langDescr[0 .. 255];
    QoS_Descriptor qosDescr[0 .. 1];
    RegistrationDescriptor regDescr[0 .. 1];
    ExtensionDescriptor extDescr[0 .. 255];
}
*/

#[allow(dead_code)]
#[derive(Debug)]
pub struct ESDescriptor {
    pub es_id: u16,
    pub dec_config: DecoderConfigDescriptor,
    pub sl_config: SLDescriptor,
}

impl ObjectDescriptor for ESDescriptor {
    fn read<B: ReadBytes>(reader: &mut B, len: u32) -> Result<Self> {
        let es_id = reader.read_be_u16()?;
        let es_flags = reader.read_u8()?;

        // Stream dependence flag.
        if es_flags & 0x80 != 0 {
            let _depends_on_es_id = reader.read_u16()?;
        }

        // URL flag.
        if es_flags & 0x40 != 0 {
            let url_len = reader.read_u8()?;
            reader.ignore_bytes(u64::from(url_len))?;
        }

        // OCR stream flag.
        if es_flags & 0x20 != 0 {
            let _ocr_es_id = reader.read_u16()?;
        }

        let mut dec_config = None;
        let mut sl_config = None;

        let mut scoped = ScopedStream::new(reader, u64::from(len) - 3);

        // Multiple descriptors follow, but only the decoder configuration descriptor is useful.
        while scoped.bytes_available() > MIN_DESCRIPTOR_SIZE {
            let (desc, desc_len) = read_descriptor_header(&mut scoped)?;

            match desc {
                DECODER_CONFIG_DESCRIPTOR => {
                    dec_config = Some(DecoderConfigDescriptor::read(&mut scoped, desc_len)?);
                }
                SL_CONFIG_DESCRIPTOR => {
                    sl_config = Some(SLDescriptor::read(&mut scoped, desc_len)?);
                }
                _ => {
                    debug!("skipping {} object in es descriptor", desc);
                    scoped.ignore_bytes(u64::from(desc_len))?;
                }
            }
        }

        // Consume remaining bytes.
        scoped.ignore()?;

        // Decoder configuration descriptor is mandatory.
        if dec_config.is_none() {
            return decode_error("isomp4: missing decoder config descriptor");
        }

        // SL descriptor is mandatory.
        if sl_config.is_none() {
            return decode_error("isomp4: missing sl config descriptor");
        }

        Ok(ESDescriptor { es_id, dec_config: dec_config.unwrap(), sl_config: sl_config.unwrap() })
    }
}

/*
class DecoderConfigDescriptor extends BaseDescriptor : bit(8) tag=DecoderConfigDescrTag {
    bit(8) objectTypeIndication;
    bit(6) streamType;
    bit(1) upStream;
    const bit(1) reserved=1;
    bit(24) bufferSizeDB;
    bit(32) maxBitrate;
    bit(32) avgBitrate;
    DecoderSpecificInfo decSpecificInfo[0 .. 1];
    profileLevelIndicationIndexDescriptor profileLevelIndicationIndexDescr [0..255];
}
*/

#[allow(dead_code)]
#[derive(Debug)]
pub struct DecoderConfigDescriptor {
    pub object_type_indication: u8,
    pub dec_specific_info: Option<DecoderSpecificInfo>,
}

impl ObjectDescriptor for DecoderConfigDescriptor {
    fn read<B: ReadBytes>(reader: &mut B, len: u32) -> Result<Self> {
        let object_type_indication = reader.read_u8()?;

        let (_stream_type, _upstream) = {
            let val = reader.read_u8()?;

            if val & 0x1 != 1 {
                debug!("decoder config descriptor reserved bit is not 1");
            }

            ((val & 0xfc) >> 2, (val & 0x2) >> 1)
        };

        let _buffer_size = reader.read_be_u24()?;
        let _max_bitrate = reader.read_be_u32()?;
        let _avg_bitrate = reader.read_be_u32()?;

        let mut dec_specific_config = None;

        let mut scoped = ScopedStream::new(reader, u64::from(len) - 13);

        // Multiple descriptors follow, but only the decoder specific info descriptor is useful.
        while scoped.bytes_available() > MIN_DESCRIPTOR_SIZE {
            let (desc, desc_len) = read_descriptor_header(&mut scoped)?;

            match desc {
                DECODER_SPECIFIC_DESCRIPTOR => {
                    dec_specific_config = Some(DecoderSpecificInfo::read(&mut scoped, desc_len)?);
                }
                _ => {
                    debug!("skipping {} object in decoder config descriptor", desc);
                    scoped.ignore_bytes(u64::from(desc_len))?;
                }
            }
        }

        // Consume remaining bytes.
        scoped.ignore()?;

        Ok(DecoderConfigDescriptor {
            object_type_indication,
            dec_specific_info: dec_specific_config,
        })
    }
}

#[derive(Debug)]
pub struct DecoderSpecificInfo {
    pub extra_data: Box<[u8]>,
}

impl ObjectDescriptor for DecoderSpecificInfo {
    fn read<B: ReadBytes>(reader: &mut B, len: u32) -> Result<Self> {
        Ok(DecoderSpecificInfo { extra_data: reader.read_boxed_slice_exact(len as usize)? })
    }
}

/*
class SLConfigDescriptor extends BaseDescriptor : bit(8) tag=SLConfigDescrTag {
    bit(8) predefined;
    if (predefined==0) {
        bit(1) useAccessUnitStartFlag;
        bit(1) useAccessUnitEndFlag;
        bit(1) useRandomAccessPointFlag;
        bit(1) hasRandomAccessUnitsOnlyFlag;
        bit(1) usePaddingFlag;
        bit(1) useTimeStampsFlag;
        bit(1) useIdleFlag;
        bit(1) durationFlag;
        bit(32) timeStampResolution;
        bit(32) OCRResolution;
        bit(8) timeStampLength; // must be  64
        bit(8) OCRLength; // must be  64
        bit(8) AU_Length; // must be  32
        bit(8) instantBitrateLength;
        bit(4) degradationPriorityLength;
        bit(5) AU_seqNumLength; // must be  16
        bit(5) packetSeqNumLength; // must be  16
        bit(2) reserved=0b11;
    }
    if (durationFlag) {
        bit(32) timeScale;
        bit(16) accessUnitDuration;
        bit(16) compositionUnitDuration;
    }
    if (!useTimeStampsFlag) {
        bit(timeStampLength) startDecodingTimeStamp;
        bit(timeStampLength) startCompositionTimeStamp;
    }
}

timeStampLength == 32, for predefined == 0x1
timeStampLength == 0,  for predefined == 0x2
*/
#[derive(Debug)]
pub struct SLDescriptor;

impl ObjectDescriptor for SLDescriptor {
    fn read<B: ReadBytes>(reader: &mut B, _len: u32) -> Result<Self> {
        // const SLCONFIG_PREDEFINED_CUSTOM: u8 = 0x0;
        // const SLCONFIG_PREDEFINED_NULL: u8 = 0x1;
        const SLCONFIG_PREDEFINED_MP4: u8 = 0x2;

        let predefined = reader.read_u8()?;

        if predefined != SLCONFIG_PREDEFINED_MP4 {
            return unsupported_error("isomp4: sl descriptor predefined not mp4");
        }

        Ok(SLDescriptor {})
    }
}
