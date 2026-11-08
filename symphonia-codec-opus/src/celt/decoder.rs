//! CELT Frame Decoder State, Synthesis Orchestration, and Packet Loss Concealment
//! (RFC 6716 Sections 4.3.1-4.3.7).
//!
//! This module implements [`Decoder`], the top-level state machine for the CELT full-bandwidth
//! MDCT transform layer of the Opus codec. For each frame, [`Decoder::decode_frame_planar`]
//! coordinates:
//!
//! ```text
//!                      +-----------------------------------+
//!   Opus Range         | Global Flags (silence, postfilter,|
//!   Bitstream -------->| transient, intra_ener, TF, spread,|
//!   (entropy::Decoder) | dynalloc boosts, alloc_trim)      |
//!                      +-----------------+-----------------+
//!                                        |
//!                                        v
//!                      +-----------------------------------+
//!                      | Coarse Energy (Laplace + 2D ARMA) |
//!                      | + Dynamic Bit Allocation (rate)   |
//!                      | + Fine Energy + PVQ (vq)          |
//!                      | + Final Energy + Anti-Collapse    |
//!                      +-----------------+-----------------+
//!                                        |
//!                                        v
//!                      +-----------------------------------+
//!                      | Denormalize Bands -> IMDCT & TDAC |
//!                      | Overlap-Add -> Comb Post-Filter   |
//!                      | -> De-Emphasis & Decimation       |
//!                      +-----------------------------------+
//! ```
//!
//! When a frame is lost (`data` is `None` or empty, or via [`Decoder::conceal_frame_planar`]),
//! the decoder conceals the missing audio using either pitch-synchronized 24th-order LPC
//! excitation extrapolation (`plc_duration < 40`, `start_band == 0`, `!skip_plc`) or decayed
//! spectral LCG noise injection (`celt_decode_lost`, RFC 6716 Section 4.3.7).
//!
//! <https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>

#![allow(clippy::needless_return)]
#![allow(clippy::too_many_arguments)]

use symphonia_core::errors::{Error as SymphoniaError, Result};

use crate::celt::constant::{CELT_WINDOW, COMB_FILTER_GAINS, EBANDS};
use crate::celt::energy::{
    apply_loss_recovery_energy_safety, decode_intra_energy_flag, unquant_coarse_energy,
    unquant_energy_finalise, unquant_fine_energy,
};
use crate::celt::error::Error;
use crate::celt::mdct::{
    LPC_ORDER, celt_autocorr, celt_fir, celt_iir, celt_lpc, comb_filter, deemphasis_channel,
    denormalise_bands, mdct_backward_frame, plc_pitch_search,
};
use crate::celt::rate::{
    PostFilterParams, clt_compute_allocation, decode_band_boosts_and_trim, decode_postfilter,
    decode_spread_decision, init_caps, tf_decode,
};
use crate::celt::vq::{anti_collapse, quant_all_bands, renormalise_vector};
use crate::celt::{
    BITRES, COMBFILTER_MAXPERIOD, COMBFILTER_MINPERIOD, DECODE_BUFFER_SIZE, MAX_BANDS,
    MAX_CHANNELS, MAX_FRAME_SIZE, OVERLAP, SHORT_MDCT_SIZE, Spread, celt_lcg_rand,
    effective_ebands,
};
use crate::entropy;
use crate::toc::Bandwidth;

/// Canonical 2-byte all-ones silence payload used when a 1-byte `[0xFF]` silence frame is passed.
const SILENCE_FRAME_BYTES: [u8; 2] = [0xFF, 0xFF];

/// Initial and minimum log2 band energy floor (`-28.0` in 6 dB base-2 units).
const INITIAL_LOG_ENERGY: f32 = -28.0;

/// Maximum saturated PLC loss duration counter (`10_000` in 2.5 ms units).
const MAX_PLC_LOSS_DURATION: usize = 10_000;

/// Maximum PLC duration (in 2.5 ms units, `40 * 2.5 ms = 100 ms`) before transitioning from
/// periodic pitch-based LPC concealment to spectral noise concealment (`celt_decoder.c:724`).
const MAX_PERIODIC_PLC_DURATION: usize = 40;

/// Converts an output sample rate in Hz (`48_000, 24_000, 16_000, 12_000, 8_000`) into the
/// integer CELT decimation factor `48_000 / sample_rate`.
#[inline]
fn sample_rate_to_downsample(sample_rate: u32) -> Result<usize> {
    return match sample_rate {
        48_000 => Ok(1),
        24_000 => Ok(2),
        16_000 => Ok(3),
        12_000 => Ok(4),
        8_000 => Ok(6),
        _ => Err(SymphoniaError::DecodeError(
            "opus celt: unsupported output sample rate",
        )),
    };
}

/// Converts a 48 kHz CELT frame size (`120, 240, 480, 960`) into its block-size exponent `lm`
/// (`0..=3`, where `frame_size_48k = 120 << lm`).
#[inline]
fn frame_size_48k_to_lm(frame_size_48k: usize) -> Result<usize> {
    return match frame_size_48k {
        120 => Ok(0),
        240 => Ok(1),
        480 => Ok(2),
        960 => Ok(3),
        _ => Err(Error::InvalidMdctSize(frame_size_48k).into()),
    };
}

/// Applies the 5-tap recursive IIR pitch comb post-filter in-place on
/// `signal_buf[history_offset .. history_offset + n]` (`comb_filter(y, y, ...)` in `celt.c`,
/// RFC 6716 Section 4.3.6.3).
fn comb_filter_iir_in_place(
    signal_buf: &mut [f32],
    history_offset: usize,
    t0: i32,
    t1: i32,
    g0: f32,
    g1: f32,
    tapset0: usize,
    tapset1: usize,
    overlap: usize,
    n: usize,
) {
    if g0 == 0.0 && g1 == 0.0 {
        return;
    }

    let period0 = (t0.max(COMBFILTER_MINPERIOD as i32) as usize).min(COMBFILTER_MAXPERIOD);
    let period1 = (t1.max(COMBFILTER_MINPERIOD as i32) as usize).min(COMBFILTER_MAXPERIOD);
    debug_assert!(history_offset >= period0 + 2);
    debug_assert!(history_offset >= period1 + 2);

    let gains0 = COMB_FILTER_GAINS[tapset0.min(2)];
    let g00 = g0 * gains0[0];
    let g01 = g0 * gains0[1];
    let g02 = g0 * gains0[2];

    let gains1 = COMB_FILTER_GAINS[tapset1.min(2)];
    let g10 = g1 * gains1[0];
    let g11 = g1 * gains1[1];
    let g12 = g1 * gains1[2];

    let mut x1 = signal_buf[history_offset - period1 + 1];
    let mut x2 = signal_buf[history_offset - period1];
    let mut x3 = signal_buf[history_offset - period1 - 1];
    let mut x4 = signal_buf[history_offset - period1 - 2];

    let same_filter = g0 == g1 && period0 == period1 && tapset0 == tapset1;
    let xfade_len = if same_filter { 0 } else { overlap.min(n) };

    for (i, &w) in CELT_WINDOW.iter().enumerate().take(xfade_len) {
        let pos = history_offset + i;
        let x0 = signal_buf[pos - period1 + 2];
        let f = w * w;
        let one_minus_f = 1.0_f32 - f;
        let p0 = pos - period0;

        signal_buf[pos] = signal_buf[pos]
            + (one_minus_f * g00) * signal_buf[p0]
            + (one_minus_f * g01) * (signal_buf[p0 + 1] + signal_buf[p0 - 1])
            + (one_minus_f * g02) * (signal_buf[p0 + 2] + signal_buf[p0 - 2])
            + (f * g10) * x2
            + (f * g11) * (x1 + x3)
            + (f * g12) * (x0 + x4);

        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
    }

    if g1 == 0.0 {
        return;
    }

    for i in xfade_len..n {
        let pos = history_offset + i;
        let x0 = signal_buf[pos - period1 + 2];
        signal_buf[pos] = signal_buf[pos] + g10 * x2 + g11 * (x1 + x3) + g12 * (x0 + x4);
        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
    }

    return;
}

