// Symphonia
// Copyright (c) 2019-2021 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::support_format;

use symphonia_core::checksum::Crc16AnsiLe;
use symphonia_core::codecs::{CodecParameters, CODEC_TYPE_MP3};
use symphonia_core::errors::{Result, SeekErrorKind, seek_error};
use symphonia_core::formats::prelude::*;
use symphonia_core::io::*;
use symphonia_core::meta::{Metadata, MetadataLog};
use symphonia_core::probe::{Descriptor, Instantiate, QueryDescriptor};

use std::io::{Seek, SeekFrom};

use log::{debug, info, warn};

use super::common::{ChannelMode, FrameHeader, MpegVersion, SAMPLES_PER_GRANULE};
use super::header;

/// MPEG1 and MPEG2 audio frame reader.
///
/// `Mp3Reader` implements a demuxer for the MPEG1 and MPEG2 audio frame format.
pub struct Mp3Reader {
    reader: MediaSourceStream,
    tracks: Vec<Track>,
    cues: Vec<Cue>,
    metadata: MetadataLog,
    first_frame_pos: u64,
    next_packet_ts: u64,
}

impl QueryDescriptor for Mp3Reader {
    fn query() -> &'static [Descriptor] {
        &[
            // Layer 3
            support_format!(
                "mp3",
                "MPEG Audio Layer 3 Native",
                &[ "mp3" ],
                &[ "audio/mp3" ],
                &[
                    &[ 0xff, 0xfa ], &[ 0xff, 0xfb ], // MPEG 1
                    &[ 0xff, 0xf2 ], &[ 0xff, 0xf3 ], // MPEG 2
                    &[ 0xff, 0xe2 ], &[ 0xff, 0xe3 ], // MPEG 2.5
                ]),
        ]
    }

    fn score(_context: &[u8]) -> u8 {
        255
    }
}

impl FormatReader for Mp3Reader {

    fn try_new(mut source: MediaSourceStream, _options: &FormatOptions) -> Result<Self> {
        // Try to read the first MPEG frame.
        let (header, packet) = read_mpeg_frame(&mut source)?;

        // Use the header to populate the codec parameters.
        let mut params = CodecParameters::new();

        params.for_codec(CODEC_TYPE_MP3)
              .with_sample_rate(header.sample_rate)
              .with_time_base(TimeBase::new(1, header.sample_rate))
              .with_channels(header.channel_mode.channels());

        let audio_frames_per_mpeg_frame = SAMPLES_PER_GRANULE * header.n_granules() as u64;

        // Check if there is a Xing/Info tag contained in the first frame.
        if let Some(info_tag) = try_read_info_tag(&packet, &header) {
            // The base Xing/Info tag may contain the number of frames.
            if let Some(n_mpeg_frames) = info_tag.num_frames {
                params.with_n_frames(u64::from(n_mpeg_frames) * audio_frames_per_mpeg_frame);
            }

            // The LAME tag contains ReplayGain and padding information.
            if let Some(lame_tag) = info_tag.lame {
                params.with_leading_padding(lame_tag.leading_padding)
                      .with_trailing_padding(lame_tag.trailing_padding);
            }
        }
        else {
            // The first frame was not a Xing/Info header, rewind back to the start of the frame so
            // that it may be decoded.
            source.seek_buffered_rev(header.frame_size + 4);

            // Likely not a VBR file, so estimate the duration if seekable.
            if source.is_seekable() {
                info!("estimating duration from bitrate, may be inaccurate for vbr files");

                if let Some(n_mpeg_frames) = estimate_num_mpeg_frames(&mut source) {
                    params.with_n_frames(n_mpeg_frames * audio_frames_per_mpeg_frame);
                }
            }
        }

        let first_frame_pos = source.pos();

        Ok(Mp3Reader {
            reader: source,
            tracks: vec![ Track::new(0, params) ],
            cues: Vec::new(),
            metadata: Default::default(),
            first_frame_pos,
            next_packet_ts: 0,
        })
    }

