// Symphonia
// Copyright (c) 2026 The Project Symphonia Developers.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The decoder follows the floating point build of the Xiph.Org Speex reference implementation, see
// NOTICE-xiph.txt.

use symphonia_core::errors::{Result, decode_error};

use crate::bits::Bits;
use crate::dsp::{iir_mem, lsp_interpolate, lsp_to_lpc, qmf_synth, rand, rms};
use crate::modes::{HB_SUBMODES, Innovation, Submode, UWB_SUBMODES, unquant_innovation};
use crate::nb::NbDecoder;
use crate::tables::QMF_H0;

const ORDER: usize = 8;
const LSP_MARGIN: f32 = 0.05;
const EPSILON: f32 = 1e-15;

const GC_QUANT_BOUND: [f32; 16] = [
    0.97979, 1.28384, 1.68223, 2.20426, 2.88829, 3.78458, 4.95900, 6.49787, 8.51428, 11.15642,
    14.61846, 19.15484, 25.09895, 32.88761, 43.09325, 56.46588,
];

/// A decoder of one band of the frame, or of the bands below it.
pub(crate) enum Layer {
    Narrow(Box<NbDecoder>),
    Split(Box<SbDecoder>),
}

impl Layer {
    /// The frame size of the layer.
    pub(crate) fn frame_size(&self) -> usize {
        match self {
            Layer::Narrow(_) => 160,
            Layer::Split(sb) => sb.full_frame_size,
        }
    }

    pub(crate) fn narrow(&mut self) -> &mut NbDecoder {
        match self {
            Layer::Narrow(nb) => nb,
            Layer::Split(sb) => sb.low.narrow(),
        }
    }

    pub(crate) fn decode(&mut self, bits: &mut Bits<'_>, out: &mut [f32]) -> Result<()> {
        match self {
            Layer::Narrow(nb) => nb.decode(bits, out),
            Layer::Split(sb) => sb.decode(bits, out),
        }
    }

    fn pi_gain(&self) -> &[f32] {
        match self {
            Layer::Narrow(nb) => &nb.pi_gain,
            Layer::Split(sb) => &sb.pi_gain,
        }
    }

    fn exc_rms(&self) -> &[f32] {
        match self {
            Layer::Narrow(nb) => &nb.exc_rms,
            Layer::Split(sb) => &sb.exc_rms,
        }
    }

    fn dtx(&mut self) -> bool {
        self.narrow().dtx
    }

    fn innov(&self) -> &[f32] {
        match self {
            Layer::Narrow(nb) => &nb.innov,
            Layer::Split(sb) => &sb.innov,
        }
    }
}

pub(crate) struct SbDecoder {
    low: Layer,
    frame_size: usize,
    full_frame_size: usize,
    subframe_size: usize,
    nb_subframes: usize,
    folding_gain: f32,
    submodes: &'static [Option<Submode>],
    old_qlsp: [f32; ORDER],
    interp_qlpc: [f32; ORDER],
    mem_sp: [f32; 2 * ORDER],
    exc_buf: Vec<f32>,
    g0_mem: [f32; 64],
    g1_mem: [f32; 64],
    pi_gain: Vec<f32>,
    exc_rms: Vec<f32>,
    innov: Vec<f32>,
    seed: u32,
    first: bool,
    last_ener: f32,
}

impl SbDecoder {
    /// Creates the decoder of the wideband (`ultra` is false) or the ultra wideband mode.
    pub(crate) fn new(ultra: bool) -> Self {
        let mut nb = NbDecoder::new();
        nb.wideband = true;

        let (low, frame_size, subframe_size, folding_gain, submodes): (
            Layer,
            usize,
            usize,
            f32,
            &'static [Option<Submode>],
        ) = if ultra {
            (Layer::Split(Box::new(SbDecoder::new(false))), 320, 80, 0.7, &UWB_SUBMODES)
        }
        else {
            (Layer::Narrow(Box::new(nb)), 160, 40, 0.9, &HB_SUBMODES)
        };

        let nb_subframes = frame_size / subframe_size;

        SbDecoder {
            low,
            frame_size,
            full_frame_size: 2 * frame_size,
            subframe_size,
            nb_subframes,
            folding_gain,
            submodes,
            old_qlsp: [0.0; ORDER],
            interp_qlpc: [0.0; ORDER],
            mem_sp: [0.0; 2 * ORDER],
            exc_buf: vec![0.0; subframe_size],
            g0_mem: [0.0; 64],
            g1_mem: [0.0; 64],
            pi_gain: vec![0.0; nb_subframes],
            exc_rms: vec![0.0; nb_subframes],
            innov: vec![0.0; 2 * frame_size],
            seed: 1000,
            first: true,
            last_ener: 0.0,
        }
    }

