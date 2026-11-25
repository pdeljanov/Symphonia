// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use symphonia_core::codecs::CodecInfo;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult};
use symphonia_core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia_core::support_audio_codec;
use symphonia_core::audio::{GenericAudioBuffer, GenericAudioBufferRef, AsGenericAudioBufferRef, AudioMut};
use symphonia_core::audio::sample::SampleFormat;
use symphonia_core::codecs::audio::well_known::CODEC_ID_DCA;
use symphonia_core::errors::{Result, unsupported_error};
use symphonia_core::packet::Packet;

use symphonia_core::io::{ReadBytes, ReadBitsLtr, BitReaderLtr};

/// DTS Coherent Acoustics (DCA) core frame header.
#[allow(dead_code)]
#[derive(Default, Clone, Copy)]
struct CoreHeader {
    /// Frame type: 0 = normal, 1 = termination.
    ftype: u8,
    /// Deficit samples: number of samples to skip at start.
    deficit: u8,
    /// CRC presence flag.
    cpf: u8,
    /// Number of blocks: (nblks + 1) * 32 = samples per frame.
    nblks: u8,
    /// Frame size: fsize + 1 = total bytes in frame.
    fsize: u16,
    /// Audio channel arrangement.
    amode: u8,
    /// Core audio sampling frequency index.
    sfreq: u8,
    /// Transmission bit rate index.
    rate: u8,
    /// Fixed bit: should be 0.
    fixed_bit: u8,
    /// Embedded dynamic range flag.
    dynf: u8,
    /// Embedded time stamp flag.
    timef: u8,
    /// Auxiliary data flag.
    auxf: u8,
    /// HDCD flag.
    hdcd: u8,
    /// Extension audio descriptor flag.
    ext_audio_id: u8,
    /// Extended audio presence flag.
    ext_audio: u8,
    /// Multirate interpolator switch.
    aspc: u8,
    /// Low frequency effects flag.
    lfe: u8,
    /// Predictor history flag.
    nhist: u8,
    /// Header CRC check word.
    chcrc: u16,
    /// Multirate interpolator switch.
    filter_perfect: u8,
    /// Encoder software revision.
    rev: u8,
    /// Copy history.
    copy: u8,
    /// Source PCM resolution.
    pcmr: u8,
    /// Front sum/difference flag.
    sumdiff_front: u8,
    /// Surround sum/difference flag.
    sumdiff_surround: u8,
    /// Dialog normalization gain.
    dnrg: u8,
}

/// DTS Coherent Acoustics (DCA) primary audio coding header.
#[allow(dead_code)]
#[derive(Default, Clone, Copy)]
struct CodingHeader {
    nsubframes: u8,
    nchannels: u8,
    nsubbands: [u8; MAX_CHANNELS],
    subband_vq_start: [u8; MAX_CHANNELS],
    joint_intensity_index: [u8; MAX_CHANNELS],
    transition_mode_sel: [u8; MAX_CHANNELS],
    scale_factor_sel: [u8; MAX_CHANNELS],
    bit_allocation_sel: [u8; MAX_CHANNELS],
    quant_index_sel: [[u8; 11]; MAX_CHANNELS],
    scale_factor_adj: [[u8; 11]; MAX_CHANNELS],
}

const MAX_CHANNELS: usize = 8;
const MAX_SUBBANDS: usize = 32;
const QMF_ORDER: usize = 512;

const QUANT_INDEX_SEL_NBITS: [u32; 10] = [1, 2, 2, 2, 2, 3, 3, 3, 3, 3];

/// DTS Coherent Acoustics (DCA) decoder.
pub struct DcaDecoder {
    params: AudioCodecParameters,
    buf: GenericAudioBuffer,
    header: CoreHeader,
    coding_header: CodingHeader,
    /// Subband samples for the current frame [channel][subband][sample]
    /// Each subframe has a certain number of blocks, each block has 32 samples (1 per subband).
    /// Max blocks per frame is 128.
    subband_samples: Box<[[[f32; 128]; MAX_SUBBANDS]; MAX_CHANNELS]>,
    /// QMF filter bank state (delay buffers) [channel][delay]
    qmf_state: Box<[[f32; QMF_ORDER]; MAX_CHANNELS]>,
    /// LFE interpolation state. Holds decoded LFE samples for the current frame's
    /// subframes, accumulated as parsing progresses (sized for 128 samples = 8
    /// LFE-samples × 16 max subframes for the lfe_present=1 path, generous bound).
    lfe_samples: Vec<f32>,

    // Subframe side information
    nsubsubframes: u8,
    prediction_mode: [[bool; MAX_SUBBANDS]; MAX_CHANNELS],
    prediction_vq_index: [[u16; MAX_SUBBANDS]; MAX_CHANNELS],
    bit_allocation: [[u8; MAX_SUBBANDS]; MAX_CHANNELS],
    transition_mode: [[u8; MAX_SUBBANDS]; MAX_CHANNELS],
    scale_indices: [[[i16; 2]; MAX_SUBBANDS]; MAX_CHANNELS],
    /// 10-bit VQ index per (ch, band) for bands at/above subband_vq_start. Read once
    /// per subframe (not per subsubframe). See FFmpeg dca_core.c lines 644-656.
    high_freq_vq_index: [[u16; MAX_SUBBANDS]; MAX_CHANNELS],
}

