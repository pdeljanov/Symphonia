//! CELT stereo band decoding: the mid/side and two-phase splits of a coupled band and the
//! unmixing back to left and right
//! ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>),
//! [RFC 8251 Section 10](<https://datatracker.ietf.org/doc/html/rfc8251#section-10>)).

use crate::celt::constant::BITRES;
use crate::celt::vq::{
    BandDecoderCtx, Half, Lowband, Partition, STEREO_MERGE_MIN_ENERGY, SplitAngle, SplitKind,
    THETA_HALF_Q14, THETA_MAX_Q14, decode_split_angle, decode_two_halves, quant_band,
    quant_band_n1,
};
use crate::entropy::Decoder;
use crate::planes::Planes;
use symphonia_core::errors::Result;

/// Unmixes normalized mid (`x[..n]`) and scaled side (`y[..n]`) vectors back into unit-norm
/// left (`x[..n]`) and right (`y[..n]`) channel vectors ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
fn stereo_merge(x: &mut [f32], y: &mut [f32], mid: f32, n: usize) {
    let (xp, side) = y[..n]
        .iter()
        .zip(x[..n].iter())
        .fold((0.0_f32, 0.0_f32), |(xp, side), (&vy, &vx)| (xp + vy * vx, side + vy * vy));

    let xp = xp * mid;
    let el = mid * mid + side - 2.0_f32 * xp;
    let er = mid * mid + side + 2.0_f32 * xp;
    if er < STEREO_MERGE_MIN_ENERGY || el < STEREO_MERGE_MIN_ENERGY {
        y[..n].copy_from_slice(&x[..n]);
        return;
    }

    let lgain = 1.0_f32 / el.sqrt();
    let rgain = 1.0_f32 / er.sqrt();
    for (vx, vy) in x[..n].iter_mut().zip(y[..n].iter_mut()) {
        let l = mid * (*vx);
        let r = *vy;
        *vx = lgain * (l - r);
        *vy = rgain * (l + r);
    }
    return;
}

/// Decodes a two-bin stereo band (`n = 2`): one channel carries the shape and a sign, the other
/// is its orthogonal rotation, then both are scaled by the split weights and unmixed
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
fn decode_two_phase_stereo(
    ctx: &mut BandDecoderCtx,
    dec: &mut Decoder<'_>,
    x: &mut [f32],
    y: &mut [f32],
    lowband: Lowband<'_>,
    part: Partition,
    split: &SplitAngle,
) -> Result<u32> {
    let (mid, side) = split.gains();
    let sbits = if split.itheta != 0 && split.itheta != THETA_MAX_Q14 { 1 << BITRES } else { 0 };
    let mbits = part.bits - sbits;
    ctx.remaining_bits -= split.qalloc + sbits;

    let sign_bit = if sbits != 0 { dec.decode_raw_bits(1)? } else { 0 };
    let sign = 1.0_f32 - 2.0_f32 * (sign_bit as f32);
    let primary = Partition { bits: mbits, gain: 1.0_f32, ..part };

    let cm = if split.itheta > THETA_HALF_Q14 {
        let cm = quant_band(ctx, dec, y, lowband, primary)?;
        x[0] = -sign * y[1];
        x[1] = sign * y[0];
        cm
    } else {
        let cm = quant_band(ctx, dec, x, lowband, primary)?;
        y[0] = -sign * x[1];
        y[1] = sign * x[0];
        cm
    };

    for (vx, vy) in x[..2].iter_mut().zip(y[..2].iter_mut()) {
        *vx *= mid;
        *vy *= side;
        let tmp = *vx;
        *vx = tmp - *vy;
        *vy += tmp;
    }
    return Ok(cm);
}

/// Decodes a mid/side stereo band (`n > 2`): mid and side as two mono bands with the budget
/// split by the angle, then unmixed into left and right
/// ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>)).
fn decode_mid_side_stereo(
    ctx: &mut BandDecoderCtx,
    dec: &mut Decoder<'_>,
    x: &mut [f32],
    y: &mut [f32],
    lowband: Lowband<'_>,
    part: Partition,
    split: &SplitAngle,
) -> Result<u32> {
    let Partition { n, blocks, bits, fill, .. } = part;
    let (mid, side) = split.gains();
    let mbits = 0.max(bits.min((bits - split.delta) / 2));
    let sbits = bits - mbits;
    ctx.remaining_bits -= split.qalloc;

    let mid_half = Half {
        x: &mut x[..n],
        lowband,
        part: Partition { bits: mbits, gain: 1.0_f32, ..part },
        mask_shift: 0,
    };
    let side_half = Half {
        x: &mut y[..n],
        lowband: Lowband::default(),
        part: Partition { bits: sbits, fill: fill >> blocks, gain: side, ..part },
        mask_shift: 0,
    };
    let cm = decode_two_halves(ctx, dec, mid_half, side_half, split.itheta, quant_band)?;
    stereo_merge(x, y, mid, n);
    return Ok(cm);
}

/// Decodes a coupled stereo Bark band (`x` and `y`) using mid/side or intensity
/// stereo splitting ([RFC 6716 Section 4.3.4.4](<https://datatracker.ietf.org/doc/html/rfc6716#section-4.3.4.4>),
/// [RFC 8251 Section 10](<https://datatracker.ietf.org/doc/html/rfc8251#section-10>)).
pub(super) fn quant_band_stereo(
    ctx: &mut BandDecoderCtx,
    dec: &mut Decoder<'_>,
    x: &mut [f32],
    y: &mut [f32],
    lowband: Lowband<'_>,
    part: Partition,
    phase_inversion: bool,
) -> Result<u32> {
    let Partition { n, blocks, lm, bits, fill, .. } = part;
    if n == 1 {
        return quant_band_n1(ctx, dec, Planes::new(x, Some(y)), lowband);
    }

    let split = decode_split_angle(ctx, dec, n, bits, lm, blocks, SplitKind::Stereo)?;
    let bits = bits - split.qalloc;
    let cm = match n {
        2 => {
            let two_phase = Partition { bits, ..part };
            decode_two_phase_stereo(ctx, dec, x, y, lowband, two_phase, &split)?
        }
        _ => {
            let mid_side = Partition { bits, fill: split.restrict_fill(fill, blocks), ..part };
            decode_mid_side_stereo(ctx, dec, x, y, lowband, mid_side, &split)?
        }
    };

    if split.invert_side && phase_inversion {
        y[..n].iter_mut().for_each(|vy| *vy = -*vy);
    }
    return Ok(cm);
}