    /// Decodes a frame of `full_frame_size` samples.
    pub(crate) fn decode(&mut self, bits: &mut Bits<'_>, out: &mut [f32]) -> Result<()> {
        let fs = self.frame_size;
        let mut low = vec![0f32; fs];
        self.low.decode(bits, &mut low)?;

        let wideband = bits.remaining() > 0 && bits.peek() != 0;
        let submode_id = if wideband {
            bits.read(1)?;
            bits.read(3)? as usize
        }
        else {
            0
        };
        let submode = match self.submodes.get(submode_id) {
            Some(m) => *m,
            None => return decode_error("speex: invalid mode"),
        };
        if submode_id != 0 && submode.is_none() {
            return decode_error("speex: invalid mode");
        }

        let mut high = vec![0f32; fs];
        let dtx = self.low.dtx();

        let Some(mode) = submode
        else {
            self.innov.fill(0.0);
            self.first = true;
            if dtx {
                for h in high.iter_mut() {
                    *h = rand(self.last_ener, &mut self.seed);
                }
            }
            else {
                high.fill(1e-15);
            }
            let input = high.clone();
            iir_mem(&input, &self.interp_qlpc, &mut high, &mut self.mem_sp);
            qmf_synth(&low, &high, &QMF_H0, out, &mut self.g0_mem, &mut self.g1_mem);
            return Ok(());
        };

        let low_pi_gain = self.low.pi_gain().to_vec();
        let low_exc_rms = self.low.exc_rms().to_vec();
        let low_innov = self.low.innov().to_vec();

        let qlsp = mode.lsp.unquant(ORDER, bits)?;
        if self.first {
            self.old_qlsp.copy_from_slice(&qlsp);
        }

        let sf = self.subframe_size;
        let mut exc_ener_sum = 0.0;
        let mut interp = [0f32; ORDER];
        let mut ak = [0f32; ORDER];
        self.innov.fill(0.0);

        for sub in 0..self.nb_subframes {
            let offset = sf * sub;

            lsp_interpolate(&self.old_qlsp, &qlsp, &mut interp, sub, self.nb_subframes, LSP_MARGIN);
            lsp_to_lpc(&interp, &mut ak);

            let mut pi = 1.0;
            let mut rh = 1.0;
            for i in (0..ORDER).step_by(2) {
                rh += ak[i + 1] - ak[i];
                pi += ak[i] + ak[i + 1];
            }
            self.pi_gain[sub] = pi;

            let rl = low_pi_gain[sub];
            let filter_ratio = (rl + 0.01) / (rh + 0.01);

            let mut exc = vec![0f32; sf];
            if matches!(mode.innovation, Innovation::None) {
                let quant = bits.read(5)?;
                let g = (0.125 * (quant as f32 - 10.0)).exp() / filter_ratio.max(EPSILON);

                for i in (0..sf).step_by(2) {
                    exc[i] = self.folding_gain * low_innov[offset + i] * g;
                    exc[i + 1] = -(self.folding_gain * low_innov[offset + i + 1] * g);
                }
            }
            else {
                let qgc = bits.read(4)? as usize;
                let el = low_exc_rms[sub];
                let mut gc = 0.87360 * GC_QUANT_BOUND[qgc];
                if sf == 80 {
                    // The constant of the reference implementation, not quite the square root of two.
                    #[allow(clippy::approx_constant)]
                    {
                        gc *= 1.4142;
                    }
                }
                let scale = gc * el / filter_ratio.max(EPSILON);

                unquant_innovation(mode.innovation, &mut exc, bits, &mut self.seed)?;
                for v in exc.iter_mut() {
                    *v *= scale;
                }

                if mode.double_codebook {
                    let mut exc2 = vec![0f32; sf];
                    unquant_innovation(mode.innovation, &mut exc2, bits, &mut self.seed)?;
                    for (v, v2) in exc.iter_mut().zip(&exc2) {
                        *v += 0.4 * scale * v2;
                    }
                }
            }

            for i in 0..sf {
                self.innov[2 * (offset + i)] = exc[i];
            }

            let input = self.exc_buf.clone();
            iir_mem(&input, &self.interp_qlpc, &mut high[offset..offset + sf], &mut self.mem_sp);
            self.exc_buf.copy_from_slice(&exc);
            self.interp_qlpc = ak;
            self.exc_rms[sub] = rms(&self.exc_buf);
            exc_ener_sum += self.exc_rms[sub] * self.exc_rms[sub] / self.nb_subframes as f32;
        }
        self.last_ener = exc_ener_sum.sqrt();

        qmf_synth(&low, &high, &QMF_H0, out, &mut self.g0_mem, &mut self.g1_mem);
        self.old_qlsp.copy_from_slice(&qlsp);
        self.first = false;

        Ok(())
    }
}
