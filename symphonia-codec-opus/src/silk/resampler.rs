//! SILK output resampler
//! ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
//!
//! Converts decoded SILK audio from the internal rate (`8`, `12` or `16` kHz) to the output
//! rate (`8`, `12`, `16`, `24` or `48` kHz) with delay-compensated allpass 2x upsampling,
//! 12-phase symmetric FIR interpolation, or AR2 and FIR downsampling. The input is processed in
//! batches of at most `RESAMPLER_MAX_BATCH_SIZE_MS` so that the stack buffers stay bounded
//! ([RFC 8251 Section 5](<https://datatracker.ietf.org/doc/html/rfc8251#section-5>)).

use symphonia_core::errors::Result;

use crate::fixed::{mul_add_q16_i16, mul_q16, mul_q16_i16, round_shift, saturate_i16};
use crate::silk::constant::resampler::{
    RESAMPLER_1_2_COEFS, RESAMPLER_1_3_COEFS, RESAMPLER_1_4_COEFS, RESAMPLER_1_6_COEFS,
    RESAMPLER_2_3_COEFS, RESAMPLER_3_4_COEFS, RESAMPLER_DELAY_MATRIX_DEC,
    RESAMPLER_DOWN_ORDER_FIR0, RESAMPLER_DOWN_ORDER_FIR1, RESAMPLER_DOWN_ORDER_FIR2,
    RESAMPLER_FRAC_FIR_12, RESAMPLER_MAX_BATCH_SIZE_MS, RESAMPLER_ORDER_FIR_12, RESAMPLER_UP2_HQ_0,
    RESAMPLER_UP2_HQ_1,
};
use crate::silk::error::Error;

/// Maximum `batch_size` for SILK decoder inputs (`16 kHz * 10 ms = 160` samples).
const MAX_DECODER_BATCH_SIZE: usize = 16 * RESAMPLER_MAX_BATCH_SIZE_MS;

/// Number of allpass sections in each of the two 2x upsampling cascades (RFC 6716 Section 4.2.9).
const ALLPASS_SECTIONS: usize = 3;

/// Fraction bits of the allpass upsampler state (RFC 6716 Section 4.2.9).
const UP2_STATE_Q10_SHIFT: u32 = 10;

/// Fraction bits of the AR2 output feeding the downsampling FIR (RFC 6716 Section 4.2.9).
const AR2_OUTPUT_Q8_SHIFT: u32 = 8;

/// Left shift of the AR2 output before the Q14 feedback products (RFC 6716 Section 4.2.9).
const AR2_FEEDBACK_SHIFT: u32 = 2;

/// Fraction bits of the Q6 downsampling FIR accumulator (RFC 6716 Section 4.2.9).
const DOWN_FIR_Q6_SHIFT: u32 = 6;

/// Fraction bits of the Q15 upsampling FIR accumulator (RFC 6716 Section 4.2.9).
const UP_FIR_Q15_SHIFT: u32 = 15;

/// Number of polyphase branches of the 2x upsampled interpolation FIR (RFC 6716 Section 4.2.9).
const UP_FIR_PHASES: i16 = 12;

/// Mask selecting the fractional part of a Q16 sample index.
const INDEX_FRAC_Q16_MASK: i32 = 0xFFFF;

/// Number of AR2 coefficients that precede the FIR taps in every downsampling table.
const AR2_COEFS: usize = 2;

