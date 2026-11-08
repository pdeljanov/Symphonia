// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The complex-valued SBR QMF filterbanks (4.6.18.4.1 and 4.6.18.4.2).
//!
//! Both filterbanks follow the flowcharts of Figure 4.42 and Figure 4.43. The matrix operations
//! `M u` and `N X` are evaluated with fast Fourier transforms: the analysis modulation after
//! factoring the complex exponentials into a pre-twiddle, a DFT, and a post-twiddle, and the
//! synthesis modulation as a pair of DCT-IV type transforms of half the length. The sample delay
//! lines are buffers that are not shifted for every QMF subsample.

mod window;

use std::f64::consts::PI;

use symphonia_core::dsp::complex::Complex;
use symphonia_core::dsp::fft::Fft;

use crate::aac::sbr::types::{QMF_ANALYSIS_BANDS, QMF_BANDS};

pub use window::QMF_WINDOW;

/// Length of the analysis input sample array x (4.6.18.4.1).
const ANALYSIS_LEN: usize = 10 * QMF_ANALYSIS_BANDS;
/// Length of the synthesis sample array v (4.6.18.4.2).
const SYNTHESIS_LEN: usize = 20 * QMF_BANDS;
/// The number of QMF subsamples the analysis input buffer can take before it must be compacted.
const ANALYSIS_BUF_SLOTS: usize = 32;
/// Length of the analysis input buffer: the 288 most recent past samples of x and room for
/// `ANALYSIS_BUF_SLOTS` subsamples of new samples.
const ANALYSIS_BUF_LEN: usize = ANALYSIS_LEN + (ANALYSIS_BUF_SLOTS - 1) * QMF_ANALYSIS_BANDS;
/// The size of the complex FFTs of the synthesis filterbank.
const FFT_LEN: usize = 32;

const ZERO: Complex<f32> = Complex { re: 0.0, im: 0.0 };

/// exp(i * phi) in single precision, computed in double precision.
fn cis(phi: f64) -> Complex<f32> {
    Complex::new(phi.cos() as f32, phi.sin() as f32)
}

/// The 32-band complex-valued analysis QMF bank (4.6.18.4.1).
pub struct QmfAnalysis {
    /// The time-domain input samples in chronological order. The sample array x of the
    /// specification is `x(n) = buf[end - 1 - n]`, 0 <= n < 320.
    buf: Box<[f32; ANALYSIS_BUF_LEN]>,
    /// The end of the valid samples in `buf`.
    end: usize,
    /// The window in the chronological order of `buf`: `window[t] = c(2 * (319 - t))`.
    window: [f32; ANALYSIS_LEN],
    /// A 64-point FFT.
    fft: Fft,
    /// Pre-twiddle, exp(-i * pi * (2n - 0.5) / 128), 0 <= n < 64.
    pre: [Complex<f32>; 2 * QMF_ANALYSIS_BANDS],
    /// Post-twiddle, 2 * exp(i * pi * k / 128), 0 <= k < 32.
    post: [Complex<f32>; QMF_ANALYSIS_BANDS],
    /// FFT work buffer.
    fft_buf: [Complex<f32>; 2 * QMF_ANALYSIS_BANDS],
}

impl Default for QmfAnalysis {
    fn default() -> Self {
        Self::new()
    }
}

impl QmfAnalysis {
    pub fn new() -> Self {
        const N: usize = 2 * QMF_ANALYSIS_BANDS;

        let window = std::array::from_fn(|t| QMF_WINDOW[2 * (ANALYSIS_LEN - 1 - t)]);

        let pre = std::array::from_fn(|n| cis(-PI * (2.0 * n as f64 - 0.5) / 128.0));

        let post = std::array::from_fn(|k| {
            let phi = PI * k as f64 / 128.0;
            Complex::new(2.0 * phi.cos() as f32, 2.0 * phi.sin() as f32)
        });

        QmfAnalysis {
            buf: Box::new([0.0; ANALYSIS_BUF_LEN]),
            end: ANALYSIS_LEN,
            window,
            fft: Fft::new(N),
            pre,
            post,
            fft_buf: [ZERO; N],
        }
    }

    /// Reset the filterbank state to its initial (all zero) state.
    pub fn reset(&mut self) {
        self.buf.fill(0.0);
        self.end = ANALYSIS_LEN;
    }

