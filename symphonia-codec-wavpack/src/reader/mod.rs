// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::io::Seek;

use symphonia_core::codecs::audio::well_known::CODEC_ID_WAVPACK;
use symphonia_core::support_format;

use symphonia_core::errors::{decode_error, unsupported_error, Result};
use symphonia_core::formats::prelude::*;
use symphonia_core::io::*;
use symphonia_core::formats::probe::{ProbeFormatData, ProbeableFormat, Score, Scoreable};
use symphonia_core::formats::well_known::FORMAT_ID_WAVPACK;
use symphonia_core::formats::FormatReader;
use symphonia_core::meta::{
    Metadata, MetadataBuilder, MetadataInfo, MetadataLog, Tag, well_known,
};
use symphonia_core::audio::layouts;
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::AudioCodecParameters;
use symphonia_core::audio::sample::SampleFormat;

use log::debug;

mod sub_block;
use sub_block::{decode_sub_block, Encoding, SubBlock};

const WAVPACK_FORMAT_INFO: FormatInfo = FormatInfo {
    format: FORMAT_ID_WAVPACK,
    short_name: "wavpack",
    long_name: "WavPack",
};

const STREAM_MARKER: [u8; 4] = *b"wvpk";
const RIFF_MARKER:   [u8; 4] = *b"RIFF";
const WAVE_MARKER:   [u8; 4] = *b"WAVE";

const SAMPLE_RATES: [u32; 15] = [
    6000, 8000, 9600, 11025, 12000, 16000, 22050, 24000, 32000, 44100,
    48000, 64000, 88200, 96000, 192000,
];

macro_rules! combine_values {
    ($u32_value:expr, $u8_value:expr) => {
        (($u8_value as u64) << 32) | ($u32_value as u64)
    };
}

// ---------------------------------------------------------------------------
// Internal format version discriminant
// ---------------------------------------------------------------------------

enum FormatVersion {
    /// WavPack v1–v3: raw PCM samples wrapped in a RIFF/WAVE container.
    V3 { num_channels: u16, bytes_per_sample: u16 },
    /// WavPack v4/v5: native wvpk block stream.
    V4V5,
}

// ---------------------------------------------------------------------------
// Reader struct
// ---------------------------------------------------------------------------

/// Format reader for WavPack (v1–v3 RIFF wrapper and v4/v5 native streams).
pub struct WavPackReader<'a> {
    reader: MediaSourceStream<'a>,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    chapters: Option<ChapterGroup>,
    /// Tracks the next packet's presentation timestamp.
    /// Uses i64 to match the signed `Timestamp` newtype introduced in dev-0.6.
    next_packet_ts: i64,
    format_version: FormatVersion,
}

// ---------------------------------------------------------------------------
// Constructor
// ---------------------------------------------------------------------------

