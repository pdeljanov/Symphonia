//! Opus Range Decoder (Entropy Coding)
//!
//! Opus uses an entropy coder based on range coding
//! [RANGE-CODING](https://datatracker.ietf.org/doc/html/rfc6716#ref-RANGE-CODING)
//! [MARTIN79](https://datatracker.ietf.org/doc/html/rfc6716#ref-MARTIN79),
//! which is itself a rediscovery of the FIFO arithmetic code
//! introduced by [CODING-THESIS](https://datatracker.ietf.org/doc/html/rfc6716#ref-CODING-THESIS).
//! It is very similar to arithmetic
//! encoding, except that encoding is done with digits in any base
//! instead of with bits, so it is faster when using larger bases (i.e.,
//! a byte). All of the calculations in the range coder must use bit-
//! exact integer arithmetic.
//!
//! Symbols may also be coded as "raw bits" packed directly into the
//! bitstream, bypassing the range coder. These are packed backwards
//! starting at the end of the frame, as illustrated in Figure 12. This
//! reduces complexity and makes the stream more resilient to bit errors,
//! as corruption in the raw bits will not desynchronize the decoding
//! process, unlike corruption in the input to the range decoder. Raw
//! bits are only used in the CELT layer.
//! ```text
//!      0                   1                   2                   3
//!      0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//!     +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//!     | Range coder data (packed MSB to LSB) ->                       :
//!     +                                                               +
//!     :                                                               :
//!     +     +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//!     :     | <- Boundary occurs at an arbitrary bit position         :
//!     +-+-+-+                                                         +
//!     :                          <- Raw bits data (packed LSB to MSB) |
//!     +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//!
//!     Legend:
//!
//!     LSB = Least Significant Bit
//!     MSB = Most Significant Bit
//!
//!          Figure 12: Illustrative Example of Packing Range Coder
//!                             and Raw Bits Data
//! ```
//!
//! <https://datatracker.ietf.org/doc/html/rfc6716#section-4.1>

use symphonia_core::errors::{Error, Result};

const BYTE_BITS: u32 = 8;
const INITIAL_BITS: u32 = 7;
const INITIAL_RANGE: u32 = 1 << INITIAL_BITS;
const INITIAL_VALUE_OFFSET: u32 = INITIAL_RANGE - 1;
const MIN_RANGE_SIZE: u32 = 1 << 23;
const MAX_RANGE_VALUE: u32 = 0x7FFF_FFFF;

const INITIAL_TOTAL_BITS: i32 = 9;
const RAW_BIT_WINDOW_SIZE: u32 = u32::BITS;
const MAX_RAW_BITS_PER_CALL: u32 = 25;
const UINT_RANGE_CODED_BITS: u32 = 8;
const FRACTIONAL_BIT_RESOLUTION: u32 = 3;

/// Computes the bit width of `value` (`0` when `value == 0`, or `1 + floor(log2(value))`).
#[inline]
const fn bit_width(value: u32) -> u32 {
    return u32::BITS - value.leading_zeros();
}

/// Bidirectional Opus entropy decoder over a single frame byte slice.
///
/// Range-coded symbols are decoded forward (MSB-first) from the start of `data`,
/// while raw bits (used by `decode_raw_bits` and `decode_uint`) are decoded backward
/// (LSB-first) from the end of `data`.
pub struct Decoder<'a> {
    data: &'a [u8],
    forward_bytes_read: usize,
    carry_byte: u8,
    backward_bytes_read: usize,
    raw_bit_window: u32,
    raw_bits_available: u32,
    total_bits_read: i32,
    range: u32,
    value: u32,
}