    /// Consume 32 new time-domain input samples and produce one QMF subsample of 32 subbands.
    ///
    /// The first 32 samples of `input` are consumed in chronological order. If `input` holds fewer
    /// than 32 samples, the missing samples are taken to be zero.
    pub fn analyze(&mut self, input: &[f32], out: &mut [Complex<f32>; QMF_ANALYSIS_BANDS]) {
        const N: usize = QMF_ANALYSIS_BANDS;

        // Discard the oldest 32 samples of x and append the new samples. Instead of shifting x
        // for every subsample, the buffer is compacted only when it is full.
        if self.end + N > ANALYSIS_BUF_LEN {
            self.buf.copy_within(self.end + N - ANALYSIS_LEN..self.end, 0);
            self.end = ANALYSIS_LEN - N;
        }

        let new = &mut self.buf[self.end..self.end + N];
        match input.first_chunk::<N>() {
            Some(input) => new.copy_from_slice(input),
            None => {
                new[..input.len()].copy_from_slice(input);
                new[input.len()..].fill(0.0);
            }
        }
        self.end += N;

        // Window the samples with every other window coefficient, z(n) = x(n) * c(2n), and sum the
        // windowed samples to form u(n) = sum_j z(n + 64j), 0 <= n < 64. In the chronological
        // order of the buffer, x(n + 64j) = y(t) and c(2 * (n + 64j)) = window(t) with
        // t = 63 - n + 64 * (4 - j), where y holds the 320 most recent samples. Thus, u(63 - m)
        // is accumulated in `u_rev[m]`.
        //
        // The sums are accumulated in groups of 16 values of u, in the order of increasing j.
        const GROUP: usize = 16;
        let y = &self.buf[self.end - ANALYSIS_LEN..self.end];
        let mut u_rev = [0.0f32; 2 * N];
        for (g, u_rev) in u_rev.chunks_exact_mut(GROUP).enumerate() {
            let mut acc = [0.0f32; GROUP];
            for j in 0..5 {
                let t = 64 * (4 - j) + g * GROUP;
                let y = &y[t..t + GROUP];
                let w = &self.window[t..t + GROUP];
                for ((acc, &y), &w) in acc.iter_mut().zip(y).zip(w) {
                    *acc += y * w;
                }
            }
            u_rev.copy_from_slice(&acc);
        }

        for ((b, pre), &u) in self.fft_buf.iter_mut().zip(self.pre.iter()).zip(u_rev.iter().rev()) {
            *b = *pre * u;
        }

        // The matrix operation W(k) = sum_n 2 * u(n) * exp(i * pi * (k + 0.5) * (2n - 0.5) / 64)
        // is evaluated as
        //
        //   W(k) = conj(2 * exp(i * pi * k / 128) * FFT{ u(n) * exp(-i * pi * (2n - 0.5) / 128) }),
        //
        // where FFT is the 64-point forward DFT.
        self.fft.fft_inplace(&mut self.fft_buf);

        for ((w, b), post) in out.iter_mut().zip(self.fft_buf.iter()).zip(self.post.iter()) {
            *w = (*post * *b).conj();
        }
    }
}

/// The 64-band complex-valued synthesis QMF bank (4.6.18.4.2).
pub struct QmfSynthesis {
    /// The sample array v as a ring buffer, v(n) = `v[(pos + n) % 1280]`.
    v: Box<[f32; SYNTHESIS_LEN]>,
    /// The position of v(0) in the ring buffer, a multiple of 128.
    pos: usize,
    /// A 32-point FFT.
    fft: Fft,
    /// Pre-twiddles, exp(-i * pi * j / 64) and exp(-i * pi * 3j / 64), 0 <= j < 32.
    pre: [(Complex<f32>, Complex<f32>); FFT_LEN],
    /// Post-twiddles, P(j) and conj(P(31 - j)), 0 <= j < 32, where P(m) = exp(-i * pi *
    /// (m + 0.25) / 64) / 64.
    post: [(Complex<f32>, Complex<f32>); FFT_LEN],
    /// FFT work buffers.
    buf_d: [Complex<f32>; FFT_LEN],
    buf_s: [Complex<f32>; FFT_LEN],
}

