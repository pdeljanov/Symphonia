// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! ITU-T G.722 7 kHz audio coding within 64 kbit/s (sub-band ADPCM).

/// The inverse quantizer of the 6-bit low band codes.
#[rustfmt::skip]
const LOW_QUANT_6: [i32; 64] = [
    -136, -136, -136, -136, -24808, -21904, -19008, -16704,
    -14984, -13512, -12280, -11192, -10232, -9360, -8576, -7856,
    -7192, -6576, -6000, -5456, -4944, -4464, -4008, -3576,
    -3168, -2776, -2400, -2032, -1688, -1360, -1040, -728,
    24808, 21904, 19008, 16704, 14984, 13512, 12280, 11192,
    10232, 9360, 8576, 7856, 7192, 6576, 6000, 5456,
    4944, 4464, 4008, 3576, 3168, 2776, 2400, 2032,
    1688, 1360, 1040, 728, 432, 136, -432, -136,
];

/// The inverse quantizer of the 4-bit low band codes (the upper 4 bits of the 6-bit codes).
#[rustfmt::skip]
const LOW_QUANT_4: [i32; 16] = [
    0, -20456, -12896, -8968, -6288, -4240, -2584, -1200,
    20456, 12896, 8968, 6288, 4240, 2584, 1200, 0,
];

/// The inverse quantizer of the 2-bit high band codes.
const HIGH_QUANT_2: [i32; 4] = [-7408, -1616, 7408, 1616];

/// The logarithmic scale factor adaptation of the low band, by 4-bit code.
#[rustfmt::skip]
const LOW_LOG_STEP: [i32; 16] = [
    -60, 3042, 1198, 538, 334, 172, 58, -30,
    3042, 1198, 538, 334, 172, 58, -30, -60,
];

/// The logarithmic scale factor adaptation of the high band, by the lowest bit of the 2-bit code.
const HIGH_LOG_STEP: [i32; 2] = [798, -214];

/// The inverse of the base-2 logarithm, as a mantissa.
#[rustfmt::skip]
const INV_LOG2: [i32; 32] = [
    2048, 2093, 2139, 2186, 2233, 2282, 2332, 2383,
    2435, 2489, 2543, 2599, 2656, 2714, 2774, 2834,
    2896, 2960, 3025, 3091, 3158, 3228, 3298, 3371,
    3444, 3520, 3597, 3676, 3756, 3838, 3922, 4008,
];

/// The coefficients of the quadrature mirror filter.
const QMF_COEFFS: [i32; 12] = [3, -11, 12, 32, -210, 951, 3876, -805, 362, -156, 53, -11];

fn saturate(value: i32) -> i32 {
    value.clamp(-32768, 32767)
}

/// The state of the adaptive predictor and quantizer of a sub-band.
#[derive(Clone, Default)]
struct Band {
    s: i32,
    sz: i32,
    r: [i32; 3],
    a: [i32; 3],
    p: [i32; 3],
    d: [i32; 7],
    b: [i32; 7],
    nb: i32,
    det: i32,
}

impl Band {
    fn new(det: i32) -> Self {
        Band { det, ..Default::default() }
    }

    /// Update the scale factor `det` from the log scale factor `nb`.
    fn update_scale(&mut self, base_shift: i32) {
        let mantissa = INV_LOG2[((self.nb >> 6) & 31) as usize];
        let shift = base_shift - (self.nb >> 11);

        let value = if shift < 0 { mantissa << -shift } else { mantissa >> shift };

        self.det = value << 2;
    }

