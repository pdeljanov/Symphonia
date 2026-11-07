//! SILK Excitation Decoding
//!
//! Implements decoding of rate level, pulse counts, shell pulse locations,
//! excitation signs, and LCG pseudo-random sign/offset scaling (RFC 6716 Section 4.2.7.8).

use symphonia_core::errors::Result;

use crate::entropy;
use crate::silk::constant;
use crate::silk::decoder::Decoder;
use crate::silk::error::Error;
use crate::silk::{
    Frame, FrameType, QuantizationOffsetType, SHELL_BLOCK_SAMPLES, SignalType,
};

impl Decoder {
    /// Decodes the excitation signal for all subframes in a SILK frame (RFC 6716 Section 4.2.7.8).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8>
    pub(crate) fn decode_excitation(
        &mut self,
        decoder: &mut entropy::Decoder<'_>,
        frame: &mut Frame,
    ) -> Result<()> {
        let frame_type = frame.frame_type;
        let rate_level = self.decode_rate_level(decoder, frame_type.signal_type)?;
        let (pulse_counts, lsb_counts) = self.decode_pulse_counts(decoder, rate_level)?;
        let lcg_seed = decoder.decode_symbol_with_icdf(&constant::ICDF_LINEAR_CONGRUENTIAL_GENERATOR_SEED)?;

        for subframe in frame.active_subframes_mut() {
            let pulse_locations = self.decode_pulse_locations(decoder, pulse_counts, lsb_counts)?;
            subframe.excitation[..SHELL_BLOCK_SAMPLES].copy_from_slice(&pulse_locations);
            subframe.excitation_len = SHELL_BLOCK_SAMPLES;

            self.decode_excitation_signs(
                decoder,
                subframe.active_excitation_mut(),
                frame_type.signal_type,
                frame_type.quantization_offset_type,
                pulse_counts,
            )?;

            self.apply_sign_and_scaling(
                subframe.active_excitation_mut(),
                frame_type,
                lcg_seed,
            )?;
        }

        return Ok(());
    }

    pub(crate) fn apply_sign_and_scaling(
        &self,
        excitation: &mut [f32],
        frame_type: FrameType,
        mut lcg_seed: u32,
    ) -> Result<()> {
        const SCALE_FACTOR: f32 = 256.0;
        const SIGN_OFFSET: i32 = 20;
        const LCG_MULTIPLIER: u32 = 196_314_165;
        const LCG_INCREMENT: u32 = 907_633_515;
        const LCG_SIGN_MASK: u32 = 0x8000_0000;

        let offset_q23 = self.get_quantization_offset(
            frame_type.signal_type,
            frame_type.quantization_offset_type,
        ) as f32;

        for sample in excitation.iter_mut() {
            let e_raw = *sample;
            let mut e_q23 = (e_raw * SCALE_FACTOR) - (e_raw.signum() * SIGN_OFFSET as f32) + offset_q23;

            lcg_seed = lcg_seed.wrapping_mul(LCG_MULTIPLIER).wrapping_add(LCG_INCREMENT);

            if (lcg_seed & LCG_SIGN_MASK) != 0 {
                e_q23 = -e_q23;
            }

            *sample = e_q23;
            lcg_seed = lcg_seed.wrapping_add(e_raw as u32);
        }

        return Ok(());
    }

    #[inline]
    pub(crate) fn get_quantization_offset(
        &self,
        signal_type: SignalType,
        quantization_offset_type: QuantizationOffsetType,
    ) -> i32 {
        let signal_idx = match signal_type {
            SignalType::Inactive => 0,
            SignalType::Unvoiced => 1,
            SignalType::Voiced => 2,
        };
        let quant_idx = match quantization_offset_type {
            QuantizationOffsetType::Low => 0,
            QuantizationOffsetType::High => 1,
        };
        return constant::QUANTIZATION_OFFSETS[signal_idx][quant_idx];
    }

    pub(crate) fn decode_rate_level(
        &self,
        decoder: &mut entropy::Decoder<'_>,
        signal_type: SignalType,
    ) -> Result<u32> {
        let icdf: &[u32] = match signal_type {
            SignalType::Voiced => &constant::ICDF_RATE_LEVEL_VOICED,
            SignalType::Unvoiced | SignalType::Inactive => &constant::ICDF_RATE_LEVEL_UNVOICED,
        };

        return decoder.decode_symbol_with_icdf(icdf);
    }

    pub(crate) fn decode_pulse_counts(
        &self,
        decoder: &mut entropy::Decoder<'_>,
        rate_level: u32,
    ) -> Result<(u8, u8)> {
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

    pub(crate) fn decode_pulse_locations(
        &self,
        decoder: &mut entropy::Decoder<'_>,
        pulse_count: u8,
        lsb_count: u8,
    ) -> Result<[f32; SHELL_BLOCK_SAMPLES]> {
        const MAX_PULSE_COUNT: u8 = 17;

        let mut excitation = [0.0f32; SHELL_BLOCK_SAMPLES];
        if pulse_count == 0 {
            return Ok(excitation);
        }

        let mut pulse_partitions = [pulse_count; SHELL_BLOCK_SAMPLES];

        for block_size in [16usize, 8, 4, 2] {
            let half_block_size = block_size / 2;

            for i in (0..SHELL_BLOCK_SAMPLES).step_by(block_size) {
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

        for (dst, &partition) in excitation.iter_mut().zip(pulse_partitions.iter()) {
            let mut value = partition as f32;

            if pulse_count == MAX_PULSE_COUNT {
                for _ in 0..lsb_count {
                    let lsb = decoder.decode_symbol_with_icdf(&constant::ICDF_EXCITATION_LSB)?;
                    value = value * 2.0 + lsb as f32;
                }
            }

            *dst = value;
        }

        return Ok(excitation);
    }

    pub(crate) fn decode_excitation_signs(
        &self,
        decoder: &mut entropy::Decoder<'_>,
        excitation: &mut [f32],
        signal_type: SignalType,
        quantization_offset_type: QuantizationOffsetType,
        pulse_count: u8,
    ) -> Result<()> {
        if excitation.is_empty() {
            return Ok(());
        }

        let signal_idx = match signal_type {
            SignalType::Inactive => 0,
            SignalType::Unvoiced => 1,
            SignalType::Voiced => 2,
        };
        let quant_idx = match quantization_offset_type {
            QuantizationOffsetType::Low => 0,
            QuantizationOffsetType::High => 1,
        };
        let pulse_idx = (pulse_count as usize).min(6);
        let icdf = &constant::ICDF_EXCITATION_SIGN[signal_idx][quant_idx][pulse_idx];

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
}