impl Default for QmfSynthesis {
    fn default() -> Self {
        Self::new()
    }
}

impl QmfSynthesis {
    pub fn new() -> Self {
        let pre =
            std::array::from_fn(|j| (cis(-PI * j as f64 / 64.0), cis(-PI * (3 * j) as f64 / 64.0)));
        let p = |m: usize| {
            let phi = -PI * (m as f64 + 0.25) / 64.0;
            Complex::new((phi.cos() / 64.0) as f32, (phi.sin() / 64.0) as f32)
        };
        let post = std::array::from_fn(|j| (p(j), p(FFT_LEN - 1 - j).conj()));

        QmfSynthesis {
            v: Box::new([0.0; SYNTHESIS_LEN]),
            pos: 0,
            fft: Fft::new(FFT_LEN),
            pre,
            post,
            buf_d: [ZERO; FFT_LEN],
            buf_s: [ZERO; FFT_LEN],
        }
    }

    /// Reset the filterbank state to its initial (all zero) state.
    pub fn reset(&mut self) {
        self.v.fill(0.0);
        self.pos = 0;
    }

    /// Consume one QMF subsample of 64 subbands and produce 64 time-domain output samples.
    ///
    /// The output samples are written to the first 64 samples of `out` in chronological order. If
    /// `out` is shorter, the remaining output samples are discarded.
    pub fn synthesize(&mut self, input: &[Complex<f32>; QMF_BANDS], out: &mut [f32]) {
        const N: usize = 2 * QMF_BANDS;

        // Shift the samples in v by 128 positions, discarding the oldest 128 samples, by moving
        // the start of the ring buffer.
        self.pos = (self.pos + SYNTHESIS_LEN - N) % SYNTHESIS_LEN;

        // The matrix operation
        //
        //   v(n) = Re{ sum_k X(k) / 64 * exp(i * pi * (k + 0.5) * (2n - 255) / 128) }
        //
        // for 0 <= n < 128 is, with X(k) = a(k) + i * b(k) and since (2n - 255) / 2 = n + 0.5 -
        // 128,
        //
        //   v(n) = -(C(n) - S(n)) / 64,
        //
        // where C is the 64-point DCT-IV of a, C(n) = sum_k a(k) cos(pi / 64 * (k + 0.5) *
        // (n + 0.5)), and S is the 64-point DST-IV of b. Both are extended to 0 <= n < 128 by
        // C(127 - n) = -C(n) and S(127 - n) = S(n). The DST-IV is S(n) = (-1)^n * D(n), where D is
        // the DCT-IV of b(63 - k).
        //
        // A 64-point DCT-IV of c(k) is computed with a 32-point FFT as Y(m) = P(m) * FFT{ (c(2j) +
        // i * c(63 - 2j)) * exp(-i * pi * j / 64) }, where P(m) = exp(-i * pi * (m + 0.25) / 64),
        // DCT-IV(2m) = Re{Y(m)} and DCT-IV(63 - 2m) = -Im{Y(m)}.
        //
        // With Y_a and Y_b (including the scaling by 1 / 64) being the DCT-IVs of a(k) and
        // b(63 - k):
        //
        //   v(2m) = Re{Y_b(m) - Y_a(m)},  v(64 + 2m) = Im{Y_b(m) - Y_a(m)},
        //   v(127 - 2m) = Re{Y_a(m) + Y_b(m)},  v(63 - 2m) = Im{Y_a(m) + Y_b(m)}.
        //
        // By linearity, the differences and sums are the DCT-IVs of the differences and sums of
        // the inputs, z_d(j) = z_b(j) - z_a(j) and z_s(j) = z_a(j) + z_b(j), with z_a(j) = (a(2j) +
        // i * a(63 - 2j)) * exp(-i * pi * j / 64) and z_b(j) = (b(63 - 2j) + i * b(2j)) *
        // exp(-i * pi * j / 64). The sums are needed in reverse order, m = 31 - j. Since
        // conj(FFT{conj(x(n)) * exp(-i * 2 * pi * n / 32)}(j)) = FFT{x(n)}(31 - j), they are
        // obtained from the FFT of conj(z_s(j)) * exp(-i * 2 * pi * j / 32) = conj(u(j)) *
        // exp(-i * pi * 3j / 64), where u(j) = z_s(j) * exp(i * pi * j / 64).
        let lo_hi = input.chunks_exact(2).zip(input.chunks_exact(2).rev());
        let bufs = self.buf_d.iter_mut().zip(self.buf_s.iter_mut());

        for (((lo, hi), (d, s)), &(w1, w3)) in lo_hi.zip(bufs).zip(self.pre.iter()) {
            // X(2j) and X(63 - 2j).
            let (lo, hi) = (lo[0], hi[1]);
            *d = Complex::new(hi.im - lo.re, lo.im - hi.re) * w1;
            *s = Complex::new(lo.re + hi.im, -(hi.re + lo.im)) * w3;
        }

        self.fft.fft_inplace(&mut self.buf_d);
        self.fft.fft_inplace(&mut self.buf_s);

        // The pairs v(2j), v(2j + 1) and v(64 + 2j), v(65 + 2j) are formed from the difference for
        // m = j, P(j) * D(j), and the sum for m = 31 - j, conj(conj(P(31 - j)) * S(j)).
        let (v_lo, v_hi) = self.v[self.pos..self.pos + N].split_at_mut(QMF_BANDS);

        let ffts = self.buf_d.iter().zip(&self.buf_s).zip(&self.post);
        let pairs = v_lo.chunks_exact_mut(2).zip(v_hi.chunks_exact_mut(2));

        for (((d, s), &(p, p_rev)), (lo, hi)) in ffts.zip(pairs) {
            let diff = *d * p;
            let sum = *s * p_rev;
            lo[0] = diff.re;
            lo[1] = -sum.im;
            hi[0] = diff.im;
            hi[1] = sum.re;
        }

        // Extract the 640-element array g from v, window it with c to form w, and sum the samples
        // of w to produce 64 new output samples. The 64-sample blocks of g are the blocks of v
        // starting at 256n and 256n + 192, 0 <= n < 5, each contiguous in the ring buffer. The
        // corresponding blocks of c are consecutive.
        let v: &[f32; SYNTHESIS_LEN] = &self.v;
        let blocks: [&[f32]; 10] = std::array::from_fn(|i| {
            let start = (self.pos + 256 * (i / 2) + 192 * (i % 2)) % SYNTHESIS_LEN;
            v.get(start..start + QMF_BANDS).unwrap_or(&[0.0; QMF_BANDS])
        });

        // The output is accumulated in groups of 16 samples, summing the windowed blocks in order.
        const GROUP: usize = 16;
        let mut samples = [0.0f32; QMF_BANDS];
        for (g, samples) in samples.chunks_exact_mut(GROUP).enumerate() {
            let range = g * GROUP..(g + 1) * GROUP;
            let mut acc = [0.0f32; GROUP];
            for (v, c) in blocks.iter().zip(QMF_WINDOW.chunks_exact(QMF_BANDS)) {
                for ((acc, &v), &c) in acc.iter_mut().zip(&v[range.clone()]).zip(&c[range.clone()])
                {
                    *acc += v * c;
                }
            }
            samples.copy_from_slice(&acc);
        }

        match out.first_chunk_mut::<QMF_BANDS>() {
            Some(out) => *out = samples,
            None => out.copy_from_slice(&samples[..out.len()]),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use symphonia_core::dsp::complex::Complex;
    use symphonia_core::dsp::fft::Fft;

    use super::{QMF_WINDOW, QmfAnalysis, QmfSynthesis};
    use crate::aac::sbr::types::{QMF_ANALYSIS_BANDS, QMF_BANDS};

    /// A simple linear congruential generator producing values in [-1, 1).
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
        }
    }

