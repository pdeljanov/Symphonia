// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! ITU-T G.726 (and G.721, G.723) adaptive differential pulse code modulation at 16, 24, 32, and
//! 40 kbit/s.

/// The code tables of a bit rate.
struct Tables {
    /// The inverse quantizer, in the logarithmic domain.
    dqln: &'static [i32],
    /// The scale factor multiplier, `W`.
    w: &'static [i32],
    /// The speed control multiplier, `F`.
    f: &'static [i32],
}

/// Marks a zero difference signal in the inverse quantizer tables.
const ZERO: i32 = -32768;

static TABLES_16: Tables =
    Tables { dqln: &[116, 365, 365, 116], w: &[-22, 439, 439, -22], f: &[0, 7, 7, 0] };

static TABLES_24: Tables = Tables {
    dqln: &[ZERO, 135, 273, 373, 373, 273, 135, ZERO],
    w: &[-4, 30, 137, 582, 582, 137, 30, -4],
    f: &[0, 1, 2, 7, 7, 2, 1, 0],
};

static TABLES_32: Tables = Tables {
    dqln: &[ZERO, 4, 135, 213, 273, 323, 373, 425, 425, 373, 323, 273, 213, 135, 4, ZERO],
    w: &[-12, 18, 41, 64, 112, 198, 355, 1122, 1122, 355, 198, 112, 64, 41, 18, -12],
    f: &[0, 0, 0, 1, 1, 1, 3, 7, 7, 3, 1, 1, 1, 0, 0, 0],
};

static TABLES_40: Tables = Tables {
    dqln: &[
        ZERO, -66, 28, 104, 169, 224, 274, 318, 358, 395, 429, 459, 488, 514, 539, 566, 566, 539,
        514, 488, 459, 429, 395, 358, 318, 274, 224, 169, 104, 28, -66, ZERO,
    ],
    w: &[
        14, 14, 24, 39, 40, 41, 58, 100, 141, 179, 219, 280, 358, 440, 529, 696, 696, 529, 440,
        358, 280, 219, 179, 141, 100, 58, 41, 40, 39, 24, 14, 14,
    ],
    f: &[
        0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 3, 4, 5, 6, 6, 6, 6, 5, 4, 3, 2, 1, 1, 1, 1, 1, 0, 0, 0,
        0, 0,
    ],
};

/// The number of values in `0..15` that are less than or equal to `2^i`: the position of the
/// most significant bit of `value`, plus one.
fn quan(value: i32) -> i32 {
    (32 - (value as u32).leading_zeros() as i32).min(15)
}

/// Multiply the predictor coefficient `an` by the floating-point signal `srn`.
fn fmult(an: i32, srn: i32) -> i32 {
    let anmag = if an > 0 { an } else { (-an) & 0x1fff };
    let anexp = quan(anmag) - 6;
    let anmant = if anmag == 0 {
        32
    }
    else if anexp >= 0 {
        anmag >> anexp
    }
    else {
        anmag << -anexp
    };

    let wanexp = anexp + ((srn >> 6) & 0xf) - 13;
    let wanmant = (anmant * (srn & 0x3f) + 0x30) >> 4;
    let retval = if wanexp >= 0 { (wanmant << wanexp) & 0x7fff } else { wanmant >> -wanexp };

    if (an ^ srn) < 0 { -retval } else { retval }
}

/// Convert a sample to the floating-point representation used by the predictor.
fn to_float(value: i32) -> i32 {
    if value == 0 {
        0x20
    }
    else if value > 0 {
        let exp = quan(value);
        (exp << 6) + ((value << 6) >> exp)
    }
    else if value > -32768 {
        let mag = -value;
        let exp = quan(mag);
        (exp << 6) + ((mag << 6) >> exp) - 0x400
    }
    else {
        -992
    }
}

/// A G.726 decoder.
#[derive(Clone)]
pub(crate) struct G726Decoder {
    code_size: u32,
    tables: &'static Tables,
    yl: i32,
    yu: i32,
    dms: i32,
    dml: i32,
    ap: i32,
    a: [i32; 2],
    b: [i32; 6],
    pk: [i32; 2],
    dq: [i32; 6],
    sr: [i32; 2],
    td: bool,
}

impl G726Decoder {
    /// Create a decoder for codes of `code_size` bits (2 to 5).
    pub(crate) fn new(code_size: u32) -> Option<Self> {
        let tables = match code_size {
            2 => &TABLES_16,
            3 => &TABLES_24,
            4 => &TABLES_32,
            5 => &TABLES_40,
            _ => return None,
        };

        Some(G726Decoder {
            code_size,
            tables,
            yl: 34816,
            yu: 544,
            dms: 0,
            dml: 0,
            ap: 0,
            a: [0; 2],
            b: [0; 6],
            pk: [0; 2],
            dq: [32; 6],
            sr: [32; 2],
            td: false,
        })
    }

    pub(crate) fn code_size(&self) -> u32 {
        self.code_size
    }

    fn step_size(&self) -> i32 {
        if self.ap >= 256 {
            return self.yu;
        }

        let mut y = self.yl >> 6;
        let dif = self.yu - y;
        let al = self.ap >> 2;

        if dif > 0 {
            y += (dif * al) >> 6;
        }
        else if dif < 0 {
            y += (dif * al + 0x3f) >> 6;
        }

        y
    }

