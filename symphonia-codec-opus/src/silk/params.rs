//! SILK frame parameters: header flags, stereo prediction weights, mid-only flag, frame type,
//! subframe gains and long-term prediction parameters
//! ([RFC 6716 Section 4.2.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.3>),
//! [Section 4.2.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.4>),
//! [Section 4.2.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.1>) to
//! [Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>),
//! [Section 4.2.7.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6>)).
//!
//! Parameters coded relative to the previous frame of the same channel read and update
//! [`CodingReferences`]; everything else is a pure function of the range decoder.

use symphonia_core::errors::Result;

use crate::entropy::{self, Cdf};
use crate::silk::constant;
use crate::silk::error::Error;
use crate::silk::{
    ChannelHeader, CodingMode, CodingReferences, Frame, FrameType, LTP_ORDER, MAX_SILK_FRAMES,
    MAX_SUBFRAMES, QuantizationOffsetType, SignalType, SilkHeaderFlags, SilkLayout, StereoWeights,
};
use crate::toc::{Bandwidth, FrameDuration};

/// Q16 fraction of the interval between two stereo weight codebook entries covered by one fine
/// quantization step, `0.1` (RFC 6716 Section 4.2.7.1).
const STEREO_WEIGHT_STEP_Q16: i32 = 6554;

/// Number of fine stereo weight steps per coarse codebook interval (RFC 6716 Section 4.2.7.1).
const STEREO_WEIGHT_COARSE_STEPS: usize = 3;

/// Number of coarse codebook intervals the first stage index is split into (RFC 6716 Section 4.2.7.1).
const STEREO_WEIGHT_STAGE_ONE_SPLIT: usize = 5;

/// Highest quantized log gain index (RFC 6716 Section 4.2.7.4).
const LOG_GAIN_MAX: i32 = 63;

/// Largest drop of an independently coded log gain below the previous one (RFC 6716 Section 4.2.7.4).
const INDEPENDENT_LOG_GAIN_MAX_DROP: i32 = 16;

/// Bits of the least significant part of an independently coded gain index (RFC 6716 Section 4.2.7.4).
const GAIN_INDEX_LSB_BITS: u32 = 3;

/// Offset of the doubled delta gain index, `2 * delta_gain_index - 16` (RFC 6716 Section 4.2.7.4).
const DELTA_LOG_GAIN_DOUBLE_OFFSET: i32 = 16;

/// Offset of the accumulated delta gain index, `previous_log_gain + delta_gain_index - 4`
/// (RFC 6716 Section 4.2.7.4).
const DELTA_LOG_GAIN_OFFSET: i32 = 4;

/// Inverse of the 64-step log gain quantizer scale in Q16, `(65536 * ((88 - 2) * 128 / 6)) / 63`
/// ([RFC 6716 Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>)).
const LOG_GAIN_INV_SCALE_Q16: i64 = 1_907_825;

/// Q7 log-domain offset added to the scaled gain index, `2090` (RFC 6716 Section 4.2.7.4).
const LOG_GAIN_OFFSET_Q7: i32 = 2090;

/// Largest Q7 log gain whose linear value fits in 32 bits, `3967` (RFC 6716 Section 4.2.7.4).
const LOG_GAIN_MAX_Q7: i32 = 3967;

/// First Q7 log gain whose linear value, `2^31`, overflows 32 bits (RFC 6716 Section 4.2.7.4).
const LOG2LIN_OVERFLOW_Q7: i32 = 3968;

/// Number of fractional bits of a Q7 logarithm.
const LOG2_Q7_SHIFT: u32 = 7;

/// Mask selecting the fractional bits of a Q7 logarithm (`0x7F`).
const LOG2_Q7_FRAC_MASK: i32 = (1 << LOG2_Q7_SHIFT) - 1;

/// `1.0` in Q7 (`128`).
const LOG2_Q7_ONE: i32 = 1 << LOG2_Q7_SHIFT;

/// Q16 curvature of the parabolic correction of the fractional part, `-174 / 65536`
/// (RFC 6716 Section 4.2.7.4).
const LOG2LIN_PARABOLIC_Q16: i64 = -174;

/// Below this Q7 log gain (`2^16`), the fractional product is shifted after multiplying; above it,
/// the integer part is shifted first so the product cannot overflow (RFC 6716 Section 4.2.7.4).
const LOG2LIN_EXACT_SHIFT_LIMIT_Q7: i32 = 2048;

/// Offset subtracted from a relative pitch lag delta index, `delta_lag_index - 9`
/// (RFC 6716 Section 4.2.7.6.1).
const RELATIVE_LAG_DELTA_OFFSET: i32 = 9;

/// Frame types of an inactive frame, indexed by the frame type symbol
/// ([RFC 6716 Section 4.2.7.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.3>)).
const INACTIVE_FRAME_TYPES: [FrameType; 2] = [
    FrameType::new(SignalType::Inactive, QuantizationOffsetType::Low),
    FrameType::new(SignalType::Inactive, QuantizationOffsetType::High),
];

/// Frame types of an active frame, indexed by the frame type symbol
/// ([RFC 6716 Section 4.2.7.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.3>)).
const ACTIVE_FRAME_TYPES: [FrameType; 4] = [
    FrameType::new(SignalType::Unvoiced, QuantizationOffsetType::Low),
    FrameType::new(SignalType::Unvoiced, QuantizationOffsetType::High),
    FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low),
    FrameType::new(SignalType::Voiced, QuantizationOffsetType::High),
];