/// Frame-level CELT decoder state and synthesis engine (RFC 6716 Section 4.3).
#[derive(Clone)]
pub struct Decoder {
    /// Number of decoded output channels (`1` for mono, `2` for stereo).
    output_channels: usize,
    /// Number of coded channels in the current CELT bitstream (`1` or `2`).
    stream_channels: usize,
    /// Integer decimation factor `48_000 / sample_rate` (`1, 2, 3, 4, 6`).
    downsample: usize,
    /// First active Bark band index (`0` for CELT-only mode, `17` for Hybrid mode).
    start_band: usize,
    /// One past the last active Bark band index (`13, 15, 17, 19, 21`).
    end_band: usize,
    /// Custom in-band signalling flag (`false` for standard RFC 6716 Opus streams).
    signalling: bool,
    /// Whether stereo intensity phase inversion is disabled (`OPUS_SET_PHASE_INVERSION_DISABLED`).
    disable_inv: bool,
    /// Final range coder state (`OPUS_GET_FINAL_RANGE`) or LCG state after noise PLC.
    rng: u32,
    /// Current/previous frame decoded log2 band energies per channel (`oldBandE`).
    old_band_e: [[f32; MAX_BANDS]; MAX_CHANNELS],
    /// One-frame-delayed log2 band energies per channel for anti-collapse and PLC (`oldLogE`).
    old_log_e: [[f32; MAX_BANDS]; MAX_CHANNELS],
    /// Two-frame-delayed log2 band energies per channel for anti-collapse and PLC (`oldLogE2`).
    old_log_e2: [[f32; MAX_BANDS]; MAX_CHANNELS],
    /// Slowly-adapting background noise floor log2 band energies (`backgroundLogE`).
    background_log_e: [[f32; MAX_BANDS]; MAX_CHANNELS],
    /// 24th-order LPC coefficients per channel computed at the onset of packet loss.
    lpc: [[f32; LPC_ORDER]; MAX_CHANNELS],
    /// Per-channel synthesis buffer holding `DECODE_BUFFER_SIZE = 2048` history samples followed
    /// by `OVERLAP = 120` MDCT overlap-add tail samples.
    decode_mem: [[f32; DECODE_BUFFER_SIZE + OVERLAP]; MAX_CHANNELS],
    /// Per-channel first-order de-emphasis filter state (`preemph_memD`).
    preemph_mem: [f32; MAX_CHANNELS],
    /// Current frame comb post-filter pitch period in 48 kHz samples.
    postfilter_period: i32,
    /// Previous frame comb post-filter pitch period in 48 kHz samples.
    postfilter_period_old: i32,
    /// Current frame comb post-filter linear gain.
    postfilter_gain: f32,
    /// Previous frame comb post-filter linear gain.
    postfilter_gain_old: f32,
    /// Current frame comb post-filter tapset index (`0..=2`).
    postfilter_tapset: usize,
    /// Previous frame comb post-filter tapset index (`0..=2`).
    postfilter_tapset_old: usize,
    /// Cumulative consecutive packet loss duration in 2.5 ms units (`1 << lm`).
    loss_duration: usize,
    /// Cumulative PLC duration in 2.5 ms units (`1 << lm`).
    plc_duration: usize,
    /// Last pitch lag (`100..=720`) estimated by [`plc_pitch_search`] during periodic PLC.
    last_pitch_index: usize,
    /// Whether periodic pitch-based PLC should be skipped in favor of spectral noise PLC.
    skip_plc: bool,
    /// Whether the next MDCT synthesis frame must inverse-comb-filter and fold the overlap tail.
    prefilter_and_fold_next: bool,
    /// Whether the immediately preceding frame used periodic pitch-based LPC concealment.
    last_frame_periodic_plc: bool,
}

impl Decoder {
    /// Creates a new CELT [`Decoder`] configured for `sample_rate` (`8_000, 12_000, 16_000,
    /// 24_000, 48_000` Hz) and `output_channels` (`1` or `2`).
    pub fn new(sample_rate: u32, output_channels: usize) -> Result<Self> {
        if output_channels == 0 || output_channels > MAX_CHANNELS {
            return Err(Error::InvalidChannelCount(output_channels).into());
        }
        let downsample = sample_rate_to_downsample(sample_rate)?;

        return Ok(Self {
            output_channels,
            stream_channels: output_channels,
            downsample,
            start_band: 0,
            end_band: MAX_BANDS,
            signalling: false,
            disable_inv: false,
            rng: 0,
            old_band_e: [[0.0; MAX_BANDS]; MAX_CHANNELS],
            old_log_e: [[INITIAL_LOG_ENERGY; MAX_BANDS]; MAX_CHANNELS],
            old_log_e2: [[INITIAL_LOG_ENERGY; MAX_BANDS]; MAX_CHANNELS],
            background_log_e: [[0.0; MAX_BANDS]; MAX_CHANNELS],
            lpc: [[0.0; LPC_ORDER]; MAX_CHANNELS],
            decode_mem: [[0.0; DECODE_BUFFER_SIZE + OVERLAP]; MAX_CHANNELS],
            preemph_mem: [0.0; MAX_CHANNELS],
            postfilter_period: 0,
            postfilter_period_old: 0,
            postfilter_gain: 0.0,
            postfilter_gain_old: 0.0,
            postfilter_tapset: 0,
            postfilter_tapset_old: 0,
            loss_duration: 0,
            plc_duration: 0,
            last_pitch_index: 0,
            skip_plc: true,
            prefilter_and_fold_next: false,
            last_frame_periodic_plc: false,
        });
    }

    /// Resets all running synthesis, energy prediction, post-filter, and PLC state to initial
    /// defaults (`OPUS_RESET_STATE` in `celt_decoder.c:1790-1810`), preserving configuration
    /// parameters (`output_channels`, `stream_channels`, `downsample`, `start_band`, `end_band`,
    /// `signalling`, `disable_inv`).
    pub fn reset(&mut self) {
        self.rng = 0;
        self.old_band_e = [[0.0; MAX_BANDS]; MAX_CHANNELS];
        self.old_log_e = [[INITIAL_LOG_ENERGY; MAX_BANDS]; MAX_CHANNELS];
        self.old_log_e2 = [[INITIAL_LOG_ENERGY; MAX_BANDS]; MAX_CHANNELS];
        self.background_log_e = [[0.0; MAX_BANDS]; MAX_CHANNELS];
        self.lpc = [[0.0; LPC_ORDER]; MAX_CHANNELS];
        self.decode_mem = [[0.0; DECODE_BUFFER_SIZE + OVERLAP]; MAX_CHANNELS];
        self.preemph_mem = [0.0; MAX_CHANNELS];
        self.postfilter_period = 0;
        self.postfilter_period_old = 0;
        self.postfilter_gain = 0.0;
        self.postfilter_gain_old = 0.0;
        self.postfilter_tapset = 0;
        self.postfilter_tapset_old = 0;
        self.loss_duration = 0;
        self.plc_duration = 0;
        self.last_pitch_index = 0;
        self.skip_plc = true;
        self.prefilter_and_fold_next = false;
        self.last_frame_periodic_plc = false;
        return;
    }

    /// Updates the output sample rate (`48_000, 24_000, 16_000, 12_000, 8_000` Hz) and
    /// corresponding decimation factor.
    pub fn set_sample_rate(&mut self, sample_rate: u32) -> Result<()> {
        self.downsample = sample_rate_to_downsample(sample_rate)?;
        return Ok(());
    }

    /// Updates the decoded output channel count (`1` or `2`).
    pub fn set_output_channels(&mut self, output_channels: usize) -> Result<()> {
        if output_channels == 0 || output_channels > MAX_CHANNELS {
            return Err(Error::InvalidChannelCount(output_channels).into());
        }
        self.output_channels = output_channels;
        return Ok(());
    }

    /// Sets the starting Bark band index (`0` for CELT-only mode, `17` for Hybrid mode).
    pub fn set_start_band(&mut self, start_band: usize) -> Result<()> {
        if start_band >= MAX_BANDS {
            return Err(Error::InvalidBandRange {
                start: start_band,
                end: self.end_band,
            }
            .into());
        }
        self.start_band = start_band;
        return Ok(());
    }

    /// Sets the exclusive upper Bark band boundary (`1..=21`).
    pub fn set_end_band(&mut self, end_band: usize) -> Result<()> {
        if end_band == 0 || end_band > MAX_BANDS {
            return Err(Error::InvalidBandRange {
                start: self.start_band,
                end: end_band,
            }
            .into());
        }
        self.end_band = end_band;
        return Ok(());
    }

    /// Sets the exclusive upper Bark band boundary from an Opus [`Bandwidth`].
    #[inline]
    pub fn set_bandwidth(&mut self, bandwidth: Bandwidth) {
        self.end_band = effective_ebands(bandwidth);
        return;
    }