/// The polyphase FIR of the downsampling stage
/// ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
///
/// Every coefficient table starts with the two AR2 coefficients followed by the FIR taps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DownFilter {
    /// `3:4` and `2:3` rates: an 18-tap FIR whose halves are read from one of `fractions`
    /// polyphase branches selected by the fractional sample position.
    Interpolated { fractions: usize, coefs: &'static [i16] },
    /// `1:2`, `1:3`, `1:4` and `1:6` rates: a symmetric FIR of `order` taps folded around its
    /// centre.
    Symmetric { order: usize, coefs: &'static [i16] },
}

impl DownFilter {
    /// Number of FIR taps, the history the filter keeps between calls.
    const fn order(self) -> usize {
        return match self {
            Self::Interpolated { .. } => RESAMPLER_DOWN_ORDER_FIR0,
            Self::Symmetric { order, .. } => order,
        };
    }

    /// Full coefficient table: AR2 coefficients followed by the FIR taps.
    const fn coefs(self) -> &'static [i16] {
        return match self {
            Self::Interpolated { coefs, .. } | Self::Symmetric { coefs, .. } => coefs,
        };
    }

    /// Q14 coefficients of the 2nd-order AR pre-filter.
    fn ar2_coefs_q14(self) -> &'static [i16] {
        return &self.coefs()[..AR2_COEFS];
    }

    /// Q16 FIR taps.
    fn fir_coefs(self) -> &'static [i16] {
        return &self.coefs()[AR2_COEFS..];
    }

    /// Interpolates the Q8 AR2 output `buf` (FIR history followed by the batch) at every Q16
    /// position `k * index_increment_q16` below the batch length into `out`, returning the
    /// number of samples written (RFC 6716 Section 4.2.9).
    fn interpolate(self, buf: &[i32], index_increment_q16: i32, out: &mut [i16]) -> usize {
        let max_index_q16 = ((buf.len() - self.order()) as i32) << 16;
        let positions_q16 = (0i32..)
            .map(|k| k * index_increment_q16)
            .take_while(|&index_q16| index_q16 < max_index_q16);

        let mut written = 0usize;
        for (out_sample, index_q16) in out.iter_mut().zip(positions_q16) {
            let window = &buf[(index_q16 >> 16) as usize..];
            let res_q6 = self.tap_sum_q6(window, index_q16 & INDEX_FRAC_Q16_MASK);
            *out_sample = saturate_i16(round_shift(res_q6, DOWN_FIR_Q6_SHIFT));
            written += 1;
        }
        return written;
    }

    /// Q6 FIR output at one position: `window` starts at the integer sample position and
    /// `frac_q16` is its fractional part (RFC 6716 Section 4.2.9).
    fn tap_sum_q6(self, window: &[i32], frac_q16: i32) -> i32 {
        let fir = self.fir_coefs();
        return match self {
            Self::Interpolated { fractions, .. } => {
                let half_order = RESAMPLER_DOWN_ORDER_FIR0 / 2;
                let phase = mul_q16_i16(frac_q16, fractions as i16) as usize;
                let rising = &fir[half_order * phase..half_order * (phase + 1)];
                let falling_start = half_order * (fractions - 1 - phase);
                let falling = &fir[falling_start..falling_start + half_order];

                window[..half_order]
                    .iter()
                    .zip(rising.iter())
                    .chain(
                        window[half_order..RESAMPLER_DOWN_ORDER_FIR0]
                            .iter()
                            .rev()
                            .zip(falling.iter()),
                    )
                    .fold(0i32, |acc, (&sample, &tap)| mul_add_q16_i16(acc, sample, tap))
            }
            Self::Symmetric { order, .. } => {
                let half_order = order / 2;
                window[..half_order]
                    .iter()
                    .zip(window[half_order..order].iter().rev())
                    .zip(fir.iter())
                    .fold(0i32, |acc, ((&early, &late), &tap)| {
                        mul_add_q16_i16(acc, early.wrapping_add(late), tap)
                    })
            }
        };
    }
}

/// Resampling algorithm selected for a given `(fs_hz_in, fs_hz_out)` pair
/// ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResamplerMode {
    /// Input and output sample rates are equal (`1:1` copy with delay compensation).
    #[default]
    Copy,
    /// Exact `2:1` upsampling (`8 -> 16` kHz or `12 -> 24` kHz) via two 3-stage allpass cascades.
    Up2Hq,
    /// Fractional upsampling (`8 -> 12/24/48` kHz, `12 -> 16/48` kHz, `16 -> 24/48` kHz)
    /// via the 2x allpass upsampler followed by 12-phase 8-tap polyphase FIR interpolation.
    IirFir,
    /// Fractional downsampling (`12 -> 8` kHz, `16 -> 8/12` kHz) via a 2nd-order AR filter
    /// followed by polyphase FIR interpolation.
    Down(DownFilter),
}