impl<'s> WavPackReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, _opts: FormatOptions) -> Result<Self> {
        let original_pos = mss.pos();
        let magic = mss.read_quad_bytes()?;
        mss.seek(std::io::SeekFrom::Start(original_pos))?;

        if magic == RIFF_MARKER {
            Self::try_new_v3(mss)
        } else {
            Self::try_new_v4v5(mss, original_pos)
        }
    }

    // ------------------------------------------------------------------
    // WavPack v1–v3: RIFF/WAVE wrapper
    // ------------------------------------------------------------------

    fn try_new_v3(mut mss: MediaSourceStream<'s>) -> Result<Self> {
        let riff_id = mss.read_quad_bytes()?;
        if riff_id != RIFF_MARKER {
            return decode_error("wavpack v3: expected RIFF");
        }
        let _riff_size = mss.read_u32()?;
        let wave_id = mss.read_quad_bytes()?;
        if wave_id != WAVE_MARKER {
            return decode_error("wavpack v3: expected WAVE");
        }

        let mut wav_header: Option<WaveHeader3> = None;
        let mut meta_builder = MetadataBuilder::new(MetadataInfo {
            metadata: well_known::METADATA_ID_WAVE,
            short_name: "riff",
            long_name: "RIFF/WAVE Sampler Metadata",
        });

        loop {
            let chunk_id   = mss.read_quad_bytes()?;
            let chunk_size = mss.read_u32()?;

            match &chunk_id {
                b"fmt " => {
                    if chunk_size < 16 {
                        return decode_error("wavpack v3: fmt chunk too small");
                    }
                    let format_tag      = mss.read_u16()?;
                    let num_channels    = mss.read_u16()?;
                    let sample_rate     = mss.read_u32()?;
                    let _bytes_per_sec  = mss.read_u32()?;
                    let block_align     = mss.read_u16()?;
                    let bits_per_sample = mss.read_u16()?;

                    let extra = chunk_size - 16;
                    if extra > 0 { mss.ignore_bytes(extra as u64)?; }
                    if chunk_size % 2 != 0 { let _ = mss.read_u8()?; }

                    if format_tag != 1 {
                        return unsupported_error("wavpack v3: non-PCM fmt");
                    }
                    if num_channels == 0 || num_channels > 2 {
                        return decode_error("wavpack v3: unsupported channel count");
                    }
                    let bytes_per_sample = block_align / num_channels;
                    wav_header = Some(WaveHeader3 {
                        sample_rate, num_channels, bits_per_sample, bytes_per_sample,
                    });
                }

                b"smpl" => {
                    parse_smpl_chunk(&mut mss, chunk_size, &mut meta_builder)?;
                    if chunk_size % 2 != 0 { let _ = mss.read_u8()?; }
                }

                b"cue " => {
                    parse_cue_chunk(&mut mss, chunk_size, &mut meta_builder)?;
                    if chunk_size % 2 != 0 { let _ = mss.read_u8()?; }
                }

                b"data" => {
                    // WavPack blocks start here.
                    break;
                }

                _ => {
                    let skip = chunk_size + (chunk_size % 2);
                    mss.ignore_bytes(skip as u64)?;
                }
            }
        }

        let wav = match wav_header {
            Some(w) => w,
            None => return decode_error("wavpack v3: no fmt chunk"),
        };

        let channel_layout = if wav.num_channels == 1 {
            layouts::CHANNEL_LAYOUT_MONO
        } else {
            layouts::CHANNEL_LAYOUT_STEREO
        };

        let bits_per_sample  = wav.bits_per_sample  as u32;
        let bytes_per_sample = wav.bytes_per_sample as u32;

        if bytes_per_sample == 0 || bytes_per_sample > 4 {
            return decode_error("wavpack v3: unsupported bytes per sample");
        }

        // The WavPackDecoder always outputs i32; sample_format matches the bit depth
        // so downstream consumers know the effective range.
        let sample_format = match bytes_per_sample {
            1 => SampleFormat::S8,
            2 => SampleFormat::S16,
            3 => SampleFormat::S24,
            _ => SampleFormat::S32,
        };

        let mut codec_params = AudioCodecParameters::new();
        codec_params
            .for_codec(CODEC_ID_WAVPACK)
            .with_bits_per_coded_sample(bits_per_sample)
            .with_bits_per_sample(bits_per_sample)
            .with_channels(channel_layout)
            .with_sample_rate(wav.sample_rate)
            .with_sample_format(sample_format);

        let mut track = Track::new(0);
        track.with_codec_params(CodecParameters::Audio(codec_params));

        let mut metadata: MetadataLog = Default::default();
        metadata.push(meta_builder.build());

        Ok(WavPackReader {
            reader: mss,
            tracks: vec![track],
            metadata,
            chapters: None,
            next_packet_ts: 0,
            format_version: FormatVersion::V3 {
                num_channels: wav.num_channels,
                bytes_per_sample: wav.bytes_per_sample,
            },
        })
    }

    // ------------------------------------------------------------------
    // WavPack v4/v5: native wvpk block stream
    // ------------------------------------------------------------------

    fn try_new_v4v5(mut mss: MediaSourceStream<'s>, original_pos: u64) -> Result<Self> {
        let _ = find_next_block(&mut mss, 100);
        let header_pos = mss.pos();
        let header = Header::decode(&mut mss)?;
        if header.get_block_index() != 0 {
            debug!("First block is not first block after all.");
        }
        let channel_layout = if header.is_stereo() {
            layouts::CHANNEL_LAYOUT_STEREO
        } else {
            layouts::CHANNEL_LAYOUT_MONO
        };

        let mut codec_params = AudioCodecParameters::new();
        codec_params
            .for_codec(CODEC_ID_WAVPACK)
            .with_bits_per_coded_sample(header.get_bytes_per_sample() * 8)
            .with_bits_per_sample(header.get_bytes_per_sample() * 8)
            .with_channels(channel_layout);

        let sample_format = match header.get_encoding() {
            Encoding::PCM => match header.get_bytes_per_sample() {
                1 => SampleFormat::S8,
                2 => SampleFormat::S16,
                3 => SampleFormat::S24,
                4 => SampleFormat::S32,
                _ => return decode_error("WavPack: Invalid sample format"),
            },
            Encoding::DSD => return unsupported_error("WavPack: DSD unsupported"),
        };
        codec_params.with_sample_format(sample_format);

        if let Some(sr) = header.get_sample_rate() {
            codec_params.with_sample_rate(sr);
        }

        let mut track = Track::new(0);
        track.with_codec_params(CodecParameters::Audio(codec_params));

        // Scan the rest of block 0 for a RiffHeader sub-block. GrandOrgue /
        // Hauptwerk store loop points (`smpl`) and the release marker (`cue `)
        // inside the WAV chunks that WavPack preserves there. Routing them
        // through the same parsers used by the v3 path keeps the tag layout
        // identical between the two flavors.
        let mut meta_builder = MetadataBuilder::new(MetadataInfo {
            metadata: well_known::METADATA_ID_WAVE,
            short_name: "riff",
            long_name: "RIFF/WAVE Sampler Metadata",
        });
        // ck_size counts everything after the ck_size field itself; the
        // remaining 24 bytes of the 32-byte header have already been consumed.
        let sub_blocks_len = (header.ck_size as u64).saturating_sub(24);
        let mut sub_buf = vec![0u8; sub_blocks_len as usize];
        if mss.read_buf_exact(&mut sub_buf).is_ok() {
            scan_v4v5_riff_meta(&sub_buf, &mut meta_builder)?;
        }

        let mut metadata: MetadataLog = Default::default();
        let revision = meta_builder.build();
        if !revision.media.tags.is_empty() {
            metadata.push(revision);
        }

        mss.seek(std::io::SeekFrom::Start(original_pos))?;
        let _ = header_pos; // silence unused if logging gets dropped

        Ok(WavPackReader {
            reader: mss,
            tracks: vec![track],
            metadata,
            chapters: None,
            next_packet_ts: 0,
            format_version: FormatVersion::V4V5,
        })
    }
}