/// LTP filter index PDF and codebook of each periodicity index
/// ([RFC 6716 Section 4.2.7.6.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6.2>)).
const LTP_FILTER_CODEBOOKS: [(Cdf<'static>, &[[i8; LTP_ORDER]]); 3] = [
    (constant::CDF_LTP_FILTER_INDEX_0, &constant::CODEBOOK_LTP_FILTER_PERIODICITY_INDEX_0),
    (constant::CDF_LTP_FILTER_INDEX_1, &constant::CODEBOOK_LTP_FILTER_PERIODICITY_INDEX_1),
    (constant::CDF_LTP_FILTER_INDEX_2, &constant::CODEBOOK_LTP_FILTER_PERIODICITY_INDEX_2),
];

// Every decoded symbol below indexes a table with one entry per symbol, so the lookups cannot
// fail: these bounds are proven when the crate compiles.
const _: () =
    assert!(constant::CDF_FRAME_TYPE_VAD_INACTIVE.symbols() == INACTIVE_FRAME_TYPES.len());
const _: () = assert!(constant::CDF_FRAME_TYPE_VAD_ACTIVE.symbols() == ACTIVE_FRAME_TYPES.len());
const _: () = assert!(constant::CDF_PERIODICITY_INDEX.symbols() == LTP_FILTER_CODEBOOKS.len());
const _: () = assert!(
    constant::CDF_LTP_FILTER_INDEX_0.symbols()
        == constant::CODEBOOK_LTP_FILTER_PERIODICITY_INDEX_0.len()
);
const _: () = assert!(
    constant::CDF_LTP_FILTER_INDEX_1.symbols()
        == constant::CODEBOOK_LTP_FILTER_PERIODICITY_INDEX_1.len()
);
const _: () = assert!(
    constant::CDF_LTP_FILTER_INDEX_2.symbols()
        == constant::CODEBOOK_LTP_FILTER_PERIODICITY_INDEX_2.len()
);
const _: () = assert!(
    constant::CDF_LTP_SCALING_PARAMETER.symbols() == constant::CODEBOOK_LTP_SCALING_Q14.len()
);
// The largest stereo weight interval index, `2 + 3 * 4`, leaves its upper boundary in the table.
const _: () = assert!(
    constant::CDF_STEREO_PREDICTION_WEIGHT_STAGE_ONE.symbols()
        <= STEREO_WEIGHT_STAGE_ONE_SPLIT * STEREO_WEIGHT_STAGE_ONE_SPLIT
);
const _: () = assert!(
    (constant::CDF_STEREO_PREDICTION_WEIGHT_STAGE_TWO.symbols() - 1)
        + STEREO_WEIGHT_COARSE_STEPS * (STEREO_WEIGHT_STAGE_ONE_SPLIT - 1)
        + 1
        < constant::CODEBOOK_STEREO_QUANTIZATION_BOUNDARIES_Q13.len()
);

/// Decodes the SILK header bits and optional per-frame LBRR flags at the start of an Opus frame
/// carrying `frame_count` SILK frames
/// ([RFC 6716 Section 4.2.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.3>),
/// [Section 4.2.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.4>)).
pub(crate) fn decode_silk_header(
    decoder: &mut entropy::Decoder<'_>,
    is_stereo: bool,
    frame_count: usize,
) -> Result<SilkHeaderFlags> {
    if frame_count == 0 || frame_count > MAX_SILK_FRAMES {
        return Err(Error::UnsupportedFrameSize.into());
    }
    let num_channels = if is_stereo { 2 } else { 1 };
    let mut channels = [ChannelHeader::default(); 2];

    for channel in channels[..num_channels].iter_mut() {
        for vad_flag in channel.vad_flags[..frame_count].iter_mut() {
            *vad_flag = decoder.decode_symbol_logp(1)? == 1;
        }
        channel.lbrr_flag = decoder.decode_symbol_logp(1)? == 1;
    }

    for channel in channels[..num_channels].iter_mut().filter(|channel| channel.lbrr_flag) {
        channel.per_frame_lbrr = decode_per_frame_lbrr_flags(decoder, frame_count)?;
    }

    return Ok(SilkHeaderFlags { num_channels, num_silk_frames: frame_count, channels });
}

/// Decodes which of the `frame_count` SILK frames of a channel carry an LBRR frame; a single
/// frame is implied by the channel LBRR flag
/// ([RFC 6716 Section 4.2.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.4>)).
fn decode_per_frame_lbrr_flags(
    decoder: &mut entropy::Decoder<'_>,
    frame_count: usize,
) -> Result<[bool; MAX_SILK_FRAMES]> {
    let cdf: Cdf<'static> = match frame_count {
        1 => return Ok([true, false, false]),
        2 => constant::CDF_PER_FRAME_LBRR_FLAGS_40_MS,
        3 => constant::CDF_PER_FRAME_LBRR_FLAGS_60_MS,
        _ => return Err(Error::UnsupportedFrameSize.into()),
    };
    // The symbol is the flag mask minus one; it never sets a bit beyond `frame_count`.
    let mask = decoder.decode_symbol(cdf)? + 1;
    return Ok(std::array::from_fn(|i| ((mask >> i) & 1) != 0));
}

