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
        if header.silence {
            self.synthesis.shift_history(self.config.output_channels, n);
            self.energy.silence(self.config.stream_channels);
        } else {
            self.decode_spectrum(dec, lm, header, total_bits, &mut x_norm)?;
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
        let bands = (start_band, end_band.min(MAX_BANDS));
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

        if self.plc.uses_noise(self.config.start_band) {
            self.conceal_noise_based(lm);
        } else {
            let cc = self.config.output_channels;
            self.plc.conceal_pitch_based(&mut self.synthesis.decode_mem, cc, n);
            self.synthesis.prefilter_and_fold_next = true;
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
        self.synthesize_mdct(&x_norm[..(cc * n)], lm, cc, (start, eff_end), false, false);

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
        c: usize,
        (start, end): (usize, usize),
        is_transient: bool,
        silence: bool,
    ) {
        let n = lm.frame_samples();
        let out_offset = DECODE_BUFFER_SIZE - n;
        let layout =
            Denormalisation { start, end, lm, downsample: self.config.downsample, silence };
        let band_e = &self.energy.current;
        let denormalise = |ch: usize, freq: &mut [f32]| {
            let x = &x_norm[(ch * n)..((ch + 1) * n)];
            denormalise_bands(x, freq, &band_e[ch], &layout);
        };
        let mut freq = [[0.0_f32; MAX_FRAME_SIZE]; MAX_CHANNELS];

        let channels = match (self.config.output_channels, c) {
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
#[allow(clippy::excessive_precision)] // recorded values keep the 9 significant digits of the dump
mod tests {
    // Fixtures: packets and expected values recorded from libopus 1.6.1. Decoding of received
    // frames is normative (RFC 6716 Section 4.3) and is verified against the RFC 8251 test
    // vectors by `tests/rfc6716_conformance.rs`; the concealment steps are regression pins for
    // behaviour that RFC 6716 Section 4.4 leaves to the implementation.
    use super::*;

    /// Frame layout for a test decode: `frame_size_48k` samples at 48 kHz, `start_band`,
    /// `bandwidth` and `stream_channels` as the TOC would give them.
    fn layout(
        frame_size_48k: usize,
        start_band: usize,
        bandwidth: Bandwidth,
        stream_channels: usize,
    ) -> FrameLayout {
        return FrameLayout {
            block: BlockSize::from_frame_size_48k(frame_size_48k).expect("valid frame size"),
            start_band,
            bandwidth,
            stream_channels,
        };
    }

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
    fn celt_mono_multiframe_decodes_and_conceals_as_recorded() {
        let mut dec = Decoder::new(SampleRate::Hz48000, 1).expect("valid mono decoder");
        let mut out = [0.0_f32; 960];

        // 1. Decode 2.5 ms (120), 5 ms (240), 10 ms (480, transient), and 20 ms (960) frames.
        for (pkt, n, exp_head, exp_tail, exp_rng) in [
            (&MONO_PKT_120[..], 120usize, &MONO_HEAD_120, &MONO_TAIL_120, MONO_RNG_120),
            (&MONO_PKT_240[..], 240usize, &MONO_HEAD_240, &MONO_TAIL_240, MONO_RNG_240),
            (&MONO_PKT_480[..], 480usize, &MONO_HEAD_480, &MONO_TAIL_480, MONO_RNG_480),
            (&MONO_PKT_960[..], 960usize, &MONO_HEAD_960, &MONO_TAIL_960, MONO_RNG_960),
        ] {
            let written = dec
                .decode_frame(
                    FrameInput::Packet(pkt),
                    layout(n, 0, Bandwidth::Fullband, 1),
                    Planes::mono(&mut out[..n]),
                    Mix::Overwrite,
                )
                .expect("decode mono frame");
            assert_eq!(written, n);
            assert_eq!(dec.final_range(), exp_rng);
            assert_f32_slice_close(&out[..8], exp_head, 2e-5);
            assert_f32_slice_close(&out[(n - 8)..n], exp_tail, 2e-5);
        }

        // 2. Conceal 6 consecutive 20 ms lost frames:
        //    - i = 0..4 use pitch-based LPC extrapolation (loss_duration = 0, 8, 16, 24, 32 < 40).
        //    - i = 5 transitions to spectral LCG noise concealment (loss_duration = 40 >= 40).
        for i in 0..6 {
            let written = dec
                .decode_frame(
                    FrameInput::Lost,
                    layout(960, 0, Bandwidth::Fullband, 1),
                    Planes::mono(&mut out),
                    Mix::Overwrite,
                )
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
            .decode_frame(
                FrameInput::Packet(&RECOVERY_PKT),
                layout(960, 0, Bandwidth::Fullband, 1),
                Planes::mono(&mut out),
                Mix::Overwrite,
            )
            .expect("decode recovery frame");
        assert_eq!(written, 960);
        assert_eq!(dec.final_range(), RECOVERY_RNG);
        assert_f32_slice_close(&out[..8], &RECOVERY_HEAD, 2e-5);
        assert_f32_slice_close(&out[952..960], &RECOVERY_TAIL, 2e-5);
        return;
    }

    #[test]
    fn celt_stereo_silence_and_mono_in_stereo_match_recorded_values() {
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

        let mut dec = Decoder::new(SampleRate::Hz48000, 2).expect("valid stereo decoder");
        let mut out_l = [0.0_f32; 960];
        let mut out_r = [0.0_f32; 960];

        // 1. 10 ms stereo transient frame (N = 480)
        let n1 = dec
            .decode_frame(
                FrameInput::Packet(&STEREO_PKT_480),
                layout(480, 0, Bandwidth::Fullband, 2),
                Planes::new(&mut out_l[..480], Some(&mut out_r[..480])),
                Mix::Overwrite,
            )
            .expect("decode stereo 480");
        assert_eq!(n1, 480);
        assert_eq!(dec.final_range(), STEREO_RNG_480);
        assert_f32_slice_close(&out_l[472..480], &STEREO_L_TAIL_480, 2e-5);
        assert_f32_slice_close(&out_r[472..480], &STEREO_R_TAIL_480, 2e-5);

        // 2. 20 ms stereo steady-state frame (N = 960)
        let n2 = dec
            .decode_frame(
                FrameInput::Packet(&STEREO_PKT_960),
                layout(960, 0, Bandwidth::Fullband, 2),
                Planes::new(&mut out_l, Some(&mut out_r[..])),
                Mix::Overwrite,
            )
            .expect("decode stereo 960");
        assert_eq!(n2, 960);
        assert_eq!(dec.final_range(), STEREO_RNG_960);
        assert_f32_slice_close(&out_l[952..960], &STEREO_L_TAIL_960, 2e-5);
        assert_f32_slice_close(&out_r[952..960], &STEREO_R_TAIL_960, 2e-5);

        // 3. Silence frame ([0xFF]) on active stereo decoder smoothly fades out the MDCT overlap tail.
        let n3 = dec
            .decode_frame(
                FrameInput::new(Some(&[0xFF]), None),
                layout(120, 0, Bandwidth::Fullband, 2),
                Planes::new(&mut out_l[..120], Some(&mut out_r[..120])),
                Mix::Overwrite,
            )
            .expect("decode silence frame");
        assert_eq!(n3, 120);
        assert_eq!(dec.final_range(), SILENCE_RNG);
        assert_f32_slice_close(&out_l[..8], &SILENCE_L_HEAD, 2e-5);
        assert_f32_slice_close(&out_r[..8], &SILENCE_R_HEAD, 2e-5);

        // 4. Mono-in-stereo (stream_channels = 1, output_channels = 2)
        let n4 = dec
            .decode_frame(
                FrameInput::Packet(&MONO_IN_STEREO_PKT),
                layout(240, 0, Bandwidth::Fullband, 1),
                Planes::new(&mut out_l[..240], Some(&mut out_r[..240])),
                Mix::Overwrite,
            )
            .expect("decode mono-in-stereo frame");
        assert_eq!(n4, 240);
        assert_eq!(dec.final_range(), MONO_IN_STEREO_RNG);
        assert_f32_slice_close(&out_l[232..240], &MONO_IN_STEREO_L_TAIL, 2e-5);
        assert_f32_slice_close(&out_r[232..240], &MONO_IN_STEREO_R_TAIL, 2e-5);
        return;
    }

    #[test]
    fn celt_hybrid_highband_on_a_shared_range_decoder_matches_recorded_values() {
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

        let mut dec = Decoder::new(SampleRate::Hz48000, 1).expect("valid mono decoder");

        let mut range_dec = entropy::Decoder::new(&HYBRID_PKT);
        let _ = range_dec.decode_symbol_logp(2).expect("silk bit");
        let _ = range_dec.decode_uint(16).expect("silk uint");

        let mut out_pcm = [0.0_f32; 480];
        for (i, sample) in out_pcm.iter_mut().enumerate() {
            *sample = 0.125_f32 * (((i % 17) as i32 - 8) as f32);
        }

        let written = dec
            .decode_frame(
                FrameInput::Shared(&mut range_dec),
                layout(480, 17, Bandwidth::Fullband, 1),
                Planes::mono(&mut out_pcm),
                Mix::Accumulate,
            )
            .expect("decode hybrid highband");
        assert_eq!(written, 480);
        assert_eq!(dec.final_range(), HYBRID_RNG);
        assert_f32_slice_close(&out_pcm[..8], &HYBRID_HEAD, 2e-5);
        assert_f32_slice_close(&out_pcm[472..480], &HYBRID_TAIL, 2e-5);
        return;
    }

    #[test]
    fn frame_input_classifies_decode_call_arguments() {
        let bytes = [0x12, 0x34];
        let mut shared = entropy::Decoder::new(&bytes);
        let shared_input = FrameInput::new(None, Some(&mut shared));
        assert!(matches!(shared_input, FrameInput::Shared(_)));
        assert!(matches!(FrameInput::new(Some(&[]), Some(&mut shared)), FrameInput::Lost));
        assert!(matches!(FrameInput::new(None, None), FrameInput::Lost));
        assert!(matches!(FrameInput::new(Some(&[]), None), FrameInput::Lost));
        assert!(matches!(FrameInput::new(Some(&[0x7F]), None), FrameInput::Lost));
        assert!(matches!(FrameInput::new(Some(&[0xFF]), None), FrameInput::Silence));
        assert!(matches!(FrameInput::new(Some(&bytes), None), FrameInput::Packet(_)));
        return;
    }

    #[test]
    fn synthesis_memory_reset_and_shift() {
        let mut memory = SynthesisMemory::default();
        memory.decode_mem[0][120] = 1.0;
        memory.decode_mem[1][120] = 2.0;
        memory.shift_history(1, 120);
        assert_eq!((memory.decode_mem[0][0], memory.decode_mem[1][0]), (1.0, 0.0));

        memory.preemph_mem[1] = 0.5;
        memory.prefilter_and_fold_next = true;
        memory.reset();
        assert!(memory.decode_mem.iter().flatten().all(|&s| s == 0.0));
        assert_eq!((memory.preemph_mem, memory.prefilter_and_fold_next), ([0.0; 2], false));
        return;
    }

    #[test]
    fn celt_decoder_configuration_and_boundary_validation() {
        assert!(SampleRate::try_from(44_100).is_err());
        assert!(Error::InvalidChannelCount(0).matches(&Decoder::new(SampleRate::Hz48000, 0)));
        assert!(Error::InvalidChannelCount(3).matches(&Decoder::new(SampleRate::Hz48000, 3)));

        let mut dec = Decoder::new(SampleRate::Hz24000, 1).expect("valid 24 kHz decoder");
        let mut full_buf = [0.0_f32; 480];
        let band_range = Error::InvalidBandRange { start: 0, end: 0 };
        for (bad, expected) in [
            (layout(120, 21, Bandwidth::Wideband, 1), band_range),
            (layout(120, 17, Bandwidth::Narrowband, 1), band_range),
            (layout(120, 0, Bandwidth::Wideband, 0), Error::InvalidChannelCount(0)),
            (layout(120, 0, Bandwidth::Wideband, 3), Error::InvalidChannelCount(3)),
        ] {
            let silence = FrameInput::new(Some(&[0xFF]), None);
            let result =
                dec.decode_frame(silence, bad, Planes::mono(&mut full_buf), Mix::Overwrite);
            assert!(expected.matches(&result));
        }

        dec.reset();
        assert_eq!(dec.final_range(), 0);

        let mut small_buf = [0.0_f32; 10];
        // At 24 kHz (downsample = 2), frame_size_48k = 120 requires 60 output samples.
        let silence = FrameInput::new(Some(&[0xFF]), None);
        let wideband = layout(120, 0, Bandwidth::Wideband, 1);
        let result =
            dec.decode_frame(silence, wideband, Planes::mono(&mut small_buf), Mix::Overwrite);
        assert!(Error::BufferTooSmall.matches(&result));
        // Invalid 48 kHz MDCT size (e.g. 360) must be rejected.
        assert!(Error::InvalidMdctSize(360).matches(&BlockSize::from_frame_size_48k(360)));
        return;
    }
}
