//! SILK Polyphase IIR/FIR Resampler (RFC 6716 Section 4.2.9)
//!
//! Converts decoded SILK audio from the internal SILK sampling rate (`8`, `12`, or `16` kHz)
//! to the output sampling rate (`8`, `12`, `16`, `24`, or `48` kHz) using delay-compensated
//! allpass 2x upsampling, 12-phase symmetric FIR interpolation, or AR2 + FIR downsampling.

use symphonia_core::errors::Result;

use crate::silk::constant::resampler::{
    RESAMPLER_1_2_COEFS, RESAMPLER_1_3_COEFS, RESAMPLER_1_4_COEFS, RESAMPLER_1_6_COEFS,
    RESAMPLER_2_3_COEFS, RESAMPLER_3_4_COEFS, RESAMPLER_DELAY_MATRIX_DEC,
    RESAMPLER_DOWN_ORDER_FIR0, RESAMPLER_DOWN_ORDER_FIR1, RESAMPLER_DOWN_ORDER_FIR2,
    RESAMPLER_FRAC_FIR_12, RESAMPLER_MAX_BATCH_SIZE_MS, RESAMPLER_ORDER_FIR_12,
    RESAMPLER_UP2_HQ_0, RESAMPLER_UP2_HQ_1,
};
use crate::silk::error::Error;
use crate::silk::{mla_q16_i16, mul_q16, mul_q16_i16, rshift_round, sat16};

/// Maximum `batch_size` for SILK decoder inputs (`16 kHz * 10 ms = 160` samples).
const MAX_DECODER_BATCH_SIZE: usize = 16 * RESAMPLER_MAX_BATCH_SIZE_MS;

/// Resampling algorithm selected for a given `(fs_hz_in, fs_hz_out)` pair.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ResamplerMode {
    /// Input and output sample rates are equal (`1:1` copy with delay compensation).
    #[default]
    Copy,
    /// Exact `2:1` upsampling (`8 -> 16` kHz or `12 -> 24` kHz) via 3-stage allpass cascade.
    Up2Hq,
    /// Fractional upsampling (`8 -> 12/24/48` kHz, `12 -> 16/48` kHz, `16 -> 24/48` kHz)
    /// via 2x allpass upsampler followed by 12-phase 8-tap polyphase FIR interpolation.
    IirFir,
    /// Fractional downsampling (`12 -> 8` kHz, `16 -> 8/12` kHz) via 2nd-order AR filter
    /// followed by polyphase FIR interpolation.
    DownFir {
        fir_fracs: usize,
        fir_order: usize,
        coefs: &'static [i16],
    },
}

/// Per-channel SILK resampler state (RFC 6716 Section 4.2.9).
///
/// Uses fixed-size stack arrays with zero heap allocations.
#[derive(Debug, Clone, Copy)]
pub struct ResamplerState {
    pub s_iir: [i32; 6],
    pub s_fir_i16: [i16; RESAMPLER_ORDER_FIR_12],
    pub s_fir_i32: [i32; RESAMPLER_DOWN_ORDER_FIR2],
    pub delay_buf: [i16; 48],
    pub fs_in_khz: usize,
    pub fs_out_khz: usize,
    pub input_delay: usize,
    pub batch_size: usize,
    pub inv_ratio_q16: i32,
    pub mode: ResamplerMode,
}

impl Default for ResamplerState {
    fn default() -> Self {
        return Self::new();
    }
}

impl ResamplerState {
    /// Creates a zeroed resampler state.
    pub const fn new() -> Self {
        return Self {
            s_iir: [0; 6],
            s_fir_i16: [0; RESAMPLER_ORDER_FIR_12],
            s_fir_i32: [0; RESAMPLER_DOWN_ORDER_FIR2],
            delay_buf: [0; 48],
            fs_in_khz: 0,
            fs_out_khz: 0,
            input_delay: 0,
            batch_size: 0,
            inv_ratio_q16: 0,
            mode: ResamplerMode::Copy,
        };
    }