impl<'a> Decoder<'a> {
    /// Creates a new entropy decoder initialized over an Opus frame slice.
    ///
    /// Let `b0` be the first input byte (or `0` if `data` is empty). The decoder
    /// initializes `range` to `128` and `value` to `127 - (b0 >> 1)`, retaining
    /// the remaining bit `b0 & 1` in `carry_byte` for the immediate renormalization
    /// that establishes `range > 2^23` (RFC 6716 Section 4.1.1).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.1>
    pub fn new(data: &'a [u8]) -> Result<Self> {
        let mut decoder = Self {
            data,
            forward_bytes_read: 0,
            carry_byte: 0,
            backward_bytes_read: 0,
            raw_bit_window: 0,
            raw_bits_available: 0,
            total_bits_read: INITIAL_TOTAL_BITS,
            range: INITIAL_RANGE,
            value: 0,
        };

        let first_byte = decoder.read_next_byte();
        decoder.carry_byte = first_byte;
        decoder.value = INITIAL_VALUE_OFFSET - ((first_byte as u32) >> (BYTE_BITS - INITIAL_BITS));
        decoder.normalize();

        return Ok(decoder);
    }

    /// Returns the current active storage length in bytes (`ec_dec::storage`).
    #[inline]
    pub fn storage_bytes(&self) -> usize {
        return self.data.len();
    }

    /// Shrinks the active frame slice from the end by `bytes` (used when trailing
    /// CELT redundancy bytes are stripped before CELT raw-bit decoding).
    #[inline]
    pub fn shrink_storage(&mut self, bytes: usize) {
        let new_len = self.data.len().saturating_sub(bytes);
        self.data = &self.data[..new_len];
    }

    /// Returns the current internal range state (`ec_dec::rng`, used for `OPUS_GET_FINAL_RANGE`).
    #[inline]
    pub fn final_range(&self) -> u32 {
        return self.range;
    }

    /// Returns the number of whole bits consumed by decoded symbols and raw bits so far
    /// (RFC 6716 Section 4.1.2).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.2>
    #[inline]
    pub fn tell(&self) -> i32 {
        return self.total_bits_read - (bit_width(self.range) as i32);
    }

    /// Returns the fractional number of bits consumed so far in `1/8`-bit units (`Q3`)
    /// (RFC 6716 Section 4.1.2).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.2>
    pub fn tell_frac(&self) -> u32 {
        const THRESHOLDS_Q15: [u32; 8] = [35733, 38967, 42495, 46340, 50535, 55109, 60097, 65535];

        let total_bits_q3 = (self.total_bits_read as u32) << FRACTIONAL_BIT_RESOLUTION;
        let range_bits = bit_width(self.range);
        let normalized_range_q15 = self.range >> (range_bits - 16);

        let mut frac_octant = (normalized_range_q15 >> 12) - 8;
        if normalized_range_q15 > THRESHOLDS_Q15[frac_octant as usize] {
            frac_octant += 1;
        }

        let remaining_range_q3 = (range_bits << FRACTIONAL_BIT_RESOLUTION) + frac_octant;
        return total_bits_q3 - remaining_range_q3;
    }

    /// Decodes a single symbol from a cumulative distribution table `[total, fh_0, fh_1, ..., total]`
    /// and returns the 0-based symbol index (RFC 6716 Section 4.1.3.3).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.3.3>
    pub fn decode_symbol_with_icdf(&mut self, table: &[u32]) -> Result<u32> {
        let (&total, cdf) = table
            .split_first()
            .ok_or(Error::DecodeError("cumulative distribution table is empty"))?;
        if total == 0 {
            return Err(Error::DecodeError("cumulative distribution table total is zero"));
        }

        let scale = self.range / total;
        let scaled_value = self.value / scale;
        let target = total - (scaled_value + 1).min(total);

        let symbol_index = cdf
            .iter()
            .position(|&high| high > target)
            .ok_or(Error::DecodeError("invalid cumulative distribution table"))?;

        let low = symbol_index
            .checked_sub(1)
            .and_then(|prev| cdf.get(prev))
            .copied()
            .unwrap_or(0);
        let high = cdf[symbol_index];

        self.update_interval(scale, low, high, total);
        return Ok(symbol_index as u32);
    }

