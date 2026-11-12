//! CELT frame decoder: per-frame orchestration and the state carried between frames
//! ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)).
//!
//! For each frame, [`Decoder::decode_frame`] reads the symbols of
//! [RFC 6716 Section 4.3, Table 56](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)
//! in order and synthesizes the frame:
//!
//! ```text
//!   range decoder --> silence, post-filter, transient, intra        header flags
//!                 --> coarse energy                                  energy
//!                 --> tf_change, tf_select, spread                   rate
//!                 --> band boosts, allocation trim, skip,            rate (bit allocation)
//!                     intensity, dual stereo
//!                 --> fine energy                                    energy
//!                 --> band shapes (PVQ residual)                     vq
//!                 --> anti-collapse, final energy bits               vq, energy
//!   synthesis:    denormalisation -> inverse MDCT -> post-filter -> de-emphasis
//! ```
//!
//! A lost frame is concealed by pitch-based LPC extrapolation for up to 100 ms of loss in
//! CELT-only mode, and by decaying band-energy noise otherwise
//! ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
//! The state is split into the band energy history ([`BandEnergyHistory`]), the comb post-filter
//! ([`CombFilter`]), the concealment state ([`PlcState`]) and the synthesis memory
//! ([`SynthesisMemory`]).

use std::ops::Range;

use symphonia_core::errors::{Error as SymphoniaError, Result};

use crate::celt::bands::{ShapeFrame, anti_collapse, unquantize_bands};
use crate::celt::energy::{
    BandEnergyHistory, unquant_coarse_energy, unquant_energy_finalise, unquant_fine_energy,
};
use crate::celt::error::Error;
use crate::celt::mdct::mdct_backward_frame;
use crate::celt::plc::{PlcState, fill_noise_spectrum};
use crate::celt::postfilter::{ChannelHistory, CombFilter};
use crate::celt::rate::{
    AllocationLayout, BandAllocation, PostFilterParams, decode_bit_allocation, decode_postfilter,
    decode_spread_decision, tf_decode,
};
use crate::celt::synthesis::{Denormalisation, deemphasis_channel, denormalise_bands};
use crate::celt::{
    BITRES, BlockSize, DECODE_BUFFER_SIZE, MAX_BANDS, MAX_CHANNELS, MAX_FRAME_SIZE, OVERLAP,
    SILENCE_FRAME, Spread, end_band_for,
};
use crate::entropy;
use crate::planes::Planes;
use crate::sample_rate::SampleRate;
use crate::toc::Bandwidth;

/// Channel layout, output rate and coded band range of the decoder
/// ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Config {
    /// Number of decoded output channels (`1` or `2`).
    output_channels: usize,
    /// Number of coded channels in the current frame (`1` or `2`).
    stream_channels: usize,
    /// Integer decimation factor `48_000 / sample_rate` (`1, 2, 3, 4, 6`).
    downsample: usize,
    /// First coded band (`0` for CELT-only frames, `17` in Hybrid mode).
    start_band: usize,
    /// One past the last coded band (`13, 17, 19, 21`).
    end_band: usize,
}

/// Time-domain synthesis state per channel: the IMDCT history with its overlap tail, the
/// de-emphasis filter memory, and whether the next frame must fold a concealed overlap tail
/// ([RFC 6716 Section 4.3.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7>)).
#[derive(Clone)]
pub(crate) struct SynthesisMemory {
    /// `DECODE_BUFFER_SIZE` history samples followed by `OVERLAP` overlap-add tail samples.
    decode_mem: [ChannelHistory; MAX_CHANNELS],
    /// First-order de-emphasis filter state.
    preemph_mem: [f32; MAX_CHANNELS],
    /// Whether the next synthesized frame must undo the comb filter on, and fold, the overlap
    /// tail left by a pitch-based concealment.
    prefilter_and_fold_next: bool,
}

impl Default for SynthesisMemory {
    fn default() -> Self {
        return Self {
            decode_mem: [[0.0; DECODE_BUFFER_SIZE + OVERLAP]; MAX_CHANNELS],
            preemph_mem: [0.0; MAX_CHANNELS],
            prefilter_and_fold_next: false,
        };
    }
}