    /// Decode a code to a 16-bit sample.
    pub(crate) fn decode(&mut self, code: u32) -> i32 {
        let code = code as usize & ((1 << self.code_size) - 1);

        let mut sezi = 0;
        for (b, dq) in self.b.iter().zip(&self.dq) {
            sezi += fmult(b >> 2, *dq);
        }

        let sez = sezi >> 1;
        let sei = sezi + fmult(self.a[1] >> 2, self.sr[1]) + fmult(self.a[0] >> 2, self.sr[0]);
        let se = sei >> 1;

        let y = self.step_size();

        // Reconstruct the quantized difference signal.
        let sign = code & (1 << (self.code_size - 1)) != 0;
        let dql = self.tables.dqln[code] + (y >> 2);

        let dq = if dql < 0 {
            if sign { -0x8000 } else { 0 }
        }
        else {
            let dex = (dql >> 7) & 15;
            let dqt = 128 + (dql & 127);
            let dq = (dqt << 7) >> (14 - dex);
            if sign { dq - 0x8000 } else { dq }
        };

        let sr = if dq < 0 { se - (dq & 0x3fff) } else { se + dq };
        let dqsez = sr - se + sez;

        self.update(y, self.tables.w[code] << 5, self.tables.f[code] << 9, dq, sr, dqsez);

        (sr << 2).clamp(-32768, 32767)
    }

    fn update(&mut self, y: i32, wi: i32, fi: i32, dq: i32, sr: i32, dqsez: i32) {
        let pk0 = i32::from(dqsez < 0);
        let mag = dq & 0x7fff;

        // Transition detection.
        let ylint = self.yl >> 15;
        let ylfrac = (self.yl >> 10) & 0x1f;
        let thr1 = (32 + ylfrac) << ylint;
        let thr2 = if ylint > 9 { 31 << 10 } else { thr1 };
        let dqthr = (thr2 + (thr2 >> 1)) >> 1;
        let tr = self.td && mag > dqthr;

        // Quantizer scale factor adaptation.
        self.yu = (y + ((wi - y) >> 5)).clamp(544, 5120);
        self.yl += self.yu + ((-self.yl) >> 6);

        let mut a2p = 0;

        if tr {
            self.a = [0; 2];
            self.b = [0; 6];
        }
        else {
            let pks1 = pk0 ^ self.pk[0];

            // Update the second predictor coefficient.
            a2p = self.a[1] - (self.a[1] >> 7);

            if dqsez != 0 {
                let fa1 = if pks1 != 0 { self.a[0] } else { -self.a[0] };

                if fa1 < -8191 {
                    a2p -= 0x100;
                }
                else if fa1 > 8191 {
                    a2p += 0xff;
                }
                else {
                    a2p += fa1 >> 5;
                }

                if pk0 ^ self.pk[1] != 0 {
                    if a2p <= -12160 {
                        a2p = -12288;
                    }
                    else if a2p >= 12416 {
                        a2p = 12288;
                    }
                    else {
                        a2p -= 0x80;
                    }
                }
                else if a2p <= -12416 {
                    a2p = -12288;
                }
                else if a2p >= 12160 {
                    a2p = 12288;
                }
                else {
                    a2p += 0x80;
                }
            }

            self.a[1] = a2p;

            // Update the first predictor coefficient.
            self.a[0] -= self.a[0] >> 8;

            if dqsez != 0 {
                if pks1 == 0 {
                    self.a[0] += 192;
                }
                else {
                    self.a[0] -= 192;
                }
            }

            let limit = 15360 - self.a[1];
            self.a[0] = self.a[0].clamp(-limit, limit);

            // Update the zero predictor coefficients.
            for (b, dqn) in self.b.iter_mut().zip(&self.dq) {
                *b -= *b >> if self.code_size == 5 { 9 } else { 8 };

                if mag != 0 {
                    if (dq ^ dqn) >= 0 {
                        *b += 128;
                    }
                    else {
                        *b -= 128;
                    }
                }
            }
        }

        self.dq.copy_within(0..5, 1);

        self.dq[0] = if mag == 0 {
            if dq >= 0 { 0x20 } else { -992 }
        }
        else {
            let exp = quan(mag);
            let value = (exp << 6) + ((mag << 6) >> exp);
            if dq >= 0 { value } else { value - 0x400 }
        };

        self.sr[1] = self.sr[0];
        self.sr[0] = to_float(sr);

        self.pk[1] = self.pk[0];
        self.pk[0] = pk0;

        // Tone detection.
        self.td = !tr && a2p < -11776;

        // Adaptation speed control.
        self.dms += (fi - self.dms) >> 5;
        self.dml += ((fi << 2) - self.dml) >> 7;

        if tr {
            self.ap = 256;
        }
        else if y < 1536 || self.td || ((self.dms << 2) - self.dml).abs() >= (self.dml >> 3) {
            self.ap += (0x200 - self.ap) >> 4;
        }
        else {
            self.ap += (-self.ap) >> 4;
        }
    }
}
