// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// Previous Author: Kostya Shishkov <kostya.shiskov@gmail.com>
//
// This source file includes code originally written for the NihAV
// project. With the author's permission, it has been relicensed for,
// and ported to the Symphonia project.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef,
};
use symphonia_core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoderOptions};
use symphonia_core::codecs::audio::{AudioDecoder, FinalizeResult};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::codecs::CodecInfo;
use symphonia_core::errors::{unsupported_error, Result};
use symphonia_core::io::{BitReaderLtr, FiniteBitStream, ReadBitsLtr};
use symphonia_core::packet::Packet;
use symphonia_core::{codec_profile, support_audio_codec};

use log::debug;

mod codebooks;
mod common;
mod cpe;
mod dsp;
mod ics;
mod imdct_arb;
mod sbr;
mod window;

use crate::common::*;
use common::*;

/// Extract bits from `data` starting at `bit_offset` (MSB-first) and compute SBR CRC-10.
fn crc10_payload(data: &[u8], bit_offset: usize, num_bits: usize) -> u16 {
    let mut extracted = vec![0u8; num_bits.div_ceil(8)];
    for i in 0..num_bits {
        let src_bit = bit_offset + i;
        let src_byte = src_bit / 8;
        let src_shift = 7 - (src_bit % 8);
        let dst_byte = i / 8;
        let dst_shift = 7 - (i % 8);
        if src_byte < data.len() {
            extracted[dst_byte] |= ((data[src_byte] >> src_shift) & 1) << dst_shift;
        }
    }
    sbr::sbr_crc10(&extracted, num_bits)
}

fn read_bits_to_vec<B: ReadBitsLtr>(bs: &mut B, num_bits: usize) -> Result<Vec<u8>> {
    let mut data = vec![0u8; num_bits.div_ceil(8)];
    for bit in 0..num_bits {
        if bs.read_bool()? {
            data[bit / 8] |= 1 << (7 - (bit & 7));
        }
    }
    Ok(data)
}

/// SBR processing state, heap-allocated due to large per-channel buffers.
struct SbrProcessor {
    header: sbr::SbrHeader,
    state: sbr::SbrState,
    channels: [sbr::SbrChannel; 2],
    analysis: [sbr::dsp::SbrAnalysis; 2],
    synthesis: [sbr::dsp::SbrSynthesis; 2],
    dsp: sbr::dsp::SbrDsp,
    /// Whether the SBR frequency tables have been initialized from a header.
    active: bool,
    /// Parametric Stereo processing context, lazily allocated.
    ps: Option<Box<sbr::ps::PsContext>>,
    /// Whether PS data was parsed and should be applied this frame.
    ps_active: bool,
    /// Whether the immediately preceding AAC frame carried a PS payload.
    ps_data_prev_frame: bool,
    /// Whether the current AAC frame carried a PS payload.
    ps_data_this_frame: bool,
    /// Whether the current frame's SBR data was successfully decoded and CRC-verified.
    frame_valid: bool,
    /// Number of SBR time slots per frame (15 for 960-sample core, 16 for 1024-sample core).
    num_time_slots: usize,
}

impl SbrProcessor {
    fn with_num_time_slots(num_time_slots: usize) -> Self {
        Self {
            header: sbr::SbrHeader::new(),
            state: sbr::SbrState::new(),
            channels: [sbr::SbrChannel::new(), sbr::SbrChannel::new()],
            analysis: [sbr::dsp::SbrAnalysis::new(), sbr::dsp::SbrAnalysis::new()],
            synthesis: [sbr::dsp::SbrSynthesis::new(), sbr::dsp::SbrSynthesis::new()],
            dsp: sbr::dsp::SbrDsp::new(),
            active: false,
            ps: None,
            ps_active: false,
            ps_data_prev_frame: false,
            ps_data_this_frame: false,
            frame_valid: false,
            num_time_slots,
        }
    }

    fn begin_frame(&mut self) {
        self.frame_valid = false;
        self.ps_active = false;
        self.ps_data_this_frame = false;
    }

    /// Parse SBR extension payload and update state.
    ///
    /// On CRC mismatch or parse error, sets `frame_valid = false` for concealment
    /// and returns `Ok(())` (not an error, since concealment is the normal response).
    fn decode_sbr_data(
        &mut self,
        sbr_payload: &[u8],
        sbr_payload_bits: usize,
        has_crc: bool,
        is_pair: bool,
        srate: u32,
        ps_enabled: bool,
    ) -> Result<()> {
        self.frame_valid = false;

        let mut br = BitReaderLtr::new(sbr_payload);

        // Validate CRC-10 if present (ISO/IEC 14496-3, 4.6.18.2).
        if has_crc {
            let crc_expected = br.read_bits_leq32(10)? as u16;

            // CRC covers all bits after the 10-bit CRC field.
            let payload_bit_offset = 10;
            let total_bits = sbr_payload_bits;
            if total_bits > payload_bit_offset {
                let payload_bits = total_bits - payload_bit_offset;
                let computed = crc10_payload(sbr_payload, payload_bit_offset, payload_bits);
                if computed != crc_expected {
                    log::warn!(
                        "sbr: CRC-10 mismatch (expected {:#05x}, got {:#05x}), concealing frame",
                        crc_expected,
                        computed
                    );
                    return Ok(());
                }
            }
        }

        // Read SBR header if present.
        if br.read_bool()? {
            match sbr::bs::sbr_read_header(&mut br) {
                Ok(hdr) => {
                    let changed = self.header.differs_from(&hdr);
                    if changed || !self.active {
                        self.active = self.state.init(&hdr, srate).is_ok();
                        self.channels[0].reset();
                        self.channels[1].reset();
                    }
                    self.header = hdr;
                }
                Err(_) => {
                    self.active = false;
                }
            }
        }

        // Lazily allocate PS context if PS is enabled.
        if ps_enabled && self.ps.is_none() {
            self.ps = Some(Box::new(sbr::ps::PsContext::new()));
        }

        // Parse channel data if SBR is active.
        if self.active {
            let ps_ctx = self.ps.as_deref_mut().map(|p| &mut p.common);

            let parse_result = if !is_pair {
                sbr::bs::sbr_read_sce(
                    &mut br,
                    self.header.amp_res,
                    &self.state,
                    &mut self.channels[0],
                    ps_ctx,
                    self.num_time_slots,
                )
            }
            else {
                sbr::bs::sbr_read_cpe(
                    &mut br,
                    self.header.amp_res,
                    &self.state,
                    &mut self.channels,
                    ps_ctx,
                    self.num_time_slots,
                )
            };

            let parsed_ps = match parse_result {
                Ok(parsed_ps) => parsed_ps,
                Err(e) => {
                    log::warn!("sbr: channel parse error: {}, concealing frame", e);
                    self.ps_active = false;
                    return Ok(());
                }
            };

            if parsed_ps && !self.ps_data_prev_frame {
                if let Some(ps) = self.ps.as_deref_mut() {
                    ps.reset_decorrelator_state();
                }
            }
            self.ps_data_this_frame = parsed_ps;
            self.ps_active = parsed_ps && self.ps.as_ref().is_some_and(|p| p.common.start);
        }
        else {
            self.ps_active = false;
        }

        self.frame_valid = true;

        Ok(())
    }