/// Per-channel SILK resampler state
/// ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
///
/// Uses fixed-size stack arrays with zero heap allocations.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ResamplerState {
    /// Allpass sections of the two upsampling cascades (Q10), or the AR2 state in its first
    /// two entries when downsampling.
    iir_state: [i32; 2 * ALLPASS_SECTIONS],
    /// History of the upsampling interpolation FIR.
    fir_state_up: [i16; RESAMPLER_ORDER_FIR_12],
    /// History of the downsampling FIR (Q8).
    fir_state_down: [i32; RESAMPLER_DOWN_ORDER_FIR2],
    /// Delay compensation line holding the last `input_delay` input samples.
    delay_line: [i16; 48],
    input_rate_khz: usize,
    output_rate_khz: usize,
    /// Input samples held back for delay compensation.
    input_delay: usize,
    /// Largest number of input samples processed per filter pass.
    batch_size: usize,
    /// Input samples per output sample in Q16.
    inv_ratio_q16: i32,
    mode: ResamplerMode,
}

impl Default for ResamplerState {
    fn default() -> Self {
        return Self::new();
    }
}

impl ResamplerState {
    /// Creates a zeroed, unconfigured resampler state.
    pub(crate) const fn new() -> Self {
        return Self {
            iir_state: [0; 2 * ALLPASS_SECTIONS],
            fir_state_up: [0; RESAMPLER_ORDER_FIR_12],
            fir_state_down: [0; RESAMPLER_DOWN_ORDER_FIR2],
            delay_line: [0; 48],
            input_rate_khz: 0,
            output_rate_khz: 0,
            input_delay: 0,
            batch_size: 0,
            inv_ratio_q16: 0,
            mode: ResamplerMode::Copy,
        };
    }

    /// Initializes the resampler for `(fs_hz_in, fs_hz_out)`: the resampling mode, the delay
    /// compensation and the filter coefficients of the rate pair
    /// ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
    pub(crate) fn init(&mut self, fs_hz_in: u32, fs_hz_out: u32) -> Result<()> {
        let input_rate = input_rate_index(fs_hz_in)?;
        let output_rate = output_rate_index(fs_hz_out)?;
        let (mode, up2x) = select_mode(fs_hz_in, fs_hz_out)?;

        let input_rate_khz = (fs_hz_in / 1000) as usize;
        *self = Self {
            input_rate_khz,
            output_rate_khz: (fs_hz_out / 1000) as usize,
            input_delay: RESAMPLER_DELAY_MATRIX_DEC[input_rate][output_rate] as usize,
            batch_size: input_rate_khz * RESAMPLER_MAX_BATCH_SIZE_MS,
            inv_ratio_q16: inverse_ratio_q16(fs_hz_in as i32, fs_hz_out as i32, up2x),
            mode,
            ..Self::new()
        };
        return Ok(());
    }

    /// Clears every filter history and the delay line while keeping the configured rates
    /// ([RFC 6716 Section 4.5.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.5.2>)).
    pub(crate) fn clear_history(&mut self) {
        self.iir_state = [0; 2 * ALLPASS_SECTIONS];
        self.fir_state_up = [0; RESAMPLER_ORDER_FIR_12];
        self.fir_state_down = [0; RESAMPLER_DOWN_ORDER_FIR2];
        self.delay_line = [0; 48];
    }