impl SynthesisMemory {
    /// Clears the history, the overlap tail and the de-emphasis memory
    /// ([RFC 6716 Section 4.3.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7>)).
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
        return;
    }

    /// Drops the oldest `n` samples of the first `channels` channels to make room for a frame
    /// ([RFC 6716 Section 4.3.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7>)).
    pub(crate) fn shift_history(&mut self, channels: usize, n: usize) {
        self.decode_mem[..channels]
            .iter_mut()
            .for_each(|channel| channel.copy_within(n..(DECODE_BUFFER_SIZE + OVERLAP), 0));
        return;
    }
}

/// Where the symbols of one CELT frame come from
/// ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>),
/// [Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
pub(crate) enum FrameInput<'a, 'd> {
    /// A CELT-only frame decoded by its own range decoder.
    Packet(&'a [u8]),
    /// The CELT layer of a Hybrid frame, read from the range decoder SILK has advanced.
    Shared(&'a mut entropy::Decoder<'d>),
    /// The one-byte `[0xFF]` frame, decoded as the two-byte silence frame.
    Silence,
    /// A lost frame (no bytes, or one byte other than `0xFF`), which is concealed.
    Lost,
}

impl<'a, 'd> FrameInput<'a, 'd> {
    /// Classifies the frame bytes and the optional shared range decoder of a decode call
    /// ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)).
    pub(crate) fn new(
        data: Option<&'a [u8]>,
        shared: Option<&'a mut entropy::Decoder<'d>>,
    ) -> Self {
        return match (shared, data) {
            (Some(_), Some([])) | (None, None | Some([])) => Self::Lost,
            (Some(dec), _) => Self::Shared(dec),
            (None, Some([0xFF])) => Self::Silence,
            (None, Some([_])) => Self::Lost,
            (None, Some(bytes)) => Self::Packet(bytes),
        };
    }
}

/// Layout of one CELT frame: MDCT block size, Bark band range and coded channel count
/// ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)).
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameLayout {
    /// MDCT block size of the frame (`120, 240, 480, 960` samples at 48 kHz).
    pub(crate) block: BlockSize,
    /// First coded Bark band (`0` in CELT-only mode, `17` in Hybrid mode).
    pub(crate) start_band: usize,
    /// Audio bandwidth that sets the exclusive upper Bark band boundary.
    pub(crate) bandwidth: Bandwidth,
    /// Number of coded channels in the CELT bitstream (`1` or `2`).
    pub(crate) stream_channels: usize,
}

/// How the de-emphasized CELT output is written into the output planes
/// ([RFC 6716 Section 4.3.7.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.2>)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mix {
    /// The output replaces the contents of the planes.
    Overwrite,
    /// The output is added onto the planes (the SILK layer of a Hybrid frame).
    Accumulate,
}

/// `-log2` probability of a set silence flag (`{32767, 1}/32768`,
/// [RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>), Table 56).
const SILENCE_FLAG_LOGP: u32 = 15;

/// `-log2` probability of a set transient flag (`{7, 1}/8`, Table 56).
const TRANSIENT_FLAG_LOGP: u32 = 3;

/// `-log2` probability of a set intra energy flag (`{7, 1}/8`, Table 56).
const INTRA_FLAG_LOGP: u32 = 3;

/// [`entropy::Decoder::tell`] before the first symbol of a frame (`1`): the silence flag is coded
/// only as the first symbol of a CELT-only frame.
const FIRST_SYMBOL_TELL: i32 = 1;

/// Frame-level flags read before the band energies
/// ([RFC 6716 Section 4.3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.1>),
/// Table 56).
#[derive(Debug, Clone, Copy, Default)]
struct FrameHeader {
    /// The frame carries no signal; nothing else is coded.
    silence: bool,
    /// Comb post-filter setting of this frame.
    postfilter: PostFilterParams,
    /// The frame uses short MDCT blocks.
    is_transient: bool,
    /// Coarse energy is coded without inter-frame prediction.
    intra: bool,
}

