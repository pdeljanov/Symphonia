// Symphonia
// Copyright (c) 2019-2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Parametric stereo (PS) tool (ISO/IEC 14496-3:2009, subclause 8.6.4 and Annex 8.A).
//!
//! The PS tool reconstructs a stereo signal from the monaural 64-band QMF representation produced
//! by the SBR tool and the stereo parameters carried in ps_data(). The processing chain of one
//! stereo frame (Figure 8.19 without the QMF banks, Annex 8.A.3) is:
//!
//! 1. hybrid analysis of the lowest QMF subbands (8.6.4.3), using the look-ahead of X_input so that
//!    no delay is introduced,
//! 2. de-correlation by all-pass filters and delays with transient reduction (8.6.4.5),
//! 3. stereo processing with mixing matrices derived from the IID, ICC, IPD, and OPD parameters and
//!    interpolated between the parameter positions (8.6.4.6),
//! 4. hybrid synthesis (8.6.4.7), producing the left and right 64-band QMF representations.
//!
//! After the hybrid analysis of the split QMF subbands, steps 2 to 4 are performed one QMF slot at
//! a time for all hybrid bands.
//!
//! Frames without ps_data() are output as mono (Annex 8.A.1), as are all frames until the stereo
//! parameters can be decoded (8.6.5.1).

mod decorrelation;
mod huffman;
mod hybrid;
#[cfg(test)]
mod reference;
mod stereo;
mod syntax;
mod tables;

use symphonia_core::dsp::complex::Complex;
use symphonia_core::errors::Result;
use symphonia_core::io::ReadBitsLtr;

use crate::aac::sbr::types::{QmfFrame, QmfSlots};

use decorrelation::{Decorrelator, TransientGains};
use hybrid::HybridAnalysis;
use stereo::{MixCoeffs, StereoState};
use syntax::{PsFrame, PsSyntaxState};
use tables::{MAX_NR_BANDS, MAX_NR_PAR_BANDS, MAX_NUM_SUB_QMF_BANDS, NUM_QMF_SLOTS};

/// Work buffers of the signal processing, allocated once.
struct WorkBuffers {
    /// The hybrid sub subband samples s_k(n) of the split QMF subbands, indexed `[n][k]`.
    sub: [[Complex<f32>; MAX_NUM_SUB_QMF_BANDS]; NUM_QMF_SLOTS],
    /// The all-pass filtered or delayed signal of each hybrid band of the current QMF slot, before
    /// transient reduction.
    d: [Complex<f32>; MAX_NR_BANDS],
    /// The transient attenuator G_TransientRatio(i, n) of the frame, indexed `[i][n]`.
    gain: TransientGains,
    /// The mixing coefficients of each stereo band of the current QMF slot.
    h: [MixCoeffs; MAX_NR_PAR_BANDS],
}

/// The parametric stereo decoder of one single channel element.
pub struct PsDecoder {
    /// The persistent state of ps_data() decoding.
    syntax: PsSyntaxState,
    /// The most recently decoded ps_data() element.
    frame: PsFrame,
    /// True if `frame` was decoded since the last call to `process`.
    frame_valid: bool,
    /// The hybrid analysis filterbank.
    hybrid: HybridAnalysis,
    /// The de-correlator.
    decorrelator: Decorrelator,
    /// The stereo processing state.
    stereo: StereoState,
    /// True if the previous frame was processed by the PS tool (rather than output as mono).
    prev_frame_processed: bool,
    work: Box<WorkBuffers>,
}

impl Default for PsDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl PsDecoder {
    pub fn new() -> Self {
        PsDecoder {
            syntax: PsSyntaxState::DEFAULT,
            frame: PsFrame::DEFAULT,
            frame_valid: false,
            hybrid: HybridAnalysis::new(),
            decorrelator: Decorrelator::new(),
            stereo: StereoState::new(),
            prev_frame_processed: false,
            work: Box::new(WorkBuffers {
                sub: [[Complex::new(0.0, 0.0); MAX_NUM_SUB_QMF_BANDS]; NUM_QMF_SLOTS],
                d: [Complex::new(0.0, 0.0); MAX_NR_BANDS],
                gain: [[0.0; NUM_QMF_SLOTS]; MAX_NR_PAR_BANDS],
                h: [[Complex::new(0.0, 0.0); 4]; MAX_NR_PAR_BANDS],
            }),
        }
    }

    /// Discard all state that depends on previous frames, e.g., after a seek.
    ///
    /// Unlike `reset`, the most recently received PS header is kept, since an encoder may transmit
    /// it only at the start of a stream. Stereo output resumes with the first ps_data() element
    /// whose enabled parameters are frequency differentially coded in the first parameter position.
    pub fn flush(&mut self) {
        if !self.syntax.header_received {
            self.reset();
            return;
        }
        self.syntax.flush();
        self.frame = PsFrame::DEFAULT;
        self.frame_valid = false;
        self.hybrid.reset();
        self.decorrelator.reset();
        self.stereo.reset();
        self.prev_frame_processed = false;
    }