    /// Resamples `input` (at the input rate) into `out` (at the output rate) and returns the
    /// number of output samples written, after the delay compensation of
    /// [RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)
    /// (in-place buffer handling per [RFC 8251 Section 5](<https://datatracker.ietf.org/doc/html/rfc8251#section-5>)).
    pub(crate) fn resample(&mut self, out: &mut [i16], input: &[i16]) -> Result<usize> {
        let in_len = input.len();
        let input_rate_khz = self.input_rate_khz;
        if input_rate_khz == 0 || in_len < input_rate_khz || self.input_delay > input_rate_khz {
            return Err(Error::InvalidFrameSize.into());
        }

        let out_len = (in_len * self.output_rate_khz) / input_rate_khz;
        if out.len() < out_len {
            return Err(Error::BufferTooSmall.into());
        }

        let fresh = input_rate_khz - self.input_delay;
        self.delay_line[self.input_delay..input_rate_khz].copy_from_slice(&input[..fresh]);

        let delay_line = self.delay_line;
        let first_in = &delay_line[..input_rate_khz];
        let rest_in = &input[fresh..in_len - self.input_delay];
        let (first_out, rest_out) = out[..out_len].split_at_mut(self.output_rate_khz);

        match self.mode {
            ResamplerMode::Up2Hq => {
                up2_hq(&mut self.iir_state, first_out, first_in);
                up2_hq(&mut self.iir_state, rest_out, rest_in);
            }
            ResamplerMode::IirFir => {
                self.iir_fir(first_out, first_in);
                self.iir_fir(rest_out, rest_in);
            }
            ResamplerMode::Down(filter) => {
                self.down_fir(first_out, first_in, filter);
                self.down_fir(rest_out, rest_in, filter);
            }
            ResamplerMode::Copy => {
                first_out.copy_from_slice(first_in);
                rest_out.copy_from_slice(rest_in);
            }
        }

        self.delay_line[..self.input_delay].copy_from_slice(&input[in_len - self.input_delay..]);
        return Ok(out_len);
    }

    /// 2x allpass upsampling followed by 12-phase 8-tap FIR interpolation, the fractional
    /// upsampling stage ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
    fn iir_fir(&mut self, out: &mut [i16], input: &[i16]) {
        const ORDER: usize = RESAMPLER_ORDER_FIR_12;
        let mut buf = [0i16; 2 * MAX_DECODER_BATCH_SIZE + ORDER];
        buf[..ORDER].copy_from_slice(&self.fir_state_up);

        let mut written = 0usize;
        for chunk in input.chunks(self.batch_size) {
            let upsampled = 2 * chunk.len();
            up2_hq(&mut self.iir_state, &mut buf[ORDER..ORDER + upsampled], chunk);
            written += interpolate_fir_12(
                &buf[..ORDER + upsampled],
                self.inv_ratio_q16,
                &mut out[written..],
            );
            buf.copy_within(upsampled..upsampled + ORDER, 0);
        }

        self.fir_state_up.copy_from_slice(&buf[..ORDER]);
    }

    /// 2nd-order AR filter followed by polyphase FIR interpolation, the downsampling stage
    /// ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
    ///
    /// A single trailing sample left after a whole batch is not processed, as in the reference
    /// decoder of RFC 6716 Appendix A; the frame lengths of this decoder never produce one.
    fn down_fir(&mut self, out: &mut [i16], input: &[i16], filter: DownFilter) {
        let order = filter.order();
        let mut buf = [0i32; MAX_DECODER_BATCH_SIZE + RESAMPLER_DOWN_ORDER_FIR2];
        buf[..order].copy_from_slice(&self.fir_state_down[..order]);

        let trailing_single = input.len() > 1 && input.len() % self.batch_size == 1;
        let processed = &input[..input.len() - usize::from(trailing_single)];

        let mut written = 0usize;
        for chunk in processed.chunks(self.batch_size) {
            ar2(
                &mut self.iir_state[..AR2_COEFS],
                &mut buf[order..order + chunk.len()],
                chunk,
                filter.ar2_coefs_q14(),
            );
            written += filter.interpolate(
                &buf[..order + chunk.len()],
                self.inv_ratio_q16,
                &mut out[written..],
            );
            buf.copy_within(chunk.len()..chunk.len() + order, 0);
        }

        self.fir_state_down[..order].copy_from_slice(&buf[..order]);
    }
}

