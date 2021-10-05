// Symphonia
// Copyright (c) 2019-2021 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]
// The following lints are allowed in all Symphonia crates. Please see clippy.toml for their
// justification.
#![allow(clippy::comparison_chain)]
#![allow(clippy::excessive_precision)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_range_contains)]
// Disable to better express the specification.
#![allow(clippy::collapsible_else_if)]

use symphonia_core::audio::AudioBufferRef;
use symphonia_core::codecs::{
    CodecDescriptor, CodecParameters, Decoder, DecoderOptions, FinalizeResult, CODEC_TYPE_OPUS,
};
use symphonia_core::errors::{decode_error, unsupported_error, Result};
use symphonia_core::formats::Packet;
use symphonia_core::io::{BufReader, ReadBytes};
use symphonia_core::support_codec;

#[allow(dead_code)]
pub struct OpusDecoder {
    ident_header: IdentificationHeader,
    params: CodecParameters,
}

impl Decoder for OpusDecoder {
    fn try_new(params: &CodecParameters, _: &DecoderOptions) -> Result<Self> {
        let extra_data = match params.extra_data.as_ref() {
            Some(buf) => buf,
            _ => return unsupported_error("opus: missing extra data"),
        };

        let mut reader = BufReader::new(extra_data);

        let ident_header = read_ident_header(&mut reader)?;
        Ok(OpusDecoder {
            ident_header,
            params: params.clone(),
        })
    }

    fn supported_codecs() -> &'static [CodecDescriptor] {
        &[support_codec!(CODEC_TYPE_OPUS, "opus", "Opus")]
    }

    fn reset(&mut self) {
        unimplemented!()
    }

    fn codec_params(&self) -> &CodecParameters {
        &self.params
    }

    #[allow(unused_variables)]
    fn decode(&mut self, packet: &Packet) -> Result<AudioBufferRef<'_>> {
        unimplemented!()
    }

    fn finalize(&mut self) -> FinalizeResult {
        unimplemented!()
    }
}

#[derive(Debug)]
pub struct IdentificationHeader {
    pub output_channel_count: u8,
    pub pre_skip: u16,
    pub input_sample_rate: u32,
    pub output_gain: u16,
    pub channel_mapping_family: u8,
    pub stream_count: u8,
    pub coupled_stream_count: u8,
    pub channel_mapping: [u8; 8],
}

/** Create an IdentificationHeader from \a reader.
 *
 * If the header is invalid, a DecodeError is returned.
 *
 * See RFC 7845 Section 5.1, https://tools.ietf.org/pdf/rfc7845.pdf.
 */