/// Decodes stereo prediction weights (`w0_Q13`, `w1_Q13`) at the start of a stereo SILK frame
/// ([RFC 6716 Section 4.2.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.1>)).
pub(crate) fn decode_stereo_prediction_weights(
    decoder: &mut entropy::Decoder<'_>,
) -> Result<StereoWeights> {
    let n = decoder.decode_symbol(constant::CDF_STEREO_PREDICTION_WEIGHT_STAGE_ONE)? as usize;
    let i0 = decoder.decode_symbol(constant::CDF_STEREO_PREDICTION_WEIGHT_STAGE_TWO)? as usize;
    let i1 = decoder.decode_symbol(constant::CDF_STEREO_PREDICTION_WEIGHT_STAGE_THREE)? as i32;
    let i2 = decoder.decode_symbol(constant::CDF_STEREO_PREDICTION_WEIGHT_STAGE_TWO)? as usize;
    let i3 = decoder.decode_symbol(constant::CDF_STEREO_PREDICTION_WEIGHT_STAGE_THREE)? as i32;

    let wi0 = i0 + STEREO_WEIGHT_COARSE_STEPS * (n / STEREO_WEIGHT_STAGE_ONE_SPLIT);
    let wi1 = i2 + STEREO_WEIGHT_COARSE_STEPS * (n % STEREO_WEIGHT_STAGE_ONE_SPLIT);

    let w1_q13 = stereo_weight_q13(wi1, i3);
    let w0_q13 = stereo_weight_q13(wi0, i1) - w1_q13;

    return Ok(StereoWeights { w0_q13, w1_q13 });
}

/// Dequantizes one stereo weight from codebook interval `boundary_index` and fine step
/// `fine_index` ([RFC 6716 Section 4.2.7.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.1>)).
fn stereo_weight_q13(boundary_index: usize, fine_index: i32) -> i32 {
    let boundaries_q13 = &constant::CODEBOOK_STEREO_QUANTIZATION_BOUNDARIES_Q13;
    let (low_q13, high_q13) = (boundaries_q13[boundary_index], boundaries_q13[boundary_index + 1]);
    let step_q13 = (((high_q13 as i32) - (low_q13 as i32)) * STEREO_WEIGHT_STEP_Q16) >> 16;
    return (low_q13 as i32) + step_q13 * (2 * fine_index + 1);
}

/// Decodes the conditional mid-only stereo flag when the side channel is inactive
/// ([RFC 6716 Section 4.2.7.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.2>)).
pub(crate) fn decode_mid_only_flag(
    decoder: &mut entropy::Decoder<'_>,
    side_active: bool,
) -> Result<bool> {
    if side_active {
        return Ok(false);
    }
    return Ok(decoder.decode_symbol(constant::CDF_STEREO_MID_ONLY)? == 1);
}

/// Decodes the SILK frame type: signal type and quantization offset type
/// ([RFC 6716 Section 4.2.7.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.3>)).
pub(crate) fn decode_frame_type(
    decoder: &mut entropy::Decoder<'_>,
    vad_flag: bool,
    is_lbrr_frame: bool,
) -> Result<FrameType> {
    let active = (constant::CDF_FRAME_TYPE_VAD_ACTIVE, ACTIVE_FRAME_TYPES.as_slice());
    let inactive = (constant::CDF_FRAME_TYPE_VAD_INACTIVE, INACTIVE_FRAME_TYPES.as_slice());
    let (cdf, frame_types) = if vad_flag || is_lbrr_frame { active } else { inactive };
    return Ok(frame_types[decoder.decode_symbol(cdf)? as usize]);
}

impl CodingReferences {
    /// Decodes the quantization gain of every subframe of `frame`, the first one independently
    /// unless the frame is coded conditionally
    /// ([RFC 6716 Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>)).
    pub(crate) fn decode_gains(
        &mut self,
        decoder: &mut entropy::Decoder<'_>,
        frame: &mut Frame,
        coding_mode: CodingMode,
    ) -> Result<()> {
        let signal_type = frame.frame_type.signal_type;
        for (i, subframe) in frame.active_subframes_mut().iter_mut().enumerate() {
            let log_gain = match (i, coding_mode) {
                (0, CodingMode::Independent) => {
                    decode_independent_log_gain(decoder, signal_type, self.last_gain_index)?
                }
                _ => decode_delta_log_gain(decoder, self.last_gain_index)?,
            };
            self.last_gain_index = log_gain;
            subframe.log_gain = log_gain;
            subframe.gain_q16 = gain_q16_from_log_gain(log_gain);
        }
        return Ok(());
    }

    /// Decodes the pitch lags, LTP filter taps and LTP scaling of a voiced frame
    /// ([RFC 6716 Section 4.2.7.6](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6>)).
    pub(crate) fn decode_ltp(
        &mut self,
        decoder: &mut entropy::Decoder<'_>,
        frame: &mut Frame,
        layout: &SilkLayout,
        coding_mode: CodingMode,
    ) -> Result<()> {
        if layout.subframes != frame.num_subframes {
            return Err(Error::MismatchFrameSubframes.into());
        }

        let pitch_lags = self.decode_pitch_lags(decoder, layout, coding_mode)?;
        let ltp_coeffs = decode_ltp_coeffs(decoder, layout.subframes)?;
        frame.ltp_scale_q14 = decode_ltp_scaling(decoder, coding_mode)?;

        for ((subframe, &pitch_lag), &coeffs) in
            frame.active_subframes_mut().iter_mut().zip(pitch_lags.iter()).zip(ltp_coeffs.iter())
        {
            subframe.pitch_lag = pitch_lag;
            subframe.ltp_coeffs = coeffs;
        }
        return Ok(());
    }

