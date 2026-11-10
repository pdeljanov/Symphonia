//! Range decoder and raw-bit reader: the entropy decoder every Opus layer reads its symbols from
//! ([RFC 6716 Section 4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1>)).
//!
//! The range decoder keeps the state `(val, rng)` and reads the frame forward from its first
//! byte, in bit-exact integer arithmetic. Raw bits bypass the range decoder: the CELT layer reads
//! them backward from the last byte of the frame, so the two readers meet at an arbitrary bit
//! position and a corrupt raw bit cannot desynchronize the range decoder.
//!
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

use symphonia_core::errors::{Error, Result};

/// Bits per input byte, the step of one renormalization.
const BYTE_BITS: u32 = 8;
/// Bits of the first byte taken into `val` at initialization; the rest is carried over.
const INITIAL_BITS: u32 = 7;
/// Initial `rng` (RFC 6716 Section 4.1.1).
const INITIAL_RANGE: u32 = 1 << INITIAL_BITS;
/// `val` starts at `127 - (b0 >> 1)` for the first byte `b0` (RFC 6716 Section 4.1.1).
const INITIAL_VALUE_OFFSET: u32 = INITIAL_RANGE - 1;
/// Renormalization keeps `rng` above this bound (RFC 6716 Section 4.1.2.1).
const MIN_RANGE_SIZE: u32 = 1 << 23;
/// Mask that keeps `val` within 31 bits after each renormalization step.
const MAX_RANGE_VALUE: u32 = 0x7FFF_FFFF;

/// Starting value of the bit usage count, so that [`Decoder::tell`] reports 1 bit right after
/// initialization.
const INITIAL_TOTAL_BITS: i32 = 9;
/// Width of the window that buffers raw bits read from the end of the frame.
const RAW_BIT_WINDOW_SIZE: u32 = u32::BITS;
/// Most raw bits one [`Decoder::decode_raw_bits`] call may read.
const MAX_RAW_BITS_PER_CALL: u32 = 25;
/// High bits of a uniform integer that are range coded; the low bits are raw bits
/// (RFC 6716 Section 4.1.5).
const UINT_RANGE_CODED_BITS: u32 = 8;
/// Fractional bits of [`Decoder::tell_frac`], which counts in 1/8 bit units.
const FRACTIONAL_BIT_RESOLUTION: u32 = 3;
/// Width the range is normalised to before the fractional bit estimate (`tell_frac`).
const NORMALIZED_RANGE_BITS: u32 = 16;
/// Shift that maps the normalised range `[2^15, 2^16)` onto the eight octants `8..16`.
const OCTANT_SHIFT: u32 = 12;
/// Octant index of the lower bound `2^15` of the normalised range.
const FIRST_OCTANT: u32 = 8;

/// Computes the bit width of `value` (`0` when `value == 0`, or `1 + floor(log2(value))`).
#[inline]
const fn bit_width(value: u32) -> u32 {
    return u32::BITS - value.leading_zeros();
}

/// A cumulative frequency table for [`Decoder::decode_symbol`]
/// ([RFC 6716 Section 4.1.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.3.3>)).
///
/// The table stores the total frequency count `ft` followed by the cumulative upper bound
/// `fh[k]` of every symbol `k`, so the last entry equals the total again; symbol `k` covers
/// `[fl[k], fh[k])` with `fl[0] = 0` and `fl[k] = fh[k - 1]`:
///
/// ```text
///   index    0     1       2       ...   n
///   value    ft    fh[0]   fh[1]   ...   fh[n-1] == ft
///   symbol         0       1       ...   n-1
/// ```
///
/// A `Cdf` is validated once, when it is built (at compile time for the constant tables in
/// `silk::constant` and `celt::constant`), so decoding never re-checks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cdf<'a> {
    table: &'a [u32],
}