/// Decodes the silence, post-filter, transient and intra flags of a frame of `total_bits`
/// ([RFC 6716 Section 4.3.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.1>)).
fn decode_frame_header(
    dec: &mut entropy::Decoder<'_>,
    lm: BlockSize,
    start: usize,
    total_bits: i32,
) -> Result<FrameHeader> {
    let silence = match dec.tell() {
        tell if tell >= total_bits => true,
        FIRST_SYMBOL_TELL => dec.decode_symbol_logp(SILENCE_FLAG_LOGP)? != 0,
        _ => false,
    };
    if silence {
        return Ok(FrameHeader { silence, ..FrameHeader::default() });
    }

    let postfilter = decode_postfilter(dec, start, total_bits)?;
    let is_transient = lm.log2() > 0 && decode_budgeted_flag(dec, TRANSIENT_FLAG_LOGP, total_bits)?;
    let intra = decode_budgeted_flag(dec, INTRA_FLAG_LOGP, total_bits)?;
    return Ok(FrameHeader { silence, postfilter, is_transient, intra });
}

/// Decodes a flag that is set with probability `2^-logp`, or returns `false` without reading
/// when its worst-case cost of `logp` bits no longer fits in the frame of `total_bits`
/// ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>), Table 56).
fn decode_budgeted_flag(
    dec: &mut entropy::Decoder<'_>,
    logp: u32,
    total_bits: i32,
) -> Result<bool> {
    if dec.tell() + logp as i32 > total_bits {
        return Ok(false);
    }
    return Ok(dec.decode_symbol_logp(logp)? != 0);
}

/// Time-frequency changes, spreading and bit allocation of a frame
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
struct SpectrumPlan {
    tf_res: [i32; MAX_BANDS],
    spread: Spread,
    alloc: BandAllocation,
    /// Eighth bits reserved for the anti-collapse flag (`0` or `8`).
    anti_collapse_rsv: i32,
}

/// Decodes the time-frequency changes, spreading, band boosts, allocation trim and bit
/// allocation of a frame of `total_bits`
/// ([RFC 6716 Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>),
/// [Section 4.3.4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.3>),
/// [Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)).
fn decode_allocation(
    dec: &mut entropy::Decoder<'_>,
    config: Config,
    lm: BlockSize,
    is_transient: bool,
    total_bits: i32,
) -> Result<SpectrumPlan> {
    let Config { stream_channels: c, start_band: start, end_band: end, .. } = config;
    let tf_res = tf_decode(start, end, is_transient, lm, dec)?;
    let spread = decode_spread_decision(dec, total_bits)?;

    let layout = AllocationLayout { start, end, channels: c, lm };
    let (alloc, anti_collapse_rsv) = decode_bit_allocation(dec, layout, is_transient, total_bits)?;
    return Ok(SpectrumPlan { tf_res, spread, alloc, anti_collapse_rsv });
}

/// Frame-level CELT decoder state and synthesis engine
/// ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)).
#[derive(Clone)]
pub(crate) struct Decoder {
    config: Config,
    energy: BandEnergyHistory,
    postfilter: CombFilter,
    plc: PlcState,
    synthesis: SynthesisMemory,
    /// Final range coder state of the last frame, or the noise generator state after a
    /// noise-concealed frame.
    rng: u32,
}

impl Decoder {
    /// Creates a new CELT [`Decoder`] configured for `sample_rate` (`8_000, 12_000, 16_000,
    /// 24_000, 48_000` Hz) and `output_channels` (`1` or `2`).
    pub(crate) fn new(sample_rate: SampleRate, output_channels: usize) -> Result<Self> {
        if output_channels == 0 || output_channels > MAX_CHANNELS {
            return Err(Error::InvalidChannelCount(output_channels).into());
        }

        return Ok(Self {
            config: Config {
                output_channels,
                stream_channels: output_channels,
                downsample: sample_rate.downsample_from_48k(),
                start_band: 0,
                end_band: MAX_BANDS,
            },
            energy: BandEnergyHistory::default(),
            postfilter: CombFilter::default(),
            plc: PlcState::default(),
            synthesis: SynthesisMemory::default(),
            rng: 0,
        });
    }

