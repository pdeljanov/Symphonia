use std::convert::TryFrom;
use crate::entropy::{self, RangeDecoder};
use crate::packet::FramePacket;
use crate::silk::error::Error;
use crate::toc::{Bandwidth, FrameSize};
use crate::silk::constant;

use symphonia_core::audio::{AsAudioBufferRef, AudioBuffer, AudioBufferRef, Channels, Signal, SignalSpec};
use symphonia_core::codecs::CodecParameters;
use symphonia_core::errors::Result;
use symphonia_core::formats::Packet;
use symphonia_core::io::{BitReaderLtr, FiniteBitStream, ReadBitsLtr};

pub struct Decoder {
    params: CodecParameters,
    buffer: AudioBuffer<f32>,
    channels: Channels,
    state: State,
}

impl Decoder {
    pub fn try_new(params: CodecParameters) -> Result<Self> {
        let params = params.to_owned();
        let state = State::default();

        let sample_rate = params.sample_rate.ok_or(Error::UnsupportedConfig)?;
        let channels = params.channels.ok_or(Error::UnsupportedConfig)?;
        let signal_spec = SignalSpec::new(sample_rate, channels);
        let buffer = AudioBuffer::new(sample_rate as u64, signal_spec);

        return Ok(Self { params, channels, buffer, state });
    }

    pub fn reset(&mut self) {
        self.state.reset();
        self.buffer.clear();
    }


    pub fn codec_params(&self) -> &CodecParameters {
        return &self.params;
    }

    pub fn decode(&mut self, packet: &Packet) -> Result<AudioBufferRef<'_>> {
        let frame_packet = FramePacket::new(&packet.data)?;

        let params = frame_packet.toc.params().map_err(|_| Error::UnsupportedConfig)?;

        if self.state.frame_size != params.frame_size || self.state.bandwidth != params.bandwidth || self.state.channels != self.channels {
            self.state = State::try_new(self.channels, params.frame_size, params.bandwidth)?;
        }

        for frame_data in frame_packet.frames.iter() {
            let frame = self.decode_frame(frame_data)?;
            self.synthesize_frame(&frame)?;
        }

        if self.state.lbrr_flag {
            let lbrr_frames_data = self.extract_lbrr_frames(&packet.data)?;
            for lbrr_data in lbrr_frames_data.iter() {
                let lbrr_frame = self.decode_frame(lbrr_data)?;
                self.synthesize_frame(&lbrr_frame)?;
            }
        }