    /// Process one channel through the full SBR pipeline.
    /// Takes core samples (960 or 1024) and produces 2x SBR output samples.
    fn process_channel(&mut self, ch: usize, core_samples: &[f32], output: &mut [f32]) {
        // QMF analysis: split time-domain samples into QMF subbands.
        sbr::synth::analysis(
            &mut self.channels[ch],
            &mut self.analysis[ch],
            &mut self.dsp,
            core_samples,
        );

        if self.active && self.frame_valid {
            // HF generation and envelope adjustment.
            sbr::synth::hf_generate(&mut self.channels[ch], &self.state, self.num_time_slots);
            sbr::synth::x_gen(&mut self.channels[ch], &self.state, self.num_time_slots);
            sbr::synth::hf_adjust(
                &mut self.channels[ch],
                &self.state,
                &self.header,
                self.num_time_slots,
            );
        }
        else {
            sbr::synth::bypass(&mut self.channels[ch], self.num_time_slots);
        }

        // QMF synthesis: reconstruct time-domain output at 2x rate.
        let top = self.synthesis_top_band();
        sbr::synth::synthesis(
            &mut self.channels[ch],
            &mut self.synthesis[ch],
            &mut self.dsp,
            top,
            output,
        );

        sbr::synth::update_frame(&mut self.channels[ch], self.num_time_slots);
        self.ps_data_prev_frame = self.ps_data_this_frame;
    }

    /// Process a mono SBR channel through PS to produce stereo output.
    /// Takes core samples (960 or 1024) and produces 2x stereo SBR output samples.
    fn process_with_ps(&mut self, core_samples: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        // QMF analysis of mono channel.
        sbr::synth::analysis(
            &mut self.channels[0],
            &mut self.analysis[0],
            &mut self.dsp,
            core_samples,
        );

        if self.active && self.frame_valid {
            // HF generation and adjustment (mono).
            sbr::synth::hf_generate(&mut self.channels[0], &self.state, self.num_time_slots);
            sbr::synth::x_gen(&mut self.channels[0], &self.state, self.num_time_slots);
            sbr::synth::hf_adjust(
                &mut self.channels[0],
                &self.state,
                &self.header,
                self.num_time_slots,
            );
        }
        else {
            sbr::synth::bypass(&mut self.channels[0], self.num_time_slots);
        }

        // Convert channel.x (Complex per slot) to QMF format [re/im][slot][band].
        let mut qmf_l = [[[0.0f32; 64]; 38]; 2];
        let mut qmf_r = [[[0.0f32; 64]; 38]; 2];

        // Number of QMF time slots = core_samples / 32 (30 for 960, 32 for 1024).
        let num_slots = (core_samples.len() / 32).min(self.channels[0].x.len());
        for t in 0..num_slots {
            for k in 0..64 {
                qmf_l[0][t][k] = self.channels[0].x[t][k].re;
                qmf_l[1][t][k] = self.channels[0].x[t][k].im;
            }
        }
        for t in num_slots..(num_slots + 6).min(qmf_l[0].len()) {
            let w_idx = sbr::HF_ADJ + t;
            if w_idx < self.channels[0].w.len() {
                for k in 0..5 {
                    qmf_l[0][t][k] = self.channels[0].w[w_idx][k].re;
                    qmf_l[1][t][k] = self.channels[0].w[w_idx][k].im;
                }
            }
        }

        // Apply Parametric Stereo processing.
        if self.ps_active {
            let ps = self.ps.as_deref_mut().expect("active PS frame has context");
            let top = self.state.f[self.state.num_master.min(sbr::SBR_BANDS - 1)];
            sbr::ps::ps_apply(ps, &mut qmf_l, &mut qmf_r, top, self.num_time_slots * 2);
        }
        else {
            // No PS context: copy mono to both channels.
            qmf_r = qmf_l;
        }

        // Copy QMF data back to channel buffers for synthesis.
        for t in 0..num_slots {
            for k in 0..64 {
                self.channels[0].x[t][k].re = qmf_l[0][t][k];
                self.channels[0].x[t][k].im = qmf_l[1][t][k];
                self.channels[1].x[t][k].re = qmf_r[0][t][k];
                self.channels[1].x[t][k].im = qmf_r[1][t][k];
            }
        }

        // QMF synthesis for both channels.
        let top = self.synthesis_top_band();
        sbr::synth::synthesis(
            &mut self.channels[0],
            &mut self.synthesis[0],
            &mut self.dsp,
            top,
            out_l,
        );
        sbr::synth::synthesis(
            &mut self.channels[1],
            &mut self.synthesis[1],
            &mut self.dsp,
            top,
            out_r,
        );

        // Update overlap state.
        sbr::synth::update_frame(&mut self.channels[0], self.num_time_slots);
        sbr::synth::update_frame(&mut self.channels[1], self.num_time_slots);
        self.ps_data_prev_frame = self.ps_data_this_frame;
    }