    /// Resets the energy history, post-filter, concealment and synthesis state, keeping the
    /// configuration ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)).
    pub(crate) fn reset(&mut self) {
        self.energy.reset();
        self.postfilter.reset();
        self.plc.reset();
        self.synthesis.reset();
        self.rng = 0;
        return;
    }

    /// Returns the final range coder state of the most recently decoded frame
    /// ([RFC 6716 Section 4.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.1>)).
    #[inline]
    pub(crate) fn final_range(&self) -> u32 {
        return self.rng;
    }

    /// Decodes, or conceals, one CELT frame described by `layout` into the planar `f32` output
    /// [`Planes`] (`out.right` is used when `output_channels == 2`), returning the number of
    /// output samples written per channel (`frame_samples / downsample`)
    /// ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)).
    ///
    /// - [`FrameInput::Packet`] decodes a CELT-only frame from a fresh [`entropy::Decoder`].
    /// - [`FrameInput::Shared`] decodes the CELT layer of a Hybrid frame from the range decoder
    ///   SILK has advanced.
    /// - [`FrameInput::Silence`] decodes the silence frame, flushing the MDCT overlap.
    /// - [`FrameInput::Lost`] conceals the frame
    ///   ([Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    /// - [`Mix::Accumulate`] adds the de-emphasized samples onto the contents of `out`.
    pub(crate) fn decode_frame(
        &mut self,
        input: FrameInput<'_, '_>,
        layout: FrameLayout,
        out: Planes<'_>,
        mix: Mix,
    ) -> Result<usize> {
        self.apply_layout(layout)?;
        let lm = layout.block;
        self.validate_output_capacity(lm.frame_samples() / self.config.downsample, &out)?;

        return match input {
            FrameInput::Lost => self.conceal_frame(lm, out, mix),
            FrameInput::Silence => {
                self.decode_coded_frame(&mut entropy::Decoder::new(&SILENCE_FRAME), lm, out, mix)
            }
            FrameInput::Packet(bytes) => {
                self.decode_coded_frame(&mut entropy::Decoder::new(bytes), lm, out, mix)
            }
            FrameInput::Shared(dec) => self.decode_coded_frame(dec, lm, out, mix),
        };
    }

    /// Applies the band range and coded channel count of `layout` to the configuration,
    /// rejecting an empty band range or a channel count other than `1` or `2`.
    fn apply_layout(&mut self, layout: FrameLayout) -> Result<()> {
        let FrameLayout { start_band, bandwidth, stream_channels, .. } = layout;
        let end_band = end_band_for(bandwidth);
        if start_band >= end_band {
            return Err(Error::InvalidBandRange { start: start_band, end: end_band }.into());
        }
        if stream_channels == 0 || stream_channels > MAX_CHANNELS {
            return Err(Error::InvalidChannelCount(stream_channels).into());
        }
        self.config.start_band = start_band;
        self.config.end_band = end_band;
        self.config.stream_channels = stream_channels;
        return Ok(());
    }

    /// Decodes one frame from `dec`: header flags, spectrum, synthesis and de-emphasis
    /// ([RFC 6716 Section 4.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3>)).
    fn decode_coded_frame(
        &mut self,
        dec: &mut entropy::Decoder<'_>,
        lm: BlockSize,
        out: Planes<'_>,
        mix: Mix,
    ) -> Result<usize> {
        let n = lm.frame_samples();
        if dec.storage_bytes() == 0 {
            return self.conceal_frame(lm, out, mix);
        }
        let total_bits = dec.storage_bits();

        self.plc.begin_frame();
        if self.config.stream_channels == 1 {
            self.energy.merge_mono();
        }

        let header = decode_frame_header(dec, lm, self.config.start_band, total_bits)?;
        let mut x_norm = [0.0_f32; MAX_CHANNELS * MAX_FRAME_SIZE];
        match header.silence {
            true => {
                self.synthesis.shift_history(self.config.output_channels, n);
                self.energy.silence(self.config.stream_channels);
            }
            false => {
                self.decode_spectrum(dec, lm, header, total_bits, &mut x_norm)?;
            }
        }

        self.synthesize(&x_norm, lm, header);
        self.rng = dec.final_range();
        self.deemphasize(n, out, mix);
        self.plc.end_loss();
        self.synthesis.prefilter_and_fold_next = false;

        if !header.silence && dec.tell() > total_bits {
            return Err(SymphoniaError::DecodeError(
                "opus celt: range decoder overread frame bit budget",
            ));
        }
        return Ok(n / self.config.downsample);
    }