        return Ok(self.buffer.as_audio_buffer_ref());
    }

    fn extract_lbrr_frames<'a>(&self, packet_data: &'a [u8]) -> Result<Vec<&'a [u8]>> {
        let mut lbrr_frames = Vec::new();
        let mut reader = BitReaderLtr::new(packet_data);
        let mut offset = 0;

        const MAX_LBRR_FRAME_SIZE: usize = 4096;
        while reader.bits_left() >= 8 {
            let frame_length = self.read_lbrr_frame_length(&mut reader)?;

            if frame_length > MAX_LBRR_FRAME_SIZE {
                return Err(Error::FrameLengthExceedsMaximum.into());
            }

            if frame_length as u64 * 8 > reader.bits_left() {
                return Err(Error::InvalidLBRRFrame.into());
            }

            let frame_start = offset;
            reader.ignore_bits(frame_length as u32 * 8)?;
            offset += frame_length;

            if offset > packet_data.len() {
                return Err(Error::FrameLengthExceedsDataSize.into());
            }

            lbrr_frames.push(&packet_data[frame_start..offset]);
        }

        return Ok(lbrr_frames);
    }


    fn read_lbrr_frame_length(&self, reader: &mut BitReaderLtr) -> Result<usize> {
        let mut length = 0;
        loop {
            let byte = reader.read_bits_leq32(8)? as u8;
            length += byte as usize;
            if byte != 0xFF {
                break;
            }
        }

        return Ok(length);
    }

    fn decode_frame(&mut self, data: &[u8]) -> Result<Frame> {
        let sample_count = State::calculate_frame_length(self.state.sample_rate, self.state.frame_size)?;
        let num_subframes = SubframeSize::from(self.state.frame_size);
        let mut frame = Frame::new(sample_count, num_subframes);

        let reader = BitReaderLtr::new(data);
        let mut range_decoder = entropy::Decoder::new(reader)?;

        let (vad_flag, lbrr_flag) = self.decode_header_bits(&mut range_decoder)?;
        frame.vad_flag = vad_flag;
        frame.lbrr_flag = lbrr_flag;

        frame.frame_type = self.decode_frame_type(&mut range_decoder, vad_flag)?;

        self.decode_gains(&mut range_decoder, &mut frame)?;

        self.decode_lsf(&mut range_decoder, &mut frame)?;

        if self.state.frame_size == FrameSize::Ms20 {
            frame.lsf_interpolation_index = Some(range_decoder.decode_symbol_with_icdf(&constant::ICDF_NORMALIZED_LSF_INTERPOLATION_INDEX)?);
        }

        if frame.frame_type.signal_type == SignalType::Voiced {
            self.decode_ltp(&mut range_decoder, &mut frame)?;
        }

        self.decode_excitation(&mut range_decoder, &mut frame)?;

       return Ok(frame)
    }

    fn decode_header_bits<R: RangeDecoder>(&self, decoder: &mut R) -> Result<(bool, bool)> {
        let vad_flag = decoder.decode_symbol_logp(1)? == 1;
        let lbrr_flag = decoder.decode_symbol_logp(1)? == 1;

        return Ok((vad_flag, lbrr_flag));
    }

    fn decode_frame_type<R: RangeDecoder>(&mut self, decoder: &mut R, vad_flag: bool) -> Result<FrameType> {
        let icdf: &[u32] = if vad_flag { &constant::ICDF_FRAME_TYPE_VAD_ACTIVE } else { &constant::ICDF_FRAME_TYPE_VAD_INACTIVE };
        let frame_type_symbol = decoder.decode_symbol_with_icdf(icdf)?;

        let (signal_type, quantization_offset_type) = match (vad_flag, frame_type_symbol) {
            (false, 0) => (SignalType::Inactive, QuantizationOffsetType::Low),
            (false, 1) => (SignalType::Inactive, QuantizationOffsetType::High),
            (true, 0) => (SignalType::Unvoiced, QuantizationOffsetType::Low),
            (true, 1) => (SignalType::Unvoiced, QuantizationOffsetType::High),
            (true, 2) => (SignalType::Voiced, QuantizationOffsetType::Low),
            (true, 3) => (SignalType::Voiced, QuantizationOffsetType::High),
            _ => return Err(Error::InvalidFrameType.into()),
        };

        return Ok(FrameType::new(signal_type, quantization_offset_type));
    }


    fn decode_lsf<R: RangeDecoder>(&self, decoder: &mut R, frame: &mut Frame) -> Result<()> {
        for subframe in frame.subframes.iter_mut() {
            let i1 = self.decode_lsf_stage1(decoder, frame.vad_flag, frame.frame_type.signal_type)?;
            let (d_lpc, res_q10) = self.decode_lsf_stage2(decoder, i1)?;
            let nlsf_q15 = self.reconstruct_nlsf(d_lpc, &res_q10, i1)?;
            let stabilized_nlsf_q15 = self.stabilize_nlsf(&nlsf_q15)?;
            subframe.nlsf_q15 = stabilized_nlsf_q15;
        }

        if self.state.frame_size == FrameSize::Ms20 {
            let interpolation_index = decoder.decode_symbol_with_icdf(&constant::ICDF_NORMALIZED_LSF_INTERPOLATION_INDEX)?;
            frame.lsf_interpolation_index = Some(interpolation_index);
        }

        return Ok(());
    }

    fn decode_lsf_stage1(&self, decoder: &mut impl RangeDecoder, vad_flag: bool, signal_type: SignalType) -> Result<u32> {
        let icdf = match (vad_flag, signal_type) {
            (false, _) => &constant::ICDF_NORMALIZED_LSF_STAGE_ONE_INDEX_NARROWBAND_OR_MEDIUMBAND_UNVOICED,
            (true, SignalType::Voiced) => &constant::ICDF_NORMALIZED_LSF_STAGE_ONE_INDEX_NARROWBAND_OR_MEDIUMBAND_VOICED,
            (true, _) => &constant::ICDF_NORMALIZED_LSF_STAGE_ONE_INDEX_WIDEBAND_UNVOICED,
        };

        return decoder.decode_symbol_with_icdf(icdf);
    }

    fn decode_lsf_stage2<R: RangeDecoder>(&self, decoder: &mut R, i1: u32) -> Result<(usize, Vec<i16>)> {
        let d_lpc = self.state.lpc_order;
        let mut res_q10 = vec![0i16; d_lpc];

        for res_q10 in res_q10.iter_mut() {
            let icdf = &constant::ICDF_NORMALIZED_LSF_STAGE_TWO_INDEX[i1 as usize];
            let symbol = decoder.decode_symbol_with_icdf(icdf)?;
            *res_q10 = (symbol as i16) - 4;

            if *res_q10 == -4 || *res_q10 == 4 {
                let extension = decoder.decode_symbol_with_icdf(&constant::ICDF_NORMALIZED_LSF_STAGE_TWO_INDEX_EXTENSION)?;
                *res_q10 += if *res_q10 < 0 { -(extension as i16) } else { extension as i16 };
            }
        }

        return Ok((d_lpc, res_q10));
    }

    fn reconstruct_nlsf(&self, d_lpc: usize, res_q10: &[i16], i1: u32) -> Result<Vec<i16>> {
        let mut nlsf_q15 = vec![0i16; d_lpc];
        let cb1_q8 = match self.state.bandwidth {
            Bandwidth::WideBand => &constant::CODEBOOK_NORMALIZED_LSF_STAGE_ONE_WIDEBAND,
            _ => &constant::CODEBOOK_NORMALIZED_LSF_STAGE_ONE_NARROWBAND_OR_MEDIUMBAND,
        };

        for k in 0..d_lpc {
            let cb_value = cb1_q8[i1 as usize][k] as i32;
            let res_value = res_q10[k] as i32;
            nlsf_q15[k] = ((cb_value << 7) + (res_value << 14) / 10) as i16;
        }

        return Ok(nlsf_q15);
    }

    fn stabilize_nlsf(&self, nlsf_q15: &[i16]) -> Result<Vec<i16>> {
        let mut stable_nlsf = nlsf_q15.to_vec();
        let min_delta: &[i32] = match self.state.bandwidth {
            Bandwidth::WideBand => &constant::MINIMUM_SPACING_NORMALIZED_LSF_WB,
            _ => &constant::MINIMUM_SPACING_NORMALIZED_LSF_NARROWBAND_MEDIUMBAND,
        };

        const MAX_STABILIZATION_ITERATIONS: usize = 20;
        for _ in 0..MAX_STABILIZATION_ITERATIONS {
            let mut min_diff = i32::MAX;
            let mut min_diff_index = 0;

            for i in 1..stable_nlsf.len() {
                let diff = stable_nlsf[i] as i32 - stable_nlsf[i - 1] as i32 - min_delta[i];
                if diff < min_diff {
                    min_diff = diff;
                    min_diff_index = i;
                }
            }

            if min_diff >= 0 {
                break;
            }

            let center = (stable_nlsf[min_diff_index - 1] as i32 + stable_nlsf[min_diff_index] as i32) / 2;
            stable_nlsf[min_diff_index - 1] = (center - min_delta[min_diff_index] / 2) as i16;
            stable_nlsf[min_diff_index] = (stable_nlsf[min_diff_index - 1] as i32 + min_delta[min_diff_index]) as i16;
        }

        return Ok(stable_nlsf);
    }

    fn decode_pitch_lags<R: RangeDecoder>(&self, decoder: &mut R) -> Result<Vec<u16>> {
        let num_subframes = SubframeSize::from(self.state.frame_size);

        let mut pitch_lags = vec![0u16; num_subframes];

        let primary_lag = self.decode_primary_lag(decoder)?;

        for i in 0..num_subframes {
            let k = decoder.decode_symbol_with_icdf(&constant::ICDF_SUBFRAME_PITCH_CONTOUR_NARROWBAND_20_MS)?;
            let contour = constant::CODEBOOK_SUBFRAME_PITCH_CONTOUR_NARROWBAND_20MS[k as usize];
            pitch_lags[i] = (primary_lag as i32 + contour[i] as i32) as u16;
        }

        return Ok(pitch_lags);
    }

    fn decode_primary_lag<R: RangeDecoder>(&self, decoder: &mut R) -> Result<u16> {
        let high_part = decoder.decode_symbol_with_icdf(&constant::ICDF_PRIMARY_PITCH_LAG_HIGH_PART)?;
        let low_part = decoder.decode_symbol_with_icdf(&constant::ICDF_PRIMARY_PITCH_LAG_LOW_PART_NARROWBAND)?;

        return Ok((high_part as u16 * 4) + low_part as u16 + 16);
    }

    fn decode_ltp_coeffs<R: RangeDecoder>(&self, decoder: &mut R) -> Result<Vec<Vec<i8>>> {
        let num_subframes = self.state.frame_size.into();
        let mut ltp_coeffs = vec![vec![0i8; 5]; num_subframes];

        let periodicity_index = decoder.decode_symbol_with_icdf(&constant::ICDF_PERIODICITY_INDEX)?;

        for subframe in ltp_coeffs.iter_mut() {
            let filter_index = match periodicity_index {
                0 => decoder.decode_symbol_with_icdf(&constant::ICDF_LTP_FILTER_INDEX_0)?,
                1 => decoder.decode_symbol_with_icdf(&constant::ICDF_LTP_FILTER_INDEX_1)?,
                2 => decoder.decode_symbol_with_icdf(&constant::ICDF_LTP_FILTER_INDEX_2)?,
                _ => return Err(Error::InvalidPeriodicityIndex.into()),
            } as usize;

            let filter = match periodicity_index {
                0 => constant::CODEBOOK_LTP_FILTER_PERIODICITY_INDEX_0[filter_index],
                1 => constant::CODEBOOK_LTP_FILTER_PERIODICITY_INDEX_1[filter_index],
                2 => constant::CODEBOOK_LTP_FILTER_PERIODICITY_INDEX_2[filter_index],
                _ => return Err(Error::InvalidPeriodicityIndex.into()),
            };

            subframe.copy_from_slice(&filter);
        }

        return Ok(ltp_coeffs);
    }

    fn decode_ltp_scaling<R: RangeDecoder>(&self, decoder: &mut R) -> Result<f32> {
        const LTP_SCALE_Q14_0: f32 = 15565.0 / 16384.0;
        const LTP_SCALE_Q14_1: f32 = 12288.0 / 16384.0;
        const LTP_SCALE_Q14_2: f32 = 8192.0 / 16384.0;

        let i = decoder.decode_symbol_with_icdf(&constant::ICDF_LTP_SCALING_PARAMETER)?;

        let scale = match i {
            0 => LTP_SCALE_Q14_0,
            1 => LTP_SCALE_Q14_1,
            2 => LTP_SCALE_Q14_2,
            _ => return Err(Error::InvalidLTPScalingIndex.into()),
        };

        return Ok(scale);
    }

    fn decode_ltp<R: RangeDecoder>(&mut self, decoder: &mut R, frame: &mut Frame) -> Result<()> {
        let pitch_lags = self.decode_pitch_lags(decoder)?;
        let ltp_coeffs = self.decode_ltp_coeffs(decoder)?;
        let ltp_scale = self.decode_ltp_scaling(decoder)?;

        if pitch_lags.len() != frame.subframes.len() || ltp_coeffs.len() != frame.subframes.len() {
            return Err(Error::MismatchFrameSubframes.into());
        }

        frame.ltp_scale = ltp_scale;

        frame.subframes.iter_mut()
            .zip(pitch_lags.iter().zip(ltp_coeffs.iter()))
            .for_each(|(subframe, (&pitch_lag, ltp_coeff))| {
                subframe.pitch_lag = pitch_lag;
                subframe.ltp_coeffs = ltp_coeff.clone();
            });

        return Ok(());
    }


    fn decode_gains<R: RangeDecoder>(&self, decoder: &mut R, frame: &mut Frame) -> Result<()> {
        for subframe in frame.subframes.iter_mut() {
            let gain = self.decode_subframe_gain(decoder)?;
            subframe.gain = gain;
        }

        return Ok(());
    }

    fn decode_subframe_gain<R: RangeDecoder>(&self, decoder: &mut R) -> Result<f32> {
        let icdf_gain_msb = match self.state.prev_frame_type.signal_type {
            SignalType::Voiced => &constant::ICDF_INDEPENDENT_QUANTIZATION_GAIN_MSB_VOICED,
            SignalType::Unvoiced | SignalType::Inactive => &constant::ICDF_INDEPENDENT_QUANTIZATION_GAIN_MSB_UNVOICED,
        };

        let gain_msb = decoder.decode_symbol_with_icdf(icdf_gain_msb)? as u8;
        let gain_lsb = decoder.decode_symbol_with_icdf(&constant::ICDF_INDEPENDENT_QUANTIZATION_GAIN_LSB)? as u8;

        let gain = ((gain_msb as u16) << 8) | (gain_lsb as u16);

        const GAIN_NORMALIZATION_FACTOR: f32 = 256.0;
        let normalized_gain = gain as f32 / GAIN_NORMALIZATION_FACTOR;

        return Ok(normalized_gain);
    }


    fn decode_excitation<R: RangeDecoder>(
        &mut self,
        decoder: &mut R,
        frame: &mut Frame,
    ) -> Result<()> {
        let rate_level = self.decode_rate_level(decoder, frame.frame_type.signal_type)?;
        let (pulse_counts, lsb_counts) = self.decode_pulse_counts(decoder, rate_level)?;
        let lcg_seed = decoder.decode_symbol_with_icdf(&constant::ICDF_LINEAR_CONGRUENTIAL_GENERATOR_SEED)?;

        for subframe in frame.subframes.iter_mut() {
            let pulse_locations = self.decode_pulse_locations(decoder, pulse_counts, lsb_counts)?;
            subframe.excitation = pulse_locations;

            // Decode the signs of the excitation
            self.decode_excitation_signs(
                decoder,
                &mut subframe.excitation,
                frame.frame_type.signal_type,
                frame.frame_type.quantization_offset_type,
                pulse_counts,
            )?;

            self.apply_sign_and_scaling(
                &mut subframe.excitation,
                frame.frame_type,
                lcg_seed,
            )?;
        }

        return Ok(());
    }


    fn apply_sign_and_scaling(
        &self,
        excitation: &mut Vec<f32>,
        frame_type: FrameType,
        lcg_seed: u32,
    ) -> Result<()> {
        const SCALE_FACTOR: f32 = 256.0;
        const SIGN_OFFSET: i32 = 20;
        const LCG_MULTIPLIER: u32 = 196_314_165;
        const LCG_INCREMENT: u32 = 907_633_515;
        const LCG_SIGN_MASK: u32 = 0x8000_0000;

        let offset_q23 = self.get_quantization_offset(frame_type.signal_type, frame_type.quantization_offset_type) as f32;
        let mut excitation_scaled = Vec::with_capacity(excitation.len());

        for &e_raw in excitation.iter() {
            let mut e_q23 = (e_raw * SCALE_FACTOR) - (e_raw.signum() * SIGN_OFFSET as f32) + offset_q23;

            let mut seed = lcg_seed;
            seed = seed.wrapping_mul(LCG_MULTIPLIER).wrapping_add(LCG_INCREMENT);

            if (seed & LCG_SIGN_MASK) != 0 {
                e_q23 = -e_q23;
            }

            excitation_scaled.push(e_q23);
        }

        *excitation = excitation_scaled;

        Ok(())
    }


    fn get_quantization_offset(&self, signal_type: SignalType, quantization_offset_type: QuantizationOffsetType) -> i32 {
        return match (signal_type, quantization_offset_type) {
            (SignalType::Inactive, QuantizationOffsetType::Low) => 25,
            (SignalType::Inactive, QuantizationOffsetType::High) => 60,
            (SignalType::Unvoiced, QuantizationOffsetType::Low) => 25,
            (SignalType::Unvoiced, QuantizationOffsetType::High) => 60,
            (SignalType::Voiced, QuantizationOffsetType::Low) => 8,
            (SignalType::Voiced, QuantizationOffsetType::High) => 25,
        };
    }
    fn decode_rate_level<R: RangeDecoder>(&self, decoder: &mut R, signal_type: SignalType) -> Result<u32> {
        let icdf = match signal_type {
            SignalType::Voiced => &constant::ICDF_RATE_LEVEL_VOICED,
            SignalType::Unvoiced | SignalType::Inactive => &constant::ICDF_RATE_LEVEL_UNVOICED,
        };

        return decoder.decode_symbol_with_icdf(icdf);
    }

    fn decode_pulse_counts<R: RangeDecoder>(&self, decoder: &mut R, rate_level: u32) -> Result<(u8, u8)> {
        const MAX_PULSE_COUNT: u32 = 17;
        const MAX_LSB_COUNT: u8 = 10;

        let mut count = decoder.decode_symbol_with_icdf(&constant::ICDF_PULSE_COUNT[rate_level as usize])?;
        let mut lsb_count = 0u8;

        while count == MAX_PULSE_COUNT && lsb_count < MAX_LSB_COUNT {
            count = decoder.decode_symbol_with_icdf(&constant::ICDF_PULSE_COUNT[9])?;
            lsb_count += 1;
        }

        if lsb_count == MAX_LSB_COUNT {
            count = decoder.decode_symbol_with_icdf(&constant::ICDF_PULSE_COUNT[MAX_LSB_COUNT as usize])?;
        }

        return Ok((count as u8, lsb_count));
    }

    fn decode_pulse_locations<R: RangeDecoder>(
        &self,
        decoder: &mut R,
        pulse_count: u8,
        lsb_count: u8,
    ) -> Result<Vec<f32>> {
        const SHELL_BLOCK_SIZE: usize = 16;
        const MAX_PULSE_COUNT: u8 = 17;

        let mut excitation = Vec::with_capacity(SHELL_BLOCK_SIZE);

        if pulse_count == 0 {
            excitation.extend(std::iter::repeat(0.0).take(SHELL_BLOCK_SIZE));
            return Ok(excitation);
        }

        let mut pulse_partitions = [pulse_count; SHELL_BLOCK_SIZE];

        for &block_size in &[16, 8, 4, 2] {
            let half_block_size = block_size / 2;

            for i in (0..SHELL_BLOCK_SIZE).step_by(block_size) {
                let icdf = match block_size {
                    16 => &constant::ICDF_PULSE_COUNT_SPLIT_16_SAMPLE_PARTITIONS[pulse_partitions[i] as usize],
                    8 => &constant::ICDF_PULSE_COUNT_SPLIT_8_SAMPLE_PARTITIONS[pulse_partitions[i] as usize],
                    4 => &constant::ICDF_PULSE_COUNT_SPLIT_4_SAMPLE_PARTITIONS[pulse_partitions[i] as usize],
                    2 => &constant::ICDF_PULSE_COUNT_SPLIT_2_SAMPLE_PARTITIONS[pulse_partitions[i] as usize],
                    _ => return Err(Error::InvalidPartitionSize.into()),
                };

                let left_pulses = decoder.decode_symbol_with_icdf(icdf)? as u8;
                let right_pulses = pulse_partitions[i].saturating_sub(left_pulses);

                pulse_partitions[i] = left_pulses;
                pulse_partitions[i + half_block_size] = right_pulses;
            }
        }

        for partition in pulse_partitions.iter() {
            let mut value = *partition as f32;

            if pulse_count == MAX_PULSE_COUNT {
                for _ in 0..lsb_count {
                    let lsb = decoder.decode_symbol_with_icdf(&constant::ICDF_EXCITATION_LSB)?;
                    value = value * 2.0 + lsb as f32;
                }
            }

            excitation.push(value);
        }

        return Ok(excitation);
    }


    fn decode_excitation_signs<R: RangeDecoder>(
        &self,
        decoder: &mut R,
        excitation: &mut [f32],
        signal_type: SignalType,
        quantization_offset_type: QuantizationOffsetType,
        pulse_count: u8,
    ) -> Result<()> {
        if excitation.is_empty() {
            return Ok(());
        }

        let icdf = match (signal_type, quantization_offset_type, pulse_count) {
            (SignalType::Inactive, QuantizationOffsetType::Low, 0) => &constant::ICDF_EXCITATION_SIGN_INACTIVE_SIGNAL_LOW_QUANTIZATION_0_PULSE,
            (SignalType::Inactive, QuantizationOffsetType::Low, 1) => &constant::ICDF_EXCITATION_SIGN_INACTIVE_SIGNAL_LOW_QUANTIZATION_1_PULSE,
            (SignalType::Inactive, QuantizationOffsetType::Low, 2) => &constant::ICDF_EXCITATION_SIGN_INACTIVE_SIGNAL_LOW_QUANTIZATION_2_PULSE,
            (SignalType::Inactive, QuantizationOffsetType::Low, 3) => &constant::ICDF_EXCITATION_SIGN_INACTIVE_SIGNAL_LOW_QUANTIZATION_3_PULSE,
            (SignalType::Inactive, QuantizationOffsetType::Low, 4) => &constant::ICDF_EXCITATION_SIGN_INACTIVE_SIGNAL_LOW_QUANTIZATION_4_PULSE,
            (SignalType::Inactive, QuantizationOffsetType::Low, 5) => &constant::ICDF_EXCITATION_SIGN_INACTIVE_SIGNAL_LOW_QUANTIZATION_5_PULSE,
            (SignalType::Inactive, QuantizationOffsetType::Low, _) => &constant::ICDF_EXCITATION_SIGN_INACTIVE_SIGNAL_LOW_QUANTIZATION_6_PLUS_PULSE,

            (SignalType::Inactive, QuantizationOffsetType::High, 0) => &constant::ICDF_EXCITATION_SIGN_INACTIVE_SIGNAL_HIGH_QUANTIZATION_0_PULSE,
            (SignalType::Inactive, QuantizationOffsetType::High, 1) => &constant::ICDF_EXCITATION_SIGN_INACTIVE_SIGNAL_HIGH_QUANTIZATION_1_PULSE,
            (SignalType::Inactive, QuantizationOffsetType::High, 2) => &constant::ICDF_EXCITATION_SIGN_INACTIVE_SIGNAL_HIGH_QUANTIZATION_2_PULSE,
            (SignalType::Inactive, QuantizationOffsetType::High, 3) => &constant::ICDF_EXCITATION_SIGN_INACTIVE_SIGNAL_HIGH_QUANTIZATION_3_PULSE,
            (SignalType::Inactive, QuantizationOffsetType::High, 4) => &constant::ICDF_EXCITATION_SIGN_INACTIVE_SIGNAL_HIGH_QUANTIZATION_4_PULSE,
            (SignalType::Inactive, QuantizationOffsetType::High, 5) => &constant::ICDF_EXCITATION_SIGN_INACTIVE_SIGNAL_HIGH_QUANTIZATION_5_PULSE,
            (SignalType::Inactive, QuantizationOffsetType::High, _) => &constant::ICDF_EXCITATION_SIGN_INACTIVE_SIGNAL_HIGH_QUANTIZATION_6_PLUS_PULSE,

            (SignalType::Unvoiced, QuantizationOffsetType::Low, 0) => &constant::ICDF_EXCITATION_SIGN_UNVOICED_SIGNAL_LOW_QUANTIZATION_0_PULSE,
            (SignalType::Unvoiced, QuantizationOffsetType::Low, 1) => &constant::ICDF_EXCITATION_SIGN_UNVOICED_SIGNAL_LOW_QUANTIZATION_1_PULSE,
            (SignalType::Unvoiced, QuantizationOffsetType::Low, 2) => &constant::ICDF_EXCITATION_SIGN_UNVOICED_SIGNAL_LOW_QUANTIZATION_2_PULSE,
            (SignalType::Unvoiced, QuantizationOffsetType::Low, 3) => &constant::ICDF_EXCITATION_SIGN_UNVOICED_SIGNAL_LOW_QUANTIZATION_3_PULSE,
            (SignalType::Unvoiced, QuantizationOffsetType::Low, 4) => &constant::ICDF_EXCITATION_SIGN_UNVOICED_SIGNAL_LOW_QUANTIZATION_4_PULSE,
            (SignalType::Unvoiced, QuantizationOffsetType::Low, 5) => &constant::ICDF_EXCITATION_SIGN_UNVOICED_SIGNAL_LOW_QUANTIZATION_5_PULSE,
            (SignalType::Unvoiced, QuantizationOffsetType::Low, _) => &constant::ICDF_EXCITATION_SIGN_UNVOICED_SIGNAL_LOW_QUANTIZATION_6_PLUS_PULSE,

            (SignalType::Unvoiced, QuantizationOffsetType::High, 0) => &constant::ICDF_EXCITATION_SIGN_UNVOICED_SIGNAL_HIGH_QUANTIZATION_0_PULSE,
            (SignalType::Unvoiced, QuantizationOffsetType::High, 1) => &constant::ICDF_EXCITATION_SIGN_UNVOICED_SIGNAL_HIGH_QUANTIZATION_1_PULSE,
            (SignalType::Unvoiced, QuantizationOffsetType::High, 2) => &constant::ICDF_EXCITATION_SIGN_UNVOICED_SIGNAL_HIGH_QUANTIZATION_2_PULSE,
            (SignalType::Unvoiced, QuantizationOffsetType::High, 3) => &constant::ICDF_EXCITATION_SIGN_UNVOICED_SIGNAL_HIGH_QUANTIZATION_3_PULSE,
            (SignalType::Unvoiced, QuantizationOffsetType::High, 4) => &constant::ICDF_EXCITATION_SIGN_UNVOICED_SIGNAL_HIGH_QUANTIZATION_4_PULSE,
            (SignalType::Unvoiced, QuantizationOffsetType::High, 5) => &constant::ICDF_EXCITATION_SIGN_UNVOICED_SIGNAL_HIGH_QUANTIZATION_5_PULSE,
            (SignalType::Unvoiced, QuantizationOffsetType::High, _) => &constant::ICDF_EXCITATION_SIGN_UNVOICED_SIGNAL_HIGH_QUANTIZATION_6_PLUS_PULSE,

            (SignalType::Voiced, QuantizationOffsetType::Low, 0) => &constant::ICDF_EXCITATION_SIGN_VOICED_SIGNAL_LOW_QUANTIZATION_0_PULSE,
            (SignalType::Voiced, QuantizationOffsetType::Low, 1) => &constant::ICDF_EXCITATION_SIGN_VOICED_SIGNAL_LOW_QUANTIZATION_1_PULSE,
            (SignalType::Voiced, QuantizationOffsetType::Low, 2) => &constant::ICDF_EXCITATION_SIGN_VOICED_SIGNAL_LOW_QUANTIZATION_2_PULSE,
            (SignalType::Voiced, QuantizationOffsetType::Low, 3) => &constant::ICDF_EXCITATION_SIGN_VOICED_SIGNAL_LOW_QUANTIZATION_3_PULSE,
            (SignalType::Voiced, QuantizationOffsetType::Low, 4) => &constant::ICDF_EXCITATION_SIGN_VOICED_SIGNAL_LOW_QUANTIZATION_4_PULSE,
            (SignalType::Voiced, QuantizationOffsetType::Low, 5) => &constant::ICDF_EXCITATION_SIGN_VOICED_SIGNAL_LOW_QUANTIZATION_5_PULSE,
            (SignalType::Voiced, QuantizationOffsetType::Low, _) => &constant::ICDF_EXCITATION_SIGN_VOICED_SIGNAL_LOW_QUANTIZATION_6_PLUS_PULSE,

            (SignalType::Voiced, QuantizationOffsetType::High, 0) => &constant::ICDF_EXCITATION_SIGN_VOICED_SIGNAL_HIGH_QUANTIZATION_0_PULSE,
            (SignalType::Voiced, QuantizationOffsetType::High, 1) => &constant::ICDF_EXCITATION_SIGN_VOICED_SIGNAL_HIGH_QUANTIZATION_1_PULSE,
            (SignalType::Voiced, QuantizationOffsetType::High, 2) => &constant::ICDF_EXCITATION_SIGN_VOICED_SIGNAL_HIGH_QUANTIZATION_2_PULSE,
            (SignalType::Voiced, QuantizationOffsetType::High, 3) => &constant::ICDF_EXCITATION_SIGN_VOICED_SIGNAL_HIGH_QUANTIZATION_3_PULSE,
            (SignalType::Voiced, QuantizationOffsetType::High, 4) => &constant::ICDF_EXCITATION_SIGN_VOICED_SIGNAL_HIGH_QUANTIZATION_4_PULSE,
            (SignalType::Voiced, QuantizationOffsetType::High, 5) => &constant::ICDF_EXCITATION_SIGN_VOICED_SIGNAL_HIGH_QUANTIZATION_5_PULSE,
            (SignalType::Voiced, QuantizationOffsetType::High, _) => &constant::ICDF_EXCITATION_SIGN_VOICED_SIGNAL_HIGH_QUANTIZATION_6_PLUS_PULSE,
        };

        for sample in excitation.iter_mut() {
            if *sample != 0.0 {
                let sign = decoder.decode_symbol_with_icdf(icdf)?;
                if sign == 0 {
                    *sample = -*sample;
                }
            }
        }

        return Ok(());
    }


    fn synthesize_frame(&mut self, frame: &Frame) -> Result<()> {
        const SUBFRAME_COUNT: usize = 4;

        let samples_per_subframe = frame.sample_count / SUBFRAME_COUNT;
        let channels = self.buffer.spec().channels.count();

        if self.buffer.frames() + frame.sample_count > self.buffer.capacity() {
            return Err(Error::BufferOverflow.into());
        }

        let start = self.buffer.frames();

        for ch in 0..channels {
            let dst = self.buffer.chan_mut(ch);
            for (s, subframe) in frame.subframes.iter().enumerate() {
                let excitation = &subframe.excitation;
                for (i, &sample) in excitation.iter().enumerate() {
                    dst[start + s * samples_per_subframe + i] = sample;
                }
            }
        }

        self.buffer.render_reserved(Some(frame.sample_count));

        return Ok(());
    }
}