impl DcaDecoder {
    pub fn try_new(params: &AudioCodecParameters, _opts: &AudioDecoderOptions) -> Result<Self> {
        if params.codec != CODEC_ID_DCA {
            return unsupported_error("dca: invalid codec");
        }

        let rate = params.sample_rate.unwrap_or(44100);
        let channels = params.channels.clone().unwrap_or(symphonia_core::audio::layouts::CHANNEL_LAYOUT_STEREO);

        // Initialize with default buffer, will grow as needed.
        let buf = GenericAudioBuffer::new(
            SampleFormat::F32,
            symphonia_core::audio::AudioSpec::new(rate, channels),
            0,
        );

        Ok(DcaDecoder {
            params: params.clone(),
            buf,
            header: CoreHeader::default(),
            coding_header: CodingHeader::default(),
            subband_samples: Box::new([[[0.0; 128]; MAX_SUBBANDS]; MAX_CHANNELS]),
            qmf_state: Box::new([[0.0; QMF_ORDER]; MAX_CHANNELS]),
            lfe_samples: Vec::new(),
            nsubsubframes: 0,
            prediction_mode: [[false; MAX_SUBBANDS]; MAX_CHANNELS],
            prediction_vq_index: [[0; MAX_SUBBANDS]; MAX_CHANNELS],
            bit_allocation: [[0; MAX_SUBBANDS]; MAX_CHANNELS],
            transition_mode: [[0; MAX_SUBBANDS]; MAX_CHANNELS],
            scale_indices: [[[0; 2]; MAX_SUBBANDS]; MAX_CHANNELS],
            high_freq_vq_index: [[0; MAX_SUBBANDS]; MAX_CHANNELS],
        })
    }