    /// Direct evaluation of the analysis filterbank flowchart (Figure 4.42).
    struct DirectAnalysis {
        x: Vec<f64>,
    }

    impl DirectAnalysis {
        fn new() -> Self {
            DirectAnalysis { x: vec![0.0; 320] }
        }

        fn analyze(&mut self, input: &[f64]) -> Vec<(f64, f64)> {
            for n in (32..320).rev() {
                self.x[n] = self.x[n - 32];
            }
            for n in (0..32).rev() {
                self.x[n] = input[31 - n];
            }
            let z: Vec<f64> = (0..320).map(|n| self.x[n] * f64::from(QMF_WINDOW[2 * n])).collect();
            let u: Vec<f64> = (0..64).map(|n| (0..5).map(|j| z[n + 64 * j]).sum::<f64>()).collect();
            (0..32)
                .map(|k| {
                    let mut re = 0.0;
                    let mut im = 0.0;
                    for (n, u) in u.iter().enumerate() {
                        let phi = PI / 64.0 * (k as f64 + 0.5) * (2.0 * n as f64 - 0.5);
                        re += u * 2.0 * phi.cos();
                        im += u * 2.0 * phi.sin();
                    }
                    (re, im)
                })
                .collect()
        }
    }

    /// Direct evaluation of the synthesis filterbank flowchart (Figure 4.43).
    struct DirectSynthesis {
        v: Vec<f64>,
    }