    /// Decodes a binary symbol where `logp` is the negative base-2 logarithm of the
    /// probability of a `1` symbol (`P(1) = 2^-logp`, RFC 6716 Section 4.1.3.2).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.3.2>
    pub fn decode_symbol_logp(&mut self, logp: u32) -> Result<u32> {
        let split = self.range >> logp;

        let bit = if self.value < split {
            self.range = split;
            1
        } else {
            self.value -= split;
            self.range -= split;
            0
        };
        self.normalize();

        return Ok(bit);
    }

    /// Decodes `bits` raw bits packed LSB-first backward from the end of the frame
    /// (RFC 6716 Section 4.1.3.1).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.3.1>
    pub fn decode_raw_bits(&mut self, bits: u32) -> Result<u32> {
        if bits > MAX_RAW_BITS_PER_CALL {
            return Err(Error::DecodeError("opus: raw bit count exceeds maximum of 25"));
        }
        if bits == 0 {
            return Ok(0);
        }

        while self.raw_bits_available < bits {
            while self.raw_bits_available <= RAW_BIT_WINDOW_SIZE - BYTE_BITS {
                let end_byte = self.read_byte_from_end() as u32;
                self.raw_bit_window |= end_byte << self.raw_bits_available;
                self.raw_bits_available += BYTE_BITS;
            }
        }

        let mask = (1u32 << bits) - 1;
        let result = self.raw_bit_window & mask;
        self.raw_bit_window >>= bits;
        self.raw_bits_available -= bits;
        self.total_bits_read += bits as i32;

        return Ok(result);
    }

    /// Decodes an equiprobable unsigned integer in `0..total` (`total >= 2`).
    ///
    /// When `total - 1` requires more than 8 bits, the top 8 bits are decoded via the
    /// range coder and the remaining low bits are read as raw bits from the end of the
    /// frame (RFC 6716 Section 4.1.3.2).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.3.2>
    pub fn decode_uint(&mut self, total: u32) -> Result<u32> {
        if total <= 1 {
            return Err(Error::DecodeError("opus: decode_uint requires total > 1"));
        }

        let max_symbol = total - 1;
        let required_bits = bit_width(max_symbol);

        if required_bits > UINT_RANGE_CODED_BITS {
            let raw_lsb_bits = required_bits - UINT_RANGE_CODED_BITS;
            let top_total = (max_symbol >> raw_lsb_bits) + 1;
            let top_symbol = self.decode_uniform_symbol(top_total);
            let low_bits = self.decode_raw_bits(raw_lsb_bits)?;
            let combined = (top_symbol << raw_lsb_bits) | low_bits;
            return Ok(combined.min(max_symbol));
        }

        return Ok(self.decode_uniform_symbol(total));
    }

    /// Decodes a Laplace-distributed signed integer used in CELT fine energy quantization
    /// (RFC 6716 Section 4.3.2.1).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.1>
    pub fn decode_laplace(&mut self, mut zero_freq: u32, decay: i32) -> i32 {
        const TOTAL_FREQ: u32 = 1 << 15;
        const MIN_PROB: u32 = 1;
        const MIN_GUARANTEED_DELTAS: u32 = 16;

        let scale = self.range >> 15;
        let scaled_value = self.value / scale;
        let fm = TOTAL_FREQ - (scaled_value + 1).min(TOTAL_FREQ);

        let mut decoded_value: i32 = 0;
        let mut low: u32 = 0;

        if fm >= zero_freq {
            decoded_value += 1;
            low = zero_freq;

            let remaining_freq = TOTAL_FREQ - MIN_PROB * (2 * MIN_GUARANTEED_DELTAS) - zero_freq;
            zero_freq = (((remaining_freq as i64 * (16384 - decay) as i64) >> 15) as u32) + MIN_PROB;

            while zero_freq > MIN_PROB && fm >= low + 2 * zero_freq {
                zero_freq *= 2;
                low += zero_freq;
                zero_freq = ((((zero_freq - 2 * MIN_PROB) as i64 * decay as i64) >> 15) as u32) + MIN_PROB;
                decoded_value += 1;
            }

            if zero_freq <= MIN_PROB {
                let tail_steps = ((fm - low) >> 1) as i32;
                decoded_value += tail_steps;
                low += (2 * tail_steps as u32) * MIN_PROB;
            }

            if fm < low + zero_freq {
                decoded_value = -decoded_value;
            } else {
                low += zero_freq;
            }
        }

        let high = (low + zero_freq).min(TOTAL_FREQ);
        self.update_interval(scale, low, high, TOTAL_FREQ);

        return decoded_value;
    }

