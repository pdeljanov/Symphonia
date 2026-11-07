//! SILK PVQ Shell Excitation Decoding
//!
//! Implements decoding of the LCG seed, rate level, per-block pulse counts and LSB shifts,
//! recursive binary tree shell pulse locations, LSBs, excitation signs, and frame-continuous
//! LCG pseudo-random sign/offset scaling (RFC 6716 Sections 4.2.7.7 and 4.2.7.8).

use symphonia_core::errors::Result;

use crate::entropy;
use crate::silk::constant;
use crate::silk::decoder::Decoder;
use crate::silk::error::Error;
use crate::silk::{
    Frame, MAX_FRAME_SAMPLES, MAX_SHELL_BLOCKS, MAX_SUBFRAME_SAMPLES, QuantizationOffsetType,
    SHELL_BLOCK_SAMPLES, SignalType,
};

impl Decoder {
    /// Decodes the 2-bit Linear Congruential Generator (LCG) seed at the end of the
    /// SILK side-information indices before excitation decoding (RFC 6716 Section 4.2.7.7).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.7>
    pub(crate) fn decode_lcg_seed(
        &self,
        decoder: &mut entropy::Decoder<'_>,
        frame: &mut Frame,
    ) -> Result<u8> {
        let seed = decoder
            .decode_symbol_with_icdf(&constant::ICDF_LINEAR_CONGRUENTIAL_GENERATOR_SEED)?
            as u8;
        frame.lcg_seed = seed;
        return Ok(seed);
    }

    /// Decodes the PVQ shell excitation pulses and constructs the Q14 excitation signal
    /// across all shell blocks in `frame` (RFC 6716 Section 4.2.7.8).
    ///
    /// Matches `silk_decode_pulses` (`silk/decode_pulses.c`) and the excitation loop in
    /// `silk_decode_core` (`silk/decode_core.c:78-94`).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8>
    pub(crate) fn decode_excitation(
        &mut self,
        decoder: &mut entropy::Decoder<'_>,
        frame: &mut Frame,
    ) -> Result<()> {
        let num_blocks = frame.num_shell_blocks();
        if num_blocks > MAX_SHELL_BLOCKS || frame.sample_count > MAX_FRAME_SAMPLES {
            return Err(Error::FrameLengthExceedsMaximum.into());
        }

        let frame_type = frame.frame_type;
        let rate_level = self.decode_rate_level(decoder, frame_type.signal_type)?;
        frame.rate_level = rate_level as u8;

        // 1. Decode pulse counts and LSB shift counts for every 16-sample shell block
        for block in 0..num_blocks {
            let (pulse_count, lsb_count) = self.decode_block_pulse_count(decoder, rate_level)?;
            frame.pulse_counts[block] = pulse_count;
            frame.lsb_counts[block] = lsb_count;
        }

        // 2. Decode shell pulse locations via depth-first recursive binary splits (16 -> 8 -> 4 -> 2 -> 1)
        let mut pulses_buf = [0i16; MAX_SHELL_BLOCKS * SHELL_BLOCK_SAMPLES];
        for block in 0..num_blocks {
            let block_pulses =
                &mut pulses_buf[block * SHELL_BLOCK_SAMPLES..(block + 1) * SHELL_BLOCK_SAMPLES];
            let pulse_count = frame.pulse_counts[block];
            if pulse_count > 0 {
                Self::decode_shell_partition(decoder, block_pulses, pulse_count)?;
            }
        }

        // 3. Decode LSBs for blocks where escape value 17 was encountered
        for block in 0..num_blocks {
            let n_lsbs = frame.lsb_counts[block];
            if n_lsbs > 0 {
                let block_pulses =
                    &mut pulses_buf[block * SHELL_BLOCK_SAMPLES..(block + 1) * SHELL_BLOCK_SAMPLES];
                for pulse in block_pulses.iter_mut() {
                    let mut abs_q = *pulse;
                    for _ in 0..n_lsbs {
                        let bit =
                            decoder.decode_symbol_with_icdf(&constant::ICDF_EXCITATION_LSB)? as i16;
                        abs_q = (abs_q << 1) | bit;
                    }
                    *pulse = abs_q;
                }
            }
        }

        // 4. Decode signs for all non-zero pulses in blocks with pulse_count > 0 or lsb_count > 0
        for block in 0..num_blocks {
            let pulse_count = frame.pulse_counts[block];
            let lsb_count = frame.lsb_counts[block];
            if pulse_count > 0 || lsb_count > 0 {
                let block_pulses =
                    &mut pulses_buf[block * SHELL_BLOCK_SAMPLES..(block + 1) * SHELL_BLOCK_SAMPLES];
                self.decode_block_excitation_signs(
                    decoder,
                    block_pulses,
                    frame_type.signal_type,
                    frame_type.quantization_offset_type,
                    pulse_count,
                )?;
            }
        }

        // 5. Copy decoded frame pulses and construct Q14 excitation with continuous LCG seed
        let sample_count = frame.sample_count;
        frame.pulses[..sample_count].copy_from_slice(&pulses_buf[..sample_count]);
        self.construct_excitation_q14(frame);

        return Ok(());
    }

    /// Decodes the excitation rate level index (`0..=8`, RFC 6716 Section 4.2.7.8.1).
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