    /// Sets the number of coded channels in the current CELT bitstream (`1` or `2`).
    pub fn set_stream_channels(&mut self, channels: usize) -> Result<()> {
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(Error::InvalidChannelCount(channels).into());
        }
        self.stream_channels = channels;
        return Ok(());
    }

    /// Enables or disables stereo intensity phase inversion (`OPUS_SET_PHASE_INVERSION_DISABLED`).
    #[inline]
    pub fn set_phase_inversion_disabled(&mut self, disabled: bool) {
        self.disable_inv = disabled;
        return;
    }

    /// Requests that the next MDCT synthesis frame apply inverse comb pre-filtering and TDAC
    /// window folding to the overlap tail (`prefilter_and_fold` in `celt_decoder.c:576`).
    #[inline]
    pub fn set_next_prefilter_and_fold(&mut self, enabled: bool) {
        self.prefilter_and_fold_next = enabled;
        return;
    }

    /// Returns the final range coder state (`ec_dec::rng`) from the most recently decoded frame.
    #[inline]
    pub fn final_range(&self) -> u32 {
        return self.rng;
    }

    /// Decodes a single CELT frame of `frame_size_48k` samples (`120, 240, 480, 960` at 48 kHz)
    /// into planar `f32` output slices `out_ch0` and `out_ch1` (when `output_channels == 2`),
    /// returning the number of output samples written per channel (`frame_size_48k / downsample`).
    ///
    /// - If `external_dec` is `Some(dec)`, symbols are decoded from the shared range decoder
    ///   (used in Hybrid mode after SILK has decoded the low-frequency layer).
    /// - If `external_dec` is `None`, a fresh [`entropy::Decoder`] is initialized over `data`.
    /// - If `data` is `None` (and `external_dec` is `None`) or `data` is empty, delegates to
    ///   [`Self::conceal_frame_planar`].
    /// - When `accum` is `true`, de-emphasized CELT samples are added onto the existing contents
    ///   of `out_ch0` (and `out_ch1`) rather than overwriting them.
    pub fn decode_frame_planar(
        &mut self,
        data: Option<&[u8]>,
        external_dec: Option<&mut entropy::Decoder<'_>>,
        frame_size_48k: usize,
        out_ch0: &mut [f32],
        out_ch1: Option<&mut [f32]>,
        accum: bool,
    ) -> Result<usize> {
        let lm = frame_size_48k_to_lm(frame_size_48k)?;
        let n = frame_size_48k;
        let out_samples = n / self.downsample;
        self.validate_band_and_buffer_bounds(out_samples, out_ch0, &out_ch1)?;

        // Check for packet loss or 1-byte DTX/PLC frame (except 1-byte [0xFF] silence).
        let should_conceal = match (external_dec.is_none(), data) {
            (true, None) => true,
            (true, Some(bytes)) => bytes.is_empty() || (bytes.len() == 1 && bytes[0] != 0xFF),
            (false, Some(bytes)) => bytes.is_empty(),
            (false, None) => false,
        };
        if should_conceal {
            return self.conceal_frame_planar(frame_size_48k, out_ch0, out_ch1, accum);
        }

        match external_dec {
            Some(ext) => {
                return self.decode_frame_with_dec(
                    ext,
                    frame_size_48k,
                    lm,
                    out_samples,
                    out_ch0,
                    out_ch1,
                    accum,
                );
            }
            None => {
                let raw_bytes = data.unwrap_or(&SILENCE_FRAME_BYTES);
                let effective_bytes = if raw_bytes == [0xFF] {
                    &SILENCE_FRAME_BYTES[..]
                } else {
                    raw_bytes
                };
                let mut local_dec = entropy::Decoder::new(effective_bytes)?;
                return self.decode_frame_with_dec(
                    &mut local_dec,
                    frame_size_48k,
                    lm,
                    out_samples,
                    out_ch0,
                    out_ch1,
                    accum,
                );
            }
        }
    }

    /// Internal frame decoding helper operating on an initialized [`entropy::Decoder`].
    fn decode_frame_with_dec(
        &mut self,
        dec: &mut entropy::Decoder<'_>,
        frame_size_48k: usize,
        lm: usize,
        out_samples: usize,
        out_ch0: &mut [f32],
        out_ch1: Option<&mut [f32]>,
        accum: bool,
    ) -> Result<usize> {
        let n = frame_size_48k;
        let len = dec.storage_bytes();
        if len == 0 {
            return self.conceal_frame_planar(frame_size_48k, out_ch0, out_ch1, accum);
        }

        // Enable pitch-based PLC once two consecutive packets have been received (`celt_decoder.c:1297`).
        if self.loss_duration == 0 {
            self.skip_plc = false;
        }

        let c = self.stream_channels;
        let cc = self.output_channels;
        let start = self.start_band;
        let end = self.end_band;
        let eff_end = end.min(MAX_BANDS);
        let m = 1usize << lm;

        if c == 1 {
            for band in 0..MAX_BANDS {
                self.old_band_e[0][band] = self.old_band_e[0][band].max(self.old_band_e[1][band]);
            }
        }

        let total_bits = (len as i32) * 8;
        let tell = dec.tell();
        let silence = match tell {
            t if t >= total_bits => true,
            1 => dec.decode_symbol_logp(15)? != 0,
            _ => false,
        };

        let mut x_norm = [0.0_f32; MAX_CHANNELS * MAX_FRAME_SIZE];
        let mut postfilter = PostFilterParams::default();
        let mut is_transient = false;

        if silence {
            // Shift synthesis memory left by N samples and set band energies to -28.0.
            for ch in 0..cc {
                self.decode_mem[ch].copy_within(n..(DECODE_BUFFER_SIZE + OVERLAP), 0);
            }
            for ch in 0..c {
                self.old_band_e[ch].fill(INITIAL_LOG_ENERGY);
            }
        } else {
            postfilter = decode_postfilter(dec, start, total_bits)?;

            if lm > 0 && dec.tell() + 3 <= total_bits {
                is_transient = dec.decode_symbol_logp(3)? != 0;
            }

            let intra_ener = decode_intra_energy_flag(dec, total_bits)?;
            if !intra_ener && self.loss_duration != 0 {
                apply_loss_recovery_energy_safety(
                    start,
                    end,
                    lm,
                    self.loss_duration,
                    &mut self.old_band_e,
                    &self.old_log_e,
                    &self.old_log_e2,
                );
            }

            unquant_coarse_energy(
                dec,
                start,
                end,
                intra_ener,
                lm,
                c,
                &mut self.old_band_e[..c],
            )?;

            let tf_res = tf_decode(start, end, is_transient, lm, dec)?;
            let spread_symbol = decode_spread_decision(dec, total_bits)? as u32;
            let spread = Spread::from_symbol(spread_symbol);

            let cap = init_caps(lm, c);
            let (offsets, alloc_trim) =
                decode_band_boosts_and_trim(dec, start, end, lm, c, total_bits, &cap)?;

            let mut bits = (total_bits << BITRES) - (dec.tell_frac() as i32) - 1;
            let anti_collapse_rsv =
                if is_transient && lm >= 2 && bits >= (((lm as i32) + 2) << BITRES) {
                    1i32 << BITRES
                } else {
                    0
                };
            bits -= anti_collapse_rsv;

            let alloc = clt_compute_allocation(
                dec,
                start,
                end,
                &offsets,
                &cap,
                alloc_trim,
                bits,
                c,
                lm,
            )?;

            unquant_fine_energy(
                dec,
                start,
                end,
                c,
                &alloc.fine_quant,
                &mut self.old_band_e[..c],
            )?;

            for ch in 0..cc {
                self.decode_mem[ch].copy_within(n..(DECODE_BUFFER_SIZE + OVERLAP), 0);
            }

            let mut collapse_masks = [0u8; MAX_CHANNELS * MAX_BANDS];
            {
                let (x_ch0, x_ch1) = x_norm[..(2 * n)].split_at_mut(n);
                let y_opt = if c == 2 { Some(&mut x_ch1[..]) } else { None };
                quant_all_bands(
                    start,
                    end,
                    x_ch0,
                    y_opt,
                    &mut collapse_masks[..(c * MAX_BANDS)],
                    &alloc.pulses,
                    is_transient,
                    spread,
                    alloc.dual_stereo,
                    alloc.intensity,
                    &tf_res,
                    (total_bits << BITRES) - anti_collapse_rsv,
                    alloc.balance,
                    dec,
                    lm,
                    alloc.coded_bands,
                    &mut self.rng,
                )?;

                if self.disable_inv && c == 2 {
                    let inv_start = alloc.intensity.max(start);
                    for band in inv_start..end {
                        let bin_start = m * EBANDS[band];
                        let bin_end = m * EBANDS[band + 1];
                        if bin_end - bin_start > 1 {
                            x_ch1[bin_start..bin_end].copy_from_slice(&x_ch0[bin_start..bin_end]);
                        }
                    }
                }
            }

            let anti_collapse_on = if anti_collapse_rsv > 0 {
                dec.decode_raw_bits(1)? != 0
            } else {
                false
            };

            let bits_left = total_bits - dec.tell();
            unquant_energy_finalise(
                dec,
                start,
                end,
                c,
                &alloc.fine_quant,
                &alloc.fine_priority,
                bits_left,
                &mut self.old_band_e[..c],
            )?;

            if anti_collapse_on {
                anti_collapse(
                    &mut x_norm[..(c * n)],
                    &collapse_masks[..(c * MAX_BANDS)],
                    lm,
                    c,
                    n,
                    start,
                    end,
                    &self.old_band_e,
                    &self.old_log_e,
                    &self.old_log_e2,
                    &alloc.pulses,
                    self.rng,
                );
            }
        }

        if self.prefilter_and_fold_next {
            self.prefilter_and_fold(n);
        }

        self.synthesize_mdct(
            &x_norm[..(c * n)],
            n,
            lm,
            c,
            cc,
            start,
            eff_end,
            is_transient,
            silence,
        );

        self.apply_postfilter(n, lm, cc, postfilter);
        self.update_energy_history(c, start, end, m, is_transient);

        self.rng = dec.final_range();
        self.deemphasize_to_planar(n, out_ch0, out_ch1, accum);

        self.loss_duration = 0;
        self.plc_duration = 0;
        self.last_frame_periodic_plc = false;
        self.prefilter_and_fold_next = false;

        if !silence && dec.tell() > total_bits {
            return Err(SymphoniaError::DecodeError(
                "opus celt: range decoder overread frame bit budget",
            ));
        }

        return Ok(out_samples);
    }

    /// Conceals a lost CELT frame of `frame_size_48k` samples (`120, 240, 480, 960` at 48 kHz)
    /// into planar `f32` output slices `out_ch0` and `out_ch1` (when `output_channels == 2`),
    /// returning the number of output samples written per channel (`frame_size_48k / downsample`).
    ///
    /// Matches `celt_decode_lost` in `celt_decoder.c` (RFC 6716 Section 4.3.7):
    /// - Uses pitch-synchronized 24th-order LPC excitation extrapolation when `plc_duration < 40`,
    ///   `start_band == 0`, and `!skip_plc`.
    /// - Falls back to decayed spectral LCG noise injection when `plc_duration >= 40`,
    ///   `start_band != 0` (Hybrid mode), or `skip_plc` is `true`.
    pub fn conceal_frame_planar(
        &mut self,
        frame_size_48k: usize,
        out_ch0: &mut [f32],
        out_ch1: Option<&mut [f32]>,
        accum: bool,
    ) -> Result<usize> {
        let lm = frame_size_48k_to_lm(frame_size_48k)?;
        let n = frame_size_48k;
        let out_samples = n / self.downsample;
        self.validate_band_and_buffer_bounds(out_samples, out_ch0, &out_ch1)?;

        let noise_based =
            self.plc_duration >= MAX_PERIODIC_PLC_DURATION || self.start_band != 0 || self.skip_plc;

        if noise_based {
            self.conceal_noise_based(n, lm);
        } else {
            self.conceal_pitch_based(n);
        }

        let step = 1usize << lm;
        self.loss_duration = (self.loss_duration + step).min(MAX_PLC_LOSS_DURATION);
        self.plc_duration = (self.plc_duration + step).min(MAX_PLC_LOSS_DURATION);

        self.deemphasize_to_planar(n, out_ch0, out_ch1, accum);
        return Ok(out_samples);
    }

    /// Validates that `start_band < end_band <= MAX_BANDS` and that `out_ch0` (and `out_ch1` when
    /// `output_channels == 2`) have capacity for at least `out_samples` samples.
    #[inline]
    fn validate_band_and_buffer_bounds(
        &self,
        out_samples: usize,
        out_ch0: &[f32],
        out_ch1: &Option<&mut [f32]>,
    ) -> Result<()> {
        if self.start_band >= self.end_band || self.end_band > MAX_BANDS {
            return Err(Error::InvalidBandRange {
                start: self.start_band,
                end: self.end_band,
            }
            .into());
        }
        if out_ch0.len() < out_samples {
            return Err(Error::BufferTooSmall.into());
        }
        if self.output_channels == 2 {
            match out_ch1 {
                Some(ch1) if ch1.len() >= out_samples => {}
                _ => return Err(Error::BufferTooSmall.into()),
            }
        }
        return Ok(());
    }

    /// Applies the inverse comb pre-filter and TDAC window folding to the 120-sample overlap region
    /// at `DECODE_BUFFER_SIZE - n` so that time-domain periodic PLC blends seamlessly into the next
    /// MDCT-synthesized frame (`prefilter_and_fold` in `celt_decoder.c:576-621`).
    fn prefilter_and_fold(&mut self, n: usize) {
        let cc = self.output_channels;
        let history_offset = DECODE_BUFFER_SIZE - n;
        let mut etmp = [0.0_f32; OVERLAP];

        for ch in 0..cc {
            comb_filter(
                &mut etmp,
                &self.decode_mem[ch],
                history_offset,
                self.postfilter_period_old,
                self.postfilter_period,
                -self.postfilter_gain_old,
                -self.postfilter_gain,
                self.postfilter_tapset_old,
                self.postfilter_tapset,
                &CELT_WINDOW,
                0,
                OVERLAP,
            );

            let half_overlap = OVERLAP / 2;
            for i in 0..half_overlap {
                self.decode_mem[ch][history_offset + i] = CELT_WINDOW[i] * etmp[OVERLAP - 1 - i]
                    + CELT_WINDOW[OVERLAP - 1 - i] * etmp[i];
            }
        }
        return;
    }

    /// Denormalizes unit-energy PVQ bands `x_norm` and executes the inverse MDCT with windowing and
    /// overlap-add into `self.decode_mem` (`celt_synthesis` in `celt_decoder.c:413-511`).
    fn synthesize_mdct(
        &mut self,
        x_norm: &[f32],
        n: usize,
        lm: usize,
        c: usize,
        cc: usize,
        start: usize,
        eff_end: usize,
        is_transient: bool,
        silence: bool,
    ) {
        let out_offset = DECODE_BUFFER_SIZE - n;
        let mut freq0 = [0.0_f32; MAX_FRAME_SIZE];

        match (cc, c) {
            (2, 1) => {
                // Mono stream duplicated to stereo output channels.
                denormalise_bands(
                    &x_norm[..n],
                    &mut freq0[..n],
                    &self.old_band_e[0],
                    start,
                    eff_end,
                    lm,
                    self.downsample,
                    silence,
                );
                mdct_backward_frame(
                    &freq0[..n],
                    &mut self.decode_mem[0][out_offset..(out_offset + n + OVERLAP)],
                    lm,
                    is_transient,
                );
                mdct_backward_frame(
                    &freq0[..n],
                    &mut self.decode_mem[1][out_offset..(out_offset + n + OVERLAP)],
                    lm,
                    is_transient,
                );
            }
            (1, 2) => {
                // Stereo stream downmixed to mono output channel in the MDCT domain.
                let mut freq1 = [0.0_f32; MAX_FRAME_SIZE];
                denormalise_bands(
                    &x_norm[..n],
                    &mut freq0[..n],
                    &self.old_band_e[0],
                    start,
                    eff_end,
                    lm,
                    self.downsample,
                    silence,
                );
                denormalise_bands(
                    &x_norm[n..(2 * n)],
                    &mut freq1[..n],
                    &self.old_band_e[1],
                    start,
                    eff_end,
                    lm,
                    self.downsample,
                    silence,
                );
                for (f0, &f1) in freq0[..n].iter_mut().zip(freq1[..n].iter()) {
                    *f0 = 0.5_f32 * (*f0 + f1);
                }
                mdct_backward_frame(
                    &freq0[..n],
                    &mut self.decode_mem[0][out_offset..(out_offset + n + OVERLAP)],
                    lm,
                    is_transient,
                );
            }
            _ => {
                // Matched channel counts (mono-to-mono or stereo-to-stereo).
                for ch in 0..cc {
                    let ch_x = &x_norm[(ch * n)..((ch + 1) * n)];
                    denormalise_bands(
                        ch_x,
                        &mut freq0[..n],
                        &self.old_band_e[ch],
                        start,
                        eff_end,
                        lm,
                        self.downsample,
                        silence,
                    );
                    mdct_backward_frame(
                        &freq0[..n],
                        &mut self.decode_mem[ch][out_offset..(out_offset + n + OVERLAP)],
                        lm,
                        is_transient,
                    );
                }
            }
        }

        return;
    }

    /// Applies the 5-tap recursive pitch comb post-filter across the newly synthesized `n` samples
    /// in `self.decode_mem` and updates the post-filter delay state (`celt_decoder.c:1537-1560`).
    fn apply_postfilter(&mut self, n: usize, lm: usize, cc: usize, postfilter: PostFilterParams) {
        let out_offset = DECODE_BUFFER_SIZE - n;
        self.postfilter_period = self.postfilter_period.max(COMBFILTER_MINPERIOD as i32);
        self.postfilter_period_old = self.postfilter_period_old.max(COMBFILTER_MINPERIOD as i32);

        for ch in 0..cc {
            comb_filter_iir_in_place(
                &mut self.decode_mem[ch],
                out_offset,
                self.postfilter_period_old,
                self.postfilter_period,
                self.postfilter_gain_old,
                self.postfilter_gain,
                self.postfilter_tapset_old,
                self.postfilter_tapset,
                OVERLAP,
                SHORT_MDCT_SIZE,
            );
            if lm != 0 {
                comb_filter_iir_in_place(
                    &mut self.decode_mem[ch],
                    out_offset + SHORT_MDCT_SIZE,
                    self.postfilter_period,
                    postfilter.pitch as i32,
                    self.postfilter_gain,
                    postfilter.gain,
                    self.postfilter_tapset,
                    postfilter.tapset,
                    OVERLAP,
                    n - SHORT_MDCT_SIZE,
                );
            }
        }

        self.postfilter_period_old = self.postfilter_period;
        self.postfilter_gain_old = self.postfilter_gain;
        self.postfilter_tapset_old = self.postfilter_tapset;
        self.postfilter_period = postfilter.pitch as i32;
        self.postfilter_gain = postfilter.gain;
        self.postfilter_tapset = postfilter.tapset;
        if lm != 0 {
            self.postfilter_period_old = self.postfilter_period;
            self.postfilter_gain_old = self.postfilter_gain;
            self.postfilter_tapset_old = self.postfilter_tapset;
        }

        return;
    }

    /// Updates `old_band_e`, `old_log_e`, `old_log_e2`, and `background_log_e` at the end of a
    /// normal frame (`celt_decoder.c:1562-1592`).
    fn update_energy_history(
        &mut self,
        c: usize,
        start: usize,
        end: usize,
        m: usize,
        is_transient: bool,
    ) {
        if c == 1 {
            self.old_band_e[1] = self.old_band_e[0];
        }

        if !is_transient {
            self.old_log_e2 = self.old_log_e;
            self.old_log_e = self.old_band_e;
        } else {
            for ch in 0..MAX_CHANNELS {
                for band in 0..MAX_BANDS {
                    self.old_log_e[ch][band] =
                        self.old_log_e[ch][band].min(self.old_band_e[ch][band]);
                }
            }
        }

        let max_background_increase = ((self.loss_duration + m).min(160) as f32) * 0.001_f32;
        for ch in 0..MAX_CHANNELS {
            for band in 0..MAX_BANDS {
                self.background_log_e[ch][band] = (self.background_log_e[ch][band]
                    + max_background_increase)
                    .min(self.old_band_e[ch][band]);
            }
            for band in 0..start {
                self.old_band_e[ch][band] = 0.0;
                self.old_log_e[ch][band] = INITIAL_LOG_ENERGY;
                self.old_log_e2[ch][band] = INITIAL_LOG_ENERGY;
            }
            for band in end..MAX_BANDS {
                self.old_band_e[ch][band] = 0.0;
                self.old_log_e[ch][band] = INITIAL_LOG_ENERGY;
                self.old_log_e2[ch][band] = INITIAL_LOG_ENERGY;
            }
        }

        return;
    }

    /// Conceals a lost frame using decayed spectral LCG noise injection and MDCT synthesis
    /// (`FRAME_PLC_NOISE` in `celt_decoder.c:738-808`).
    fn conceal_noise_based(&mut self, n: usize, lm: usize) {
        let cc = self.output_channels;
        let start = self.start_band;
        let end = self.end_band;
        let eff_end = start.max(end.min(MAX_BANDS));

        for ch in 0..cc {
            self.decode_mem[ch].copy_within(n..(DECODE_BUFFER_SIZE + OVERLAP), 0);
        }

        if self.prefilter_and_fold_next {
            self.prefilter_and_fold(n);
        }

        let decay = if self.loss_duration == 0 {
            1.5_f32
        } else {
            0.5_f32
        };
        for ch in 0..cc {
            for band in start..end {
                self.old_band_e[ch][band] =
                    self.background_log_e[ch][band].max(self.old_band_e[ch][band] - decay);
            }
        }

        let mut x_norm = [0.0_f32; MAX_CHANNELS * MAX_FRAME_SIZE];
        let mut seed = self.rng;
        for ch in 0..cc {
            for band in start..eff_end {
                let boffs = ch * n + (EBANDS[band] << lm);
                let blen = (EBANDS[band + 1] - EBANDS[band]) << lm;
                let band_slice = &mut x_norm[boffs..(boffs + blen)];
                for coeff in band_slice.iter_mut() {
                    seed = celt_lcg_rand(seed);
                    *coeff = ((seed as i32) >> 20) as f32;
                }
                renormalise_vector(band_slice, blen, 1.0_f32);
            }
        }
        self.rng = seed;

        self.synthesize_mdct(
            &x_norm[..(cc * n)],
            n,
            lm,
            cc,
            cc,
            start,
            eff_end,
            false,
            false,
        );

        let keep_postfilter = PostFilterParams {
            pitch: self.postfilter_period.max(COMBFILTER_MINPERIOD as i32) as usize,
            gain: self.postfilter_gain,
            tapset: self.postfilter_tapset,
        };
        self.apply_postfilter(n, lm, cc, keep_postfilter);

        self.prefilter_and_fold_next = false;
        self.skip_plc = true;
        self.last_frame_periodic_plc = false;
        return;
    }

    /// Conceals a lost frame using pitch-synchronized 24th-order LPC excitation extrapolation
    /// (`FRAME_PLC_PERIODIC` in `celt_decoder.c:809-1077`, RFC 6716 Section 4.3.7).
    fn conceal_pitch_based(&mut self, n: usize) {
        let cc = self.output_channels;
        let max_period = COMBFILTER_MAXPERIOD;

        let (pitch_index, fade) = if !self.last_frame_periodic_plc {
            let ch_refs = [
                &self.decode_mem[0][..DECODE_BUFFER_SIZE],
                &self.decode_mem[1][..DECODE_BUFFER_SIZE],
            ];
            let found_pitch = plc_pitch_search(&ch_refs[..cc], cc);
            self.last_pitch_index = found_pitch;
            (found_pitch, 1.0_f32)
        } else {
            (self.last_pitch_index, 0.8_f32)
        };

        let exc_length = (2 * pitch_index).min(max_period);
        let mut exc_buf = [0.0_f32; COMBFILTER_MAXPERIOD + LPC_ORDER];
        let mut fir_tmp = [0.0_f32; COMBFILTER_MAXPERIOD];
        let mut iir_in = [0.0_f32; MAX_FRAME_SIZE + OVERLAP];

        for ch in 0..cc {
            let hist_start = DECODE_BUFFER_SIZE - max_period - LPC_ORDER;
            exc_buf.copy_from_slice(&self.decode_mem[ch][hist_start..DECODE_BUFFER_SIZE]);

            if !self.last_frame_periodic_plc {
                let mut ac = [0.0_f32; LPC_ORDER + 1];
                celt_autocorr(
                    &exc_buf[LPC_ORDER..(LPC_ORDER + max_period)],
                    &mut ac,
                    Some(&CELT_WINDOW),
                    OVERLAP,
                    LPC_ORDER,
                    max_period,
                );
                // Add a -40 dB noise floor and apply Gaussian lag windowing.
                ac[0] *= 1.0001_f32;
                for (i, ac_val) in ac.iter_mut().enumerate().skip(1).take(LPC_ORDER) {
                    let i_f = i as f32;
                    *ac_val -= *ac_val * (0.008_f32 * 0.008_f32) * i_f * i_f;
                }
                celt_lpc(&mut self.lpc[ch], &ac, LPC_ORDER);
            }

            // Filter the last `exc_length` samples before the loss with the LPC analysis FIR filter.
            let fir_input_start = max_period - exc_length;
            celt_fir(
                &exc_buf[fir_input_start..(LPC_ORDER + max_period)],
                &self.lpc[ch],
                &mut fir_tmp[..exc_length],
                exc_length,
                LPC_ORDER,
            );
            exc_buf[(LPC_ORDER + fir_input_start)..(LPC_ORDER + max_period)]
                .copy_from_slice(&fir_tmp[..exc_length]);

            // Check if the excitation is decaying across the two half-windows of `exc_length`.
            let decay_length = exc_length >> 1;
            let mut e1 = 1.0_f32;
            let mut e2 = 1.0_f32;
            let exc_only = &exc_buf[LPC_ORDER..(LPC_ORDER + max_period)];
            for i in 0..decay_length {
                let ex1 = exc_only[max_period - decay_length + i];
                e1 += ex1 * ex1;
                let ex2 = exc_only[max_period - 2 * decay_length + i];
                e2 += ex2 * ex2;
            }
            e1 = e1.min(e2);
            let decay = (e1 / e2).sqrt();

            // Shift synthesis buffer history left by `n` samples.
            self.decode_mem[ch].copy_within(n..DECODE_BUFFER_SIZE, 0);

            // Extrapolate `n + OVERLAP` excitation samples periodically at `pitch_index`.
            let extrapolation_offset = max_period - pitch_index;
            let extrapolation_len = n + OVERLAP;
            let mut attenuation = fade * decay;
            let mut s1 = 0.0_f32;
            let mut j = 0usize;

            for i in 0..extrapolation_len {
                if j >= pitch_index {
                    j -= pitch_index;
                    attenuation *= decay;
                }
                self.decode_mem[ch][DECODE_BUFFER_SIZE - n + i] =
                    attenuation * exc_only[extrapolation_offset + j];
                let prev_sample = self.decode_mem[ch]
                    [DECODE_BUFFER_SIZE - max_period - n + extrapolation_offset + j];
                s1 += prev_sample * prev_sample;
                j += 1;
            }

            // Initialize IIR filter memory with the `LPC_ORDER` samples immediately preceding the
            // concealed region, then synthesize the extrapolated waveform.
            let mut lpc_mem = [0.0_f32; LPC_ORDER];
            for (i, mem_slot) in lpc_mem.iter_mut().enumerate() {
                *mem_slot = self.decode_mem[ch][DECODE_BUFFER_SIZE - n - 1 - i];
            }
            let extrap_slice = &mut self.decode_mem[ch]
                [(DECODE_BUFFER_SIZE - n)..(DECODE_BUFFER_SIZE - n + extrapolation_len)];
            iir_in[..extrapolation_len].copy_from_slice(extrap_slice);
            celt_iir(
                &iir_in[..extrapolation_len],
                &self.lpc[ch],
                extrap_slice,
                extrapolation_len,
                LPC_ORDER,
                &mut lpc_mem,
            );

            // Attenuate if the IIR synthesis energy exceeds the reference energy `s1`.
            let s2 = extrap_slice
                .iter()
                .fold(0.0_f32, |acc, &sample| acc + sample * sample);
            match (s1 <= 0.2_f32 * s2 || s2.is_nan(), s1 < s2) {
                (true, _) => extrap_slice.fill(0.0_f32),
                (false, true) => {
                    let ratio = ((s1 + 1.0_f32) / (s2 + 1.0_f32)).sqrt();
                    for (sample, &w) in extrap_slice[..OVERLAP].iter_mut().zip(CELT_WINDOW.iter()) {
                        let tmp_g = 1.0_f32 - w * (1.0_f32 - ratio);
                        *sample *= tmp_g;
                    }
                    for sample in extrap_slice[OVERLAP..extrapolation_len].iter_mut() {
                        *sample *= ratio;
                    }
                }
                (false, false) => {}
            }
        }

        self.prefilter_and_fold_next = true;
        self.last_frame_periodic_plc = true;
        return;
    }

    /// Runs first-order de-emphasis, integer decimation (`downsample`), and `1 / 32768.0`
    /// normalization on the newly synthesized `n` samples in `self.decode_mem`, writing or
    /// accumulating into `out_ch0` and `out_ch1`.
    fn deemphasize_to_planar(
        &mut self,
        n: usize,
        out_ch0: &mut [f32],
        out_ch1: Option<&mut [f32]>,
        accum: bool,
    ) {
        let out_offset = DECODE_BUFFER_SIZE - n;
        let out_samples = n / self.downsample;

        if !accum {
            deemphasis_channel(
                &self.decode_mem[0][out_offset..DECODE_BUFFER_SIZE],
                &mut out_ch0[..out_samples],
                n,
                self.downsample,
                &mut self.preemph_mem[0],
            );
            if self.output_channels == 2 {
                if let Some(ch1) = out_ch1 {
                    deemphasis_channel(
                        &self.decode_mem[1][out_offset..DECODE_BUFFER_SIZE],
                        &mut ch1[..out_samples],
                        n,
                        self.downsample,
                        &mut self.preemph_mem[1],
                    );
                }
            }
        } else {
            let mut tmp_pcm = [0.0_f32; MAX_FRAME_SIZE];
            deemphasis_channel(
                &self.decode_mem[0][out_offset..DECODE_BUFFER_SIZE],
                &mut tmp_pcm[..out_samples],
                n,
                self.downsample,
                &mut self.preemph_mem[0],
            );
            for (dst, &src) in out_ch0[..out_samples]
                .iter_mut()
                .zip(tmp_pcm[..out_samples].iter())
            {
                *dst += src;
            }
            if self.output_channels == 2 {
                if let Some(ch1) = out_ch1 {
                    deemphasis_channel(
                        &self.decode_mem[1][out_offset..DECODE_BUFFER_SIZE],
                        &mut tmp_pcm[..out_samples],
                        n,
                        self.downsample,
                        &mut self.preemph_mem[1],
                    );
                    for (dst, &src) in ch1[..out_samples]
                        .iter_mut()
                        .zip(tmp_pcm[..out_samples].iter())
                    {
                        *dst += src;
                    }
                }
            }
        }

        return;
    }
}

