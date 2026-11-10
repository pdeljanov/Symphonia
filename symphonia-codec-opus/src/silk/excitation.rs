//! SILK excitation decoding
//! ([RFC 6716 Section 4.2.7.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.7>),
//! [Section 4.2.7.8](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8>)).
//!
//! Decodes the LCG seed, the rate level, the pulse and LSB counts of every 16-sample block, the
//! pulse locations of the recursive shell partitions, the LSBs and the signs, and reconstructs
//! the Q14 excitation with the frame-continuous pseudorandom sign and the quantization offset.

use symphonia_core::errors::Result;

use crate::entropy::{self, Cdf};
use crate::silk::constant;
use crate::silk::error::Error;
use crate::silk::{
    Frame, FrameType, MAX_FRAME_SAMPLES, MAX_SHELL_BLOCKS, QuantizationOffsetType,
    SHELL_BLOCK_SAMPLES, SignalType,
};

/// Linear Congruential Generator (LCG) multiplier (RFC 6716 Section 4.2.7.8.6).
const LCG_MULTIPLIER: i32 = 196_314_165;

/// Linear Congruential Generator (LCG) additive increment (RFC 6716 Section 4.2.7.8.6).
const LCG_INCREMENT: i32 = 907_633_515;

/// Pulse count symbol meaning "17 or more pulses": one more LSB per pulse follows
/// (RFC 6716 Section 4.2.7.8.2).
const ESCAPE_PULSE_COUNT: u32 = 17;

/// Largest number of LSBs per shell block (RFC 6716 Section 4.2.7.8.2).
const MAX_LSB_COUNT: u8 = 10;

/// Pulse count CDF used after an escape (RFC 6716 Section 4.2.7.8.2).
const PULSE_COUNT_CDF_AFTER_ESCAPE: usize = 9;

/// Pulse count CDF used after the last allowed escape, which cannot escape again
/// (RFC 6716 Section 4.2.7.8.2).
const PULSE_COUNT_CDF_LAST_ESCAPE: usize = 10;

/// Pulse counts above this share the last sign CDF of their frame type (RFC 6716 Section 4.2.7.8.5).
const SIGN_CDF_MAX_PULSE_COUNT: usize = 6;

/// Left shift placing an integer pulse amplitude in Q14 (RFC 6716 Section 4.2.7.8.6).
const PULSE_TO_Q14_SHIFT: u32 = 14;

/// Left shift turning a Q8 quantization offset into Q14 (RFC 6716 Section 4.2.7.8.6).
const OFFSET_Q8_TO_Q14_SHIFT: u32 = 6;

/// Magnitude subtracted from every non-zero excitation sample, `20` in Q8
/// (RFC 6716 Section 4.2.7.8.6).
const QUANT_LEVEL_ADJUST_Q8: i32 = 20;

/// The magnitude adjustment in Q14 (RFC 6716 Section 4.2.7.8.6).
const QUANT_LEVEL_ADJUST_Q14: i32 = QUANT_LEVEL_ADJUST_Q8 << OFFSET_Q8_TO_Q14_SHIFT;

/// Advances the SILK 32-bit Linear Congruential Generator
/// ([RFC 6716 Section 4.2.7.8.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8.6>)).
#[inline(always)]
pub(crate) fn advance_lcg_seed(seed: i32) -> i32 {
    return seed.wrapping_mul(LCG_MULTIPLIER).wrapping_add(LCG_INCREMENT);
}

/// Decodes the 2-bit Linear Congruential Generator (LCG) seed that follows the side
/// information ([RFC 6716 Section 4.2.7.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.7>)).
pub(crate) fn decode_lcg_seed(decoder: &mut entropy::Decoder<'_>, frame: &mut Frame) -> Result<()> {
    frame.lcg_seed = decoder.decode_symbol(constant::CDF_LINEAR_CONGRUENTIAL_GENERATOR_SEED)? as u8;
    return Ok(());
}