    /// Decodes the pulse count (`0..=16`) and LSB count (`0..=10`) for a single 16-sample shell block
    /// (RFC 6716 Section 4.2.7.8.2, `silk/decode_pulses.c:67-78`).
    pub(crate) fn decode_block_pulse_count(
        &self,
        decoder: &mut entropy::Decoder<'_>,
        rate_level: u32,
    ) -> Result<(u8, u8)> {
        const ESCAPE_PULSE_COUNT: u32 = 17;
        const MAX_LSB_SHIFTS: u8 = 10;

        let rate_idx = (rate_level as usize).min(constant::ICDF_PULSE_COUNT.len() - 1);
        let mut count = decoder.decode_symbol_with_icdf(&constant::ICDF_PULSE_COUNT[rate_idx])?;
        let mut lsb_count = 0u8;

        while count == ESCAPE_PULSE_COUNT {
            lsb_count += 1;
            let table_idx = if lsb_count == MAX_LSB_SHIFTS { 10 } else { 9 };
            count = decoder.decode_symbol_with_icdf(&constant::ICDF_PULSE_COUNT[table_idx])?;
        }

        return Ok((count as u8, lsb_count));
    }

    /// Recursively splits `total_pulses` across `partition` in depth-first order
    /// (`16 -> 8 -> 4 -> 2 -> 1`, RFC 6716 Section 4.2.7.8.3, `silk/shell_coder.c:119-151`).
    pub(crate) fn decode_shell_partition(
        decoder: &mut entropy::Decoder<'_>,
        partition: &mut [i16],
        total_pulses: u8,
    ) -> Result<()> {
        if total_pulses == 0 {
            partition.fill(0);
            return Ok(());
        }

        let len = partition.len();
        if len == 1 {
            partition[0] = total_pulses as i16;
            return Ok(());
        }

        let pulse_table_idx = (total_pulses as usize) - 1;
        let icdf: &[u32] = match len {
            16 => constant::ICDF_PULSE_COUNT_SPLIT_16_SAMPLE_PARTITIONS[pulse_table_idx],
            8 => constant::ICDF_PULSE_COUNT_SPLIT_8_SAMPLE_PARTITIONS[pulse_table_idx],
            4 => constant::ICDF_PULSE_COUNT_SPLIT_4_SAMPLE_PARTITIONS[pulse_table_idx],
            2 => constant::ICDF_PULSE_COUNT_SPLIT_2_SAMPLE_PARTITIONS[pulse_table_idx],
            _ => return Err(Error::InvalidPartitionSize.into()),
        };

        let left_pulses = decoder.decode_symbol_with_icdf(icdf)? as u8;
        let right_pulses = total_pulses
            .checked_sub(left_pulses)
            .ok_or(Error::InvalidPulseCount)?;

        let (left_half, right_half) = partition.split_at_mut(len >> 1);
        Self::decode_shell_partition(decoder, left_half, left_pulses)?;
        Self::decode_shell_partition(decoder, right_half, right_pulses)?;

        return Ok(());
    }

    /// Decodes signs for non-zero pulses in a 16-sample shell block
    /// (RFC 6716 Section 4.2.7.8.5, `silk/code_signs.c:75-115`).
    pub(crate) fn decode_block_excitation_signs(
        &self,
        decoder: &mut entropy::Decoder<'_>,
        block_pulses: &mut [i16],
        signal_type: SignalType,
        quantization_offset_type: QuantizationOffsetType,
        pulse_count: u8,
    ) -> Result<()> {
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

        for pulse in block_pulses.iter_mut() {
            if *pulse > 0 {
                let sign_symbol = decoder.decode_symbol_with_icdf(icdf)?;
                if sign_symbol == 0 {
                    *pulse = -*pulse;
                }
            }
        }

        return Ok(());
    }

    /// Constructs `frame.exc_q14` from `frame.pulses` and `frame.lcg_seed`, threading
    /// the LCG state continuously across all `frame.sample_count` samples of the SILK frame
    /// (RFC 6716 Section 4.2.7.8.6, `silk/decode_core.c:78-94`).
    pub(crate) fn construct_excitation_q14(&self, frame: &mut Frame) {
        const QUANT_LEVEL_ADJUST_Q14: i32 = 80 << 4;
        const LCG_MULTIPLIER: i32 = 196_314_165;
        const LCG_INCREMENT: i32 = 907_633_515;

        let offset_q8 = self.get_quantization_offset(
            frame.frame_type.signal_type,
            frame.frame_type.quantization_offset_type,
        );
        let offset_q14 = offset_q8 << 6;

        let sample_count = frame.sample_count;
        let mut rand_seed = frame.lcg_seed as i32;

        for i in 0..sample_count {
            rand_seed = rand_seed
                .wrapping_mul(LCG_MULTIPLIER)
                .wrapping_add(LCG_INCREMENT);

            let pulse = frame.pulses[i] as i32;
            let mut exc = pulse << 14;
            if exc > 0 {
                exc -= QUANT_LEVEL_ADJUST_Q14;
            } else if exc < 0 {
                exc += QUANT_LEVEL_ADJUST_Q14;
            }
            exc += offset_q14;

            if rand_seed < 0 {
                exc = -exc;
            }

            frame.exc_q14[i] = exc;
            rand_seed = rand_seed.wrapping_add(pulse);
        }

        if let Some(raw_subfr_len) = sample_count.checked_div(frame.num_subframes) {
            let subfr_len = raw_subfr_len.min(MAX_SUBFRAME_SAMPLES);
            for (s, subframe) in frame.subframes[..frame.num_subframes]
                .iter_mut()
                .enumerate()
            {
                let start = s * subfr_len;
                subframe.excitation_len = subfr_len;
                for i in 0..subfr_len {
                    let exc_q14 = frame.exc_q14[start + i];
                    subframe.excitation_q14[i] = exc_q14;
                    subframe.excitation[i] = exc_q14 as f32;
                }
            }
        }
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
}