#[cfg(test)]
#[allow(clippy::excessive_precision)]
mod tests {
    use super::*;

    const MONO_PKT_120: [u8; 20] = [
        0x7c, 0x3c, 0x4a, 0x25, 0xae, 0x69, 0x4c, 0x84, 0xe3, 0x2c, 0x1d, 0x8a, 0x9e, 0x7a, 0x30,
        0x0b, 0xa3, 0xbb, 0xe1, 0x5c,
    ];
    const MONO_HEAD_120: [f32; 8] = [
        -3.11499889e-06_f32,
        -1.29576329e-05_f32,
        -2.13003987e-05_f32,
        7.35655995e-05_f32,
        -5.78037725e-05_f32,
        -9.47556837e-05_f32,
        2.44734198e-04_f32,
        3.67265166e-05_f32,
    ];
    const MONO_TAIL_120: [f32; 8] = [
        -8.53236765e-04_f32,
        -2.93973424e-02_f32,
        -1.93749368e-02_f32,
        5.63502871e-03_f32,
        -2.30330266e-02_f32,
        -1.34612210e-02_f32,
        5.58452401e-03_f32,
        5.10411188e-02_f32,
    ];
    const MONO_RNG_120: u32 = 0x12041c00;

    const MONO_PKT_240: [u8; 40] = [
        0x7d, 0x06, 0x44, 0xc4, 0x03, 0xbc, 0xe4, 0x65, 0xe6, 0xd2, 0x49, 0x4e, 0xed, 0xb7, 0xc3,
        0x63, 0x18, 0xe2, 0xd0, 0xa2, 0x91, 0x0e, 0x23, 0x4a, 0x6f, 0x66, 0x31, 0x86, 0xbd, 0x33,
        0x4b, 0x44, 0x56, 0x59, 0xd6, 0xde, 0x8a, 0x80, 0x5f, 0x18,
    ];
    const MONO_HEAD_240: [f32; 8] = [
        1.49026409e-01_f32,
        2.15950310e-01_f32,
        2.79721141e-01_f32,
        2.82028317e-01_f32,
        2.61658549e-01_f32,
        3.21524531e-01_f32,
        3.37180316e-01_f32,
        3.17863524e-01_f32,
    ];
    const MONO_TAIL_240: [f32; 8] = [
        3.37832958e-01_f32,
        3.25293601e-01_f32,
        3.11400712e-01_f32,
        2.96576291e-01_f32,
        2.80858099e-01_f32,
        2.64457643e-01_f32,
        2.47565538e-01_f32,
        2.30448693e-01_f32,
    ];
    const MONO_RNG_240: u32 = 0x013a4a00;