/// Decodes the PVQ shell excitation pulses of every 16-sample block of `frame` and constructs
/// its Q14 excitation: rate level, pulse counts, pulse locations, LSBs, signs and reconstruction
/// ([RFC 6716 Sections 4.2.7.8.1 to 4.2.7.8.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8>)).
///
/// The pulses of the last block beyond `sample_count` are decoded and then cleared.
pub(crate) fn decode_excitation(
    decoder: &mut entropy::Decoder<'_>,
    frame: &mut Frame,
) -> Result<()> {
    let num_blocks = frame.num_shell_blocks();
    let sample_count = frame.sample_count;
    if num_blocks > MAX_SHELL_BLOCKS || sample_count > MAX_FRAME_SAMPLES {
        return Err(Error::FrameLengthExceedsMaximum.into());
    }

    let frame_type = frame.frame_type;
    let rate_level = decode_rate_level(decoder, frame_type.signal_type)?;
    frame.rate_level = rate_level as u8;

    let Frame { pulses, pulse_counts, lsb_counts, .. } = frame;
    let pulse_counts = &mut pulse_counts[..num_blocks];
    let lsb_counts = &mut lsb_counts[..num_blocks];
    for (pulse_count, lsb_count) in pulse_counts.iter_mut().zip(lsb_counts.iter_mut()) {
        (*pulse_count, *lsb_count) = decode_block_pulse_count(decoder, rate_level)?;
    }

    let blocks = &mut pulses[..num_blocks * SHELL_BLOCK_SAMPLES];
    blocks.fill(0);
    for (block, &pulse_count) in blocks
        .chunks_exact_mut(SHELL_BLOCK_SAMPLES)
        .zip(pulse_counts.iter())
        .filter(|&(_, &pulse_count)| pulse_count > 0)
    {
        decode_shell_partition(decoder, block, pulse_count)?;
    }
    for (block, &lsb_count) in blocks
        .chunks_exact_mut(SHELL_BLOCK_SAMPLES)
        .zip(lsb_counts.iter())
        .filter(|&(_, &lsb_count)| lsb_count > 0)
    {
        decode_block_lsbs(decoder, block, lsb_count)?;
    }
    for ((block, &pulse_count), _) in blocks
        .chunks_exact_mut(SHELL_BLOCK_SAMPLES)
        .zip(pulse_counts.iter())
        .zip(lsb_counts.iter())
        .filter(|&((_, &pulse_count), &lsb_count)| pulse_count > 0 || lsb_count > 0)
    {
        decode_block_signs(decoder, block, frame_type, pulse_count)?;
    }
    pulses[sample_count..].fill(0);

    construct_excitation_q14(frame);
    return Ok(());
}

/// Decodes the excitation rate level index (`0..=8`)
/// ([RFC 6716 Section 4.2.7.8.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8.1>)).
fn decode_rate_level(decoder: &mut entropy::Decoder<'_>, signal_type: SignalType) -> Result<u32> {
    let cdf: Cdf<'static> = match signal_type {
        SignalType::Voiced => constant::CDF_RATE_LEVEL_VOICED,
        SignalType::Unvoiced | SignalType::Inactive => constant::CDF_RATE_LEVEL_UNVOICED,
    };
    return decoder.decode_symbol(cdf);
}

/// Decodes the pulse count (`0..=16`) and LSB count (`0..=10`) of one 16-sample shell block
/// ([RFC 6716 Section 4.2.7.8.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8.2>)).
fn decode_block_pulse_count(
    decoder: &mut entropy::Decoder<'_>,
    rate_level: u32,
) -> Result<(u8, u8)> {
    let rate_row = (rate_level as usize).min(constant::CDF_PULSE_COUNT.len() - 1);
    let mut count = decoder.decode_symbol(constant::CDF_PULSE_COUNT[rate_row])?;
    let mut lsb_count = 0u8;

    while count == ESCAPE_PULSE_COUNT {
        lsb_count += 1;
        let row = if lsb_count == MAX_LSB_COUNT {
            PULSE_COUNT_CDF_LAST_ESCAPE
        } else {
            PULSE_COUNT_CDF_AFTER_ESCAPE
        };
        count = decoder.decode_symbol(constant::CDF_PULSE_COUNT[row])?;
    }

    return Ok((count as u8, lsb_count));
}