impl<'a> Cdf<'a> {
    /// Builds a table, or returns `None` when it is not a well-formed cumulative distribution:
    /// fewer than two entries, a zero total, a decreasing bound, or a last bound that differs
    /// from the total.
    pub(crate) const fn try_new(table: &'a [u32]) -> Option<Self> {
        if table.len() < 2 || table[0] == 0 || table[table.len() - 1] != table[0] {
            return None;
        }
        let mut i = 2;
        while i < table.len() {
            if table[i] < table[i - 1] || table[i] > table[0] {
                return None;
            }
            i += 1;
        }
        return Some(Self { table });
    }

    /// Builds a table from a constant, panicking at compile time if it is malformed.
    pub(crate) const fn new(table: &'a [u32]) -> Self {
        return match Self::try_new(table) {
            Some(cdf) => cdf,
            None => panic!("malformed cumulative distribution table"),
        };
    }

    /// The total frequency count `ft`.
    #[inline]
    pub(crate) const fn total(self) -> u32 {
        return self.table[0];
    }

    /// The number of symbols `n`, the length of every table a decoded symbol indexes.
    #[inline]
    pub(crate) const fn symbols(self) -> usize {
        return self.table.len() - 1;
    }

    /// The cumulative upper bounds `fh[0..n]`, one per symbol.
    #[inline]
    pub(crate) fn bounds(self) -> &'a [u32] {
        return &self.table[1..];
    }

    /// The raw `[ft, fh_0, ..., ft]` layout, for table invariant tests.
    #[cfg(test)]
    pub(crate) const fn as_slice(self) -> &'a [u32] {
        return self.table;
    }
}

/// Range decoder over one Opus frame, together with the raw-bit reader that shares its bytes
/// ([RFC 6716 Section 4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1>)).
///
/// Range-coded symbols are read forward from the first byte, most significant bit first; raw
/// bits are read backward from the last byte, least significant bit first. The range decoder
/// state is the RFC's two-tuple `(val, rng)`, held in `value` and `range`.
pub(crate) struct Decoder<'a> {
    /// The frame bytes; [`Decoder::shrink_storage`] may cut redundancy bytes from the end.
    data: &'a [u8],
    /// Bytes read from the front by the range decoder, including the zeros read past the end.
    forward_bytes_read: usize,
    /// The previous input byte, whose low bit is the high bit of the next renormalization symbol.
    carry_byte: u8,
    /// Bytes read from the end by the raw-bit reader.
    backward_bytes_read: usize,
    /// Raw bits read from the end and not yet returned, least significant bit first.
    raw_bit_window: u32,
    /// Number of valid bits in `raw_bit_window`.
    raw_bits_available: u32,
    /// Bits read so far by both readers, the basis of [`Decoder::tell`].
    total_bits_read: i32,
    /// The RFC's `rng`: the size of the current range.
    range: u32,
    /// The RFC's `val`: the high end of the current range minus the coded value, minus one.
    value: u32,
}

impl<'a> Decoder<'a> {
    /// Starts decoding a frame: `rng = 128` and `val = 127 - (b0 >> 1)` for the first byte `b0`
    /// (zero for an empty frame), then renormalizes with the remaining bit of `b0`
    /// ([RFC 6716 Section 4.1.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.1>)).
    pub(crate) fn new(data: &'a [u8]) -> Self {
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

        return decoder;
    }

    /// Number of bytes in the active frame slice; raw bits are read backward from its end
    /// ([RFC 6716 Section 4.1, Figure 12](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1>)).
    #[inline]
    pub(crate) fn storage_bytes(&self) -> usize {
        return self.data.len();
    }

    /// Size of the active frame slice in bits, the budget that [`Decoder::tell`] counts against
    /// ([RFC 6716 Section 4.1.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.6>)).
    #[inline]
    pub(crate) fn storage_bits(&self) -> i32 {
        return (self.data.len() as i32) * (BYTE_BITS as i32);
    }

    /// Drops `bytes` from the end of the frame, so that raw bits are read from before a
    /// redundant CELT frame stored at the end
    /// ([RFC 6716 Section 4.5.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.1>)).
    #[inline]
    pub(crate) fn shrink_storage(&mut self, bytes: usize) {
        let new_len = self.data.len().saturating_sub(bytes);
        self.data = &self.data[..new_len];
    }