    /// Decodes the primary pitch lag and the subframe pitch contour of a voiced frame
    /// ([RFC 6716 Section 4.2.7.6.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6.1>)).
    fn decode_pitch_lags(
        &mut self,
        decoder: &mut entropy::Decoder<'_>,
        layout: &SilkLayout,
        coding_mode: CodingMode,
    ) -> Result<[u16; MAX_SUBFRAMES]> {
        let (min_lag, max_lag) = pitch_lag_bounds(layout.bandwidth);
        let lag_index = self.decode_primary_lag_index(decoder, layout.bandwidth, coding_mode)?;
        let base_lag = min_lag + lag_index;
        let contour = decode_pitch_contour(decoder, layout)?;

        return Ok(std::array::from_fn(|k| {
            contour
                .get(k)
                .map_or(0, |&offset| (base_lag + i32::from(offset)).clamp(min_lag, max_lag) as u16)
        }));
    }

    /// Decodes the primary pitch lag index, relative to the previous frame when this frame is
    /// coded conditionally after a voiced frame and the delta is non-zero, absolutely otherwise
    /// ([RFC 6716 Section 4.2.7.6.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6.1>)).
    fn decode_primary_lag_index(
        &mut self,
        decoder: &mut entropy::Decoder<'_>,
        bandwidth: Bandwidth,
        coding_mode: CodingMode,
    ) -> Result<i32> {
        let relative =
            coding_mode == CodingMode::Conditional && self.prev_signal_type == SignalType::Voiced;
        let delta_lag_index = relative
            .then(|| decoder.decode_symbol(constant::CDF_RELATIVE_DELTA_PITCH_LAG))
            .transpose()?
            .map_or(0, |sym| sym as i32);

        let lag_index = match delta_lag_index {
            0 => decode_absolute_lag_index(decoder, bandwidth)?,
            delta => (self.prev_lag_index as i32) + delta - RELATIVE_LAG_DELTA_OFFSET,
        };

        self.prev_lag_index = lag_index as i16;
        return Ok(lag_index);
    }
}

/// Decodes an independently coded log gain index from its most and least significant parts,
/// limited to a drop of 16 below `prev_log_gain`
/// ([RFC 6716 Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>)).
fn decode_independent_log_gain(
    decoder: &mut entropy::Decoder<'_>,
    signal_type: SignalType,
    prev_log_gain: i32,
) -> Result<i32> {
    let msb_cdf: Cdf<'static> = match signal_type {
        SignalType::Inactive => constant::CDF_INDEPENDENT_QUANTIZATION_GAIN_MSB_INACTIVE,
        SignalType::Unvoiced => constant::CDF_INDEPENDENT_QUANTIZATION_GAIN_MSB_UNVOICED,
        SignalType::Voiced => constant::CDF_INDEPENDENT_QUANTIZATION_GAIN_MSB_VOICED,
    };
    let gain_msb = decoder.decode_symbol(msb_cdf)? as i32;
    let gain_lsb = decoder.decode_symbol(constant::CDF_INDEPENDENT_QUANTIZATION_GAIN_LSB)? as i32;
    let gain_index = (gain_msb << GAIN_INDEX_LSB_BITS) + gain_lsb;
    return Ok(gain_index
        .max(prev_log_gain - INDEPENDENT_LOG_GAIN_MAX_DROP)
        .clamp(0, LOG_GAIN_MAX));
}

/// Decodes a delta coded log gain index,
/// `clamp(0, max(2 * delta - 16, prev_log_gain + delta - 4), 63)`
/// ([RFC 6716 Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>)).
fn decode_delta_log_gain(decoder: &mut entropy::Decoder<'_>, prev_log_gain: i32) -> Result<i32> {
    let delta_gain = decoder.decode_symbol(constant::CDF_DELTA_QUANTIZATION_GAIN)? as i32;
    return Ok((2 * delta_gain - DELTA_LOG_GAIN_DOUBLE_OFFSET)
        .max(prev_log_gain + delta_gain - DELTA_LOG_GAIN_OFFSET)
        .clamp(0, LOG_GAIN_MAX));
}

/// Converts a quantized log gain index (`0..=63`) into a Q16 linear gain
/// ([RFC 6716 Section 4.2.7.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.4>)).
///
/// The index is mapped onto a Q7 base-2 logarithm, `inLog_Q7`, and the result is
/// `2^(inLog_Q7 / 128)` from the integer part as a power of two and a parabolic
/// approximation of the fractional part.
#[inline]
pub(crate) fn gain_q16_from_log_gain(log_gain: i32) -> i32 {
    let in_log_q7 = (((LOG_GAIN_INV_SCALE_Q16 * (log_gain as i64)) >> 16) as i32
        + LOG_GAIN_OFFSET_Q7)
        .min(LOG_GAIN_MAX_Q7);
    if in_log_q7 < 0 {
        return 0;
    }
    if in_log_q7 >= LOG2LIN_OVERFLOW_Q7 {
        return i32::MAX;
    }

    let integer_part = 1_i32 << (in_log_q7 >> LOG2_Q7_SHIFT);
    let frac_q7 = in_log_q7 & LOG2_Q7_FRAC_MASK;
    let frac_correction_q7 = frac_q7
        + ((((frac_q7 * (LOG2_Q7_ONE - frac_q7)) as i64 * LOG2LIN_PARABOLIC_Q16) >> 16) as i32);
    let exact = in_log_q7 < LOG2LIN_EXACT_SHIFT_LIMIT_Q7;
    let (int, corr) = (integer_part, frac_correction_q7);
    let fractional_part =
        if exact { (int * corr) >> LOG2_Q7_SHIFT } else { (int >> LOG2_Q7_SHIFT) * corr };
    return integer_part + fractional_part;
}

