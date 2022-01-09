// Symphonia
// Copyright (c) 2019-2022 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The `dct` module implements the Discrete Cosine Transform (DCT).
//!
//! The (I)DCT algorithms in this module are not general purpose and are specialized for use in
//! typical audio compression applications. Therefore, some constraints may apply.

use std::f64;

use lazy_static::lazy_static;

macro_rules! dct_cos_table {
    ($bi:expr, $name:ident) => {
        lazy_static! {
            static ref $name: [f32; 1 << ($bi - 1)] = {
                const N: usize = 1 << $bi;
                const N2: usize = N >> 1;
                const FREQ: f64 = f64::consts::PI / (N as f64);

                let mut table = [0f32; N2];

                for (i, c) in table.iter_mut().enumerate() {
                    *c = (2.0 * ((i as f64 + 0.5) * FREQ).cos()).recip() as f32;
                }

                table
            };
        }
    };
}

dct_cos_table!(5, DCT_COS_TABLE_32);
dct_cos_table!(6, DCT_COS_TABLE_64);
dct_cos_table!(7, DCT_COS_TABLE_128);
dct_cos_table!(8, DCT_COS_TABLE_256);
dct_cos_table!(9, DCT_COS_TABLE_512);
dct_cos_table!(10, DCT_COS_TABLE_1024);
dct_cos_table!(11, DCT_COS_TABLE_2048);
dct_cos_table!(12, DCT_COS_TABLE_4096);
dct_cos_table!(13, DCT_COS_TABLE_8192);

fn dct_cos_table(n: u32) -> &'static [f32] {
    match n {
        32 => DCT_COS_TABLE_32.as_ref(),
        64 => DCT_COS_TABLE_64.as_ref(),
        128 => DCT_COS_TABLE_128.as_ref(),
        256 => DCT_COS_TABLE_256.as_ref(),
        512 => DCT_COS_TABLE_512.as_ref(),
        1024 => DCT_COS_TABLE_1024.as_ref(),
        2048 => DCT_COS_TABLE_2048.as_ref(),
        4096 => DCT_COS_TABLE_4096.as_ref(),
        8192 => DCT_COS_TABLE_8192.as_ref(),
        _ => unimplemented!(),
    }
}

/// Discrete Cosine Transform (DCT).
///
/// Implements the DCT using the fast algorithm described in \[1\].
///
/// \[1\] B.G. Lee, "A new algorithm to compute the discrete cosine transform", IEEE Transactions
///       on Acoustics, Speech, and Signal Processing, vol. 32, no. 6, pp. 1243-1245, 1984.
///
/// <https://ieeexplore.ieee.org/document/1164443>
pub struct Dct {
    temp: Vec<f32>,
}

impl Dct {
    /// Instantiate a N-point IMDCT.
    ///
    /// The value of `n` must be a power-of-2, and less-than or equal to 8192.
    pub fn new(n: u32) -> Dct {
        // The algorithm implemented requires a power-of-two N.
        assert!(n.is_power_of_two(), "n must be a power-of-two");
        // This limitation is somewhat arbitrary, but a limit must be set somewhere.
        assert!(n <= 8192, "maximum of 8192-point dct");

        Dct { temp: vec![0.0; n as usize] }
    }

    /// Performs a N-point Discrete Cosine Transform.
    ///
    /// The number of input samples in `src`, N, must equal the value `Dct` was instantiated with.
    /// The length of the output slice, `dst`, must also equal N. Failing to meet these requirements
    /// will throw an assertion.
    pub fn dct_ii(&mut self, src: &[f32], dst: &mut [f32]) {
        assert_eq!(src.len(), self.temp.len());
        assert_eq!(dst.len(), self.temp.len());

        // Enter recursion.
        dst.copy_from_slice(src);
        dct_ii_step(dst, &mut self.temp);
    }

    /// Performs a N-point Discrete Cosine Transform in-place.
    ///
    /// The number of input samples in `src`, N, must equal the value `Dct` was instantiated with.
    pub fn dct_ii_inplace(&mut self, src: &mut [f32]) {
        assert_eq!(src.len(), self.temp.len());

        // Enter recursion.
        dct_ii_step(src, &mut self.temp);
    }
}