    /// Decodes the band energies and the normalised band shapes of a non-silent frame into
    /// `x_norm` (`n` bins per coded channel)
    /// ([RFC 6716 Section 4.3.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2>) to
    /// [Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>)).
    fn decode_spectrum(
        &mut self,
        dec: &mut entropy::Decoder<'_>,
        lm: BlockSize,
        header: FrameHeader,
        total_bits: i32,
        x_norm: &mut [f32; MAX_CHANNELS * MAX_FRAME_SIZE],
    ) -> Result<()> {
        let plan = self.decode_energies(dec, lm, header, total_bits)?;
        self.synthesis.shift_history(self.config.output_channels, lm.frame_samples());
        return self.decode_shapes(dec, lm, header, total_bits, &plan, x_norm);
    }

    /// Decodes the coarse band energies, the bit allocation and the fine energies of a non-silent
    /// frame, after the energy safety rule for the first frame after a loss
    /// ([RFC 6716 Section 4.3.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2>),
    /// [Section 4.3.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.3>)).
    fn decode_energies(
        &mut self,
        dec: &mut entropy::Decoder<'_>,
        lm: BlockSize,
        header: FrameHeader,
        total_bits: i32,
    ) -> Result<SpectrumPlan> {
        let Config { stream_channels: c, start_band: start, end_band: end, .. } = self.config;
        if !header.intra && self.plc.loss_duration != 0 {
            self.energy.apply_loss_recovery_safety(start..end, lm, self.plc.loss_duration);
        }
        let band_e = &mut self.energy.current[..c];
        unquant_coarse_energy(dec, start..end, header.intra, lm, band_e)?;

        let plan = decode_allocation(dec, self.config, lm, header.is_transient, total_bits)?;
        let fine_quant = &plan.alloc.fine_quant;
        unquant_fine_energy(dec, start..end, fine_quant, &mut self.energy.current[..c])?;
        return Ok(plan);
    }

    /// Decodes the normalised band shapes into `x_norm` (`n` bins per coded channel), the final
    /// energy refinement and the anti-collapse flag of a non-silent frame
    /// ([RFC 6716 Section 4.3.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4>),
    /// [Section 4.3.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.2.2>),
    /// [Section 4.3.5](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.5>),
    /// [RFC 8251 Section 10](<https://datatracker.ietf.org/doc/html/rfc8251#section-10>)).
    fn decode_shapes(
        &mut self,
        dec: &mut entropy::Decoder<'_>,
        lm: BlockSize,
        header: FrameHeader,
        total_bits: i32,
        plan: &SpectrumPlan,
        x_norm: &mut [f32; MAX_CHANNELS * MAX_FRAME_SIZE],
    ) -> Result<()> {
        let Config { stream_channels: c, start_band: start, end_band: end, .. } = self.config;
        let n = lm.frame_samples();
        let alloc = &plan.alloc;

        let mut collapse_masks = [0u8; MAX_CHANNELS * MAX_BANDS];
        let (x_ch0, x_ch1) = x_norm[..(2 * n)].split_at_mut(n);
        let frame = ShapeFrame {
            start,
            end,
            lm,
            blocks: if header.is_transient { lm.blocks() } else { 1 },
            spread: plan.spread,
            tf_res: plan.tf_res,
            total_bits: (total_bits << BITRES) - plan.anti_collapse_rsv,
            phase_inversion: self.config.output_channels == 2,
        };
        unquantize_bands(
            &frame,
            alloc,
            dec,
            Planes::new(x_ch0, (c == 2).then_some(&mut x_ch1[..])),
            &mut collapse_masks[..(c * MAX_BANDS)],
            &mut self.rng,
        )?;

        let anti_collapse_on =
            if plan.anti_collapse_rsv > 0 { dec.decode_raw_bits(1)? != 0 } else { false };

        let bits_left = total_bits - dec.tell();
        let band_e = &mut self.energy.current[..c];
        let (fine_quant, priority) = (&alloc.fine_quant, &alloc.fine_priority);
        unquant_energy_finalise(dec, start..end, fine_quant, priority, bits_left, band_e)?;

        if anti_collapse_on {
            let masks = &collapse_masks[..(c * MAX_BANDS)];
            let x = &mut x_norm[..(c * n)];
            anti_collapse(x, masks, &frame, c, &self.energy, &alloc.pulses, self.rng);
        }
        return Ok(());
    }

