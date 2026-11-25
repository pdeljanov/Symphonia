// Symphonia
// Copyright (c) 2019-2024 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

mod bits;
mod words;
mod v3;
mod v4v5;

use symphonia_core::audio::{AsGenericAudioBufferRef, AudioSpec, GenericAudioBuffer, GenericAudioBufferRef};
use symphonia_core::audio::sample::SampleFormat;
use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult};
use symphonia_core::codecs::audio::well_known::CODEC_ID_WAVPACK;
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::errors::{decode_error, unsupported_error, Result};
use symphonia_core::packet::Packet;
use symphonia_core::support_audio_codec;

use v3::{DecorrPass, DcState, unpack_init3, unpack_samples_v3, MONO_FLAG};
use words::WordState;
use v4v5::{
    DecorrPass as DecorrPass45, WordsState as WordsState45, Int32Info,
    PACKET_MAGIC, PKT_HDR,
    parse_decorr_terms, parse_decorr_weights, parse_decorr_samples,
    parse_entropy_vars, parse_int32_info,
    unpack_samples_v4v5,
};

// Packet-prefix layout (32 bytes) — written by the format reader:
//   [0..2]   version      i16 LE
//   [2..4]   bits         i16 LE  (0 = lossless)
//   [4..6]   flags        i16 LE
//   [6..8]   shift        i16 LE
//   [8..12]  total_samples i32 LE
//   [12..16] crc          i32 LE
//   [16..20] crc2         i32 LE
//   [20..24] ext[4]
//   [24]     extra_bc
//   [25..28] extras[3]
//   [28..30] num_channels u16 LE
//   [30..32] bytes_per_sample u16 LE
//   [32..]   compressed audio
const PREFIX_LEN: usize = 32;

#[derive(Debug)]
struct BlockHeader {
    version:       i16,
    bits:          i16,
    flags:         i16,
    shift:         i16,
    total_samples: i32,
    crc:           i32,
    #[allow(dead_code)]
    crc2:          i32,
    num_channels:  u16,
    bytes_per_sample: u16,
}

fn parse_prefix(data: &[u8]) -> Option<(BlockHeader, &[u8])> {
    if data.len() < PREFIX_LEN {
        return None;
    }
    let hdr = BlockHeader {
        version:          i16::from_le_bytes([data[0],  data[1]]),
        bits:             i16::from_le_bytes([data[2],  data[3]]),
        flags:            i16::from_le_bytes([data[4],  data[5]]),
        shift:            i16::from_le_bytes([data[6],  data[7]]),
        total_samples:    i32::from_le_bytes([data[8],  data[9],  data[10], data[11]]),
        crc:              i32::from_le_bytes([data[12], data[13], data[14], data[15]]),
        crc2:             i32::from_le_bytes([data[16], data[17], data[18], data[19]]),
        // [20..24] ext, [24] extra_bc, [25..28] extras — unused in decode
        num_channels:     u16::from_le_bytes([data[28], data[29]]),
        bytes_per_sample: u16::from_le_bytes([data[30], data[31]]),
    };
    Some((hdr, &data[PREFIX_LEN..]))
}

// ---------------------------------------------------------------------------
// WavPackDecoder
// ---------------------------------------------------------------------------

pub struct WavPackDecoder {
    params:      AudioCodecParameters,
    // v3 per-stream state
    dc:          DcState,
    decorr:      Vec<DecorrPass>,
    num_terms:   usize,
    word_state:  WordState,
    initialized: bool,
    last_flags:  i16,
    // v4/v5 per-stream state
    decorr45:    Vec<DecorrPass45>,
    words45:     WordsState45,
    // Output buffer
    buf: GenericAudioBuffer,
}