fn dct_ii_step(x: &mut [f32], t: &mut [f32]) {
    let n = x.len();

    // Recursion becomes costly for small values of N. Dispatch to specialized function(s) in
    // these cases.
    if n > 32 {
        let n_half = n >> 1;

        let (xl, xr) = x.split_at_mut(n_half);
        let (tl, tr) = t.split_at_mut(n_half);
        let table = dct_cos_table(n as u32);

        for ((((tls, trs), &xls), &xrs), &c) in
            tl.iter_mut().zip(tr.iter_mut()).zip(xl.iter()).zip(xr.iter().rev()).zip(table.iter())
        {
            *tls = xls + xrs;
            *trs = (xls - xrs) * c;
        }

        dct_ii_step(tl, xl);
        dct_ii_step(tr, xr);

        for ((xsc, &tls), trsw) in x.chunks_exact_mut(2).zip(tl.iter()).zip(tr.windows(2)) {
            xsc[0] = tls;
            xsc[1] = trsw[0] + trsw[1];
        }

        x[n - 2] = t[n_half - 1];
        x[n - 1] = t[n - 1];
    }
    else {
        // TODO: We had a 32-point unrolled version of this DCT-II on-hand, but should we make this
        // smaller as it places a lower-bound on the value of N?
        dct_ii_32(x);
    }
}