#[derive(Debug, Default, Clone)]
pub struct State {
    sample_rate: u32,
    channels: Channels,
    frame_size: FrameSize,
    bandwidth: Bandwidth,
    prev_frame_type: FrameType,
    prev_samples: Vec<f32>,
    lbrr_flag: bool,
    lpc_order: usize,
}

impl State {
    pub fn try_new(channels: Channels, frame_size: FrameSize, bandwidth: Bandwidth) -> Result<Self> {
        let sample_rate = bandwidth.sample_rate();
        let frame_length = Self::calculate_frame_length(sample_rate, frame_size)?;
        let channel_count = channels.count();
        let lpc_order = LpcOrder::from(bandwidth);
        let prev_frame_type = FrameType::default();
        let prev_samples = vec![0.0; frame_length * channel_count];
        let lbrr_flag = false;

        return Ok(Self {
            sample_rate,
            channels,
            frame_size,
            bandwidth,
            prev_frame_type,
            prev_samples,
            lbrr_flag,
            lpc_order,
        });
    }

    pub fn reset(&mut self) {
        self.lbrr_flag = false;
        self.prev_samples.fill(0.0);
    }

    fn calculate_frame_length(sample_rate: u32, frame_size: FrameSize) -> Result<usize> {
        let samples = (sample_rate as u128)
            .checked_mul(frame_size.duration().as_nanos())
            .and_then(|ns| ns.checked_div(1_000_000_000))
            .ok_or(Error::CalculationOverflow)?;

        return usize::try_from(samples).map_err(|_| Error::CalculationOverflow.into());
    }
}