    /// Folds a pending concealed overlap tail, runs the inverse MDCT and the comb post-filter on
    /// the decoded spectrum, and commits the band energies to the history
    /// ([RFC 6716 Section 4.3.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7>)).
    fn synthesize(&mut self, x_norm: &[f32], lm: BlockSize, header: FrameHeader) {
        let n = lm.frame_samples();
        let Config { output_channels: cc, stream_channels: c, start_band, end_band, .. } =
            self.config;

        if self.synthesis.prefilter_and_fold_next {
            self.postfilter.prefilter_and_fold(&mut self.synthesis.decode_mem[..cc], n);
        }
        let bands = start_band..end_band.min(MAX_BANDS);
        self.synthesize_mdct(&x_norm[..(c * n)], lm, c, bands, header.is_transient, header.silence);

        self.postfilter.apply(&mut self.synthesis.decode_mem[..cc], n, lm, header.postfilter);
        let loss_duration = self.plc.loss_duration;
        self.energy.commit_frame(c, start_band..end_band, lm, loss_duration, header.is_transient);
        return;
    }

    /// Conceals a lost CELT frame of block size `lm` into `out`, returning the output samples
    /// written per channel (`frame_samples / downsample`). Pitch-based LPC extrapolation conceals
    /// the first 100 ms of a loss in CELT-only mode once two frames in a row decoded; band-energy
    /// noise conceals every other loss
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    fn conceal_frame(&mut self, lm: BlockSize, out: Planes<'_>, mix: Mix) -> Result<usize> {
        let n = lm.frame_samples();
        let out_samples = n / self.config.downsample;

        match self.plc.uses_noise(self.config.start_band) {
            true => self.conceal_noise_based(lm),
            false => {
                let cc = self.config.output_channels;
                self.plc.conceal_pitch_based(&mut self.synthesis.decode_mem, cc, n);
                self.synthesis.prefilter_and_fold_next = true;
            }
        }
        self.plc.record_loss(lm);

        self.deemphasize(n, out, mix);
        return Ok(out_samples);
    }

    /// Conceals a lost frame with band-energy noise decayed towards the background floor, then
    /// synthesizes and post-filters it with the current comb filter setting
    /// ([RFC 6716 Section 4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.4>)).
    fn conceal_noise_based(&mut self, lm: BlockSize) {
        let n = lm.frame_samples();
        let Config { output_channels: cc, start_band: start, end_band: end, .. } = self.config;
        let eff_end = start.max(end.min(MAX_BANDS));

        self.synthesis.shift_history(cc, n);
        if self.synthesis.prefilter_and_fold_next {
            self.postfilter.prefilter_and_fold(&mut self.synthesis.decode_mem[..cc], n);
        }
        self.energy.decay_for_concealment(cc, start..end, self.plc.noise_energy_decay());

        let mut x_norm = [0.0_f32; MAX_CHANNELS * MAX_FRAME_SIZE];
        self.rng = fill_noise_spectrum(&mut x_norm, cc, n, (start, eff_end), lm, self.rng);
        self.synthesize_mdct(&x_norm[..(cc * n)], lm, cc, start..eff_end, false, false);

        let held = self.postfilter.held();
        self.postfilter.apply(&mut self.synthesis.decode_mem[..cc], n, lm, held);

        self.synthesis.prefilter_and_fold_next = false;
        self.plc.end_noise_concealment();
        return;
    }