/// Per-bandwidth pitch lag bounds and low-part range-coder parameters
/// ([RFC 6716 Section 4.2.7.6.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6.1>)).
struct PitchLagSpec {
    min_lag: i32,
    max_lag: i32,
    low_cdf: Cdf<'static>,
    low_part_count: i32,
}

impl PitchLagSpec {
    const fn new(min_lag: i32, max_lag: i32, low_cdf: Cdf<'static>, low_part_count: i32) -> Self {
        assert!(min_lag > 0 && min_lag < max_lag);
        assert!(low_part_count > 0 && low_cdf.symbols() == (low_part_count as usize));
        return Self { min_lag, max_lag, low_cdf, low_part_count };
    }
}

/// Pitch lag descriptors for narrowband (`0`), mediumband (`1`) and wideband (`2`).
static PITCH_LAG_SPECS: [PitchLagSpec; 3] = [
    PitchLagSpec::new(16, 144, constant::CDF_PRIMARY_PITCH_LAG_LOW_PART_NARROWBAND, 4),
    PitchLagSpec::new(24, 216, constant::CDF_PRIMARY_PITCH_LAG_LOW_PART_MEDIUMBAND, 6),
    PitchLagSpec::new(32, 288, constant::CDF_PRIMARY_PITCH_LAG_LOW_PART_WIDEBAND, 8),
];

/// Per-subframe pitch contour codebook rows for 10 ms (2 subframes) or 20 ms (4 subframes) frames.
#[derive(Clone, Copy)]
enum ContourTable {
    Ms10(&'static [[i8; 2]]),
    Ms20(&'static [[i8; 4]]),
}

impl ContourTable {
    const fn len(self) -> usize {
        return match self {
            Self::Ms10(rows) => rows.len(),
            Self::Ms20(rows) => rows.len(),
        };
    }

    #[inline]
    fn row(self, i: usize) -> &'static [i8] {
        return match self {
            Self::Ms10(rows) => &rows[i],
            Self::Ms20(rows) => &rows[i],
        };
    }
}

/// Pitch contour CDF and per-subframe offset codebook for one `(bandwidth, duration)` slot
/// ([RFC 6716 Section 4.2.7.6.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6.1>)).
struct PitchContourSpec {
    cdf: Cdf<'static>,
    table: ContourTable,
}

impl PitchContourSpec {
    const fn new(cdf: Cdf<'static>, table: ContourTable) -> Self {
        assert!(cdf.symbols() == table.len());
        return Self { cdf, table };
    }
}

/// Pitch contour codebooks indexed `[usize::from(bandwidth != Narrowband)][usize::from(duration != Ms10)]`.
static PITCH_CONTOUR_CODEBOOKS: [[PitchContourSpec; 2]; 2] = [
    [
        PitchContourSpec::new(
            constant::CDF_SUBFRAME_PITCH_CONTOUR_NARROWBAND_10_MS,
            ContourTable::Ms10(&constant::CODEBOOK_SUBFRAME_PITCH_CONTOUR_NB_10MS),
        ),
        PitchContourSpec::new(
            constant::CDF_SUBFRAME_PITCH_CONTOUR_NARROWBAND_20_MS,
            ContourTable::Ms20(&constant::CODEBOOK_SUBFRAME_PITCH_CONTOUR_NARROWBAND_20MS),
        ),
    ],
    [
        PitchContourSpec::new(
            constant::CDF_SUBFRAME_PITCH_CONTOUR_MEDIUMBAND_OR_WIDEBAND_10_MS,
            ContourTable::Ms10(&constant::CODEBOOK_SUBFRAME_PITCH_CONTOUR_MEDIUMBAND_WIDEBAND_10MS),
        ),
        PitchContourSpec::new(
            constant::CDF_SUBFRAME_PITCH_CONTOUR_MEDIUMBAND_OR_WIDEBAND_20_MS,
            ContourTable::Ms20(&constant::CODEBOOK_SUBFRAME_PITCH_CONTOUR_MEDIUMBAND_WIDEBAND_20MS),
        ),
    ],
];

/// Slot (`0..=2`) of `bandwidth` in [`PITCH_LAG_SPECS`].
#[inline]
const fn silk_band_slot(bandwidth: Bandwidth) -> usize {
    return match bandwidth {
        Bandwidth::Narrowband => 0,
        Bandwidth::MediumBand => 1,
        Bandwidth::Wideband | Bandwidth::SuperWideband | Bandwidth::Fullband => 2,
    };
}

/// Smallest and largest pitch lag in samples at the internal rate of `bandwidth`
/// ([RFC 6716 Section 4.2.7.6.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6.1>)).
#[inline]
const fn pitch_lag_bounds(bandwidth: Bandwidth) -> (i32, i32) {
    let spec = &PITCH_LAG_SPECS[silk_band_slot(bandwidth)];
    return (spec.min_lag, spec.max_lag);
}

/// Decodes an absolute primary pitch lag index from its high part and its bandwidth dependent
/// low part ([RFC 6716 Section 4.2.7.6.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6.1>)).
fn decode_absolute_lag_index(
    decoder: &mut entropy::Decoder<'_>,
    bandwidth: Bandwidth,
) -> Result<i32> {
    let high_part = decoder.decode_symbol(constant::CDF_PRIMARY_PITCH_LAG_HIGH_PART)? as i32;
    let spec = &PITCH_LAG_SPECS[silk_band_slot(bandwidth)];
    let low_part = decoder.decode_symbol(spec.low_cdf)? as i32;
    return Ok(high_part * spec.low_part_count + low_part);
}

/// Decodes the subframe pitch contour, the per-subframe offsets from the primary lag, from the
/// codebook of the layout's bandwidth and frame duration
/// ([RFC 6716 Section 4.2.7.6.1](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6.1>)).
fn decode_pitch_contour(
    decoder: &mut entropy::Decoder<'_>,
    layout: &SilkLayout,
) -> Result<&'static [i8]> {
    let wide = usize::from(layout.bandwidth != Bandwidth::Narrowband);
    let long = usize::from(layout.frame_duration != FrameDuration::Ms10);
    let spec = &PITCH_CONTOUR_CODEBOOKS[wide][long];
    let row = decoder.decode_symbol(spec.cdf)? as usize;
    return Ok(spec.table.row(row));
}