    /// Initializes or resets the decoder resampler for `(fs_hz_in, fs_hz_out)`.
    ///
    /// Matches `silk_resampler_init(S, Fs_Hz_in, Fs_Hz_out, 0)` in `silk/resampler.c`.
    pub fn init(&mut self, fs_hz_in: u32, fs_hz_out: u32) -> Result<()> {
        *self = Self::new();

        let in_rate_id = match fs_hz_in {
            8_000 => 0,
            12_000 => 1,
            16_000 => 2,
            _ => return Err(Error::UnsupportedConfig.into()),
        };
        let out_rate_id = match fs_hz_out {
            8_000 => 0,
            12_000 => 1,
            16_000 => 2,
            24_000 => 3,
            48_000 => 4,
            _ => return Err(Error::UnsupportedConfig.into()),
        };

        self.input_delay = RESAMPLER_DELAY_MATRIX_DEC[in_rate_id][out_rate_id] as usize;
        self.fs_in_khz = (fs_hz_in / 1000) as usize;
        self.fs_out_khz = (fs_hz_out / 1000) as usize;
        self.batch_size = self.fs_in_khz * RESAMPLER_MAX_BATCH_SIZE_MS;

        let (mode, up2x) = match (fs_hz_in, fs_hz_out) {
            (in_hz, out_hz) if out_hz == in_hz => (ResamplerMode::Copy, 0),
            (in_hz, out_hz) if out_hz == in_hz * 2 => (ResamplerMode::Up2Hq, 0),
            (in_hz, out_hz) if out_hz > in_hz => (ResamplerMode::IirFir, 1),
            (in_hz, out_hz) if out_hz * 4 == in_hz * 3 => (
                ResamplerMode::DownFir {
                    fir_fracs: 3,
                    fir_order: RESAMPLER_DOWN_ORDER_FIR0,
                    coefs: &RESAMPLER_3_4_COEFS,
                },
                0,
            ),
            (in_hz, out_hz) if out_hz * 3 == in_hz * 2 => (
                ResamplerMode::DownFir {
                    fir_fracs: 2,
                    fir_order: RESAMPLER_DOWN_ORDER_FIR0,
                    coefs: &RESAMPLER_2_3_COEFS,
                },
                0,
            ),
            (in_hz, out_hz) if out_hz * 2 == in_hz => (
                ResamplerMode::DownFir {
                    fir_fracs: 1,
                    fir_order: RESAMPLER_DOWN_ORDER_FIR1,
                    coefs: &RESAMPLER_1_2_COEFS,
                },
                0,
            ),
            (in_hz, out_hz) if out_hz * 3 == in_hz => (
                ResamplerMode::DownFir {
                    fir_fracs: 1,
                    fir_order: RESAMPLER_DOWN_ORDER_FIR2,
                    coefs: &RESAMPLER_1_3_COEFS,
                },
                0,
            ),
            (in_hz, out_hz) if out_hz * 4 == in_hz => (
                ResamplerMode::DownFir {
                    fir_fracs: 1,
                    fir_order: RESAMPLER_DOWN_ORDER_FIR2,
                    coefs: &RESAMPLER_1_4_COEFS,
                },
                0,
            ),
            (in_hz, out_hz) if out_hz * 6 == in_hz => (
                ResamplerMode::DownFir {
                    fir_fracs: 1,
                    fir_order: RESAMPLER_DOWN_ORDER_FIR2,
                    coefs: &RESAMPLER_1_6_COEFS,
                },
                0,
            ),
            _ => return Err(Error::UnsupportedConfig.into()),
        };
        self.mode = mode;

        let fs_in_i32 = fs_hz_in as i32;
        let fs_out_i32 = fs_hz_out as i32;
        let mut inv_ratio_q16 = ((fs_in_i32 << (14 + up2x)) / fs_out_i32) << 2;
        while mul_q16(inv_ratio_q16, fs_out_i32) < (fs_in_i32 << up2x) {
            inv_ratio_q16 += 1;
        }
        self.inv_ratio_q16 = inv_ratio_q16;

        return Ok(());
    }

    /// Resamples `input` (`in_len` samples at `fs_in_khz`) into `out` (at `fs_out_khz`).
    ///
    /// Returns the number of output samples written.
    /// Matches `silk_resampler` in `silk/resampler.c`.
    pub fn resample(&mut self, out: &mut [i16], input: &[i16]) -> Result<usize> {
        let in_len = input.len();
        if self.fs_in_khz == 0 || in_len < self.fs_in_khz || self.input_delay > self.fs_in_khz {
            return Err(Error::InvalidFrameSize.into());
        }

        let out_len = (in_len * self.fs_out_khz) / self.fs_in_khz;
        if out.len() < out_len {
            return Err(Error::BufferTooSmall.into());
        }

        let n_samples = self.fs_in_khz - self.input_delay;
        self.delay_buf[self.input_delay..self.fs_in_khz].copy_from_slice(&input[..n_samples]);

        let delay_chunk = self.delay_buf;
        let first_in = &delay_chunk[..self.fs_in_khz];
        let rest_len = in_len - self.fs_in_khz;
        let rest_in = &input[n_samples..n_samples + rest_len];

        match self.mode {
            ResamplerMode::Up2Hq => {
                Self::up2_hq(&mut self.s_iir, &mut out[..self.fs_out_khz], first_in);
                Self::up2_hq(&mut self.s_iir, &mut out[self.fs_out_khz..out_len], rest_in);
            }
            ResamplerMode::IirFir => {
                self.iir_fir(&mut out[..self.fs_out_khz], first_in);
                self.iir_fir(&mut out[self.fs_out_khz..out_len], rest_in);
            }
            ResamplerMode::DownFir {
                fir_fracs,
                fir_order,
                coefs,
            } => {
                self.down_fir(
                    &mut out[..self.fs_out_khz],
                    first_in,
                    fir_fracs,
                    fir_order,
                    coefs,
                );
                self.down_fir(
                    &mut out[self.fs_out_khz..out_len],
                    rest_in,
                    fir_fracs,
                    fir_order,
                    coefs,
                );
            }
            ResamplerMode::Copy => {
                out[..self.fs_in_khz].copy_from_slice(first_in);
                out[self.fs_out_khz..out_len].copy_from_slice(rest_in);
            }
        }

        self.delay_buf[..self.input_delay]
            .copy_from_slice(&input[in_len - self.input_delay..in_len]);

        return Ok(out_len);
    }