    const MONO_PKT_480: [u8; 80] = [
        0x79, 0xed, 0xeb, 0x78, 0xc8, 0x60, 0xe3, 0xc4, 0x2d, 0x2a, 0x8e, 0x4d, 0x4b, 0x8b, 0x77,
        0x02, 0xe7, 0xc0, 0x51, 0x37, 0xfa, 0x36, 0xf4, 0x27, 0xee, 0xe1, 0xbe, 0xd3, 0xb5, 0x6f,
        0xe7, 0xf8, 0xd1, 0x1f, 0x9d, 0x46, 0x92, 0xbb, 0xa7, 0x74, 0x8f, 0x67, 0xe6, 0x87, 0xe8,
        0x49, 0x7a, 0x30, 0x6e, 0x23, 0x00, 0x19, 0x4f, 0xc8, 0xa1, 0xdd, 0xd7, 0xc3, 0x83, 0xbe,
        0x66, 0xe9, 0x82, 0x25, 0x4f, 0xdc, 0x17, 0x01, 0x9d, 0x48, 0x5c, 0x63, 0x9e, 0x71, 0xdf,
        0xdc, 0x3e, 0xd3, 0x2e, 0xda,
    ];
    const MONO_HEAD_480: [f32; 8] = [
        2.13207811e-01_f32,
        1.96301848e-01_f32,
        1.79373786e-01_f32,
        1.63763851e-01_f32,
        1.48507282e-01_f32,
        1.33852482e-01_f32,
        1.22132048e-01_f32,
        1.09447777e-01_f32,
    ];
    const MONO_TAIL_480: [f32; 8] = [
        -4.72173393e-01_f32,
        -4.60278928e-01_f32,
        -4.47942793e-01_f32,
        -4.36154902e-01_f32,
        -4.24710900e-01_f32,
        -4.13523257e-01_f32,
        -4.02829885e-01_f32,
        -3.92409801e-01_f32,
    ];
    const MONO_RNG_480: u32 = 0x471a6800;