    /// The final `rng` after the last decoded symbol. Every conformant decoder ends a frame in
    /// the same state, which makes it the bit-exactness check of
    /// [RFC 6716 Section 6](<https://datatracker.ietf.org/doc/html/rfc6716#section-6>).
    #[inline]
    pub(crate) fn final_range(&self) -> u32 {
        return self.range;
    }

    /// Whole bits used so far by range-coded symbols and raw bits together
    /// ([RFC 6716 Section 4.1.6.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.6.1>)).
    #[inline]
    pub(crate) fn tell(&self) -> i32 {
        return self.total_bits_read - (bit_width(self.range) as i32);
    }

    /// Bits used so far in 1/8 bit units (`Q3`), a finer count than [`Decoder::tell`]
    /// ([RFC 6716 Section 4.1.6.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.6.2>)).
    pub(crate) fn tell_frac(&self) -> u32 {
        const THRESHOLDS_Q15: [u32; 8] = [35733, 38967, 42495, 46340, 50535, 55109, 60097, 65535];

        let total_bits_q3 = (self.total_bits_read as u32) << FRACTIONAL_BIT_RESOLUTION;
        let range_bits = bit_width(self.range);
        let normalized_range_q15 = self.range >> (range_bits - NORMALIZED_RANGE_BITS);

        let mut frac_octant = (normalized_range_q15 >> OCTANT_SHIFT) - FIRST_OCTANT;
        if normalized_range_q15 > THRESHOLDS_Q15[frac_octant as usize] {
            frac_octant += 1;
        }

        let remaining_range_q3 = (range_bits << FRACTIONAL_BIT_RESOLUTION) + frac_octant;
        return total_bits_q3 - remaining_range_q3;
    }

    /// Decodes one symbol from `cdf` and returns its index `k`, the symbol whose interval
    /// `[fl[k], fh[k])` holds the decoded frequency `fs`, then narrows `(val, rng)` to it
    /// ([RFC 6716 Section 4.1.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.3.3>)).
    ///
    /// The table is validated when built, so the search always terminates on a bound: the last
    /// bound equals the total and the target is always below it.
    pub(crate) fn decode_symbol(&mut self, cdf: Cdf<'_>) -> Result<u32> {
        let bounds = cdf.bounds();
        let symbol = self.decode_with(cdf.total(), |frequency| {
            let i = bounds.iter().position(|&high| high > frequency).unwrap_or(bounds.len() - 1);
            let low = i.checked_sub(1).map_or(0, |prev| bounds[prev]);
            return (i as u32, low, bounds[i]);
        });
        return Ok(symbol);
    }

    /// Decodes one symbol of a distribution of total frequency `total` whose intervals `locate`
    /// computes: it receives the frequency `fs` the coded value falls on and returns the symbol
    /// with its interval `[fl, fh)`, which the decoder then consumes
    /// ([RFC 6716 Section 4.1.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.2>)).
    /// This lets a distribution with a closed-form inverse decode without a table.
    #[inline]
    pub(crate) fn decode_with<T>(
        &mut self,
        total: u32,
        locate: impl FnOnce(u32) -> (T, u32, u32),
    ) -> T {
        let scale = self.range / total;
        let frequency = total - (self.value / scale + 1).min(total);
        let (symbol, low, high) = locate(frequency);
        self.update_interval(scale, low, high, total);
        return symbol;
    }