    /// Reset all state, as if no PS data was ever decoded or processed.
    pub fn reset(&mut self) {
        self.syntax = PsSyntaxState::DEFAULT;
        self.frame = PsFrame::DEFAULT;
        self.frame_valid = false;
        self.hybrid.reset();
        self.decorrelator.reset();
        self.stereo.reset();
        self.prev_frame_processed = false;
    }

    /// Read ps_data() (Table 8.9) from `bs`, reading at most `num_bits_left` bits, and return the
    /// number of bits read (Table 8.A.1).
    ///
    /// The decoded parameters are used by the next call to `process`. Parameters that ps_data()
    /// does not transmit are kept from previous ps_data() elements as specified in 8.5.2 and
    /// 8.6.4.6.5. On error, the stereo parameters of the frame are discarded, and the output is
    /// mono until the parameters can be decoded correctly again (8.6.5.1).
    pub fn read_ps_data<B: ReadBitsLtr>(
        &mut self,
        bs: &mut B,
        num_bits_left: usize,
    ) -> Result<usize> {
        match syntax::read_ps_data(&mut self.syntax, bs, num_bits_left, &mut self.frame) {
            Ok(num_bits) => {
                self.frame_valid = true;
                Ok(num_bits)
            }
            Err(err) => {
                self.frame_valid = false;
                Err(err)
            }
        }
    }

    /// Process one stereo frame (Annex 8.A.3).
    ///
    /// `input` is the matrix X_input. `ps_data_present` is true if ps_data() was read for this
    /// frame. `kx_plus_m` is k_x + M of the current SBR frequency band tables. The left and right
    /// 64-band QMF matrices are written to `left` and `right`.
    ///
    /// If ps_data() was not read successfully for this frame, or the stereo parameters cannot be
    /// decoded correctly yet (8.6.5.1), both outputs are the monaural input (Annex 8.A.1).
    pub fn process(
        &mut self,
        input: &QmfFrame,
        ps_data_present: bool,
        kx_plus_m: usize,
        left: &mut QmfSlots,
        right: &mut QmfSlots,
    ) {
        let frame_valid = std::mem::replace(&mut self.frame_valid, false);

        if !(ps_data_present && frame_valid && self.syntax.synced) {
            self.process_mono(input, left, right);
            return;
        }

        // The stereo band configuration (Table 8.44). If neither IID nor ICC parameters are
        // enabled, the configuration of the previous frame is kept.
        let config = self.frame.header.stereo_bands().unwrap_or(self.stereo.config());

        // A change of the configuration resets the de-correlator (Table 8.47), as does a previous
        // frame without stereo processing (Annex 8.A.3).
        if config != self.stereo.config() || !self.prev_frame_processed {
            self.decorrelator.reset();
        }
        self.stereo.set_config(config);

        // Partial reset of the de-correlator above the highest QMF subband generated by the SBR
        // tool (Annex 8.A.3).
        let k_max = kx_plus_m.saturating_add(config.qmf_to_hybrid_offset());
        self.decorrelator.reset_bands(k_max);

        let work = &mut *self.work;

        // Hybrid analysis (8.6.4.3) of the split QMF subbands.
        self.hybrid.analyze(config, input, &mut work.sub);
        self.hybrid.update_history(input);

        // Transient detection (8.6.4.5.3).
        self.decorrelator.transient_gains(config, &work.sub, input, &mut work.gain);

        // The mixing coefficients at the parameter positions (8.6.4.6).
        self.stereo.prepare(&self.frame);
        let real = self.stereo.is_real();

        let slots = work.sub.iter().zip(input.iter()).zip(left.iter_mut().zip(right.iter_mut()));

        for (n, ((sub, x), (l, r))) in slots.enumerate() {
            // De-correlation (8.6.4.5.2).
            let s = self.decorrelator.process_slot(config, sub, x, &mut work.d);

            // Transient reduction (8.6.4.5.4), mixing with interpolated mixing matrices (8.6.4.6.2
            // to 8.6.4.6.4), and hybrid synthesis (8.6.4.7).
            self.stereo.slot_coeffs(n, &mut work.h);
            let mix = stereo::SlotMix { config, h: &work.h, s, d: &work.d, gain: &work.gain, n };
            if real {
                mix.mix_real(l, r);
            }
            else {
                mix.mix(l, r);
            }
        }

        self.stereo.finish();
        self.prev_frame_processed = true;
    }