    const MONO_PKT_960: [u8; 160] = [
        0x7c, 0x85, 0x51, 0xaf, 0x45, 0x13, 0xb4, 0x3b, 0x2f, 0xee, 0x14, 0x9b, 0x28, 0x00, 0xcc,
        0xc4, 0xc4, 0x02, 0x1b, 0xd8, 0x64, 0x08, 0xca, 0x41, 0x00, 0xb1, 0xbb, 0x39, 0xe5, 0x35,
        0x20, 0x94, 0xe5, 0x92, 0xa6, 0x00, 0x54, 0xb6, 0x46, 0xaa, 0x1a, 0x29, 0xb8, 0xc3, 0x3d,
        0xc7, 0x1b, 0x8e, 0xee, 0xeb, 0x7e, 0x3f, 0xb9, 0x3d, 0xa4, 0xd7, 0xd7, 0xd6, 0x62, 0x61,
        0x74, 0x27, 0x44, 0x0e, 0x13, 0x7c, 0x60, 0x4a, 0xf4, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x1d, 0x69, 0x2f, 0xa3, 0x70, 0x00, 0xe6, 0x49, 0x08,
        0x18, 0x17, 0x3b, 0xa4, 0x64, 0x8a, 0x08, 0xfa, 0x5d, 0xd1, 0xe9, 0x98, 0xdb, 0xd6, 0x00,
        0x00, 0xa0, 0x19, 0xe9, 0x8d, 0x51, 0x1e, 0x3e, 0x2a, 0xd0, 0x88, 0x01, 0xbd, 0x88, 0xc7,
        0x80, 0x05, 0x27, 0xdb, 0x6d, 0x8d, 0xb6, 0x9b, 0x8f, 0x03,
    ];
    const MONO_HEAD_960: [f32; 8] = [
        -3.82692218e-01_f32,
        -3.73273969e-01_f32,
        -3.64426851e-01_f32,
        -3.55661809e-01_f32,
        -3.47891659e-01_f32,
        -3.40573609e-01_f32,
        -3.32307875e-01_f32,
        -3.26006442e-01_f32,
    ];
    const MONO_TAIL_960: [f32; 8] = [
        5.53317666e-02_f32,
        4.05748375e-02_f32,
        2.44297516e-02_f32,
        6.78314455e-03_f32,
        -1.19780730e-02_f32,
        -3.21609192e-02_f32,
        -5.35312481e-02_f32,
        -7.58339986e-02_f32,
    ];
    const MONO_RNG_960: u32 = 0x3c141500;

    const PLC_HEAD_0: [f32; 8] = [
        -9.92090106e-02_f32,
        -1.23355642e-01_f32,
        -1.48679987e-01_f32,
        -1.74979076e-01_f32,
        -2.01964051e-01_f32,
        -2.29726657e-01_f32,
        -2.57801831e-01_f32,
        -2.86338896e-01_f32,
    ];
    const PLC_TAIL_0: [f32; 8] = [
        -2.79531717e-01_f32,
        -2.73246348e-01_f32,
        -2.66399741e-01_f32,
        -2.59047687e-01_f32,
        -2.51049578e-01_f32,
        -2.42268860e-01_f32,
        -2.32731909e-01_f32,
        -2.22371534e-01_f32,
    ];
    const PLC_RNG_0: u32 = 0x3c141500;

    const PLC_HEAD_1: [f32; 8] = [
        -2.11372018e-01_f32,
        -1.99950516e-01_f32,
        -1.87685072e-01_f32,
        -1.74513876e-01_f32,
        -1.60712898e-01_f32,
        -1.46051675e-01_f32,
        -1.30629361e-01_f32,
        -1.14582062e-01_f32,
    ];
    const PLC_TAIL_1: [f32; 8] = [
        6.04914837e-02_f32,
        6.15975000e-02_f32,
        6.36616424e-02_f32,
        6.65952265e-02_f32,
        7.03878999e-02_f32,
        7.49458671e-02_f32,
        8.01428184e-02_f32,
        8.60325098e-02_f32,
    ];
    const PLC_RNG_1: u32 = 0x3c141500;

    const PLC_HEAD_5: [f32; 8] = [
        2.98432112e-02_f32,
        3.11845299e-02_f32,
        3.29137966e-02_f32,
        3.49432863e-02_f32,
        3.68133858e-02_f32,
        3.84001546e-02_f32,
        4.14124504e-02_f32,
        4.40755300e-02_f32,
    ];
    const PLC_TAIL_5: [f32; 8] = [
        1.68310285e-01_f32,
        1.37671083e-01_f32,
        1.33715436e-01_f32,
        1.84914246e-01_f32,
        1.79313451e-01_f32,
        2.61573136e-01_f32,
        3.10502410e-01_f32,
        2.62677759e-01_f32,
    ];
    const PLC_RNG_5: u32 = 0xd911c0a0;