    /// Decodes an equiprobable symbol in `0..total` using the range coder.
    #[inline]
    fn decode_uniform_symbol(&mut self, total: u32) -> u32 {
        let scale = self.range / total;
        let scaled_value = self.value / scale;
        let symbol = total - (scaled_value + 1).min(total);
        self.update_interval(scale, symbol, symbol + 1, total);
        return symbol;
    }

    /// Updates `(value, range)` for the frequency interval `[low, high)` out of `total`
    /// and renormalizes the range (RFC 6716 Section 4.1.2.1).
    #[inline]
    fn update_interval(&mut self, scale: u32, low: u32, high: u32, total: u32) {
        let top_offset = scale * (total - high);
        self.value -= top_offset;
        self.range = if low > 0 {
            scale * (high - low)
        } else {
            self.range - top_offset
        };
        self.normalize();
    }

    /// Renormalizes `(value, range)` by shifting in 8-bit windows until `range > 2^23`
    /// (RFC 6716 Section 4.1.2.1).
    ///
    /// <https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.2.1>
    #[inline]
    fn normalize(&mut self) {
        while self.range <= MIN_RANGE_SIZE {
            self.total_bits_read += BYTE_BITS as i32;
            self.range <<= BYTE_BITS;

            let next_byte = self.read_next_byte();
            let sym = (((self.carry_byte as u32) << BYTE_BITS) | (next_byte as u32))
                >> (BYTE_BITS - INITIAL_BITS);
            self.carry_byte = next_byte;

            self.value = ((self.value << BYTE_BITS) + (u8::MAX as u32 - (sym & 0xFF))) & MAX_RANGE_VALUE;
        }
    }

    #[inline]
    fn read_next_byte(&mut self) -> u8 {
        let byte = self.data.get(self.forward_bytes_read).copied().unwrap_or(0);
        self.forward_bytes_read = self.forward_bytes_read.saturating_add(1);
        return byte;
    }