    fn next_packet(&mut self) -> Result<Packet> {
        let (header, packet) = read_mpeg_frame(&mut self.reader)?;

        // Each frame contains 1 or 2 granules with each granule being exactly 576 samples long.
        let duration = SAMPLES_PER_GRANULE * header.n_granules() as u64;

        let ts = self.next_packet_ts;

        self.next_packet_ts += duration;

        Ok(Packet::new_from_boxed_slice(0, ts, duration, packet.into_boxed_slice()))
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn cues(&self) -> &[Cue] {
        &self.cues
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        const MAX_REF_FRAMES: usize = 4;
        const REF_FRAMES_MASK: usize = MAX_REF_FRAMES - 1;

        // Get the timestamp of the desired audio frame.
        let required_ts = match to {
            // Frame timestamp given.
            SeekTo::TimeStamp { ts, .. } => ts,
            // Time value given, calculate frame timestamp from sample rate.
            SeekTo::Time { time, .. } => {
                // Use the sample rate to calculate the frame timestamp. If sample rate is not
                // known, the seek cannot be completed.
                if let Some(sample_rate) = self.tracks[0].codec_params.sample_rate {
                    TimeBase::new(1, sample_rate).calc_timestamp(time)
                }
                else {
                    return seek_error(SeekErrorKind::Unseekable);
                }
            }
        };

        debug!("seeking to ts={}", required_ts);

        // If the desired timestamp is less-than the next packet timestamp, attempt to seek
        // to the start of the stream.
        if required_ts < self.next_packet_ts {
            // If the reader is not seekable then only forward seeks are possible.
            if self.reader.is_seekable() {
                let seeked_pos = self.reader.seek(SeekFrom::Start(self.first_frame_pos))?;

                // Since the elementary stream has no timestamp information, the position seeked
                // to must be exactly as requested.
                if seeked_pos != self.first_frame_pos {
                    return seek_error(SeekErrorKind::Unseekable);
                }
            }
            else {
                return seek_error(SeekErrorKind::ForwardOnly)
            }

            // Successfuly seeked to the start of the stream, reset the next packet timestamp.
            self.next_packet_ts = 0;
        }

        let mut frames : [FramePos; MAX_REF_FRAMES] = Default::default();
        let mut n_frames = 0;

        // Parse frames from the stream until the frame containing the desired timestamp is
        // reached.
        loop {
            // Parse the next frame header.
            let header = header::parse_frame_header(header::sync_frame(&mut self.reader)?)?;

            // Position of the frame header.
            let frame_pos = self.reader.pos() - core::mem::size_of::<u32>() as u64;

            // Calculate the duration of the frame.
            let duration = SAMPLES_PER_GRANULE * header.n_granules() as u64;

            // Add the frame to the frame ring.
            frames[n_frames & REF_FRAMES_MASK] = FramePos { pos: frame_pos, ts: self.next_packet_ts };
            n_frames += 1;

            // If the next frame's timestamp would exceed the desired timestamp, rewind back to the
            // start of this frame and end the search.
            if self.next_packet_ts + duration > required_ts {
                // The main_data_begin offset is a negative offset from the frame's header to where
                // its main data begins. Therefore, for a decoder to properly decode this frame, the
                // reader must provide previous (reference) frames up-to and including the frame
                // that contains the first byte this frame's main_data.
                let main_data_begin = read_main_data_begin(&mut self.reader, &header)? as u64;

                debug!(
                    "found frame with ts={} @ pos={} with main_data_begin={}",
                    self.next_packet_ts,
                    frame_pos,
                    main_data_begin
                );

                // The number of reference frames is 0 if main_data_begin is also 0. Otherwise,
                // attempt to find the first (oldest) reference frame, then select 1 frame before
                // that one to actually seek to.
                let mut n_ref_frames = 0;
                let mut ref_frame = &frames[(n_frames - 1) & REF_FRAMES_MASK];

                if main_data_begin > 0 {
                    // The maximum number of reference frames is limited to the number of frames
                    // read and the number of previous frames recorded.
                    let max_ref_frames = core::cmp::min(n_frames, frames.len());

                    while n_ref_frames < max_ref_frames {
                        ref_frame = &frames[(n_frames - n_ref_frames - 1) & REF_FRAMES_MASK];

                        if frame_pos - ref_frame.pos >= main_data_begin {
                            break;
                        }

                        n_ref_frames += 1;
                    }

                    debug!(
                        "will seek to ts={} (-{} frames) @ pos={} (-{} bytes)",
                        ref_frame.ts,
                        n_ref_frames,
                        ref_frame.pos,
                        frame_pos - ref_frame.pos
                    );
                }

                // Do the actual seek to the reference frame.
                self.next_packet_ts = ref_frame.ts;
                self.reader.seek_buffered(ref_frame.pos);

                break;
            }

            // Otherwise, ignore the frame body.
            self.reader.ignore_bytes(header.frame_size as u64)?;

            // Increment the timestamp for the next packet.
            self.next_packet_ts += duration;
        }

        debug!("seeked to ts={} (delta={})",
            self.next_packet_ts,
            required_ts as i64 - self.next_packet_ts as i64);

        Ok(SeekedTo {
            track_id: 0,
            required_ts,
            actual_ts: self.next_packet_ts,
        })
    }

    fn into_inner(self: Box<Self>) -> MediaSourceStream {
        self.reader
    }
}

/// Reads a MPEG frame and returns the header and buffer.
#[inline(always)]
fn read_mpeg_frame(reader: &mut MediaSourceStream) -> Result<(FrameHeader, Vec<u8>)> {
    let (header, header_word) = loop {
        // Sync to the next frame header.
        let sync = header::sync_frame(reader)?;

        // Parse the frame header fully.
        if let Ok(header) = header::parse_frame_header(sync) {
            break (header, sync);
        }

        warn!("invalid mpeg audio header");
    };

    // Allocate frame buffer.
    let mut packet = vec![0u8; header.frame_size + 4];
    packet[0..4].copy_from_slice(&header_word.to_be_bytes());

    // Read the frame body.
    reader.read_buf_exact(&mut packet[4..])?;

    // Return the parsed header and packet body.
    Ok((header, packet))
}

#[derive(Default)]
struct FramePos {
    ts: u64,
    pos: u64,
}

/// Reads the main_data_begin field from the side information of a MP3 frame.
fn read_main_data_begin<B: ReadBytes>(reader: &mut B, header: &FrameHeader) -> Result<u16> {
    // After the head the optional CRC is present.
    if header.has_crc {
        let _crc = reader.read_be_u16()?;
    }

    // For MPEG version 1 the first 9 bits is main_data_begin.
    let main_data_begin = if header.is_mpeg1() {
        reader.read_be_u16()? >> 7
    }
    // For MPEG version 2 the first 8 bits is main_data_begin.
    else {
        u16::from(reader.read_u8()?)
    };

    Ok(main_data_begin)
}

/// Estimates the total number of MPEG frames in the media source stream.
fn estimate_num_mpeg_frames(reader: &mut MediaSourceStream) -> Option<u64> {
    const MAX_FRAMES: u32 = 16;
    const MAX_LEN: usize  = 16 * 1024;

    // Macro to convert a Result to Option, and break a loop on exit.
    macro_rules! break_on_err {
        ($expr:expr) => {
            match $expr {
                Ok(a) => a,
                _ => break None,
            }
        };
    }

    let start_pos = reader.pos();

    let mut total_frame_len = 0;
    let mut total_frames = 0;

    let total_len = match reader.byte_len() {
        Some(len) => len - start_pos,
        _ => return None,
    };

    let num_mpeg_frames = loop {
        // Read the frame header.
        let header_val = break_on_err!(reader.read_be_u32());

        // Parse the frame header.
        let header = break_on_err!(header::parse_frame_header(header_val));

        // Tabulate the size.
        total_frame_len += header.frame_size + 4;
        total_frames += 1;

        // Ignore the frame body.
        break_on_err!(reader.ignore_bytes(header.frame_size as u64));

        // Read up-to 16 frames, or 16kB, then calculate the average MPEG frame length, and from
        // that, the total number of MPEG frames.
        if total_frames > MAX_FRAMES || total_frame_len > MAX_LEN {
            let avg_mpeg_frame_len = total_frame_len as f64 / total_frames as f64;
            break Some((total_len as f64 / avg_mpeg_frame_len) as u64);
        }
    };

    // Rewind back to the first frame seen upon entering this function.
    reader.seek_buffered_rev((reader.pos() - start_pos) as usize);

    num_mpeg_frames
}

/// The LAME tag is an extension to the Xing/Info tag.
#[allow(dead_code)]
struct LameTag {
    encoder: String,
    replaygain_peak: Option<f32>,
    replaygain_radio: Option<f32>,
    replaygain_audiophile: Option<f32>,
    trailing_padding: u32,
    leading_padding: u32,
    music_crc: u16,
}

/// The Xing/Info time additional information for regarding a MP3 file.
#[allow(dead_code)]
struct XingInfoTag {
    num_frames: Option<u32>,
    num_bytes: Option<u32>,
    toc: Option<[u8; 100]>,
    quality: Option<u32>,
    is_cbr: bool,
    lame: Option<LameTag>,
}

/// Try to read a Xing/Info tag from the provided MPEG frame.
fn try_read_info_tag(buf: &[u8], header: &FrameHeader) -> Option<XingInfoTag> {
    // The Info header is a completely optional piece of information. Therefore, flatten an error
    // reading the tag into a None.
    try_read_info_tag_inner(buf, header).ok().flatten()
}

fn try_read_info_tag_inner(buf: &[u8], header: &FrameHeader) -> Result<Option<XingInfoTag>> {
    // The position of the Xing/Info tag relative to the end of the header. This is equal to the
    // side information length for the frame.
    let offset = match (header.version, header.channel_mode) {
        (MpegVersion::Mpeg1, ChannelMode::Mono) => 17,
        (MpegVersion::Mpeg1, _                ) => 32,
        (_                 , ChannelMode::Mono) => 9,
        (_                 , _                ) => 17,
    };

    // Start the CRC with the header and side information.
    let mut crc16 = Crc16AnsiLe::new(0);
    crc16.process_buf_bytes(&buf[..offset + 4]);

    // Start reading the Xing/Info tag after the side information.
    let mut reader = MonitorStream::new(BufReader::new(&buf[offset + 4..]), crc16);

    // Check for Xing/Info header.
    let id = reader.read_quad_bytes()?;

    if id != *b"Xing" && id != *b"Info" {
        return Ok(None);
    }

    // The "Info" id is used for CBR files.
    let is_cbr = id == *b"Info";

    // Flags indicates what information is provided in this Xing/Info tag.
    let flags = reader.read_be_u32()?;

    let num_frames = if flags & 0x1 != 0 {
        Some(reader.read_be_u32()?)
    }
    else {
        None
    };

    let num_bytes = if flags & 0x2 != 0 {
        Some(reader.read_be_u32()?)
    }
    else {
        None
    };

    let toc = if flags & 0x4 != 0 {
        let mut toc = [0; 100];
        reader.read_buf_exact(&mut toc)?;
        Some(toc)
    }
    else {
        None
    };

    let quality = if flags & 0x8 != 0 {
        Some(reader.read_be_u32()?)
    }
    else {
        None
    };

    const LAME_EXTENSION_LEN: u64 = 36;

    // The LAME extension may not always be present. We don't want to return an error if we try to
    // read a frame that doesn't have the LAME extension, so ensure there is enough data to
    // to potentially read one. Even if there are enough bytes available, it still does not
    // guarantee what was read was a LAME tag, so the CRC will be used to make sure it was.
    let lame = if reader.inner().bytes_available() >= LAME_EXTENSION_LEN {
        // Encoder string.
        let mut encoder = [0; 9];
        reader.read_buf_exact(&mut encoder)?;

        // Revision.
        let _revision = reader.read_u8()?;

        // Lowpass filter value.
        let _lowpass = reader.read_u8()?;

        // Replay gain peak in 9.23 (bit) fixed-point format.
        let replaygain_peak = match reader.read_be_u32()? {
            0 => None,
            peak => Some(32767.0 * (peak as f32 / 2.0f32.powi(23))),
        };

        // Radio replay gain.
        let replaygain_radio = parse_lame_tag_replaygain(reader.read_be_u16()?, 1);

        // Audiophile replay gain.
        let replaygain_audiophile = parse_lame_tag_replaygain(reader.read_be_u16()?, 2);

        // Encoding flags & ATH type.
        let _encoding_flags = reader.read_u8()?;

        // Arbitrary bitrate.
        let _abr = reader.read_u8()?;

        let (leading_padding, trailing_padding) = {
            let delay = reader.read_be_u24()?;

            if encoder[..4] == *b"LAME" || encoder[..4] == *b"Lavf" || encoder[..4] == *b"Lavc" {
                // These encoders always add a 529 sample delay on-top of the stated encoder delay.
                let leading = 528 + 1 + (delay >> 12);
                let trailing = delay & ((1 << 12) - 1);

                (leading, trailing)
            }
            else {
                (0, 0)
            }
        };

        // Misc.
        let _misc = reader.read_u8()?;

        // MP3 gain.
        let _mp3_gain = reader.read_u8()?;

        // Preset and surround info.
        let _surround_info = reader.read_be_u16()?;

        // Music length.
        let _music_len = reader.read_be_u32()?;

        // Music (audio) CRC.
        let music_crc = reader.read_be_u16()?;

        // CRC (read using the inner reader to not change the computed CRC).
        let crc = reader.inner_mut().read_be_u16()?;

        if crc == reader.monitor().crc() {
            // The CRC matched, return the LAME tag.
            Some(LameTag {
                encoder: String::from_utf8_lossy(&encoder).into(),
                replaygain_peak,
                replaygain_radio,
                replaygain_audiophile,
                trailing_padding,
                leading_padding,
                music_crc,
            })
        }
        else {
            // The CRC did not match, this is probably not a LAME tag.
            None
        }
    }
    else {
        // Frame not large enough for a LAME tag.
        None
    };

    Ok(Some(XingInfoTag {
        num_frames,
        num_bytes,
        toc,
        quality,
        is_cbr,
        lame,
    }))
}

fn parse_lame_tag_replaygain(value: u16, expected_name: u8) -> Option<f32> {
    // The 3 most-significant bits are the name code.
    let name = ((value & 0xe000) >> 13) as u8;

    if name == expected_name {
        let gain = (value & 0x01ff) as f32 / 10.0;
        Some(if value & 0x200 != 0 { -gain } else { gain })
    }
    else {
        None
    }
}