    /// High-quality 2x allpass upsampler (`silk_resampler_private_up2_HQ`).
    ///
    /// Internal state `s_iir` is in Q10 format.
    pub(crate) fn up2_hq(s_iir: &mut [i32; 6], out: &mut [i16], input: &[i16]) {
        for (i, &sample) in input.iter().enumerate() {
            let in32 = (sample as i32) << 10;

            // Even output sample all-pass cascade
            let y0 = in32.wrapping_sub(s_iir[0]);
            let x0 = mul_q16_i16(y0, RESAMPLER_UP2_HQ_0[0]);
            let out32_1 = s_iir[0].wrapping_add(x0);
            s_iir[0] = in32.wrapping_add(x0);

            let y1 = out32_1.wrapping_sub(s_iir[1]);
            let x1 = mul_q16_i16(y1, RESAMPLER_UP2_HQ_0[1]);
            let out32_2 = s_iir[1].wrapping_add(x1);
            s_iir[1] = out32_1.wrapping_add(x1);

            let y2 = out32_2.wrapping_sub(s_iir[2]);
            let x2 = mla_q16_i16(y2, y2, RESAMPLER_UP2_HQ_0[2]);
            let out32_even = s_iir[2].wrapping_add(x2);
            s_iir[2] = out32_2.wrapping_add(x2);

            out[2 * i] = sat16(rshift_round(out32_even, 10));

            // Odd output sample all-pass cascade
            let y3 = in32.wrapping_sub(s_iir[3]);
            let x3 = mul_q16_i16(y3, RESAMPLER_UP2_HQ_1[0]);
            let out32_3 = s_iir[3].wrapping_add(x3);
            s_iir[3] = in32.wrapping_add(x3);

            let y4 = out32_3.wrapping_sub(s_iir[4]);
            let x4 = mul_q16_i16(y4, RESAMPLER_UP2_HQ_1[1]);
            let out32_4 = s_iir[4].wrapping_add(x4);
            s_iir[4] = out32_3.wrapping_add(x4);

            let y5 = out32_4.wrapping_sub(s_iir[5]);
            let x5 = mla_q16_i16(y5, y5, RESAMPLER_UP2_HQ_1[2]);
            let out32_odd = s_iir[5].wrapping_add(x5);
            s_iir[5] = out32_4.wrapping_add(x5);

            out[2 * i + 1] = sat16(rshift_round(out32_odd, 10));
        }
    }

    /// 2x allpass upsampling followed by 12-phase 8-tap FIR interpolation
    /// (`silk_resampler_private_IIR_FIR`).
    fn iir_fir(&mut self, out: &mut [i16], mut input: &[i16]) {
        let mut buf = [0i16; 2 * MAX_DECODER_BATCH_SIZE + RESAMPLER_ORDER_FIR_12];
        buf[..RESAMPLER_ORDER_FIR_12].copy_from_slice(&self.s_fir_i16);

        let index_increment_q16 = self.inv_ratio_q16;
        let mut out_idx = 0usize;
        let mut n_samples_in;

        loop {
            n_samples_in = input.len().min(self.batch_size);

            Self::up2_hq(
                &mut self.s_iir,
                &mut buf[RESAMPLER_ORDER_FIR_12..RESAMPLER_ORDER_FIR_12 + 2 * n_samples_in],
                &input[..n_samples_in],
            );

            let max_index_q16 = (n_samples_in as i32) << 17;
            let mut index_q16 = 0i32;
            while index_q16 < max_index_q16 {
                let table_index = mul_q16_i16(index_q16 & 0xFFFF, 12) as usize;
                let buf_offset = (index_q16 >> 16) as usize;
                let b = &buf[buf_offset..buf_offset + 8];
                let c0 = &RESAMPLER_FRAC_FIR_12[table_index];
                let c1 = &RESAMPLER_FRAC_FIR_12[11 - table_index];

                let res_q15 = b[..4]
                    .iter()
                    .zip(c0.iter())
                    .chain(b[4..8].iter().zip(c1.iter().rev()))
                    .fold(0i32, |acc, (&s, &c)| {
                        acc.wrapping_add((s as i32) * (c as i32))
                    });

                out[out_idx] = sat16(rshift_round(res_q15, 15));
                out_idx += 1;
                index_q16 += index_increment_q16;
            }

            input = &input[n_samples_in..];
            if !input.is_empty() {
                buf.copy_within(2 * n_samples_in..2 * n_samples_in + RESAMPLER_ORDER_FIR_12, 0);
            } else {
                break;
            }
        }

        self.s_fir_i16
            .copy_from_slice(&buf[2 * n_samples_in..2 * n_samples_in + RESAMPLER_ORDER_FIR_12]);
    }