    const RECOVERY_PKT: [u8; 80] = [
        0x78, 0x38, 0xdc, 0x47, 0xf1, 0xef, 0x72, 0xad, 0x17, 0x39, 0x88, 0x60, 0x4d, 0xd5, 0x72,
        0x9d, 0x0a, 0xea, 0x6e, 0x34, 0xd6, 0x79, 0xf4, 0xbb, 0xd2, 0x34, 0xee, 0x39, 0x6e, 0xa3,
        0x03, 0xce, 0x7a, 0xf4, 0x47, 0x40, 0x09, 0xa3, 0x9e, 0x3f, 0x5c, 0x7e, 0xa5, 0x5c, 0xcc,
        0x32, 0x0d, 0xa3, 0xd8, 0x48, 0x00, 0x5e, 0x73, 0x40, 0x00, 0x8a, 0x42, 0xc0, 0x00, 0x80,
        0x02, 0x6c, 0x00, 0x0f, 0xf8, 0x86, 0x08, 0x07, 0xf4, 0xc7, 0x5f, 0x08, 0xdc, 0xe0, 0xa2,
        0x57, 0x58, 0xf0, 0x34, 0x93,
    ];
    const RECOVERY_HEAD: [f32; 8] = [
        2.60167867e-01_f32,
        1.50642604e-01_f32,
        8.24194700e-02_f32,
        1.21272191e-01_f32,
        1.05687872e-01_f32,
        1.06321760e-01_f32,
        9.18942466e-02_f32,
        9.43008214e-02_f32,
    ];
    const RECOVERY_TAIL: [f32; 8] = [
        9.41674225e-04_f32,
        9.99980140e-04_f32,
        1.05673436e-03_f32,
        1.11144036e-03_f32,
        1.16428710e-03_f32,
        1.21546513e-03_f32,
        1.26444083e-03_f32,
        1.31133106e-03_f32,
    ];
    const RECOVERY_RNG: u32 = 0x0083c940;

    fn assert_f32_slice_close(actual: &[f32], expected: &[f32], tol: f32) {
        assert_eq!(actual.len(), expected.len());
        for (i, (&a, &e)) in actual.iter().zip(expected.iter()).enumerate() {
            let diff = (a - e).abs();
            assert!(
                diff <= tol,
                "sample {i} mismatch: actual={a:.8e}, expected={e:.8e}, diff={diff:.8e} > tol={tol:.8e}"
            );
        }
        return;
    }

    #[test]
    fn celt_mono_multiframe_and_plc_match_libopus_oracle() {
        let mut dec = Decoder::new(48_000, 1).expect("valid mono decoder");
        let mut out = [0.0_f32; 960];

        // 1. Decode 2.5 ms (120), 5 ms (240), 10 ms (480, transient), and 20 ms (960) frames.
        for (pkt, n, exp_head, exp_tail, exp_rng) in [
            (
                &MONO_PKT_120[..],
                120usize,
                &MONO_HEAD_120,
                &MONO_TAIL_120,
                MONO_RNG_120,
            ),
            (
                &MONO_PKT_240[..],
                240usize,
                &MONO_HEAD_240,
                &MONO_TAIL_240,
                MONO_RNG_240,
            ),
            (
                &MONO_PKT_480[..],
                480usize,
                &MONO_HEAD_480,
                &MONO_TAIL_480,
                MONO_RNG_480,
            ),
            (
                &MONO_PKT_960[..],
                960usize,
                &MONO_HEAD_960,
                &MONO_TAIL_960,
                MONO_RNG_960,
            ),
        ] {
            let written = dec
                .decode_frame_planar(Some(pkt), None, n, &mut out[..n], None, false)
                .expect("decode mono frame");
            assert_eq!(written, n);
            assert_eq!(dec.final_range(), exp_rng);
            assert_f32_slice_close(&out[..8], exp_head, 2e-5);
            assert_f32_slice_close(&out[(n - 8)..n], exp_tail, 2e-5);
        }

        // 2. Conceal 6 consecutive 20 ms lost frames:
        //    - i = 0..4 use pitch-based LPC extrapolation (plc_duration = 0, 8, 16, 24, 32 < 40).
        //    - i = 5 transitions to spectral LCG noise concealment (plc_duration = 40 >= 40).
        for i in 0..6 {
            let written = dec
                .conceal_frame_planar(960, &mut out, None, false)
                .expect("conceal lost frame");
            assert_eq!(written, 960);
            match i {
                0 => {
                    assert_eq!(dec.final_range(), PLC_RNG_0);
                    assert_f32_slice_close(&out[..8], &PLC_HEAD_0, 2e-5);
                    assert_f32_slice_close(&out[952..960], &PLC_TAIL_0, 2e-5);
                }
                1 => {
                    assert_eq!(dec.final_range(), PLC_RNG_1);
                    assert_f32_slice_close(&out[..8], &PLC_HEAD_1, 2e-5);
                    assert_f32_slice_close(&out[952..960], &PLC_TAIL_1, 2e-5);
                }
                5 => {
                    assert_eq!(dec.final_range(), PLC_RNG_5);
                    assert_f32_slice_close(&out[..8], &PLC_HEAD_5, 2e-5);
                    assert_f32_slice_close(&out[952..960], &PLC_TAIL_5, 2e-5);
                }
                _ => {}
            }
        }

        // 3. Decode recovery frame after packet loss (exercises apply_loss_recovery_energy_safety).
        let written = dec
            .decode_frame_planar(Some(&RECOVERY_PKT), None, 960, &mut out, None, false)
            .expect("decode recovery frame");
        assert_eq!(written, 960);
        assert_eq!(dec.final_range(), RECOVERY_RNG);
        assert_f32_slice_close(&out[..8], &RECOVERY_HEAD, 2e-5);
        assert_f32_slice_close(&out[952..960], &RECOVERY_TAIL, 2e-5);
        return;
    }