impl WavPackDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_WAVPACK {
            return unsupported_error("wavpack decoder: wrong codec id");
        }

        let rate = params.sample_rate.unwrap_or(44100);
        let channels = match &params.channels {
            Some(ch) => ch.clone(),
            None => return unsupported_error("wavpack decoder: no channel info"),
        };
        let sample_format = params.sample_format.unwrap_or(SampleFormat::S32);
        let spec = AudioSpec::new(rate, channels);
        let buf = GenericAudioBuffer::new(sample_format, spec, 0);

        Ok(WavPackDecoder {
            params: params.clone(),
            dc: DcState::default(),
            decorr: Vec::new(),
            num_terms: 0,
            word_state: WordState::default(),
            initialized: false,
            last_flags: 0,
            decorr45: Vec::new(),
            words45: WordsState45::default(),
            buf,
        })
    }

    fn decode_inner(&mut self, packet: &Packet) -> Result<()> {
        // Dispatch on packet type: v4/v5 packets start with "WV45" magic.
        if packet.data.starts_with(PACKET_MAGIC) {
            return self.decode_inner_v4v5(packet);
        }

        let data = &packet.data;
        let (hdr, audio) = parse_prefix(data)
            .ok_or_else(|| symphonia_core::errors::Error::DecodeError("wavpack: packet too short"))?;

        if hdr.total_samples <= 0 {
            self.buf.clear();
            return Ok(());
        }
        let sample_count = hdr.total_samples as u32;
        let num_channels = hdr.num_channels as u32;

        // First block (or after reset): initialise decorr passes
        if !self.initialized || hdr.flags != self.last_flags {
            unpack_init3(hdr.flags, &mut self.decorr, &mut self.num_terms);
            self.dc = DcState::default();
            self.word_state = WordState::default();
            self.initialized = true;
            self.last_flags = hdr.flags;
        }

        // Decode
        let samples = unpack_samples_v3(
            hdr.version,
            hdr.bits,
            hdr.flags,
            hdr.shift,
            sample_count,
            num_channels,
            audio,
            &mut self.dc,
            &mut self.decorr,
            self.num_terms,
            &mut self.word_state,
        );

        if samples.is_empty() && sample_count > 0 {
            return decode_error("wavpack: no samples decoded (unsupported flags?)");
        }

        let is_mono = (hdr.flags & MONO_FLAG) != 0;
        let decoded_frames = if is_mono { samples.len() } else { samples.len() / 2 };

        // Grow / reset buffer
        self.buf.clear();
        match &mut self.buf {
            GenericAudioBuffer::S16(b) => {
                b.grow_capacity(decoded_frames);
                b.render_with(Some(decoded_frames), |idx, planes| {
                    if is_mono {
                        planes[0][idx] = samples[idx].clamp(-32768, 32767) as i16;
                    } else {
                        planes[0][idx] = samples[idx * 2    ].clamp(-32768, 32767) as i16;
                        planes[1][idx] = samples[idx * 2 + 1].clamp(-32768, 32767) as i16;
                    }
                    Ok(())
                })?;
            }
            GenericAudioBuffer::S32(b) => {
                b.grow_capacity(decoded_frames);
                b.render_with(Some(decoded_frames), |idx, planes| {
                    if is_mono {
                        planes[0][idx] = samples[idx];
                    } else {
                        planes[0][idx] = samples[idx * 2];
                        planes[1][idx] = samples[idx * 2 + 1];
                    }
                    Ok(())
                })?;
            }
            GenericAudioBuffer::S24(b) => {
                use symphonia_core::audio::sample::i24;
                b.grow_capacity(decoded_frames);
                b.render_with(Some(decoded_frames), |idx, planes| {
                    let clamp = |v: i32| i24::from(v.clamp(-8_388_608, 8_388_607));
                    if is_mono {
                        planes[0][idx] = clamp(samples[idx]);
                    } else {
                        planes[0][idx] = clamp(samples[idx * 2]);
                        planes[1][idx] = clamp(samples[idx * 2 + 1]);
                    }
                    Ok(())
                })?;
            }
            GenericAudioBuffer::S8(b) => {
                b.grow_capacity(decoded_frames);
                b.render_with(Some(decoded_frames), |idx, planes| {
                    if is_mono {
                        planes[0][idx] = samples[idx].clamp(-128, 127) as i8;
                    } else {
                        planes[0][idx] = samples[idx * 2    ].clamp(-128, 127) as i8;
                        planes[1][idx] = samples[idx * 2 + 1].clamp(-128, 127) as i8;
                    }
                    Ok(())
                })?;
            }
            _ => return unsupported_error("wavpack decoder: unsupported output sample format"),
        }

        // Optional CRC check (version 3 only, lossless)
        if hdr.version == 3 && hdr.bits == 0 && self.dc.crc != hdr.crc {
            log::warn!("wavpack: CRC mismatch (expected {:08x}, got {:08x})", hdr.crc, self.dc.crc);
        }

        Ok(())
    }

    fn decode_inner_v4v5(&mut self, packet: &Packet) -> Result<()> {
        let data = &packet.data;
        if data.len() < PKT_HDR {
            return decode_error("wavpack v4/v5: packet too short");
        }

        // Parse fixed header (offsets from PKT_HDR layout)
        let flags         = u32::from_le_bytes([data[4],  data[5],  data[6],  data[7]]);
        let block_samples = u32::from_le_bytes([data[8],  data[9],  data[10], data[11]]);
        let _crc          = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        let tl            = u16::from_le_bytes([data[16], data[17]]) as usize;
        let wl            = u16::from_le_bytes([data[18], data[19]]) as usize;
        let sl            = u16::from_le_bytes([data[20], data[21]]) as usize;
        let el            = u16::from_le_bytes([data[22], data[23]]) as usize;
        let il            = data[24] as usize;

        let mut pos = PKT_HDR;
        let end     = data.len();

        eprintln!("[v45] flags={:#010x} block_samples={} pkt_len={} tl={} wl={} sl={} el={} il={}",
            flags, block_samples, data.len(), tl, wl, sl, el, il);

        let terms_raw   = &data[pos..pos.saturating_add(tl).min(end)]; pos += tl;
        let weights_raw = &data[pos..pos.saturating_add(wl).min(end)]; pos += wl;
        let samples_raw = &data[pos..pos.saturating_add(sl).min(end)]; pos += sl;
        let entropy_raw = &data[pos..pos.saturating_add(el).min(end)]; pos += el;
        let int32_raw   = &data[pos..pos.saturating_add(il).min(end)]; pos += il;
        let audio       = &data[pos.min(end)..];
        eprintln!("[v45] audio offset={} audio_len={}", pos, audio.len());
        eprintln!("[v45] audio first 16 bytes: {:02x?}", &audio[..audio.len().min(16)]);

        if block_samples == 0 {
            self.buf.clear();
            return Ok(());
        }

        let is_mono = (flags & v4v5::MONO_DATA) != 0;

        // v4/v5 entropy state is freshly initialized from sub-blocks each block
        self.words45 = WordsState45::default();

        // Parse sub-block data into decoder state
        parse_decorr_terms(terms_raw, &mut self.decorr45);
        parse_decorr_weights(weights_raw, &mut self.decorr45, is_mono);
        parse_decorr_samples(samples_raw, &mut self.decorr45, is_mono);
        parse_entropy_vars(entropy_raw, &mut self.words45, is_mono);
        let i32info = parse_int32_info(int32_raw);

        let samples = unpack_samples_v4v5(
            flags, block_samples,
            &mut self.decorr45,
            &mut self.words45,
            &i32info,
            audio,
        ).ok_or_else(|| symphonia_core::errors::Error::DecodeError(
            "wavpack v4/v5: unsupported encoding (hybrid or float)"
        ))?;

        if samples.is_empty() && block_samples > 0 {
            return decode_error("wavpack v4/v5: no samples decoded");
        }

        let decoded_frames = if is_mono { samples.len() } else { samples.len() / 2 };

        self.buf.clear();
        match &mut self.buf {
            GenericAudioBuffer::S16(b) => {
                b.grow_capacity(decoded_frames);
                b.render_with(Some(decoded_frames), |idx, planes| {
                    if is_mono {
                        planes[0][idx] = samples[idx].clamp(-32768, 32767) as i16;
                    } else {
                        planes[0][idx] = samples[idx * 2    ].clamp(-32768, 32767) as i16;
                        planes[1][idx] = samples[idx * 2 + 1].clamp(-32768, 32767) as i16;
                    }
                    Ok(())
                })?;
            }
            GenericAudioBuffer::S32(b) => {
                b.grow_capacity(decoded_frames);
                b.render_with(Some(decoded_frames), |idx, planes| {
                    if is_mono {
                        planes[0][idx] = samples[idx];
                    } else {
                        planes[0][idx] = samples[idx * 2];
                        planes[1][idx] = samples[idx * 2 + 1];
                    }
                    Ok(())
                })?;
            }
            GenericAudioBuffer::S24(b) => {
                use symphonia_core::audio::sample::i24;
                b.grow_capacity(decoded_frames);
                b.render_with(Some(decoded_frames), |idx, planes| {
                    let clamp = |v: i32| i24::from(v.clamp(-8_388_608, 8_388_607));
                    if is_mono {
                        planes[0][idx] = clamp(samples[idx]);
                    } else {
                        planes[0][idx] = clamp(samples[idx * 2]);
                        planes[1][idx] = clamp(samples[idx * 2 + 1]);
                    }
                    Ok(())
                })?;
            }
            GenericAudioBuffer::S8(b) => {
                b.grow_capacity(decoded_frames);
                b.render_with(Some(decoded_frames), |idx, planes| {
                    if is_mono {
                        planes[0][idx] = samples[idx].clamp(-128, 127) as i8;
                    } else {
                        planes[0][idx] = samples[idx * 2    ].clamp(-128, 127) as i8;
                        planes[1][idx] = samples[idx * 2 + 1].clamp(-128, 127) as i8;
                    }
                    Ok(())
                })?;
            }
            _ => return unsupported_error("wavpack v4/v5: unsupported output sample format"),
        }

        Ok(())
    }
}

impl AudioDecoder for WavPackDecoder {
    fn reset(&mut self) {
        self.initialized = false;
        self.dc = DcState::default();
        self.decorr.clear();
        self.num_terms = 0;
        self.word_state = WordState::default();
        self.decorr45.clear();
        self.words45 = WordsState45::default();
        self.buf.clear();
    }

    fn codec_info(&self) -> &CodecInfo {
        &Self::supported_codecs()
            .iter()
            .find(|d| d.id == self.params.codec)
            .unwrap()
            .info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: &Packet) -> Result<GenericAudioBufferRef<'_>> {
        if let Err(e) = self.decode_inner(packet) {
            self.buf.clear();
            return Err(e);
        }
        Ok(self.buf.as_generic_audio_buffer_ref())
    }

    fn finalize(&mut self) -> FinalizeResult {
        Default::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for WavPackDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(WavPackDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[support_audio_codec!(CODEC_ID_WAVPACK, "wavpack", "WavPack Lossless Audio (v1–v3)")]
    }
}