    #[inline]
    fn synthesis_top_band(&self) -> usize {
        if self.active && self.frame_valid {
            self.state.f[self.state.num_master.min(sbr::SBR_BANDS - 1)].min(sbr::SBR_BANDS)
        }
        else {
            32
        }
    }
}

struct M4AInfo {
    otype: M4AType,
    srate: u32,
    channels: usize,
    samples: usize,
    sbr_ps_info: Option<(u32, usize)>,
    sbr_present: bool,
    ps_present: bool,
    /// SBR downsampled mode: extension sample rate equals core sample rate.
    sbr_downsampled: bool,
}

impl M4AInfo {
    fn new() -> Self {
        Self {
            otype: M4AType::None,
            srate: 0,
            channels: 0,
            samples: 0,
            sbr_ps_info: Option::None,
            sbr_present: false,
            ps_present: false,
            sbr_downsampled: false,
        }
    }

    fn has_sbr(&self) -> bool {
        self.sbr_present || self.sbr_ps_info.is_some()
    }

    fn output_sample_rate(&self) -> u32 {
        if self.has_sbr() && !self.sbr_downsampled {
            self.sbr_ps_info
                .map(|(srate, _)| srate)
                .filter(|&srate| srate > 0)
                .unwrap_or_else(|| self.srate.saturating_mul(2))
        }
        else {
            self.srate
        }
    }

    fn output_channels(&self) -> usize {
        if self.ps_present {
            2
        }
        else {
            self.channels
        }
    }

    fn apply_container_hints(&mut self, params: &AudioCodecParameters) {
        let container_rate = params.sample_rate.unwrap_or(0);
        let container_channels =
            params.channels.as_ref().map(|channels| channels.count()).unwrap_or(0);

        // Backward-compatible implicit SBR leaves the ASC as AAC-LC. In MP4,
        // the sample entry may still carry the final output sample rate. Use
        // that as a configuration hint so the public codec parameters match
        // the eventual SBR output before the first frame is decoded.
        if !self.has_sbr()
            && matches!(self.otype, M4AType::Lc | M4AType::ER_AAC_LC)
            && self.channels <= 2
            && self.srate > 0
            && container_rate == self.srate.saturating_mul(2)
        {
            self.sbr_ps_info = Some((container_rate, 0));
        }

        // PS may also be implicitly signalled in the SBR extension payload.
        // When the core is mono but the container advertises stereo output,
        // expose stereo parameters up front and enable PS parsing.
        if self.has_sbr() && !self.ps_present && self.channels == 1 && container_channels == 2 {
            self.ps_present = true;
        }
    }

    fn read_object_type<B: ReadBitsLtr>(bs: &mut B) -> Result<M4AType> {
        let otypeidx = match bs.read_bits_leq32(5)? {
            idx if idx < 31 => idx as usize,
            31 => (bs.read_bits_leq32(6)? + 32) as usize,
            _ => unreachable!(),
        };

        if otypeidx >= M4A_TYPES.len() {
            Ok(M4AType::Unknown)
        }
        else {
            Ok(M4A_TYPES[otypeidx])
        }
    }

    fn read_sampling_frequency<B: ReadBitsLtr>(bs: &mut B) -> Result<u32> {
        match bs.read_bits_leq32(4)? {
            idx if idx < 15 => Ok(AAC_SAMPLE_RATES[idx as usize]),
            _ => Ok(bs.read_bits_leq32(24)?),
        }
    }

    fn read_channel_config<B: ReadBitsLtr>(bs: &mut B) -> Result<usize> {
        let chidx = bs.read_bits_leq32(4)? as usize;
        if chidx < AAC_CHANNELS.len() {
            Ok(AAC_CHANNELS[chidx])
        }
        else {
            Ok(chidx)
        }
    }