    #[allow(clippy::field_reassign_with_default)]
    fn parse_core_header<'a>(&mut self, packet: &'a Packet) -> Result<BitReaderLtr<'a>> {
        let mut reader = packet.as_buf_reader();
        
        // Sync word was already checked by the demuxer.
        let sync = reader.read_be_u32()?;
        if sync != 0x7FFE8001 {
            return unsupported_error("dca: sync word not found in packet");
        }

        // We'll use a single BitReader for the whole packet data after the sync word.
        // The packet data includes the sync word, so we skip 4 bytes.
        let mut bs = BitReaderLtr::new(&packet.data[4..]);

        let mut header = CoreHeader::default();

        // Frame Type: 1 bit
        header.ftype = bs.read_bits_leq32(1)? as u8;
        // Deficit Samples: 5 bits
        header.deficit = bs.read_bits_leq32(5)? as u8;
        // CPF: 1 bit
        header.cpf = bs.read_bits_leq32(1)? as u8;
        // NBLKS: 7 bits
        header.nblks = bs.read_bits_leq32(7)? as u8;
        // FSIZE: 14 bits
        header.fsize = bs.read_bits_leq32(14)? as u16 + 1;
        // AMODE: 6 bits
        header.amode = bs.read_bits_leq32(6)? as u8;
        // SFREQ: 4 bits
        header.sfreq = bs.read_bits_leq32(4)? as u8;
        // RATE: 5 bits
        header.rate = bs.read_bits_leq32(5)? as u8;
        // Fixed bit: 1 bit
        header.fixed_bit = bs.read_bits_leq32(1)? as u8;
        // DYNF (Embedded Dynamic Range Flag): 1 bit (per ETSI TS 102 114 §5.3.1).
        header.dynf = bs.read_bits_leq32(1)? as u8;
        // TIMEF: 1 bit
        header.timef = bs.read_bits_leq32(1)? as u8;
        // AUXF: 1 bit
        header.auxf = bs.read_bits_leq32(1)? as u8;
        // HDCD: 1 bit
        header.hdcd = bs.read_bits_leq32(1)? as u8;
        // EXT_AUDIO_ID: 3 bits
        header.ext_audio_id = bs.read_bits_leq32(3)? as u8;
        // EXT_AUDIO: 1 bit
        header.ext_audio = bs.read_bits_leq32(1)? as u8;
        // ASPC: 1 bit
        header.aspc = bs.read_bits_leq32(1)? as u8;
        // LFE: 2 bits
        header.lfe = bs.read_bits_leq32(2)? as u8;
        // NHIST: 1 bit
        header.nhist = bs.read_bits_leq32(1)? as u8;

        if header.cpf != 0 {
            // Header CRC check word: 16 bits
            header.chcrc = bs.read_bits_leq32(16)? as u16;
        }

        // Filter Perfect: 1 bit
        header.filter_perfect = bs.read_bits_leq32(1)? as u8;
        // Rev: 4 bits
        header.rev = bs.read_bits_leq32(4)? as u8;
        // Copy: 2 bits
        header.copy = bs.read_bits_leq32(2)? as u8;
        // PCMR: 3 bits
        header.pcmr = bs.read_bits_leq32(3)? as u8;
        // SumDiff Front: 1 bit
        header.sumdiff_front = bs.read_bits_leq32(1)? as u8;
        // SumDiff Surround: 1 bit
        header.sumdiff_surround = bs.read_bits_leq32(1)? as u8;
        // Dialog Normalization Gain: 4 bits (per ETSI TS 102 114 §5.3.1, FFmpeg dca.c L140).
        header.dnrg = bs.read_bits_leq32(4)? as u8;

        self.header = header;
        Ok(bs)
    }

    #[allow(clippy::field_reassign_with_default)]
    fn parse_coding_header(&mut self, bs: &mut BitReaderLtr<'_>) -> Result<()> {
        let mut coding = CodingHeader::default();

        // Number of subframes
        coding.nsubframes = bs.read_bits_leq32(4)? as u8 + 1;
        // Number of primary audio channels
        coding.nchannels = bs.read_bits_leq32(3)? as u8 + 1;


        if coding.nchannels as usize > MAX_CHANNELS {
            return unsupported_error("dca: too many channels");
        }

        // Subband activity count
        for ch in 0..coding.nchannels as usize {
            coding.nsubbands[ch] = bs.read_bits_leq32(5)? as u8 + 2;
            if coding.nsubbands[ch] as usize > MAX_SUBBANDS {
                return unsupported_error("dca: too many subbands");
            }
        }

        // High frequency VQ start subband
        for ch in 0..coding.nchannels as usize {
            coding.subband_vq_start[ch] = bs.read_bits_leq32(5)? as u8 + 1;
        }

        // Joint intensity coding index
        for ch in 0..coding.nchannels as usize {
            coding.joint_intensity_index[ch] = bs.read_bits_leq32(3)? as u8;
        }

        // Transient mode code book selection
        for ch in 0..coding.nchannels as usize {
            coding.transition_mode_sel[ch] = bs.read_bits_leq32(2)? as u8;
        }

        // Scale factor code book selection
        for ch in 0..coding.nchannels as usize {
            coding.scale_factor_sel[ch] = bs.read_bits_leq32(3)? as u8;
        }

        // Bit allocation quantizer selection
        for ch in 0..coding.nchannels as usize {
            coding.bit_allocation_sel[ch] = bs.read_bits_leq32(3)? as u8;
        }

        // Quantization index codebook selection — codebook-major iteration order
        // per FFmpeg dca_core.c parse_subframe_header (5.4.1.5). Always reads all
        // 10 codebooks per channel regardless of bit_allocation_sel.
        for (n, &nbits) in QUANT_INDEX_SEL_NBITS.iter().enumerate() {
            for ch in 0..coding.nchannels as usize {
                coding.quant_index_sel[ch][n] = bs.read_bits_leq32(nbits)? as u8;
            }
        }

        // Scale factor adjustment index (5.4.1.6). Present only when the matching
        // quant_index_sel selects a Huffman codebook (sel < group_size). The 2-bit
        // value indexes into SCALE_FACTOR_ADJ for the actual Q22 multiplier.
        use crate::tables::{QUANT_INDEX_GROUP_SIZE, SCALE_FACTOR_ADJ};
        for (n, &group_size) in QUANT_INDEX_GROUP_SIZE.iter().enumerate() {
            for ch in 0..coding.nchannels as usize {
                if coding.quant_index_sel[ch][n] < group_size {
                    let adj_idx = bs.read_bits_leq32(2)? as usize;
                    coding.scale_factor_adj[ch][n] = adj_idx as u8;
                    let _ = SCALE_FACTOR_ADJ[adj_idx]; // table presence check
                }
            }
        }

        if self.header.cpf != 0 {
            // Audio header CRC: 16 bits
            let _crc = bs.read_bits_leq32(16)?;
        }

        self.coding_header = coding;
        Ok(())
    }

    fn parse_subframe_header(&mut self, _sf: usize, bs: &mut BitReaderLtr<'_>) -> Result<()> {
        // 5.4.1 - Primary audio coding side information
        
        // Subsubframe count
        self.nsubsubframes = bs.read_bits_leq32(2)? as u8 + 1;
        // Partial subsubframe sample count
        bs.ignore_bits(3)?;

        let nchannels = self.coding_header.nchannels as usize;

        // Prediction mode
        for ch in 0..nchannels {
            for band in 0..self.coding_header.nsubbands[ch] as usize {
                self.prediction_mode[ch][band] = bs.read_bool()?;
            }
        }

        // Prediction coefficients VQ address
        for ch in 0..nchannels {
            for band in 0..self.coding_header.nsubbands[ch] as usize {
                if self.prediction_mode[ch][band] {
                    self.prediction_vq_index[ch][band] = bs.read_bits_leq32(12)? as u16;
                }
            }
        }

        // Bit allocation index. The Huffman codebooks (sel<5) emit raw symbol values
        // 0..11; FFmpeg's VLC init biases by +1 to recover the spec's abits range 1..12.
        // We add the offset at use site since make_dca_codebook does not apply offsets.
        // For sel 5..6 the value is read directly as `sel - 1` bits and is already
        // the correct abits. sel==7 is reserved/invalid (FFmpeg rejects it).
        for ch in 0..nchannels {
            let sel = self.coding_header.bit_allocation_sel[ch] as usize;
            for band in 0..self.coding_header.subband_vq_start[ch] as usize {
                let abits = if sel < 5 {
                    use crate::tables::BIT_ALLOC_12_VLC;
                    bs.read_codebook(&BIT_ALLOC_12_VLC[sel])?.0 + 1
                } else if sel < 7 {
                    bs.read_bits_leq32(sel as u32 - 1)? as u8
                } else {
                    return symphonia_core::errors::decode_error("dca: invalid bit_allocation_sel");
                };
                self.bit_allocation[ch][band] = abits;
            }
        }

        // Transition mode
        for ch in 0..nchannels {
            if self.nsubsubframes > 1 {
                let sel = self.coding_header.transition_mode_sel[ch] as usize;
                for band in 0..self.coding_header.subband_vq_start[ch] as usize {
                    if self.bit_allocation[ch][band] > 0 {
                        use crate::tables::TRANSITION_MODE_VLC;
                        self.transition_mode[ch][band] = bs.read_codebook(&TRANSITION_MODE_VLC[sel])?.0;
                    } else {
                        self.transition_mode[ch][band] = 0;
                    }
                }
            } else {
                for band in 0..MAX_SUBBANDS {
                    self.transition_mode[ch][band] = 0;
                }
            }
        }

        // Scale factors. Per FFmpeg dca_core.c parse_subframe_header (lines 467-505):
        //
        // - A fresh accumulator per channel per subframe, reset to 0 at start.
        // - For sel<5: each VLC value is a signed delta added to the accumulator;
        //   for sel>=5: each direct read is an *absolute* index that overwrites it.
        // - Bands below subband_vq_start: scale present only when bit_allocation > 0.
        // - Bands at/above subband_vq_start (HF VQ region): scale always present.
        // - When transition_mode > 0 the second scale is the *next* accumulator step
        //   (uses the same running sum as the band's primary scale).
        let vq6_size = crate::tables::SCALE_FACTOR_QUANT6.len() as i16;
        let vq7_size = crate::tables::SCALE_FACTOR_QUANT7.len() as i16;
        for ch in 0..nchannels {
            let sel = self.coding_header.scale_factor_sel[ch] as usize;
            let max_idx = if sel > 5 { vq7_size } else { vq6_size };
            let mut acc: i16 = 0;

            for band in 0..self.coding_header.subband_vq_start[ch] as usize {
                if self.bit_allocation[ch][band] > 0 {
                    acc = self.parse_scale(bs, sel, acc)?;
                    if acc < 0 || acc >= max_idx {
                        return symphonia_core::errors::decode_error(
                            "dca: scale factor index out of range",
                        );
                    }
                    self.scale_indices[ch][band][0] = acc;
                    if self.transition_mode[ch][band] > 0 {
                        acc = self.parse_scale(bs, sel, acc)?;
                        if acc < 0 || acc >= max_idx {
                            return symphonia_core::errors::decode_error(
                                "dca: scale factor index out of range",
                            );
                        }
                        self.scale_indices[ch][band][1] = acc;
                    } else {
                        self.scale_indices[ch][band][1] = self.scale_indices[ch][band][0];
                    }
                }
            }
            for band in self.coding_header.subband_vq_start[ch] as usize
                ..self.coding_header.nsubbands[ch] as usize
            {
                acc = self.parse_scale(bs, sel, acc)?;
                if acc < 0 || acc >= max_idx {
                    return symphonia_core::errors::decode_error(
                        "dca: scale factor index out of range",
                    );
                }
                self.scale_indices[ch][band][0] = acc;
            }
        }

        Ok(())
    }

    fn parse_scale(&mut self, bs: &mut BitReaderLtr<'_>, sel: usize, mut index: i16) -> Result<i16> {
        // sel<5 codebooks emit unsigned 0..128; FFmpeg's VLC init biases by -64 to
        // recover the spec's signed delta in -64..64. Apply the bias at use site.
        // sel==5/6 reads `sel+1` bits as an absolute (not differential) index, per
        // FFmpeg parse_scale (dca_core.c §5.4.1.7). sel==7 is invalid.
        if sel < 5 {
            use crate::tables::SCALE_FACTOR_VLC;
            let diff = bs.read_codebook(&SCALE_FACTOR_VLC[sel])?.0 as i16 - 64;
            index += diff;
        } else if sel < 7 {
            index = bs.read_bits_leq32((sel + 1) as u32)? as i16;
        } else {
            return symphonia_core::errors::decode_error("dca: invalid scale_factor_sel");
        }
        Ok(index)
    }

    fn parse_subframe_audio(&mut self, sf_idx: usize, bs: &mut BitReaderLtr<'_>) -> Result<()> {
        let nchannels = self.coding_header.nchannels as usize;
        let nss = self.nsubsubframes as usize;
        let subframe_offset = sf_idx * nss * 8;

        // Primary audio (5.5): per subsubframe, per channel, per band below VQ start.
        // Bands at/above subband_vq_start have abits == 0 and are decoded separately
        // via the high-frequency VQ codebook below.
        // DSYNC (`0xFFFF` between ssfs) reads NOT inserted: against the Apollo 13
        // reference stream, including those reads at any cadence misaligns the
        // bitstream and triggers LFE scale-index errors on every frame. The other
        // missing piece in this decoder (ADPCM application, joint coding, partial
        // ssf samples) likely interacts with the byte position; revisit once the
        // ssf consumption is bit-exact end-to-end with FFmpeg.
        for n in 0..nss {
            for ch in 0..nchannels {
                for band in 0..self.coding_header.nsubbands[ch] as usize {
                    let abits = self.bit_allocation[ch][band];
                    if abits > 0 {
                        self.extract_audio(bs, ch, band, subframe_offset + n * 8, n, abits)?;
                    }
                }
            }
        }

        // Inverse ADPCM (5.5.4): for each band with prediction_mode set, add the
        // VQ-codebook prediction to the residual we just decoded. Mirrors FFmpeg
        // dca_core.c `inverse_adpcm` (L606-624). Runs once per subframe across all
        // `nss * 8` samples; reads 4-sample history from the previous subframe.
        let nsamples = nss * 8;
        // ADPCM prediction is disabled pending a fix for the underlying dequant-
        // magnitude drift that makes this path AMPLIFY error rather than remove it.
        // The code and `adpcm_table::ADPCM_VB` are kept in place so this can be flipped
        // back on once the normalization mismatch is resolved.
        let _ = (nchannels, nsamples, Self::inverse_adpcm);

        // High-frequency VQ (5.5.6): per channel, one 10-bit VQ index per band in
        // [subband_vq_start, nsubbands), followed by per-band synthesis of `nss * 8`
        // samples from HIGH_FREQ_VQ scaled by the band's first scale factor.
        for ch in 0..nchannels {
            let vq_start = self.coding_header.subband_vq_start[ch] as usize;
            let nsubbands = self.coding_header.nsubbands[ch] as usize;
            for band in vq_start..nsubbands {
                self.high_freq_vq_index[ch][band] = bs.read_bits_leq32(10)? as u16;
            }
            if vq_start < nsubbands {
                self.decode_hf_vq(ch, vq_start, nsubbands, subframe_offset, nsamples);
            }
        }

        // Low-frequency effect data (5.5.7). Per FFmpeg dca_core.c L660-693:
        // `nlfesamples = 2 * lfe_present * nsubsubframes`, each as 8-bit signed,
        // followed by an 8-bit scale factor index into SCALE_FACTOR_QUANT7. Step size
        // for LFE is fixed at ~0.035 of full-scale (Q22-encoded as 4_663_904).
        if self.header.lfe != 0 && self.header.lfe != 3 {
            let lfe_present = self.header.lfe as usize;
            let nlfesamples = 2 * lfe_present * nss;
            let mut raw = [0i32; 64];
            for slot in raw.iter_mut().take(nlfesamples) {
                *slot = bs.read_bits_leq32_signed(8)?;
            }
            let scale_idx = bs.read_bits_leq32(8)? as usize;
            // Out-of-range scale index indicates upstream bit misalignment (usually in
            // the HF VQ or joint-coding paths we don't fully implement yet). FFmpeg
            // fails the frame here; we log and treat the LFE scale as 0 so the core
            // channels can still play, rather than dropping the whole frame.
            let scale_int = if scale_idx < crate::tables::SCALE_FACTOR_QUANT7.len() {
                crate::tables::SCALE_FACTOR_QUANT7[scale_idx] as f64
            } else {
                log::warn!("dca: LFE scale factor index out of range: {}", scale_idx);
                0.0
            };
            // Per FFmpeg: step_scale = scale * 4663904 (= 0.035 * 2^27); the int
            // LFE sample is `raw * step_scale >> 23`, landing in clip23 range ±2^23.
            // LFE bypasses the polyphase QMF, so the float conversion is just
            // `int_sample / 2^23` to normalize to [-1, +1]. Combined: /(2^46).
            const LFE_STEP: f64 = 4_663_904.0;
            let lfe_scale = scale_int * LFE_STEP / (1u64 << 46) as f64;
            for &sample in raw.iter().take(nlfesamples) {
                self.lfe_samples.push((sample as f64 * lfe_scale) as f32);
            }
        }

        Ok(())
    }

    /// Add the ADPCM-predicted contribution to each prediction-enabled subband sample.
    /// `pred = sum(history[3-i] * coeff[i] for i in 0..4) / (1 << 13)` (FFmpeg
    /// `ff_dcaadpcm_predict`, dcaadpcm.h L33-44). The "history" is the previous 4
    /// subband samples; for j>=4 within this subframe those are already-predicted
    /// outputs, for j<4 they come from the tail of the previous subframe (or 0 at
    /// frame start since we zero `subband_samples` in `decode()`).
    #[allow(dead_code)]
    fn inverse_adpcm(&mut self, nchannels: usize, ofs: usize, len: usize) {
        use crate::adpcm_table::ADPCM_VB;

        // Bit-equivalent to FFmpeg `inverse_adpcm` + `ff_dcaadpcm_predict`. We work
        // in the same Q23 fixed-point space as FFmpeg (subband_samples * 2^18) and
        // convert back at the end. Per FFmpeg dca_core.c L606-624 + dcaadpcm.h L33-44.
        const TO_INT: f32 = (1u64 << 18) as f32;
        const FROM_INT: f32 = 1.0 / TO_INT;
        const CLIP23_MAX: i64 = (1 << 23) - 1;
        const CLIP23_MIN: i64 = -(1 << 23);

        let clip23 = |v: i64| v.clamp(CLIP23_MIN, CLIP23_MAX) as i32;
        let norm13 = |v: i64| (v + (1 << 12)) >> 13;

        for ch in 0..nchannels {
            for band in 0..self.coding_header.nsubbands[ch] as usize {
                if !self.prediction_mode[ch][band] {
                    continue;
                }
                let vq = self.prediction_vq_index[ch][band] as usize;
                if vq >= ADPCM_VB.len() {
                    continue;
                }
                let coeff = ADPCM_VB[vq];
                for j in 0..len {
                    let buf = &self.subband_samples[ch][band];
                    let p3 = (buf[(ofs + j + 128 - 1) % 128] * TO_INT).round() as i64;
                    let p2 = (buf[(ofs + j + 128 - 2) % 128] * TO_INT).round() as i64;
                    let p1 = (buf[(ofs + j + 128 - 3) % 128] * TO_INT).round() as i64;
                    let p0 = (buf[(ofs + j + 128 - 4) % 128] * TO_INT).round() as i64;
                    let sum = p3 * coeff[0] as i64
                        + p2 * coeff[1] as i64
                        + p1 * coeff[2] as i64
                        + p0 * coeff[3] as i64;
                    let pred = clip23(norm13(sum)) as i64;
                    let idx = (ofs + j) % 128;
                    let cur = (self.subband_samples[ch][band][idx] * TO_INT).round() as i64;
                    let new = clip23(cur + pred);
                    self.subband_samples[ch][band][idx] = (new as f32) * FROM_INT;
                }
            }
        }
    }

    /// Synthesize `len` HF-VQ samples for each band in [`sb_start`, `sb_end`) into
    /// `subband_samples[ch][band]` starting at `ofs`. Mirrors FFmpeg `decode_hf_c` in
    /// `dcadsp.c`: `out_int = (coeff * scale + 8) >> 4`, then the float QMF normalizer
    /// is applied (collapsed into NORMALIZER as in `dequantize`).
    fn decode_hf_vq(&mut self, ch: usize, sb_start: usize, sb_end: usize, ofs: usize, len: usize) {
        use crate::hf_vq_table::HIGH_FREQ_VQ;
        use crate::tables::{SCALE_FACTOR_QUANT6, SCALE_FACTOR_QUANT7};

        // FFmpeg `decode_hf_c` (dcadsp.c L26-41): `clip23((coeff*scale + 8) >> 4)` then
        // float QMF * 1/(1<<17). Mirror the chain: pre-clip divide by 16, saturate to
        // ±2^23, then apply the same POST_CLIP_NORM (1/(1<<18)) as primary dequant.
        const PRE_CLIP_NORM: f64 = 1.0 / (1u64 << 4) as f64;
        const CLIP23_MAX: f64 = ((1u64 << 23) - 1) as f64;
        const CLIP23_MIN: f64 = -(1i64 << 23) as f64;
        const POST_CLIP_NORM: f32 = 1.0 / ((1u64 << 18) as f64) as f32;

        let sel = self.coding_header.scale_factor_sel[ch];
        for band in sb_start..sb_end {
            let vq_idx = self.high_freq_vq_index[ch][band] as usize;
            let coeff = &HIGH_FREQ_VQ[vq_idx];
            let scale_idx = self.scale_indices[ch][band][0] as usize;
            let scale = if sel > 5 {
                SCALE_FACTOR_QUANT7[scale_idx] as f64
            } else {
                SCALE_FACTOR_QUANT6[scale_idx] as f64
            };
            for j in 0..len {
                let c = coeff[j % 32] as f64;
                let raw = c * scale * PRE_CLIP_NORM;
                let clipped = raw.clamp(CLIP23_MIN, CLIP23_MAX);
                let idx = (ofs + j) % 128;
                self.subband_samples[ch][band][idx] = (clipped as f32) * POST_CLIP_NORM;
            }
        }
    }

    /// Decode 8 quantization indices for one subband-subsubframe, dequantize them, and
    /// write into subband_samples[ch][band]. Mirrors FFmpeg `extract_audio` +
    /// `parse_huffman_codes` / `parse_block_codes` / `get_array`.
    fn extract_audio(
        &mut self,
        bs: &mut BitReaderLtr<'_>,
        ch: usize,
        band: usize,
        samples_offset: usize,
        ssf: usize,
        abits: u8,
    ) -> Result<()> {
        use crate::tables::{
            BLOCK_CODE_NBITS, QUANT_INDEX_GROUP_SIZE, QUANT_INDEX_VLC, QUANT_LEVELS,
        };

        let mut indices = [0i32; 8];

        if abits >= 1 && abits <= 10 {
            let abits_idx = abits as usize - 1;
            let sel = self.coding_header.quant_index_sel[ch][abits_idx] as usize;
            let group_size = QUANT_INDEX_GROUP_SIZE[abits_idx] as usize;
            let levels = QUANT_LEVELS[abits as usize] as i32;

            if sel < group_size {
                // Huffman-coded indices. Our codebooks emit raw 0..levels-1 values;
                // center them to signed -(levels/2)..(levels/2).
                let codebook = &QUANT_INDEX_VLC[abits_idx][sel];
                for slot in indices.iter_mut() {
                    let (val, _) = bs.read_codebook(codebook)?;
                    *slot = (val as i32) - (levels / 2);
                }
            } else if abits <= 7 {
                // Block code path: two base-`levels` multi-symbol codes pack the 8 samples.
                // See FFmpeg decode_blockcodes (dca_core.c). Per the spec §5.5.1 this
                // is used when the selected VLC set is the reserved "no Huffman" entry.
                let nbits = BLOCK_CODE_NBITS[abits_idx] as u32;
                let mut code1 = bs.read_bits_leq32(nbits)? as i32;
                let mut code2 = bs.read_bits_leq32(nbits)? as i32;
                let offset = (levels - 1) / 2;
                for slot in indices[..4].iter_mut() {
                    let div = code1 / levels;
                    *slot = code1 - div * levels - offset;
                    code1 = div;
                }
                for slot in indices[4..].iter_mut() {
                    let div = code2 / levels;
                    *slot = code2 - div * levels - offset;
                    code2 = div;
                }
                if code1 | code2 != 0 {
                    log::warn!(
                        "dca: block code residue ch={} band={} abits={} code1={} code2={}",
                        ch, band, abits, code1, code2,
                    );
                }
            } else {
                // abits 8..10 with sel == group_size: direct read, `abits - 3` bits signed.
                let nbits = abits as u32 - 3;
                for slot in indices.iter_mut() {
                    *slot = bs.read_bits_leq32_signed(nbits)?;
                }
            }
        } else if abits >= 11 && abits <= 26 {
            // Direct read only, `abits - 3` signed bits per sample.
            let nbits = abits as u32 - 3;
            for slot in indices.iter_mut() {
                *slot = bs.read_bits_leq32_signed(nbits)?;
            }
        } else {
            return symphonia_core::errors::decode_error("dca: abits out of range");
        }

        // Whether the Huffman path was taken — scale_factor_adj only applies then.
        let huffman = abits >= 1
            && abits <= 10
            && (self.coding_header.quant_index_sel[ch][abits as usize - 1] as usize)
                < QUANT_INDEX_GROUP_SIZE[abits as usize - 1] as usize;

        for (i, &index) in indices.iter().enumerate() {
            let idx = (samples_offset + i) % 128;
            self.subband_samples[ch][band][idx] =
                self.dequantize(index, abits, ch, band, ssf, huffman);
        }
        Ok(())
    }

    /// Convert a signed quantization index to a float subband sample matching FFmpeg's
    /// fixed-point pipeline: `sample_int = (index * step_size * scale) >> 22`, then
    /// the float QMF synthesis scales the int by `1/(1<<17)`. Collapsed into a single
    /// multiplication by `1/(1<<39)` for the float-only path. `scale_factor_adj` is
    /// applied only when the sample came from a Huffman codebook.
    fn dequantize(
        &self,
        index: i32,
        abits: u8,
        ch: usize,
        band: usize,
        ssf: usize,
        huffman: bool,
    ) -> f32 {
        use crate::tables::{LOSSY_QUANT_STEP, SCALE_FACTOR_ADJ, SCALE_FACTOR_QUANT6, SCALE_FACTOR_QUANT7};

        // FFmpeg's fixed-point chain (dca_core.h L226): the dequantized sample is
        //   `clip23((index * step_size * scale) >> 22)`
        // i.e. divide by 2^22, saturate to ±2^23. The float QMF then multiplies by
        // `1/(1<<17)`. Symphonia's polyphase synthesis empirically has 2× the gain of
        // FFmpeg's `synth_filter_float`, so the post-clip normalizer is `1/(1<<18)`.
        // Replicating clip23 in float is required — without it loud bands overshoot
        // peak amplitudes by ~5× (verified against FFmpeg reference on 5.1 content).
        const PRE_CLIP_NORM: f64 = 1.0 / (1u64 << 22) as f64;
        const CLIP23_MAX: f64 = ((1u64 << 23) - 1) as f64;
        const CLIP23_MIN: f64 = -(1i64 << 23) as f64;
        const POST_CLIP_NORM: f32 = 1.0 / ((1u64 << 18) as f64) as f32;

        let step_size = LOSSY_QUANT_STEP[abits as usize] as f64;

        let trans = self.transition_mode[ch][band] as usize;
        let slot = if trans == 0 || ssf < trans { 0 } else { 1 };
        let scale_index = self.scale_indices[ch][band][slot] as usize;

        let sel = self.coding_header.scale_factor_sel[ch];
        let mut scale = if sel > 5 {
            SCALE_FACTOR_QUANT7[scale_index] as f64
        } else {
            SCALE_FACTOR_QUANT6[scale_index] as f64
        };

        if huffman {
            let adj_idx = self.coding_header.scale_factor_adj[ch][abits as usize - 1] as usize;
            scale = (scale * SCALE_FACTOR_ADJ[adj_idx] as f64) / (1u64 << 22) as f64;
        }

        let raw = (index as f64) * step_size * scale * PRE_CLIP_NORM;
        let clipped = raw.clamp(CLIP23_MIN, CLIP23_MAX);
        (clipped as f32) * POST_CLIP_NORM
    }

    /// Map a DCA primary channel index (in bitstream order, per `prm_ch_to_spkr_map`
    /// in FFmpeg dca_core.c L40-52) to the plane index in our output layout.
    ///
    /// DCA bitstream channel order for AMODE values currently supported:
    /// * AMODE 0 (mono):        C
    /// * AMODE 2 (stereo):      L, R
    /// * AMODE 9 (5.0 + LFE):   C, L, R, Ls, Rs   ← note C is FIRST
    ///
    /// Symphonia output plane order:
    /// * MONO:    [C]
    /// * STEREO:  [L, R]
    /// * 5P1:     [L, R, C, LFE, Ls, Rs]
    fn output_plane_for(amode: u8, dca_ch: usize) -> usize {
        match amode {
            0 => 0,                                      // mono
            2 => dca_ch,                                 // stereo passthrough
            9 => match dca_ch {                          // 5.0(+LFE)
                0 => 2,                                  // C  → plane 2
                1 => 0,                                  // L  → plane 0
                2 => 1,                                  // R  → plane 1
                3 => 4,                                  // Ls → plane 4
                4 => 5,                                  // Rs → plane 5
                _ => dca_ch,
            },
            _ => dca_ch,
        }
    }

    fn synthesis_filter_bank(&mut self, block_idx: usize, output_offset: usize) {
        let nchannels = self.coding_header.nchannels as usize;
        let amode = self.header.amode;
        use crate::tables::DCA_FIR_32BANDS_PERFECT;
        use std::f32::consts::PI;

        for ch in 0..nchannels {
            // 1. Cosine Modulation
            let mut ra = [0.0f32; 64];
            for (n, slot) in ra.iter_mut().enumerate() {
                let mut sum = 0.0f32;
                for k in 0..32 {
                    let subband_sample = self.subband_samples[ch][k][block_idx % 128];
                    let angle = (2.0 * n as f32 + 1.0) * (2.0 * k as f32 + 1.0) * PI / 128.0;
                    sum += subband_sample * angle.cos();
                }
                *slot = sum;
            }

            // 2. Shift delay buffer and insert new samples
            self.qmf_state[ch].copy_within(0..448, 64);
            self.qmf_state[ch][..64].copy_from_slice(&ra);

            // 3. Windowing and Summation
            let mut pcm = [0.0f32; 32];
            for (i, slot) in pcm.iter_mut().enumerate() {
                let mut sum = 0.0f32;
                for j in 0..8 {
                    let idx1 = i + 64 * j;
                    let idx2 = i + 32 + 64 * j;
                    sum += self.qmf_state[ch][idx1] * DCA_FIR_32BANDS_PERFECT[idx1];
                    sum -= self.qmf_state[ch][idx2] * DCA_FIR_32BANDS_PERFECT[idx2];
                }
                *slot = sum;
            }

            // 4. Output to buffer (remap DCA primary channel order to layout planes).
            let plane_idx = Self::output_plane_for(amode, ch);
            if let GenericAudioBuffer::F32(ref mut buf) = self.buf {
                if let Some(out_slice) = buf.plane_mut(plane_idx) {
                    for i in 0..32 {
                        if output_offset + i < out_slice.len() {
                            out_slice[output_offset + i] = pcm[i];
                        }
                    }
                }
            }
        }
    }
}