    /// Validates that `out.left` (and `out.right` when `output_channels == 2`) have capacity for
    /// at least `out_samples` samples.
    #[inline]
    fn validate_output_capacity(&self, out_samples: usize, out: &Planes<'_>) -> Result<()> {
        if out.left.len() < out_samples {
            return Err(Error::BufferTooSmall.into());
        }
        if self.config.output_channels == 2 {
            match out.right.as_deref() {
                Some(right) if right.len() >= out_samples => {}
                _ => return Err(Error::BufferTooSmall.into()),
            }
        }
        return Ok(());
    }

    /// Denormalises the `c` coded channels of `x_norm` over `bands` and runs the inverse MDCT
    /// with overlap-add into the synthesis memory, duplicating a mono stream to stereo output or
    /// averaging a stereo stream to mono output
    /// ([RFC 6716 Section 4.3.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.6>),
    /// [Section 4.3.7](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7>)).
    fn synthesize_mdct(
        &mut self,
        x_norm: &[f32],
        lm: BlockSize,
        coded_channels: usize,
        bands: Range<usize>,
        is_transient: bool,
        silence: bool,
    ) {
        let n = lm.frame_samples();
        let out_offset = DECODE_BUFFER_SIZE - n;
        let layout = Denormalisation {
            start: bands.start,
            end: bands.end,
            lm,
            downsample: self.config.downsample,
            silence,
        };
        let band_e = &self.energy.current;
        let denormalise = |ch: usize, freq: &mut [f32]| {
            let x = x_norm.chunks_exact(n).nth(ch).unwrap_or(&[]);
            denormalise_bands(x, freq, &band_e[ch], &layout);
        };
        let mut freq = [[0.0_f32; MAX_FRAME_SIZE]; MAX_CHANNELS];

        let channels = match (self.config.output_channels, coded_channels) {
            (2, 1) => {
                denormalise(0, &mut freq[0][..n]);
                freq[1] = freq[0];
                2
            }
            (1, 2) => {
                let [left, right] = &mut freq;
                denormalise(0, &mut left[..n]);
                denormalise(1, &mut right[..n]);
                for (f0, &f1) in left[..n].iter_mut().zip(right[..n].iter()) {
                    *f0 = 0.5_f32 * (*f0 + f1);
                }
                1
            }
            (cc, _) => {
                (0..cc).for_each(|ch| denormalise(ch, &mut freq[ch][..n]));
                cc
            }
        };

        for (spectrum, channel) in
            freq.iter().zip(self.synthesis.decode_mem.iter_mut()).take(channels)
        {
            let window = &mut channel[out_offset..(out_offset + n + OVERLAP)];
            mdct_backward_frame(&spectrum[..n], window, lm, is_transient);
        }
        return;
    }

    /// De-emphasizes and decimates the `n` newest samples of each output channel into `out`,
    /// adding onto its contents for [`Mix::Accumulate`]
    /// ([RFC 6716 Section 4.3.7.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.7.2>)).
    fn deemphasize(&mut self, n: usize, out: Planes<'_>, mix: Mix) {
        let out_offset = DECODE_BUFFER_SIZE - n;
        let downsample = self.config.downsample;
        let out_samples = n / downsample;
        let Planes { left, right } = out;
        let right = if self.config.output_channels == 2 { right } else { None };
        let SynthesisMemory { decode_mem, preemph_mem, .. } = &mut self.synthesis;

        let planes = std::iter::once(left).chain(right);
        for ((plane, channel), mem) in planes.zip(decode_mem.iter()).zip(preemph_mem.iter_mut()) {
            let input = &channel[out_offset..DECODE_BUFFER_SIZE];
            if mix == Mix::Overwrite {
                deemphasis_channel(input, &mut plane[..out_samples], downsample, mem);
                continue;
            }
            let mut tmp_pcm = [0.0_f32; MAX_FRAME_SIZE];
            deemphasis_channel(input, &mut tmp_pcm[..out_samples], downsample, mem);
            for (dst, &src) in plane[..out_samples].iter_mut().zip(tmp_pcm[..out_samples].iter()) {
                *dst += src;
            }
        }
        return;
    }
}

#[cfg(test)]
mod tests;