/// Performs a 32-point Discrete Cosine Transform (DCT) using Byeong Gi Lee's fast algorithm
/// published in article \[1\] without inverse square-root 2 scaling.
///
/// This is a straight-forward implemention of the recursive algorithm, flattened into a single
/// function body to avoid the overhead of function calls and the stack.
///
/// \[1\] B.G. Lee, "A new algorithm to compute the discrete cosine transform", IEEE Transactions
/// on Acoustics, Speech, and Signal Processing, vol. 32, no. 6, pp. 1243-1245, 1984.
///
/// <https://ieeexplore.ieee.org/document/1164443>
fn dct_ii_32(x: &mut [f32]) {
    assert!(x.len() == 32);

    // The following tables are pre-computed values of the the following equation:
    //
    // c[i] = 1.0 / [2.0 * cos((PI / N) * (2*i + 1))]    for i = 0..N/2
    //
    // where N = [32, 16, 8, 4, 2], for COS_16, COS8, COS_4, and COS_2, respectively.
    const COS_16: [f32; 16] = [
        0.500_602_998_235_196_3,  // i= 0
        0.505_470_959_897_543_6,  // i= 1
        0.515_447_309_922_624_6,  // i= 2
        0.531_042_591_089_784_1,  // i= 3
        0.553_103_896_034_444_5,  // i= 4
        0.582_934_968_206_133_9,  // i= 5
        0.622_504_123_035_664_8,  // i= 6
        0.674_808_341_455_005_7,  // i= 7
        0.744_536_271_002_298_6,  // i= 8
        0.839_349_645_415_526_8,  // i= 9
        0.972_568_237_861_960_8,  // i=10
        1.169_439_933_432_884_7,  // i=11
        1.484_164_616_314_166_2,  // i=12
        2.057_781_009_953_410_8,  // i=13
        3.407_608_418_468_719_0,  // i=14
        10.190_008_123_548_032_9, // i=15
    ];

    const COS_8: [f32; 8] = [
        0.502_419_286_188_155_7, // i=0
        0.522_498_614_939_688_9, // i=1
        0.566_944_034_816_357_7, // i=2
        0.646_821_783_359_990_1, // i=3
        0.788_154_623_451_250_2, // i=4
        1.060_677_685_990_347_1, // i=5
        1.722_447_098_238_334_2, // i=6
        5.101_148_618_689_155_3, // i=7
    ];

    const COS_4: [f32; 4] = [
        0.509_795_579_104_159_2, // i=0
        0.601_344_886_935_045_3, // i=1
        0.899_976_223_136_415_6, // i=2
        2.562_915_447_741_505_5, // i=3
    ];

    const COS_2: [f32; 2] = [
        0.541_196_100_146_197_0, // i=0
        1.306_562_964_876_376_4, // i=1
    ];

    const COS_1: f32 = 0.707_106_781_186_547_5;

    // 16-point DCT decomposition
    let mut t0 = [
        (x[0] + x[32 - 1]),
        (x[1] + x[32 - 2]),
        (x[2] + x[32 - 3]),
        (x[3] + x[32 - 4]),
        (x[4] + x[32 - 5]),
        (x[5] + x[32 - 6]),
        (x[6] + x[32 - 7]),
        (x[7] + x[32 - 8]),
        (x[8] + x[32 - 9]),
        (x[9] + x[32 - 10]),
        (x[10] + x[32 - 11]),
        (x[11] + x[32 - 12]),
        (x[12] + x[32 - 13]),
        (x[13] + x[32 - 14]),
        (x[14] + x[32 - 15]),
        (x[15] + x[32 - 16]),
        (x[0] - x[32 - 1]) * COS_16[0],
        (x[1] - x[32 - 2]) * COS_16[1],
        (x[2] - x[32 - 3]) * COS_16[2],
        (x[3] - x[32 - 4]) * COS_16[3],
        (x[4] - x[32 - 5]) * COS_16[4],
        (x[5] - x[32 - 6]) * COS_16[5],
        (x[6] - x[32 - 7]) * COS_16[6],
        (x[7] - x[32 - 8]) * COS_16[7],
        (x[8] - x[32 - 9]) * COS_16[8],
        (x[9] - x[32 - 10]) * COS_16[9],
        (x[10] - x[32 - 11]) * COS_16[10],
        (x[11] - x[32 - 12]) * COS_16[11],
        (x[12] - x[32 - 13]) * COS_16[12],
        (x[13] - x[32 - 14]) * COS_16[13],
        (x[14] - x[32 - 15]) * COS_16[14],
        (x[15] - x[32 - 16]) * COS_16[15],
    ];

    // 16-point DCT decomposition of t0[0..16]
    {
        let mut t1 = [
            (t0[0] + t0[16 - 1]),
            (t0[1] + t0[16 - 2]),
            (t0[2] + t0[16 - 3]),
            (t0[3] + t0[16 - 4]),
            (t0[4] + t0[16 - 5]),
            (t0[5] + t0[16 - 6]),
            (t0[6] + t0[16 - 7]),
            (t0[7] + t0[16 - 8]),
            (t0[0] - t0[16 - 1]) * COS_8[0],
            (t0[1] - t0[16 - 2]) * COS_8[1],
            (t0[2] - t0[16 - 3]) * COS_8[2],
            (t0[3] - t0[16 - 4]) * COS_8[3],
            (t0[4] - t0[16 - 5]) * COS_8[4],
            (t0[5] - t0[16 - 6]) * COS_8[5],
            (t0[6] - t0[16 - 7]) * COS_8[6],
            (t0[7] - t0[16 - 8]) * COS_8[7],
        ];

        // 8-point DCT decomposition of t1[0..8]
        {
            let mut t2 = [
                (t1[0] + t1[8 - 1]),
                (t1[1] + t1[8 - 2]),
                (t1[2] + t1[8 - 3]),
                (t1[3] + t1[8 - 4]),
                (t1[0] - t1[8 - 1]) * COS_4[0],
                (t1[1] - t1[8 - 2]) * COS_4[1],
                (t1[2] - t1[8 - 3]) * COS_4[2],
                (t1[3] - t1[8 - 4]) * COS_4[3],
            ];

            // 4-point DCT decomposition of t2[0..4]
            {
                let mut t3 = [
                    (t2[0] + t2[4 - 1]),
                    (t2[1] + t2[4 - 2]),
                    (t2[0] - t2[4 - 1]) * COS_2[0],
                    (t2[1] - t2[4 - 2]) * COS_2[1],
                ];

                // 2-point DCT decomposition of t3[0..2]
                {
                    let t4 = [(t3[0] + t3[2 - 1]), (t3[0] - t3[2 - 1]) * COS_1];

                    t3[0] = t4[0];
                    t3[1] = t4[1];
                }

                // 2-point DCT decomposition of t3[2..4]
                {
                    let t4 = [(t3[2] + t3[4 - 1]), (t3[2] - t3[4 - 1]) * COS_1];

                    t3[2 + 0] = t4[0];
                    t3[2 + 1] = t4[1];
                }

                t2[0 + 0] = t3[0];
                t2[0 + 1] = t3[2] + t3[3];
                t2[0 + 2] = t3[1];
                t2[0 + 3] = t3[3];
            }

            // 4-point DCT decomposition of t2[4..8]
            {
                let mut t3 = [
                    (t2[4] + t2[8 - 1]),
                    (t2[5] + t2[8 - 2]),
                    (t2[4] - t2[8 - 1]) * COS_2[0],
                    (t2[5] - t2[8 - 2]) * COS_2[1],
                ];

                // 2-point DCT decomposition of t3[0..2]
                {
                    let t4 = [(t3[0] + t3[2 - 1]), (t3[0] - t3[2 - 1]) * COS_1];

                    t3[0] = t4[0];
                    t3[1] = t4[1];
                }

                // 2-point DCT decomposition of t3[2..4]
                {
                    let t4 = [(t3[2] + t3[4 - 1]), (t3[2] - t3[4 - 1]) * COS_1];

                    t3[2 + 0] = t4[0];
                    t3[2 + 1] = t4[1];
                }

                t2[4 + 0] = t3[0];
                t2[4 + 1] = t3[2] + t3[3];
                t2[4 + 2] = t3[1];
                t2[4 + 3] = t3[3];
            }

            // Recombine t2[0..4] and t2[4..8], overwriting t1[0..8].
            for i in 0..3 {
                t1[(i << 1) + 0] = t2[i];
                t1[(i << 1) + 1] = t2[4 + i] + t2[4 + i + 1];
            }

            t1[8 - 2] = t2[4 - 1];
            t1[8 - 1] = t2[8 - 1];
        }

        // 8-point DCT decomposition of t1[8..16]
        {
            let mut t2 = [
                (t1[8] + t1[16 - 1]),
                (t1[9] + t1[16 - 2]),
                (t1[10] + t1[16 - 3]),
                (t1[11] + t1[16 - 4]),
                (t1[8] - t1[16 - 1]) * COS_4[0],
                (t1[9] - t1[16 - 2]) * COS_4[1],
                (t1[10] - t1[16 - 3]) * COS_4[2],
                (t1[11] - t1[16 - 4]) * COS_4[3],
            ];

            // 4-point DCT decomposition of t2[0..4]
            {
                let mut t3 = [
                    (t2[0] + t2[4 - 1]),
                    (t2[1] + t2[4 - 2]),
                    (t2[0] - t2[4 - 1]) * COS_2[0],
                    (t2[1] - t2[4 - 2]) * COS_2[1],
                ];

                // 2-point DCT decomposition of t3[0..2]
                {
                    let t4 = [(t3[0] + t3[2 - 1]), (t3[0] - t3[2 - 1]) * COS_1];

                    t3[0] = t4[0];
                    t3[1] = t4[1];
                }

                // 2-point DCT decomposition of t3[2..4]
                {
                    let t4 = [(t3[2] + t3[4 - 1]), (t3[2] - t3[4 - 1]) * COS_1];

                    t3[2 + 0] = t4[0];
                    t3[2 + 1] = t4[1];
                }

                t2[0 + 0] = t3[0];
                t2[0 + 1] = t3[2] + t3[3];
                t2[0 + 2] = t3[1];
                t2[0 + 3] = t3[3];
            }

            // 4-point DCT decomposition of t2[4..8]
            {
                let mut t3 = [
                    (t2[4] + t2[8 - 1]),
                    (t2[5] + t2[8 - 2]),
                    (t2[4] - t2[8 - 1]) * COS_2[0],
                    (t2[5] - t2[8 - 2]) * COS_2[1],
                ];

                // 2-point DCT decomposition of t3[0..2]
                {
                    let t4 = [(t3[0] + t3[2 - 1]), (t3[0] - t3[2 - 1]) * COS_1];

                    t3[0] = t4[0];
                    t3[1] = t4[1];
                }

                // 2-point DCT decomposition of t3[2..4]
                {
                    let t4 = [(t3[2] + t3[4 - 1]), (t3[2] - t3[4 - 1]) * COS_1];

                    t3[2 + 0] = t4[0];
                    t3[2 + 1] = t4[1];
                }

                t2[4 + 0] = t3[0];
                t2[4 + 1] = t3[2] + t3[3];
                t2[4 + 2] = t3[1];
                t2[4 + 3] = t3[3];
            }

            // Recombine t2[0..4] and t2[4..8], overwriting t1[8..16].
            for i in 0..3 {
                t1[8 + (i << 1) + 0] = t2[i];
                t1[8 + (i << 1) + 1] = t2[4 + i] + t2[4 + i + 1];
            }

            t1[16 - 2] = t2[4 - 1];
            t1[16 - 1] = t2[8 - 1];
        }

        // Recombine t1[0..8] and t1[8..16], overwriting t0[0..16].
        for i in 0..7 {
            t0[(i << 1) + 0] = t1[i];
            t0[(i << 1) + 1] = t1[8 + i] + t1[8 + i + 1];
        }

        t0[16 - 2] = t1[8 - 1];
        t0[16 - 1] = t1[16 - 1];
    }

    // 16-point DCT decomposition of t0[16..32]
    {
        let mut t1 = [
            (t0[16] + t0[32 - 1]),
            (t0[17] + t0[32 - 2]),
            (t0[18] + t0[32 - 3]),
            (t0[19] + t0[32 - 4]),
            (t0[20] + t0[32 - 5]),
            (t0[21] + t0[32 - 6]),
            (t0[22] + t0[32 - 7]),
            (t0[23] + t0[32 - 8]),
            (t0[16] - t0[32 - 1]) * COS_8[0],
            (t0[17] - t0[32 - 2]) * COS_8[1],
            (t0[18] - t0[32 - 3]) * COS_8[2],
            (t0[19] - t0[32 - 4]) * COS_8[3],
            (t0[20] - t0[32 - 5]) * COS_8[4],
            (t0[21] - t0[32 - 6]) * COS_8[5],
            (t0[22] - t0[32 - 7]) * COS_8[6],
            (t0[23] - t0[32 - 8]) * COS_8[7],
        ];

        // 8-point DCT decomposition of t1[0..8]
        {
            let mut t2 = [
                (t1[0] + t1[8 - 1]),
                (t1[1] + t1[8 - 2]),
                (t1[2] + t1[8 - 3]),
                (t1[3] + t1[8 - 4]),
                (t1[0] - t1[8 - 1]) * COS_4[0],
                (t1[1] - t1[8 - 2]) * COS_4[1],
                (t1[2] - t1[8 - 3]) * COS_4[2],
                (t1[3] - t1[8 - 4]) * COS_4[3],
            ];

            // 4-point DCT decomposition of t2[0..4]
            {
                let mut t3 = [
                    (t2[0] + t2[4 - 1]),
                    (t2[1] + t2[4 - 2]),
                    (t2[0] - t2[4 - 1]) * COS_2[0],
                    (t2[1] - t2[4 - 2]) * COS_2[1],
                ];

                // 2-point DCT decomposition of t3[0..2]
                {
                    let t4 = [(t3[0] + t3[2 - 1]), (t3[0] - t3[2 - 1]) * COS_1];

                    t3[0] = t4[0];
                    t3[1] = t4[1];
                }

                // 2-point DCT decomposition of t3[2..4]
                {
                    let t4 = [(t3[2] + t3[4 - 1]), (t3[2] - t3[4 - 1]) * COS_1];

                    t3[2 + 0] = t4[0];
                    t3[2 + 1] = t4[1];
                }

                t2[0 + 0] = t3[0];
                t2[0 + 1] = t3[2] + t3[3];
                t2[0 + 2] = t3[1];
                t2[0 + 3] = t3[3];
            }

            // 4-point DCT decomposition of t2[4..8]
            {
                let mut t3 = [
                    (t2[4] + t2[8 - 1]),
                    (t2[5] + t2[8 - 2]),
                    (t2[4] - t2[8 - 1]) * COS_2[0],
                    (t2[5] - t2[8 - 2]) * COS_2[1],
                ];

                // 2-point DCT decomposition of t3[0..2]
                {
                    let t4 = [(t3[0] + t3[2 - 1]), (t3[0] - t3[2 - 1]) * COS_1];

                    t3[0] = t4[0];
                    t3[1] = t4[1];
                }

                // 2-point DCT decomposition of t3[2..4]
                {
                    let t4 = [(t3[2] + t3[4 - 1]), (t3[2] - t3[4 - 1]) * COS_1];

                    t3[2 + 0] = t4[0];
                    t3[2 + 1] = t4[1];
                }

                t2[4 + 0] = t3[0];
                t2[4 + 1] = t3[2] + t3[3];
                t2[4 + 2] = t3[1];
                t2[4 + 3] = t3[3];
            }

            // Recombine t2[0..4] and t2[4..8], overwriting t1[0..8].
            for i in 0..3 {
                t1[(i << 1) + 0] = t2[i];
                t1[(i << 1) + 1] = t2[4 + i] + t2[4 + i + 1];
            }

            t1[8 - 2] = t2[4 - 1];
            t1[8 - 1] = t2[8 - 1];
        }

        // 8-point DCT decomposition of t1[8..16]
        {
            let mut t2 = [
                (t1[8] + t1[16 - 1]),
                (t1[9] + t1[16 - 2]),
                (t1[10] + t1[16 - 3]),
                (t1[11] + t1[16 - 4]),
                (t1[8] - t1[16 - 1]) * COS_4[0],
                (t1[9] - t1[16 - 2]) * COS_4[1],
                (t1[10] - t1[16 - 3]) * COS_4[2],
                (t1[11] - t1[16 - 4]) * COS_4[3],
            ];

            // 4-point DCT decomposition of t2[0..4]
            {
                let mut t3 = [
                    (t2[0] + t2[4 - 1]),
                    (t2[1] + t2[4 - 2]),
                    (t2[0] - t2[4 - 1]) * COS_2[0],
                    (t2[1] - t2[4 - 2]) * COS_2[1],
                ];

                // 2-point DCT decomposition of t3[0..2]
                {
                    let t4 = [(t3[0] + t3[2 - 1]), (t3[0] - t3[2 - 1]) * COS_1];

                    t3[0] = t4[0];
                    t3[1] = t4[1];
                }

                // 2-point DCT decomposition of t3[2..4]
                {
                    let t4 = [(t3[2] + t3[4 - 1]), (t3[2] - t3[4 - 1]) * COS_1];

                    t3[2 + 0] = t4[0];
                    t3[2 + 1] = t4[1];
                }

                t2[0 + 0] = t3[0];
                t2[0 + 1] = t3[2] + t3[3];
                t2[0 + 2] = t3[1];
                t2[0 + 3] = t3[3];
            }

            // 4-point DCT decomposition of t2[4..8]
            {
                let mut t3 = [
                    (t2[4] + t2[8 - 1]),
                    (t2[5] + t2[8 - 2]),
                    (t2[4] - t2[8 - 1]) * COS_2[0],
                    (t2[5] - t2[8 - 2]) * COS_2[1],
                ];

                // 2-point DCT decomposition of t3[0..2]
                {
                    let t4 = [(t3[0] + t3[2 - 1]), (t3[0] - t3[2 - 1]) * COS_1];

                    t3[0] = t4[0];
                    t3[1] = t4[1];
                }

                // 2-point DCT decomposition of t3[2..4]
                {
                    let t4 = [(t3[2] + t3[4 - 1]), (t3[2] - t3[4 - 1]) * COS_1];

                    t3[2 + 0] = t4[0];
                    t3[2 + 1] = t4[1];
                }

                t2[4 + 0] = t3[0];
                t2[4 + 1] = t3[2] + t3[3];
                t2[4 + 2] = t3[1];
                t2[4 + 3] = t3[3];
            }

            // Recombine t2[0..4] and t2[4..8], overwriting t1[8..16].
            for i in 0..3 {
                t1[8 + (i << 1) + 0] = t2[i];
                t1[8 + (i << 1) + 1] = t2[4 + i] + t2[4 + i + 1];
            }

            t1[16 - 2] = t2[4 - 1];
            t1[16 - 1] = t2[8 - 1];
        }

        // Recombine t1[0..8] and t1[8..16], overwriting t0[0..16].
        for i in 0..7 {
            t0[16 + (i << 1) + 0] = t1[i];
            t0[16 + (i << 1) + 1] = t1[8 + i] + t1[8 + i + 1];
        }

        t0[32 - 2] = t1[8 - 1];
        t0[32 - 1] = t1[16 - 1];
    }

    // Recombine t1[0..16] and t1[16..32] into y.
    for i in 0..15 {
        x[(i << 1) + 0] = t0[i];
        x[(i << 1) + 1] = t0[16 + i] + t0[16 + i + 1];
    }

    x[32 - 2] = t0[16 - 1];
    x[32 - 1] = t0[32 - 1];
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64;

    fn dct_analytical(x: &[f32], y: &mut [f32]) {
        let n = x.len();

        let w = f64::consts::PI / n as f64;

        for i in 0..n {
            let mut sum = 0.0;
            for j in 0..n {
                sum += (x[j] as f64) * (w * (i as f64) * ((j as f64) + 0.5)).cos();
            }
            y[i] = sum as f32;
        }
    }

    #[test]
    fn verify_dct_ii_short() {
        #[rustfmt::skip]
        const TEST_VECTOR: [f32; 32] = [
            0.1710, 0.1705, 0.3476, 0.1866, 0.4784, 0.6525, 0.2690, 0.9996,
            0.1864, 0.7277, 0.1163, 0.6620, 0.0911, 0.3225, 0.1126, 0.5344,
            0.7839, 0.9741, 0.8757, 0.5763, 0.5926, 0.2756, 0.1757, 0.6531,
            0.7101, 0.7376, 0.1924, 0.0351, 0.8044, 0.2409, 0.9347, 0.9417,
        ];

        let mut actual = [0f32; 32];
        let mut expected = [0f32; 32];

        let mut dct = Dct::new(32);
        dct.dct_ii(&TEST_VECTOR, &mut actual);

        dct_analytical(&TEST_VECTOR, &mut expected);
        for i in 0..32 {
            assert!((actual[i] - expected[i]).abs() < 0.00001);
        }
    }

    #[test]
    fn verify_dct_ii_long() {
        #[rustfmt::skip]
        const TEST_VECTOR: [f32; 256] = [
            -0.2206,  0.1221, -0.1538, -0.3949,  0.9577, -0.3674,  0.0843,  0.6186,
             0.0786,  0.5193,  0.5449,  0.3213, -0.1223,  0.0408, -0.0130, -0.0495,
             0.5088,  0.9099, -0.4402, -0.0315, -0.7183,  0.1811,  0.3019, -0.8413,
            -0.5740, -0.6372,  0.0341, -0.1903,  0.0071,  0.7627,  0.4080,  0.7445,
             0.3896,  0.1956, -0.0735,  0.0235, -0.0258,  0.6105, -0.0132,  0.0645,
            -0.1403, -0.5696,  0.8074, -0.2233,  0.5913, -0.4121, -0.3175, -0.4807,
            -0.4599, -0.4462,  0.0971, -0.0174, -0.0383, -0.9007,  0.7615, -0.4130,
             0.0452, -0.5287, -0.5030,  0.4733, -0.8281,  0.3222,  0.0809,  0.0320,
             0.3767, -0.3435,  0.9541, -0.0575, -0.8242, -0.9775, -0.1079,  0.3029,
            -0.8443, -0.6308,  0.1159, -0.2204, -0.3212,  0.0357,  0.0703,  0.7130,
            -0.3782,  0.1006,  0.7246,  0.5880, -0.6111, -0.5137, -0.1145, -0.3680,
             0.2494,  0.2553, -0.5659, -0.6298,  0.1392, -0.0020,  0.9737, -0.2716,
             0.0697, -0.4455, -0.7427, -0.2963,  0.452,  -0.3149, -0.7498,  0.5100,
            -0.1139,  0.5958, -0.0308, -0.6761,  0.0164, -0.0646, -0.3338,  0.4079,
            -0.0510, -0.0494, -0.3273, -0.0273,  0.7553, -0.0856, -0.3496,  0.0088,
            -0.2072,  0.1259, -0.5635,  0.2155, -0.6006,  0.2973, -0.3359, -0.9719,
            -0.0277,  0.7293,  0.9611, -0.7272,  0.1031, -0.1038, -0.2772, -0.2660,
            -0.4362, -0.4180,  0.4680, -0.5491, -0.1011, -0.8015, -0.0052,  0.0650,
            -0.9727, -0.1275, -0.5002,  0.4489, -0.4135, -0.7062,  0.1079,  0.6841,
             0.8715,  0.6371, -0.1195, -0.2584,  0.0376,  0.5324, -0.4332, -0.2072,
             0.1551, -0.3402, -0.9390, -0.5519,  0.0435, -0.0143,  0.7945,  0.9227,
            -0.2940,  0.7307, -0.6797, -0.2594, -0.3799, -0.2197, -0.9969, -0.5420,
            -0.6299, -0.5407, -0.0043,  0.0555, -0.5249,  0.0861,  0.0417,  0.8385,
            -0.2329, -0.3030,  0.2990, -0.1459,  0.5442, -0.1575, -0.7049, -0.8823,
             0.6298, -0.5132, -0.5228,  0.4108,  0.5986,  0.7738, -0.0726,  0.5995,
            -0.6931, -0.6978, -0.3004, -0.4843, -0.5923, -0.1717, -0.3906,  0.5776,
             0.3917, -0.6866, -0.6574,  0.0996,  0.1991, -0.8099,  0.3079,  0.0873,
            -0.2757,  0.0221, -0.2491, -0.0491,  0.4347, -0.3696,  0.9161, -0.0299,
             0.3613, -0.3709,  0.2694,  0.2962, -0.1211,  0.7926,  0.1552,  0.4116,
            -0.0559, -0.4295, -0.3380, -0.3943, -0.0280, -0.4846,  0.2796,  0.6129,
            -0.6100, -0.5164,  0.2817, -0.4388,  0.3060, -0.3001, -0.9920,  0.1849,
            -0.1679,  0.4384, -0.3204,  0.0196, -0.3825,  0.9539,  0.1455,  0.5182,
        ];

        let mut actual = [0f32; 256];
        let mut expected = [0f32; 256];

        dct_analytical(&TEST_VECTOR, &mut expected);

        let mut dct = Dct::new(256);
        dct.dct_ii(&TEST_VECTOR, &mut actual);

        for i in 0..256 {
            assert!((actual[i] - expected[i]).abs() < 0.0001);
        }
    }

    #[test]
    fn verify_dct_ii_32() {
        #[rustfmt::skip]
        const TEST_VECTOR: [f32; 32] = [
            0.1710, 0.1705, 0.3476, 0.1866, 0.4784, 0.6525, 0.2690, 0.9996,
            0.1864, 0.7277, 0.1163, 0.6620, 0.0911, 0.3225, 0.1126, 0.5344,
            0.7839, 0.9741, 0.8757, 0.5763, 0.5926, 0.2756, 0.1757, 0.6531,
            0.7101, 0.7376, 0.1924, 0.0351, 0.8044, 0.2409, 0.9347, 0.9417,
        ];

        let mut actual = TEST_VECTOR;
        let mut expected = [0f32; 32];

        dct_analytical(&TEST_VECTOR, &mut expected);

        dct_ii_32(&mut actual);

        for i in 0..32 {
            assert!((actual[i] - expected[i]).abs() < 0.00001);
        }
    }
}
