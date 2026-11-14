// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_common::xiph::audio::opus;
use symphonia_core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia_core::errors::Error;
use symphonia_core::io::BufReader;

use crate::atoms::stsd::AudioSampleEntry;
use crate::atoms::{
    Atom, AtomHeader, AtomIterator, ReadAtom, Result, decode_error, unsupported_error,
};

const OPUS_MAGIC: &[u8] = b"OpusHead";
const OPUS_MAGIC_LEN: usize = OPUS_MAGIC.len();

/// Byte ranges of the multi-byte `dOps` fields in the synthesized extra data: `PreSkip` (16-bit),
/// `InputSampleRate` (32-bit) and `OutputGain` (16-bit). They follow the magic signature, the
/// version and the output channel count.
///
/// The Opus Specific Box stores all its fields in big-endian order
/// ([Encapsulation of Opus in ISO Base Media File Format, Section 4.3.2](<https://opus-codec.org/docs/opus_in_isobmff.html#4.3.2>)),
/// whereas the `OpusHead` identification header that the extra data imitates is little-endian
/// ([RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)).
/// The fields are byte-swapped so the extra data is a genuine `OpusHead` for every consumer.
const OPUS_DOPS_BIG_ENDIAN_FIELDS: [core::ops::Range<usize>; 3] = [
    OPUS_MAGIC_LEN + 2..OPUS_MAGIC_LEN + 4,
    OPUS_MAGIC_LEN + 4..OPUS_MAGIC_LEN + 8,
    OPUS_MAGIC_LEN + 8..OPUS_MAGIC_LEN + 10,
];

/// Opus atom.
#[allow(dead_code)]
#[derive(Debug)]
pub struct OpusAtom {
    /// Opus extra data (identification header).
    extra_data: Box<[u8]>,
}

impl Atom for OpusAtom {
    fn read<R: ReadAtom>(reader: &mut AtomIterator<R>, header: &AtomHeader) -> Result<Self> {
        const MIN_OPUS_EXTRA_DATA_SIZE: usize = OPUS_MAGIC_LEN + 11;
        const MAX_OPUS_EXTRA_DATA_SIZE: usize = MIN_OPUS_EXTRA_DATA_SIZE + 257;

        // Offset of the Opus version number in the extra data.
        const OPUS_EXTRADATA_VERSION_OFFSET: usize = OPUS_MAGIC_LEN;

        // The dops atom contains an Opus identification header excluding the OpusHead magic
        // signature. Therefore, the atom data length should be atleast as long as the shortest
        // Opus identification header.
        let data_len = header
            .data_size()
            .ok_or(Error::DecodeError("isomp4 (opus): expected atom size to be known"))?
            as usize;

        if data_len < MIN_OPUS_EXTRA_DATA_SIZE - OPUS_MAGIC_LEN {
            return decode_error("isomp4 (opus): opus identification header too short");
        }

        if data_len > MAX_OPUS_EXTRA_DATA_SIZE - OPUS_MAGIC_LEN {
            return decode_error("isomp4 (opus): opus identification header too large");
        }

        let mut extra_data = vec![0; OPUS_MAGIC_LEN + data_len].into_boxed_slice();

        // The Opus magic is excluded in the atom, but the extra data must start with it.
        extra_data[..OPUS_MAGIC_LEN].copy_from_slice(OPUS_MAGIC);

        // Read the extra data from the atom.
        reader.read_buf_exact(&mut extra_data[OPUS_MAGIC_LEN..])?;

        // Verify the version number is 0.
        if extra_data[OPUS_EXTRADATA_VERSION_OFFSET] != 0 {
            return unsupported_error("isomp4 (opus): unsupported opus version");
        }

        // Convert the big-endian dOps fields to the little-endian OpusHead layout.
        for field in OPUS_DOPS_BIG_ENDIAN_FIELDS {
            extra_data[field].reverse();
        }

        Ok(OpusAtom { extra_data })
    }
}

impl OpusAtom {
    pub fn fill_audio_sample_entry(self, entry: &mut AudioSampleEntry) {
        let mut reader = BufReader::new(&self.extra_data);

        if let Ok(opus_head) = opus::OpusHead::read(&mut reader, 0) {
            entry.channels = Some(opus_head.channels);
            entry.sample_rate = 48_000.0;
        }

        entry.codec_id = CODEC_ID_OPUS;
        entry.extra_data = Some(self.extra_data);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use symphonia_core::io::MediaSourceStream;

    use super::*;

    /// Reads one `dOps` atom with the given big-endian payload and returns the extra data, or
    /// `None` when the atom is rejected.
    fn read_dops(payload: &[u8]) -> Option<Box<[u8]>> {
        let mut atom = Vec::with_capacity(AtomHeader::HEADER_SIZE as usize + payload.len());
        atom.extend_from_slice(
            &((AtomHeader::HEADER_SIZE as usize + payload.len()) as u32).to_be_bytes(),
        );
        atom.extend_from_slice(b"dOps");
        atom.extend_from_slice(payload);

        let source = MediaSourceStream::new(Box::new(Cursor::new(atom)), Default::default());
        let mut it = AtomIterator::new(source, None);
        it.next_header().ok()??;
        Some(it.read_atom::<OpusAtom>().ok()?.extra_data)
    }

    /// [Encapsulation of Opus in ISO Base Media File Format, Section 4.3.2](<https://opus-codec.org/docs/opus_in_isobmff.html#4.3.2>):
    /// all `dOps` fields are big-endian, while
    /// [RFC 7845 Section 5.1](<https://datatracker.ietf.org/doc/html/rfc7845#section-5.1>)
    /// defines `OpusHead` as little-endian.
    #[test]
    fn big_endian_dops_fields_become_little_endian_opus_head() {
        let dops: [u8; 11] = [
            0, // Version
            2, // OutputChannelCount
            0x01, 0x38, // PreSkip = 312
            0x00, 0x00, 0xbb, 0x80, // InputSampleRate = 48000
            0xff, 0x00, // OutputGain = -256 (-1.0 dB in Q7.8)
            0,    // ChannelMappingFamily
        ];
        let extra_data = read_dops(&dops).unwrap();

        let expected: [u8; 19] = [
            b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', 0, 2, 0x38, 0x01, 0x80, 0xbb, 0x00,
            0x00, 0x00, 0xff, 0,
        ];
        assert_eq!(&*extra_data, &expected);

        let opus_head = opus::OpusHead::read(&mut BufReader::new(&extra_data), 0).unwrap();
        assert_eq!(opus_head.pre_skip, 312);
        assert_eq!(opus_head.original_sample_rate, 48_000);
        assert_eq!(opus_head.gain, -256);
    }

    /// The channel mapping table that follows the fixed fields is byte-oriented and is copied
    /// unchanged.
    #[test]
    fn channel_mapping_table_is_preserved() {
        let dops: [u8; 16] = [
            0, 3, 0x01, 0x38, 0x00, 0x00, 0xbb, 0x80, 0x00, 0x00, 1, // Fixed fields, family 1
            2, 1, 0, 2, 1, // StreamCount, CoupledCount, ChannelMapping
        ];
        let extra_data = read_dops(&dops).unwrap();
        assert_eq!(&extra_data[OPUS_MAGIC_LEN + 11..], &[2, 1, 0, 2, 1]);
    }

    #[test]
    fn non_zero_dops_version_is_rejected() {
        let dops: [u8; 11] = [1, 2, 0x01, 0x38, 0x00, 0x00, 0xbb, 0x80, 0x00, 0x00, 0];
        assert!(read_dops(&dops).is_none());
    }
}