    impl DirectSynthesis {
        fn new() -> Self {
            DirectSynthesis { v: vec![0.0; 1280] }
        }

        fn synthesize(&mut self, x: &[(f64, f64)]) -> Vec<f64> {
            for n in (128..1280).rev() {
                self.v[n] = self.v[n - 128];
            }
            for n in 0..128 {
                let mut acc = 0.0;
                for (k, &(re, im)) in x.iter().enumerate() {
                    let phi = PI / 128.0 * (k as f64 + 0.5) * (2.0 * n as f64 - 255.0);
                    // Re{(re + i im) / 64 * (cos + i sin)}
                    acc += (re * phi.cos() - im * phi.sin()) / 64.0;
                }
                self.v[n] = acc;
            }
            let mut g = vec![0.0; 640];
            for n in 0..5 {
                for k in 0..64 {
                    g[128 * n + k] = self.v[256 * n + k];
                    g[128 * n + 64 + k] = self.v[256 * n + 192 + k];
                }
            }
            let w: Vec<f64> = (0..640).map(|n| g[n] * f64::from(QMF_WINDOW[n])).collect();
            (0..64).map(|k| (0..10).map(|n| w[64 * n + k]).sum()).collect()
        }
    }

    #[test]
    fn verify_window_table() {
        // Spot values of Table 4.A.89.
        assert_eq!(QMF_WINDOW[0], 0.0);
        assert_eq!(QMF_WINDOW[1], -0.0005525286);
        assert_eq!(QMF_WINDOW[128], 0.013271822);
        assert_eq!(QMF_WINDOW[256], 0.3611589903);
        assert_eq!(QMF_WINDOW[319], 0.8535720573);
        assert_eq!(QMF_WINDOW[320], 0.8537385600);
        assert_eq!(QMF_WINDOW[384], -0.3611589903);
        assert_eq!(QMF_WINDOW[512], -0.0132718220);
        assert_eq!(QMF_WINDOW[513], -0.0121849995);
        assert_eq!(QMF_WINDOW[639], -0.000552528);

        // The window is symmetric, |c(640 - i)| = |c(i)|, where the sign differs only for the pairs
        // (128, 512) and (256, 384) due to the sign alternation of 128-sample blocks.
        for i in 1..640 {
            let a = QMF_WINDOW[i];
            let b = QMF_WINDOW[640 - i];
            let expected = if i == 128 || i == 256 || i == 384 || i == 512 { -a } else { a };
            assert!((b - expected).abs() <= 1e-9, "c[{}] = {}, c[{}] = {}", i, a, 640 - i, b);
        }
    }

    #[test]
    fn verify_analysis_against_direct() {
        let mut lcg = Lcg(1);
        let mut fast = QmfAnalysis::new();
        let mut direct = DirectAnalysis::new();

        // Enough subsamples to compact the input buffer of the analysis bank several times.
        for l in 0..100 {
            let input: Vec<f64> = (0..32).map(|_| lcg.next() * 1000.0).collect();
            let input_f32: Vec<f32> = input.iter().map(|&x| x as f32).collect();
            // Use exactly representable inputs for both implementations.
            let input: Vec<f64> = input_f32.iter().map(|&x| f64::from(x)).collect();

            let mut out = [Complex::new(0.0, 0.0); QMF_ANALYSIS_BANDS];
            fast.analyze(&input_f32, &mut out);
            let expected = direct.analyze(&input);

            let max = expected.iter().map(|&(re, im)| re.hypot(im)).fold(0.0, f64::max);
            for (k, (w, &(re, im))) in out.iter().zip(expected.iter()).enumerate() {
                let err = (f64::from(w.re) - re).hypot(f64::from(w.im) - im);
                assert!(err <= 1e-5 * max, "l={} k={} err={} max={}", l, k, err, max);
            }
        }
    }