fn read_ident_header<B: ReadBytes>(reader: &mut B) -> Result<IdentificationHeader> {
    // The first 8 bytes are the magic signature ASCII bytes.
    const OGG_OPUS_MAGIC_SIGNATURE: &[u8] = b"OpusHead";

    let mut magic_signature = [0; 8];
    reader.read_buf_exact(&mut magic_signature)?;

    if magic_signature != *OGG_OPUS_MAGIC_SIGNATURE {
        return decode_error("incorrect opus signature");
    }

    // The next byte is the OGG Opus encapsulation version.
    const OGG_OPUS_VERSION: u8 = 0x01;

    let mut version = [0; 1];
    reader.read_buf_exact(&mut version)?;

    // TODO: Allow version numbers that are < 15 and disallow all > 16.
    // See RFC 7845 Section 5.1 (Version).
    if version[0] != OGG_OPUS_VERSION {
        return decode_error("incorrect opus version");
    }

    // The next byte is the number of channels/
    let output_channel_count = reader.read_byte()?;

    if output_channel_count == 0 {
        return decode_error("output channel count is 0");
    }

    // The next 16-bit integer is the pre-skip padding.
    let pre_skip = reader.read_u16()?;

    // The next 32-bit integer is the sample rate of the original audio.
    let input_sample_rate = reader.read_u32()?;

    // Next, the 16-bit gain value.
    let output_gain = reader.read_u16()?;

    // The next byte indicates the channel mapping. Most of these values are reserved.
    let channel_mapping_family = reader.read_byte()?;

    let (stream_count, coupled_stream_count) = match channel_mapping_family {
        // RTP mapping. Supports up-to 2 channels.
        0 => {
            if output_channel_count > 2 {
                return decode_error("invalid output channel count");
            }

            (1, output_channel_count - 1)
        }
        // Vorbis mapping. Supports 1 to 8 channels.
        1 => {
            if output_channel_count > 8 {
                return decode_error("invalid output channel count");
            }

            let stream_count = reader.read_u8()?;
            if stream_count == 0 {
                return decode_error("stream count is 0");
            }

            let coupled_stream_count = reader.read_u8()?;
            (stream_count, coupled_stream_count)
        }
        _ => return decode_error("reserved mapping family"),
    };

    if stream_count.checked_add(coupled_stream_count).is_none() {
        return decode_error("stream count + coupled stream count > 255");
    }

    let mut channel_mapping = [0; 8];

    // The channel mapping table is only read if not using the RTP mapping.
    if channel_mapping_family != 0 {
        for mapping in &mut channel_mapping[..output_channel_count as usize] {
            *mapping = reader.read_u8()?;
        }
    }

    Ok(IdentificationHeader {
        output_channel_count,
        pre_skip,
        input_sample_rate,
        output_gain,
        channel_mapping_family,
        stream_count,
        coupled_stream_count,
        channel_mapping,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_err_if_no_magic_signature() {
        let bytes: [u8; 23] = [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x09, 0x38, 0x01, 0x80, 0xbb,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x4f, 0x67, 0x67, 0x53,
        ];
        let mut reader = BufReader::new(&bytes);
        let result = read_ident_header(&mut reader);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "malformed stream: incorrect opus signature"
        );
    }

    #[test]
    fn verify_err_if_version_number_neq_1() {
        let bytes: [u8; 23] = [
            0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 0x02, 0x09, 0x38, 0x01, 0x80, 0xbb,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x4f, 0x67, 0x67, 0x53,
        ];
        let mut reader = BufReader::new(&bytes);
        let result = read_ident_header(&mut reader);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "malformed stream: incorrect opus version"
        );
    }

    #[test]
    fn verify_err_if_channel_count_eq_0() {
        let bytes: [u8; 23] = [
            0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 0x01, 0x00, 0x38, 0x01, 0x80, 0xbb,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x4f, 0x67, 0x67, 0x53,
        ];
        let mut reader = BufReader::new(&bytes);
        let result = read_ident_header(&mut reader);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "malformed stream: output channel count is 0"
        );
    }

    #[test]
    fn verify_err_if_channel_family_gt_2() {
        let bytes: [u8; 23] = [
            0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 0x01, 0x02, 0x38, 0x01, 0x80, 0xbb,
            0x00, 0x00, 0x00, 0x00, 0x02, 0x4f, 0x67, 0x67, 0x53,
        ];
        let mut reader = BufReader::new(&bytes);
        let result = read_ident_header(&mut reader);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "malformed stream: reserved mapping family"
        );
    }

    #[test]
    fn verify_err_if_channel_family_eq_0_and_channel_count_gt_2() {
        let bytes: [u8; 23] = [
            0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 0x01, 0x03, 0x38, 0x01, 0x80, 0xbb,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x4f, 0x67, 0x67, 0x53,
        ];

        let mut reader = BufReader::new(&bytes);
        let result = read_ident_header(&mut reader);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "malformed stream: invalid output channel count"
        );
    }

    #[test]
    fn verify_err_if_channel_family_eq_1_and_channel_count_gt_8() {
        let bytes: [u8; 23] = [
            0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 0x01, 0x09, 0x38, 0x01, 0x80, 0xbb,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x4f, 0x67, 0x67, 0x53,
        ];

        let mut reader = BufReader::new(&bytes);
        let result = read_ident_header(&mut reader);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "malformed stream: invalid output channel count"
        );
    }

    #[test]
    fn verify_err_if_channel_family_eq_1_and_stream_count_eq_0() {
        let bytes: [u8; 23] = [
            0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 0x01, 0x02, 0x38, 0x01, 0x80, 0xbb,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x67, 0x67, 0x53,
        ];
        let mut reader = BufReader::new(&bytes);
        let result = read_ident_header(&mut reader);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "malformed stream: stream count is 0"
        );
    }

    #[test]
    fn verify_err_if_channel_family_eq_1_and_stream_counts_sum_gt_255() {
        let bytes: [u8; 23] = [
            0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 0x01, 0x02, 0x38, 0x01, 0x80, 0xbb,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0xFF, 0x67, 0x53,
        ];
        let mut reader = BufReader::new(&bytes);
        let result = read_ident_header(&mut reader);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "malformed stream: stream count + coupled stream count > 255"
        );
    }
}