/// Row of the delay matrix for an input rate (RFC 6716 Section 4.2.9).
fn input_rate_index(fs_hz: u32) -> Result<usize> {
    return match fs_hz {
        8_000 => Ok(0),
        12_000 => Ok(1),
        16_000 => Ok(2),
        _ => Err(Error::UnsupportedConfig.into()),
    };
}

/// Column of the delay matrix for an output rate (RFC 6716 Section 4.2.9).
fn output_rate_index(fs_hz: u32) -> Result<usize> {
    return match fs_hz {
        8_000 => Ok(0),
        12_000 => Ok(1),
        16_000 => Ok(2),
        24_000 => Ok(3),
        48_000 => Ok(4),
        _ => Err(Error::UnsupportedConfig.into()),
    };
}

/// Selects the resampling mode of a rate pair and whether the FIR stage runs on the 2x
/// upsampled signal (RFC 6716 Section 4.2.9).
fn select_mode(fs_hz_in: u32, fs_hz_out: u32) -> Result<(ResamplerMode, u32)> {
    let down = |filter: DownFilter| (ResamplerMode::Down(filter), 0);
    let (mode, up2x) = match (fs_hz_in, fs_hz_out) {
        (in_hz, out_hz) if out_hz == in_hz => (ResamplerMode::Copy, 0),
        (in_hz, out_hz) if out_hz == in_hz * 2 => (ResamplerMode::Up2Hq, 0),
        (in_hz, out_hz) if out_hz > in_hz => (ResamplerMode::IirFir, 1),
        (in_hz, out_hz) if out_hz * 4 == in_hz * 3 => {
            down(DownFilter::Interpolated { fractions: 3, coefs: &RESAMPLER_3_4_COEFS })
        }
        (in_hz, out_hz) if out_hz * 3 == in_hz * 2 => {
            down(DownFilter::Interpolated { fractions: 2, coefs: &RESAMPLER_2_3_COEFS })
        }
        (in_hz, out_hz) if out_hz * 2 == in_hz => down(DownFilter::Symmetric {
            order: RESAMPLER_DOWN_ORDER_FIR1,
            coefs: &RESAMPLER_1_2_COEFS,
        }),
        (in_hz, out_hz) if out_hz * 3 == in_hz => down(DownFilter::Symmetric {
            order: RESAMPLER_DOWN_ORDER_FIR2,
            coefs: &RESAMPLER_1_3_COEFS,
        }),
        (in_hz, out_hz) if out_hz * 4 == in_hz => down(DownFilter::Symmetric {
            order: RESAMPLER_DOWN_ORDER_FIR2,
            coefs: &RESAMPLER_1_4_COEFS,
        }),
        (in_hz, out_hz) if out_hz * 6 == in_hz => down(DownFilter::Symmetric {
            order: RESAMPLER_DOWN_ORDER_FIR2,
            coefs: &RESAMPLER_1_6_COEFS,
        }),
        _ => return Err(Error::UnsupportedConfig.into()),
    };
    return Ok((mode, up2x));
}

/// Q16 ratio of (possibly 2x upsampled) input rate to output rate, rounded up until it covers
/// the output rate (RFC 6716 Section 4.2.9).
fn inverse_ratio_q16(fs_in: i32, fs_out: i32, up2x: u32) -> i32 {
    let mut inv_ratio_q16 = ((fs_in << (14 + up2x)) / fs_out) << 2;
    while mul_q16(inv_ratio_q16, fs_out) < (fs_in << up2x) {
        inv_ratio_q16 += 1;
    }
    return inv_ratio_q16;
}