// ---------------------------------------------------------------------------
// Probe / FormatReader impl
// ---------------------------------------------------------------------------

impl ProbeableFormat<'_> for WavPackReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(WavPackReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        &[
            support_format!(WAVPACK_FORMAT_INFO, &["wv"], &["audio/x-wavpack"], &[b"wvpk"]),
            support_format!(WAVPACK_FORMAT_INFO, &["wv"], &["audio/x-wavpack"], &[b"RIFF"]),
        ]
    }
}

impl Scoreable for WavPackReader<'_> {
    fn score(_src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        Ok(Score::Supported(255))
    }
}

impl FormatReader for WavPackReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &WAVPACK_FORMAT_INFO
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        if self.tracks.is_empty() {
            return decode_error("wavpack: no tracks");
        }
        match &self.format_version {
            FormatVersion::V3 { num_channels, bytes_per_sample } => {
                let (ch, bps) = (*num_channels, *bytes_per_sample);
                self.next_packet_v3(ch, bps)
            }
            FormatVersion::V4V5 => self.next_packet_v4v5(),
        }
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
        todo!("seek")
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}

// ---------------------------------------------------------------------------
// Packet helpers
// ---------------------------------------------------------------------------

impl WavPackReader<'_> {
    fn next_packet_v3(
        &mut self,
        num_channels: u16,
        bytes_per_sample: u16,
    ) -> Result<Option<Packet>> {
        if find_next_block(&mut self.reader, 65536).is_err() {
            return Ok(None);
        }
        let hdr = Header3::decode(&mut self.reader)?;

        let header_payload = Header3::header_payload_size(hdr.version);
        let ck_size = hdr.ck_size as u32;

        // Build packet: 32-byte prefix || compressed audio
        let prefix = hdr.to_prefix(num_channels, bytes_per_sample);
        let mut pkt_data = prefix.to_vec();

        if ck_size > header_payload {
            // Audio bytes are encoded within the block (ck_size includes them).
            let audio_size = (ck_size - header_payload) as usize;
            pkt_data.resize(32 + audio_size, 0u8);
            self.reader.read_buf_exact(&mut pkt_data[32..])?;
        } else {
            // Real WavPack 3.97 files: ck_size == header only; compressed audio
            // follows the block header and extends to the next "wvpk" or EOF.
            let audio = read_v3_audio(&mut self.reader)?;
            pkt_data.extend_from_slice(&audio);
        }

        let n_samples = hdr.total_samples.max(0) as u64;
        let ts = self.next_packet_ts;
        self.next_packet_ts += n_samples as i64;

        Ok(Some(Packet::new(
            0,
            Timestamp::new(ts),
            Duration::new(n_samples),
            pkt_data,
        )))
    }

    fn next_packet_v4v5(&mut self) -> Result<Option<Packet>> {
        // Skip non-block bytes and locate the next wvpk marker.
        if find_next_block(&mut self.reader, 10000).is_err() {
            return Ok(None);
        }
        let header = Header::decode(&mut self.reader)?;

        // Collect sub-blocks until we see the audio bitstream.
        let mut terms_data:   Vec<u8> = Vec::new();
        let mut weights_data: Vec<u8> = Vec::new();
        let mut samples_data: Vec<u8> = Vec::new();
        let mut entropy_data: Vec<u8> = Vec::new();
        let mut int32_data:   Vec<u8> = Vec::new();
        let mut audio_data:   Vec<u8> = Vec::new();

        loop {
            // Check for end-of-block: the block covers ck_size bytes after the
            // first 4 (the ck_size field itself is not counted by WavPack).
            // We detect end-of-block by trying to peek; if the next bytes form
            // a new block header we stop.  The simplest approach: just process
            // all sub-blocks until we get the audio bitstream, then break.
            let sb = decode_sub_block(&mut self.reader)?;
            match sb {
                SubBlock::DecorrelationTerms(d)   => terms_data   = d,
                SubBlock::DecorrelationWeights(d) => weights_data = d,
                SubBlock::DecorrelationSamples(d) => samples_data = d,
                SubBlock::EntropyVariables(d)     => entropy_data = d,
                SubBlock::Int32Info(d)             => int32_data   = d,
                SubBlock::WvBitStream(d) => {
                    audio_data = d;
                    break;
                }
                SubBlock::DsdBlock(_) => {
                    return symphonia_core::errors::unsupported_error("wavpack: DSD not supported");
                }
                // Skip everything else (metadata, checksums, RIFF headers, wvc/wvx)
                _ => debug!("v4v5: skipping non-audio sub-block"),
            }
        }

        // Serialise into a packet understood by the decoder:
        //   magic(4) + flags(4) + block_samples(4) + crc(4)
        //   + terms_len(2) + weights_len(2) + samples_len(2) + entropy_len(2)
        //   + int32_len(1) + pad(3)
        //   followed by the raw sub-block bytes then the audio bitstream.
        let tl = terms_data.len()   as u16;
        let wl = weights_data.len() as u16;
        let sl = samples_data.len() as u16;
        let el = entropy_data.len() as u16;
        let il = int32_data.len()   as u8;

        let mut pkt: Vec<u8> = Vec::with_capacity(
            28 + terms_data.len() + weights_data.len() + samples_data.len()
               + entropy_data.len() + int32_data.len() + audio_data.len()
        );
        pkt.extend_from_slice(b"WV45");
        pkt.extend_from_slice(&header.flags.to_le_bytes());
        pkt.extend_from_slice(&header.block_samples.to_le_bytes());
        pkt.extend_from_slice(&header.crc.to_le_bytes());
        pkt.extend_from_slice(&tl.to_le_bytes());
        pkt.extend_from_slice(&wl.to_le_bytes());
        pkt.extend_from_slice(&sl.to_le_bytes());
        pkt.extend_from_slice(&el.to_le_bytes());
        pkt.push(il);
        pkt.extend_from_slice(&[0u8; 3]); // pad to 28 bytes
        pkt.extend_from_slice(&terms_data);
        pkt.extend_from_slice(&weights_data);
        pkt.extend_from_slice(&samples_data);
        pkt.extend_from_slice(&entropy_data);
        pkt.extend_from_slice(&int32_data);
        pkt.extend_from_slice(&audio_data);

        // block_samples is the per-channel frame count
        let n  = header.block_samples as i64;
        let ts = self.next_packet_ts;
        self.next_packet_ts += n;

        Ok(Some(Packet::new(0, Timestamp::new(ts), Duration::new(n as u64), pkt)))
    }
}

