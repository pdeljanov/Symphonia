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
use symphonia_core::audio::{GenericAudioBuffer, GenericAudioBufferRef, AsGenericAudioBufferRef};
use symphonia_core::audio::sample::SampleFormat;
use symphonia_core::codecs::audio::well_known::CODEC_ID_DCA;
use symphonia_core::errors::{Result, unsupported_error};
use symphonia_core::packet::Packet;

use symphonia_core::io::{ReadBytes, ReadBitsLtr, BitReaderLtr};

/// DTS Coherent Acoustics (DCA) core frame header.
#[allow(dead_code)]
struct CoreHeader {
    nblks: u8,
    fsize: u16,
    amode: u8,
    sfreq: u8,
    rate: u8,
    fixed_bit: u8,
    dynf: u8,
    timef: u8,
    auxf: u8,
    hdcd: u8,
    ext_audio_id: u8,
    ext_audio: u8,
    aspc: u8,
    lfe: u8,
    nhist: u8,
    cpf: u8,
    quant: u8,
    vlc: u8,
    nsubframes: u8,
    nchannels: u8,
}

/// DTS Coherent Acoustics (DCA) decoder.
pub struct DcaDecoder {
    params: AudioCodecParameters,
    buf: GenericAudioBuffer,
    header: Option<CoreHeader>,
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

        Ok(DcaDecoder { params: params.clone(), buf, header: None })
    }

    fn parse_core_header(&self, packet: &Packet) -> Result<CoreHeader> {
        let mut reader = packet.as_buf_reader();
        
        // Sync word was already checked by the demuxer, but we might be in a different container.
        // For simplicity, we assume the packet starts with the data AFTER the sync word if it was stripped,
        // or we check for it.
        let sync = reader.read_u32()?;
        if sync != 0x7FFE8001 {
            // If the sync word is not there, we might need to search for it or assume it was stripped.
            // For now, let's assume it's there.
            return unsupported_error("dca: sync word not found in packet");
        }

        let mut buf = [0u8; 8];
        reader.read_buf_exact(&mut buf)?;

        let mut bs = BitReaderLtr::new(&buf);

        // Frame Type: 1 bit
        let _ftype = bs.read_bits_leq32(1)?;
        // Deficit Samples: 5 bits
        let _deficit = bs.read_bits_leq32(5)?;
        // CPF: 1 bit
        let cpf = bs.read_bits_leq32(1)? as u8;
        // NBLKS: 7 bits
        let nblks = bs.read_bits_leq32(7)? as u8;
        // FSIZE: 14 bits
        let fsize = bs.read_bits_leq32(14)? as u16 + 1;
        // AMODE: 6 bits
        let amode = bs.read_bits_leq32(6)? as u8;
        // SFREQ: 4 bits
        let sfreq = bs.read_bits_leq32(4)? as u8;
        // RATE: 5 bits
        let rate = bs.read_bits_leq32(5)? as u8;
        
        // Next byte
        let mut buf2 = [0u8; 4];
        reader.read_buf_exact(&mut buf2)?;
        let mut bs2 = BitReaderLtr::new(&buf2);

        // Fixed bit: 1 bit
        let fixed_bit = bs2.read_bits_leq32(1)? as u8;
        // DYNF: 2 bits
        let dynf = bs2.read_bits_leq32(2)? as u8;
        // TIMEF: 1 bit
        let timef = bs2.read_bits_leq32(1)? as u8;
        // AUXF: 1 bit
        let auxf = bs2.read_bits_leq32(1)? as u8;
        // HDCD: 1 bit
        let hdcd = bs2.read_bits_leq32(1)? as u8;
        // EXT_AUDIO_ID: 3 bits
        let ext_audio_id = bs2.read_bits_leq32(3)? as u8;
        // EXT_AUDIO: 1 bit
        let ext_audio = bs2.read_bits_leq32(1)? as u8;
        // ASPC: 1 bit
        let aspc = bs2.read_bits_leq32(1)? as u8;
        // LFE: 2 bits
        let lfe = bs2.read_bits_leq32(2)? as u8;
        // NHIST: 1 bit
        let nhist = bs2.read_bits_leq32(1)? as u8;

        // More fields...
        
        Ok(CoreHeader {
            nblks,
            fsize,
            amode,
            sfreq,
            rate,
            fixed_bit,
            dynf,
            timef,
            auxf,
            hdcd,
            ext_audio_id,
            ext_audio,
            aspc,
            lfe,
            nhist,
            cpf,
            quant: 0,
            vlc: 0,
            nsubframes: 0,
            nchannels: 0,
        })
    }
}

impl AudioDecoder for DcaDecoder {
    fn reset(&mut self) {
        // Reset decoder state.
    }

    fn codec_info(&self) -> &CodecInfo {
        &Self::supported_codecs()[0].info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode(&mut self, packet: &Packet) -> Result<GenericAudioBufferRef<'_>> {
        let header = self.parse_core_header(packet)?;
        self.header = Some(header);

        // TODO: Implement actual subframe and sample decoding.
        // For now, return an error to satisfy the test expectation of it being incomplete.
        unsupported_error("dca: decoder not fully implemented")
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