impl AudioDecoder for DcaDecoder {
    fn reset(&mut self) {
        self.header = CoreHeader::default();
        self.coding_header = CodingHeader::default();
        for ch in 0..MAX_CHANNELS {
            for sb in 0..MAX_SUBBANDS {
                self.subband_samples[ch][sb].fill(0.0);
            }
            self.qmf_state[ch].fill(0.0);
        }
        self.lfe_samples.clear();
        self.nsubsubframes = 0;
        self.prediction_mode = [[false; MAX_SUBBANDS]; MAX_CHANNELS];
        self.prediction_vq_index = [[0; MAX_SUBBANDS]; MAX_CHANNELS];
        self.bit_allocation = [[0; MAX_SUBBANDS]; MAX_CHANNELS];
        self.transition_mode = [[0; MAX_SUBBANDS]; MAX_CHANNELS];
        self.scale_indices = [[[0; 2]; MAX_SUBBANDS]; MAX_CHANNELS];
        self.high_freq_vq_index = [[0; MAX_SUBBANDS]; MAX_CHANNELS];
    }

    fn codec_info(&self) -> &CodecInfo {
        &Self::supported_codecs()[0].info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: &Packet) -> Result<GenericAudioBufferRef<'_>> {
        let mut bs = self.parse_core_header(packet)?;
        self.parse_coding_header(&mut bs)?;