    fn read(&mut self, buf: &[u8]) -> Result<()> {
        let mut bs = BitReaderLtr::new(buf);

        self.otype = Self::read_object_type(&mut bs)?;
        self.srate = Self::read_sampling_frequency(&mut bs)?;

        validate!(self.srate > 0);

        self.channels = Self::read_channel_config(&mut bs)?;

        let signaled_otype = self.otype;
        if (self.otype == M4AType::Sbr) || (self.otype == M4AType::PS) {
            self.sbr_present = true;
            self.ps_present = self.otype == M4AType::PS;

            let ext_srate = Self::read_sampling_frequency(&mut bs)?;
            if ext_srate > 0 && ext_srate == self.srate {
                self.sbr_downsampled = true;
            }
            self.otype = Self::read_object_type(&mut bs)?;

            let ext_chans = if self.otype == M4AType::ER_BSAC {
                Self::read_channel_config(&mut bs)?
            }
            else {
                0
            };

            self.sbr_ps_info = Some((ext_srate, ext_chans));
        }

        match self.otype {
            M4AType::Main
            | M4AType::Lc
            | M4AType::Ssr
            | M4AType::Scalable
            | M4AType::TwinVQ
            | M4AType::ER_AAC_LC
            | M4AType::ER_AAC_LTP
            | M4AType::ER_AAC_Scalable
            | M4AType::ER_TwinVQ
            | M4AType::ER_BSAC
            | M4AType::ER_AAC_LD => {
                // GASpecificConfig
                let short_frame = bs.read_bool()?;

                self.samples = if short_frame { 960 } else { 1024 };

                let depends_on_core = bs.read_bool()?;

                if depends_on_core {
                    let _delay = bs.read_bits_leq32(14)?;
                }

                let extension_flag = bs.read_bool()?;

                if self.channels == 0 {
                    return unsupported_error("aac: program config element");
                }

                if (self.otype == M4AType::Scalable) || (self.otype == M4AType::ER_AAC_Scalable) {
                    let _layer = bs.read_bits_leq32(3)?;
                }

                if extension_flag {
                    if self.otype == M4AType::ER_BSAC {
                        let _num_subframes = bs.read_bits_leq32(5)? as usize;
                        let _layer_length = bs.read_bits_leq32(11)?;
                    }

                    if (self.otype == M4AType::ER_AAC_LC)
                        || (self.otype == M4AType::ER_AAC_LTP)
                        || (self.otype == M4AType::ER_AAC_Scalable)
                        || (self.otype == M4AType::ER_AAC_LD)
                    {
                        let _section_data_resilience = bs.read_bool()?;
                        let _scalefactors_resilience = bs.read_bool()?;
                        let _spectral_data_resilience = bs.read_bool()?;
                    }

                    let extension_flag3 = bs.read_bool()?;

                    if extension_flag3 {
                        return unsupported_error("aac: version3 extensions");
                    }
                }
            }
            M4AType::Celp => {
                return unsupported_error("aac: CELP config");
            }
            M4AType::Hvxc => {
                return unsupported_error("aac: HVXC config");
            }
            M4AType::Ttsi => {
                return unsupported_error("aac: TTS config");
            }
            M4AType::MainSynth
            | M4AType::WavetableSynth
            | M4AType::GeneralMIDI
            | M4AType::Algorithmic => {
                return unsupported_error("aac: structured audio config");
            }
            M4AType::ER_CELP => {
                return unsupported_error("aac: ER CELP config");
            }
            M4AType::ER_HVXC => {
                return unsupported_error("aac: ER HVXC config");
            }
            M4AType::ER_HILN | M4AType::ER_Parametric => {
                return unsupported_error("aac: parametric config");
            }
            M4AType::Ssc => {
                return unsupported_error("aac: SSC config");
            }
            M4AType::MPEGSurround => {
                // bs.ignore_bits(1)?; // sacPayloadEmbedding
                return unsupported_error("aac: MPEG Surround config");
            }
            M4AType::Layer1 | M4AType::Layer2 | M4AType::Layer3 => {
                return unsupported_error("aac: MPEG Layer 1/2/3 config");
            }
            M4AType::Dst => {
                return unsupported_error("aac: DST config");
            }
            M4AType::Als => {
                // bs.ignore_bits(5)?; // fillBits
                return unsupported_error("aac: ALS config");
            }
            M4AType::Sls | M4AType::SLSNonCore => {
                return unsupported_error("aac: SLS config");
            }
            M4AType::ER_AAC_ELD => {
                return unsupported_error("aac: ELD config");
            }
            M4AType::SMRSimple | M4AType::SMRMain => {
                return unsupported_error("aac: symbolic music config");
            }
            _ => {}
        };

        match self.otype {
            M4AType::ER_AAC_LC
            | M4AType::ER_AAC_LTP
            | M4AType::ER_AAC_Scalable
            | M4AType::ER_TwinVQ
            | M4AType::ER_BSAC
            | M4AType::ER_AAC_LD
            | M4AType::ER_CELP
            | M4AType::ER_HVXC
            | M4AType::ER_HILN
            | M4AType::ER_Parametric
            | M4AType::ER_AAC_ELD => {
                let ep_config = bs.read_bits_leq32(2)?;

                if (ep_config == 2) || (ep_config == 3) {
                    return unsupported_error("aac: error protection config");
                }
            }
            _ => {}
        };

        // Explicit backward-compatible SBR/PS signaling (ISO 14496-3 §1.6.5.3).
        // Check for 0x2B7 sync word after GASpecificConfig regardless of whether
        // implicit signaling (AOT=SBR/PS) was used. This handles the common DAB+
        // case: AOT=LC + explicit SBR extension.
        if bs.bits_left() >= 16 {
            let sync = bs.read_bits_leq32(11)?;

            if sync == 0x2B7 {
                let ext_otype = Self::read_object_type(&mut bs)?;
                if ext_otype == M4AType::Sbr {
                    self.sbr_present = bs.read_bool()?;
                    if self.sbr_present {
                        let ext_srate = Self::read_sampling_frequency(&mut bs)?;
                        if ext_srate > 0 && ext_srate == self.srate {
                            self.sbr_downsampled = true;
                        }
                        self.sbr_ps_info = Some((ext_srate, 0));
                        if bs.bits_left() >= 12 {
                            let sync = bs.read_bits_leq32(11)?;
                            if sync == 0x548 {
                                self.ps_present = bs.read_bool()?;
                            }
                        }
                    }
                }
                if ext_otype == M4AType::PS {
                    self.ps_present = true;
                    self.sbr_present = bs.read_bool()?;
                    if self.sbr_present {
                        let ext_srate = Self::read_sampling_frequency(&mut bs)?;
                        if ext_srate > 0 && ext_srate == self.srate {
                            self.sbr_downsampled = true;
                        }
                        self.sbr_ps_info = Some((ext_srate, 0));
                    }
                    let _ext_channels = bs.read_bits_leq32(4)?;
                }
            }
        }

        if signaled_otype == M4AType::PS {
            self.ps_present = true;
        }

        Ok(())
    }
}

impl std::fmt::Display for M4AInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MPEG 4 Audio {}, {} Hz, {} channels, {} samples per frame",
            self.otype, self.srate, self.channels, self.samples
        )
    }
}