    /// 2nd-order AR filter (`silk_resampler_private_AR2`), producing Q8 output.
    fn ar2(s_iir: &mut [i32], out_q8: &mut [i32], input: &[i16], a_q14: &[i16]) {
        for (i, &sample) in input.iter().enumerate() {
            let out32 = s_iir[0].wrapping_add((sample as i32) << 8);
            out_q8[i] = out32;
            let out32_shifted = out32 << 2;
            s_iir[0] = mla_q16_i16(s_iir[1], out32_shifted, a_q14[0]);
            s_iir[1] = mul_q16_i16(out32_shifted, a_q14[1]);
        }
    }

    /// 2nd-order AR filter followed by polyphase FIR interpolation (`silk_resampler_private_down_FIR`).
    fn down_fir(
        &mut self,
        out: &mut [i16],
        mut input: &[i16],
        fir_fracs: usize,
        fir_order: usize,
        coefs: &[i16],
    ) {
        let mut buf = [0i32; MAX_DECODER_BATCH_SIZE + RESAMPLER_DOWN_ORDER_FIR2];
        buf[..fir_order].copy_from_slice(&self.s_fir_i32[..fir_order]);

        let fir_coefs = &coefs[2..];
        let index_increment_q16 = self.inv_ratio_q16;
        let mut out_idx = 0usize;
        let mut n_samples_in;

        loop {
            n_samples_in = input.len().min(self.batch_size);

            Self::ar2(
                &mut self.s_iir[..2],
                &mut buf[fir_order..fir_order + n_samples_in],
                &input[..n_samples_in],
                &coefs[..2],
            );

            let max_index_q16 = (n_samples_in as i32) << 16;
            let mut index_q16 = 0i32;

            match fir_order {
                RESAMPLER_DOWN_ORDER_FIR0 => {
                    let half_order = RESAMPLER_DOWN_ORDER_FIR0 / 2;
                    while index_q16 < max_index_q16 {
                        let buf_ptr = &buf[(index_q16 >> 16) as usize..];
                        let interpol_ind =
                            mul_q16_i16(index_q16 & 0xFFFF, fir_fracs as i16) as usize;

                        let c0 =
                            &fir_coefs[half_order * interpol_ind..half_order * (interpol_ind + 1)];
                        let c1_start = half_order * (fir_fracs - 1 - interpol_ind);
                        let c1 = &fir_coefs[c1_start..c1_start + half_order];

                        let res_q6 = buf_ptr[..half_order]
                            .iter()
                            .zip(c0.iter())
                            .chain(
                                buf_ptr[half_order..RESAMPLER_DOWN_ORDER_FIR0]
                                    .iter()
                                    .rev()
                                    .zip(c1.iter()),
                            )
                            .fold(0i32, |acc, (&s, &c)| mla_q16_i16(acc, s, c));

                        out[out_idx] = sat16(rshift_round(res_q6, 6));
                        out_idx += 1;
                        index_q16 += index_increment_q16;
                    }
                }
                RESAMPLER_DOWN_ORDER_FIR1 | RESAMPLER_DOWN_ORDER_FIR2 => {
                    let half_order = fir_order / 2;
                    while index_q16 < max_index_q16 {
                        let buf_ptr = &buf[(index_q16 >> 16) as usize..];
                        let res_q6 = (0..half_order).fold(0i32, |acc, i| {
                            mla_q16_i16(
                                acc,
                                buf_ptr[i].wrapping_add(buf_ptr[fir_order - 1 - i]),
                                fir_coefs[i],
                            )
                        });

                        out[out_idx] = sat16(rshift_round(res_q6, 6));
                        out_idx += 1;
                        index_q16 += index_increment_q16;
                    }
                }
                _ => unreachable!(),
            }

            input = &input[n_samples_in..];
            if input.len() > 1 {
                buf.copy_within(n_samples_in..n_samples_in + fir_order, 0);
            } else {
                break;
            }
        }

        self.s_fir_i32[..fir_order].copy_from_slice(&buf[n_samples_in..n_samples_in + fir_order]);
    }
}