    #[inline]
    fn read_byte_from_end(&mut self) -> u8 {
        let next_offset = self.backward_bytes_read.saturating_add(1);
        let byte = self
            .data
            .len()
            .checked_sub(next_offset)
            .and_then(|idx| self.data.get(idx))
            .copied()
            .unwrap_or(0);
        self.backward_bytes_read = next_offset;
        return byte;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SILK_MODEL_FRAME_TYPE_INACTIVE: [u32; 3] = [256, 26, 256];

    const SILK_MODEL_GAIN_HIGH_BITS: [[u32; 9]; 3] = [
        [256, 32, 144, 212, 241, 253, 254, 255, 256],
        [256, 2, 19, 64, 124, 186, 233, 252, 256],
        [256, 1, 4, 30, 101, 195, 245, 254, 256],
    ];

    const SILK_MODEL_GAIN_LOW_BITS: [u32; 9] = [256, 32, 64, 96, 128, 160, 192, 224, 256];

    const SILK_MODEL_GAIN_DELTA: [u32; 42] = [
        256, 6, 11, 22, 53, 185, 206, 214, 218, 221, 223, 225, 227, 228,
        229, 230, 231, 232, 233, 234, 235, 236, 237, 238, 239, 240, 241, 242,
        243, 244, 245, 246, 247, 248, 249, 250, 251, 252, 253, 254, 255, 256,
    ];

    static SILK_MODEL_LSF_S1: [[[u32; 33]; 2]; 2] = [
        [
            [
                256, 44, 78, 108, 127, 148, 160, 171, 174, 177, 179, 195, 197, 199, 200, 205, 207,
                208, 211, 214, 215, 216, 218, 220, 222, 225, 226, 235, 244, 246, 253, 255, 256,
            ],
            [
                256, 1, 11, 12, 20, 23, 31, 39, 53, 66, 80, 81, 95, 107, 120, 131, 142, 154, 165,
                175, 185, 196, 204, 213, 221, 228, 236, 237, 238, 244, 245, 251, 256,
            ],
        ],
        [
            [
                256, 31, 52, 55, 72, 73, 81, 98, 102, 103, 121, 137, 141, 143, 146, 147, 157, 158,
                161, 177, 188, 204, 206, 208, 211, 213, 224, 225, 229, 238, 246, 253, 256,
            ],
            [
                256, 1, 5, 21, 26, 44, 55, 60, 74, 89, 90, 93, 105, 118, 132, 146, 152, 166, 178,
                180, 186, 187, 199, 211, 222, 232, 235, 245, 250, 251, 252, 253, 256,
            ],
        ],
    ];

    static SILK_MODEL_LSF_S2: [[u32; 10]; 16] = [
        [256, 1, 2, 3, 18, 242, 253, 254, 255, 256],
        [256, 1, 2, 4, 38, 221, 253, 254, 255, 256],
        [256, 1, 2, 6, 48, 197, 252, 254, 255, 256],
        [256, 1, 2, 10, 62, 185, 246, 254, 255, 256],
        [256, 1, 4, 20, 73, 174, 248, 254, 255, 256],
        [256, 1, 4, 21, 76, 166, 239, 254, 255, 256],
        [256, 1, 8, 32, 85, 159, 226, 252, 255, 256],
        [256, 1, 2, 20, 83, 161, 219, 249, 255, 256],
        [256, 1, 2, 3, 12, 244, 253, 254, 255, 256],
        [256, 1, 2, 4, 32, 218, 253, 254, 255, 256],
        [256, 1, 2, 5, 47, 199, 252, 254, 255, 256],
        [256, 1, 2, 12, 61, 187, 252, 254, 255, 256],
        [256, 1, 5, 24, 72, 172, 249, 254, 255, 256],
        [256, 1, 2, 16, 70, 170, 242, 254, 255, 256],
        [256, 1, 2, 17, 78, 165, 226, 251, 255, 256],
        [256, 1, 8, 29, 79, 156, 237, 254, 255, 256],
    ];

    const SILK_MODEL_LSF_INTERPOLATION_OFFSET: [u32; 6] = [256, 13, 35, 64, 75, 256];

    const SILK_MODEL_LCG_SEED: [u32; 5] = [256, 64, 128, 192, 256];

    const SILK_MODEL_EXC_RATE: [[u32; 10]; 2] = [
        [256, 15, 66, 78, 124, 169, 182, 215, 242, 256],
        [256, 33, 63, 99, 116, 150, 199, 217, 238, 256],
    ];

    static SILK_MODEL_PULSE_COUNT: [[u32; 19]; 11] = [
        [256, 131, 205, 230, 238, 241, 244, 245, 246, 247, 248, 249, 250, 251, 252, 253, 254, 255, 256],
        [256, 58, 151, 211, 234, 241, 244, 245, 246, 247, 248, 249, 250, 251, 252, 253, 254, 255, 256],
        [256, 43, 94, 140, 173, 197, 213, 224, 232, 238, 241, 244, 247, 249, 250, 251, 253, 254, 256],
        [256, 17, 69, 140, 197, 228, 240, 245, 246, 247, 248, 249, 250, 251, 252, 253, 254, 255, 256],
        [256, 6, 27, 68, 121, 170, 205, 226, 237, 243, 246, 248, 250, 251, 252, 253, 254, 255, 256],
        [256, 7, 21, 43, 71, 100, 128, 153, 173, 190, 203, 214, 223, 230, 235, 239, 243, 246, 256],
        [256, 2, 7, 21, 50, 92, 138, 179, 210, 229, 240, 246, 249, 251, 252, 253, 254, 255, 256],
        [256, 1, 3, 7, 17, 36, 65, 100, 137, 171, 199, 219, 233, 241, 246, 250, 252, 254, 256],
        [256, 1, 3, 5, 10, 19, 33, 53, 77, 104, 132, 158, 181, 201, 216, 227, 235, 241, 256],
        [256, 1, 2, 3, 9, 36, 94, 150, 189, 214, 228, 238, 244, 247, 250, 252, 253, 254, 256],
        [256, 2, 3, 9, 36, 94, 150, 189, 214, 228, 238, 244, 247, 250, 252, 253, 254, 255, 256],
    ];

    macro_rules! check {
        ($decoder: ident, $method:ident, $table: expr, $expected: expr) => {
            match $decoder.$method($table) {
                Ok(result) => assert_eq!(result, $expected, "Mismatch for table {:?}", stringify!($table)),
                Err(err) => panic!("Decoding table {:?}: {:?}", stringify!($table), err),
            }
        }
    }

    #[test]
    fn decoder() {
        let mut dec = Decoder::new(&[0x0b, 0xe4, 0xc1, 0x36, 0xec, 0xc5, 0x80]).unwrap();
        assert_eq!(dec.tell(), 1);
        assert_eq!(dec.tell_frac(), 8);

        check!(dec, decode_symbol_logp, 0x1, 0);
        check!(dec, decode_symbol_logp, 0x1, 0);
        check!(dec, decode_symbol_with_icdf, &SILK_MODEL_FRAME_TYPE_INACTIVE, 1);
        check!(dec, decode_symbol_with_icdf, &SILK_MODEL_GAIN_HIGH_BITS[0], 0);
        check!(dec, decode_symbol_with_icdf, &SILK_MODEL_GAIN_LOW_BITS, 6);
        check!(dec, decode_symbol_with_icdf, &SILK_MODEL_GAIN_DELTA, 0);
        check!(dec, decode_symbol_with_icdf, &SILK_MODEL_GAIN_DELTA, 3);
        check!(dec, decode_symbol_with_icdf, &SILK_MODEL_GAIN_DELTA, 4);
        check!(dec, decode_symbol_with_icdf, &SILK_MODEL_LSF_S1[1][0], 9);
        check!(dec, decode_symbol_with_icdf, &SILK_MODEL_LSF_S2[10], 5);
        check!(dec, decode_symbol_with_icdf, &SILK_MODEL_LSF_S2[9], 4);
        for _ in 0..14 {
            check!(dec, decode_symbol_with_icdf, &SILK_MODEL_LSF_S2[8], 4);
        }
        check!(dec, decode_symbol_with_icdf, &SILK_MODEL_LSF_INTERPOLATION_OFFSET, 4);
        check!(dec, decode_symbol_with_icdf, &SILK_MODEL_LCG_SEED, 2);
        check!(dec, decode_symbol_with_icdf, &SILK_MODEL_EXC_RATE[0], 0);
        for _ in 0..20 {
            check!(dec, decode_symbol_with_icdf, &SILK_MODEL_PULSE_COUNT[0], 0);
        }
    }

    #[test]
    fn final_byte_lsb_preserved_on_renormalization() {
        // 1-byte buffer [0x01]: b0 >> 1 == 0, b0 & 1 == 1.
        // During initial normalize(), the 1st renormalization shifts in ((b0 & 1) << 7) = 0x80
        // and two zero bytes, yielding value = 0x7FFF_FFFF - (128 << 16).
        let dec_lsb_one = Decoder::new(&[0x01]).unwrap();
        let dec_lsb_zero = Decoder::new(&[0x00]).unwrap();

        assert_ne!(dec_lsb_one.value, dec_lsb_zero.value);
        assert_eq!(dec_lsb_zero.value, 0x7FFF_FFFF);
        assert_eq!(dec_lsb_one.value, 0x7FFF_FFFF - (128 << 16));
    }

    #[test]
    fn backward_raw_bits_reading() {
        let buf = [0x00, 0x00, 0x00, 0x00, 0b1011_0101, 0b1100_1010];
        let mut dec = Decoder::new(&buf).unwrap();
        assert_eq!(dec.tell(), 1);

        // From last byte 0b1100_1010 (0xCA): low 4 bits = 0b1010 (10), next 4 bits = 0b1100 (12)
        // From second-to-last byte 0b1011_0101 (0xB5): all 8 bits = 0xB5 (181)
        assert_eq!(dec.decode_raw_bits(4).unwrap(), 0b1010);
        assert_eq!(dec.tell(), 5);
        assert_eq!(dec.decode_raw_bits(4).unwrap(), 0b1100);
        assert_eq!(dec.tell(), 9);
        assert_eq!(dec.decode_raw_bits(8).unwrap(), 0xB5);
        assert_eq!(dec.tell(), 17);
    }

    #[test]
    fn libopus_ec_dec_uint_laplace_and_raw_bits_oracle() {
        // Generated by libopus ec_enc and verified against ec_dec C oracle:
        // u1 = ec_dec_uint(100) -> 42 (tell_frac = 62)
        // u2 = ec_dec_uint(50000) -> 12345 (tell_frac = 187)
        // lap = ec_laplace_decode(16000, 6000) x 3 -> [-5, 0, 7]
        // rb = ec_dec_bits(5) -> 0x15 (tell = 50, tell_frac = 393)
        const ENTROPY_ORACLE_BYTES: [u8; 12] = [
            0x6c, 0x28, 0xef, 0x33, 0xd0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x15, 0x39,
        ];

        let mut dec = Decoder::new(&ENTROPY_ORACLE_BYTES).unwrap();
        assert_eq!(dec.decode_uint(100).unwrap(), 42);
        assert_eq!(dec.tell_frac(), 62);

        assert_eq!(dec.decode_uint(50_000).unwrap(), 12_345);
        assert_eq!(dec.tell_frac(), 187);

        assert_eq!(dec.decode_laplace(16_000, 6_000), -5);
        assert_eq!(dec.decode_laplace(16_000, 6_000), 0);
        assert_eq!(dec.decode_laplace(16_000, 6_000), 7);

        assert_eq!(dec.decode_raw_bits(5).unwrap(), 0x15);
        assert_eq!(dec.tell(), 50);
        assert_eq!(dec.tell_frac(), 393);
        assert_eq!(dec.storage_bytes(), 12);
        assert!(dec.final_range() > MIN_RANGE_SIZE);

        // Shrinking storage by 2 bytes redirects subsequent backward raw-bit reads
        // to the new end of the active slice (used when stripping trailing CELT redundancy).
        let mut shrink_dec = Decoder::new(&[0x00, 0x00, 0x00, 0xA5, 0xFF, 0xFF]).unwrap();
        assert_eq!(shrink_dec.storage_bytes(), 6);
        shrink_dec.shrink_storage(2);
        assert_eq!(shrink_dec.storage_bytes(), 4);
        assert_eq!(shrink_dec.decode_raw_bits(8).unwrap(), 0xA5);
    }
}