    /// Decodes one binary symbol whose probability of being `1` is `2^-logp`
    /// ([RFC 6716 Section 4.1.3.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.3.2>)).
    pub(crate) fn decode_symbol_logp(&mut self, logp: u32) -> Result<u32> {
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

    /// Reads `bits` (at most 25) raw bits, which bypass the range decoder and are packed from the
    /// end of the frame toward the front, least significant bit first
    /// ([RFC 6716 Section 4.1.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.4>)).
    pub(crate) fn decode_raw_bits(&mut self, bits: u32) -> Result<u32> {
        if bits > MAX_RAW_BITS_PER_CALL {
            return Err(Error::DecodeError("opus: raw bit count exceeds maximum of 25"));
        }
        if bits == 0 {
            return Ok(0);
        }

        if self.raw_bits_available < bits {
            while self.raw_bits_available <= RAW_BIT_WINDOW_SIZE - BYTE_BITS {
                let end_byte = u32::from(self.read_byte_from_end());
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

    /// Decodes a uniformly distributed integer in `0..total` (`total >= 2`): up to 8 high bits
    /// with the range decoder and the remaining low bits as raw bits
    /// ([RFC 6716 Section 4.1.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.5>)).
    ///
    /// A combined value above `total - 1` means a corrupt bitstream; it is clamped here, as the
    /// normative decoder does, and the corruption surfaces as a bit budget overrun at the frame
    /// boundary rather than as an error at this call.
    pub(crate) fn decode_uint(&mut self, total: u32) -> Result<u32> {
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

    /// Decodes a Laplace-distributed signed integer, the coarse energy residual of the CELT
    /// layer, from the frequency of zero `zero_freq` (out of `2^15`) and the geometric `decay`
    /// ([RFC 6716 Section 4.3.2.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.1>)).
    pub(crate) fn decode_laplace(&mut self, mut zero_freq: u32, decay: i32) -> i32 {
        const TOTAL_FREQ_BITS: u32 = 15;
        const TOTAL_FREQ: u32 = 1 << TOTAL_FREQ_BITS;
        const ONE_Q15: i64 = 1 << TOTAL_FREQ_BITS;
        const MIN_PROB: u32 = 1;
        const MIN_GUARANTEED_DELTAS: u32 = 16;
        // Every value in the tail is reachable from both signs, so twice the guaranteed deltas
        // of minimum probability are reserved before the geometric decay starts.
        const RESERVED_TAIL_FREQ: u32 = MIN_PROB * 2 * MIN_GUARANTEED_DELTAS;

        let scale = self.range >> TOTAL_FREQ_BITS;
        let scaled_value = self.value / scale;
        let fm = TOTAL_FREQ - (scaled_value + 1).min(TOTAL_FREQ);

        let mut decoded_value: i32 = 0;
        let mut low: u32 = 0;

        if fm >= zero_freq {
            decoded_value += 1;
            low = zero_freq;

            let remaining_freq = TOTAL_FREQ - RESERVED_TAIL_FREQ - zero_freq;
            zero_freq = (((i64::from(remaining_freq) * (ONE_Q15 / 2 - i64::from(decay)))
                >> TOTAL_FREQ_BITS) as u32)
                + MIN_PROB;

            while zero_freq > MIN_PROB && fm >= low + 2 * zero_freq {
                zero_freq *= 2;
                low += zero_freq;
                zero_freq = (((i64::from(zero_freq - 2 * MIN_PROB) * i64::from(decay))
                    >> TOTAL_FREQ_BITS) as u32)
                    + MIN_PROB;
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

    /// Decodes a uniformly distributed symbol in `0..total` with the range decoder, the
    /// range-coded part of a uniform integer
    /// ([RFC 6716 Section 4.1.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.5>)).
    #[inline]
    fn decode_uniform_symbol(&mut self, total: u32) -> u32 {
        return self.decode_with(total, |symbol| (symbol, symbol, symbol + 1));
    }

    /// Narrows `(val, rng)` to the decoded interval `[low, high)` out of `total`, with
    /// `scale = rng / total`, then renormalizes
    /// ([RFC 6716 Section 4.1.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.2>)).
    #[inline]
    fn update_interval(&mut self, scale: u32, low: u32, high: u32, total: u32) {
        let top_offset = scale * (total - high);
        self.value -= top_offset;
        self.range = if low > 0 { scale * (high - low) } else { self.range - top_offset };
        self.normalize();
    }

    /// Renormalizes until `rng > 2^23`, shifting into `val` one symbol per step built from the
    /// bit carried over from the previous byte and the top 7 bits of the next byte
    /// ([RFC 6716 Section 4.1.2.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.2.1>)).
    #[inline]
    fn normalize(&mut self) {
        while self.range <= MIN_RANGE_SIZE {
            self.total_bits_read += BYTE_BITS as i32;
            self.range <<= BYTE_BITS;

            let next_byte = self.read_next_byte();
            let sym = (((self.carry_byte as u32) << BYTE_BITS) | (next_byte as u32))
                >> (BYTE_BITS - INITIAL_BITS);
            self.carry_byte = next_byte;

            self.value =
                ((self.value << BYTE_BITS) + (u8::MAX as u32 - (sym & 0xFF))) & MAX_RANGE_VALUE;
        }
    }

    /// Reads the next input byte for renormalization
    /// ([RFC 6716 Section 4.1.2.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.2.1>));
    /// past the end of the frame it returns zero.
    #[inline]
    fn read_next_byte(&mut self) -> u8 {
        let byte = self.data.get(self.forward_bytes_read).copied().unwrap_or(0);
        self.forward_bytes_read = self.forward_bytes_read.saturating_add(1);
        return byte;
    }

    /// Reads the next raw-bit byte, moving from the end of the frame toward the front
    /// ([RFC 6716 Section 4.1.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1.4>));
    /// once every byte has been read it returns zero.
    #[inline]
    fn read_byte_from_end(&mut self) -> u8 {
        let next_offset = self.backward_bytes_read.saturating_add(1);
        let byte = self
            .data
            .len()
            .checked_sub(next_offset)
            .and_then(|i| self.data.get(i))
            .copied()
            .unwrap_or(0);
        self.backward_bytes_read = next_offset;
        return byte;
    }
}

#[cfg(test)]
mod tests {
    // Fixtures: packets and expected values recorded from libopus 1.6.1. The behaviour they pin is
    // normative (RFC 6716 Section 4.1) and is verified against the RFC 8251 test vectors by
    // `tests/rfc6716_conformance.rs`.
    use super::*;
    use crate::silk::constant::{
        CDF_DELTA_QUANTIZATION_GAIN, CDF_FRAME_TYPE_VAD_INACTIVE,
        CDF_INDEPENDENT_QUANTIZATION_GAIN_LSB, CDF_INDEPENDENT_QUANTIZATION_GAIN_MSB_INACTIVE,
        CDF_LINEAR_CONGRUENTIAL_GENERATOR_SEED, CDF_NORMALIZED_LSF_INTERPOLATION_INDEX,
        CDF_NORMALIZED_LSF_STAGE_ONE_INDEX_WIDEBAND_UNVOICED, CDF_NORMALIZED_LSF_STAGE_TWO_INDEX,
        CDF_PULSE_COUNT, CDF_RATE_LEVEL_UNVOICED,
    };

    macro_rules! check {
        ($decoder: ident, $method:ident, $table: expr, $expected: expr) => {
            match $decoder.$method($table) {
                Ok(result) => {
                    assert_eq!(result, $expected, "Mismatch for table {:?}", stringify!($table))
                }
                Err(err) => panic!("Decoding table {:?}: {:?}", stringify!($table), err),
            }
        };
    }

    #[test]
    fn decoder() {
        let mut dec = Decoder::new(&[0x0b, 0xe4, 0xc1, 0x36, 0xec, 0xc5, 0x80]);
        assert_eq!(dec.tell(), 1);
        assert_eq!(dec.tell_frac(), 8);

        check!(dec, decode_symbol_logp, 0x1, 0);
        check!(dec, decode_symbol_logp, 0x1, 0);
        check!(dec, decode_symbol, CDF_FRAME_TYPE_VAD_INACTIVE, 1);
        check!(dec, decode_symbol, CDF_INDEPENDENT_QUANTIZATION_GAIN_MSB_INACTIVE, 0);
        check!(dec, decode_symbol, CDF_INDEPENDENT_QUANTIZATION_GAIN_LSB, 6);
        check!(dec, decode_symbol, CDF_DELTA_QUANTIZATION_GAIN, 0);
        check!(dec, decode_symbol, CDF_DELTA_QUANTIZATION_GAIN, 3);
        check!(dec, decode_symbol, CDF_DELTA_QUANTIZATION_GAIN, 4);
        check!(dec, decode_symbol, CDF_NORMALIZED_LSF_STAGE_ONE_INDEX_WIDEBAND_UNVOICED, 9);
        check!(dec, decode_symbol, CDF_NORMALIZED_LSF_STAGE_TWO_INDEX[10], 5);
        check!(dec, decode_symbol, CDF_NORMALIZED_LSF_STAGE_TWO_INDEX[9], 4);
        for _ in 0..14 {
            check!(dec, decode_symbol, CDF_NORMALIZED_LSF_STAGE_TWO_INDEX[8], 4);
        }
        check!(dec, decode_symbol, CDF_NORMALIZED_LSF_INTERPOLATION_INDEX, 4);
        check!(dec, decode_symbol, CDF_LINEAR_CONGRUENTIAL_GENERATOR_SEED, 2);
        check!(dec, decode_symbol, CDF_RATE_LEVEL_UNVOICED, 0);
        for _ in 0..20 {
            check!(dec, decode_symbol, CDF_PULSE_COUNT[0], 0);
        }
    }

    #[test]
    fn final_byte_lsb_preserved_on_renormalization() {
        // 1-byte buffer [0x01]: b0 >> 1 == 0, b0 & 1 == 1.
        // During initial normalize(), the 1st renormalization shifts in ((b0 & 1) << 7) = 0x80
        // and two zero bytes, yielding value = 0x7FFF_FFFF - (128 << 16).
        let dec_lsb_one = Decoder::new(&[0x01]);
        let dec_lsb_zero = Decoder::new(&[0x00]);

        assert_ne!(dec_lsb_one.value, dec_lsb_zero.value);
        assert_eq!(dec_lsb_zero.value, 0x7FFF_FFFF);
        assert_eq!(dec_lsb_one.value, 0x7FFF_FFFF - (128 << 16));
    }

    #[test]
    fn backward_raw_bits_reading() {
        let buf = [0x00, 0x00, 0x00, 0x00, 0b1011_0101, 0b1100_1010];
        let mut dec = Decoder::new(&buf);
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
    fn uint_laplace_and_raw_bits_match_recorded_values() {
        // Range-coded symbols and the values they decode to:
        // u1 = ec_dec_uint(100) -> 42 (tell_frac = 62)
        // u2 = ec_dec_uint(50000) -> 12345 (tell_frac = 187)
        // lap = ec_laplace_decode(16000, 6000) x 3 -> [-5, 0, 7]
        // rb = ec_dec_bits(5) -> 0x15 (tell = 50, tell_frac = 393)
        const ENTROPY_FIXTURE_BYTES: [u8; 12] =
            [0x6c, 0x28, 0xef, 0x33, 0xd0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x15, 0x39];

        let mut dec = Decoder::new(&ENTROPY_FIXTURE_BYTES);
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
        assert_eq!(dec.storage_bits(), 96);
        assert!(dec.final_range() > MIN_RANGE_SIZE);

        // Shrinking storage by 2 bytes redirects subsequent backward raw-bit reads
        // to the new end of the active slice (used when stripping trailing CELT redundancy).
        let mut shrink_dec = Decoder::new(&[0x00, 0x00, 0x00, 0xA5, 0xFF, 0xFF]);
        assert_eq!(shrink_dec.storage_bytes(), 6);
        shrink_dec.shrink_storage(2);
        assert_eq!(shrink_dec.storage_bytes(), 4);
        assert_eq!(shrink_dec.storage_bits(), 32);
        assert_eq!(shrink_dec.decode_raw_bits(8).unwrap(), 0xA5);
    }
}