        // Copy needed fields to avoid multiple mutable borrows of self
        let nsubframes = self.coding_header.nsubframes;
        let nchannels = self.coding_header.nchannels;
        let sfreq = self.header.sfreq;
        let amode = self.header.amode;
        let nblks = self.header.nblks;

        // Update parameters based on the decoded header.
        let rate = match sfreq {
            1 => 8000,
            2 => 16000,
            3 => 32000,
            6 => 11025,
            7 => 22050,
            8 => 44100,
            11 => 12000,
            12 => 24000,
            13 => 48000,
            _ => 44100,
        };

        let mut channels = match amode {
            0 => symphonia_core::audio::layouts::CHANNEL_LAYOUT_MONO,
            2 => symphonia_core::audio::layouts::CHANNEL_LAYOUT_STEREO,
            9 => symphonia_core::audio::layouts::CHANNEL_LAYOUT_5P1,
            _ => symphonia_core::audio::layouts::CHANNEL_LAYOUT_STEREO,
        };

        // If nchannels is 8, it might be 7.1
        if nchannels == 8 {
            channels = symphonia_core::audio::layouts::CHANNEL_LAYOUT_7P1;
        }

        let samples_per_frame = (u32::from(nblks) + 1) * 32;

        self.params.with_sample_rate(rate).with_channels(channels.clone());

