// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::codecs::video::{VideoExtraData, VIDEO_EXTRA_DATA_ID_NULL};
use symphonia_core::codecs::CodecId;
use symphonia_core::errors::{decode_error, Error, Result};
use symphonia_core::io::ReadBytes;

use symphonia_common::mpeg::formats::*;

use crate::atoms::stsd::{AudioSampleEntry, VisualSampleEntry};
use crate::atoms::{Atom, AtomHeader};

/// Elementary stream descriptor atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct EsdsAtom {
    /// Elementary stream descriptor.
    es_desc: ESDescriptor,
}

impl Atom for EsdsAtom {
    fn read<B: ReadBytes>(reader: &mut B, mut header: AtomHeader) -> Result<Self> {
        let (_, _) = header.read_extended_header(reader)?;

        let ds_size = header
            .data_len()
            .ok_or(Error::DecodeError("isomp4 (esds): expected atom size to be known"))?;

        let mut descriptor = None;

        // The ES descriptor occupies the rest of the atom.
        if ds_size > MIN_OBJECT_DESCRIPTOR_SIZE {
            // Read the object descriptor header.
            let (desc_tag, desc_len) = read_object_descriptor_header(reader)?;

            // Read the ES descriptor.
            if desc_tag == ClassTag::EsDescriptor {
                descriptor = Some(ESDescriptor::read(reader, desc_len)?);
            }
        }

        if descriptor.is_none() {
            return decode_error("isomp4: missing es descriptor in esds");
        }

        Ok(EsdsAtom { es_desc: descriptor.unwrap() })
    }
}

impl EsdsAtom {
    /// If the elementary stream descriptor describes an audio stream, populate the provided
    /// audio sample entry.
    pub fn fill_audio_sample_entry(&self, entry: &mut AudioSampleEntry) -> Result<()> {
        match codec_id_from_object_type_indication(self.es_desc.dec_config.object_type_indication) {
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

        if let Some(ds_config) = &self.es_desc.dec_config.dec_specific_info {
            entry.extra_data = Some(ds_config.extra_data.clone());
        }

        Ok(())
    }

    /// If the elementary stream descriptor describes an video stream, populate the provided
    /// video sample entry.
    pub fn fill_video_sample_entry(&self, entry: &mut VisualSampleEntry) -> Result<()> {
        match codec_id_from_object_type_indication(self.es_desc.dec_config.object_type_indication) {
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

        if let Some(ds_config) = &self.es_desc.dec_config.dec_specific_info {
            entry.extra_data.push(VideoExtraData {
                id: VIDEO_EXTRA_DATA_ID_NULL,
                data: ds_config.extra_data.clone(),
            });
        }

        Ok(())
    }
}