    /// The analysis filterbank with the sample array x shifted for every subsample, and the
    /// matrix operation evaluated with the same factorization as `QmfAnalysis`.
    struct ShiftingAnalysis {
        x: [f32; 320],
        fft: Fft,
    }

    impl ShiftingAnalysis {
        fn analyze(&mut self, input: &[f32], out: &mut [Complex<f32>; QMF_ANALYSIS_BANDS]) {
            self.x.copy_within(0..288, 32);
            for (n, x) in self.x[..32].iter_mut().rev().enumerate() {
                *x = input[n];
            }
            let mut buf = [Complex::new(0.0, 0.0); 64];
            for (n, b) in buf.iter_mut().enumerate() {
                let mut u = 0.0;
                for j in 0..5 {
                    let i = n + 64 * j;
                    u += self.x[i] * QMF_WINDOW[2 * i];
                }
                let phi = -PI * (2.0 * n as f64 - 0.5) / 128.0;
                *b = Complex::new(phi.cos() as f32, phi.sin() as f32) * u;
            }
            self.fft.fft_inplace(&mut buf);
            for (k, (w, b)) in out.iter_mut().zip(buf.iter()).enumerate() {
                let phi = PI * k as f64 / 128.0;
                let post = Complex::new(2.0 * phi.cos() as f32, 2.0 * phi.sin() as f32);
                *w = (post * *b).conj();
            }
        }
    }

    #[test]
    fn verify_analysis_is_bit_exact() {
        // The analysis output does not depend on how the input buffer is managed: it is
        // identical to that of a filterbank that shifts its sample array for every subsample.
        // The covariance method of the HF generator (4.6.18.6.2) is very sensitive to small
        // changes of tonal signals, so the analysis is kept bit-exact.
        let mut lcg = Lcg(4);
        let mut fast = QmfAnalysis::new();
        let mut shifting = ShiftingAnalysis { x: [0.0; 320], fft: Fft::new(64) };

        for _ in 0..100 {
            let input: Vec<f32> = (0..32).map(|_| (lcg.next() * 30000.0) as f32).collect();
            let mut a = [Complex::new(0.0, 0.0); QMF_ANALYSIS_BANDS];
            let mut b = [Complex::new(0.0, 0.0); QMF_ANALYSIS_BANDS];
            fast.analyze(&input, &mut a);
            shifting.analyze(&input, &mut b);
            assert_eq!(a, b);
        }
    }

    #[test]
    fn verify_synthesis_against_direct() {
        let mut lcg = Lcg(2);
        let mut fast = QmfSynthesis::new();
        let mut direct = DirectSynthesis::new();

        // Enough subsamples to wrap around the ring buffer of the synthesis bank several times.
        for l in 0..100 {
            let input: Vec<Complex<f32>> = (0..QMF_BANDS)
                .map(|_| Complex::new((lcg.next() * 100.0) as f32, (lcg.next() * 100.0) as f32))
                .collect();
            let input_f64: Vec<(f64, f64)> =
                input.iter().map(|x| (f64::from(x.re), f64::from(x.im))).collect();
            let mut input_arr = [Complex::new(0.0, 0.0); QMF_BANDS];
            input_arr.copy_from_slice(&input);

            let mut out = [0.0f32; 64];
            fast.synthesize(&input_arr, &mut out);
            let expected = direct.synthesize(&input_f64);

            let max = expected.iter().map(|x| x.abs()).fold(0.0, f64::max);
            for (n, (&y, &e)) in out.iter().zip(expected.iter()).enumerate() {
                let err = (f64::from(y) - e).abs();
                assert!(err <= 1e-5 * max, "l={} n={} err={} max={}", l, n, err, max);
            }
        }
    }

