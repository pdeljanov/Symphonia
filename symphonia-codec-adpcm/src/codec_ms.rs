use symphonia_core::errors::{unsupported_error, Result};
use symphonia_core::io::{BufReader, ReadBytes};
use symphonia_core::util::clamp::clamp_i16;

use crate::common::{from_i16_shift, i16_to_i32, Nibble};

#[rustfmt::skip]
const MS_ADAPTATION_TABLE: [i32; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614,
    768, 614, 512, 409, 307, 230, 230, 230,
];

const MS_ADAPT_COEFFS1: [i32; 7] = [256, 512, 0, 192, 240, 460, 392];
const MS_ADAPT_COEFFS2: [i32; 7] = [0, -256, 0, 64, 0, -208, -232];

const DELTA_MIN: i32 = 16;

macro_rules! check_block_predictor {
    ($block_predictor:ident, $max:expr) => {
        if $block_predictor > $max {
            return unsupported_error("adpcm: block predictor exceeds range.");
        }
    };
}

pub fn signed_nibble(nibble: u8) -> i8 {
    if (nibble & 0x08) != 0 {
        nibble as i8 - 0x10
    } else {
        nibble as i8
    }
}

/// `AdpcmMsParameters` contains the sets of coefficients used to iniialize `AdpcmMsBlockStatus`
pub(crate) struct AdpcmMsParameters {
    coeffs1: Vec<i32>,
    coeffs2: Vec<i32>,
}

impl AdpcmMsParameters {
    pub(crate) fn from_extra_data(extra_data: &Option<Box<[u8]>>) -> Result<Self> {
        let mut params = AdpcmMsParameters {
            coeffs1: Vec::from(MS_ADAPT_COEFFS1),
            coeffs2: Vec::from(MS_ADAPT_COEFFS2),
        };
        if let Some(extra_data) = extra_data {
            let mut reader = BufReader::new(extra_data);

            let coeff_num = reader.read_u16()? as usize;
            params.coeffs1.resize(coeff_num, 0);
            params.coeffs2.resize(coeff_num, 0);
            for i in 0..coeff_num {
                params.coeffs1[i] = i16_to_i32!(reader.read_u16()?);
                params.coeffs2[i] = i16_to_i32!(reader.read_u16()?);
            }
        }
        Ok(params)
    }
}

/// `AdpcmMsBlockStatus` contains values to decode a block
struct AdpcmMsBlockStatus {
    coeff1: i32,
    coeff2: i32,
    delta: i32,
    sample1: i32,
    sample2: i32,
}

impl AdpcmMsBlockStatus {
    fn read_mono_preample<B: ReadBytes>(
        stream: &mut B,
        params: &AdpcmMsParameters,
    ) -> Result<Self> {
        let block_predictor = stream.read_byte()? as usize;
        check_block_predictor!(block_predictor, params.coeffs1.len());
        let status = Self {
            coeff1: params.coeffs1[block_predictor],
            coeff2: params.coeffs2[block_predictor],
            delta: i16_to_i32!(stream.read_u16()?),
            sample1: i16_to_i32!(stream.read_u16()?),
            sample2: i16_to_i32!(stream.read_u16()?),
        };
        Ok(status)
    }

    fn read_stereo_preample<B: ReadBytes>(
        stream: &mut B,
        params: &AdpcmMsParameters,
    ) -> Result<(Self, Self)> {
        let left_block_predictor = stream.read_byte()? as usize;
        check_block_predictor!(left_block_predictor, params.coeffs1.len());
        let right_block_predictor = stream.read_byte()? as usize;
        check_block_predictor!(right_block_predictor, params.coeffs1.len());
        let left_delta = i16_to_i32!(stream.read_u16()?);
        let right_delta = i16_to_i32!(stream.read_u16()?);
        let left_sample1 = i16_to_i32!(stream.read_u16()?);
        let right_sample1 = i16_to_i32!(stream.read_u16()?);
        let left_sample2 = i16_to_i32!(stream.read_u16()?);
        let right_sample2 = i16_to_i32!(stream.read_u16()?);
        Ok((
            Self {
                coeff1: params.coeffs1[left_block_predictor],
                coeff2: params.coeffs2[left_block_predictor],
                delta: left_delta,
                sample1: left_sample1,
                sample2: left_sample2,
            },
            Self {
                coeff1: params.coeffs1[right_block_predictor],
                coeff2: params.coeffs2[right_block_predictor],
                delta: right_delta,
                sample1: right_sample1,
                sample2: right_sample2,
            },
        ))
    }

    fn expand_nibble(&mut self, byte: u8, nibble: Nibble) -> i32 {
        let nibble = nibble.get_nibble(byte);
        let signed_nibble = signed_nibble(nibble) as i32;
        let predictor = ((self.sample1 * self.coeff1) + (self.sample2 * self.coeff2)) / 256
            + signed_nibble * self.delta;
        self.sample2 = self.sample1;
        self.sample1 = clamp_i16(predictor) as i32;
        self.delta = (MS_ADAPTATION_TABLE[nibble as usize] * self.delta) / 256;
        self.delta = self.delta.max(DELTA_MIN);
        from_i16_shift!(self.sample1)
    }
}

pub(crate) fn decode_mono<B: ReadBytes>(
    stream: &mut B,
    buffer: &mut [i32],
    frames_per_block: usize,
    params: &AdpcmMsParameters,
) -> Result<()> {
    let mut status = AdpcmMsBlockStatus::read_mono_preample(stream, params)?;
    buffer[0] = from_i16_shift!(status.sample2);
    buffer[1] = from_i16_shift!(status.sample1);
    for byte in 1..(frames_per_block / 2) {
        let nibbles = stream.read_u8()?;
        buffer[byte * 2] = status.expand_nibble(nibbles, Nibble::Upper);
        buffer[byte * 2 + 1] = status.expand_nibble(nibbles, Nibble::Lower);
    }
    Ok(())
}

pub(crate) fn decode_stereo<B: ReadBytes>(
    stream: &mut B,
    buffers: [&mut [i32]; 2],
    frames_per_block: usize,
    params: &AdpcmMsParameters,
) -> Result<()> {
    let (mut left_status, mut right_status) =
        AdpcmMsBlockStatus::read_stereo_preample(stream, params)?;
    buffers[0][0] = from_i16_shift!(left_status.sample2);
    buffers[0][1] = from_i16_shift!(left_status.sample1);
    buffers[1][0] = from_i16_shift!(right_status.sample2);
    buffers[1][1] = from_i16_shift!(right_status.sample1);
    for frame in 2..frames_per_block {
        let nibbles = stream.read_u8()?;
        buffers[0][frame] = left_status.expand_nibble(nibbles, Nibble::Upper);
        buffers[1][frame] = right_status.expand_nibble(nibbles, Nibble::Lower);
    }
    Ok(())
}