// ---------------------------------------------------------------------------
// RIFF mark parsers
// ---------------------------------------------------------------------------

/// Parse a `smpl` chunk and add its fields as `WavPack/*` tags.
///
/// Tag keys follow the `WavPack/<Field>` convention so applications can
/// retrieve them without depending on format-specific types. The GrandOrgue
/// sampler chunk layout (`GO_WAVESAMPLERCHUNK` / `GO_WAVESAMPLERLOOP`) is
/// used as the authoritative field mapping.
fn parse_smpl_chunk(
    source: &mut MediaSourceStream<'_>,
    chunk_size: u32,
    builder: &mut MetadataBuilder,
) -> Result<()> {
    // GO_WAVESAMPLERCHUNK: 9 × u32 = 36 bytes
    const SAMPLER_HDR: u32 = 36;
    // GO_WAVESAMPLERLOOP: 6 × u32 = 24 bytes
    const LOOP_ENTRY:  u32 = 24;

    if chunk_size < SAMPLER_HDR {
        debug!("wavpack: smpl chunk too small ({})", chunk_size);
        mss_skip(source, chunk_size as u64)?;
        return Ok(());
    }

    let _manufacturer  = source.read_u32()?;
    let _product       = source.read_u32()?;
    let sample_period  = source.read_u32()?;
    let midi_note      = source.read_u32()?;
    let pitch_fraction = source.read_u32()?;
    let _smpte_format  = source.read_u32()?;
    let _smpte_offset  = source.read_u32()?;
    let num_loops      = source.read_u32()?;
    let _sampler_data  = source.read_u32()?;

    builder.add_tag(Tag::new_from_parts("WavPack/MidiNote",      midi_note,      None));
    builder.add_tag(Tag::new_from_parts("WavPack/PitchFraction", pitch_fraction, None));
    // Sample period (ns/sample) is used by Hauptwerk for fine tuning when it
    // disagrees with the format chunk's sample rate. Skip the trivial case
    // (0 = "unspecified") so the tag set stays meaningful.
    if sample_period != 0 {
        builder.add_tag(Tag::new_from_parts("WavPack/SamplePeriod", sample_period, None));
    }
    builder.add_tag(Tag::new_from_parts("WavPack/LoopCount", num_loops, None));

    let loops_size = num_loops.saturating_mul(LOOP_ENTRY);
    let remaining  = chunk_size.saturating_sub(SAMPLER_HDR);

    if loops_size > remaining {
        debug!("wavpack: smpl loop count exceeds chunk size");
        mss_skip(source, remaining as u64)?;
        return Ok(());
    }

    for i in 0..num_loops {
        let _id        = source.read_u32()?;
        let loop_type  = source.read_u32()?;
        let start      = source.read_u32()?;
        let end        = source.read_u32()?;
        let fraction   = source.read_u32()?;
        let play_cnt   = source.read_u32()?;
        // Hauptwerk uses Type (0=forward, 1=alternating, 2=reverse) and
        // PlayCount (0 = infinite). GrandOrgue only consults Start/End but
        // is fine with the extras being present.
        builder.add_tag(Tag::new_from_parts(format!("WavPack/Loop{i}/Type"),      loop_type, None));
        builder.add_tag(Tag::new_from_parts(format!("WavPack/Loop{i}/Start"),     start,     None));
        builder.add_tag(Tag::new_from_parts(format!("WavPack/Loop{i}/End"),       end,       None));
        if fraction != 0 {
            builder.add_tag(Tag::new_from_parts(format!("WavPack/Loop{i}/Fraction"),  fraction, None));
        }
        if play_cnt != 0 {
            builder.add_tag(Tag::new_from_parts(format!("WavPack/Loop{i}/PlayCount"), play_cnt, None));
        }
    }

    let consumed = SAMPLER_HDR + loops_size;
    if chunk_size > consumed {
        mss_skip(source, (chunk_size - consumed) as u64)?;
    }
    Ok(())
}

