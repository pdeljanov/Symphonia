// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::errors::{Error, unsupported_error};
use symphonia_core::support_format;

use symphonia_core::audio::{Channels, layouts};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::codecs::audio::well_known::CODEC_ID_DCA;
use symphonia_core::errors::{Result, decode_error};
use symphonia_core::formats::prelude::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::FORMAT_ID_NULL;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};

const DCA_FORMAT_INFO: FormatInfo = FormatInfo {
    format: FORMAT_ID_NULL,
    short_name: "dca",
    long_name: "DTS Coherent Acoustics",
};

pub struct DcaReader<'s> {
    reader: MediaSourceStream<'s>,
    tracks: Vec<Track>,
    chapters: Option<ChapterGroup>,
    metadata: MetadataLog,
    next_packet_ts: Timestamp,
}

impl<'s> DcaReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let header = DcaHeader::read(&mut mss)?;

        // Rewind back to the start of the frame.
        mss.seek_buffered_rev(header.header_len as usize);

        let mut codec_params = AudioCodecParameters::new();
        codec_params.for_codec(CODEC_ID_DCA).with_sample_rate(header.sample_rate);

        if let Some(channels) = header.channels {
            codec_params.with_channels(channels);
        }

        let mut track = Track::new(0);
        track.with_codec_params(CodecParameters::Audio(codec_params));

        Ok(DcaReader {
            reader: mss,
            tracks: vec![track],
            chapters: opts.external_data.chapters,
            metadata: opts.external_data.metadata.unwrap_or_default(),
            next_packet_ts: Timestamp::new(0),
        })
    }
}

impl Scoreable for DcaReader<'_> {
    fn score(mut src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        let _ = DcaHeader::read(&mut src)?;
        Ok(Score::Supported(127))
    }
}

pub struct DcaHeader {
    pub sample_rate: u32,
    pub channels: Option<Channels>,
    pub frame_len: u32,
    pub header_len: u32,
    pub samples_per_frame: u32,
}

impl DcaHeader {
    pub fn read<B: ReadBytes>(reader: &mut B) -> Result<Self> {
        let mut sync = 0u32;
        
        // Resync to the next sync word.
        loop {
            sync = (sync << 8) | u32::from(reader.read_u8()?);
            if sync == 0x7FFE8001 {
                break;
            }
        }

        // Basic header parsing (minimal)
        // NBLKS: 7 bits, FSIZE: 14 bits, AMODE: 6 bits, SFREQ: 4 bits, RATE: 5 bits
        // We'll read the next 8 bytes to get some of these.
        let mut buf = [0u8; 8];
        reader.read_buf_exact(&mut buf)?;

        let mut bs = BitReaderLtr::new(&buf);
        
        // Frame Type: 1 bit (ignored)
        bs.ignore_bit()?;
        // Deficit Samples: 5 bits (ignored)
        bs.ignore_bits(5)?;
        // CPF: 1 bit (ignored)
        bs.ignore_bit()?;
        // NBLKS: 7 bits
        let nblks = bs.read_bits_leq32(7)?;
        // FSIZE: 14 bits
        let fsize = bs.read_bits_leq32(14)? + 1;
        // AMODE: 6 bits
        let amode = bs.read_bits_leq32(6)?;
        // SFREQ: 4 bits
        let sfreq_idx = bs.read_bits_leq32(4)?;

        let sample_rate = match sfreq_idx {
            1 => 8000,
            2 => 16000,
            3 => 32000,
            6 => 11025,
            7 => 22050,
            8 => 44100,
            11 => 12000,
            12 => 24000,
            13 => 48000,
            _ => return decode_error("dca: invalid sample rate index"),
        };

        let channels = match amode {
            0 => Some(layouts::CHANNEL_LAYOUT_MONO),
            2 => Some(layouts::CHANNEL_LAYOUT_STEREO),
            9 => Some(layouts::CHANNEL_LAYOUT_5P1), // L, C, R, LS, RS + LFE is handled separately
            _ => None, // TODO: Add more layouts
        };

        Ok(DcaHeader {
            sample_rate,
            channels,
            frame_len: fsize,
            header_len: 12, // Sync (4) + 8 bytes read
            samples_per_frame: (nblks + 1) * 32,
        })
    }
}

impl ProbeableFormat<'_> for DcaReader<'_> {
    fn try_probe_new(mss: MediaSourceStream<'_>, opts: FormatOptions) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(DcaReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[support_format!(
            DCA_FORMAT_INFO,
            &["dts", "dca"],
            &["audio/dts", "audio/vnd.dts"],
            &[&[0x7f, 0xfe, 0x80, 0x01]]
        )]
    }
}

impl FormatReader for DcaReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &DCA_FORMAT_INFO
    }
    fn next_packet(&mut self) -> Result<Option<Packet>> {
        let header = match DcaHeader::read(&mut self.reader) {
            Ok(header) => header,
            Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(err) => return Err(err),
        };

        // Rewind back to the start of the frame (including sync word).
        self.reader.seek_buffered_rev(header.header_len as usize);

        let ts = self.next_packet_ts;
        let duration = Duration::from(u64::from(header.samples_per_frame)); 

        self.next_packet_ts = match self.next_packet_ts.checked_add(duration) {
            Some(ts) => ts,
            None => return Ok(None),
        };

        Ok(Some(Packet::new(
            0,
            ts,
            duration,
            self.reader.read_boxed_slice_exact(header.frame_len as usize)?,
        )))
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn chapters(&self) -> Option<&ChapterGroup> {
        self.chapters.as_ref()
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn seek(&mut self, _mode: SeekMode, _to: SeekTo) -> Result<SeekedTo> {
        unsupported_error("dca: seek not implemented")
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}