/// One first-order allpass section: `feedback` maps the difference between the input and the
/// section state to the fed-back term (RFC 6716 Section 4.2.9).
#[inline]
fn allpass_section(state: &mut i32, input: i32, feedback: impl Fn(i32) -> i32) -> i32 {
    let difference = input.wrapping_sub(*state);
    let fed_back = feedback(difference);
    let output = state.wrapping_add(fed_back);
    *state = input.wrapping_add(fed_back);
    return output;
}

/// Three allpass sections in series with Q16 coefficients `coefs_q16`; the last coefficient is
/// stored minus one so its section adds the difference back (RFC 6716 Section 4.2.9).
#[inline]
fn allpass_cascade(state: &mut [i32], coefs_q16: &[i16; ALLPASS_SECTIONS], input: i32) -> i32 {
    let first = allpass_section(&mut state[0], input, |y| mul_q16_i16(y, coefs_q16[0]));
    let second = allpass_section(&mut state[1], first, |y| mul_q16_i16(y, coefs_q16[1]));
    return allpass_section(&mut state[2], second, |y| mul_add_q16_i16(y, y, coefs_q16[2]));
}

/// High-quality 2x allpass upsampler: the even output samples come from one cascade and the odd
/// ones from the other, both fed with the same Q10 input
/// ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
fn up2_hq(iir_state: &mut [i32; 2 * ALLPASS_SECTIONS], out: &mut [i16], input: &[i16]) {
    let (even_state, odd_state) = iir_state.split_at_mut(ALLPASS_SECTIONS);
    for (out_pair, &sample) in out.chunks_exact_mut(2).zip(input.iter()) {
        let in_q10 = (sample as i32) << UP2_STATE_Q10_SHIFT;
        let even_q10 = allpass_cascade(even_state, &RESAMPLER_UP2_HQ_0, in_q10);
        let odd_q10 = allpass_cascade(odd_state, &RESAMPLER_UP2_HQ_1, in_q10);
        out_pair[0] = saturate_i16(round_shift(even_q10, UP2_STATE_Q10_SHIFT));
        out_pair[1] = saturate_i16(round_shift(odd_q10, UP2_STATE_Q10_SHIFT));
    }
}

/// Interpolates the 2x upsampled signal `buf` (8 history samples followed by the batch) with
/// the 12-phase 4+4 tap FIR at every Q16 position below the batch length, returning the number
/// of samples written (RFC 6716 Section 4.2.9).
fn interpolate_fir_12(buf: &[i16], index_increment_q16: i32, out: &mut [i16]) -> usize {
    const ORDER: usize = RESAMPLER_ORDER_FIR_12;
    const HALF_ORDER: usize = ORDER / 2;
    let max_index_q16 = ((buf.len() - ORDER) as i32) << 16;
    let positions_q16 = (0i32..)
        .map(|k| k * index_increment_q16)
        .take_while(|&index_q16| index_q16 < max_index_q16);

    let mut written = 0usize;
    for (out_sample, index_q16) in out.iter_mut().zip(positions_q16) {
        let phase = mul_q16_i16(index_q16 & INDEX_FRAC_Q16_MASK, UP_FIR_PHASES) as usize;
        let offset = (index_q16 >> 16) as usize;
        let window = &buf[offset..offset + ORDER];
        let rising = &RESAMPLER_FRAC_FIR_12[phase];
        let falling = &RESAMPLER_FRAC_FIR_12[(UP_FIR_PHASES as usize) - 1 - phase];

        let res_q15 = window[..HALF_ORDER]
            .iter()
            .zip(rising.iter())
            .chain(window[HALF_ORDER..].iter().zip(falling.iter().rev()))
            .fold(0i32, |acc, (&sample, &tap)| acc.wrapping_add((sample as i32) * (tap as i32)));

        *out_sample = saturate_i16(round_shift(res_q15, UP_FIR_Q15_SHIFT));
        written += 1;
    }
    return written;
}