    /// Update the adaptive predictor with the dequantized difference signal `d`.
    fn update_predictor(&mut self, d: i32) {
        self.d[0] = d;
        self.r[0] = saturate(self.s + d);
        self.p[0] = saturate(self.sz + d);

        // Update the second pole coefficient.
        let sg: [i32; 3] = [self.p[0] >> 15, self.p[1] >> 15, self.p[2] >> 15];

        let wd1 = saturate(self.a[1] << 2);
        let wd2 = (if sg[0] == sg[1] { -wd1 } else { wd1 }).min(32767);
        let mut wd3 = if sg[0] == sg[2] { 128 } else { -128 };
        wd3 += wd2 >> 7;
        wd3 += (self.a[2] * 32512) >> 15;
        let ap2 = wd3.clamp(-12288, 12288);

        // Update the first pole coefficient.
        let wd1 = if sg[0] == sg[1] { 192 } else { -192 };
        let wd2 = (self.a[1] * 32640) >> 15;
        let mut ap1 = saturate(wd1 + wd2);
        let limit = saturate(15360 - ap2);
        ap1 = ap1.clamp(-limit, limit);

        // Update the zero coefficients.
        let wd1 = if d == 0 { 0 } else { 128 };
        let sg0 = d >> 15;
        let mut bp = [0; 7];
        for i in 1..7 {
            let sgi = self.d[i] >> 15;
            let wd2 = if sgi == sg0 { wd1 } else { -wd1 };
            let wd3 = (self.b[i] * 32640) >> 15;
            bp[i] = saturate(wd2 + wd3);
        }

        // Delay the signals.
        for i in (1..7).rev() {
            self.d[i] = self.d[i - 1];
            self.b[i] = bp[i];
        }

        for i in (1..3).rev() {
            self.r[i] = self.r[i - 1];
            self.p[i] = self.p[i - 1];
        }

        self.a[1] = ap1;
        self.a[2] = ap2;

        // Pole and zero filters.
        let wd1 = saturate(self.r[1] + self.r[1]);
        let wd1 = (self.a[1] * wd1) >> 15;
        let wd2 = saturate(self.r[2] + self.r[2]);
        let wd2 = (self.a[2] * wd2) >> 15;
        let sp = saturate(wd1 + wd2);

        self.sz = 0;
        for i in (1..7).rev() {
            let wd1 = saturate(self.d[i] + self.d[i]);
            self.sz += (self.b[i] * wd1) >> 15;
        }
        self.sz = saturate(self.sz);

        self.s = saturate(sp + self.sz);
    }
}

/// A G.722 decoder, for the 64 kbit/s mode.
#[derive(Clone)]
pub(crate) struct G722Decoder {
    bands: [Band; 2],
    x: [i32; 24],
}

impl G722Decoder {
    pub(crate) fn new() -> Self {
        G722Decoder { bands: [Band::new(32), Band::new(8)], x: [0; 24] }
    }

    /// Decode a byte to two 16-bit samples.
    pub(crate) fn decode_byte(&mut self, code: u8) -> [i32; 2] {
        let code = usize::from(code);
        let low_code = code & 0x3f;
        let high_code = code >> 6;

        // Low band.
        let [low, high] = &mut self.bands;

        let diff = (low.det * LOW_QUANT_6[low_code]) >> 15;
        let rlow = (low.s + diff).clamp(-16384, 16383);

        let dlowt = (low.det * LOW_QUANT_4[low_code >> 2]) >> 15;

        low.nb = (((low.nb * 127) >> 7) + LOW_LOG_STEP[low_code >> 2]).clamp(0, 18432);
        low.update_scale(8);
        low.update_predictor(dlowt);

        // High band.
        let dhigh = (high.det * HIGH_QUANT_2[high_code]) >> 15;
        let rhigh = (dhigh + high.s).clamp(-16384, 16383);

        high.nb = (((high.nb * 127) >> 7) + HIGH_LOG_STEP[high_code & 1]).clamp(0, 22528);
        high.update_scale(10);
        high.update_predictor(dhigh);

        // Receive quadrature mirror filter.
        self.x.copy_within(2.., 0);
        self.x[22] = rlow + rhigh;
        self.x[23] = rlow - rhigh;

        let mut out1 = 0;
        let mut out2 = 0;

        for i in 0..12 {
            out2 += self.x[2 * i] * QMF_COEFFS[i];
            out1 += self.x[2 * i + 1] * QMF_COEFFS[11 - i];
        }

        [saturate(out1 >> 11), saturate(out2 >> 11)]
    }
}