/// Decodes the periodicity index and one 5-tap LTP filter per subframe
/// ([RFC 6716 Section 4.2.7.6.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6.2>)).
fn decode_ltp_coeffs(
    decoder: &mut entropy::Decoder<'_>,
    subframes: usize,
) -> Result<[[i8; LTP_ORDER]; MAX_SUBFRAMES]> {
    let periodicity_index = decoder.decode_symbol(constant::CDF_PERIODICITY_INDEX)? as usize;
    let (cdf, codebook) = LTP_FILTER_CODEBOOKS[periodicity_index];

    let mut ltp_coeffs = [[0i8; LTP_ORDER]; MAX_SUBFRAMES];
    for subframe_coeffs in ltp_coeffs.iter_mut().take(subframes) {
        let filter_index = decoder.decode_symbol(cdf)? as usize;
        *subframe_coeffs = codebook[filter_index];
    }
    return Ok(ltp_coeffs);
}

/// Decodes the Q14 LTP scaling parameter of an independently coded frame; conditionally coded
/// frames and LBRR side frames after a coded frame use the first codebook entry
/// ([RFC 6716 Section 4.2.7.6.3](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.7.6.3>)).
fn decode_ltp_scaling(decoder: &mut entropy::Decoder<'_>, coding_mode: CodingMode) -> Result<i16> {
    let i = match coding_mode {
        CodingMode::Independent => {
            decoder.decode_symbol(constant::CDF_LTP_SCALING_PARAMETER)? as usize
        }
        CodingMode::IndependentWithoutLtpScaling | CodingMode::Conditional => 0,
    };
    return Ok(constant::CODEBOOK_LTP_SCALING_Q14[i]);
}

#[cfg(test)]
mod tests {
    // Fixtures: packets and expected values recorded from libopus 1.6.1. The behaviour they pin is
    // normative (RFC 6716 Section 4.2.7) and is verified against the RFC 8251 test vectors by
    // `tests/rfc6716_conformance.rs`.
    use super::*;

    /// Layout of an Opus frame of `frame_duration` coded at `bandwidth`.
    fn layout(bandwidth: Bandwidth, frame_duration: FrameDuration) -> SilkLayout {
        return SilkLayout::new(bandwidth, frame_duration).unwrap();
    }

    /// Voiced frame of `samples` samples in `subframes` subframes with an LPC filter of
    /// `lpc_order`.
    fn voiced_frame(samples: usize, subframes: usize, lpc_order: usize) -> Frame {
        let mut frame = Frame::new(samples, subframes, lpc_order);
        frame.frame_type = FrameType::new(SignalType::Voiced, QuantizationOffsetType::Low);
        return frame;
    }

    macro_rules! assert_silk_gains {
        (
            $state:expr,
            $dec:expr,
            samples: $samples:expr,
            subframes: $subframes:expr,
            lpc_order: $lpc_order:expr,
            mode: $mode:expr,
            expected_log_gains: $expected_log:expr,
            expected_gains_q16: $expected_q16:expr,
            expected_last_gain_index: $expected_last:expr
        ) => {{
            let mut frame = voiced_frame($samples, $subframes, $lpc_order);
            $state.decode_gains(&mut $dec, &mut frame, $mode).unwrap();
            let log_gains: Vec<i32> = frame.active_subframes().iter().map(|s| s.log_gain).collect();
            let gains_q16: Vec<i32> = frame.active_subframes().iter().map(|s| s.gain_q16).collect();
            assert_eq!(log_gains.as_slice(), &$expected_log);
            assert_eq!(gains_q16.as_slice(), &$expected_q16);
            assert_eq!($state.last_gain_index, $expected_last);
        }};
    }

    #[test]
    fn silk_header_mono_10ms_and_20ms_flags() {
        // Range-coded symbols:
        // HEADER_MONO_20MS_BYTES = [0x80]: [VAD_0 = 1, LBRR = 0]
        // HEADER_MONO_10MS_LBRR_BYTES = [0x40]: [VAD_0 = 0, LBRR = 1] -> implicit per_frame_lbrr[0][0] = true
        const HEADER_MONO_20MS_BYTES: [u8; 1] = [0x80];
        const HEADER_MONO_10MS_LBRR_BYTES: [u8; 1] = [0x40];

        let mut dec_20 = entropy::Decoder::new(&HEADER_MONO_20MS_BYTES);
        let header = decode_silk_header(&mut dec_20, false, 1).unwrap();

        assert_eq!(header.num_channels, 1);
        assert_eq!(header.num_silk_frames, 1);
        assert_eq!(header.channels[0].vad_flags, [true, false, false]);
        assert!(!header.channels[0].lbrr_flag);
        assert_eq!(header.channels[0].per_frame_lbrr, [false, false, false]);

        let mut dec_10 = entropy::Decoder::new(&HEADER_MONO_10MS_LBRR_BYTES);
        let header_lbrr = decode_silk_header(&mut dec_10, false, 1).unwrap();

        assert!(header_lbrr.channels[0].lbrr_flag);
        assert_eq!(header_lbrr.channels[0].per_frame_lbrr, [true, false, false]);
        assert!(header_lbrr.has_frame_lbrr(0));
    }