/// Parse a `cue ` chunk and store the release point as a tag.
///
/// GrandOrgue identifies the release point as the highest `dwSampleOffset`
/// across all cue points (`GO_WAVECUEPOINT`). That sample offset is stored
/// under `"WavPack/ReleasePoint"`.
fn parse_cue_chunk(
    source: &mut MediaSourceStream<'_>,
    chunk_size: u32,
    builder: &mut MetadataBuilder,
) -> Result<()> {
    // GO_WAVECUECHUNK:  1 × u32 = 4 bytes
    // GO_WAVECUEPOINT:  6 × u32 = 24 bytes
    //   (dwName, dwPosition, fccChunk, dwChunkStart, dwBlockStart, dwSampleOffset)
    const CUE_HDR:   u32 = 4;
    const CUE_ENTRY: u32 = 24;

    if chunk_size < CUE_HDR {
        debug!("wavpack: cue chunk too small");
        mss_skip(source, chunk_size as u64)?;
        return Ok(());
    }

    let num_cues   = source.read_u32()?;
    let entries_sz = num_cues.saturating_mul(CUE_ENTRY);
    let remaining  = chunk_size.saturating_sub(CUE_HDR);

    if entries_sz > remaining {
        debug!("wavpack: cue count exceeds chunk size");
        mss_skip(source, remaining as u64)?;
        return Ok(());
    }

    builder.add_tag(Tag::new_from_parts("WavPack/CueCount", num_cues, None));

    // Keep two views: each cue point individually (Hauptwerk's
    // multi-release-stage convention) and the highest sample offset as
    // "ReleasePoint" (GrandOrgue's single-marker convention).
    let mut release_point: Option<u32> = None;
    for i in 0..num_cues {
        let name          = source.read_u32()?;
        let _position     = source.read_u32()?;
        let _fcc_chunk    = source.read_u32()?;
        let _chunk_start  = source.read_u32()?;
        let _block_start  = source.read_u32()?;
        let sample_offset = source.read_u32()?;

        builder.add_tag(Tag::new_from_parts(format!("WavPack/Cue{i}/Name"),         name,          None));
        builder.add_tag(Tag::new_from_parts(format!("WavPack/Cue{i}/SampleOffset"), sample_offset, None));

        release_point = Some(match release_point {
            Some(prev) => prev.max(sample_offset),
            None       => sample_offset,
        });
    }

    if let Some(rp) = release_point {
        builder.add_tag(Tag::new_from_parts("WavPack/ReleasePoint", rp, None));
    }

    let consumed = CUE_HDR + entries_sz;
    if chunk_size > consumed {
        mss_skip(source, (chunk_size - consumed) as u64)?;
    }
    Ok(())
}