/// Advanced Audio Coding (AAC) decoder.
///
/// Implements a decoder for Advanced Audio Decoding Low-Complexity (AAC-LC) as defined in
/// ISO/IEC 13818-7 and ISO/IEC 14496-3. Supports HE-AAC v1 (SBR) for bandwidth extension.
pub struct AacDecoder {
    m4ainfo: M4AInfo,
    opts: AudioDecoderOptions,
    pairs: Vec<cpe::ChannelPair>,
    dsp: dsp::Dsp,
    sbinfo: GASubbandInfo,
    params: AudioCodecParameters,
    /// Core AAC output buffer (960 or 1024 samples per channel).
    buf: AudioBuffer<f32>,
    /// SBR output buffer (1920 or 2048 samples per channel at 2x sample rate).
    sbr_buf: Option<AudioBuffer<f32>>,
    /// SBR processor state, lazily allocated on first SBR data.
    sbr: Option<Box<SbrProcessor>>,
    /// Whether this frame produced SBR output (determines which buffer to return).
    sbr_output: bool,
    /// Temporary buffer for core audio during SBR processing.
    core_tmp: Vec<f32>,
}

impl AacDecoder {
    pub fn try_new(params: &AudioCodecParameters, opts: &AudioDecoderOptions) -> Result<Self> {
        // This decoder only supports AAC.
        if params.codec != CODEC_ID_AAC {
            return unsupported_error("aac: invalid codec");
        }

        let mut m4ainfo = M4AInfo::new();

        // If extra data present, parse the audio specific config
        if let Some(extra_data_buf) = &params.extra_data {
            validate!(extra_data_buf.len() >= 2);
            m4ainfo.read(extra_data_buf)?;
            m4ainfo.apply_container_hints(params);
        }
        else {
            // Otherwise, assume there is no ASC and use the codec parameters for ADTS.
            m4ainfo.otype = M4AType::Lc;
            m4ainfo.samples = 1024;

            m4ainfo.srate = match params.sample_rate {
                Some(rate) => rate,
                None => return unsupported_error("aac: sample rate is required"),
            };

            m4ainfo.channels = if let Some(channels) = &params.channels {
                channels.count()
            }
            else {
                return unsupported_error("aac: channels or channel layout is required");
            };
        }

        debug!(
            "aac: init otype={} srate={} output_srate={} channels={} output_channels={} samples={} sbr={} ps={} sbr_ds={}",
            m4ainfo.otype,
            m4ainfo.srate,
            m4ainfo.output_sample_rate(),
            m4ainfo.channels,
            m4ainfo.output_channels(),
            m4ainfo.samples,
            m4ainfo.sbr_present,
            m4ainfo.ps_present,
            m4ainfo.sbr_downsampled,
        );

        if !matches!(m4ainfo.otype, M4AType::Lc | M4AType::ER_AAC_LC)
            || (m4ainfo.samples != 1024 && m4ainfo.samples != 960)
        {
            return unsupported_error("aac: unsupported object type or frame length");
        }

        let frame_len = m4ainfo.samples;

        // Reject downsampled SBR mode (ext_srate == core_srate instead of 2x).
        if m4ainfo.sbr_downsampled {
            return unsupported_error("aac: downsampled SBR mode not supported");
        }

        // Map channel count to a set of channels.
        let channels = map_to_channels(m4ainfo.channels).unwrap();
        let output_channels = map_to_channels(m4ainfo.output_channels()).unwrap();
        let output_sample_rate = m4ainfo.output_sample_rate();

        // Clone and amend the codec parameters with information from the extra data.
        let mut params = params.clone();
        params.with_channels(output_channels.clone()).with_sample_rate(output_sample_rate);

        // Select the correct scalefactor band tables based on frame length.
        let sbinfo = GASubbandInfo::find_for_frame_len(frame_len, m4ainfo.srate);

        let buf = AudioBuffer::new(AudioSpec::new(m4ainfo.srate, channels.clone()), frame_len);

        // Number of SBR time slots per frame (15 for 960 core, 16 for 1024 core).
        let num_time_slots = frame_len / 64;

        // If SBR is signaled in the AudioSpecificConfig, pre-allocate the SBR output buffer.
        // SBR is only used with mono/stereo content. Multi-channel AAC uses plain AAC-LC.
        // For PS (parametric stereo), output is always stereo even when core is mono.
        let (sbr, sbr_buf) = if m4ainfo.has_sbr() && m4ainfo.channels <= 2 {
            let sbr_channels = if m4ainfo.ps_present {
                // PS produces stereo from mono.
                map_to_channels(2).unwrap()
            }
            else {
                channels.clone()
            };
            let sbr_spec = AudioSpec::new(output_sample_rate, sbr_channels);
            let sbr_output_samples = frame_len * 2;
            (
                Some(Box::new(SbrProcessor::with_num_time_slots(num_time_slots))),
                Some(AudioBuffer::new(sbr_spec, sbr_output_samples)),
            )
        }
        else {
            (None, None)
        };

        Ok(AacDecoder {
            m4ainfo,
            opts: *opts,
            pairs: Vec::new(),
            dsp: dsp::Dsp::with_frame_len(frame_len),
            sbinfo,
            params,
            buf,
            sbr_buf,
            sbr,
            sbr_output: false,
            core_tmp: vec![0.0f32; frame_len],
        })
    }

    fn set_pair(&mut self, pair_no: usize, channel: usize, pair: bool) -> Result<()> {
        if self.pairs.len() <= pair_no {
            self.pairs.push(cpe::ChannelPair::new(pair, channel, self.sbinfo));
        }
        else {
            validate!(self.pairs[pair_no].channel == channel);
            validate!(self.pairs[pair_no].is_pair == pair);
        }
        validate!(if pair { channel + 1 } else { channel } < self.m4ainfo.channels);
        Ok(())
    }