#[derive(Debug, Default)]
pub struct Frame {
    pub frame_type: FrameType,
    pub vad_flag: bool,
    pub lbrr_flag: bool,
    pub sample_count: usize,
    pub subframes: Vec<Subframe>,
    pub ltp_scale: f32,
    pub lsf_interpolation_index: Option<u32>,
}

impl Frame {
    pub fn new(sample_count: usize, num_subframes: usize) -> Self {
        return Self {
            frame_type: FrameType::default(),
            vad_flag: false,
            lbrr_flag: false,
            sample_count,
            subframes: vec![Subframe::default(); num_subframes],
            ltp_scale: 0.0,
            lsf_interpolation_index: None,
        };
    }
}

#[derive(Debug, Clone, Default)]
pub struct Subframe {
    pub gain: f32,
    pub nlsf_q15: Vec<i16>,
    pub ltp_coeffs: Vec<i8>,
    pub pitch_lag: u16,
    pub excitation: Vec<f32>,
}

#[derive(Debug, Clone, Default, Copy, PartialEq, Eq)]
pub struct FrameType {
    signal_type: SignalType,
    quantization_offset_type: QuantizationOffsetType,
}
impl FrameType {
    pub fn new(signal_type: SignalType, quantization_offset_type: QuantizationOffsetType) -> Self {
        return Self { signal_type, quantization_offset_type };
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SignalType {
    #[default]
    Inactive,
    Voiced,
    Unvoiced,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum QuantizationOffsetType {
    #[default]
    High,
    Low,
}

type SubframeSize = usize;
impl From<FrameSize> for SubframeSize {
    fn from(frame_size: FrameSize) -> Self {
        return match frame_size {
            FrameSize::Ms2_5 => 1,
            FrameSize::Ms5 => 1,
            FrameSize::Ms10 => 2,
            FrameSize::Ms20 => 4,
            FrameSize::Ms40 => 8,
            FrameSize::Ms60 => 12,
        };
    }
}

type LpcOrder = usize;
impl From<Bandwidth> for LpcOrder {
    fn from(value: Bandwidth) -> Self {
        return match value {
            Bandwidth::NarrowBand => 10,
            Bandwidth::MediumBand => 12,
            Bandwidth::WideBand | Bandwidth::SuperWideBand | Bandwidth::FullBand => 16,
        };
    }
}