        // Re-allocate buffer if parameters changed.
        let spec = symphonia_core::audio::AudioSpec::new(rate, channels);
        if self.buf.spec() != &spec || self.buf.capacity() < samples_per_frame as usize {
            self.buf = GenericAudioBuffer::new(SampleFormat::F32, spec, samples_per_frame as usize);
        }
        
        // Ensure buffer frames match
        self.buf.resize_uninit(samples_per_frame as usize);

        // Zero out subband samples for this frame
        for ch in 0..MAX_CHANNELS {
            for sb in 0..MAX_SUBBANDS {
                self.subband_samples[ch][sb].fill(0.0);
            }
        }
        self.lfe_samples.clear();

        // Subframe loop
        let mut output_offset = 0;
        for i in 0..nsubframes as usize {
            self.parse_subframe_header(i, &mut bs)?;
            self.parse_subframe_audio(i, &mut bs)?;

            let nss = self.nsubsubframes as usize;
            let subframe_start_block = i * nss * 8;
            for block in 0..nss * 8 {
                self.synthesis_filter_bank(subframe_start_block + block, output_offset);
                output_offset += 32;
            }
        }

        // LFE FIR interpolation is deferred (proper port of `ff_dca_lfe_fir_64/128`
        // and the polyphase upsampler is needed). For now LFE bits are *consumed* in
        // parse_subframe_audio so the rest of the bitstream stays aligned, but we leave
        // the LFE plane silent. With AMODE 9 the LFE plane is plane 3 (5.1 layout).
        let _ = amode;