    fn decode_ga<B: ReadBitsLtr + FiniteBitStream>(&mut self, bs: &mut B) -> Result<()> {
        let mut cur_pair = 0;
        let mut cur_ch = 0;

        // Collect SBR fill data: (pair_index, payload bytes, payload bit length, has_crc).
        let mut sbr_fills: Vec<(usize, Vec<u8>, usize, bool)> = Vec::new();

        while bs.bits_left() > 3 {
            let id = bs.read_bits_leq32(3)?;

            match id {
                0 => {
                    // ID_SCE
                    let _tag = bs.read_bits_leq32(4)?;
                    self.set_pair(cur_pair, cur_ch, false)?;
                    self.pairs[cur_pair].decode_ga_sce(bs, self.m4ainfo.otype)?;
                    cur_pair += 1;
                    cur_ch += 1;
                }
                1 => {
                    // ID_CPE
                    let _tag = bs.read_bits_leq32(4)?;
                    self.set_pair(cur_pair, cur_ch, true)?;
                    self.pairs[cur_pair].decode_ga_cpe(bs, self.m4ainfo.otype)?;
                    cur_pair += 1;
                    cur_ch += 2;
                }
                2 => {
                    // ID_CCE
                    return unsupported_error("aac: coupling channel element");
                }
                3 => {
                    // ID_LFE
                    let _tag = bs.read_bits_leq32(4)?;
                    self.set_pair(cur_pair, cur_ch, false)?;
                    self.pairs[cur_pair].decode_ga_sce(bs, self.m4ainfo.otype)?;
                    cur_pair += 1;
                    cur_ch += 1;
                }
                4 => {
                    // ID_DSE
                    let _id = bs.read_bits_leq32(4)?;
                    let align = bs.read_bool()?;
                    let mut count = bs.read_bits_leq32(8)?;
                    if count == 255 {
                        count += bs.read_bits_leq32(8)?;
                    }
                    if align {
                        bs.realign();
                    }
                    bs.ignore_bits(count * 8)?;
                }
                5 => {
                    // ID_PCE
                    return unsupported_error("aac: program config");
                }
                6 => {
                    // ID_FIL
                    let mut count = bs.read_bits_leq32(4)? as usize;
                    if count == 15 {
                        count += bs.read_bits_leq32(8)? as usize;
                        count -= 1;
                    }

                    if count > 0 {
                        let ext_type = bs.read_bits_leq32(4)?;

                        match ext_type {
                            // EXT_SBR_DATA (0xd) or EXT_SBR_DATA_CRC (0xe)
                            0xd | 0xe => {
                                let has_crc = ext_type == 0xe;

                                let payload_bits = count * 8 - 4;
                                let sbr_data = read_bits_to_vec(bs, payload_bits)?;

                                // Associate with the most recently decoded channel pair.
                                if cur_pair > 0 {
                                    sbr_fills.push((cur_pair - 1, sbr_data, payload_bits, has_crc));
                                }
                            }
                            _ => {
                                // Skip non-SBR extension payload.
                                bs.ignore_bits(4)?;
                                for _ in 0..count - 1 {
                                    bs.ignore_bits(8)?;
                                }
                            }
                        }
                    }
                }
                7 => {
                    // ID_TERM
                    break;
                }
                _ => unreachable!(),
            };
        }

        // Check if we have SBR data to process.
        let have_sbr = !sbr_fills.is_empty();

        // SBR payload is frame-local. If this AAC frame has no valid SBR
        // extension, the processing path must conceal/bypass instead of
        // reusing the previous frame's envelope or PS extension data.
        if let Some(sbr) = &mut self.sbr {
            sbr.begin_frame();
        }

        // Lazily initialize SBR processor on first SBR fill element.
        // SBR is only supported for mono/stereo (channels <= 2).
        if have_sbr && self.sbr.is_none() && self.m4ainfo.channels <= 2 {
            let num_time_slots = self.m4ainfo.samples / 64;
            self.sbr = Some(Box::new(SbrProcessor::with_num_time_slots(num_time_slots)));
            // For PS, always allocate stereo output buffer.
            let sbr_channels = if self.m4ainfo.ps_present {
                map_to_channels(2).unwrap()
            }
            else {
                map_to_channels(self.m4ainfo.channels).unwrap()
            };
            let sbr_output_samples = self.m4ainfo.samples * 2;
            let sbr_spec = AudioSpec::new(self.m4ainfo.output_sample_rate(), sbr_channels);
            self.sbr_buf = Some(AudioBuffer::new(sbr_spec, sbr_output_samples));
        }

        // Parse SBR extension data.
        if let Some(sbr) = &mut self.sbr {
            let ps_enabled = self.m4ainfo.ps_present;
            let sbr_sample_rate = self.m4ainfo.output_sample_rate();
            for (pair_idx, sbr_data, payload_bits, has_crc) in &sbr_fills {
                let is_pair = self.pairs[*pair_idx].is_pair;
                if let Err(e) = sbr.decode_sbr_data(
                    sbr_data,
                    *payload_bits,
                    *has_crc,
                    is_pair,
                    sbr_sample_rate,
                    ps_enabled,
                ) {
                    log::warn!("sbr: decode error: {}", e);
                }
            }
        }

        let rate_idx = GASubbandInfo::find_idx(self.m4ainfo.srate);

        if self.sbr.is_some() {
            // SBR mode: synthesize core audio to temp buffer, then SBR process to output.
            if let Some(sbr_buf) = &mut self.sbr_buf {
                sbr_buf.clear();
                sbr_buf.render_silence(None);
            }

            // Use the PS path whenever the ASC signaled PS and the output buffer is
            // stereo. Even before PS data arrives (ps_active == false), this ensures
            // mono is copied to both L and R instead of leaving R silent.
            let use_ps = self.m4ainfo.ps_present || self.sbr.as_ref().is_some_and(|s| s.ps_active);

            for pair_idx in 0..cur_pair {
                let is_pair = self.pairs[pair_idx].is_pair;
                let nch = if is_pair { 2 } else { 1 };

                if use_ps && !is_pair {
                    // PS mode: mono SCE -> stereo output via Parametric Stereo.
                    // Synthesize core AAC to temp buffer.
                    self.pairs[pair_idx].synth_channel_to_buf(
                        &mut self.dsp,
                        rate_idx,
                        0,
                        &mut self.core_tmp,
                    );

                    // Ensure sbr_buf has 2 channels for PS stereo output.
                    if let (Some(sbr), Some(sbr_buf)) = (&mut self.sbr, &mut self.sbr_buf) {
                        let (left, right) = sbr_buf.plane_pair_mut(0, 1).unwrap();
                        sbr.process_with_ps(&self.core_tmp, left, right);
                    }
                }
                else {
                    // Normal SBR mode: per-channel processing.
                    for ch in 0..nch {
                        let ch_idx = self.pairs[pair_idx].channel + ch;

                        // Synthesize core AAC to temp buffer.
                        self.pairs[pair_idx].synth_channel_to_buf(
                            &mut self.dsp,
                            rate_idx,
                            ch,
                            &mut self.core_tmp,
                        );

                        // Run SBR pipeline: core samples -> 2x SBR output samples.
                        if let (Some(sbr), Some(sbr_buf)) = (&mut self.sbr, &mut self.sbr_buf) {
                            sbr.process_channel(
                                ch_idx,
                                &self.core_tmp,
                                sbr_buf.plane_mut(ch_idx).unwrap(),
                            );
                        }
                    }
                }
            }
            self.sbr_output = true;
        }
        else {
            // Non-SBR mode: direct synthesis to output buffer.
            for pair_idx in 0..cur_pair {
                self.pairs[pair_idx].synth_audio(&mut self.dsp, &mut self.buf, rate_idx);
            }
            self.sbr_output = false;
        }

        Ok(())
    }