    /// Output the monaural input in both channels.
    fn process_mono(&mut self, input: &QmfFrame, left: &mut QmfSlots, right: &mut QmfSlots) {
        for ((l, r), x) in left.iter_mut().zip(right.iter_mut()).zip(input.iter()) {
            *l = *x;
            *r = *x;
        }
        self.hybrid.update_history(input);
        self.stereo.set_mono();
        self.prev_frame_processed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::syntax::test_util::BitWriter;
    use super::*;
    use crate::aac::sbr::types::{PS_LOOKAHEAD, PS_LOOKAHEAD_BANDS, QMF_BANDS};
    use symphonia_core::io::{BitReaderLtr, FiniteBitStream};

    /// A simple linear congruential generator.
    struct Lcg(u64);

    impl Lcg {
        fn next_u32(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 32) as u32
        }

        fn next_f32(&mut self) -> f32 {
            (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
        }
    }

    static ZERO_FRAME: QmfFrame =
        [[Complex::new(0.0, 0.0); QMF_BANDS]; NUM_QMF_SLOTS + PS_LOOKAHEAD];
    static ZERO_SLOTS: QmfSlots = [[Complex::new(0.0, 0.0); QMF_BANDS]; NUM_QMF_SLOTS];

    /// Generate frames of a continuous random QMF signal that is zero in the QMF subbands
    /// `num_bands` and above, with the look-ahead of Annex 8.A.3.
    fn random_frames(rng: &mut Lcg, num_frames: usize, num_bands: usize) -> Vec<QmfFrame> {
        let total = num_frames * NUM_QMF_SLOTS + PS_LOOKAHEAD;
        let signal: Vec<[Complex<f32>; QMF_BANDS]> = (0..total)
            .map(|_| {
                std::array::from_fn(|k| {
                    if k < num_bands {
                        Complex::new(rng.next_f32(), rng.next_f32())
                    }
                    else {
                        Complex::new(0.0, 0.0)
                    }
                })
            })
            .collect();
        (0..num_frames)
            .map(|f| {
                let mut frame = ZERO_FRAME;
                for (l, row) in frame.iter_mut().enumerate() {
                    for (k, x) in row.iter_mut().enumerate() {
                        if l < NUM_QMF_SLOTS || k < PS_LOOKAHEAD_BANDS {
                            *x = signal[f * NUM_QMF_SLOTS + l][k];
                        }
                    }
                }
                frame
            })
            .collect()
    }

    /// Feed a test bitstream to `read_ps_data` with a budget of exactly its length.
    fn read(dec: &mut PsDecoder, w: &BitWriter) -> Result<usize> {
        let bytes = w.bytes();
        let mut bs = BitReaderLtr::new(&bytes);
        dec.read_ps_data(&mut bs, w.len())
    }

    /// ps_data() with a header, FIX_BORDERS, one envelope, and frequency differentially coded IID
    /// and ICC parameters that are equal in all bands. With `ipdopd`, the extension carries IPD and
    /// OPD parameters with the given values.
    fn ps_data(
        iid_mode: u32,
        iid: i32,
        icc_mode: u32,
        icc: i32,
        ipdopd: Option<(i32, i32)>,
    ) -> BitWriter {
        let nr_iid = [10, 20, 34, 10, 20, 34][iid_mode as usize];
        let nr_icc = [10, 20, 34, 10, 20, 34][icc_mode as usize];
        let fine = iid_mode >= 3;
        let mut w = BitWriter::new();
        w.bit(true).bit(true).put(iid_mode, 3).bit(true).put(icc_mode, 3).bit(ipdopd.is_some());
        w.bit(false).put(1, 2);
        w.bit(false).iid_df(fine, iid);
        for _ in 1..nr_iid {
            w.iid_df(fine, 0);
        }
        w.bit(false).icc_df(icc);
        for _ in 1..nr_icc {
            w.icc_df(0);
        }
        if let Some((ipd, opd)) = ipdopd {
            let nr = [5, 11, 17, 5, 11, 17][iid_mode as usize];
            let mut ext = BitWriter::new();
            ext.put(0, 2).bit(true);
            ext.bit(false).ipd_df(ipd);
            for _ in 1..nr {
                ext.ipd_df(0);
            }
            ext.bit(false).opd_df(opd);
            for _ in 1..nr {
                ext.opd_df(0);
            }
            ext.bit(false);
            let cnt = ext.len().div_ceil(8);
            assert!(cnt < 15);
            w.put(cnt as u32, 4);
            let bytes = ext.bytes();
            for i in 0..8 * cnt {
                w.bit(bytes[i / 8] & (0x80 >> (i % 8)) != 0);
            }
        }
        w
    }

    /// ps_data() without a header, FIX_BORDERS, and one envelope, for a previous header with the
    /// given IID and ICC modes and no extension. With `dt`, the IID and ICC parameters are time
    /// differentially coded without change. Otherwise, they are frequency differentially coded and
    /// equal in all bands.
    fn ps_data_without_header(
        iid_mode: u32,
        iid: i32,
        icc_mode: u32,
        icc: i32,
        dt: bool,
    ) -> BitWriter {
        let nr_iid = [10, 20, 34, 10, 20, 34][iid_mode as usize];
        let nr_icc = [10, 20, 34, 10, 20, 34][icc_mode as usize];
        let fine = iid_mode >= 3;
        let mut w = BitWriter::new();
        w.bit(false);
        w.bit(false).put(1, 2);
        w.bit(dt);
        for b in 0..nr_iid {
            match (dt, b) {
                (true, _) => w.iid_dt(fine, 0),
                (false, 0) => w.iid_df(fine, iid),
                (false, _) => w.iid_df(fine, 0),
            };
        }
        w.bit(dt);
        for b in 0..nr_icc {
            match (dt, b) {
                (true, _) => w.icc_dt(0),
                (false, 0) => w.icc_df(icc),
                (false, _) => w.icc_df(0),
            };
        }
        w
    }

    /// ps_data() without a header and with num_env = 0.
    fn ps_data_hold() -> BitWriter {
        let mut w = BitWriter::new();
        w.bit(false).bit(false).put(0, 2);
        w
    }

    fn energy(slots: &[[Complex<f32>; QMF_BANDS]]) -> f64 {
        slots.iter().flatten().map(|x| f64::from(x.norm_sqr())).sum()
    }

    /// Returns true if every sample of the slot `a` equals `scale` times that of `b`.
    fn is_scaled(a: &[Complex<f32>; QMF_BANDS], b: &[Complex<f32>; QMF_BANDS], scale: f32) -> bool {
        a.iter()
            .zip(b)
            .all(|(a, b)| (a.re - scale * b.re).abs() < 1e-5 && (a.im - scale * b.im).abs() < 1e-5)
    }

    fn max_diff(a: &[[Complex<f32>; QMF_BANDS]], b: &[[Complex<f32>; QMF_BANDS]]) -> f32 {
        a.iter()
            .zip(b)
            .flat_map(|(x, y)| x.iter().zip(y).map(|(x, y)| (x - y).norm()))
            .fold(0.0, f32::max)
    }

    #[test]
    fn verify_default_parameters_output_mono() {
        let cases =
            [(0, 0, None), (1, 3, None), (2, 2, None), (4, 5, Some((0, 0))), (5, 1, Some((0, 0)))];
        let mut rng = Lcg(3);
        for (iid_mode, icc_mode, ipdopd) in cases {
            let frames = random_frames(&mut rng, 5, 40);
            let mut dec = PsDecoder::new();
            let mut left = ZERO_SLOTS;
            let mut right = ZERO_SLOTS;

            // A frame without ps_data() is output as mono.
            dec.process(&frames[0], false, 40, &mut left, &mut right);
            assert_eq!(max_diff(&left, &frames[0][..NUM_QMF_SLOTS]), 0.0);
            assert_eq!(max_diff(&right, &frames[0][..NUM_QMF_SLOTS]), 0.0);

            // IID = 0 dB, ICC = 1, and IPD = OPD = 0: both channels equal the mono input. The
            // first stereo frame interpolates from H(k, n_-1) = 0 (8.6.4.6.4) at the last slot of
            // the previous frame (8.6.4.4), even after mono frames, to the parameters at its
            // parameter position n_0 = 31.
            for (f, frame) in frames.iter().enumerate().skip(1) {
                let w = ps_data(iid_mode, 0, icc_mode, 0, ipdopd);
                assert_eq!(read(&mut dec, &w).ok(), Some(w.len()));
                dec.process(frame, true, 40, &mut left, &mut right);
                let first = if f == 1 { NUM_QMF_SLOTS - 1 } else { 0 };
                let err_l = max_diff(&left[first..], &frame[first..NUM_QMF_SLOTS]);
                let err_r = max_diff(&right[first..], &frame[first..NUM_QMF_SLOTS]);
                assert!(
                    err_l < 1e-5 && err_r < 1e-5,
                    "mode {iid_mode}/{icc_mode}: {err_l} {err_r}"
                );
                if f == 1 {
                    assert!(is_scaled(&left[0], &frame[0], 1.0 / 32.0));
                    assert!(is_scaled(&right[0], &frame[0], 1.0 / 32.0));
                }
            }
        }
    }

    #[test]
    fn verify_first_stereo_frame_fades_in() {
        // H(k, n_-1) is zero for the first stereo frame (8.6.4.6.4), and applies to the last slot
        // of the previous frame (8.6.4.4).
        let mut rng = Lcg(5);
        let frames = random_frames(&mut rng, 1, 64);
        let mut dec = PsDecoder::new();
        let mut left = ZERO_SLOTS;
        let mut right = ZERO_SLOTS;
        let w = ps_data(0, 0, 0, 0, None);
        read(&mut dec, &w).expect("valid ps_data");
        dec.process(&frames[0], true, 64, &mut left, &mut right);
        assert!(is_scaled(&left[0], &frames[0][0], 1.0 / 32.0));
        assert!(max_diff(&left[31..], &frames[0][31..32]) < 1e-5);
    }

    #[test]
    fn verify_large_iid_pans() {
        let mut rng = Lcg(11);
        for (iid, icc_mode) in [(15, 0), (-15, 0), (15, 3), (-15, 3)] {
            let frames = random_frames(&mut rng, 4, 48);
            let mut dec = PsDecoder::new();
            let mut left = ZERO_SLOTS;
            let mut right = ZERO_SLOTS;
            dec.process(&frames[0], false, 48, &mut left, &mut right);

            for (f, frame) in frames.iter().enumerate().skip(1) {
                // IID = +-50 dB (fine grid), ICC = 1. The last frame holds the parameters.
                let w = if f < 3 { ps_data(3, iid, icc_mode, 0, None) } else { ps_data_hold() };
                read(&mut dec, &w).expect("valid ps_data");
                dec.process(frame, true, 48, &mut left, &mut right);
                if f >= 2 {
                    // The parameters are fully applied from the second stereo frame on.
                    let (el, er) = (energy(&left), energy(&right));
                    let ratio_db = 10.0 * (el / er).log10();
                    assert!((ratio_db - f64::from(iid.signum() * 50)).abs() < 0.1, "{ratio_db}");
                    // The total energy is preserved: |h11|^2 + |h12|^2 = 2 with ICC = 1.
                    let em = energy(&frame[..NUM_QMF_SLOTS]);
                    assert!(((el + er) / em - 2.0).abs() < 1e-3);
                }
            }
        }
    }

    #[test]
    fn verify_34_band_configuration() {
        let mut rng = Lcg(13);
        let frames = random_frames(&mut rng, 6, 50);
        let mut dec = PsDecoder::new();
        let mut left = ZERO_SLOTS;
        let mut right = ZERO_SLOTS;
        for (f, frame) in frames.iter().enumerate() {
            // Alternate between the 34 and the 20 stereo band configurations, with decorrelation
            // (ICC < 1) and phase parameters.
            let w = match f % 3 {
                0 => ps_data(2, 3, 2, 4, Some((3, 6))),
                1 => ps_data(5, -9, 0, 6, Some((1, 2))),
                _ => ps_data(1, 2, 4, 2, None),
            };
            read(&mut dec, &w).expect("valid ps_data");
            dec.process(frame, true, 50, &mut left, &mut right);
            let expected = if f % 3 == 2 {
                tables::StereoBands::Bands20
            }
            else {
                tables::StereoBands::Bands34
            };
            assert_eq!(dec.stereo.config(), expected);
            assert!(
                left.iter()
                    .chain(right.iter())
                    .flatten()
                    .all(|x| x.re.is_finite() && x.im.is_finite())
            );
            assert!(energy(&left) > 0.0 && energy(&right) > 0.0);
            assert!(max_diff(&left, &right) > 0.1);
            // Nothing is generated above the SBR range.
            assert!(
                left.iter()
                    .chain(right.iter())
                    .all(|row| row[50..].iter().all(|x| x.norm() == 0.0))
            );
        }
    }

    #[test]
    fn verify_decorrelator_partial_reset() {
        let mut rng = Lcg(17);
        for kx_plus_m in [64, 20] {
            let mut frames = random_frames(&mut rng, 3, 64);
            // The last frame has no content above QMF subband 20.
            for row in frames[2].iter_mut() {
                row[20..].fill(Complex::new(0.0, 0.0));
            }
            let mut dec = PsDecoder::new();
            let mut left = ZERO_SLOTS;
            let mut right = ZERO_SLOTS;
            for (f, frame) in frames.iter().enumerate() {
                let w = ps_data(1, 0, 1, 5, None);
                read(&mut dec, &w).expect("valid ps_data");
                let k = if f == 2 { kx_plus_m } else { 64 };
                dec.process(frame, true, k, &mut left, &mut right);
            }
            // With k_x + M = 20, all hybrid bands from k_max = 27 on were reset and stay silent.
            // Otherwise, the all-pass filter tails of hybrid bands 27 to 29 remain.
            assert_eq!(dec.decorrelator.is_zero_from(27), kx_plus_m == 20);
            assert!(!dec.decorrelator.is_zero_from(20));
            if kx_plus_m == 20 {
                assert!(
                    left.iter()
                        .chain(right.iter())
                        .all(|row| row[20..].iter().all(|x| x.norm() == 0.0))
                );
            }
        }
    }

    #[test]
    fn verify_decorrelator_full_reset() {
        let mut rng = Lcg(19);
        for skip_ps_data in [false, true] {
            let mut frames = random_frames(&mut rng, 3, 64);
            // The last frame is silent, including the history of the hybrid filters.
            frames[2] = ZERO_FRAME;
            for row in frames[1][NUM_QMF_SLOTS - 6..].iter_mut() {
                row.fill(Complex::new(0.0, 0.0));
            }
            let mut dec = PsDecoder::new();
            let mut left = ZERO_SLOTS;
            let mut right = ZERO_SLOTS;
            for (f, frame) in frames.iter().enumerate() {
                let present = !(skip_ps_data && f == 1);
                if present {
                    let w = ps_data(1, 0, 1, 5, None);
                    read(&mut dec, &w).expect("valid ps_data");
                }
                dec.process(frame, present, 64, &mut left, &mut right);
                if f == 1 {
                    assert!(!dec.decorrelator.is_zero_from(0));
                }
            }
            // If no ps_data() was present in the previous frame, the de-correlator was reset, and
            // with a silent input it stays silent.
            assert_eq!(dec.decorrelator.is_zero_from(0), skip_ps_data);
            if skip_ps_data {
                assert_eq!(energy(&left) + energy(&right), 0.0);
            }
            else {
                assert!(energy(&left) + energy(&right) > 0.0);
            }
        }
    }

    #[test]
    fn verify_unsynchronized_output_is_mono() {
        let mut rng = Lcg(23);
        let frames = random_frames(&mut rng, 3, 64);
        let mut dec = PsDecoder::new();
        let mut left = ZERO_SLOTS;
        let mut right = ZERO_SLOTS;

        // Without a previous header, ps_data() does not allow correct decoding.
        let w = ps_data_hold();
        read(&mut dec, &w).expect("valid ps_data");
        dec.process(&frames[0], true, 64, &mut left, &mut right);
        assert_eq!(max_diff(&left, &frames[0][..NUM_QMF_SLOTS]), 0.0);
        assert_eq!(max_diff(&right, &frames[0][..NUM_QMF_SLOTS]), 0.0);

        // A read error makes the frame mono.
        let w = ps_data(3, 15, 0, 0, None);
        let bytes = w.bytes();
        let mut bs = BitReaderLtr::new(&bytes);
        assert!(dec.read_ps_data(&mut bs, w.len() - 1).is_err());
        let consumed = bytes.len() * 8 - bs.bits_left() as usize;
        assert!(consumed < w.len());
        dec.process(&frames[1], true, 64, &mut left, &mut right);
        assert_eq!(max_diff(&left, &frames[1][..NUM_QMF_SLOTS]), 0.0);

        // A complete element allows stereo processing. The mixing matrices are interpolated from
        // those of the mono output, so the parameters are fully applied at the end of the frame.
        read(&mut dec, &w).expect("valid ps_data");
        dec.process(&frames[2], true, 64, &mut left, &mut right);
        assert!(energy(&left[31..]) > 1e4 * energy(&right[31..]));

        // After a reset, the output is mono again.
        dec.reset();
        let w = ps_data_hold();
        read(&mut dec, &w).expect("valid ps_data");
        dec.process(&frames[2], true, 64, &mut left, &mut right);
        assert_eq!(max_diff(&left, &frames[2][..NUM_QMF_SLOTS]), 0.0);
    }

    /// Returns true if the output is the mono input.
    fn is_mono(left: &QmfSlots, right: &QmfSlots, frame: &QmfFrame) -> bool {
        max_diff(left, &frame[..NUM_QMF_SLOTS]) == 0.0
            && max_diff(right, &frame[..NUM_QMF_SLOTS]) == 0.0
    }

    #[test]
    fn verify_flush_resumes_with_frequency_differential_frame() {
        let mut rng = Lcg(31);
        let frames = random_frames(&mut rng, 6, 64);
        let mut dec = PsDecoder::new();
        let mut left = ZERO_SLOTS;
        let mut right = ZERO_SLOTS;

        // A header with IID mode 3 (fine grid) and ICC mode 0, IID = +50 dB, then a time
        // differentially coded frame without a header.
        let w = ps_data(3, 15, 0, 0, None);
        read(&mut dec, &w).expect("valid ps_data");
        dec.process(&frames[0], true, 64, &mut left, &mut right);
        let w = ps_data_without_header(3, 0, 0, 0, true);
        read(&mut dec, &w).expect("valid ps_data");
        dec.process(&frames[1], true, 64, &mut left, &mut right);
        assert!(energy(&left) > 1e4 * energy(&right));

        // Flush: the header configuration is kept, everything depending on previous frames is
        // discarded.
        dec.flush();
        assert!(dec.syntax.header_received);
        assert!(!dec.syntax.synced);
        assert_eq!(dec.syntax.header.iid_mode, 3);
        assert_eq!(dec.syntax.prev, syntax::ParamSet::DEFAULT);
        assert!(dec.decorrelator.is_zero_from(0));
        assert!(!dec.prev_frame_processed);

        // A time differentially coded frame cannot be decoded without the previous frame.
        let w = ps_data_without_header(3, 0, 0, 0, true);
        read(&mut dec, &w).expect("valid ps_data");
        dec.process(&frames[2], true, 64, &mut left, &mut right);
        assert!(is_mono(&left, &right, &frames[2]));

        // A frequency differentially coded frame without a header resumes stereo decoding. It is
        // the first stereo frame: the output is interpolated from H(k, n_-1) = 0 (8.6.4.6.4) at the
        // last slot of the previous frame and reaches the parameters at n_0 = 31.
        let w = ps_data_without_header(3, 15, 0, 0, false);
        read(&mut dec, &w).expect("valid ps_data");
        dec.process(&frames[3], true, 64, &mut left, &mut right);
        assert!(dec.syntax.synced);
        assert!(energy(&left[..1]) + energy(&right[..1]) < 1e-2 * energy(&frames[3][..1]));
        assert!(energy(&left[31..]) > 1e4 * energy(&right[31..]));

        // Time differential coding is possible again.
        let w = ps_data_without_header(3, 0, 0, 0, true);
        read(&mut dec, &w).expect("valid ps_data");
        dec.process(&frames[4], true, 64, &mut left, &mut right);
        assert!(energy(&left) > 1e4 * energy(&right));
    }

    #[test]
    fn verify_flush_time_differential_frames_stay_mono() {
        let mut rng = Lcg(37);
        let frames = random_frames(&mut rng, 5, 64);
        let mut dec = PsDecoder::new();
        let mut left = ZERO_SLOTS;
        let mut right = ZERO_SLOTS;

        let w = ps_data(1, 3, 4, 2, None);
        read(&mut dec, &w).expect("valid ps_data");
        dec.process(&frames[0], true, 64, &mut left, &mut right);
        assert!(!is_mono(&left, &right, &frames[0]));

        dec.flush();

        // Time differentially coded frames, and frames holding the parameters, stay mono.
        for frame in &frames[1..4] {
            let w = ps_data_without_header(1, 0, 4, 0, true);
            read(&mut dec, &w).expect("valid ps_data");
            dec.process(frame, true, 64, &mut left, &mut right);
            assert!(is_mono(&left, &right, frame));
        }
        let w = ps_data_hold();
        read(&mut dec, &w).expect("valid ps_data");
        dec.process(&frames[4], true, 64, &mut left, &mut right);
        assert!(is_mono(&left, &right, &frames[4]));
        assert!(!dec.syntax.synced);
    }

    #[test]
    fn verify_flush_without_header_is_reset() {
        let mut rng = Lcg(41);
        let frames = random_frames(&mut rng, 6, 64);
        let mut flushed = PsDecoder::new();
        let mut reset = PsDecoder::new();
        let mut left_a = ZERO_SLOTS;
        let mut right_a = ZERO_SLOTS;
        let mut left_b = ZERO_SLOTS;
        let mut right_b = ZERO_SLOTS;

        // Without any header, ps_data() does not allow stereo decoding (and only its first 4 bits
        // are read). The decoder to be flushed accumulates hybrid filter history.
        for frame in &frames[..2] {
            let w = ps_data_without_header(0, 0, 0, 0, false);
            read(&mut flushed, &w).expect("valid ps_data");
            flushed.process(frame, true, 64, &mut left_a, &mut right_a);
            assert!(is_mono(&left_a, &right_a, frame));
        }

        flushed.flush();
        reset.reset();
        assert!(!flushed.syntax.header_received);
        assert!(!flushed.syntax.synced);
        assert_eq!(flushed.syntax.header, reset.syntax.header);
        assert_eq!(flushed.syntax.prev, reset.syntax.prev);

        // Both decoders behave identically from now on: a frequency differentially coded frame
        // without a header is still mono, a frame with a header starts stereo decoding.
        let streams = [
            ps_data_without_header(0, 0, 0, 0, false),
            ps_data(2, 4, 1, 3, None),
            ps_data_without_header(2, 0, 1, 0, true),
            ps_data(4, -6, 3, 6, None),
        ];
        for (i, (w, frame)) in streams.iter().zip(&frames[2..]).enumerate() {
            read(&mut flushed, w).expect("valid ps_data");
            read(&mut reset, w).expect("valid ps_data");
            flushed.process(frame, true, 64, &mut left_a, &mut right_a);
            reset.process(frame, true, 64, &mut left_b, &mut right_b);
            assert_eq!(left_a, left_b);
            assert_eq!(right_a, right_b);
            assert_eq!(is_mono(&left_a, &right_a, frame), i == 0);
        }
    }

    #[test]
    fn verify_against_band_wise_reference() {
        // The slot-wise processing produces the same output as the band-wise reference
        // implementation for a sequence of frames with both stereo band configurations, phase
        // parameters, mono frames, partial and full resets of the de-correlator, held parameters,
        // and a flush.
        let mut rng = Lcg(47);
        let frames = random_frames(&mut rng, 16, 64);
        let mut dec = PsDecoder::new();
        let mut reference = reference::ReferencePs::new();
        let mut out = [ZERO_SLOTS; 4];
        let mut num_processed = 0;
        let mut num_valid = 0;

        for (f, frame) in frames.iter().enumerate() {
            let w = match f % 8 {
                0 => Some(ps_data(1, 3, 4, 2, None)),
                1 => Some(ps_data(5, -9, 2, 6, Some((1, 2)))),
                2 => Some(ps_data_hold()),
                3 => None,
                4 => Some(ps_data(2, 4, 2, 5, Some((3, 6)))),
                5 => Some(ps_data_without_header(2, 0, 2, 0, true)),
                6 => Some(ps_data(0, -2, 3, 7, None)),
                _ => Some(ps_data(4, 11, 1, 1, Some((7, 4)))),
            };
            let mut num_ok = 0;
            if let Some(w) = &w {
                let res = read(&mut dec, w).ok();
                assert_eq!(read(&mut reference.dec, w).ok(), res);
                if res == Some(w.len()) {
                    num_ok += 1;
                }
            }
            if f == 11 {
                dec.flush();
                reference.dec.flush();
            }

            let kx_plus_m = [64, 40, 23, 64, 0, 64, 50, 13][f % 8];
            let [left, right, ref_left, ref_right] = &mut out;
            dec.process(frame, w.is_some(), kx_plus_m, left, right);
            reference.process(frame, w.is_some(), kx_plus_m, ref_left, ref_right);

            assert_eq!(dec.prev_frame_processed, reference.dec.prev_frame_processed);
            assert_eq!(left, ref_left, "frame {f}");
            assert_eq!(right, ref_right, "frame {f}");
            num_processed += usize::from(dec.prev_frame_processed);
            num_valid += num_ok;
        }
        assert!(num_processed >= 8, "{num_processed}");
        assert!(num_valid >= 8, "{num_valid}");
    }

    #[test]
    fn verify_random_bitstreams_never_panic() {
        let mut rng = Lcg(29);
        let frames = random_frames(&mut rng, 2, 64);
        let mut dec = PsDecoder::new();
        let mut left = ZERO_SLOTS;
        let mut right = ZERO_SLOTS;
        let mut num_ok = 0;
        let mut num_processed = 0;
        for i in 0..20000 {
            let len = (rng.next_u32() % 64) as usize + 1;
            let mut bytes: Vec<u8> = (0..len).map(|_| rng.next_u32() as u8).collect();
            // Bias towards streams with a header to reach deeper into the syntax.
            if i % 2 == 0 {
                bytes[0] |= 0x80;
            }
            let budget = (rng.next_u32() as usize) % (len * 8 + 16);
            let mut bs = BitReaderLtr::new(&bytes);
            let res = dec.read_ps_data(&mut bs, budget);
            let consumed = bytes.len() * 8 - bs.bits_left() as usize;
            assert!(consumed <= budget);
            if let Ok(used) = res {
                assert_eq!(used, consumed);
                num_ok += 1;
            }
            if i % 20 == 0 {
                let present = rng.next_u32() % 4 != 0;
                let kx_plus_m = (rng.next_u32() % 80) as usize;
                let frame = &frames[i % 2];
                dec.process(frame, present, kx_plus_m, &mut left, &mut right);
                if dec.prev_frame_processed {
                    num_processed += 1;
                }
                assert!(
                    left.iter()
                        .chain(right.iter())
                        .flatten()
                        .all(|x| x.re.is_finite() && x.im.is_finite())
                );
            }
            if i % 997 == 0 {
                dec.reset();
            }
        }
        assert!(num_ok > 1000);
        assert!(num_processed > 50, "{num_processed}");

        // Extreme input values do not panic.
        let mut frame = ZERO_FRAME;
        for (l, row) in frame.iter_mut().enumerate() {
            for (k, x) in row.iter_mut().enumerate() {
                *x = match (l + k) % 4 {
                    0 => Complex::new(f32::MAX, f32::MIN),
                    1 => Complex::new(f32::NAN, 0.0),
                    2 => Complex::new(f32::INFINITY, f32::NEG_INFINITY),
                    _ => Complex::new(1e-38, -1e-38),
                };
            }
        }
        let w = ps_data(2, 5, 5, 7, Some((7, 7)));
        read(&mut dec, &w).expect("valid ps_data");
        dec.process(&frame, true, usize::MAX, &mut left, &mut right);
        read(&mut dec, &w).expect("valid ps_data");
        dec.process(&frame, true, 0, &mut left, &mut right);
    }
}