    #[test]
    fn celt_stereo_silence_and_mono_in_stereo_match_libopus_oracle() {
        const STEREO_PKT_480: [u8; 60] = [
            0x7f, 0xaa, 0x5a, 0x6f, 0x47, 0xc9, 0x27, 0x2d, 0xa6, 0x0c, 0xb9, 0xa0, 0x34, 0x27,
            0x45, 0x2e, 0x43, 0x03, 0x2c, 0x61, 0x24, 0x02, 0x4b, 0x1b, 0xc6, 0x55, 0x1c, 0x43,
            0xdc, 0xd5, 0xc9, 0x9e, 0xb8, 0xa0, 0x0c, 0x8e, 0x63, 0xf5, 0xc6, 0x38, 0xda, 0x1b,
            0x12, 0xc0, 0x6a, 0x2f, 0x31, 0x6c, 0xac, 0x29, 0xf2, 0x7f, 0x0e, 0x43, 0x3c, 0xbe,
            0x3a, 0xf3, 0xca, 0x28,
        ];
        const STEREO_L_TAIL_480: [f32; 8] = [
            -4.17376965e-01_f32,
            -4.00183052e-01_f32,
            -3.92758131e-01_f32,
            -4.02621746e-01_f32,
            -3.88109535e-01_f32,
            -3.74064475e-01_f32,
            -3.80719841e-01_f32,
            -3.67973894e-01_f32,
        ];
        const STEREO_R_TAIL_480: [f32; 8] = [
            -1.45076156e-01_f32,
            -1.27458289e-01_f32,
            -1.11662380e-01_f32,
            -9.88233164e-02_f32,
            -8.15347508e-02_f32,
            -6.38602301e-02_f32,
            -4.98592928e-02_f32,
            -3.31351310e-02_f32,
        ];
        const STEREO_RNG_480: u32 = 0x37ba0a00;

        const STEREO_PKT_960: [u8; 120] = [
            0xca, 0x38, 0xaf, 0xe2, 0xfb, 0x71, 0xca, 0x2c, 0xda, 0xf6, 0xb3, 0xdf, 0xe6, 0x36,
            0xef, 0xcc, 0x9d, 0x6b, 0x0b, 0xcd, 0x68, 0x31, 0x20, 0x4f, 0xaf, 0x9a, 0x36, 0x51,
            0xf8, 0xf2, 0x76, 0x55, 0x45, 0xd6, 0xf6, 0x6c, 0xfb, 0x75, 0x9c, 0xb4, 0xe1, 0xce,
            0xbf, 0x48, 0x8f, 0xa7, 0xad, 0x32, 0x6b, 0x48, 0x6d, 0x53, 0x03, 0x10, 0xda, 0xa9,
            0x3b, 0x9d, 0xa9, 0xaf, 0xf1, 0x9d, 0xef, 0x41, 0x15, 0xad, 0x9b, 0x78, 0x9d, 0x23,
            0xda, 0x26, 0x77, 0x83, 0xab, 0x36, 0x91, 0x28, 0x17, 0xdb, 0x66, 0x86, 0x27, 0x73,
            0x56, 0x9d, 0x5a, 0x08, 0x88, 0x00, 0x77, 0x29, 0xae, 0xca, 0x44, 0x00, 0x94, 0x50,
            0xc0, 0x01, 0x00, 0x03, 0xcd, 0xc4, 0x0a, 0x10, 0xfd, 0xdb, 0x54, 0x6f, 0x67, 0x8c,
            0xdc, 0x99, 0x9f, 0x7d, 0x72, 0x33, 0x8d, 0xc1,
        ];
        const STEREO_L_TAIL_960: [f32; 8] = [
            -9.60404426e-02_f32,
            -1.08324490e-01_f32,
            -1.20585375e-01_f32,
            -1.32816896e-01_f32,
            -1.45001888e-01_f32,
            -1.57111838e-01_f32,
            -1.69115692e-01_f32,
            -1.80991650e-01_f32,
        ];
        const STEREO_R_TAIL_960: [f32; 8] = [
            1.83009833e-01_f32,
            1.66810691e-01_f32,
            1.50145844e-01_f32,
            1.33115575e-01_f32,
            1.15805209e-01_f32,
            9.82826427e-02_f32,
            8.06091353e-02_f32,
            6.28555566e-02_f32,
        ];
        const STEREO_RNG_960: u32 = 0x029cef00;

        const SILENCE_L_HEAD: [f32; 8] = [
            -1.92732185e-01_f32,
            -2.04334676e-01_f32,
            -2.15793774e-01_f32,
            -2.27094308e-01_f32,
            -2.38211840e-01_f32,
            -2.49119058e-01_f32,
            -2.59790957e-01_f32,
            -2.70205140e-01_f32,
        ];
        const SILENCE_R_HEAD: [f32; 8] = [
            4.51083407e-02_f32,
            2.74521299e-02_f32,
            9.96772107e-03_f32,
            -7.27671199e-03_f32,
            -2.42255889e-02_f32,
            -4.08281609e-02_f32,
            -5.70321754e-02_f32,
            -7.27841631e-02_f32,
        ];
        const SILENCE_RNG: u32 = 0x01000000;

        const MONO_IN_STEREO_PKT: [u8; 40] = [
            0x7f, 0x52, 0xdb, 0x7b, 0x1f, 0x34, 0x78, 0x79, 0xba, 0xe8, 0x4c, 0x33, 0xdf, 0x7b,
            0xce, 0x11, 0x2f, 0xb0, 0xc7, 0xd7, 0x6b, 0x3a, 0x40, 0xf8, 0x45, 0x4e, 0xbe, 0x3e,
            0x32, 0x4f, 0x2d, 0x3c, 0x5e, 0x79, 0xf5, 0xce, 0x80, 0x2c, 0xd1, 0xe9,
        ];
        const MONO_IN_STEREO_L_TAIL: [f32; 8] = [
            -1.26281887e-01_f32,
            -1.11190245e-01_f32,
            -9.57806483e-02_f32,
            -8.00936297e-02_f32,
            -6.41752258e-02_f32,
            -4.80800085e-02_f32,
            -3.18579338e-02_f32,
            -1.55676492e-02_f32,
        ];
        const MONO_IN_STEREO_R_TAIL: [f32; 8] = [
            -1.26281887e-01_f32,
            -1.11190245e-01_f32,
            -9.57806483e-02_f32,
            -8.00936297e-02_f32,
            -6.41752258e-02_f32,
            -4.80800085e-02_f32,
            -3.18579338e-02_f32,
            -1.55676492e-02_f32,
        ];
        const MONO_IN_STEREO_RNG: u32 = 0x5a536100;

        let mut dec = Decoder::new(48_000, 2).expect("valid stereo decoder");
        let mut out_l = [0.0_f32; 960];
        let mut out_r = [0.0_f32; 960];

        // 1. 10 ms stereo transient frame (N = 480)
        let n1 = dec
            .decode_frame_planar(
                Some(&STEREO_PKT_480),
                None,
                480,
                &mut out_l[..480],
                Some(&mut out_r[..480]),
                false,
            )
            .expect("decode stereo 480");
        assert_eq!(n1, 480);
        assert_eq!(dec.final_range(), STEREO_RNG_480);
        assert_f32_slice_close(&out_l[472..480], &STEREO_L_TAIL_480, 2e-5);
        assert_f32_slice_close(&out_r[472..480], &STEREO_R_TAIL_480, 2e-5);

        // 2. 20 ms stereo steady-state frame (N = 960)
        let n2 = dec
            .decode_frame_planar(
                Some(&STEREO_PKT_960),
                None,
                960,
                &mut out_l,
                Some(&mut out_r),
                false,
            )
            .expect("decode stereo 960");
        assert_eq!(n2, 960);
        assert_eq!(dec.final_range(), STEREO_RNG_960);
        assert_f32_slice_close(&out_l[952..960], &STEREO_L_TAIL_960, 2e-5);
        assert_f32_slice_close(&out_r[952..960], &STEREO_R_TAIL_960, 2e-5);

        // 3. Silence frame ([0xFF]) on active stereo decoder smoothly fades out the MDCT overlap tail.
        let n3 = dec
            .decode_frame_planar(
                Some(&[0xFF]),
                None,
                120,
                &mut out_l[..120],
                Some(&mut out_r[..120]),
                false,
            )
            .expect("decode silence frame");
        assert_eq!(n3, 120);
        assert_eq!(dec.final_range(), SILENCE_RNG);
        assert_f32_slice_close(&out_l[..8], &SILENCE_L_HEAD, 2e-5);
        assert_f32_slice_close(&out_r[..8], &SILENCE_R_HEAD, 2e-5);

        // 4. Mono-in-stereo (stream_channels = 1, output_channels = 2)
        dec.set_stream_channels(1).expect("set stream channels = 1");
        let n4 = dec
            .decode_frame_planar(
                Some(&MONO_IN_STEREO_PKT),
                None,
                240,
                &mut out_l[..240],
                Some(&mut out_r[..240]),
                false,
            )
            .expect("decode mono-in-stereo frame");
        assert_eq!(n4, 240);
        assert_eq!(dec.final_range(), MONO_IN_STEREO_RNG);
        assert_f32_slice_close(&out_l[232..240], &MONO_IN_STEREO_L_TAIL, 2e-5);
        assert_f32_slice_close(&out_r[232..240], &MONO_IN_STEREO_R_TAIL, 2e-5);
        return;
    }

    #[test]
    fn celt_hybrid_highband_with_external_range_decoder_and_accum_matches_libopus_oracle() {
        const HYBRID_PKT: [u8; 48] = [
            0x29, 0x72, 0xbb, 0x04, 0x4d, 0x96, 0xdf, 0x28, 0x71, 0xba, 0x03, 0x4c, 0x95, 0xde,
            0x27, 0x70, 0xb9, 0x02, 0x4b, 0x94, 0xdd, 0x26, 0x6f, 0xb8, 0x01, 0x4a, 0x93, 0xdc,
            0x25, 0x6e, 0xb7, 0x00, 0x49, 0x92, 0xdb, 0x24, 0x6d, 0xb6, 0xff, 0x48, 0x91, 0xda,
            0x23, 0x6c, 0xb5, 0xfe, 0x47, 0x90,
        ];
        const HYBRID_HEAD: [f32; 8] = [
            -1.00000000e+00_f32,
            -8.75000417e-01_f32,
            -7.50000894e-01_f32,
            -6.25000179e-01_f32,
            -4.99999672e-01_f32,
            -3.74994516e-01_f32,
            -2.49995813e-01_f32,
            -1.25009388e-01_f32,
        ];
        const HYBRID_TAIL: [f32; 8] = [
            6.25069678e-01_f32,
            7.49322832e-01_f32,
            8.74420822e-01_f32,
            1.00020945e+00_f32,
            -9.99211788e-01_f32,
            -8.74615490e-01_f32,
            -7.50568449e-01_f32,
            -6.25610173e-01_f32,
        ];
        const HYBRID_RNG: u32 = 0x3a35db00;

        let mut dec = Decoder::new(48_000, 1).expect("valid mono decoder");
        dec.set_start_band(17).expect("set hybrid start band 17");
        dec.set_end_band(21).expect("set fullband end band 21");

        let mut range_dec = entropy::Decoder::new(&HYBRID_PKT).expect("init range decoder");
        let _ = range_dec.decode_symbol_logp(2).expect("silk bit");
        let _ = range_dec.decode_uint(16).expect("silk uint");

        let mut out_pcm = [0.0_f32; 480];
        for (i, sample) in out_pcm.iter_mut().enumerate() {
            *sample = 0.125_f32 * (((i % 17) as i32 - 8) as f32);
        }

        let written = dec
            .decode_frame_planar(
                Some(&HYBRID_PKT),
                Some(&mut range_dec),
                480,
                &mut out_pcm,
                None,
                true,
            )
            .expect("decode hybrid highband");
        assert_eq!(written, 480);
        assert_eq!(dec.final_range(), HYBRID_RNG);
        assert_f32_slice_close(&out_pcm[..8], &HYBRID_HEAD, 2e-5);
        assert_f32_slice_close(&out_pcm[472..480], &HYBRID_TAIL, 2e-5);
        return;
    }

    #[test]
    fn celt_decoder_configuration_and_boundary_validation() {
        assert!(Decoder::new(44_100, 1).is_err());
        assert!(Decoder::new(48_000, 0).is_err());
        assert!(Decoder::new(48_000, 3).is_err());

        let mut dec = Decoder::new(24_000, 1).expect("valid 24 kHz decoder");
        assert!(dec.set_start_band(21).is_err());
        assert!(dec.set_end_band(0).is_err());
        assert!(dec.set_end_band(22).is_err());
        assert!(dec.set_stream_channels(0).is_err());
        assert!(dec.set_stream_channels(3).is_err());
        assert!(dec.set_output_channels(0).is_err());
        assert!(dec.set_sample_rate(32_000).is_err());

        dec.set_bandwidth(Bandwidth::WideBand);
        dec.set_phase_inversion_disabled(true);
        dec.set_next_prefilter_and_fold(true);
        dec.reset();
        assert_eq!(dec.final_range(), 0);

        let mut small_buf = [0.0_f32; 10];
        // At 24 kHz (downsample = 2), frame_size_48k = 120 requires 60 output samples.
        assert!(
            dec.decode_frame_planar(Some(&[0xFF]), None, 120, &mut small_buf, None, false)
                .is_err()
        );
        // Invalid 48 kHz MDCT size (e.g. 360) must be rejected.
        let mut full_buf = [0.0_f32; 480];
        assert!(
            dec.decode_frame_planar(Some(&[0xFF]), None, 360, &mut full_buf, None, false)
                .is_err()
        );
        return;
    }
}