#[inline]
fn mss_skip(source: &mut MediaSourceStream<'_>, n: u64) -> Result<()> {
    source.ignore_bytes(n).map_err(symphonia_core::errors::Error::IoError)
}

// ---------------------------------------------------------------------------
// V4/V5 RIFF metadata scan
// ---------------------------------------------------------------------------

// Sub-block IDs we care about for metadata extraction.
const SBID_RIFF_HEADER:  u8 = 0x21;
const SBID_RIFF_TRAILER: u8 = 0x22;

/// Walk the sub-blocks contained in the first wvpk block body, locate any
/// RIFF header bytes WavPack stashed there (sub-block IDs 0x21 / 0x22), and
/// parse `smpl` and `cue ` chunks out of them into builder tags.
fn scan_v4v5_riff_meta(body: &[u8], builder: &mut MetadataBuilder) -> Result<()> {
    let mut p = 0usize;
    while p + 2 <= body.len() {
        let id = body[p];
        // ID_LARGE_BLOCK uses a 3-byte size field, otherwise 1-byte.
        let (hdr_len, words) = if id & 0x80 != 0 {
            if p + 4 > body.len() { break; }
            let w = (body[p + 1] as u32)
                  | ((body[p + 2] as u32) << 8)
                  | ((body[p + 3] as u32) << 16);
            (4usize, w)
        } else {
            (2usize, body[p + 1] as u32)
        };

        let data_len = (words as usize) * 2;
        let pad      = if id & 0x40 != 0 { 1 } else { 0 };
        let start    = p + hdr_len;
        let end_full = start + data_len;
        if end_full > body.len() { break; }
        let end_used = end_full - pad;

        let fn_id = id & 0x3F;
        if fn_id == SBID_RIFF_HEADER || fn_id == SBID_RIFF_TRAILER {
            parse_riff_meta_bytes(&body[start..end_used], builder)?;
        }

        p = end_full;
    }
    Ok(())
}