/// 2nd-order AR filter of the downsampling stage producing Q8 output
/// ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
fn ar2(state: &mut [i32], out_q8: &mut [i32], input: &[i16], a_q14: &[i16]) {
    for (out, &sample) in out_q8.iter_mut().zip(input.iter()) {
        let out32 = state[0].wrapping_add((sample as i32) << AR2_OUTPUT_Q8_SHIFT);
        *out = out32;
        let fed_back = out32 << AR2_FEEDBACK_SHIFT;
        state[0] = mul_add_q16_i16(state[1], fed_back, a_q14[0]);
        state[1] = mul_q16_i16(fed_back, a_q14[1]);
    }
}

#[cfg(test)]
mod tests {
    // Fixtures: packets and expected values recorded from libopus 1.6.1. The behaviour they pin is
    // normative (RFC 6716 Section 4.2.9) and is verified against the RFC 8251 test vectors by
    // `tests/rfc6716_conformance.rs`.
    use super::*;

    macro_rules! assert_resampler_tail {
        ($in_rate:expr, $out_rate:expr, $out_len:expr, $in_sig:expr, $expected_tail:expr) => {{
            let mut resampler = ResamplerState::default();
            resampler.init($in_rate, $out_rate).unwrap();
            let mut out_buf = [0i16; $out_len];
            resampler.resample(&mut out_buf, &$in_sig).unwrap();
            assert_eq!(&out_buf[$out_len - 16..], &$expected_tail);
        }};
    }

    #[test]
    fn resampler_modes_match_recorded_values() {
        const EXPECTED_RESAMP_8_TO_16_TAIL: [i16; 16] = [
            3819, 4356, 4647, 4591, 4133, 3279, 2094, 701, -745, -2085, -3176, -3920, -4275, -4262,
            -3957, -3473,
        ];
        const EXPECTED_RESAMP_16_TO_8_TAIL: [i16; 16] = [
            4383, 829, -468, -1852, -4765, -3335, 1948, 3713, 2445, 2516, 728, -4079, -4964, -1025,
            1123, 1766,
        ];
        const EXPECTED_RESAMP_16_TO_12_TAIL: [i16; 16] = [
            -2456, 1230, 3540, 3513, 2585, 2376, 2525, 1311, -1750, -4749, -5277, -3110, -368, 997,
            1279, 2097,
        ];

        let in_sig: [i16; 80] = std::array::from_fn(|i| {
            let v = 4000.0 * (2.0 * std::f64::consts::PI * i as f64 / 16.0).sin()
                + 1500.0 * (2.0 * std::f64::consts::PI * i as f64 / 7.0).cos();
            v as i16
        });

        assert_resampler_tail!(8000, 16000, 160, in_sig, EXPECTED_RESAMP_8_TO_16_TAIL);
        assert_resampler_tail!(16000, 8000, 40, in_sig, EXPECTED_RESAMP_16_TO_8_TAIL);
        assert_resampler_tail!(16000, 12000, 60, in_sig, EXPECTED_RESAMP_16_TO_12_TAIL);
    }

    /// Initialisation rejects a rate pair outside the SILK internal and output rates, and
    /// resampling rejects an unconfigured state and an output buffer shorter than the frame
    /// ([RFC 6716 Section 4.2.9](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.9>)).
    #[test]
    fn resampler_rejects_unsupported_rates_and_short_buffers() {
        let mut resampler = ResamplerState::default();
        assert!(Error::UnsupportedConfig.matches(&resampler.init(44_100, 48_000)));
        assert!(Error::UnsupportedConfig.matches(&resampler.init(16_000, 44_100)));
        assert!(Error::InvalidFrameSize.matches(&resampler.resample(&mut [0; 480], &[0; 160])));

        resampler.init(16_000, 48_000).unwrap();
        assert!(Error::BufferTooSmall.matches(&resampler.resample(&mut [0; 10], &[0; 160])));
    }
}
