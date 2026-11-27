// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The filters follow the floating point build of the Xiph.Org Speex reference implementation, see
// NOTICE-xiph.txt.

use std::f32::consts::PI;

/// The noise generator of the decoder.
pub(crate) fn rand(std: f32, seed: &mut u32) -> f32 {
    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    let bits = 0x3f80_0000 | (0x007f_ffff & *seed);
    3.4642 * std * (f32::from_bits(bits) - 1.5)
}

pub(crate) fn rms(x: &[f32]) -> f32 {
    (0.1 + x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

pub(crate) fn bw_lpc(gamma: f32, lpc: &[f32], out: &mut [f32]) {
    let mut g = gamma;
    for (o, &l) in out.iter_mut().zip(lpc) {
        *o = g * l;
        g *= gamma;
    }
}

/// Filters `x` with the all-pole filter `1 / (1 + sum den[j] z^-(j+1))`.
pub(crate) fn iir_mem(x: &[f32], den: &[f32], y: &mut [f32], mem: &mut [f32]) {
    let ord = den.len();
    for i in 0..x.len() {
        let yi = x[i] + mem[0];
        let nyi = -yi;
        for j in 0..ord - 1 {
            mem[j] = mem[j + 1] + den[j] * nyi;
        }
        mem[ord - 1] = den[ord - 1] * nyi;
        y[i] = yi;
    }
}

const HIGHPASS_P: [[f32; 3]; 5] = [
    [1.0, -1.91120, 0.91498],
    [1.0, -1.92683, 0.93071],
    [1.0, -1.93338, 0.93553],
    [1.0, -1.97226, 0.97332],
    [1.0, -1.37000, 0.39900],
];
const HIGHPASS_Z: [[f32; 3]; 5] = [
    [0.95654, -1.91309, 0.95654],
    [0.96446, -1.92879, 0.96446],
    [0.96723, -1.93445, 0.96723],
    [0.98645, -1.97277, 0.98645],
    [0.88000, -1.76000, 0.88000],
];

/// The output high pass filter, `wideband` selects the one for the 16 kHz low band.
pub(crate) fn highpass(x: &mut [f32], wideband: bool, mem: &mut [f32; 2]) {
    let id = if wideband { 3 } else { 1 };
    let (den, num) = (&HIGHPASS_P[id], &HIGHPASS_Z[id]);
    for v in x.iter_mut() {
        let xi = *v;
        let vout = num[0] * xi + mem[0];
        mem[0] = mem[1] + num[1] * xi - den[1] * vout;
        mem[1] = num[2] * xi - den[2] * vout;
        *v = vout;
    }
}

pub(crate) fn lsp_interpolate(
    old: &[f32],
    new: &[f32],
    out: &mut [f32],
    sub: usize,
    nb_subframes: usize,
    margin: f32,
) {
    let len = old.len();
    let t = (1.0 + sub as f32) / nb_subframes as f32;
    for i in 0..len {
        out[i] = (1.0 - t) * old[i] + t * new[i];
    }

    if out[0] < margin {
        out[0] = margin;
    }
    if out[len - 1] > PI - margin {
        out[len - 1] = PI - margin;
    }
    for i in 1..len - 1 {
        if out[i] < out[i - 1] + margin {
            out[i] = out[i - 1] + margin;
        }
        if out[i] > out[i + 1] - margin {
            out[i] = 0.5 * (out[i] + out[i + 1] - margin);
        }
    }
}

pub(crate) fn lsp_to_lpc(freq: &[f32], ak: &mut [f32]) {
    let order = freq.len();
    let m = order >> 1;
    let mut wp = vec![0f32; 4 * m + 2];
    let x: Vec<f32> = freq.iter().map(|f| f.cos()).collect();

    let (mut xin1, mut xin2) = (1.0f32, 1.0f32);
    for j in 0..=order {
        let mut i2 = 0;
        let mut n4 = 0;
        for i in 0..m {
            let n1 = i * 4;
            let (n2, n3) = (n1 + 1, n1 + 2);
            n4 = n1 + 3;
            let xout1 = xin1 - 2.0 * x[i2] * wp[n1] + wp[n2];
            let xout2 = xin2 - 2.0 * x[i2 + 1] * wp[n3] + wp[n4];
            wp[n2] = wp[n1];
            wp[n4] = wp[n3];
            wp[n1] = xin1;
            wp[n3] = xin2;
            xin1 = xout1;
            xin2 = xout2;
            i2 += 2;
        }
        let xout1 = xin1 + wp[n4 + 1];
        let xout2 = xin2 - wp[n4 + 2];
        if j > 0 {
            ak[j - 1] = (xout1 + xout2) * 0.5;
        }
        wp[n4 + 1] = xin1;
        wp[n4 + 2] = xin2;

        xin1 = 0.0;
        xin2 = 0.0;
    }
}

/// The 64 tap synthesis filter bank that joins two bands into one of twice the sampling rate.
pub(crate) fn qmf_synth(
    x1: &[f32],
    x2: &[f32],
    a: &[f32; 64],
    y: &mut [f32],
    mem1: &mut [f32; 64],
    mem2: &mut [f32; 64],
) {
    const M2: usize = 32;
    let n2 = y.len() / 2;

    let mut xx1 = vec![0f32; M2 + n2];
    let mut xx2 = vec![0f32; M2 + n2];
    for i in 0..n2 {
        xx1[i] = x1[n2 - 1 - i];
        xx2[i] = x2[n2 - 1 - i];
    }
    for i in 0..M2 {
        xx1[n2 + i] = mem1[2 * i + 1];
        xx2[n2 + i] = mem2[2 * i + 1];
    }

    for i in (0..n2).step_by(2) {
        let (mut y0, mut y1, mut y2, mut y3) = (0f32, 0f32, 0f32, 0f32);
        let mut x10 = xx1[n2 - 2 - i];
        let mut x20 = xx2[n2 - 2 - i];

        for j in (0..M2).step_by(2) {
            let (a0, a1) = (a[2 * j], a[2 * j + 1]);
            let x11 = xx1[n2 - 1 + j - i];
            let x21 = xx2[n2 - 1 + j - i];

            y0 += a0 * (x11 - x21);
            y1 += a1 * (x11 + x21);
            y2 += a0 * (x10 - x20);
            y3 += a1 * (x10 + x20);

            let (a0, a1) = (a[2 * j + 2], a[2 * j + 3]);
            x10 = xx1[n2 + j - i];
            x20 = xx2[n2 + j - i];

            y0 += a0 * (x10 - x20);
            y1 += a1 * (x10 + x20);
            y2 += a0 * (x11 - x21);
            y3 += a1 * (x11 + x21);
        }

        y[2 * i] = 2.0 * y0;
        y[2 * i + 1] = 2.0 * y1;
        y[2 * i + 2] = 2.0 * y2;
        y[2 * i + 3] = 2.0 * y3;
    }

    for i in 0..M2 {
        mem1[2 * i + 1] = xx1[i];
        mem2[2 * i + 1] = xx2[i];
    }
}

const SHIFT_FILT: [[f32; 7]; 3] = [
    [-0.011915, 0.046995, -0.152373, 0.614108, 0.614108, -0.152373, 0.046995],
    [-0.0324855, 0.0859768, -0.2042986, 0.9640297, 0.2086420, -0.0302054, -0.0063646],
    [-0.0063646, -0.0302054, 0.2086420, 0.9640297, -0.2042986, 0.0859768, -0.0324855],
];

fn inner_prod(buf: &[f32], a: isize, b: isize, len: usize) -> f32 {
    (0..len as isize).map(|i| buf[(a + i) as usize] * buf[(b + i) as usize]).sum()
}

/// Interpolates the excitation at `pos` with a fractional pitch period.
fn interp_pitch(buf: &[f32], pos: isize, interp: &mut [f32], pitch: isize, len: usize) {
    let mut corr = [[0f32; 7]; 4];
    for i in 0..7 {
        corr[0][i] = inner_prod(buf, pos, pos - pitch - 3 + i as isize, len);
    }
    for i in 0..3 {
        for j in 0..7 {
            let i1 = (3 - j as isize).max(0) as usize;
            let i2 = (10 - j).min(7);
            let mut tmp = 0.0;
            for k in i1..i2 {
                tmp += SHIFT_FILT[i][k] * corr[0][j + k - 3];
            }
            corr[i + 1][j] = tmp;
        }
    }

    let (mut maxi, mut maxj) = (0, 0);
    let mut maxcorr = corr[0][0];
    for i in 0..4 {
        for j in 0..7 {
            if corr[i][j] > maxcorr {
                maxcorr = corr[i][j];
                maxi = i;
                maxj = j;
            }
        }
    }

    for i in 0..len as isize {
        let base = pos + i - (pitch - maxj as isize + 3);
        interp[i as usize] = if maxi > 0 {
            (0..7).map(|k| buf[(base + k - 3) as usize] * SHIFT_FILT[maxi - 1][k as usize]).sum()
        }
        else {
            buf[base as usize]
        };
    }
}

/// The comb filter that enhances the excitation of the narrowband decoder.
pub(crate) fn multicomb(
    buf: &[f32],
    pos: isize,
    new_exc: &mut [f32],
    nsf: usize,
    pitch: isize,
    max_pitch: isize,
    comb_gain: f32,
) {
    let mut iexc = vec![0f32; 2 * nsf];
    interp_pitch(buf, pos, &mut iexc[..nsf], pitch, 80);
    if pitch > max_pitch {
        interp_pitch(buf, pos, &mut iexc[nsf..], 2 * pitch, 80);
    }
    else {
        interp_pitch(buf, pos, &mut iexc[nsf..], -pitch, 80);
    }

    let exc: Vec<f32> = (0..nsf as isize).map(|i| buf[(pos + i) as usize]).collect();
    let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();

    let iexc0_mag = (1000.0 + dot(&iexc[..nsf], &iexc[..nsf])).sqrt();
    let iexc1_mag = (1000.0 + dot(&iexc[nsf..], &iexc[nsf..])).sqrt();
    let exc_mag = (1.0 + dot(&exc, &exc)).sqrt();
    let corr0 = dot(&iexc[..nsf], &exc).max(0.0);
    let corr1 = dot(&iexc[nsf..], &exc).max(0.0);

    let pgain1 = if corr0 > iexc0_mag * exc_mag { 1.0 } else { corr0 / exc_mag / iexc0_mag };
    let pgain2 = if corr1 > iexc1_mag * exc_mag { 1.0 } else { corr1 / exc_mag / iexc1_mag };
    let gg1 = exc_mag / iexc0_mag;
    let gg2 = exc_mag / iexc1_mag;

    let (c1, c2) = if comb_gain > 0.0 {
        let c1 = 0.4 * comb_gain + 0.07;
        (c1, 0.5 + 1.72 * (c1 - 0.07))
    }
    else {
        (0.0, 0.0)
    };

    let g1 = (1.0 - c2 * pgain1 * pgain1).max(c1);
    let g2 = (1.0 - c2 * pgain2 * pgain2).max(c1);
    let g1 = c1 / g1;
    let g2 = c1 / g2;

    let (gain0, gain1) = if pitch > max_pitch {
        (0.7 * (g1 * gg1), 0.3 * (g2 * gg2))
    }
    else {
        (0.6 * (g1 * gg1), 0.6 * (g2 * gg2))
    };

    for i in 0..nsf {
        new_exc[i] = exc[i] + (gain0 * iexc[i] + gain1 * iexc[i + nsf]);
    }

    let new_ener = rms(&new_exc[..nsf]).max(1.0);
    let old_ener = rms(&exc).max(1.0).min(new_ener);
    let ngain = old_ener / new_ener;

    for v in new_exc[..nsf].iter_mut() {
        *v *= ngain;
    }
}