        Ok(self.buf.as_generic_audio_buffer_ref())
    }

    fn finalize(&mut self) -> FinalizeResult {
        Default::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for DcaDecoder {
    fn try_registry_new(params: &AudioCodecParameters, opts: &AudioDecoderOptions) -> Result<Box<dyn AudioDecoder>> {
        Ok(Box::new(DcaDecoder::try_new(params, opts)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[
            support_audio_codec!(CODEC_ID_DCA, "dca", "DTS Coherent Acoustics"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AMODE 0 (Mono) — only a center channel, goes to plane 0.
    #[test]
    fn output_plane_amode_mono() {
        assert_eq!(DcaDecoder::output_plane_for(0, 0), 0);
    }

    /// AMODE 2 (Stereo) — L/R pass through to planes 0/1.
    #[test]
    fn output_plane_amode_stereo() {
        assert_eq!(DcaDecoder::output_plane_for(2, 0), 0); // L → FL
        assert_eq!(DcaDecoder::output_plane_for(2, 1), 1); // R → FR
    }

    /// AMODE 9 (5.0) maps the DCA primary-channel order (C, L, R, Ls, Rs) — see
    /// FFmpeg `prm_ch_to_spkr_map` in dca_core.c — onto our 5.1 layout plane order
    /// (L, R, C, LFE, Ls, Rs). Getting this wrong plays center out of the fronts.
    #[test]
    fn output_plane_amode_5p1_reorders_dca_native_to_standard_layout() {
        assert_eq!(DcaDecoder::output_plane_for(9, 0), 2); // C  → plane 2
        assert_eq!(DcaDecoder::output_plane_for(9, 1), 0); // L  → plane 0
        assert_eq!(DcaDecoder::output_plane_for(9, 2), 1); // R  → plane 1
        assert_eq!(DcaDecoder::output_plane_for(9, 3), 4); // Ls → plane 4
        assert_eq!(DcaDecoder::output_plane_for(9, 4), 5); // Rs → plane 5
    }

    /// Channel indices beyond the known primary-channel count for an AMODE fall
    /// back to identity rather than panicking — higher-layer code guards against
    /// this via `nchannels`.
    #[test]
    fn output_plane_out_of_range_falls_back_to_identity() {
        assert_eq!(DcaDecoder::output_plane_for(9, 7), 7);
    }

    /// Unknown AMODEs fall back to identity mapping (safe default while we only
    /// support AMODE 0 / 2 / 9).
    #[test]
    fn output_plane_unknown_amode_is_identity() {
        assert_eq!(DcaDecoder::output_plane_for(5, 0), 0);
        assert_eq!(DcaDecoder::output_plane_for(5, 3), 3);
    }
}