    #[test]
    fn silk_header_mono_40ms_and_60ms_per_frame_lbrr_masks() {
        // Range-coded symbols:
        // HEADER_MONO_40MS_LBRR_BYTES = [0xa8]: VAD=[1, 0], LBRR=1, Table 4 sym=1 -> mask=2 (0b10)
        // HEADER_MONO_60MS_LBRR_BYTES = [0xd8, 0x80]: VAD=[1, 1, 0], LBRR=1, Table 4 sym=4 -> mask=5 (0b101)
        const HEADER_MONO_40MS_LBRR_BYTES: [u8; 1] = [0xa8];
        const HEADER_MONO_60MS_LBRR_BYTES: [u8; 2] = [0xd8, 0x80];

        let mut dec_40 = entropy::Decoder::new(&HEADER_MONO_40MS_LBRR_BYTES);
        let header_40 = decode_silk_header(&mut dec_40, false, 2).unwrap();

        assert_eq!(header_40.num_silk_frames, 2);
        assert_eq!(header_40.channels[0].vad_flags, [true, false, false]);
        assert_eq!(header_40.channels[0].per_frame_lbrr, [false, true, false]);
        assert!(!header_40.has_frame_lbrr(0));
        assert!(header_40.has_frame_lbrr(1));

        let mut dec_60 = entropy::Decoder::new(&HEADER_MONO_60MS_LBRR_BYTES);
        let header_60 = decode_silk_header(&mut dec_60, false, 3).unwrap();

        assert_eq!(header_60.num_silk_frames, 3);
        assert_eq!(header_60.channels[0].vad_flags, [true, true, false]);
        assert_eq!(header_60.channels[0].per_frame_lbrr, [true, false, true]);
        assert!(header_60.has_frame_lbrr(0));
        assert!(!header_60.has_frame_lbrr(1));
        assert!(header_60.has_frame_lbrr(2));
    }

    #[test]
    fn silk_header_stereo_60ms_8_bits_and_dual_channel_lbrr() {
        // Range-coded symbols:
        // Mid: VAD=[1, 0, 1], LBRR=1; Side: VAD=[0, 1, 0], LBRR=1;
        // Mid Table 4 sym=0 -> mask=1 (0b001); Side Table 4 sym=5 -> mask=6 (0b110).
        const HEADER_STEREO_60MS_LBRR_BYTES: [u8; 2] = [0xb5, 0x18];

        let mut dec = entropy::Decoder::new(&HEADER_STEREO_60MS_LBRR_BYTES);
        let header = decode_silk_header(&mut dec, true, 3).unwrap();
        let [mid, side] = header.channels;

        assert_eq!(header.num_channels, 2);
        assert_eq!(header.num_silk_frames, 3);
        assert_eq!(mid.vad_flags, [true, false, true]);
        assert_eq!(side.vad_flags, [false, true, false]);
        assert_eq!([mid.lbrr_flag, side.lbrr_flag], [true, true]);
        assert_eq!(mid.per_frame_lbrr, [true, false, false]);
        assert_eq!(side.per_frame_lbrr, [false, true, true]);
        assert!(header.has_frame_lbrr(0));
        assert!(header.has_frame_lbrr(1));
        assert!(header.has_frame_lbrr(2));
    }

    #[test]
    fn stereo_prediction_weights_and_mid_only_flag_match_recorded_values() {
        // Range-coded symbols and the values they decode to:
        // n=7, i0=1, i1=2, i2=2, i3=0 -> w0_Q13 = -6783, w1_Q13 = 1033; mid_only = 1 (tell = 16).
        const STEREO_WEIGHTS_AND_MID_ONLY_BYTES: [u8; 2] = [0x32, 0x1f];

        let mut dec = entropy::Decoder::new(&STEREO_WEIGHTS_AND_MID_ONLY_BYTES);

        let weights = decode_stereo_prediction_weights(&mut dec).unwrap();
        assert_eq!(weights, StereoWeights { w0_q13: -6783, w1_q13: 1033 });

        let tell_before_active_mid_only = dec.tell();
        assert!(!decode_mid_only_flag(&mut dec, true).unwrap());
        assert_eq!(dec.tell(), tell_before_active_mid_only);

        assert!(decode_mid_only_flag(&mut dec, false).unwrap());
        assert_eq!(dec.tell(), 16);
    }