/// Parse RIFF chunks out of a raw byte buffer (with or without a leading
/// `RIFF....WAVE` envelope) and feed `smpl` / `cue ` into the existing
/// chunk parsers via a Cursor-backed MediaSourceStream.
fn parse_riff_meta_bytes(buf: &[u8], builder: &mut MetadataBuilder) -> Result<()> {
    use symphonia_core::io::MediaSourceStreamOptions;

    let mut start = 0usize;
    if buf.len() >= 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"WAVE" {
        start = 12;
    } else if buf.len() >= 4 && &buf[0..4] == b"WAVE" {
        start = 4;
    }

    let chunks = buf[start..].to_vec();
    let cursor = std::io::Cursor::new(chunks);
    let mut mss = MediaSourceStream::new(Box::new(cursor), MediaSourceStreamOptions::default());

    loop {
        let chunk_id = match mss.read_quad_bytes() {
            Ok(v) => v,
            Err(_) => break,
        };
        let chunk_size = match mss.read_u32() {
            Ok(v) => v,
            Err(_) => break,
        };

        match &chunk_id {
            b"smpl" => {
                parse_smpl_chunk(&mut mss, chunk_size, builder)?;
                if chunk_size % 2 != 0 { let _ = mss.read_u8(); }
            }
            b"cue " => {
                parse_cue_chunk(&mut mss, chunk_size, builder)?;
                if chunk_size % 2 != 0 { let _ = mss.read_u8(); }
            }
            b"data" => {
                // We don't expect 'data' inside a RiffHeader sub-block payload
                // (audio is in the WvBitStream sub-block), but if encountered
                // the bytes after it would be PCM and we should stop walking
                // metadata.
                break;
            }
            _ => {
                let skip = (chunk_size as u64) + (chunk_size as u64 % 2);
                if mss.ignore_bytes(skip).is_err() { break; }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// V3 structures
// ---------------------------------------------------------------------------

struct WaveHeader3 {
    sample_rate:     u32,
    num_channels:    u16,
    bits_per_sample: u16,
    bytes_per_sample: u16,
}

struct Header3 {
    ck_size:       i32,
    version:       i16,
    bits:          i16,
    flags:         i16,
    shift:         i16,
    total_samples: i32,
    crc:           i32,
    crc2:          i32,
    ext:           [u8; 4],
    extra_bc:      u8,
    extras:        [u8; 3],
}

impl Header3 {
    // Payload bytes consumed after ck_size, per version.
    fn header_payload_size(version: i16) -> u32 {
        match version {
            1 => 2,
            2 => 4,
            _ => 28,
        }
    }

    fn decode(reader: &mut MediaSourceStream<'_>) -> Result<Header3> {
        let marker = reader.read_quad_bytes()?;
        if marker != STREAM_MARKER {
            return decode_error("wavpack v3: missing wvpk marker");
        }
        let ck_size = reader.read_i32()?;
        let version = reader.read_i16()?;
        if version < 1 || version > 3 {
            return decode_error("wavpack: unsupported v3 block version");
        }

        let bits = if version >= 2 { reader.read_i16()? } else { 0 };

        let (flags, shift, total_samples, crc, crc2, ext, extra_bc, extras) = if version >= 3 {
            let flags         = reader.read_i16()?;
            let shift         = reader.read_i16()?;
            let total_samples = reader.read_i32()?;
            let crc           = reader.read_i32()?;
            let crc2          = reader.read_i32()?;
            let mut ext = [0u8; 4];
            reader.read_buf_exact(&mut ext)?;
            let extra_bc      = reader.read_u8()?;
            let mut extras = [0u8; 3];
            reader.read_buf_exact(&mut extras)?;
            (flags, shift, total_samples, crc, crc2, ext, extra_bc, extras)
        } else {
            (0, 0, 0, 0, 0, [0u8; 4], 0u8, [0u8; 3])
        };

        Ok(Header3 { ck_size, version, bits, flags, shift, total_samples, crc, crc2, ext, extra_bc, extras })
    }

    /// Serialise the header into a 32-byte packet prefix understood by `WavPackDecoder`.
    fn to_prefix(&self, num_channels: u16, bytes_per_sample: u16) -> [u8; 32] {
        let mut p = [0u8; 32];
        p[0..2].copy_from_slice(&self.version.to_le_bytes());
        p[2..4].copy_from_slice(&self.bits.to_le_bytes());
        p[4..6].copy_from_slice(&self.flags.to_le_bytes());
        p[6..8].copy_from_slice(&self.shift.to_le_bytes());
        p[8..12].copy_from_slice(&self.total_samples.to_le_bytes());
        p[12..16].copy_from_slice(&self.crc.to_le_bytes());
        p[16..20].copy_from_slice(&self.crc2.to_le_bytes());
        p[20..24].copy_from_slice(&self.ext);
        p[24] = self.extra_bc;
        p[25..28].copy_from_slice(&self.extras);
        p[28..30].copy_from_slice(&num_channels.to_le_bytes());
        p[30..32].copy_from_slice(&bytes_per_sample.to_le_bytes());
        p
    }
}

// ---------------------------------------------------------------------------
// V4/V5 structures
// ---------------------------------------------------------------------------

struct Header {
    ck_size:           u32,
    version:           u16,
    block_index_u8:    u8,
    total_samples_u8:  u8,
    total_samples_u32: u32,
    block_index_u32:   u32,
    block_samples:     u32,
    flags:             u32,
    crc:               u32,
}

impl Header {
    fn decode(reader: &mut MediaSourceStream<'_>) -> Result<Header> {
        let marker = reader.read_quad_bytes()?;
        if marker != STREAM_MARKER {
            return unsupported_error("wavpack: missing marker");
        }
        Ok(Header {
            ck_size:           reader.read_u32()?,
            version:           reader.read_u16()?,
            block_index_u8:    reader.read_u8()?,
            total_samples_u8:  reader.read_u8()?,
            total_samples_u32: reader.read_u32()?,
            block_index_u32:   reader.read_u32()?,
            block_samples:     reader.read_u32()?,
            flags:             reader.read_u32()?,
            crc:               reader.read_u32()?,
        })
    }

    fn get_block_index(&self) -> u64 {
        combine_values!(self.block_index_u32, self.block_index_u8)
    }

    fn get_bytes_per_sample(&self) -> u32 {
        (self.flags & 3) + 1
    }

    fn get_encoding(&self) -> Encoding {
        if (self.flags >> 31) & 1 == 0 { Encoding::PCM } else { Encoding::DSD }
    }

    fn get_n_channels(&self) -> u32 {
        if self.is_stereo() { 2 } else { 1 }
    }

    fn get_n_samples(&self) -> u32 {
        self.block_samples / self.get_n_channels()
    }

    fn is_stereo(&self) -> bool {
        ((self.flags >> 2) & 1) == 0
    }

    fn get_sample_rate(&self) -> Option<u32> {
        let idx = (self.flags >> 23) & 0xF;
        if idx == 0xF { return None; }
        SAMPLE_RATES.get(idx as usize).copied()
    }
}

/// Read compressed audio bytes from `reader` until the next `wvpk` marker or EOF.
///
/// Used for real WavPack 3.97 files where `ck_size` only covers the block header
/// metadata and the compressed audio runs implicitly to the next block boundary.
fn read_v3_audio(reader: &mut MediaSourceStream<'_>) -> Result<Vec<u8>> {
    // We need 4 bytes of seekback so we can "unread" the next "wvpk" marker.
    reader.ensure_seekback_buffer(4);

    let mut audio: Vec<u8> = Vec::new();
    loop {
        let b = match reader.read_u8() {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(audio),
            Err(e) => return Err(symphonia_core::errors::Error::IoError(e)),
        };
        audio.push(b);
        let n = audio.len();
        if n >= 4 && &audio[n - 4..] == b"wvpk" {
            audio.truncate(n - 4);
            reader.seek_buffered_rev(4);
            return Ok(audio);
        }
    }
}

fn find_next_block(source: &mut MediaSourceStream<'_>, max_bytes: usize) -> Result<u64> {
    let mut n = 0usize;
    source.ensure_seekback_buffer(max_bytes);
    loop {
        if n + 4 >= max_bytes {
            return decode_error("no block found");
        }
        let b = source.read_u8()?;
        n += 1;
        if b == b'w' {
            let t = source.read_triple_bytes()?;
            n += 3;
            if t == *b"vpk" {
                source.seek_buffered_rev(4);
                return Ok(n as u64);
            }
        }
    }
}