/// Whether every row of `rows` has `symbols` symbols.
const fn every_row_has(rows: &[Cdf<'static>], symbols: usize) -> bool {
    // A `const fn` cannot use iterators, so the rows are walked with an index.
    let mut r = 0;
    while r < rows.len() {
        if rows[r].symbols() != symbols {
            return false;
        }
        r += 1;
    }
    return true;
}

/// Whether row `r` of a shell split table offers the `r + 2` splits `0..=r + 1` of `r + 1` pulses,
/// so that the pulses left for the right half are never negative
/// ([RFC 6716 Section 4.2.7.8.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8.3>)).
const fn split_rows_are_complete(rows: &[Cdf<'static>]) -> bool {
    // A `const fn` cannot use iterators, so the rows are walked with an index.
    let mut r = 0;
    while r < rows.len() {
        if rows[r].symbols() != r + 2 {
            return false;
        }
        r += 1;
    }
    return true;
}

// A block carries at most 16 pulses after the escapes, every split table has a row for each
// count, and every row offers all splits: the shell decoding cannot index out of range or
// underflow, which is proven when the crate compiles.
const _: () = assert!(every_row_has(&constant::CDF_PULSE_COUNT, ESCAPE_PULSE_COUNT as usize + 1));
const _: () =
    assert!(split_rows_are_complete(&constant::CDF_PULSE_COUNT_SPLIT_16_SAMPLE_PARTITIONS));
const _: () =
    assert!(split_rows_are_complete(&constant::CDF_PULSE_COUNT_SPLIT_8_SAMPLE_PARTITIONS));
const _: () =
    assert!(split_rows_are_complete(&constant::CDF_PULSE_COUNT_SPLIT_4_SAMPLE_PARTITIONS));
const _: () =
    assert!(split_rows_are_complete(&constant::CDF_PULSE_COUNT_SPLIT_2_SAMPLE_PARTITIONS));
const _: () = assert!(
    constant::CDF_PULSE_COUNT_SPLIT_16_SAMPLE_PARTITIONS.len() == ESCAPE_PULSE_COUNT as usize - 1
);

/// Recursively splits `total_pulses` across `partition` in depth-first order
/// (`16 -> 8 -> 4 -> 2 -> 1`,
/// [RFC 6716 Section 4.2.7.8.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8.3>)).
fn decode_shell_partition(
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

    let row = (total_pulses as usize) - 1;
    let cdf: Cdf<'static> = match len {
        16 => constant::CDF_PULSE_COUNT_SPLIT_16_SAMPLE_PARTITIONS[row],
        8 => constant::CDF_PULSE_COUNT_SPLIT_8_SAMPLE_PARTITIONS[row],
        4 => constant::CDF_PULSE_COUNT_SPLIT_4_SAMPLE_PARTITIONS[row],
        2 => constant::CDF_PULSE_COUNT_SPLIT_2_SAMPLE_PARTITIONS[row],
        _ => return Err(Error::InvalidPartitionSize.into()),
    };

    let left_pulses = decoder.decode_symbol(cdf)? as u8;
    let right_pulses = total_pulses - left_pulses;

    let (left_half, right_half) = partition.split_at_mut(len >> 1);
    decode_shell_partition(decoder, left_half, left_pulses)?;
    decode_shell_partition(decoder, right_half, right_pulses)?;
    return Ok(());
}

/// Appends `lsb_count` least significant bits to every pulse magnitude of a shell block
/// ([RFC 6716 Section 4.2.7.8.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8.4>)).
fn decode_block_lsbs(
    decoder: &mut entropy::Decoder<'_>,
    block_pulses: &mut [i16],
    lsb_count: u8,
) -> Result<()> {
    for pulse in block_pulses.iter_mut() {
        for _ in 0..lsb_count {
            let bit = decoder.decode_symbol(constant::CDF_EXCITATION_LSB)? as i16;
            *pulse = (*pulse << 1) | bit;
        }
    }
    return Ok(());
}

/// Decodes the sign of every non-zero pulse of a shell block
/// ([RFC 6716 Section 4.2.7.8.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8.5>)).
fn decode_block_signs(
    decoder: &mut entropy::Decoder<'_>,
    block_pulses: &mut [i16],
    frame_type: FrameType,
    pulse_count: u8,
) -> Result<()> {
    let (signal_row, offset_column) = frame_type_table_position(frame_type);
    let pulse_row = (pulse_count as usize).min(SIGN_CDF_MAX_PULSE_COUNT);
    let cdf = constant::CDF_EXCITATION_SIGN[signal_row][offset_column][pulse_row];

    for pulse in block_pulses.iter_mut().filter(|pulse| **pulse > 0) {
        if decoder.decode_symbol(cdf)? == 0 {
            *pulse = -*pulse;
        }
    }
    return Ok(());
}

/// Constructs `frame.exc_q14` from `frame.pulses` and `frame.lcg_seed`, threading the LCG
/// state across all `frame.sample_count` samples of the SILK frame
/// ([RFC 6716 Section 4.2.7.8.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8.6>)).
fn construct_excitation_q14(frame: &mut Frame) {
    let offset_q14 = quantization_offset_q8(frame.frame_type) << OFFSET_Q8_TO_Q14_SHIFT;
    let sample_count = frame.sample_count;
    let mut seed = frame.lcg_seed as i32;

    let Frame { pulses, exc_q14, .. } = frame;
    for (&pulse, exc) in pulses[..sample_count].iter().zip(exc_q14[..sample_count].iter_mut()) {
        seed = advance_lcg_seed(seed);

        let pulse = pulse as i32;
        let pulse_q14 = pulse << PULSE_TO_Q14_SHIFT;
        let magnitude_q14 = match pulse_q14.cmp(&0) {
            std::cmp::Ordering::Greater => pulse_q14 - QUANT_LEVEL_ADJUST_Q14,
            std::cmp::Ordering::Less => pulse_q14 + QUANT_LEVEL_ADJUST_Q14,
            std::cmp::Ordering::Equal => 0,
        } + offset_q14;

        *exc = if seed < 0 { -magnitude_q14 } else { magnitude_q14 };
        seed = seed.wrapping_add(pulse);
    }
}

/// Row (signal type) and column (quantization offset type) of `frame_type` in the sign CDF
/// and quantization offset tables
/// ([RFC 6716 Section 4.2.7.8.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8.5>),
/// [Section 4.2.7.8.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8.6>)).
const fn frame_type_table_position(frame_type: FrameType) -> (usize, usize) {
    let signal_row = match frame_type.signal_type {
        SignalType::Inactive => 0,
        SignalType::Unvoiced => 1,
        SignalType::Voiced => 2,
    };
    let offset_column = match frame_type.quantization_offset_type {
        QuantizationOffsetType::Low => 0,
        QuantizationOffsetType::High => 1,
    };
    return (signal_row, offset_column);
}

/// Q8 quantization offset added to every excitation sample of `frame_type`
/// ([RFC 6716 Section 4.2.7.8.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8.6>)).
#[inline]
fn quantization_offset_q8(frame_type: FrameType) -> i32 {
    let (signal_row, offset_column) = frame_type_table_position(frame_type);
    return constant::QUANTIZATION_OFFSETS[signal_row][offset_column];
}

#[cfg(test)]
mod tests {
    // Fixtures: packets and expected values recorded from libopus 1.6.1. The behaviour they pin is
    // normative (RFC 6716 Section 4.2.7.8) and is verified against the RFC 8251 test vectors by
    // `tests/rfc6716_conformance.rs`.
    use super::*;
    use crate::silk::{QuantizationOffsetType, SignalType};

    #[test]
    fn excitation_shell_splits_lsb_escape_and_signs_match_recorded_values() {
        // Range-coded excitation covering:
        // - block 0: pulse_count = 0 with lsb_count = 1 (escape 17 -> 0, LSB-only pulses + signs)
        // - block 1: pulse_count = 4
        // - block 2: pulse_count = 0, lsb_count = 0 (all-zero block)
        // - block 3: pulse_count = 7 with lsb_count = 2 (double escape 17 -> 17 -> 7)
        // - block 4: pulse_count = 12 (dense shell split)
        const EXCITATION_TEST_BYTES: [u8; 25] = [
            0xa5, 0x77, 0x85, 0xab, 0xf8, 0xf2, 0x89, 0xdb, 0xac, 0xa9, 0x0e, 0xcf, 0xec, 0x43,
            0xbc, 0xb5, 0x5f, 0x16, 0x0f, 0x72, 0xaf, 0xa7, 0x7c, 0xc4, 0x80,
        ];
        const EXPECTED_DECODED_PULSES: [i16; 80] = [
            -1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, -1, 1, 0, 2, 0, 0, -1, 0, 0, 0, 0, 1, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 2, -3, 4, 1, -2, 3,
            0, -1, 2, 3, 0, 1, 2, -7, -2, 1, 0, 1, -1, 0, -2, 0, -1, 1, 0, 0, -1, 0, -1, 1,
        ];

        let mut dec = entropy::Decoder::new(&EXCITATION_TEST_BYTES);
        let mut frame = Frame::new(80, 2, 10);
        frame.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::High);

        decode_lcg_seed(&mut dec, &mut frame).unwrap();
        assert_eq!(frame.lcg_seed, 2);

        decode_excitation(&mut dec, &mut frame).unwrap();
        assert_eq!(dec.tell(), 193);
        assert_eq!(&frame.pulses[..80], &EXPECTED_DECODED_PULSES);
    }

    /// The shell coder splits partitions of 16, 8, 4 and 2 samples only, and a frame never holds
    /// more samples than one 20 ms SILK frame
    /// ([RFC 6716 Section 4.2.7.8.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.8.3>)).
    #[test]
    fn shell_partitions_and_frame_lengths_are_validated() {
        let mut dec = entropy::Decoder::new(&[0x55; 8]);
        let result = decode_shell_partition(&mut dec, &mut [0; 3], 1);
        assert!(Error::InvalidPartitionSize.matches(&result));

        let mut frame = Frame::new(320, 4, 16);
        frame.sample_count = MAX_FRAME_SAMPLES + SHELL_BLOCK_SAMPLES;
        let result = decode_excitation(&mut dec, &mut frame);
        assert!(Error::FrameLengthExceedsMaximum.matches(&result));
    }
}