    #[test]
    fn verify_reset() {
        let mut lcg = Lcg(3);
        let mut analysis = QmfAnalysis::new();
        let mut synthesis = QmfSynthesis::new();

        let input: Vec<f32> = (0..32).map(|_| lcg.next() as f32).collect();
        let mut first = [Complex::new(0.0, 0.0); QMF_ANALYSIS_BANDS];
        analysis.analyze(&input, &mut first);
        let mut tmp = [Complex::new(0.0, 0.0); QMF_ANALYSIS_BANDS];
        analysis.analyze(&input, &mut tmp);
        analysis.reset();
        let mut again = [Complex::new(0.0, 0.0); QMF_ANALYSIS_BANDS];
        analysis.analyze(&input, &mut again);
        assert_eq!(first, again);

        let mut x = [Complex::new(0.0, 0.0); QMF_BANDS];
        x[3] = Complex::new(1.0, -0.5);
        let mut first = [0.0f32; 64];
        synthesis.synthesize(&x, &mut first);
        let mut tmp = [0.0f32; 64];
        synthesis.synthesize(&x, &mut tmp);
        synthesis.reset();
        let mut again = [0.0f32; 64];
        synthesis.synthesize(&x, &mut again);
        assert_eq!(first, again);
    }

    #[test]
    fn verify_short_buffers() {
        let mut analysis = QmfAnalysis::new();
        let mut synthesis = QmfSynthesis::new();
        let mut w = [Complex::new(0.0, 0.0); QMF_ANALYSIS_BANDS];
        analysis.analyze(&[1.0; 5], &mut w);
        analysis.analyze(&[], &mut w);
        let x = [Complex::new(1.0, 1.0); QMF_BANDS];
        let mut out = [0.0f32; 7];
        synthesis.synthesize(&x, &mut out);
        synthesis.synthesize(&x, &mut []);
    }

    /// Run `input` through the 32-band analysis and the 64-band synthesis, with all subbands above
    /// 32 set to zero.
    fn round_trip(input: &[f32]) -> Vec<f32> {
        let mut analysis = QmfAnalysis::new();
        let mut synthesis = QmfSynthesis::new();
        let num_slots = input.len() / 32;
        let mut output = vec![0.0f32; 64 * num_slots];
        for l in 0..num_slots {
            let mut w = [Complex::new(0.0, 0.0); QMF_ANALYSIS_BANDS];
            analysis.analyze(&input[32 * l..32 * (l + 1)], &mut w);
            let mut x = [Complex::new(0.0, 0.0); QMF_BANDS];
            x[..QMF_ANALYSIS_BANDS].copy_from_slice(&w);
            synthesis.synthesize(&x, &mut output[64 * l..64 * (l + 1)]);
        }
        output
    }

    /// The delay, in output samples, of the analysis-synthesis chain: the position of the peak of
    /// the impulse response.
    pub fn round_trip_delay() -> usize {
        let mut input = vec![0.0f32; 32 * 40];
        input[0] = 1.0;
        let output = round_trip(&input);
        let mut peak = 0;
        for (n, y) in output.iter().enumerate() {
            if y.abs() > output[peak].abs() {
                peak = n;
            }
        }
        peak
    }

    /// Analysis with the 32-band bank followed by synthesis with the 64-band bank (all subbands
    /// above 32 set to zero) reproduces a low frequency sine at twice the sampling rate.
    #[test]
    fn verify_analysis_synthesis_round_trip() {
        let delay = round_trip_delay();
        // The impulse response of the analysis-synthesis chain peaks 578 output samples after the
        // input impulse.
        assert_eq!(delay, 578);

        for &freq in &[0.01, 0.03, 0.1, 0.2] {
            // A sine at `freq` cycles per input sample, freq / 2 cycles per output sample.
            let input: Vec<f32> =
                (0..32 * 200).map(|n| (2.0 * PI * freq * n as f64).sin() as f32).collect();
            let output = round_trip(&input);

            let mut num = 0.0;
            let mut den = 0.0;
            let mut err = 0.0f64;
            for (n, &y) in output.iter().enumerate().skip(2000) {
                let r = (2.0 * PI * freq / 2.0 * (n as f64 - delay as f64)).sin();
                num += f64::from(y) * r;
                den += r * r;
                err = err.max((f64::from(y) - r).abs());
            }
            let gain = num / den;
            assert!(err < 2e-3, "freq={} err={} gain={}", freq, err, gain);
            assert!((gain - 1.0).abs() < 2e-3, "freq={} err={} gain={}", freq, err, gain);
        }
    }
}