    #[test]
    fn gains_independent_delta_clamping_and_log_to_linear_match_recorded_values() {
        // Range-coded symbols and the values they decode to:
        // Frame 1 (20ms Voiced, independent): gains_Q16 = [7929856, 4194304, 1048576000, 1686110208], last_gain_index = 63
        // Frame 2 (10ms Voiced, independent): gains_Q16 = [135266304, 135266304], last_gain_index = 47
        // Frame 3 (10ms Voiced, conditional): gains_Q16 = [83886080, 83886080], last_gain_index = 44
        const GAINS_MULTI_FRAME_BYTES: [u8; 6] = [0x4a, 0x94, 0xce, 0xa2, 0xc2, 0xf8];

        let mut state = CodingReferences::new();
        let mut dec = entropy::Decoder::new(&GAINS_MULTI_FRAME_BYTES);

        assert_silk_gains!(
            state,
            dec,
            samples: 160,
            subframes: 4,
            lpc_order: 10,
            mode: CodingMode::Independent,
            expected_log_gains: [29, 25, 60, 63],
            expected_gains_q16: [7_929_856, 4_194_304, 1_048_576_000, 1_686_110_208],
            expected_last_gain_index: 63
        );

        assert_silk_gains!(
            state,
            dec,
            samples: 80,
            subframes: 2,
            lpc_order: 10,
            mode: CodingMode::Independent,
            expected_log_gains: [47, 47],
            expected_gains_q16: [135_266_304, 135_266_304],
            expected_last_gain_index: 47
        );

        assert_silk_gains!(
            state,
            dec,
            samples: 80,
            subframes: 2,
            lpc_order: 10,
            mode: CodingMode::Conditional,
            expected_log_gains: [44, 44],
            expected_gains_q16: [83_886_080, 83_886_080],
            expected_last_gain_index: 44
        );
    }

    #[test]
    fn pitch_lags_and_ltp_scaling_nb_mb_wb_match_recorded_values() {
        const LTP_WB_THREE_FRAMES_BYTES: [u8; 8] = [0x39, 0x6f, 0x58, 0xee, 0x9c, 0x60, 0xe0, 0xf4];
        const LTP_MB_10MS_BYTES: [u8; 3] = [0x0b, 0xd1, 0xc0];

        let wideband = layout(Bandwidth::Wideband, FrameDuration::Ms20);
        let mut state = CodingReferences::new();
        let mut dec_wb = entropy::Decoder::new(&LTP_WB_THREE_FRAMES_BYTES);

        // 1. WB 20 ms independent voiced frame
        let mut frame1 = voiced_frame(320, 4, 16);
        state.decode_ltp(&mut dec_wb, &mut frame1, &wideband, CodingMode::Independent).unwrap();
        state.prev_signal_type = SignalType::Voiced;

        assert_eq!(state.prev_lag_index, 43);
        assert_eq!(
            frame1.active_subframes().iter().map(|s| s.pitch_lag).collect::<Vec<_>>(),
            vec![74, 75, 76, 77]
        );
        assert_eq!(frame1.ltp_scale_q14, 8192);

        // 2. Conditional voiced frame with relative delta pitch lag
        let mut frame2 = voiced_frame(320, 4, 16);
        state.decode_ltp(&mut dec_wb, &mut frame2, &wideband, CodingMode::Conditional).unwrap();

        assert_eq!(state.prev_lag_index, 46);
        assert_eq!(
            frame2.active_subframes().iter().map(|s| s.pitch_lag).collect::<Vec<_>>(),
            vec![78, 78, 78, 78]
        );
        assert_eq!(frame2.ltp_scale_q14, 15565);

        // 3. Conditional voiced frame with delta_lag_index == 0 falling back to absolute lag
        let mut frame3 = voiced_frame(320, 4, 16);
        state.decode_ltp(&mut dec_wb, &mut frame3, &wideband, CodingMode::Conditional).unwrap();

        assert_eq!(
            frame3.active_subframes().iter().map(|s| s.pitch_lag).collect::<Vec<_>>(),
            vec![32, 32, 35, 41]
        );

        // 4. MediumBand 10 ms frame (2 subframes, scale = 6, min_lag = 24, max_lag = 216)
        let mediumband = layout(Bandwidth::MediumBand, FrameDuration::Ms10);
        let mut state_mb = CodingReferences::new();
        let mut dec_mb = entropy::Decoder::new(&LTP_MB_10MS_BYTES);
        let mut frame_mb = voiced_frame(120, 2, 10);
        state_mb
            .decode_ltp(&mut dec_mb, &mut frame_mb, &mediumband, CodingMode::Independent)
            .unwrap();

        assert_eq!(
            frame_mb.active_subframes().iter().map(|s| s.pitch_lag).collect::<Vec<_>>(),
            vec![40, 43]
        );
        assert_eq!(frame_mb.ltp_scale_q14, 12288);
    }

    /// A header of zero SILK frames, an Opus frame without a SILK layer and a frame whose
    /// subframe count disagrees with its layout are rejected
    /// ([RFC 6716 Section 4.2.2](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.2.2>)).
    #[test]
    fn frame_counts_and_subframe_layouts_are_validated() {
        let mut dec = entropy::Decoder::new(&[0x80]);
        assert!(Error::UnsupportedFrameSize.matches(&decode_silk_header(&mut dec, false, 0)));
        let five_ms = SilkLayout::new(Bandwidth::Wideband, FrameDuration::Ms5);
        assert!(Error::UnsupportedFrameSize.matches(&five_ms));

        let ten_ms = layout(Bandwidth::Wideband, FrameDuration::Ms10);
        let mut frame = voiced_frame(320, 4, 16);
        let result = CodingReferences::new().decode_ltp(
            &mut dec,
            &mut frame,
            &ten_ms,
            CodingMode::Independent,
        );
        assert!(Error::MismatchFrameSubframes.matches(&result));
    }
}