    fn decode_inner(&mut self, packet: &Packet) -> Result<()> {
        // Clear the core audio output buffer.
        self.buf.clear();
        self.buf.render_silence(None);
        self.sbr_output = false;

        let mut bs = BitReaderLtr::new(packet.buf());

        // Choose decode step based on the object type.
        match self.m4ainfo.otype {
            M4AType::Lc | M4AType::ER_AAC_LC => self.decode_ga(&mut bs)?,
            _ => return unsupported_error("aac: object type"),
        }

        Ok(())
    }
}

impl AudioDecoder for AacDecoder {
    fn reset(&mut self) {
        for pair in self.pairs.iter_mut() {
            pair.reset();
        }
        if let Some(sbr) = &mut self.sbr {
            sbr.channels[0].reset();
            sbr.channels[1].reset();
            sbr.active = false;
            sbr.ps_active = false;
            // Drop PS context; it will be re-allocated lazily.
            sbr.ps = None;
        }
    }

    fn codec_info(&self) -> &CodecInfo {
        // Only one codec is supported.
        &Self::supported_codecs().first().unwrap().info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: &Packet) -> Result<GenericAudioBufferRef<'_>> {
        if let Err(e) = self.decode_inner(packet) {
            self.buf.clear();
            Err(e)
        }
        else if self.sbr_output {
            if self.opts.gapless {
                self.sbr_buf
                    .as_mut()
                    .unwrap()
                    .trim(packet.trim_start().get() as usize, packet.trim_end().get() as usize);
            }
            Ok(self.sbr_buf.as_ref().unwrap().as_generic_audio_buffer_ref())
        }
        else {
            if self.opts.gapless {
                self.buf.trim(packet.trim_start().get() as usize, packet.trim_end().get() as usize);
            }
            Ok(self.buf.as_generic_audio_buffer_ref())
        }
    }

    fn finalize(&mut self) -> FinalizeResult {
        Default::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        if self.sbr_output {
            if let Some(sbr_buf) = &self.sbr_buf {
                return sbr_buf.as_generic_audio_buffer_ref();
            }
        }
        self.buf.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for AacDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>>
    where
        Self: Sized,
    {
        Ok(Box::new(AacDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        use symphonia_core::codecs::audio::well_known::profiles::CODEC_PROFILE_AAC_LC;

        &[support_audio_codec!(
            CODEC_ID_AAC,
            "aac",
            "Advanced Audio Coding",
            &[codec_profile!(CODEC_PROFILE_AAC_LC, "aac-lc", "Low Complexity"),]
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack_bits(fields: &[(u32, u8)]) -> Vec<u8> {
        let num_bits = fields.iter().map(|&(_, n)| n as usize).sum::<usize>();
        let mut out = vec![0u8; num_bits.div_ceil(8)];
        let mut bit_pos = 0;

        for &(value, width) in fields {
            for bit in (0..width).rev() {
                if ((value >> bit) & 1) != 0 {
                    out[bit_pos / 8] |= 1 << (7 - (bit_pos & 7));
                }
                bit_pos += 1;
            }
        }

        out
    }

    fn make_decoder_from_asc(asc: Vec<u8>) -> AacDecoder {
        let mut params = AudioCodecParameters::new();
        params.for_codec(CODEC_ID_AAC).with_extra_data(asc.into_boxed_slice());
        AacDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap()
    }

    fn make_decoder_from_asc_with_container(
        asc: Vec<u8>,
        sample_rate: u32,
        channels: usize,
    ) -> AacDecoder {
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_AAC)
            .with_sample_rate(sample_rate)
            .with_channels(map_to_channels(channels).unwrap())
            .with_extra_data(asc.into_boxed_slice());
        AacDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap()
    }

    #[test]
    fn implicit_sbr_sets_output_sample_rate() {
        // AOT=SBR, core sample rate 22050 Hz, extension/output sample rate 44100 Hz,
        // stereo AAC-LC core. This matches the YouTube HE-AAC AudioSpecificConfig shape.
        let asc =
            vec![0x2b, 0x92, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let mut info = M4AInfo::new();
        info.read(&asc).unwrap();

        assert!(info.has_sbr());
        assert!(info.sbr_present);
        assert!(!info.ps_present);
        assert_eq!(info.srate, 22_050);
        assert_eq!(info.output_sample_rate(), 44_100);
        assert_eq!(info.output_channels(), 2);

        let decoder = make_decoder_from_asc(asc);
        assert_eq!(decoder.codec_params().sample_rate, Some(44_100));
        assert_eq!(decoder.codec_params().channels.as_ref().map(|ch| ch.count()), Some(2));
    }

    #[test]
    fn implicit_ps_sets_stereo_output() {
        // AOT=PS, mono AAC-LC core. PS must advertise stereo output even though
        // the underlying AAC core decodes one channel.
        let asc = pack_bits(&[
            (29, 5), // audioObjectType = PS.
            (7, 4),  // core sample rate = 22050 Hz.
            (1, 4),  // mono core channel configuration.
            (4, 4),  // extension/output sample rate = 44100 Hz.
            (2, 5),  // extension audioObjectType = AAC-LC.
            (0, 1),  // frameLengthFlag.
            (0, 1),  // dependsOnCoreCoder.
            (0, 1),  // extensionFlag.
        ]);
        let mut info = M4AInfo::new();
        info.read(&asc).unwrap();

        assert!(info.has_sbr());
        assert!(info.sbr_present);
        assert!(info.ps_present);
        assert_eq!(info.channels, 1);
        assert_eq!(info.output_channels(), 2);
        assert_eq!(info.output_sample_rate(), 44_100);

        let decoder = make_decoder_from_asc(asc);
        assert_eq!(decoder.codec_params().sample_rate, Some(44_100));
        assert_eq!(decoder.codec_params().channels.as_ref().map(|ch| ch.count()), Some(2));
    }

    #[test]
    fn container_hints_enable_implicit_sbr_and_ps_output() {
        // Fraunhofer SBRtestStereoAot29Sig0.mp4: AAC-LC ASC with mono core
        // 22050 Hz, while the MP4 sample entry advertises 44100 Hz stereo.
        let asc = vec![0x13, 0x88];

        let decoder = make_decoder_from_asc_with_container(asc, 44_100, 2);

        assert!(decoder.m4ainfo.has_sbr());
        assert!(decoder.m4ainfo.ps_present);
        assert_eq!(decoder.m4ainfo.srate, 22_050);
        assert_eq!(decoder.codec_params().sample_rate, Some(44_100));
        assert_eq!(decoder.codec_params().channels.as_ref().map(|ch| ch.count()), Some(2));
    }

    #[test]
    fn container_hints_enable_implicit_ps_for_aot5_sbr() {
        // Fraunhofer SBRtestStereoAot5SigusePS.mp4: AOT=SBR with mono AAC-LC
        // core and PS carried implicitly in the SBR extension payload.
        let asc = vec![0x2b, 0x8a, 0x08, 0x00];

        let decoder = make_decoder_from_asc_with_container(asc, 44_100, 2);

        assert!(decoder.m4ainfo.has_sbr());
        assert!(decoder.m4ainfo.sbr_present);
        assert!(decoder.m4ainfo.ps_present);
        assert_eq!(decoder.m4ainfo.channels, 1);
        assert_eq!(decoder.codec_params().sample_rate, Some(44_100));
        assert_eq!(decoder.codec_params().channels.as_ref().map(|ch| ch.count()), Some(2));
    }

    #[test]
    fn explicit_sampling_frequency_reads_24_bits() {
        // AOT=AAC-LC, explicit core sample rate 12345 Hz, mono, standard GA config.
        let asc = pack_bits(&[
            (2, 5),       // audioObjectType = AAC-LC.
            (15, 4),      // samplingFrequencyIndex = explicit.
            (12_345, 24), // samplingFrequency.
            (1, 4),       // mono channel configuration.
            (0, 1),       // frameLengthFlag.
            (0, 1),       // dependsOnCoreCoder.
            (0, 1),       // extensionFlag.
        ]);
        let mut info = M4AInfo::new();
        info.read(&asc).unwrap();

        assert_eq!(info.srate, 12_345);
        assert_eq!(info.output_sample_rate(), 12_345);
    }

    #[test]
    fn sbr_frame_state_is_frame_local() {
        let mut sbr = SbrProcessor::with_num_time_slots(16);
        sbr.frame_valid = true;
        sbr.ps_active = true;

        sbr.begin_frame();

        assert!(!sbr.frame_valid);
        assert!(!sbr.ps_active);
    }

    #[test]
    fn non_byte_aligned_sbr_payload_keeps_exact_bit_count() {
        let mut reader = BitReaderLtr::new(&[0b1010_1100, 0b1111_0000]);
        let payload = read_bits_to_vec(&mut reader, 9).unwrap();

        assert_eq!(payload, vec![0b1010_1100, 0b1000_0000]);
    }

    #[test]
    fn crc10_payload_ignores_padding_bits() {
        // Ten leading CRC bits followed by a 9-bit SBR payload and seven padding bits.
        let data = pack_bits(&[(0, 10), (0b101100101, 9), (0b1111111, 7)]);
        let expected_payload = [0b1011_0010, 0b1000_0000];

        assert_eq!(crc10_payload(&data, 10, 9), sbr::sbr_crc10(&expected_payload, 9));
        assert_ne!(crc10_payload(&data, 10, 16), crc10_payload(&data, 10, 9));
    }
}
